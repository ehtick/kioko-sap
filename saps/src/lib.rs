extern crate self as saps;

pub mod errors;

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

#[cfg(feature = "server")]
mod constants;

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
