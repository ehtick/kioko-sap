use saps::background_tasks::dal::model::QueuedTask;
use saps::background_tasks::registry::TASK_REGISTRY;
use saps::background_tasks::worker_pool::WorkerPool;
use saps::dal::connections::{LivePostGresPool, YieldPostGresPool};
use saps::scheduled_tasks::dal::model::{ScheduledTask, register_scheduled_task};
use saps::scheduled_tasks::scheduler::Scheduler;
use saps::sqlx::Executor;
use saps_background_task::background_task;

// `pool` is injected as a parameter into the generated core function, so
// every handler must reference it (or shadow it as `_`) to avoid an unused
// variable warning. Each handler just prints its name and the current UTC time.

#[background_task]
fn ping_30s() {
    let _ = pool;
    println!(
        "  [ping_30s]   tick @ {}",
        chrono::Utc::now().format("%H:%M:%S UTC")
    );
}

#[background_task]
fn ping_1m() {
    let _ = pool;
    println!(
        "  [ping_1m]    tick @ {}",
        chrono::Utc::now().format("%H:%M:%S UTC")
    );
}

#[background_task]
fn ping_2m() {
    let _ = pool;
    println!(
        "  [ping_2m]    tick @ {}",
        chrono::Utc::now().format("%H:%M:%S UTC")
    );
}

#[background_task]
fn daily_1915() {
    let _ = pool;
    println!(
        "  [daily_1915] tick @ {}",
        chrono::Utc::now().format("%H:%M:%S UTC")
    );
}

#[tokio::main]
async fn main() {
    // Set the DATABASE_URL env var to connect to the docker-compose postgres
    unsafe {
        std::env::set_var(
            "DATABASE_URL",
            "postgres://saps_worker:saps_worker_pass@localhost:5488/saps_workers",
        );
    }

    let pool = LivePostGresPool::yield_pool();

    // Wipe and recreate the queue table for a clean slate.
    pool.execute("DROP TABLE IF EXISTS saps.queued_tasks CASCADE")
        .await
        .expect("failed to drop queued_tasks table");
    pool.execute(QueuedTask::generate_migration_sql())
        .await
        .expect("failed to migrate queued_tasks");

    // ScheduledTask::generate_migration_sql() drops the table internally.
    pool.execute(ScheduledTask::generate_migration_sql())
        .await
        .expect("failed to migrate scheduled_tasks");

    // Show what the #[background_task] macro registered at startup.
    {
        let registry = TASK_REGISTRY.read().unwrap();
        let mut keys: Vec<&String> = registry.keys().collect();
        keys.sort();
        println!("Registered handlers: {:?}", keys);
    }

    // Register the four schedules. Cron format is 6-field:
    //   second  minute  hour  day-of-month  month  day-of-week
    println!("\n=== Registering schedules ===");
    register_scheduled_task::<LivePostGresPool>(
        "ping_30s",
        serde_json::json!({}),
        "*/30 * * * * *", // every 30 seconds
    )
    .await
    .expect("register ping_30s");

    register_scheduled_task::<LivePostGresPool>(
        "ping_1m",
        serde_json::json!({}),
        "0 * * * * *", // every minute on the :00
    )
    .await
    .expect("register ping_1m");

    register_scheduled_task::<LivePostGresPool>(
        "ping_2m",
        serde_json::json!({}),
        "0 */2 * * * *", // every 2 minutes on the :00
    )
    .await
    .expect("register ping_2m");

    register_scheduled_task::<LivePostGresPool>(
        "daily_1915",
        serde_json::json!({}),
        "0 23 18 * * *", // every day at 19:15:00 UTC
    )
    .await
    .expect("register daily_1915");
    println!("Registered 4 schedules.");

    // Start the worker pool that executes queued tasks.
    println!("\n=== Starting WorkerPool (2 workers, 5s poll) ===");
    let mut worker_pool = WorkerPool::<LivePostGresPool>::new().with_workers(2);
    worker_pool.init_workers();

    // Start the scheduler that posts due scheduled rows onto the queue.
    // 10s interval (instead of the 5-minute default) so the every-30s and
    // every-1-minute schedules become observable within the demo window.
    println!("=== Starting Scheduler (10s poll) ===");
    let mut scheduler = Scheduler::<LivePostGresPool>::new().with_interval(10);
    scheduler.init();

    // Run for 4 minutes so we can observe several firings of each schedule.
    let demo_secs = 240;
    println!(
        "\n=== Running for {}s — ctrl-C to exit early ===\n",
        demo_secs
    );
    tokio::time::sleep(tokio::time::Duration::from_secs(demo_secs)).await;

    // Dump the final state of both tables so we can audit what fired.
    println!("\n=== Final saps.scheduled_tasks ===");
    let rows = saps::sqlx::query("SELECT * FROM saps.scheduled_tasks ORDER BY id ASC")
        .fetch_all(pool)
        .await
        .expect("failed to query scheduled_tasks");
    for row in &rows {
        let task = ScheduledTask::from_row(row).expect("failed to parse row");
        println!(
            "  id={} fn={:<11} next={:?} last_fired={:?} task_id={:?} locked={}",
            task.id,
            task.function_name,
            task.time_scheduled,
            task.time_completed,
            task.task_id,
            task.locked,
        );
    }

    println!("\n=== Final saps.queued_tasks ===");
    let rows = saps::sqlx::query("SELECT * FROM saps.queued_tasks ORDER BY time_posted ASC")
        .fetch_all(pool)
        .await
        .expect("failed to query queued_tasks");
    for row in &rows {
        let task = QueuedTask::from_row(row).expect("failed to parse row");
        println!(
            "  fn={:<11} status={:<10} posted={} finished={:?}",
            task.function_name, task.status, task.time_posted, task.time_finished,
        );
    }
    println!("=== {} queued rows total ===", rows.len());

    loop {
        tokio::time::sleep(tokio::time::Duration::from_mins(1)).await;
        println!("polling");
    }
}
