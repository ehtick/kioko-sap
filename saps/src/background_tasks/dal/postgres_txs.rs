//! PostgreSQL implementations of the background task transaction traits.
//!
//! This module contains the concrete database logic for inserting, claiming, and
//! completing background tasks in PostgreSQL. Each function is annotated with the
//! [`#[db_transaction]`](crate::db_transaction) attribute macro, which implements the
//! corresponding trait (defined in [`tx_definitions`](super::tx_definitions)) on
//! [`BackgroundTaskPostGresDescriptor<T>`](crate::dal::connections::BackgroundTaskPostGresDescriptor).
//!
//! # How it works
//!
//! The `#[db_transaction(BackgroundTaskPostGresDescriptor, TraitName)]` macro expands each
//! function into an `impl<T: YieldPostGresPool> TraitName for BackgroundTaskPostGresDescriptor<T>`
//! block. Inside the function body, `T::yield_pool()` provides a reference to the connection
//! pool (`&'static Pool<Postgres>`), which is used to execute queries.
//!
//! # Schema
//!
//! All queries target the `saps.queued_tasks` table. The table and stored procedures can be
//! created by calling [`QueuedTask::generate_migration_sql()`](super::model::QueuedTask::generate_migration_sql).

use super::{
    model::QueuedTask,
    tx_definitions::{
        InsertBackgroundTask,
        GetNextBackgroundTask,
        MarkBackgroundTaskAsCompleted,
        MarkBackgroundTaskAsExited,
    },
};
use crate::{
    dal::connections::BackgroundTaskPostGresDescriptor,
    db_transaction,
};


/// Inserts a new background task into `saps.queued_tasks`.
///
/// The caller constructs a [`QueuedTask`] (typically via [`QueuedTask::new`]) and this
/// function persists it to the database. Returns `true` if the insert succeeded.
///
/// # Errors
///
/// Returns `sqlx::Error` if the insert fails (e.g. connection error, constraint violation).
#[db_transaction(BackgroundTaskPostGresDescriptor, InsertBackgroundTask)]
async fn insert_background_task(task: QueuedTask) -> bool {
    let pool = T::yield_pool();
    let result = sqlx::query(
        r#"
        INSERT INTO saps.queued_tasks (id, function_name, params, status, time_posted, time_started, time_finished, locked)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
        .bind(task.id)
        .bind(task.function_name)
        .bind(task.params)
        .bind(task.status.to_string())
        .bind(task.time_posted)
        .bind(task.time_started)
        .bind(task.time_finished)
        .bind(task.locked)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}


/// Atomically claims the next unlocked background task by calling `saps.get_next_task()`.
///
/// The stored procedure finds the oldest row where `locked = FALSE`, sets `locked = TRUE`,
/// and returns the full row. Uses `FOR UPDATE SKIP LOCKED` for concurrent worker safety.
///
/// # Returns
///
/// - `Ok(Some(task))` if an unlocked task was found and claimed.
/// - `Ok(None)` if no unlocked tasks exist.
///
/// # Errors
///
/// Returns `sqlx::Error` if the query fails or the row cannot be parsed.
#[db_transaction(BackgroundTaskPostGresDescriptor, GetNextBackgroundTask)]
async fn get_next_background_task() -> Option<QueuedTask> {
    let pool = T::yield_pool();
    let row = sqlx::query("SELECT * FROM saps.get_next_task()")
        .fetch_optional(pool)
        .await?;
    match row {
        Some(r) => {
            // The stored procedure returns NULL as a composite row when no unlocked
            // tasks exist. Postgres still delivers a row with all-null columns,
            // so we check the id column before attempting to parse.
            let id: Option<uuid::Uuid> = sqlx::Row::try_get(&r, "id")
                .map_err(|e: sqlx::Error| sqlx::Error::Protocol(e.to_string()))?;
            if id.is_none() {
                return Ok(None);
            }
            let task = QueuedTask::from_row(&r)
                .map_err(|e| sqlx::Error::Protocol(e.message))?;
            Ok(Some(task))
        }
        None => Ok(None),
    }
}


/// Marks a background task as completed by setting its status to `'completed'`
/// and `time_finished` to `NOW()`.
///
/// # Arguments
///
/// * `id` — the UUID of the task to mark as completed.
///
/// # Returns
///
/// - `Ok(true)` if the task existed and was updated.
/// - `Ok(false)` if no task was found with the given UUID.
///
/// # Errors
///
/// Returns `sqlx::Error` if the query fails.
#[db_transaction(BackgroundTaskPostGresDescriptor, MarkBackgroundTaskAsCompleted)]
async fn mark_background_task_as_completed(id: uuid::Uuid) -> bool {
    let pool = T::yield_pool();
    let result = sqlx::query(
        "UPDATE saps.queued_tasks SET status = 'completed', time_finished = NOW() WHERE id = $1"
    )
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}


/// Marks a background task as exited (failed) by setting its status to `'exited'`
/// and `time_finished` to `NOW()`.
///
/// This is used when a task fails or is terminated abnormally, as opposed to
/// [`mark_background_task_as_completed`] which indicates success.
///
/// # Arguments
///
/// * `id` — the UUID of the task to mark as exited.
///
/// # Returns
///
/// - `Ok(true)` if the task existed and was updated.
/// - `Ok(false)` if no task was found with the given UUID.
///
/// # Errors
///
/// Returns `sqlx::Error` if the query fails.
#[db_transaction(BackgroundTaskPostGresDescriptor, MarkBackgroundTaskAsExited)]
async fn mark_background_task_as_exited(id: uuid::Uuid) -> bool {
    let pool = T::yield_pool();
    let result = sqlx::query(
        "UPDATE saps.queued_tasks SET status = 'exited', time_finished = NOW() WHERE id = $1"
    )
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
