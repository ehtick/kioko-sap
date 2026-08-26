extern crate self as saps;

pub mod errors;
/// Shared pure-data types. Baseline surface — no feature required.
pub mod kernel;

#[cfg(feature = "files")]
pub mod files;

#[cfg(feature = "server")]
pub mod auth;
#[cfg(feature = "server")]
pub mod background_tasks;
#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod dal;
#[cfg(feature = "server")]
pub mod scheduled_tasks;


pub mod constants;

// re-exports
#[cfg(feature = "server")]
pub use axum;
#[cfg(feature = "server")]
pub use mime_guess;
#[cfg(feature = "server")]
pub use paste;
#[cfg(feature = "server")]
pub use rust_embed;
#[cfg(feature = "server")]
pub use sqlx;
#[cfg(feature = "server")]
pub use tokio;
#[cfg(feature = "openapi")]
pub use aide;
#[cfg(feature = "embedded_postgres")]
pub use postgresql_embedded;
#[cfg(feature = "embedded_postgres")]
pub use saps_db_embedded_pool_macro::define_embedded_pg_pool;

pub use serde_json::Value;

// macros
#[cfg(feature = "server")]
pub use saps_background_task::background_task;
#[cfg(feature = "server")]
pub use saps_db_pool_macro::define_pg_pool;
#[cfg(feature = "server")]
pub use saps_db_tx::db_transaction;
#[cfg(feature = "server")]
pub use saps_frontend_macro::mount_frontend;
#[cfg(feature = "server")]
pub use saps_test_macro::db_test;
