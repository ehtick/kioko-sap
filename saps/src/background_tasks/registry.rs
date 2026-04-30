//! Global registry of background task handler functions.
//!
//! This module owns [`TASK_REGISTRY`], the process-wide map from a task's
//! `function_name` (a `String` key, e.g. `"add"` or `"send_welcome_email"`) to the
//! function pointer that knows how to execute it. The registry is the bridge
//! between the database queue (`saps.queued_tasks`) and the actual Rust code that
//! runs a task: the queue stores *what* should run by name plus a JSON payload,
//! and the registry resolves that name to *how* to run it.
//!
//! # The flow at a glance
//!
//! ```text
//!   compile time                        process start                runtime
//!   ------------                        -------------                -------
//!
//!   #[background_task]                  #[ctor::ctor] fn runs        worker claims a row
//!   fn my_job(...) { ... }      ──►     before main(), inserts  ──►  registry.get(&row.function_name)
//!         │                             handler into                 → invokes handler(row.params, pool)
//!         │                             TASK_REGISTRY
//!         │
//!         └─ macro expands to:
//!              - core async fn (the executable handler)
//!              - typed enqueue fn (serializes args + INSERT)
//!              - ctor that registers the core fn under the function name
//! ```
//!
//! # Why a global, lazily-initialised `RwLock<HashMap>`?
//!
//! - **Global** — handlers are registered by `#[ctor::ctor]` constructors emitted
//!   by the [`background_task`] proc-macro. Those constructors run before `main`
//!   and have no access to any user-supplied state, so the registry must live in
//!   a `static`.
//! - **`LazyLock`** — the underlying `HashMap` is only constructed on first
//!   access, so we don't pay for it (or hit static-init ordering issues) in
//!   binaries that don't use background tasks.
//! - **`RwLock`** — registration is write-heavy at startup (one write per
//!   `#[background_task]` function) and read-only at steady state (every worker
//!   poll cycle does one `read()`). An `RwLock` lets all workers look up handlers
//!   concurrently without contending with each other.
//!
//! # Lifecycle
//!
//! 1. **Compile time**: each `#[background_task]` annotated function generates a
//!    `saps_background_register_<name>` constructor.
//! 2. **Process start**: the `ctor` crate runs every such constructor before
//!    `main`, each one taking a write lock on [`TASK_REGISTRY`] and inserting
//!    its handler keyed by the original function name.
//! 3. **Runtime**: [`worker_cycle`](super::worker_pool) takes a read lock on
//!    [`TASK_REGISTRY`], looks up the handler by `function_name`, **clones the
//!    function pointer**, and drops the guard before any `.await` (the read
//!    guard is `!Send`, so it cannot be held across an await point).
//!
//! [`background_task`]: https://docs.rs/saps_background_task

use crate::errors::saps::SapsError;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, RwLock};

/// The signature every registered background task handler must match.
///
/// A `TaskFnPtr` is a plain `fn` pointer (not a `Box<dyn Fn>`) so it implements
/// `Copy`, can be stored in a `static`, and can be cheaply cloned out of the
/// registry under a short-lived read guard.
///
/// # Parameters
///
/// - `Value` — the task's parameters, deserialized from the `params` JSONB
///   column of `saps.queued_tasks`. The `#[background_task]` macro generates
///   code at the top of each handler that destructures this value back into
///   the original typed arguments.
/// - `&'static sqlx::Pool<sqlx::Postgres>` — a long-lived reference to the
///   database pool, yielded by a `YieldPostGresPool` impl. The `'static`
///   lifetime is required so the future returned by the handler can outlive
///   any borrow from the worker that spawned it.
///
/// # Return type
///
/// `Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send>>` — a boxed,
/// pinned, `Send` future. The boxing is necessary because:
///
/// - Different `#[background_task]` functions produce different concrete future
///   types, but the registry stores one uniform pointer type.
/// - The future must be `Send` so the worker can hand it to
///   `tokio::task::spawn_blocking` and drive it on a blocking thread.
///
/// # Why a `fn` pointer and not `Box<dyn Fn>`?
///
/// A `fn` pointer is `Copy`, so worker code can do
/// `let handler = *registry.get(name)?;` and immediately drop the read guard
/// before awaiting. A `Box<dyn Fn>` would require either `Arc` cloning or
/// holding the guard across the await — both more expensive and the latter
/// outright impossible (`RwLockReadGuard` is `!Send`).
pub type TaskFnPtr = fn(
    Value,
    &'static sqlx::Pool<sqlx::Postgres>,
) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send>>;

/// A name + handler pair describing a single background task.
///
/// Pairs the registry key (the task's `function_name` as it appears in
/// `saps.queued_tasks.function_name`) with the function pointer that executes
/// it. The `name` is `&'static str` because every task name is a string literal
/// produced at compile time by the `#[background_task]` macro.
pub struct BackgroundTaskEntry {
    /// The task's registry key — matches `saps.queued_tasks.function_name`
    /// for rows that should be dispatched to `handler`.
    pub name: &'static str,
    /// The function pointer invoked by the worker when a row with this `name`
    /// is claimed from the queue. See [`TaskFnPtr`] for the exact signature.
    pub handler: TaskFnPtr,
}

/// Process-wide map from task name to handler function.
///
/// Populated at process startup by `#[ctor::ctor]` constructors that the
/// `#[background_task]` proc-macro emits for every annotated function. Read by
/// every worker on every poll cycle to dispatch a claimed task to its handler.
///
/// # Keying
///
/// The key is the task's `function_name` — i.e. the original Rust function
/// identifier passed to `#[background_task]`. The macro both:
///
/// - inserts the handler under this key at startup, and
/// - stamps the same key into `saps.queued_tasks.function_name` whenever the
///   typed enqueue function is called.
///
/// As long as both ends of the system are compiled from the same code, the
/// lookup in [`worker_cycle`](super::worker_pool) is guaranteed to find a
/// matching entry. A miss at runtime almost always means a worker process is
/// running an older build than whatever enqueued the row (or the task was
/// renamed without clearing the queue).
///
/// # Locking
///
/// - **Writes** happen exactly once per task at startup, from `#[ctor]`
///   constructors. After `main` begins the map is effectively immutable.
/// - **Reads** happen on every worker poll. Workers must scope the
///   `RwLockReadGuard` into a block and copy the `fn` pointer out before any
///   `.await`, because the guard is `!Send` and cannot cross an await point.
///
/// # Example: registering a handler manually
///
/// In normal use you should prefer the `#[background_task]` macro, which both
/// registers the handler and generates a typed enqueue function. Manual
/// registration is mainly useful in tests.
///
/// ```ignore
/// use saps::background_tasks::registry::{TASK_REGISTRY, TaskFnPtr};
/// use saps::errors::saps::SapsError;
/// use std::pin::Pin;
/// use std::future::Future;
///
/// fn my_handler(
///     params: serde_json::Value,
///     _pool: &'static sqlx::Pool<sqlx::Postgres>,
/// ) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send>> {
///     Box::pin(async move {
///         println!("got params: {}", params);
///         Ok(())
///     })
/// }
///
/// TASK_REGISTRY
///     .write()
///     .unwrap()
///     .insert("my_handler".to_string(), my_handler as TaskFnPtr);
/// ```
///
/// # Example: looking up a handler safely
///
/// ```ignore
/// use saps::background_tasks::registry::TASK_REGISTRY;
///
/// // Scope the read guard into its own block so it's dropped before the await.
/// let handler = {
///     let registry = TASK_REGISTRY.read().unwrap();
///     *registry.get("my_handler").expect("handler not registered")
/// };
/// // guard is gone here — safe to .await
/// handler(serde_json::json!({}), pool).await?;
/// ```
pub static TASK_REGISTRY: LazyLock<RwLock<HashMap<String, TaskFnPtr>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
