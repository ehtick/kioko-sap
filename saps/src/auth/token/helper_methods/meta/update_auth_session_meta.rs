//! `update_auth_session_meta` inner: replace the entire `meta` JSON for
//! the session identified by `unique_id`.
//!
//! Generic over the pool provider `Z` only.

use crate::{
    auth::dal::tx_definitions::UpdateAuthSessionMeta,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Sets the `meta` column for `unique_id`. The caller is responsible for
/// updating any cached [`AuthSession`](crate::auth::dal::model::AuthSession)
/// it may hold.
pub async fn update_auth_session_meta<Z>(
    unique_id: &str,
    meta: serde_json::Value,
) -> Result<(), SapsError>
where
    Z: YieldPostGresPool + Send + Sync,
{
    AuthPostGresDescriptor::<Z>::update_auth_session_meta(unique_id, meta).await?;
    Ok(())
}
