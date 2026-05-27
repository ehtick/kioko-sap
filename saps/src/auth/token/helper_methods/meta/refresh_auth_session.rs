//! `refresh_auth_session` inner: re-fetch the auth session from the
//! database.
//!
//! Generic over the role enum `R` and the pool provider `Z` only. Called
//! from [`HeaderToken::refresh_auth_session`] and from every async
//! `meta_get_*` wrapper that needs a fresh read before reading the cached
//! `meta`.
//!
//! [`HeaderToken::refresh_auth_session`]: crate::auth::token::header_token::HeaderToken::refresh_auth_session

use crate::{
    auth::{
        dal::{model::AuthSession, tx_definitions::GetAuthSessionStrict},
        token::checks::UserRole,
    },
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// Re-fetch the row identified by `unique_id` from `saps.auth_sessions`.
///
/// A missing row surfaces as `sqlx::Error::RowNotFound` (wrapped in
/// [`SapsError`]) per the contract of [`GetAuthSessionStrict`].
pub async fn refresh_auth_session<R, Z>(
    unique_id: &str,
) -> Result<AuthSession<R>, SapsError>
where
    R: UserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
{
    Ok(AuthPostGresDescriptor::<Z>::get_auth_session_strict::<R>(unique_id).await?)
}
