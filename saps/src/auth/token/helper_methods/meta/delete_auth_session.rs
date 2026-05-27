//! `delete_auth_session` inner: delete the row identified by the token's
//! UUID from `saps.auth_sessions`.
//!
//! Generic over the pool provider `Z` only — neither `X`, `Y`, nor `R`
//! appear in the body.

use crate::{
    auth::dal::tx_definitions::DeleteAuthSession,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Parses `unique_id` as a UUID and deletes the matching session row.
///
/// Returns `true` if a row was deleted, `false` if none existed.
pub async fn delete_auth_session<Z>(unique_id: &str) -> Result<bool, SapsError>
where
    Z: YieldPostGresPool + Send + Sync,
{
    let session_id = uuid::Uuid::parse_str(unique_id)
        .map_err(|e| SapsError::unknown(e.to_string()))?;
    Ok(AuthPostGresDescriptor::<Z>::delete_auth_session(session_id).await?)
}
