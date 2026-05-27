//! `upsert_auth_sessions_meta_key_by_meta_key_pair` inner: set
//! `upsert_key`/`upsert_value` on every row whose `meta` matches both
//! provided `(key, value)` pairs.
//!
//! Generic over the pool provider `Z` only.

use crate::{
    auth::dal::tx_definitions::UpsertAuthSessionsMetaKeyByMetaKeyPair,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Bulk upsert keyed on a meta pair. Returns the number of rows touched.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_auth_sessions_meta_key_by_meta_key_pair<Z>(
    match_key1: &str,
    match_value1: serde_json::Value,
    match_key2: &str,
    match_value2: serde_json::Value,
    upsert_key: &str,
    upsert_value: serde_json::Value,
) -> Result<u64, SapsError>
where
    Z: YieldPostGresPool + Send + Sync,
{
    Ok(AuthPostGresDescriptor::<Z>::upsert_auth_sessions_meta_key_by_meta_key_pair(
        match_key1,
        match_value1,
        match_key2,
        match_value2,
        upsert_key,
        upsert_value,
    )
    .await?)
}
