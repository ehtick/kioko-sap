# SAPS

This framework combines `Svelte`, `Axum`, `Postgres`, and `sqlx` with helpful macros to enable you to test every part of your system with isolated postgres DBs for each test enabling you to run end to end tests in a multithreaded runtime. It also embeds the frontend into the Rust binary and mounts it to the server so your Axum server is serving the frontend and backend within one binary. Auth cookies are also handled and background/cron jobs are also handled using postgres as task status persistence.

## Contents

- [DB Transactions](#db-transactions)
- [Config Variables](#config-variables)
- [Background Tasks](#background-tasks)
- [Other things that are being worked on](#other-things-that-are-being-worked-on)

## DB Transactions

DB transactions are the core of this framework because testing and mounting different databases rely on the way we handle DB transactions.

### Yielding the DB Pool

The simplest way of handling a DB connection is just yielding the postgres pool. This is a trait that can be utilised as a stateless generic parameter as seen below:

> [!WARNING]
> For the `#[db_test]` to run you need a postgres DB running separately with the following connection params:
>
> ```
> postgres://username:password@localhost:5434/main_db
> ```
>
> Each test will create a unique DB within that postgres instance so each test is isolated and threadsafe but for now it's just the easiest and quickest way. It works well for CI too. I will work on a nicer, smoother way of handling the postgres instance in the background in the future.

```rust
use saps::dal::connections::{SqlxPostGresDescriptor, YieldPostGresPool, LivePostGresPool};
use saps::auth::dal::run_script::run_sql_script;
use saps::errors::saps::SapsError;
use saps::sqlx::{Pool, Postgres};

// Declare the function with generics
pub async fn prep<X: YieldPostGresPool>() -> Result<(), SapsError> {
    run_sql_script(X::yield_pool(), "./path/to/setup.sql")
            .await 
}

// Declare the function accepting a direct pool
pub async fn prep_with_pool(pool: &Pool<Postgres>) -> Result<(), SapsError> {
    run_sql_script(pool, "./path/to/setup.sql")
            .await 
}

#[cfg(test)]
mod tests {

    use super::*;
    use saps::db_test;

    #[db_test]
    async fn test_prep<TestDbHandle: YieldPostGresPool>(pool: &Pool<Postgres>) {
        // The TestDbHandle yields the pool specifically for an isolated postgres DB
        let outcome = prep::<TestDbHandle>().await;
    }

    #[db_test]
    async fn test_prep_with_pool<TestDbHandle: YieldPostGresPool>(pool: &Pool<Postgres>) {
        // A reference to the DB pool for the isolated DB for the test is also provided as pool
        let outcome = prep_with_pool(pool).await;
    }

}
```

### DB Transactions

Saps also supports individual DB transactions if you want to mock them in unit tests. However, this is done by the db transaction macro defining an individual trait per transaction. This will slow down your compilation times but you will get more fine grained control over individual db transactions and mocking them. I personally use db transactions in production and I'm happy with my compilation times.

We can define DB transactions with the following code:

```rust
use saps::define_dal_transactions;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct User {
    pub id: uuid::Uuid,
    pub username: String,
    pub email: String,
    pub password_hash: String,
}

define_dal_transactions!(
    CreateUser => create_user(username: String, email: String, password_hash: String) -> User,
    GetUserByEmail => get_user_by_email(email: String) -> Option<User>,
    GetUserById => get_user_by_id(user_id: uuid::Uuid) -> Option<User>,
    DeleteUser => delete_user(user_id: uuid::Uuid) -> bool
);
```

We can then implement these transactions to a db descriptor with the code below:

```rust
use saps::dal::connections::SqlxPostGresDescriptor;
use saps::db_transaction;
use super::tx_definitions::CreateUser;

#[db_transaction(SqlxPostGresDescriptor, CreateUser)]
async fn create_user(username: String, email: String, password_hash: String) -> User {
    // The T is an internal yield postgres pool inside the SqlxPostGresDescriptor struct.
    let pool = T::yield_pool();
    let row = saps::sqlx::query_as::<_, (uuid::Uuid, String, String, String)>(
        r#"
        INSERT INTO users (username, email, password_hash)
        VALUES ($1, $2, $3)
        RETURNING id, username, email, password_hash
        "#,
    )
        .bind(&username)
        .bind(&email)
        .bind(&password_hash)
        .fetch_one(pool)
        .await?;
    Ok(User {
        id: row.0,
        username: row.1,
        email: row.2,
        password_hash: row.3,
    })
}
```

This binds the `SqlxPostGresDescriptor` to the `create_user` transaction. We can use this descriptor with the code below:

```rust
use super::tx_definitions::{CreateUser, User};


async fn create_user<X: CreateUser>(username: String, email: String, password_hash: String) -> Result<(), saps::sqlx::Error> {
    X::create_user(username, email, password_hash).await
}


#[cfg(test)]
mod tests {

    use super::*;
    use saps::db_test;
    use saps::dal::connections::SqlxPostGresDescriptor;
    use saps::dal::connections::YieldPostGresPool;

    #[db_test]
    async fn test_create_user<TestDbHandle: YieldPostGresPool>(pool: &Pool<Postgres>) {
        // Here we can see that the `SqlxPostGresDescriptor` accepts the `TestDbHandle` so connects to the
        // test db
        let outcome = create_user::<SqlxPostGresDescriptor<TestDbHandle>>(
            "maxwell".to_string(),
            "max@gmail.com".to_string(),
            "hashed-password".to_string()
        ).await;
    }

    // mock the DB
    struct MockDbHandle<T: YieldPostGresPool> {
        db_handle: PhantomData<T>,
    }

    #[db_transaction(MockDbHandle, CreateUser)]
    async fn create_user(username: String, email: String, password_hash: String) -> User {
        // check the input of the mock
        if username != "maxwell".to_string() {
            panic!("username should be 'maxwell'");
        }
        Ok(User{
            id: uuid::Uuid::new_v4(),
            username: "maxwell".to_string(),
            email: "max@gmail.com".to_string(),
            password_hash: "hashed-password".to_string()
        })
    }

    #[db_test]
    async fn test_create_user_with_mock<TestDbHandle: YieldPostGresPool>(pool: &Pool<Postgres>) {
        // Here we can see that the `MockDbHandle` is now passed into the function we're testing
        let outcome = create_user::<MockDbHandle<TestDbHandle>>(
            "maxwell".to_string(),
            "max@gmail.com".to_string(),
            "hashed-password".to_string()
        ).await;
    }

}
```

### Mounting views to server

When mounting to a server it's advised to use a factory pattern like the following code:

```rust
pub mod create;

use saps::axum::{Router, routing::{get, post, delete as delete_method}};
use saps::config::GetConfigVariable;
use saps::dal::connections::{
    LivePostGresPool, SqlxPostGresDescriptor,
};

/// Attaches all user-related views to the router.
pub fn users_factory(app: Router) -> Router {
    app.route(
        "/api/v1/users",
        post(
            create::create_user_handler::<SqlxPostGresDescriptor<LivePostGresPool>>,
        ),
    )
}
```

Here we can see that the `SqlxPostGresDescriptor` takes in the `LivePostGresPool`. This is a oncelocked live postgres connection pool that requires the two environment variables below:

- `"DATABASE_URL"`: The URL connection string to the database
- `"DB_MAX_CONNECTIONS"`: The maximum number of connections that the connection pool has

### Defining your own postgres pools

You can have multiple PG pools to different databases. Below is how we can build a pool and handle:

```rust
use saps::define_pg_pool;
use saps::dal::connections::YieldPostGresPool;

define_pg_pool!(SECOND_LIVE_POOL, "DATABASE_URL_TWO", "DB_MAX_CONNECTIONS_TWO");

pub struct SecondLivePool;

impl YieldPostGresPool for SecondLivePool {

    fn yield_pool() -> &'static saps::sqlx::Pool<sqlx::Postgres> {
        &SECOND_LIVE_POOL
    }

}
```

This gives us another oncelocked pool under the `SECOND_LIVE_POOL` variable that requires the two environment variables below:

- `"DATABASE_URL_TWO"`: The URL connection string to the database
- `"DB_MAX_CONNECTIONS_TWO"`: The maximum number of connections that the connection pool has


## Config Variables

We can pass in the ability to get config variables with the following code:

```rust
use saps::config::GetConfigVariable;
use saps::errors::saps::SapsError;


pub fn check_var<C: GetConfigVariable>() -> Result<String, SapsError> {
    C::get_config_variable("NAME".to_string())
}


#[cfg(test)]
mod tests {

    use super::*;
    use saps::define_static_config;

    // define a static struct for config that maps keys on the left
    // to values on the right for testing
    define_static_config!(
        TestConfig,
        "NAME" => "maxwell"
    );

    #[test]
    fn test_check_var() {
        let outcome: String = check_var::<TestConfig>().expect("variable is present");
        assert_eq!(outcome, "maxwell");
    }
}
```

You can mount the config var struct just like you would mounting a DB handle to the server with a factory function. For ease you can use the `use saps::config::EnvConfig;` but this will check config variables for every lookup which is not optimal and surprisingly slow when there's contention. You can use the `define_env_config` macro for optimal config lookups with the following code:

```rust
use saps::define_env_config;
use saps::errors::saps::SapsError;

define_env_config!(LiveConfig, "DB_CONNECTION", "SECRET_KEY", "RATE_LIMIT");

fn main() {
    let result: Result<(), SapsError> = LiveConfig::init();
}
```

What happens here essentially is that the `LiveConfig::init()` loops through all the keys provided and gets them from the environment variables. This means you will fail fast if an environment variable is missing. The macro creates oncelocks for each key and a match statement returning the specific oncelock variable depending on the key passed in. This gives us lock free reads that are faster than a hashmap until the number of keys gets into the 100s. Then it is advised that you should look into hashmaps. Once the `init` is called, the config cannot be altered, or reset for the duration of the program.

It must be noted that every lookup clones the value at this point in time. This isn't too bad for now but will work on removing this and also removing the `to_string` requirement for passing in the key.


## Background Tasks

The persistence of background tasks is handled in a non-public schema in postgres. With saps, we can shoot off random background tasks to the execution queue and you can also schedule background tasks for individual times or periods. First, we need the following imports:

```rust
use saps_background_task::background_task;
use saps::background_tasks::dal::model::QueuedTask;
use saps::background_tasks::worker_pool::WorkerPool;
use saps::scheduled_tasks::dal::model::{ScheduledTask, register_scheduled_task};
use saps::scheduled_tasks::scheduler::Scheduler;
use saps::dal::connections::{LivePostGresPool, YieldPostGresPool};
use saps::sqlx::Executor;
```

We can then define the background tasks with the `background_task` macro with the following code:

```rust
#[background_task]
async fn add(one: i32, two: i32) {
    println!("add result: {}", one + two);
}

#[background_task]
fn ping_30s() {
    let _ = pool;
    println!("  [ping_30s]   tick @ {}", chrono::Utc::now().format("%H:%M:%S UTC"));
}

#[background_task]
fn ping_1m() {
    let _ = pool;
    println!("  [ping_1m]    tick @ {}", chrono::Utc::now().format("%H:%M:%S UTC"));
}

#[background_task]
fn ping_2m() {
    let _ = pool;
    println!("  [ping_2m]    tick @ {}", chrono::Utc::now().format("%H:%M:%S UTC"));
}

#[background_task]
fn daily_1915() {
    let _ = pool;
    println!("  [daily_1915] tick @ {}", chrono::Utc::now().format("%H:%M:%S UTC"));
}
```

We can then initialize the db schema with the code below:

```rust
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
```

We now have the DB state setup for background tasks. We can now register some of these background tasks to run at certain intervals or times with the following code:

```rust
register_scheduled_task::<LivePostGresPool>(
    "ping_30s",
    serde_json::json!({}),
    "*/30 * * * * *", // every 30 seconds
).await.expect("register ping_30s");

register_scheduled_task::<LivePostGresPool>(
    "ping_1m",
    serde_json::json!({}),
    "0 * * * * *", // every minute on the :00
).await.expect("register ping_1m");

register_scheduled_task::<LivePostGresPool>(
    "ping_2m",
    serde_json::json!({}),
    "0 */2 * * * *", // every 2 minutes on the :00
).await.expect("register ping_2m");

register_scheduled_task::<LivePostGresPool>(
    "daily_1915",
    serde_json::json!({}),
    "0 15 19 * * *", // every day at 19:15:00 UTC
).await.expect("register daily_1915");
```

Our background tasks are now scheduled. We can kick off our worker pool and scheduler with the code below:

```rust
let mut worker_pool = WorkerPool::<LivePostGresPool>::new()
    .with_workers(2);
worker_pool.init_workers();

// Start the scheduler that posts due scheduled rows onto the queue.
// 10s interval (instead of the 5-minute default) so the every-30s and
// every-1-minute schedules become observable within the demo window.
println!("=== Starting Scheduler (10s poll) ===");
let mut scheduler = Scheduler::<LivePostGresPool>::new()
    .with_interval(10);
scheduler.init();
```

We can also shoot off an ad-hoc task to be processed on the worker queue with the following code:

```rust
let outcome = add::<LivePostGresPool>(1, 2).await;
```

Note that the `::<LivePostGresPool>` was added to the `add` function. This is because we need a handle to access the db pool. Remember, because it has the DB pool handle these background tasks can be involved in `#[db_test]` tests.


## Other things that are being worked on

- cookie based auth sessions with role checks.

