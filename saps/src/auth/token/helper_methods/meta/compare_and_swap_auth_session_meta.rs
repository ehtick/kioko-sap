//! `compare_and_swap_auth_session_meta` inner: atomic compare-and-swap on
//! a single top-level `meta` key.
//!
//! Generic over the role enum `R` and the pool provider `Z` only.

use crate::{
    auth::{
        dal::{model::AuthSession, tx_definitions::CompareAndSwapAuthSessionMeta},
        token::checks::UserRole,
    },
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Atomic CAS on one meta key. Returns `Some(updated_row)` if the swap
/// went through, or `None` if the session is gone, the key is absent, or
/// the current value differs from `expected`.
pub async fn compare_and_swap_auth_session_meta<R, Z>(
    unique_id: &str,
    key: &str,
    expected: serde_json::Value,
    new_value: serde_json::Value,
) -> Result<Option<AuthSession<R>>, SapsError>
where
    R: UserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
{
    Ok(AuthPostGresDescriptor::<Z>::compare_and_swap_auth_session_meta::<R>(
        unique_id, key, expected, new_value,
    )
    .await?)
}
