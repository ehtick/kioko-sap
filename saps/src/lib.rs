extern crate self as saps;

pub mod auth;
pub mod config;
pub mod dal;
pub mod errors;
pub mod background_tasks;

mod constants;

// re-exports
pub use axum;
pub use sqlx;
pub use mime_guess;
pub use rust_embed;
pub use serde_json::Value;

// macros
pub use saps_db_pool_macro::define_pg_pool;
pub use saps_db_tx::db_transaction;
pub use saps_test_macro::db_test;
pub use saps_frontend_macro::mount_frontend;
