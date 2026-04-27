//! PostgreSQL implementations of the scheduled task transaction traits.
//!
//! Each function is annotated with [`#[db_transaction]`](crate::db_transaction)
//! which implements the corresponding trait (declared in
//! [`tx_definitions`](super::tx_definitions)) on
//! [`ScheduledTaskPostGresDescriptor<T>`](crate::dal::connections::ScheduledTaskPostGresDescriptor).
//!
//! All queries target `saps.scheduled_tasks` and (for `post_scheduled_task`)
//! `saps.queued_tasks`. The schema and the `saps.claim_due_scheduled_tasks()`
//! stored procedure are created by
//! [`ScheduledTask::generate_migration_sql()`](super::model::ScheduledTask::generate_migration_sql).

use super::{
    model::ScheduledTask,
    tx_definitions::{
        InsertScheduledTask,
        GetDueScheduledTasks,
        PostScheduledTask,
    },
};
use crate::{
    dal::connections::ScheduledTaskPostGresDescriptor,
    db_transaction,
};


/// Inserts a new scheduled task into `saps.scheduled_tasks`.
///
/// `id` is omitted from the INSERT — the `SERIAL` column generates it. The
/// `id` field on the passed-in `ScheduledTask` is therefore ignored.
#[db_transaction(ScheduledTaskPostGresDescriptor, InsertScheduledTask)]
async fn insert_scheduled_task(task: ScheduledTask) -> bool {
    let pool = T::yield_pool();
    let result = sqlx::query(
        r#"
        INSERT INTO saps.scheduled_tasks
            (function_name, params, task_id, time_scheduled, time_completed, cron_string, locked)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
        .bind(task.function_name)
        .bind(task.params)
        .bind(task.task_id)
        .bind(task.time_scheduled)
        .bind(task.time_completed)
        .bind(task.cron_string)
        .bind(task.locked)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}


/// Atomically claims all due scheduled tasks via `saps.claim_due_scheduled_tasks()`.
///
/// The stored procedure selects rows where `time_scheduled <= NOW()` and
/// `locked = FALSE`, sets `locked = TRUE` on each, and returns them. Uses
/// `FOR UPDATE SKIP LOCKED` so concurrent schedulers never claim the same row.
#[db_transaction(ScheduledTaskPostGresDescriptor, GetDueScheduledTasks)]
async fn get_due_scheduled_task() -> Vec<ScheduledTask> {
    let pool = T::yield_pool();
    let rows = sqlx::query("SELECT * FROM saps.claim_due_scheduled_tasks()")
        .fetch_all(pool)
        .await?;
    let mut tasks = Vec::with_capacity(rows.len());
    for row in &rows {
        let task = ScheduledTask::from_row(row)
            .map_err(|e| sqlx::Error::Protocol(e.message))?;
        tasks.push(task);
    }
    Ok(tasks)
}


/// Posts a due scheduled task to the background queue and advances the
/// scheduled row in a single database transaction.
///
/// Both writes happen in one `BEGIN ... COMMIT` so that a failure in either
/// rolls the other back; this prevents the "queue insert succeeded but
/// schedule update failed → duplicate fire on next tick" failure mode.
///
/// The caller must populate `task` with:
/// - `task_id` = `Some(uuid)` — the UUID to assign to the new `queued_tasks` row.
/// - `time_scheduled` = the next firing time (computed from the cron string).
/// - `time_completed` = the current time (when this fire happened).
///
/// On commit the row's `locked` flag is reset to `FALSE`.
#[db_transaction(ScheduledTaskPostGresDescriptor, PostScheduledTask)]
async fn post_scheduled_task(task: ScheduledTask) -> bool {
    let pool = T::yield_pool();
    let mut tx = pool.begin().await?;

    let queued_id = task.task_id.unwrap_or_else(uuid::Uuid::new_v4);
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        r#"
        INSERT INTO saps.queued_tasks
            (id, function_name, params, status, time_posted, time_started, time_finished, locked)
        VALUES ($1, $2, $3, 'pending', $4, NULL, NULL, FALSE)
        "#,
    )
        .bind(queued_id)
        .bind(&task.function_name)
        .bind(&task.params)
        .bind(now)
        .execute(&mut *tx)
        .await?;

    let result = sqlx::query(
        r#"
        UPDATE saps.scheduled_tasks
        SET time_scheduled = $1,
            time_completed = $2,
            task_id        = $3,
            locked         = FALSE
        WHERE id = $4
        "#,
    )
        .bind(task.time_scheduled)
        .bind(task.time_completed)
        .bind(queued_id)
        .bind(task.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(result.rows_affected() > 0)
}
