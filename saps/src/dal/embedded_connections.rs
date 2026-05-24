//! Defines a [`YieldPostGresPool`] handle backed by an embedded PostgreSQL instance.
//!
//! # Overview
//! - Boots a real PostgreSQL process inside the binary using `postgresql_embedded`.
//! - Exposes the resulting `sqlx::PgPool` via [`LiveEmbeddedPostgresPool`] so it
//!   slots into every framework component that already takes a `YieldPostGresPool`
//!   handle (DAL transactions, `AuthPostGresDescriptor`, worker pool, etc.).
//! - Reads the embedded database name from `EMBEDDED_DATABASE_NAME` and the pool
//!   size from `DB_MAX_CONNECTIONS`.
//!
//! # Lifecycle
//! Booting embedded postgres is async (it may download binaries on first run),
//! so unlike [`LivePostGresPool`](super::connections::LivePostGresPool) the pool
//! is not a `LazyLock`. Call [`init_live_embedded_postgres_pool`] once at startup
//! before any consumer calls `LiveEmbeddedPostgresPool::yield_pool()`.
use crate::dal::connections::YieldPostGresPool;
use sqlx::{Pool, Postgres};

saps_db_embedded_pool_macro::define_embedded_pg_pool!(
    EMBEDDED_SQLX_POSTGRES_POOL,
    "EMBEDDED_DATABASE_NAME",
    "DB_MAX_CONNECTIONS"
);

/// A [`YieldPostGresPool`] handle backed by an embedded PostgreSQL instance.
///
/// [`init_live_embedded_postgres_pool`] must be awaited once at program startup
/// before this handle is used — accessing the pool before initialization will
/// panic.
#[derive(Clone, Debug)]
pub struct LiveEmbeddedPostgresPool;

impl YieldPostGresPool for LiveEmbeddedPostgresPool {
    fn yield_pool() -> &'static Pool<Postgres> {
        EMBEDDED_SQLX_POSTGRES_POOL
            .get()
            .expect("init_live_embedded_postgres_pool().await must be called before LiveEmbeddedPostgresPool::yield_pool()")
    }
}

/// Boots the embedded PostgreSQL server, creates the database named by
/// `EMBEDDED_DATABASE_NAME` if it does not already exist, and connects an
/// `sqlx::PgPool` to it. Subsequent calls are no-ops and return the cached pool.
pub async fn init_live_embedded_postgres_pool() -> &'static Pool<Postgres> {
    init_embedded_sqlx_postgres_pool().await
}
