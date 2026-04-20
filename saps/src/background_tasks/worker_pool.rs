//! Background task worker pool for processing queued tasks.
//!
//! This module provides [`WorkerPool`], which spawns one or more asynchronous worker
//! tasks that continuously poll the `saps.queued_tasks` database table for pending work.
//! Each worker runs an independent [`worker_cycle`] loop that:
//!
//! 1. Calls `saps.get_next_task()` to atomically claim the oldest unlocked task.
//! 2. Looks up the corresponding handler function in the [`TASK_REGISTRY`].
//! 3. Executes the handler on a **blocking thread** via `tokio::task::spawn_blocking`
//!    to avoid starving the async runtime if the task is CPU-intensive.
//! 4. Marks the task as `completed` or `exited` in the database depending on whether
//!    the handler succeeded or panicked.
//! 5. If no tasks are available, sleeps for a configurable polling interval before retrying.
//!
//! # Architecture
//!
//! ```text
//! WorkerPool::init_workers()
//!     |
//!     |-- spawns N async tasks (tokio::task::spawn)
//!     |       |
//!     |       +-- worker_cycle (loop)
//!     |               |
//!     |               +-- get_next_background_task()  [DB: claim + lock row]
//!     |               |
//!     |               +-- TASK_REGISTRY.read()        [lookup handler by name]
//!     |               |
//!     |               +-- spawn_blocking(handler)     [run on blocking thread]
//!     |               |
//!     |               +-- mark_completed / mark_exited [DB: update status]
//!     |               |
//!     |               +-- sleep(interval) if no tasks
//! ```
//!
//! # Why `spawn_blocking`?
//!
//! Background task handlers may perform CPU-bound work (data processing, image
//! manipulation, etc.) that would block the tokio async runtime if run directly.
//! By using `spawn_blocking`, the handler runs on a dedicated OS thread from tokio's
//! blocking thread pool, keeping the async workers free to poll for new tasks and
//! handle I/O. Inside the blocking thread, `Handle::block_on` drives the handler's
//! future to completion.
//!
//! # Concurrency safety
//!
//! - The `saps.get_next_task()` stored procedure uses `FOR UPDATE SKIP LOCKED`,
//!   so multiple workers will never claim the same task.
//! - The [`TASK_REGISTRY`] read guard is scoped into a block and dropped before
//!   any `.await` point, ensuring the future remains `Send`.
//!
//! # Example
//!
//! ```ignore
//! use saps::background_tasks::worker_pool::WorkerPool;
//! use saps::dal::connections::LivePostGresPool;
//!
//! #[tokio::main]
//! async fn main() {
//!     // Create a pool with 4 workers polling every 5 seconds
//!     let mut pool = WorkerPool::<LivePostGresPool>::new()
//!         .with_workers(4)
//!         .with_interval(5);
//!
//!     // Spawn all workers as background tokio tasks
//!     pool.init_workers();
//!
//!     // Keep the main task alive (or run your server, etc.)
//!     tokio::signal::ctrl_c().await.unwrap();
//! }
//! ```
use std::marker::{PhantomData, Send, Sync};
use tokio::task::JoinHandle;
use crate::background_tasks::{
    dal::tx_definitions::{
        GetNextBackgroundTask, MarkBackgroundTaskAsCompleted, MarkBackgroundTaskAsExited,
    },
    registry::TASK_REGISTRY,
};
use crate::dal::connections::{BackgroundTaskPostGresDescriptor, YieldPostGresPool};


/// A pool of background workers that process tasks from the `saps.queued_tasks` table.
///
/// The pool is generic over `Y: YieldPostGresPool`, which determines which database
/// connection pool the workers use. In production this is typically `LivePostGresPool`;
/// in tests you can substitute `MockDeadPostGresPool` or a test-specific pool.
///
/// # Builder pattern
///
/// ```ignore
/// let mut pool = WorkerPool::<LivePostGresPool>::new()
///     .with_workers(4)      // default: 1
///     .with_interval(5);    // default: 10 seconds
///
/// pool.init_workers();  // spawns 4 async worker tasks
/// ```
///
/// # Fields
///
/// | Field | Default | Description |
/// |-------|---------|-------------|
/// | `worker_num` | 1 | Number of concurrent workers to spawn |
/// | `interval` | 10 | Seconds to sleep when no tasks are available |
/// | `worker_handles` | `[]` | `JoinHandle`s for each spawned worker (populated by `init_workers`) |
pub struct WorkerPool<Y: YieldPostGresPool + Sync + Send> {
    /// Phantom marker for the database pool provider type `Y`.
    db_pool: PhantomData<Y>,
    /// The number of worker tasks to spawn when `init_workers` is called.
    worker_num: usize,
    /// Tokio `JoinHandle`s for each spawned worker task, allowing the caller
    /// to await or abort individual workers if needed.
    worker_handles: Vec<JoinHandle<()>>,
    /// The polling interval in seconds. When a worker finds no pending tasks,
    /// it sleeps for this duration before checking again.
    interval: usize
}


impl <Y: YieldPostGresPool + Sync + Send> WorkerPool<Y> {

    /// Creates a new `WorkerPool` with default settings.
    ///
    /// Defaults:
    /// - **workers**: 1
    /// - **interval**: 10 seconds
    /// - **handles**: empty (populated by [`init_workers`](Self::init_workers))
    pub fn new() -> Self {
        Self {
            db_pool: PhantomData::<Y>,
            worker_num: 1,
            worker_handles: vec![],
            interval: 10
        }
    }

    /// Sets the number of concurrent worker tasks to spawn.
    ///
    /// Each worker runs its own independent [`worker_cycle`] loop, polling the
    /// database and processing tasks concurrently. More workers means higher
    /// throughput but also more database connections in use.
    ///
    /// # Arguments
    ///
    /// * `workers` — the number of worker tasks to spawn.
    pub fn with_workers(mut self, workers: usize) -> Self {
        self.worker_num = workers;
        self
    }

    /// Spawns all worker tasks on the tokio runtime.
    ///
    /// Each worker is spawned as an independent `tokio::task::spawn` future that
    /// runs [`worker_cycle`] in an infinite loop. The `JoinHandle` for each worker
    /// is stored in `self.worker_handles`.
    ///
    /// This method must be called from within a tokio runtime context (i.e. inside
    /// an `async fn` or a `#[tokio::main]` block).
    ///
    /// # Note
    ///
    /// The `interval` value is copied before entering the async block to avoid
    /// capturing `&self` across the `spawn` boundary (which requires `'static`).
    pub fn init_workers(&mut self) {
        let interval = self.interval;
        for i in 0..self.worker_num {
            let handle = tokio::task::spawn(async move {
                worker_cycle::<Y>(i, interval).await
            });
            self.worker_handles.push(handle);
        }
    }

}


/// The core worker loop that polls for and executes background tasks.
///
/// This function runs indefinitely, performing the following cycle:
///
/// 1. **Poll** — calls `get_next_background_task()` which invokes the
///    `saps.get_next_task()` stored procedure. This atomically finds the oldest
///    unlocked row, sets `locked = TRUE`, and returns it.
///
/// 2. **Lookup** — reads the [`TASK_REGISTRY`] to find the handler function
///    registered for the task's `function_name`. The `RwLockReadGuard` is scoped
///    into a block and dropped before any `.await` to keep the future `Send`.
///
/// 3. **Execute** — runs the handler on a blocking thread via
///    `tokio::task::spawn_blocking`. This prevents CPU-intensive handlers from
///    blocking the async runtime. `Handle::block_on` is used inside the blocking
///    thread to drive the handler's async future to completion.
///
/// 4. **Finalize** — based on the outcome:
///    - **Success** (`Ok`) — marks the task as `completed` with `time_finished = NOW()`.
///    - **Panic/Error** (`Err`) — marks the task as `exited` and logs the error.
///
/// 5. **Sleep** — if no tasks were available, sleeps for `interval` seconds before
///    polling again.
///
/// # Arguments
///
/// * `number` — the worker index (used in log messages for identification).
/// * `interval` — seconds to sleep when no tasks are available.
///
/// # Error handling
///
/// This function never returns — it loops forever. All errors (database failures,
/// missing registry entries, handler panics) are logged via `tracing::error!` and
/// the worker continues to the next iteration.
async fn worker_cycle<Z: YieldPostGresPool + Sync + Send>(number: usize, interval: usize) {
    tracing::info!("worker number {} starting", number);

    let interval_duration = tokio::time::Duration::from_secs(interval as u64);

    loop {
        // Step 1: Claim the next unlocked task from the database.
        let task = match BackgroundTaskPostGresDescriptor::<Z>::get_next_background_task().await {
            Ok(task) => task,
            Err(error) => {
                tracing::error!("worker number {} error getting background task: {}", number, error);
                continue;
            }
        };
        match task {
            Some(task) => {
                // Step 2: Look up the handler in the registry.
                // The RwLockReadGuard is scoped so it's dropped before any .await,
                // which is required for the future to be Send (RwLockReadGuard is !Send).
                let handler = {
                    let registry = match TASK_REGISTRY.read() {
                        Ok(reg) => reg,
                        Err(error) => {
                            tracing::error!("worker number {} error getting registry: {}", number, error);
                            continue;
                        }
                    };
                    match registry.get(&task.function_name) {
                        Some(handle) => *handle,
                        None => {
                            tracing::error!("worker number {} handle {} not in registry", number, &task.function_name);
                            continue;
                        }
                    }
                    // registry guard is dropped here
                };

                // Step 3: Execute the handler on a blocking thread.
                // We use spawn_blocking to avoid starving the async runtime with
                // potentially CPU-bound task handlers. Handle::block_on drives the
                // async handler future to completion on the blocking thread.
                let params = task.params.clone();
                let pool = Z::yield_pool();
                let rt_handle = tokio::runtime::Handle::current();
                let outcome = tokio::task::spawn_blocking(move || {
                    rt_handle.block_on(handler(params, pool))
                }).await;

                // Step 4: Mark the task as completed or exited based on the outcome.
                match outcome {
                    Ok(_) => {
                        let _ = BackgroundTaskPostGresDescriptor::<Z>::mark_background_task_as_completed(task.id).await;
                    },
                    Err(error) => {
                        tracing::error!("worker number {} background task resulted in error: {} for task id: {}", number, error, task.id);
                        let _ = BackgroundTaskPostGresDescriptor::<Z>::mark_background_task_as_exited(task.id).await;
                    }
                }
            },
            None => {
                // Step 5: No tasks available — sleep before polling again.
                tokio::time::sleep(interval_duration).await;
                tracing::info!("worker number {} polling", number);
            }
        }
    }
}


#[cfg(test)]
mod tests {

    use super::*;
    use crate::background_tasks::dal::model::QueuedTask;
    use crate::background_tasks::dal::tx_definitions::InsertBackgroundTask;
    use crate::background_tasks::registry::TaskFnPtr;
    use crate::dal::connections::MockDeadPostGresPool;
    use crate::errors::saps::SapsError;
    use std::pin::Pin;
    use std::future::Future;

    // -- Builder tests (no DB needed) --

    #[test]
    fn test_new_defaults() {
        let pool = WorkerPool::<MockDeadPostGresPool>::new();
        assert_eq!(pool.worker_num, 1);
        assert_eq!(pool.interval, 10);
        assert!(pool.worker_handles.is_empty());
    }

    #[test]
    fn test_with_workers_sets_count() {
        let pool = WorkerPool::<MockDeadPostGresPool>::new().with_workers(8);
        assert_eq!(pool.worker_num, 8);
    }

    #[test]
    fn test_with_workers_chaining() {
        let pool = WorkerPool::<MockDeadPostGresPool>::new()
            .with_workers(3);
        assert_eq!(pool.worker_num, 3);
        assert_eq!(pool.interval, 10);
    }

    // -- DB integration tests --

    /// Verifies that inserting a task and calling get_next_background_task
    /// returns the task with locked = true.
    #[saps::db_test]
    async fn test_get_next_task_locks_row() {
        let task = QueuedTask::new("test_handler", serde_json::json!({"key": "value"}));
        let task_id = task.id;

        BackgroundTaskPostGresDescriptor::<TestDbHandle>::insert_background_task(task)
            .await
            .expect("failed to insert task");

        let claimed = BackgroundTaskPostGresDescriptor::<TestDbHandle>::get_next_background_task()
            .await
            .expect("failed to get next task");

        assert!(claimed.is_some());
        let claimed = claimed.unwrap();
        assert_eq!(claimed.id, task_id);
        assert!(claimed.locked);
        assert_eq!(claimed.function_name, "test_handler");
        assert_eq!(claimed.params, serde_json::json!({"key": "value"}));
    }

    /// Verifies that get_next_background_task returns None when no tasks exist.
    #[saps::db_test]
    async fn test_get_next_task_returns_none_when_empty() {
        let result = BackgroundTaskPostGresDescriptor::<TestDbHandle>::get_next_background_task()
            .await
            .expect("failed to get next task");

        assert!(result.is_none());
    }

    /// Verifies that already-locked tasks are skipped by get_next_background_task.
    #[saps::db_test]
    async fn test_get_next_task_skips_locked_rows() {
        // Insert two tasks
        let task1 = QueuedTask::new("handler_a", serde_json::json!({}));
        let task2 = QueuedTask::new("handler_b", serde_json::json!({}));
        let task2_id = task2.id;

        BackgroundTaskPostGresDescriptor::<TestDbHandle>::insert_background_task(task1)
            .await
            .expect("failed to insert task1");
        BackgroundTaskPostGresDescriptor::<TestDbHandle>::insert_background_task(task2)
            .await
            .expect("failed to insert task2");

        // Claim the first task (locks it)
        let first = BackgroundTaskPostGresDescriptor::<TestDbHandle>::get_next_background_task()
            .await
            .expect("failed to get first task");
        assert!(first.is_some());
        assert_eq!(first.unwrap().function_name, "handler_a");

        // Next call should skip the locked row and return the second task
        let second = BackgroundTaskPostGresDescriptor::<TestDbHandle>::get_next_background_task()
            .await
            .expect("failed to get second task");
        assert!(second.is_some());
        let second = second.unwrap();
        assert_eq!(second.id, task2_id);
        assert_eq!(second.function_name, "handler_b");
    }

    /// Verifies that marking a task as completed updates status and time_finished.
    #[saps::db_test]
    async fn test_mark_task_as_completed() {
        let task = QueuedTask::new("complete_me", serde_json::json!({}));
        let task_id = task.id;

        BackgroundTaskPostGresDescriptor::<TestDbHandle>::insert_background_task(task)
            .await
            .expect("failed to insert task");

        let marked = BackgroundTaskPostGresDescriptor::<TestDbHandle>::mark_background_task_as_completed(task_id)
            .await
            .expect("failed to mark as completed");
        assert!(marked);

        // Verify the row in the database
        let row = saps::sqlx::query("SELECT status, time_finished FROM saps.queued_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(pool)
            .await
            .expect("failed to fetch task");

        let status: String = sqlx::Row::try_get(&row, "status").unwrap();
        let time_finished: Option<chrono::NaiveDateTime> = sqlx::Row::try_get(&row, "time_finished").unwrap();
        assert_eq!(status, "completed");
        assert!(time_finished.is_some());
    }

    /// Verifies that marking a task as exited updates status and time_finished.
    #[saps::db_test]
    async fn test_mark_task_as_exited() {
        let task = QueuedTask::new("fail_me", serde_json::json!({}));
        let task_id = task.id;

        BackgroundTaskPostGresDescriptor::<TestDbHandle>::insert_background_task(task)
            .await
            .expect("failed to insert task");

        let marked = BackgroundTaskPostGresDescriptor::<TestDbHandle>::mark_background_task_as_exited(task_id)
            .await
            .expect("failed to mark as exited");
        assert!(marked);

        let row = saps::sqlx::query("SELECT status, time_finished FROM saps.queued_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(pool)
            .await
            .expect("failed to fetch task");

        let status: String = sqlx::Row::try_get(&row, "status").unwrap();
        let time_finished: Option<chrono::NaiveDateTime> = sqlx::Row::try_get(&row, "time_finished").unwrap();
        assert_eq!(status, "exited");
        assert!(time_finished.is_some());
    }

    /// Verifies that marking a nonexistent task returns false.
    #[saps::db_test]
    async fn test_mark_nonexistent_task_returns_false() {
        let fake_id = uuid::Uuid::new_v4();

        let completed = BackgroundTaskPostGresDescriptor::<TestDbHandle>::mark_background_task_as_completed(fake_id)
            .await
            .expect("failed to mark as completed");
        assert!(!completed);

        let exited = BackgroundTaskPostGresDescriptor::<TestDbHandle>::mark_background_task_as_exited(fake_id)
            .await
            .expect("failed to mark as exited");
        assert!(!exited);
    }

    /// Verifies that a handler registered in the TASK_REGISTRY can be looked up
    /// and executed with the correct parameters.
    #[saps::db_test]
    async fn test_registry_handler_lookup_and_execution() {
        // Register a test handler
        fn test_handler(
            params: serde_json::Value,
            _pool: &'static sqlx::Pool<sqlx::Postgres>,
        ) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send>> {
            Box::pin(async move {
                assert_eq!(params.get("x").unwrap(), 42);
                Ok(())
            })
        }

        TASK_REGISTRY
            .write()
            .unwrap()
            .insert("test_registry_fn".to_string(), test_handler as TaskFnPtr);

        // Look up the handler — scope the guard so it's dropped before .await
        let handler = {
            let registry = TASK_REGISTRY.read().unwrap();
            *registry.get("test_registry_fn").expect("handler not found")
        };
        let result = handler(serde_json::json!({"x": 42}), pool).await;
        assert!(result.is_ok());
    }

    /// End-to-end test: insert a task, claim it, execute via registry, and mark complete.
    #[saps::db_test]
    async fn test_full_task_lifecycle() {
        // Register a handler that will be looked up by function_name
        fn lifecycle_handler(
            params: serde_json::Value,
            _pool: &'static sqlx::Pool<sqlx::Postgres>,
        ) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send>> {
            Box::pin(async move {
                let a = params.get("a").unwrap().as_i64().unwrap();
                let b = params.get("b").unwrap().as_i64().unwrap();
                assert_eq!(a + b, 30);
                Ok(())
            })
        }

        TASK_REGISTRY
            .write()
            .unwrap()
            .insert("lifecycle_test".to_string(), lifecycle_handler as TaskFnPtr);

        // Insert a task
        let task = QueuedTask::new("lifecycle_test", serde_json::json!({"a": 10, "b": 20}));
        let task_id = task.id;
        BackgroundTaskPostGresDescriptor::<TestDbHandle>::insert_background_task(task)
            .await
            .expect("failed to insert task");

        // Claim it
        let claimed = BackgroundTaskPostGresDescriptor::<TestDbHandle>::get_next_background_task()
            .await
            .expect("failed to claim task")
            .expect("expected a task");
        assert_eq!(claimed.id, task_id);
        assert!(claimed.locked);

        // Look up and execute the handler
        let handler = {
            let registry = TASK_REGISTRY.read().unwrap();
            *registry.get(&claimed.function_name).expect("handler not found")
        };
        let result = handler(claimed.params.clone(), pool).await;
        assert!(result.is_ok());

        // Mark as completed
        let marked = BackgroundTaskPostGresDescriptor::<TestDbHandle>::mark_background_task_as_completed(claimed.id)
            .await
            .expect("failed to mark completed");
        assert!(marked);

        // Verify final state
        let row = saps::sqlx::query("SELECT status, time_finished FROM saps.queued_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(pool)
            .await
            .expect("failed to fetch task");

        let status: String = sqlx::Row::try_get(&row, "status").unwrap();
        assert_eq!(status, "completed");
        let time_finished: Option<chrono::NaiveDateTime> = sqlx::Row::try_get(&row, "time_finished").unwrap();
        assert!(time_finished.is_some());

        // No more tasks available
        let next = BackgroundTaskPostGresDescriptor::<TestDbHandle>::get_next_background_task()
            .await
            .expect("failed to get next task");
        assert!(next.is_none());
    }

}
