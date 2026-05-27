//! Free-function bodies for the async meta methods on
//! [`HeaderToken`](crate::auth::token::header_token::HeaderToken).
//!
//! Each method on the token is a tiny boxed shim that delegates to the
//! function in this module. The shims compile per `(X, Y, R, Z)` but are
//! trivial; the heavy bodies here compile only per `(R, Z)` (or `(Z)` for
//! the few that don't touch the role type). This collapses the Y and X
//! axes from the per-call monomorphization matrix, which matters when the
//! same `HeaderToken` family is used across many axum handlers.
pub mod compare_and_swap_auth_session_meta;
pub mod delete_auth_session;
pub mod delete_auth_session_meta_key;
pub mod refresh_auth_session;
pub mod update_auth_session_meta;
pub mod upsert_auth_session_meta_key;
pub mod upsert_auth_sessions_meta_key_by_meta_key_pair;
