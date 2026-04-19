use saps_background_task::background_task;
use saps::background_tasks::registry::TASK_REGISTRY;
use saps::background_tasks::dal::model::QueuedTask;
use saps::dal::connections::{LivePostGresPool, YieldPostGresPool};
use saps::sqlx::Executor;
use serde_json::json;


#[background_task]
fn add(one: i32, two: i32) {
    let sum = one + two;
    println!("sum: {}", sum);
}

#[background_task]
fn minus(one: i32, two: i32) {
    let sum = one - two;
    println!("minus: {}", sum);
}


async fn run_migrations(pool: &saps::sqlx::Pool<saps::sqlx::Postgres>) {
    let sql = QueuedTask::generate_migration_sql();
    pool.execute(sql).await.expect("failed to run background task migrations");
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

    // Get the database connection pool (LazyLock initializes on first access,
    // reads DATABASE_URL and DB_MAX_CONNECTIONS from env)
    let pool = LivePostGresPool::yield_pool();

    // Run migrations to create the queued_tasks table and stored procedures
    run_migrations(pool).await;

    // Print registered background tasks
    let registry = TASK_REGISTRY.read().unwrap();
    println!("Registered tasks: {:?}", registry.keys().collect::<Vec<_>>());
    drop(registry);

    // Example: insert a task via the generated interface function
    let result = add::<LivePostGresPool>(1, 4).await;
    println!("Task inserted: {:?}", result);

    // Example: call the core function directly with a JSON Value
    let package = json!({
        "one": 1,
        "two": 4
    });
    let _ = saps_background_core_add(package).await;

    // Claim the next task from the DB
    use saps::background_tasks::dal::tx_definitions::{GetNextBackgroundTask, MarkBackgroundTaskAsCompleted};
    use saps::dal::connections::BackgroundTaskPostGresDescriptor;

    let task = BackgroundTaskPostGresDescriptor::<LivePostGresPool>::get_next_background_task()
        .await
        .expect("failed to get next task");

    match task {
        Some(task) => {
            println!("Claimed task: id={}, function={}, params={}", task.id, task.function_name, task.params);

            // Look up the handler in the registry and execute it
            let registry = TASK_REGISTRY.read().unwrap();
            let handler = registry.get(&task.function_name)
                .expect(&format!("no handler registered for '{}'", task.function_name));
            let result = handler(task.params.clone()).await;
            drop(registry);

            match result {
                Ok(()) => {
                    println!("Task {} completed successfully", task.id);
                    BackgroundTaskPostGresDescriptor::<LivePostGresPool>::mark_background_task_as_completed(task.id)
                        .await
                        .expect("failed to mark task as completed");
                }
                Err(e) => {
                    println!("Task {} failed: {}", task.id, e);
                }
            }
        }
        None => {
            println!("No pending tasks found");
        }
    }

    // Dump the entire queued_tasks table to see how it evolved
    println!("\n=== Full queued_tasks table ===");
    let rows = saps::sqlx::query("SELECT * FROM saps.queued_tasks ORDER BY time_posted ASC")
        .fetch_all(pool)
        .await
        .expect("failed to query queued_tasks");

    for row in &rows {
        let task = QueuedTask::from_row(row).expect("failed to parse row");
        println!(
            "  id={} fn={} status={} locked={} params={} posted={} started={:?} finished={:?}",
            task.id, task.function_name, task.status, task.locked, task.params,
            task.time_posted, task.time_started, task.time_finished
        );
    }
    println!("=== {} total rows ===", rows.len());
}
