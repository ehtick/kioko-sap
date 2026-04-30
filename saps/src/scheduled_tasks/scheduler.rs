//! Recurring scheduler that posts due cron-driven tasks onto the background queue.
//!
//! [`Scheduler`] spawns a single async task that wakes every `interval` seconds
//! (default: 300 = 5 minutes), calls
//! [`get_due_scheduled_task`](super::dal::tx_definitions::GetDueScheduledTasks),
//! and for every claimed row:
//!
//! 1. Parses the row's `cron_string` to compute the next firing time.
//! 2. Pre-generates a UUID for the soon-to-be-created `queued_tasks` row.
//! 3. Calls [`post_scheduled_task`](super::dal::tx_definitions::PostScheduledTask),
//!    which atomically (in a single SQL transaction) inserts the queue row and
//!    advances the schedule row.
//!
//! The existing [`WorkerPool`](crate::background_tasks::worker_pool::WorkerPool)
//! is responsible for actually running each posted task — the scheduler is
//! purely a producer for the queue.
//!
//! # Example
//!
//! ```ignore
//! use saps::scheduled_tasks::scheduler::Scheduler;
//! use saps::dal::connections::LivePostGresPool;
//!
//! #[tokio::main]
//! async fn main() {
//!     let mut scheduler = Scheduler::<LivePostGresPool>::new()
//!         .with_interval(300); // poll every 5 minutes
//!     scheduler.init();
//!
//!     // Run your server / wait for shutdown
//!     tokio::signal::ctrl_c().await.unwrap();
//! }
//! ```

use std::marker::{PhantomData, Send, Sync};
use std::str::FromStr;
use tokio::task::JoinHandle;

use crate::dal::connections::{ScheduledTaskPostGresDescriptor, YieldPostGresPool};
use crate::scheduled_tasks::dal::{
    model::ScheduledTask,
    tx_definitions::{GetDueScheduledTasks, PostScheduledTask},
};

/// A single-actor scheduler that polls `saps.scheduled_tasks` on a fixed
/// interval and posts due rows onto `saps.queued_tasks`.
///
/// Generic over `Y: YieldPostGresPool`, which determines the connection pool
/// the actor uses (`LivePostGresPool` in production).
pub struct Scheduler<Y: YieldPostGresPool + Sync + Send> {
    db_pool: PhantomData<Y>,
    /// Polling interval in seconds. Defaults to 300 (5 minutes).
    interval: usize,
    /// `JoinHandle` of the spawned actor task, populated by [`init`](Self::init).
    handle: Option<JoinHandle<()>>,
}

impl<Y: YieldPostGresPool + Sync + Send + 'static> Scheduler<Y> {
    /// Creates a new `Scheduler` polling every 5 minutes.
    pub fn new() -> Self {
        Self {
            db_pool: PhantomData::<Y>,
            interval: 300,
            handle: None,
        }
    }

    /// Overrides the polling interval (in seconds).
    pub fn with_interval(mut self, secs: usize) -> Self {
        self.interval = secs;
        self
    }

    /// Spawns the scheduler actor on the current tokio runtime.
    ///
    /// Must be called from within a tokio runtime context.
    pub fn init(&mut self) {
        let interval = self.interval;
        self.handle = Some(tokio::task::spawn(async move {
            scheduler_cycle::<Y>(interval).await
        }));
    }
}

/// The actor's main loop. Runs forever, logging and continuing on every error.
async fn scheduler_cycle<Z: YieldPostGresPool + Sync + Send>(interval: usize) {
    tracing::info!("scheduler starting (interval = {}s)", interval);
    let interval_duration = tokio::time::Duration::from_secs(interval as u64);

    loop {
        let due = match ScheduledTaskPostGresDescriptor::<Z>::get_due_scheduled_task().await {
            Ok(v) => v,
            Err(error) => {
                tracing::error!("scheduler error claiming due tasks: {}", error);
                tokio::time::sleep(interval_duration).await;
                continue;
            }
        };

        for task in due {
            let schedule = match cron::Schedule::from_str(&task.cron_string) {
                Ok(s) => s,
                Err(error) => {
                    tracing::error!(
                        "scheduler bad cron '{}' on row id {}: {}",
                        task.cron_string,
                        task.id,
                        error
                    );
                    continue;
                }
            };

            let next = schedule
                .upcoming(chrono::Utc)
                .next()
                .map(|dt| dt.naive_utc());
            let now = chrono::Utc::now().naive_utc();

            let updated = ScheduledTask {
                task_id: Some(uuid::Uuid::new_v4()),
                time_scheduled: next,
                time_completed: Some(now),
                ..task
            };
            let row_id = updated.id;

            if let Err(error) =
                ScheduledTaskPostGresDescriptor::<Z>::post_scheduled_task(updated).await
            {
                tracing::error!("scheduler error posting row id {}: {}", row_id, error);
                // Row is left with locked = TRUE; next tick will skip it until
                // the lock is manually cleared.
            }
        }

        tokio::time::sleep(interval_duration).await;
        tracing::debug!("scheduler polling");
    }
}
