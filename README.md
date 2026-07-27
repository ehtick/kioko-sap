# SAPS

This framework combines `Svelte`, `Axum`, `Postgres`, and `sqlx` with helpful macros to enable you to test every part of your system with isolated postgres DBs for each test enabling you to run end to end tests in a multithreaded runtime. It also embeds the frontend into the Rust binary and mounts it to the server so your Axum server is serving the frontend and backend within one binary. Auth cookies are also handled and background/cron jobs are also handled using postgres as task status persistence.

## Contents

- [DB Transactions](#db-transactions)
- [Config Variables](#config-variables)
- [Auth](#auth)
- [Embedding and serving a frontend](#embedding-and-serving-a-frontend)
- [Background Tasks](#background-tasks)
- [Roadmap](#roadmap)

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

### Embedded PostgreSQL pool

If you don't want to point at an external database — for staging deploys, self-contained demo binaries, or local development without docker — saps can boot a real PostgreSQL process inside your binary and hand back an `sqlx::PgPool` connected to it. The embedded server is provided by [`postgresql_embedded`](https://crates.io/crates/postgresql_embedded).

Turn it on by enabling the `embedded_postgres` feature on saps in your crate's `Cargo.toml`:

```toml
[dependencies]
saps = { version = "0.4", features = ["embedded_postgres"] }
```

The feature unlocks the [`saps::dal::embedded_connections`](saps/src/dal/embedded_connections.rs) module which ships a framework-integrated `YieldPostGresPool` handle so the embedded pool slots directly into every component that already takes a postgres handle (DAL transactions, `AuthPostGresDescriptor`, the worker pool, etc.). Booting embedded postgres is async (it may download binaries to `~/.theseus/postgresql` on first run), so the handle is paired with an async `init_live_embedded_postgres_pool()` initializer that you `await` once at startup:

```rust
use saps::dal::connections::YieldPostGresPool;
use saps::dal::embedded_connections::{
    LiveEmbeddedPostgresPool, init_live_embedded_postgres_pool,
};

#[tokio::main]
async fn main() {
    // Boots the embedded server, creates EMBEDDED_DATABASE_NAME if it doesn't
    // already exist, and connects a PgPool to it. Subsequent calls are no-ops
    // and return the cached pool.
    init_live_embedded_postgres_pool().await;

    // Now anything keyed on YieldPostGresPool can take LiveEmbeddedPostgresPool
    // in place of LivePostGresPool.
    let pool = LiveEmbeddedPostgresPool::yield_pool();
    sqlx::query("CREATE TABLE IF NOT EXISTS notes (id SERIAL PRIMARY KEY, body TEXT NOT NULL)")
        .execute(pool)
        .await
        .unwrap();
}
```

The handle reads two environment variables:

- `"EMBEDDED_DATABASE_NAME"`: name of the database to create inside the embedded server
- `"DB_MAX_CONNECTIONS"`: maximum number of connections that the `PgPool` holds (defaults to `5` if unset)

A keep-alive static inside the module holds the running `postgresql_embedded::PostgreSQL` handle for the lifetime of the program, so the child process never gets dropped while the pool is in use.

To wire it into the rest of the framework, hand `LiveEmbeddedPostgresPool` to any descriptor that already accepts a `YieldPostGresPool` parameter — for example a router factory that previously took `LivePostGresPool`:

```rust
use saps::axum::{Router, routing::post};
use saps::dal::connections::SqlxPostGresDescriptor;
use saps::dal::embedded_connections::LiveEmbeddedPostgresPool;

pub fn users_factory(app: Router) -> Router {
    app.route(
        "/api/v1/users",
        post(create::create_user_handler::<SqlxPostGresDescriptor<LiveEmbeddedPostgresPool>>),
    )
}
```

For a runnable end-to-end demo (boot embedded postgres, create a table, insert/read rows back) see [`examples/embedded-db`](examples/embedded-db).

#### Defining your own embedded pools

If you want a second embedded pool (or want to swap env-var names), the underlying machinery is exposed as the [`saps::define_embedded_pg_pool!`](macros/db-embedded-pool-macro) macro, mirroring the shape of `define_pg_pool!`:

```rust
use saps::define_embedded_pg_pool;

define_embedded_pg_pool!(SECOND_EMBEDDED_POOL, "SECOND_EMBEDDED_DB_NAME", "DB_MAX_CONNECTIONS_TWO");

// At startup:
let pool: &'static sqlx::PgPool = init_second_embedded_pool().await;
```

Each invocation emits a `tokio::sync::OnceCell<PgPool>` named after the pool identifier, a keep-alive `OnceCell<PostgreSQL>` that holds the server handle, and an async `init_<pool_lowercase>()` initializer that returns `&'static PgPool`. Implement `YieldPostGresPool` on a marker struct the same way `LiveEmbeddedPostgresPool` does to plug the new pool into the rest of the framework.

> [!NOTE]
> By default the postgres binaries are downloaded on first run and cached under `~/.theseus/postgresql`. To bake the binaries directly into your release artifact instead, enable `postgresql_embedded`'s `bundled` feature in your own `Cargo.toml` — the archive is then embedded at build time and no network access is required at runtime.


## Config Variables

We can pass in the ability to get config variables with the following code:

```rust
use saps::config::GetConfigVariable;
use saps::errors::saps::SapsError;


pub fn check_var<C: GetConfigVariable>() -> Result<String, SapsError> {
    C::get_config_variable("NAME")
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

It must be noted that every lookup clones the value at this point in time. This isn't too bad for now but will work on removing this.


## Auth

Saps ships with cookie-based auth sessions backed by a row in `saps.auth_sessions` and a JWT that lives in an HttpOnly `saps-token` cookie. The cycle is:

1. After a user logs in, you create an `AuthSession` row with whatever you want to remember about that session in its `meta` JSONB column (e.g. `user_id`, `user_role`, the department they're scoped to).
2. You hand the user back a JWT whose `unique_id` is the session row's UUID, set as the `saps-token` cookie.
3. On every subsequent request the `HeaderToken` extractor pulls the JWT, pings the session row (which extends `last_interacted` and rotates the UUID after 5 minutes of inactivity), and attaches the full `AuthSession` to the token before your handler runs.
4. Your handler reads typed meta values directly off the token — no extra DB round-trip — and can update meta atomically via the same token.

Login and logout get their own examples later in the docs. This section assumes a session has already been created and focuses on what your handlers do with it.

### Auth lifecycle

Every request that uses a `HeaderToken` extractor goes through the following inside `from_request_parts`:

1. Extract the JWT from the `saps-token` cookie, the `token` header, or `Authorization: Bearer …` (in that order).
2. Decode and verify the HS256 signature using `SECRET_KEY`.
3. Call `saps.ping(10, session_id)`. This either bumps `last_interacted`, regenerates the session UUID once `date_created` is older than 5 minutes, or returns `NULL` for a session that's been idle for 10 minutes.
4. Run the role check (e.g. `AdminRoleCheck`, `NoRoleCheck`) against the session's role.
5. Stash the loaded `AuthSession` on the token and hand it to your handler.

If step 3 rotated the UUID, the [middleware](#middleware) attaches a fresh `Set-Cookie` to the response automatically — your handler doesn't have to do anything special.

### Reading session meta

The `meta` column on `saps.auth_sessions` is a JSONB blob you control. Anything you put in there during login is available on the token without going back to the DB. Each accessor comes in two flavours:

- **`_local`** — synchronous, reads the cached session attached to the token (what the extractor loaded). Fast, no DB round-trip.
- **plain** — async, calls `refresh_auth_session` first so the read is up-to-the-instant. Use these when another task may have written to the session since the extractor ran.

```rust
use saps::auth::token::header_token::HeaderToken;

// Cached read — no DB call.
let user_id: i32 = token.meta_get_typed_strict_local("user_id")?;

// Fresh read — refreshes the session row from the DB first.
let user_id: i32 = token.meta_get_typed_strict("user_id").await?;

// Optional value — returns Ok(None) if the key isn't there (no error).
let project_id: Option<i32> = token.meta_get_typed_owned_local("project_id")?;

// Borrowed deserialization — &str borrows from the cached Value, no allocation.
let team: &str = token.meta_get_typed_strict_local("team")?;
```

The `_strict` variants return `SapsError` with status `NotFound` when the key is absent (or `BadRequest` when the value can't be decoded as `T`). The non-strict variants return `Result<Option<T>, SapsError>` — missing key is `Ok(None)`, decode failures are still errors. The `_owned` variants always allocate; the borrowed variants let `&str` and other zero-copy types share storage with the cached `Value`.

### Updating session meta

Every meta-mutation transaction in `tx_definitions.rs` has a wrapper on the token. Each one goes through `AuthPostGresDescriptor::<Z>` under the hood and refreshes the cached session on success, so a subsequent `_local` read sees the new value.

```rust
// Replace the whole meta blob.
token.update_auth_session_meta(serde_json::json!({"user_id": 7})).await?;

// Insert or overwrite a single key, leaving others intact.
token.upsert_auth_session_meta_key("project_id", serde_json::json!(42)).await?;

// Remove a key.
token.delete_auth_session_meta_key("project_id").await?;

// Atomic compare-and-swap. Returns true only if the swap actually happened.
let won = token.compare_and_swap_auth_session_meta(
    "nonce",
    serde_json::json!("old"),
    serde_json::json!("new"),
).await?;
```

### Enforcing uniqueness on a meta key

If you want at most one active session per user, install a partial unique index on `meta->>key` during migration:

```rust
use saps::auth::dal::model::AuthSession;
use saps::dal::connections::LivePostGresPool;

// Run after the base schema migration. Each entry in the slice creates one
// partial unique index — sessions whose meta doesn't have the key are
// excluded from the index, so unbound sessions never collide.
AuthSession::<MyRole>::run_migration::<LivePostGresPool>(&["user_id"]).await?;
```

A duplicate insert against a covered key fails with `sqlx::Error::Database` whose `is_unique_violation()` is `true`.

### Enforcing uniqueness on a meta key pair

A single-key index forces one session per `user_id` *globally*. If the same user needs an active session on more than one server (e.g. an app server and a metrics server), scope uniqueness to a pair of meta keys — typically `user_id` plus a `server_tag`:

```rust
use saps::auth::dal::model::AuthSession;
use saps::dal::connections::{LivePostGresPool, YieldPostGresPool};

// Base schema first — pass &[] if you don't also want a single-key index.
AuthSession::<MyRole>::run_migration::<LivePostGresPool>(&[]).await?;

// Composite partial unique index over (meta->>'user_id', meta->>'server_tag').
// Sessions missing either key are excluded from the index, so they never
// collide with one another.
let sql = AuthSession::<MyRole>::generate_unique_meta_key_pair_sql(
    "user_id",
    "server_tag",
);
sqlx::raw_sql(&sql)
    .execute(LivePostGresPool::yield_pool())
    .await?;
```

With this index in place, `(user_id=1, server_tag="app")` and `(user_id=1, server_tag="metrics")` can coexist, but a second `(user_id=1, server_tag="app")` insert fails with a unique violation in the same way as the single-key case.

### Looking up sessions by meta

The session UUID is the primary lookup key, but you often have a domain identifier (`user_id`, an external service id, etc.) in `meta` and want to find the session that way. Every meta-key query uses the JSONB containment operator `@>`, so a single GIN index on `meta` accelerates all of them.

```rust
use saps::auth::dal::tx_definitions::{
    GetAuthSessionByMetaKey, GetAuthSessionByMetaKeyStrict, GetAuthSessionsByMetaKey,
    GetAuthSessionsByMetaKeyPair,
};
use saps::dal::connections::{AuthPostGresDescriptor, LivePostGresPool};

// All sessions matching one meta key/value — no uniqueness assumed.
let sessions = AuthPostGresDescriptor::<LivePostGresPool>
    ::get_auth_sessions_by_meta_key::<MyRole>("user_id", serde_json::json!(42))
    .await?;

// At most one session — pair with the single-key partial unique index from
// generate_unique_meta_key_sql so this is guaranteed to be the unique row.
let session = AuthPostGresDescriptor::<LivePostGresPool>
    ::get_auth_session_by_meta_key::<MyRole>("user_id", serde_json::json!(42))
    .await?;

// Strict variant — returns sqlx::Error::RowNotFound instead of Ok(None) when
// no session matches. Useful when the caller knows the session must exist.
let session = AuthPostGresDescriptor::<LivePostGresPool>
    ::get_auth_session_by_meta_key_strict::<MyRole>("user_id", serde_json::json!(42))
    .await?;

// Composite lookup — typically used with the generate_unique_meta_key_pair_sql
// index so this returns one row, but the API is Vec because the query stays
// correct without the index.
let sessions = AuthPostGresDescriptor::<LivePostGresPool>
    ::get_auth_sessions_by_meta_key_pair::<MyRole>(
        "user_id", serde_json::json!(42),
        "server_tag", serde_json::json!("app"),
    )
    .await?;
```

Sessions whose `meta` is `NULL` or whose `meta` is missing any of the queried keys never match — `@>` is strict containment, so a row with `{"user_id": 42}` does not match a lookup for `("user_id", 42, "server_tag", "app")`.

### Upserting meta across sessions

The token-scoped wrappers in [Updating session meta](#updating-session-meta) target one row — the session the JWT is bound to. The DAL also exposes two cross-session upserts that match by `meta` content rather than by session UUID. They're useful when you want to set the same flag on every session belonging to a user, or to one specific `(user, server)` slot, without first looking each row up.

Both are a single `UPDATE` that combines a `meta @> jsonb_build_object(...)` filter with `jsonb_set(COALESCE(meta, '{}'::jsonb), ARRAY[upsert_key], upsert_value, true)` — so the match and the write are atomic, and a row with `NULL` meta is treated as `'{}'` when the upsert applies (though `NULL` rows can't pass the `@>` filter, so they're never touched in practice).

```rust
use saps::auth::dal::tx_definitions::{
    UpsertAuthSessionsMetaKeyByMeta, UpsertAuthSessionsMetaKeyByMetaKeyPair,
};
use saps::dal::connections::{AuthPostGresDescriptor, LivePostGresPool};

// Force every session for user 42 to take the "premium" flag.
let count = AuthPostGresDescriptor::<LivePostGresPool>
    ::upsert_auth_sessions_meta_key_by_meta(
        "user_id", serde_json::json!(42),
        "plan", serde_json::json!("premium"),
    )
    .await?;

// Set "level"=3 only on user 42's app-server session, leaving any
// metrics-server session for the same user untouched.
let count = AuthPostGresDescriptor::<LivePostGresPool>
    ::upsert_auth_sessions_meta_key_by_meta_key_pair(
        "user_id", serde_json::json!(42),
        "server_tag", serde_json::json!("app"),
        "level", serde_json::json!(3),
    )
    .await?;
```

If `upsert_key` is already present on a matching row the existing value is overwritten; otherwise the key is inserted. Pair these with the partial unique indexes from [Enforcing uniqueness on a meta key](#enforcing-uniqueness-on-a-meta-key) / [pair](#enforcing-uniqueness-on-a-meta-key-pair) to guarantee the return value is `0` or `1`; without an index, every session matching the filter is updated.

The upsert key may also be one of the match keys — useful for renaming a value in place. Matching `(user_id=1, server_tag="app")` and upserting `server_tag` → `"stage"` rewrites that row's `server_tag` from `"app"` to `"stage"`; Postgres evaluates the `WHERE` on the pre-update snapshot, so sibling rows that happen to share the old `server_tag` value aren't dragged along. Note that if a composite unique index is installed, an in-place rename that collides with an existing pair fails with a unique violation just like an insert would.

The same pair-scoped upsert is also exposed on the token as `upsert_auth_sessions_meta_key_by_meta_key_pair(match_key1, match_value1, match_key2, match_value2, upsert_key, upsert_value)`. Unlike the other `*_meta_*` token wrappers, it takes `&self` (not `&mut self`) and does **not** refresh the cached session — the DAL returns a row count, not the post-update rows. If the call may have touched this token's own session and you need the new value locally, follow it with `token.refresh_auth_session().await?` or read through one of the non-`_local` `meta_get*` methods.

### Deleting sessions by meta

Useful for logging a user out everywhere (single key) or out of one specific server (pair). Both return the number of rows removed.

```rust
use saps::auth::dal::tx_definitions::{
    DeleteAuthSessionsByMetaKey, DeleteAuthSessionsByMetaKeyPair,
};
use saps::dal::connections::{AuthPostGresDescriptor, LivePostGresPool};

// Force-logout: drop every session for this user, regardless of server.
let removed = AuthPostGresDescriptor::<LivePostGresPool>
    ::delete_auth_sessions_by_meta_key("user_id", serde_json::json!(42))
    .await?;

// Logout from one server only — leaves the user's other server sessions in place.
let removed = AuthPostGresDescriptor::<LivePostGresPool>
    ::delete_auth_sessions_by_meta_key_pair(
        "user_id", serde_json::json!(42),
        "server_tag", serde_json::json!("app"),
    )
    .await?;
```

Same containment semantics as the lookups — a row matches only when `meta` contains every key/value pair you pass, so rows missing one of the keys (or with a different value for one of them) are left untouched.

### Middleware

When `saps.ping` rotates the session UUID, the extractor needs to send the new `saps-token` cookie back to the client. Apply [`attach_refreshed_cookie`](saps/src/auth/middleware/mod.rs) at the router level so this happens transparently:

```rust
use axum::{Router, middleware::from_fn, routing::post};
use saps::auth::middleware::attach_refreshed_cookie;

let app = Router::new()
    .route("/api/v1/auth/validate-session", post(validate_session::<…>))
    .layer(from_fn(attach_refreshed_cookie));
```

The layer installs a slot in the request extensions, the extractor writes the new cookie into that slot when a rotation happens, and the layer attaches the `Set-Cookie` to the response after the handler returns. It's a no-op when no rotation occurred, so it's safe to apply broadly — even on routes that don't use `HeaderToken`.

### Debug tracing

The auth flow is silent by default. When something is misbehaving — users being kicked out unexpectedly, cookie rotations not reaching the client, role checks failing for the wrong reason — turn on the `auth-tracing` Cargo feature to get a `trace!`-level event at every meaningful step of `HeaderToken::from_request_parts` and `attach_refreshed_cookie`.

> [!WARNING]
> `auth-tracing` is expensive. Every authenticated request emits ~6–10 trace events, each carrying request and session fields, and the rotation/meta paths include the full session row (including the `meta` JSONB). Only enable it for debugging — never leave it on in production.

Enable it in your application's `Cargo.toml`:

```toml
[dependencies]
saps = { version = "0.3", features = ["auth-tracing"] }
```

Install a `tracing` subscriber in `main` and filter to the `saps::auth` target so you don't have to wade through unrelated logs:

```rust
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("saps::auth=trace")),
        )
        .init();

    // … axum app setup …
}
```

Or set it via the environment without code changes:

```bash
RUST_LOG="saps::auth=trace" cargo run
```

Every event from the auth flow carries `system="saps-auth"` (a constant tag for `grep` / log-aggregator filtering), `method`, and `uri`; everything past the JWT decode also carries `session_id`; rotations additionally carry `new_session_id`. A typical request that triggers a rotation looks like this:

```text
TRACE saps::auth: attach_refreshed_cookie — installing CookieSlot system="saps-auth" method=POST uri=/api/v1/me
TRACE saps::auth: from_request_parts — auth flow start system="saps-auth" method=POST uri=/api/v1/me
TRACE saps::auth: from_request_parts — JWT located system="saps-auth" method=POST uri=/api/v1/me source=cookie
TRACE saps::auth: decoded JWT system="saps-auth" session_id=8b1e… time_expire=2026-05-13T12:54:56Z
TRACE saps::auth: from_request_parts — JWT decoded, pinging session system="saps-auth" method=POST uri=/api/v1/me session_id=8b1e… time_expire=2026-05-13T12:54:56Z
TRACE saps::auth: from_request_parts — ping returned session system="saps-auth" method=POST uri=/api/v1/me session_id=8b1e… returned_session_id=4f2a… role=admin date_created=2026-05-13T12:29:11Z last_interacted=2026-05-13T12:34:55Z meta=Some(Object {"user_id": Number(42)})
TRACE saps::auth: from_request_parts — role check passed system="saps-auth" method=POST uri=/api/v1/me session_id=8b1e… role=admin
TRACE saps::auth: from_request_parts — ROTATION detected, refreshing JWT and cookie system="saps-auth" method=POST uri=/api/v1/me session_id=8b1e… new_session_id=4f2a…
TRACE saps::auth: encoding JWT system="saps-auth" session_id=4f2a… time_expire=2026-05-13T12:54:56Z
TRACE saps::auth: from_request_parts — handing refreshed cookie to CookieSlot system="saps-auth" method=POST uri=/api/v1/me session_id=4f2a…
TRACE saps::auth: from_request_parts — auth flow complete system="saps-auth" method=POST uri=/api/v1/me session_id=4f2a… rotated=true
TRACE saps::auth: attach_refreshed_cookie — attaching Set-Cookie (rotation occurred) system="saps-auth" method=POST uri=/api/v1/me cookie=saps-token=…; HttpOnly; Path=/; Max-Age=… status=200 OK
```

To pull just the auth lines out of a mixed log stream:

```bash
grep 'system="saps-auth"' app.log
```

Failure paths emit the same shape with an `error` field — `JWT decode failed`, `session not present in DB`, `role check FAILED`, `ping_auth_session DB call failed`. The rotation path also emits a loud warning when no `CookieSlot` is installed in the request extensions (`client will NOT receive new cookie`) — that's the giveaway that the [`attach_refreshed_cookie`](#middleware) layer isn't mounted on a route that needs it.

### Role checks

The second generic on `HeaderToken<X, Y, R, Z>` is the role-check strategy. Saps doesn't ship a fixed set of checks — you generate your own role enum and any number of check structs in one go with `construct_checks!`:

```rust
use saps::construct_checks;

construct_checks!(
    enum UserRoleCheck {
        SuperAdmin,
        Admin,
        Customer,
        Unreachable,
    }
    SuperAdminRoleCheck      => SuperAdmin,
    AdminRoleCheck           => SuperAdmin | Admin,
    CustomerRoleCheck        => SuperAdmin | Admin | Customer,
    NoRoleCheck              => SuperAdmin | Admin | Customer,
    ExactSuperAdminRoleCheck => SuperAdmin,
    ExactAdminRoleCheck      => Admin,
    ExactCustomerRoleCheck   => Customer
);
```

The macro emits the `UserRoleCheck` enum (with `Display`, `TryFrom<String>`, and the `UserRole` impls saps needs) plus one unit struct per `Foo => …` line. Each check struct implements `CheckUserRole` and accepts only the listed variants — so `SuperAdminRoleCheck` accepts just `SuperAdmin`, `AdminRoleCheck` accepts `SuperAdmin` or `Admin`, the `Exact*` checks each accept exactly one variant, and `NoRoleCheck` accepts everything.

The check fires in step 4 of the auth lifecycle, after the JWT decodes and the session row is loaded. A failed check rejects the request with `401 Unauthorized` before your handler body ever runs, so handlers that pin a specific role can safely ignore the token entirely:

```rust
use axum::{Json, http::StatusCode, response::IntoResponse};
use saps::{
    auth::token::header_token::HeaderToken,
    config::GetConfigVariable,
    dal::connections::YieldPostGresPool,
    errors::saps::SapsError,
};
// `UserRoleCheck` and `SuperAdminRoleCheck` come from the construct_checks!
// invocation above (typically re-exported from a kernel module in your crate).

#[derive(serde::Deserialize)]
pub struct DepartmentEmailLinkRequest {
    pub email_id: i32,
    pub department_id: i32,
}

pub async fn link_department_to_email<X, Z>(
    _token: HeaderToken<X, SuperAdminRoleCheck, UserRoleCheck, Z>,
    Json(payload): Json<DepartmentEmailLinkRequest>,
) -> Result<impl IntoResponse, SapsError>
where
    X: GetConfigVariable + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
{
    // Call your own DAL traits to actually create the link. Omitted here
    // because they're application-specific.
    let _ = (payload.email_id, payload.department_id);
    Ok(StatusCode::CREATED)
}
```

The handler ignores `_token` — its only job is to gate access. A non-`SuperAdmin` request never reaches this body.

For a `db_test`, seed an `AuthSession` whose role matches the check, mint a `HeaderToken`, and `set_uuid` it to the seeded session's id. A small helper keeps the success and forbidden paths symmetric:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{HeaderValue, Request, StatusCode},
        routing::post,
    };
    use saps::{
        auth::dal::{model::AuthSession, tx_definitions::CreateAuthSession},
        auth::token::header_token::HeaderToken,
        dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
        db_test, define_static_config,
    };
    use saps::sqlx::{Pool, Postgres, types::Uuid};
    use tower::ServiceExt;

    define_static_config!(
        TestConfig,
        "TOKEN_EXPIRE_MINS" => "20",
        "SECRET_KEY" => "test-secret"
    );

    /// Persist an `AuthSession` with the supplied role and return its UUID.
    /// Tests mint a `HeaderToken`, `set_uuid` it to this id, encode it, and
    /// the extractor's `saps.ping` lands on this row — so the role check
    /// runs against the role we seeded.
    async fn seed_session_with_role<Y: YieldPostGresPool>(role: UserRoleCheck) -> Uuid {
        let session = AuthSession::new(role);
        let id = session.id;
        AuthPostGresDescriptor::<Y>::create_auth_session::<UserRoleCheck>(session)
            .await
            .expect("create auth session");
        id
    }

    /// Fire a request through the router using a JWT minted for `role`.
    async fn fire_request<TestDbHandle: YieldPostGresPool>(role: UserRoleCheck) -> StatusCode {
        let session_id = seed_session_with_role::<TestDbHandle>(role).await;

        let jwt = HeaderToken::<TestConfig, SuperAdminRoleCheck, UserRoleCheck, TestDbHandle>
            ::new::<UserRoleCheck>()
            .expect("construct token")
            .set_uuid(&session_id)
            .encode()
            .expect("encode jwt");

        let app = Router::new().route(
            "/link",
            post(link_department_to_email::<TestConfig, TestDbHandle>),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/link")
            .header(
                "Cookie",
                HeaderValue::from_str(&format!("saps-token={jwt}"))
                    .expect("cookie header"),
            )
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"email_id":1,"department_id":1}"#))
            .expect("build request");

        app.oneshot(request).await.expect("send").status()
    }

    #[db_test]
    async fn test_link_department_to_email_ok<TestDbHandle: YieldPostGresPool>(
        _pool: &Pool<Postgres>,
    ) {
        // SuperAdmin satisfies SuperAdminRoleCheck — handler runs, returns 201.
        assert_eq!(
            fire_request::<TestDbHandle>(UserRoleCheck::SuperAdmin).await,
            StatusCode::CREATED,
        );
    }

    #[db_test]
    async fn test_link_department_to_email_forbidden_for_customer<TestDbHandle: YieldPostGresPool>(
        _pool: &Pool<Postgres>,
    ) {
        // Customer does not satisfy SuperAdminRoleCheck — the extractor rejects
        // with 401 before the handler body runs.
        assert_eq!(
            fire_request::<TestDbHandle>(UserRoleCheck::Customer).await,
            StatusCode::UNAUTHORIZED,
        );
    }
}
```

Note that calling the handler function directly (without going through the router) bypasses the extractor and therefore the role check — direct calls only test the body. Going through `oneshot` exercises the same path a real request takes, including the role check.

### Login

Login is the one handler that *creates* the session — every other handler in this section reads it. The flow is the same regardless of how you check the user's identity (password, OAuth, magic link…): once you've decided who they are, build an `AuthSession` with whatever meta your handlers will need, mint a JWT whose `unique_id` matches the session's UUID, persist the session, and return both a `Set-Cookie` and the encoded token in the body.

```rust
use axum::{
    Json,
    body::{self, Body},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use saps::{
    auth::{
        dal::{model::AuthSession, tx_definitions::CreateAuthSession},
        token::{cookies::AuthTokenCookie, header_token::HeaderToken},
        utils::extract_credentials::extract_credentials,
    },
    config::GetConfigVariable,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};
// `UserRoleCheck` and `NoRoleCheck` come from the construct_checks!
// invocation in the Role checks section above.

#[derive(serde::Deserialize)]
pub struct LoginBody {
    pub role: UserRoleCheck,
}

#[derive(serde::Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub role: UserRoleCheck,
}

pub async fn login<X, Z>(mut req: Request<Body>) -> Result<impl IntoResponse, SapsError>
where
    X: GetConfigVariable,
    Z: YieldPostGresPool,
{
    // 1. Pull `email`/`password` from the `Authorization: Basic …` header.
    let credentials = extract_credentials(&req)?;

    // 2. Read the requested role from the JSON body.
    let raw_body = std::mem::take(req.body_mut());
    let bytes = body::to_bytes(raw_body, usize::MAX)
        .await
        .map_err(|e| SapsError::bad_request(e.to_string()))?;
    let parsed: LoginBody =
        serde_json::from_slice(&bytes).map_err(|e| SapsError::bad_request(e.to_string()))?;

    // 3. Do specific app checks here — look up the user by email, verify the
    //    password hash, decide whether the user is allowed to assume the
    //    requested role, etc. Omitted because it's all application-specific.
    let _ = credentials;
    let user_id: i32 = 1;

    // 4. Build the auth_session row with whatever meta your handlers will
    //    read off the token later.
    let user_role = parsed.role;
    let meta = serde_json::json!({
        "user_id": user_id,
        "user_role": user_role,
    });
    let auth_session = AuthSession::new(user_role).with_meta(meta);

    // 5. Mint a JWT whose unique_id matches the auth_session's UUID. The
    //    extractor pings into this row on every subsequent request.
    let token = HeaderToken::<X, NoRoleCheck, UserRoleCheck, Z>::new::<UserRoleCheck>()?
        .set_uuid(&auth_session.id);

    // 6. Persist the session.
    AuthPostGresDescriptor::<Z>::create_auth_session::<UserRoleCheck>(auth_session).await?;

    // 7. Encode the token and turn it into a Set-Cookie header.
    let encoded = token.encode()?;
    let headers = AuthTokenCookie::from(&encoded).generate_header()?;

    Ok((
        StatusCode::OK,
        headers,
        Json(LoginResponse { token: encoded, role: user_role }),
    ))
}
```

Steps 4–7 are the saps-specific bit; everything before is just authentication. Build the session, mint a token bound to its UUID, persist the row, and ship both cookie + token. The token in the body lets non-browser clients (mobile apps, CLIs) read it; the cookie is for browsers.

A `db_test` fires the real route through `oneshot`, decodes the response JWT, and asserts the auth_session row exists in the DB with the right role and meta:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::post,
    };
    use base64::{Engine, engine::general_purpose};
    use saps::{
        auth::dal::tx_definitions::GetAuthSessionStrict,
        auth::token::header_token::HeaderToken,
        dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
        db_test, define_static_config,
    };
    use saps::sqlx::{Pool, Postgres};
    use tower::ServiceExt;

    define_static_config!(
        TestConfig,
        "TOKEN_EXPIRE_MINS" => "30",
        "SECRET_KEY" => "test-secret"
    );

    fn login_request(email: &str, password: &str, role: UserRoleCheck) -> Request<Body> {
        let creds = general_purpose::STANDARD.encode(format!("{email}:{password}"));
        let body = serde_json::to_vec(&LoginBody { role })
            .expect("serialize login body");
        Request::builder()
            .method("POST")
            .uri("/login")
            .header(header::AUTHORIZATION, format!("Basic {creds}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("build request")
    }

    #[db_test]
    async fn test_login_creates_session<TestDbHandle: YieldPostGresPool>(
        _pool: &Pool<Postgres>,
    ) {
        let app = Router::new().route(
            "/login",
            post(login::<TestConfig, TestDbHandle>),
        );

        let response = app
            .oneshot(login_request("alice@example.com", "hunter2", UserRoleCheck::Customer))
            .await
            .expect("send");

        assert_eq!(response.status(), StatusCode::OK);

        // Set-Cookie carries the saps-token for browsers.
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("login should set the auth cookie")
            .to_str()
            .expect("utf-8")
            .to_string();
        assert!(cookie.contains("saps-token="));

        // Body carries the same JWT for non-browser clients.
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let login: LoginResponse = serde_json::from_slice(&bytes).expect("decode response");
        assert_eq!(login.role, UserRoleCheck::Customer);

        // The response JWT decodes back to a token whose unique_id keys into
        // the auth_session row login just wrote.
        let decoded = HeaderToken::<TestConfig, NoRoleCheck, UserRoleCheck, TestDbHandle>
            ::decode(&login.token)
            .expect("decode response token");

        let session = AuthPostGresDescriptor::<TestDbHandle>
            ::get_auth_session_strict::<UserRoleCheck>(&decoded.unique_id)
            .await
            .expect("session should exist");

        assert_eq!(session.role, UserRoleCheck::Customer);
        let meta_user_id: i32 = session
            .meta_get_typed_strict("user_id")
            .expect("user_id present in meta");
        assert_eq!(meta_user_id, 1);
    }
}
```

Hitting `/login` twice for the same user creates two separate `auth_session` rows — saps doesn't dedupe by user, since the session UUID is what the JWT is bound to. If you want at most one active session per user, install a partial unique index on `meta->>'user_id'` (see [Enforcing uniqueness on a meta key](#enforcing-uniqueness-on-a-meta-key)) — the second insert will fail with a unique-violation.

### Logout

Logout is a thin handler: delete the session row and clear the auth cookie. Any logged-in role should be able to log out, so the route uses `NoRoleCheck`. `token.delete_auth_session()` wraps the DAL delete, and `AuthTokenCookie::from("").wipe_from_cookies()` produces the `Set-Cookie` headers that tell the browser to drop `saps-token` (the wrapped value is ignored — `wipe_from_cookies` only emits the clearing cookie):

```rust
use axum::{http::StatusCode, response::IntoResponse};
use saps::{
    auth::token::{cookies::AuthTokenCookie, header_token::HeaderToken},
    config::GetConfigVariable,
    dal::connections::YieldPostGresPool,
    errors::saps::SapsError,
};
// `UserRoleCheck` and `NoRoleCheck` come from the construct_checks!
// invocation in the Role checks section above.

pub async fn logout<X, Z>(
    token: HeaderToken<X, NoRoleCheck, UserRoleCheck, Z>,
) -> Result<impl IntoResponse, SapsError>
where
    X: GetConfigVariable,
    Z: YieldPostGresPool,
{
    token.delete_auth_session().await?;
    let headers = AuthTokenCookie::from("").wipe_from_cookies()?;
    Ok((StatusCode::OK, headers))
}
```

The DB row deletion alone doesn't tell the client anything — the clearing cookie is what makes the browser stop sending `saps-token` on subsequent requests. `wipe_from_cookies` emits a `Set-Cookie: saps-token=; Max-Age=0; …` header with the same `HttpOnly`/`Path` attributes saps uses on login, so the browser actually evicts the cookie.

A `db_test` is most realistic if it logs in to get a real cookie and then sends that cookie to `/logout` — this exercises the full round-trip the way a browser would. It also covers the unauthenticated path where no cookie is sent and the extractor rejects with 401 before the handler body runs:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // The `login` handler from the previous section.
    use crate::login;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::post,
    };
    use base64::{Engine, engine::general_purpose};
    use saps::{
        auth::dal::tx_definitions::GetAllAuthSessions,
        constants::AUTH_TOKEN_COOKIE_KEY,
        dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
        db_test, define_static_config,
    };
    use saps::sqlx::{Pool, Postgres};
    use tower::ServiceExt;

    define_static_config!(
        TestConfig,
        "TOKEN_EXPIRE_MINS" => "30",
        "SECRET_KEY" => "test-secret"
    );

    /// Strip everything after the first `;` so we can put the cookie on a
    /// follow-up request — `Cookie` headers want just `name=value` pairs,
    /// not the attribute soup browsers receive in `Set-Cookie`.
    fn cookie_kv_only(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap_or(set_cookie).trim().to_string()
    }

    fn login_request() -> Request<Body> {
        let creds = general_purpose::STANDARD.encode("alice@example.com:hunter2");
        let body = serde_json::to_vec(&LoginBody { role: UserRoleCheck::Customer })
            .expect("serialize login body");
        Request::builder()
            .method("POST")
            .uri("/login")
            .header(header::AUTHORIZATION, format!("Basic {creds}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("build login request")
    }

    #[db_test]
    async fn test_logout_clears_cookie_and_deletes_session<TestDbHandle: YieldPostGresPool>(
        _pool: &Pool<Postgres>,
    ) {
        // Mount login + logout on the same router so we can chain them.
        let app = Router::new()
            .route("/login", post(login::<TestConfig, TestDbHandle>))
            .route("/logout", post(logout::<TestConfig, TestDbHandle>));

        // 1. Log in and capture the cookie that login set.
        let login_resp = app.clone()
            .oneshot(login_request())
            .await
            .expect("login");
        assert_eq!(login_resp.status(), StatusCode::OK);
        let set_cookie = login_resp
            .headers()
            .get(header::SET_COOKIE)
            .expect("login set-cookie")
            .to_str()
            .expect("utf-8")
            .to_string();
        let cookie = cookie_kv_only(&set_cookie);

        // Sanity: login created exactly one auth_session row.
        let sessions =
            AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<UserRoleCheck>()
                .await
                .expect("get all sessions");
        assert_eq!(sessions.len(), 1);

        // 2. Send the cookie to /logout.
        let logout_req = Request::builder()
            .method("POST")
            .uri("/logout")
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .expect("build logout request");

        let logout_resp = app.oneshot(logout_req).await.expect("logout");

        // 3. 200, Set-Cookie clears the auth cookie, and the session row is gone.
        assert_eq!(logout_resp.status(), StatusCode::OK);
        let cleared = logout_resp
            .headers()
            .get(header::SET_COOKIE)
            .expect("logout set-cookie")
            .to_str()
            .expect("utf-8");
        assert!(cleared.contains(&format!("{AUTH_TOKEN_COOKIE_KEY}=")));
        assert!(cleared.contains("Max-Age=0"));

        let sessions =
            AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<UserRoleCheck>()
                .await
                .expect("get all sessions");
        assert!(sessions.is_empty(), "session should be deleted after logout");
    }

    #[db_test]
    async fn test_logout_without_cookie_is_unauthorized<TestDbHandle: YieldPostGresPool>(
        _pool: &Pool<Postgres>,
    ) {
        let app = Router::new().route("/logout", post(logout::<TestConfig, TestDbHandle>));

        let request = Request::builder()
            .method("POST")
            .uri("/logout")
            .body(Body::empty())
            .expect("build request");

        let response = app.oneshot(request).await.expect("send");
        // The extractor can't find a JWT in cookies/headers and rejects with 401.
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
```

Note the `cookie_kv_only` helper: `Set-Cookie` headers in responses include attributes like `HttpOnly; Path=/; Max-Age=…`, but the `Cookie` header on a subsequent request only wants `name=value` pairs. Stripping at the first `;` gives you exactly what the browser would echo back.

### Putting it together

A handler that returns the caller's session meta:

```rust
use axum::{Json, http::StatusCode, response::IntoResponse};
use saps::{
    auth::token::{
        checks::{NoRoleCheck, UserRole},
        header_token::HeaderToken,
    },
    config::GetConfigVariable,
    dal::connections::YieldPostGresPool,
    errors::saps::{SapsError, SapsErrorStatus},
};

#[derive(serde::Serialize)]
struct SessionResponse {
    user_id: i32,
    project_id: Option<i32>,
}

pub async fn validate_session<X, Z, R>(
    mut token: HeaderToken<X, NoRoleCheck, R, Z>,
) -> Result<impl IntoResponse, SapsError>
where
    X: GetConfigVariable,
    Z: YieldPostGresPool,
    R: UserRole,
{
    // Required key — falls through as NotFound if missing.
    let user_id: i32 = token.meta_get_typed_strict("user_id").await?;

    // Optional key — convert NotFound into None, propagate everything else.
    let project_id: Option<i32> = match token.meta_get_typed_strict_local("project_id") {
        Ok(id) => Some(id),
        Err(err) if err.status == SapsErrorStatus::NotFound => None,
        Err(err) => return Err(err),
    };

    Ok((StatusCode::OK, Json(SessionResponse { user_id, project_id })))
}
```

Note that the first read is `meta_get_typed_strict` (async, refreshes from the DB) so we have the freshest copy on the token, and the second read is `_local` because we already refreshed for the first call — no need to round-trip again.

### Testing

`#[db_test]` gives you an isolated postgres DB per test. Insert an `AuthSession` directly, build a JWT for it, and fire a request. The extractor will load the session row, populate the token's meta, and your handler runs against real data:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{HeaderValue, Request, StatusCode},
        routing::post,
    };
    use saps::{
        auth::dal::model::AuthSession,
        auth::dal::tx_definitions::CreateAuthSession,
        auth::token::{
            checks::{NoRoleCheck, construct_checks},
            header_token::HeaderToken,
        },
        dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
        db_test, define_static_config,
    };
    use saps::sqlx::{Pool, Postgres};
    use tower::ServiceExt;

    construct_checks!(
        enum TestRole {
            Customer,
        }
    );

    define_static_config!(
        TestConfig,
        "TOKEN_EXPIRE_MINS" => "20",
        "SECRET_KEY" => "test-secret"
    );

    #[db_test]
    async fn test_validate_session_ok<TestDbHandle: YieldPostGresPool>(pool: &Pool<Postgres>) {
        // 1. Insert a session with the meta our handler will read.
        let session = AuthSession::new(TestRole::Customer)
            .with_meta(serde_json::json!({"user_id": 1, "project_id": 42}));
        let created = AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("create session");

        // 2. Build a JWT whose unique_id matches the session row.
        let token = HeaderToken::<TestConfig, NoRoleCheck, TestRole, TestDbHandle>
            ::new::<TestRole>()
            .expect("new token")
            .set_uuid(&created.id);
        let jwt = token.encode().expect("encode jwt");

        // 3. Mount the handler and fire a request with the JWT in the cookie.
        let app = Router::new().route(
            "/validate",
            post(validate_session::<TestConfig, TestDbHandle, TestRole>),
        );

        let request = Request::builder()
            .method("POST")
            .uri("/validate")
            .header(
                "Cookie",
                HeaderValue::from_str(&format!("saps-token={jwt}"))
                    .expect("cookie header"),
            )
            .body(Body::empty())
            .expect("build request");

        let response = app.oneshot(request).await.expect("send");
        assert_eq!(response.status(), StatusCode::OK);
    }
}
```

`define_static_config!` is enough for testing because the extractor only needs `TOKEN_EXPIRE_MINS` and `SECRET_KEY`. Use `define_env_config!` (see [Config Variables](#config-variables)) in production.


## Embedding and serving a frontend

Saps embeds your frontend build folder into the binary at compile time and mounts it onto your axum router with a single macro call. The folder is walked at compile time via `rust-embed`, so the resulting binary serves the frontend without ever touching the filesystem at runtime — one self-contained executable that hosts both your API and your SPA.

```rust
use saps::axum::{Router, response::IntoResponse, routing::get};
use saps::config::EnvConfig;
use saps::mount_frontend;

async fn ping() -> impl IntoResponse { "pong" }

#[tokio::main]
async fn main() {
    // Mount API routes BEFORE mount_frontend! — they win against the SPA fallback.
    let app = Router::new().route("/api/v1/ping", get(ping));
    let app = api::networking::users::users_factory::<EnvConfig>(app);

    // Args: (folder relative to your crate's Cargo.toml, the Router binding to
    // extend in place, max-age in seconds for cached static assets).
    mount_frontend!("frontend/web/public", app, 604800);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    saps::axum::serve(listener, app).await.unwrap();
}
```

> [!IMPORTANT]
> Prefix every backend endpoint with `/api/`. The macro registers an axum fallback that catches every unmatched `GET` and serves it as either an embedded asset or the SPA shell — without the prefix convention, a typo in an API URL would silently return `index.html` with `200 OK` and `Content-Type: text/html`, and your client would parse HTML where it expected JSON. The fallback hard-rejects `/api/...` with a `404` so the mistake surfaces immediately, but it can only do that if your real API routes follow that prefix.

The third macro argument is a `max-age` in seconds. The caching rules the macro applies are:

- `index.html` is always `Cache-Control: no-cache` (it references hashed asset filenames that change every build, so caching it would freeze users on the previous deploy).
- Every other asset is `Cache-Control: public, max-age=<cache_seconds>`.

So the `604800` (7 days) above caches the hashed bundle assets aggressively while always fetching fresh `index.html`, meaning deploys take effect for users on their next pageload.

For the full routing decision tree (SPA fallback rules, `.wasm` MIME handling, file-shaped 404s for stale hashed assets), see the [`saps-frontend-macro` crate README](macros/frontend/README.md).


## Background Tasks

The persistence of background tasks is handled in a non-public schema in postgres. With saps, we can shoot off random background tasks to the execution queue and you can also schedule background tasks for individual times or periods.

> [!WARNING]
> The `#[background_task]` macro registers each task into a global registry **before `main` runs** so the worker pool can look tasks up by name. That registration relies on the [`ctor`](https://crates.io/crates/ctor) crate's pre-main initialization. If you're going to use background tasks, add `ctor` as a direct dependency in your crate's `Cargo.toml` — the macro expansion references it by name, and without the crate present the build will fail with an unresolved-path error.
>
> ```toml
> [dependencies]
> ctor = "1.0.6"
> ```

First, we need the following imports:

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


## Roadmap

The following are on the list to land in saps. Each one will follow the same conventions as the existing modules — handles passed in as generics, `#[db_test]` integration tests, and minimal runtime surface so application code stays thin.

- **Email sending** — a transport-agnostic interface (SMTP / SES / Mailgun / Resend behind one trait) with a queued path that piggybacks on the background-tasks runtime so retries and rate limits are persisted in postgres rather than in memory.
- **Payment processing** — Stripe-first integration covering checkout sessions, webhooks (with idempotency keyed off the event id), and a customer/subscription model that lives in your DAL but is migrated by saps.
- **OAuth providers** — Google, GitHub, LinkedIn, Microsoft, and Apple. Each provider plugs into the existing auth/session pipeline: the OAuth dance normalizes provider claims into a saps-managed external identity record, writes selected claims into session meta, and returns the same session/token shape consumed by HeaderToken.
- **Single-user tokens** — short-lived, single-use tokens for password reset, email verification, magic-link login, and other one-shot flows. Backed by a saps-managed table with TTL + atomic consume-on-use semantics so a token can never be exchanged twice.
- **File storage** — files (bytes + metadata: owner, size, content-type, expiry) stored as rows in postgres so a single transaction commits both, and so the existing `#[db_test]` machinery gives every test an isolated file store for free. The whole thing sits behind a trait, so a local-disk backend (bytes on the filesystem, metadata still in postgres) is drop-in interchangeable for cases where blobs are large enough that you'd rather not hold them in the DB — your handler code is unchanged either way. Also be interchangable with `s3`, `R2`, or streaming large object storage in postgres.
- **Admin Panel** - Integration with DB transaction traits to support an admin panel.
