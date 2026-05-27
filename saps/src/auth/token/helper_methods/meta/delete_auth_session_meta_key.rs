//! `delete_auth_session_meta_key` inner: remove a single top-level `key`
//! from the auth session's `meta`.
//!
//! Generic over the role enum `R` and the pool provider `Z` only.

use crate::{
    auth::{
        dal::{model::AuthSession, tx_definitions::DeleteAuthSessionMetaKey},
        token::checks::UserRole,
    },
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Delete one meta key for `unique_id`. Returns `Some(updated_row)` on
/// success, or `None` if the row no longer exists.
pub async fn delete_auth_session_meta_key<R, Z>(
    unique_id: &str,
    key: &str,
) -> Result<Option<AuthSession<R>>, SapsError>
where
    R: UserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
{
    Ok(AuthPostGresDescriptor::<Z>::delete_auth_session_meta_key::<R>(unique_id, key).await?)
}
