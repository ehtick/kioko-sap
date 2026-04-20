use saps_background_task::background_task;
use saps::background_tasks::registry::TASK_REGISTRY;
use saps::background_tasks::dal::model::QueuedTask;
use saps::background_tasks::worker_pool::WorkerPool;
use saps::dal::connections::{LivePostGresPool, YieldPostGresPool};
use saps::sqlx::Executor;


#[background_task]
fn add(one: i32, two: i32) {
    let sum = one + two;
    println!("  [add] {} + {} = {}", one, two, sum);
}

#[background_task]
fn minus(one: i32, two: i32) {
    let _db_pool = pool;
    let diff = one - two;
    println!("  [minus] {} - {} = {}", one, two, diff);
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

    // Get the database connection pool
    let pool = LivePostGresPool::yield_pool();

    // Drop the table so we get a clean slate every run
    pool.execute("DROP TABLE IF EXISTS saps.queued_tasks CASCADE")
        .await
        .expect("failed to drop queued_tasks table");

    // Run migrations to recreate the table and stored procedures
    let sql = QueuedTask::generate_migration_sql();
    pool.execute(sql).await.expect("failed to run migrations");

    // Print registered background tasks
    let registry = TASK_REGISTRY.read().unwrap();
    println!("Registered tasks: {:?}", registry.keys().collect::<Vec<_>>());
    drop(registry);

    // Queue up a batch of add and minus tasks
    println!("\n=== Inserting tasks ===");
    add::<LivePostGresPool>(1, 2).await.expect("failed to insert add(1,2)");
    add::<LivePostGresPool>(10, 20).await.expect("failed to insert add(10,20)");
    add::<LivePostGresPool>(100, 200).await.expect("failed to insert add(100,200)");
    minus::<LivePostGresPool>(50, 30).await.expect("failed to insert minus(50,30)");
    minus::<LivePostGresPool>(99, 1).await.expect("failed to insert minus(99,1)");
    add::<LivePostGresPool>(7, 8).await.expect("failed to insert add(7,8)");
    minus::<LivePostGresPool>(1000, 1).await.expect("failed to insert minus(1000,1)");
    add::<LivePostGresPool>(42, 0).await.expect("failed to insert add(42,0)");
    println!("Inserted 8 tasks");

    // Start the worker pool with 2 workers
    println!("\n=== Starting worker pool (2 workers) ===");
    let mut worker_pool = WorkerPool::<LivePostGresPool>::new()
        .with_workers(2);
    worker_pool.init_workers();

    // Give workers time to process all tasks
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Dump the entire queued_tasks table
    println!("\n=== Full queued_tasks table ===");
    let rows = saps::sqlx::query("SELECT * FROM saps.queued_tasks ORDER BY time_posted ASC")
        .fetch_all(pool)
        .await
        .expect("failed to query queued_tasks");

    for row in &rows {
        let task = QueuedTask::from_row(row).expect("failed to parse row");
        println!(
            "  id={} fn={:<6} status={:<12} locked={:<5} params={} started={:?} finished={:?}",
            task.id, task.function_name, task.status, task.locked, task.params,
            task.time_started, task.time_finished
        );
    }
    println!("=== {} total rows ===", rows.len());
}
