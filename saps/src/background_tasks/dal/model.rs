//! Data model for the background task queue.
//!
//! This module defines [`QueuedTask`] and [`TaskStatus`], which represent rows in the
//! `saps.queued_tasks` table. Tasks are inserted with status [`Pending`](TaskStatus::Pending)
//! and progress through [`InProgress`](TaskStatus::InProgress) to either
//! [`Completed`](TaskStatus::Completed) or [`Exited`](TaskStatus::Exited).
//!
//! The module also provides [`QueuedTask::generate_migration_sql`] which returns the SQL
//! needed to create the table and a stored procedure for atomically claiming the next
//! pending task.
use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;
use crate::errors::saps::SapsError;


/// The lifecycle status of a queued background task.
///
/// Tasks move through these states in order:
///
/// ```text
/// Pending ──► InProgress ──► Completed
///                        └──► Exited (on failure)
/// ```
///
/// The status is stored as a `VARCHAR` in the database using the lowercase string
/// representation (e.g. `"pending"`, `"in_progress"`).
#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    /// The task has been enqueued but no worker has picked it up yet.
    Pending,
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
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Exited => write!(f, "exited"),
        }
    }
}

impl TryFrom<String> for TaskStatus {
    type Error = SapsError;

    /// Parses a status string (case-insensitive) into a [`TaskStatus`].
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the string does not match any known status.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.to_lowercase().as_str() {
            "pending" => Ok(TaskStatus::Pending),
            "in_progress" => Ok(TaskStatus::InProgress),
            "completed" => Ok(TaskStatus::Completed),
            "exited" => Ok(TaskStatus::Exited),
            _ => Err(SapsError::bad_request(format!("Unknown task status: {}", value))),
        }
    }
}


/// A background task stored in the `saps.queued_tasks` table.
///
/// Each task represents a unit of work identified by `function_name` with JSON
/// `params`. Workers claim pending tasks, execute them, and update the status
/// to `Completed` or `Exited`.
///
/// # Fields
///
/// | Field | Column | Description |
/// |-------|--------|-------------|
/// | `id` | `id` (UUID PK) | Unique identifier, generated on insert |
/// | `function_name` | `function_name` (VARCHAR) | The name of the function/handler to execute |
/// | `params` | `params` (JSONB) | Arguments passed to the function |
/// | `status` | `status` (VARCHAR) | Current lifecycle state (see [`TaskStatus`]) |
/// | `time_posted` | `time_posted` (TIMESTAMP) | When the task was enqueued |
/// | `time_started` | `time_started` (TIMESTAMP) | When a worker began execution (`NULL` if pending) |
/// | `time_finished` | `time_finished` (TIMESTAMP) | When execution ended (`NULL` if not finished) |
/// | `locked` | `locked` (BOOLEAN) | Whether the task is locked for processing (default `false`) |
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedTask {
    /// The unique identifier of the task.
    pub id: Uuid,
    /// The name of the function/handler to execute for this task.
    pub function_name: String,
    /// JSON parameters to pass to the function.
    pub params: Value,
    /// The current lifecycle status of the task.
    pub status: TaskStatus,
    /// The timestamp when the task was enqueued.
    pub time_posted: NaiveDateTime,
    /// The timestamp when a worker started executing the task, or `None` if still pending.
    pub time_started: Option<NaiveDateTime>,
    /// The timestamp when execution finished, or `None` if still running or pending.
    pub time_finished: Option<NaiveDateTime>,
    /// Whether the task is currently locked for processing. Defaults to `false`.
    pub locked: bool,
}

impl QueuedTask {
    /// Constructs a `QueuedTask` from a Postgres row, converting the `status`
    /// column (VARCHAR) into a [`TaskStatus`] via `TryFrom<String>`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if any column is missing or the `status` string
    /// does not match a known variant.
    pub fn from_row(row: &PgRow) -> Result<Self, SapsError> {
        let status_str: String = row.try_get("status")
            .map_err(|e| SapsError::unknown(e.to_string()))?;
        let status = TaskStatus::try_from(status_str)?;
        Ok(Self {
            id: row.try_get("id").map_err(|e| SapsError::unknown(e.to_string()))?,
            function_name: row.try_get("function_name").map_err(|e| SapsError::unknown(e.to_string()))?,
            params: row.try_get("params").map_err(|e| SapsError::unknown(e.to_string()))?,
            status,
            time_posted: row.try_get("time_posted").map_err(|e| SapsError::unknown(e.to_string()))?,
            time_started: row.try_get("time_started").map_err(|e| SapsError::unknown(e.to_string()))?,
            time_finished: row.try_get("time_finished").map_err(|e| SapsError::unknown(e.to_string()))?,
            locked: row.try_get("locked").map_err(|e| SapsError::unknown(e.to_string()))?,
        })
    }
}

impl QueuedTask {
    /// Creates a new `QueuedTask` with status [`Pending`](TaskStatus::Pending),
    /// the current timestamp as `time_posted`, and `time_started`/`time_finished`
    /// set to `None`.
    ///
    /// # Arguments
    ///
    /// * `function_name` — the name of the function/handler this task should invoke.
    /// * `params` — JSON parameters to pass to the function. Use
    ///   `serde_json::to_value(...)` or `serde_json::json!(...)` to construct this.
    pub fn new(function_name: impl Into<String>, params: Value) -> Self {
        let now = chrono::Utc::now().naive_utc();
        Self {
            id: Uuid::new_v4(),
            function_name: function_name.into(),
            params,
            status: TaskStatus::Pending,
            time_posted: now,
            time_started: None,
            time_finished: None,
            locked: false,
        }
    }

    /// Returns a SQL script that creates the `saps` schema (if it doesn't exist),
    /// the `saps.queued_tasks` table, and two stored procedures:
    ///
    /// ## `saps.claim_task(p_function_name VARCHAR)`
    ///
    /// Atomically claims the oldest pending task for a given function name:
    /// 1. Finds the oldest row with `status = 'pending'` matching `p_function_name`.
    /// 2. Updates its `status` to `'in_progress'` and sets `time_started` to `NOW()`.
    /// 3. Returns the full row, or `NULL` if no pending task was found.
    /// 4. Uses `FOR UPDATE SKIP LOCKED` for concurrent worker safety.
    ///
    /// ## `saps.get_next_task()`
    ///
    /// Atomically finds and locks the oldest unlocked task regardless of function name:
    /// 1. Finds the oldest row where `locked = FALSE`, ordered by `time_posted ASC`.
    /// 2. Sets `locked = TRUE` on that row.
    /// 3. Returns the full row, or `NULL` if no unlocked tasks exist.
    /// 4. Uses `FOR UPDATE SKIP LOCKED` so multiple workers calling this concurrently
    ///    will each get a different task — no two workers will ever receive the same row.
    pub fn generate_migration_sql() -> &'static str {
        r#"
CREATE SCHEMA IF NOT EXISTS saps;

CREATE TABLE IF NOT EXISTS saps.queued_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    function_name VARCHAR(255) NOT NULL,
    params JSONB NOT NULL DEFAULT '{}',
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    time_posted TIMESTAMP NOT NULL DEFAULT NOW(),
    time_started TIMESTAMP,
    time_finished TIMESTAMP,
    locked BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX IF NOT EXISTS idx_saps_queued_tasks_status
    ON saps.queued_tasks (status);

CREATE INDEX IF NOT EXISTS idx_saps_queued_tasks_function_name
    ON saps.queued_tasks (function_name);

CREATE OR REPLACE FUNCTION saps.claim_task(
    p_function_name VARCHAR
)
RETURNS saps.queued_tasks
LANGUAGE plpgsql
AS $$
DECLARE
    task_record saps.queued_tasks;
BEGIN
    SELECT * INTO task_record
    FROM saps.queued_tasks
    WHERE function_name = p_function_name
      AND status = 'pending'
    ORDER BY time_posted ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED;

    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    UPDATE saps.queued_tasks
    SET status = 'in_progress',
        time_started = NOW()
    WHERE id = task_record.id
    RETURNING * INTO task_record;

    RETURN task_record;
END;
$$;

CREATE OR REPLACE FUNCTION saps.get_next_task()
RETURNS saps.queued_tasks
LANGUAGE plpgsql
AS $$
DECLARE
    task_record saps.queued_tasks;
BEGIN
    SELECT * INTO task_record
    FROM saps.queued_tasks
    WHERE locked = FALSE
    ORDER BY time_posted ASC
    LIMIT 1
    FOR UPDATE SKIP LOCKED;

    IF NOT FOUND THEN
        RETURN NULL;
    END IF;

    UPDATE saps.queued_tasks
    SET locked = TRUE
    WHERE id = task_record.id
    RETURNING * INTO task_record;

    RETURN task_record;
END;
$$;
"#
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_task_has_pending_status() {
        let task = QueuedTask::new("send_email", serde_json::json!({"to": "user@example.com"}));
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.function_name, "send_email");
        assert!(task.time_started.is_none());
        assert!(task.time_finished.is_none());
    }

    #[test]
    fn test_new_task_has_valid_uuid() {
        let task = QueuedTask::new("process", serde_json::json!({}));
        // UUID v4 string is 36 chars (8-4-4-4-12 with hyphens)
        assert_eq!(task.id.to_string().len(), 36);
    }

    #[test]
    fn test_new_task_preserves_params() {
        let params = serde_json::json!({"user_id": 42, "action": "upgrade"});
        let task = QueuedTask::new("handle_upgrade", params.clone());
        assert_eq!(task.params, params);
    }

    #[test]
    fn test_task_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::InProgress.to_string(), "in_progress");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
        assert_eq!(TaskStatus::Exited.to_string(), "exited");
    }

    #[test]
    fn test_task_status_try_from_valid() {
        assert_eq!(TaskStatus::try_from("pending".to_string()).unwrap(), TaskStatus::Pending);
        assert_eq!(TaskStatus::try_from("in_progress".to_string()).unwrap(), TaskStatus::InProgress);
        assert_eq!(TaskStatus::try_from("completed".to_string()).unwrap(), TaskStatus::Completed);
        assert_eq!(TaskStatus::try_from("exited".to_string()).unwrap(), TaskStatus::Exited);
    }

    #[test]
    fn test_task_status_try_from_case_insensitive() {
        assert_eq!(TaskStatus::try_from("PENDING".to_string()).unwrap(), TaskStatus::Pending);
        assert_eq!(TaskStatus::try_from("In_Progress".to_string()).unwrap(), TaskStatus::InProgress);
    }

    #[test]
    fn test_task_status_try_from_invalid() {
        let result = TaskStatus::try_from("unknown_status".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_migration_sql_contains_table() {
        let sql = QueuedTask::generate_migration_sql();
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS saps.queued_tasks"));
    }

    #[test]
    fn test_generate_migration_sql_contains_claim_function() {
        let sql = QueuedTask::generate_migration_sql();
        assert!(sql.contains("CREATE OR REPLACE FUNCTION saps.claim_task"));
        assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
    }

    #[test]
    fn test_generate_migration_sql_contains_get_next_task_function() {
        let sql = QueuedTask::generate_migration_sql();
        assert!(sql.contains("CREATE OR REPLACE FUNCTION saps.get_next_task"));
        assert!(sql.contains("WHERE locked = FALSE"));
        assert!(sql.contains("SET locked = TRUE"));
    }

    #[test]
    fn test_new_task_defaults_locked_to_false() {
        let task = QueuedTask::new("test_fn", serde_json::json!({}));
        assert!(!task.locked);
    }
}
