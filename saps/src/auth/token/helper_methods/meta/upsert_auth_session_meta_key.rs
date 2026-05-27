//! `upsert_auth_session_meta_key` inner: set a single top-level `key` in
//! the auth session's `meta`, leaving other keys intact.
//!
//! Generic over the role enum `R` and the pool provider `Z` only.

use crate::{
    auth::{
        dal::{model::AuthSession, tx_definitions::UpsertAuthSessionMetaKey},
        token::checks::UserRole,
    },
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Upsert one meta key for `unique_id`. Returns `Some(updated_row)` on
/// success, or `None` if the row no longer exists.
pub async fn upsert_auth_session_meta_key<R, Z>(
    unique_id: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<Option<AuthSession<R>>, SapsError>
where
    R: UserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
{
    Ok(AuthPostGresDescriptor::<Z>::upsert_auth_session_meta_key::<R>(unique_id, key, value).await?)
}
