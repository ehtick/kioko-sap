//! Y-free core of the `HeaderToken` extractor.
//!
//! This module owns the heavy lifting of the auth flow — token extraction,
//! JWT decode, DB ping, rotation handling — generic only over the config
//! provider `X`, the role enum `R`, and the DB pool `Z`. The role-check
//! strategy `Y` lives one level up in [`HeaderToken::from_request_parts`]
//! so the body here compiles once per `(X, R, Z)` regardless of how many
//! `Y` strategies the consumer crate uses.
//!
//! [`HeaderToken::from_request_parts`]: crate::auth::token::header_token::HeaderToken

use axum::http::request::Parts;
use chrono::{DateTime, Utc};

use crate::{
    auth::{
        auth_trace,
        dal::{model::AuthSession, tx_definitions::PingAuthSession},
        middleware::CookieSlot,
        token::{
            checks::UserRole,
            cookies::AuthTokenCookie,
            header_token::UpdatedAuthCookie,
            helper_methods::{
                extract_bearer_token::extract_bearer_token,
                extract_token_from_cookies::extract_token_from_cookies,
                extract_token_from_header::extract_token_from_header,
                jwt_claims::JwtClaims,
            },
        },
    },
    config::GetConfigVariable,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};

/// The output of [`run_auth_extraction`]: everything the caller needs to do
/// the (Y-dependent) role check and assemble the final [`HeaderToken`].
///
/// [`HeaderToken`]: crate::auth::token::header_token::HeaderToken
pub struct RawAuthExtraction<R: UserRole> {
    /// The session row pinged from the database. Owned, ready to move onto
    /// the assembled `HeaderToken`.
    pub session: AuthSession<R>,
    /// The session UUID to attach to the assembled token. Equal to
    /// `session.id.to_string()` when rotation occurred, or the original
    /// JWT's `unique_id` otherwise.
    pub unique_id: String,
    /// The JWT expiry to attach to the assembled token. Refreshed on
    /// rotation, otherwise carried over from the inbound JWT.
    pub time_expire: DateTime<Utc>,
    /// The previous UUID if a rotation occurred during this request,
    /// otherwise `None`.
    pub old_uuid: Option<String>,
}

/// Runs the Y-free portion of the auth flow:
///
/// 1. Locate the JWT in cookies / `token` header / Authorization bearer.
/// 2. Decode and verify the JWT.
/// 3. Ping `saps.ping(10, unique_id)` to keep the session alive and detect
///    rotation.
/// 4. On rotation: re-encode a refreshed JWT, build the `Set-Cookie`
///    payload, and stash it in the request's [`CookieSlot`] (if installed).
///
/// The role check is intentionally *not* performed here — it lives in the
/// outer `from_request_parts` shim and is the only place `Y` is consulted.
/// Keeping `Y` out of this function means the body compiles once per
/// `(X, R, Z)` rather than once per `(X, Y, R, Z)`.
pub async fn run_auth_extraction<X, R, Z>(
    parts: &mut Parts,
) -> Result<RawAuthExtraction<R>, SapsError>
where
    X: GetConfigVariable + Send + Sync,
    R: UserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
{
    let headers = &parts.headers;

    // Snapshot identifying request info up front so every trace from this
    // extractor carries the same context.
    #[cfg(feature = "auth-tracing")]
    let method = parts.method.clone();
    #[cfg(feature = "auth-tracing")]
    let uri = parts.uri.clone();
    auth_trace!(
        method = %method,
        uri = %uri,
        "run_auth_extraction — auth flow start",
    );

    // Try cookie first, then the `token` header, then bearer as a fallback.
    let raw_token = match extract_token_from_cookies(headers)? {
        Some(token) => {
            auth_trace!(
                method = %method,
                uri = %uri,
                source = "cookie",
                "run_auth_extraction — JWT located",
            );
            token
        }
        None => {
            auth_trace!(
                method = %method,
                uri = %uri,
                "run_auth_extraction — no `saps-token` cookie, trying `token` header",
            );
            match extract_token_from_header(headers)? {
                Some(token) => {
                    auth_trace!(
                        method = %method,
                        uri = %uri,
                        source = "token-header",
                        "run_auth_extraction — JWT located",
                    );
                    token
                }
                None => {
                    auth_trace!(
                        method = %method,
                        uri = %uri,
                        "run_auth_extraction — no `token` header, falling back to bearer",
                    );
                    let token = extract_bearer_token(headers).map_err(|e| {
                        auth_trace!(
                            method = %method,
                            uri = %uri,
                            error = %e.message,
                            "run_auth_extraction — no JWT found in any supported location",
                        );
                        e
                    })?;
                    auth_trace!(
                        method = %method,
                        uri = %uri,
                        source = "bearer",
                        "run_auth_extraction — JWT located",
                    );
                    token
                }
            }
        }
    };

    // Decode the JWT into the minimal claims struct (generic on X only).
    let claims = JwtClaims::decode::<X>(&raw_token).map_err(|e| {
        auth_trace!(
            method = %method,
            uri = %uri,
            error = %e.message,
            "run_auth_extraction — JWT decode failed",
        );
        e
    })?;

    let existing_id = claims.unique_id.clone();
    auth_trace!(
        method = %method,
        uri = %uri,
        session_id = %existing_id,
        time_expire = %claims.time_expire,
        "run_auth_extraction — JWT decoded, pinging session",
    );

    // Ping the session to keep it alive and check if it still exists.
    let session = match AuthPostGresDescriptor::<Z>::ping_auth_session::<R>(
        10,
        &claims.unique_id,
    )
    .await
    .map_err(|e| {
        auth_trace!(
            method = %method,
            uri = %uri,
            session_id = %existing_id,
            error = %e,
            "run_auth_extraction — ping_auth_session DB call failed",
        );
        e
    })? {
        Some(session) => session,
        None => {
            auth_trace!(
                method = %method,
                uri = %uri,
                session_id = %existing_id,
                "run_auth_extraction — session not present in DB (expired or never existed)",
            );
            return Err(SapsError::unauthorized("session not present"));
        }
    };

    auth_trace!(
        method = %method,
        uri = %uri,
        session_id = %existing_id,
        returned_session_id = %session.id,
        role = %session.role.to_string(),
        date_created = %session.date_created,
        last_interacted = %session.last_interacted,
        meta = ?session.meta,
        "run_auth_extraction — ping returned session",
    );

    // If the stored procedure regenerated the UUID, refresh the JWT and
    // stash a Set-Cookie value into the response-layer's CookieSlot.
    let (unique_id, time_expire, old_uuid) = if session.id.to_string() != existing_id {
        auth_trace!(
            method = %method,
            uri = %uri,
            session_id = %existing_id,
            new_session_id = %session.id,
            "run_auth_extraction — ROTATION detected, refreshing JWT and cookie",
        );
        // Refresh the JWT expiry on rotation. See the original comment in
        // `from_request_parts` for the reasoning — without this a long-lived
        // active session would appear expired the moment the original
        // login window elapsed.
        let token_expire_mins = X::get_config_variable("TOKEN_EXPIRE_MINS")?
            .parse::<i64>()
            .map_err(|e| SapsError::unknown(e.to_string()))?;
        let new_claims = JwtClaims {
            unique_id: session.id.to_string(),
            time_expire: Utc::now() + chrono::Duration::minutes(token_expire_mins),
        };
        auth_trace!(
            method = %method,
            uri = %uri,
            session_id = %session.id,
            time_expire = %new_claims.time_expire,
            token_expire_mins = token_expire_mins,
            "run_auth_extraction — new time_expire applied to rotated token",
        );
        let new_jwt = new_claims.encode::<X>()?;
        let cookie_str = AuthTokenCookie::from(&new_jwt).yield_cookie_string();
        if let Some(slot) = parts.extensions.get::<CookieSlot>() {
            auth_trace!(
                method = %method,
                uri = %uri,
                session_id = %session.id,
                "run_auth_extraction — handing refreshed cookie to CookieSlot",
            );
            slot.set(UpdatedAuthCookie(cookie_str));
        } else {
            auth_trace!(
                method = %method,
                uri = %uri,
                session_id = %session.id,
                "run_auth_extraction — no CookieSlot in extensions; client will NOT receive new cookie (is `attach_refreshed_cookie` layer installed?)",
            );
        }
        (new_claims.unique_id, new_claims.time_expire, Some(existing_id))
    } else {
        auth_trace!(
            method = %method,
            uri = %uri,
            session_id = %existing_id,
            "run_auth_extraction — no rotation, session UUID unchanged",
        );
        (claims.unique_id, claims.time_expire, None)
    };

    Ok(RawAuthExtraction {
        session,
        unique_id,
        time_expire,
        old_uuid,
    })
}
