use std::str::FromStr;

use serde_json::Value;
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;
use chrono::NaiveDateTime;

use crate::dal::connections::{ScheduledTaskPostGresDescriptor, YieldPostGresPool};
use crate::errors::saps::SapsError;
use crate::scheduled_tasks::dal::tx_definitions::InsertScheduledTask;


#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    /// The task has been put on the background queue
    Scheduled,
    /// A worker has claimed the task and is currently executing it.
    InProgress,
    /// The task finished successfully.
    Completed,
    /// The task failed or was terminated abnormally.
    Exited,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Scheduled => write!(f, "scheduled"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Exited => write!(f, "exited"),
        }
    }
}

impl TryFrom<String> for TaskStatus {
    type Error = SapsError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "scheduled" => Ok(TaskStatus::Scheduled),
            "in_progress" => Ok(TaskStatus::InProgress),
            "completed" => Ok(TaskStatus::Completed),
            "exited" => Ok(TaskStatus::Exited),
            _ => Err(SapsError::bad_request(format!("Unknown task status: {}", value))),
        }
    }
}


#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTask {
    pub id: i32,
    pub function_name: String,
    pub params: Value,
    pub task_id: Option<Uuid>,
    pub time_scheduled: Option<NaiveDateTime>,
    pub time_completed: Option<NaiveDateTime>,
    pub cron_string: String,
    pub locked: bool
}

impl ScheduledTask {
    /// Constructs a `ScheduledTask` from a Postgres row.
    pub fn from_row(row: &PgRow) -> Result<Self, SapsError> {
        Ok(Self {
            id: row.try_get("id").map_err(|e| SapsError::unknown(e.to_string()))?,
            function_name: row.try_get("function_name").map_err(|e| SapsError::unknown(e.to_string()))?,
            params: row.try_get("params").map_err(|e| SapsError::unknown(e.to_string()))?,
            task_id: row.try_get("task_id").map_err(|e| SapsError::unknown(e.to_string()))?,
            time_scheduled: row.try_get("time_scheduled").map_err(|e| SapsError::unknown(e.to_string()))?,
            time_completed: row.try_get("time_completed").map_err(|e| SapsError::unknown(e.to_string()))?,
            cron_string: row.try_get("cron_string").map_err(|e| SapsError::unknown(e.to_string()))?,
            locked: row.try_get("locked").map_err(|e| SapsError::unknown(e.to_string()))?,
        })
    }

    /// Returns SQL that **drops and recreates** the `saps.scheduled_tasks`
    /// table, its index, and the `saps.claim_due_scheduled_tasks()` stored
    /// procedure.
    ///
    /// # ⚠️ Wipes the table on every call
    ///
    /// Unlike the analogous function on `QueuedTask`, this script begins with
    /// `DROP TABLE IF EXISTS saps.scheduled_tasks CASCADE`. Schedules are
    /// expected to be re-registered from code on every app start (via
    /// [`register_scheduled_task`]), so the DB row is treated as transient
    /// working state, not a durable record.
    ///
    /// ## `saps.claim_due_scheduled_tasks()`
    ///
    /// Atomically selects all rows where `time_scheduled <= NOW()` and
    /// `locked = FALSE`, sets `locked = TRUE` on them, and returns the
    /// updated rows. Uses `FOR UPDATE SKIP LOCKED` so multiple scheduler
    /// instances can run safely.
    pub fn generate_migration_sql() -> &'static str {
        r#"
CREATE SCHEMA IF NOT EXISTS saps;

DROP TABLE IF EXISTS saps.scheduled_tasks CASCADE;

CREATE TABLE saps.scheduled_tasks (
    id             SERIAL PRIMARY KEY,
    function_name  VARCHAR(255) NOT NULL,
    params         JSONB NOT NULL DEFAULT '{}',
    task_id        UUID,
    time_scheduled TIMESTAMP,
    time_completed TIMESTAMP,
    cron_string    VARCHAR(255) NOT NULL,
    locked         BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX idx_saps_scheduled_tasks_due
    ON saps.scheduled_tasks (time_scheduled)
    WHERE locked = FALSE;

CREATE OR REPLACE FUNCTION saps.claim_due_scheduled_tasks()
RETURNS SETOF saps.scheduled_tasks
LANGUAGE plpgsql
AS $$
BEGIN
    RETURN QUERY
    UPDATE saps.scheduled_tasks
    SET locked = TRUE
    WHERE id IN (
        SELECT id FROM saps.scheduled_tasks
        WHERE locked = FALSE
          AND time_scheduled IS NOT NULL
          AND time_scheduled <= NOW()
        ORDER BY time_scheduled ASC
        FOR UPDATE SKIP LOCKED
    )
    RETURNING *;
END;
$$;
"#
    }

    /// Constructs a fresh `ScheduledTask` from a function name, JSON params,
    /// and a cron expression. Parses the cron string and computes the first
    /// firing time as `time_scheduled`.
    ///
    /// `id` is set to `0` (the DB will assign one on insert via `SERIAL`),
    /// `task_id` and `time_completed` are `None`, and `locked` is `false`.
    ///
    /// # Errors
    ///
    /// - Invalid `cron_string` → [`SapsError::bad_request`].
    /// - Cron schedule yields no upcoming firing → [`SapsError::bad_request`].
    pub fn new(
        function_name: impl Into<String>,
        params: Value,
        cron_string: impl Into<String>,
    ) -> Result<Self, SapsError> {
        let cron_string = cron_string.into();
        let schedule = cron::Schedule::from_str(&cron_string)
            .map_err(|e| SapsError::bad_request(format!("invalid cron '{}': {}", cron_string, e)))?;
        let next = schedule
            .upcoming(chrono::Utc)
            .next()
            .ok_or_else(|| SapsError::bad_request(format!("cron '{}' has no upcoming firing", cron_string)))?
            .naive_utc();

        Ok(Self {
            id: 0,
            function_name: function_name.into(),
            params,
            task_id: None,
            time_scheduled: Some(next),
            time_completed: None,
            cron_string,
            locked: false,
        })
    }
}


/// Registers a scheduled task by constructing a [`ScheduledTask`] via
/// [`ScheduledTask::new`] and inserting it into `saps.scheduled_tasks`.
///
/// Call this for each schedule the app wants to fire. Because
/// [`ScheduledTask::generate_migration_sql`] drops the table on every
/// migration run, you must call `register_scheduled_task` for every
/// schedule on every app startup — schedules are not persistent.
///
/// # Errors
///
/// - Invalid cron expression → [`SapsError::bad_request`].
/// - DB insert failure → [`SapsError::unknown`] wrapping the sqlx error.
pub async fn register_scheduled_task<Y: YieldPostGresPool>(
    function_name: impl Into<String>,
    params: Value,
    cron_string: impl Into<String>,
) -> Result<bool, SapsError> {
    let task = ScheduledTask::new(function_name, params, cron_string)?;
    ScheduledTaskPostGresDescriptor::<Y>::insert_scheduled_task(task)
        .await
        .map_err(|e| SapsError::unknown(e.to_string()))
}


#[derive(Debug, Clone, PartialEq)]
pub struct NewScheduledTask {
    pub function_name: String,
    pub params: Value,
    pub task_id: Option<Uuid>,
    pub time_scheduled: Option<NaiveDateTime>,
    pub time_completed: Option<NaiveDateTime>,
    pub cron_string: String,
    pub locked: bool
}
