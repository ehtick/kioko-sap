//! JWT-based authentication token and axum extractor.
//!
//! This module provides [`HeaderToken`], a generic struct that serves two roles:
//!
//! 1. **JWT wrapper** — encodes and decodes a session identifier (`unique_id`) and expiry
//!    timestamp into a signed HS256 JSON Web Token.
//! 2. **Axum extractor** — implements [`FromRequestParts`] so it can be used directly as a
//!    handler parameter. When extracted, it automatically validates the JWT, pings the
//!    database session, checks the user's role, and populates session metadata.
//!
//! # Token extraction order
//!
//! The extractor looks for the JWT in the following locations, using the first one found:
//!
//! 1. **Cookie** — the `saps-token` cookie (see `AUTH_TOKEN_COOKIE_KEY`).
//! 2. **`token` header** — a custom header named `token`.
//! 3. **`Authorization` header** — standard `Bearer <JWT>` scheme.
//! 4. **`Sec-WebSocket-Protocol` header** — `bearer, <JWT>` subprotocol (for WebSocket
//!    connections that cannot set custom headers).
//!
//! # Session refresh
//!
//! The database stored procedure (`saps.ping`) may regenerate the session UUID when the
//! session's `date_created` is older than 5 minutes. When this happens, the extractor:
//!
//! 1. Updates the token's `unique_id` to the new UUID.
//! 2. Re-encodes a fresh JWT.
//! 3. Inserts an [`UpdatedAuthCookie`] into the request extensions so that downstream
//!    middleware or handlers can set the updated `Set-Cookie` header on the response.
//!
//! # Generic parameters
//!
//! | Parameter | Constraint | Purpose |
//! |-----------|-----------|---------|
//! | `X` | [`GetConfigVariable`] | Provides `SECRET_KEY` (for signing) and `TOKEN_EXPIRE_MINS` (for expiry) |
//! | `Y` | [`CheckUserRole`] | Role-check strategy — controls which roles are permitted (e.g. `AdminRoleCheck`) |
//! | `R` | [`UserRole`] | The concrete role enum type (e.g. `MyRole`) |
//! | `Z` | [`YieldPostGresPool`] | Provides the PostgreSQL connection pool for session operations |
//!
//! # Example
//!
//! ```
//! use saps::auth::token::header_token::HeaderToken;
//! use saps::auth::token::checks::{
//!     AdminRoleCheck, NoRoleCheck, DefaultRole,
//! };
//! use saps::config::EnvConfig;
//! use saps::dal::connections::MockDeadPostGresPool;
//! use saps::errors::saps::SapsError;
//!
//! // Type alias for a token that requires at least Admin role.
//! // In production, replace EnvConfig/MockDeadPostGresPool with your
//! // own config provider and live pool.
//! type AdminToken = HeaderToken<EnvConfig, AdminRoleCheck, DefaultRole, MockDeadPostGresPool>;
//!
//! // Manually create and encode a token (typically done via the login module).
//! // Here we set the required env vars so the example runs.
//! // SAFETY: single-threaded doc test — no concurrent readers of these env vars.
//! unsafe {
//!     std::env::set_var("TOKEN_EXPIRE_MINS", "20");
//!     std::env::set_var("SECRET_KEY", "my-secret-key");
//! }
//!
//! let token = AdminToken::new::<DefaultRole>().unwrap();
//! let jwt_string = token.encode().unwrap();
//!
//! // Decode a JWT string back into a HeaderToken.
//! let decoded = AdminToken::decode(&jwt_string).unwrap();
//! assert_eq!(decoded.unique_id.len(), 36); // UUID format
//! ```
use crate::{
    auth::{
        auth_trace,
        dal::model::AuthSession,
        token::{
            checks::{CheckUserRole, UserRole},
            helper_methods::{
                jwt_claims::JwtClaims,
                meta::{
                    compare_and_swap_auth_session_meta::compare_and_swap_auth_session_meta as cas_inner,
                    delete_auth_session::delete_auth_session as delete_auth_session_inner,
                    delete_auth_session_meta_key::delete_auth_session_meta_key as delete_meta_key_inner,
                    refresh_auth_session::refresh_auth_session as refresh_auth_session_inner,
                    update_auth_session_meta::update_auth_session_meta as update_meta_inner,
                    upsert_auth_session_meta_key::upsert_auth_session_meta_key as upsert_meta_key_inner,
                    upsert_auth_sessions_meta_key_by_meta_key_pair::upsert_auth_sessions_meta_key_by_meta_key_pair as upsert_pair_inner,
                },
                run_auth_extraction::run_auth_extraction,
            },
        },
    },
    config::GetConfigVariable,
    dal::connections::YieldPostGresPool,
    errors::saps::SapsError,
};
use axum::{extract::FromRequestParts, http::request::Parts};
use chrono::{DateTime, Utc};
use serde::{Deserialize, de::DeserializeOwned};
use std::{future::Future, marker::PhantomData, pin::Pin};
use uuid::Uuid;

/// A `Set-Cookie` value produced by the extractor when the stored procedure
/// regenerates the session UUID (because `date_created` was older than 5 minutes).
///
/// The extractor writes one of these into the
/// [`CookieSlot`](crate::auth::middleware::CookieSlot) installed by
/// [`attach_refreshed_cookie`](crate::auth::middleware::attach_refreshed_cookie).
/// The layer then reads it back after the handler runs and attaches it as a
/// `Set-Cookie` header on the response.
///
/// You normally don't construct or read this directly — apply the layer at
/// router level and the refresh happens transparently. For per-request
/// observability of rotation events, read [`HeaderToken::old_uuid`] inside
/// your handler instead.
#[derive(Debug, Clone)]
pub struct UpdatedAuthCookie(pub String);

/// A JWT authentication token that doubles as an axum request extractor.
///
/// This struct is serialized into (and deserialized from) a signed HS256 JWT. Only
/// the `unique_id` and `time_expire` fields are included in the JWT payload — the
/// generic marker fields (`PhantomData`) and `meta` are skipped during serialization.
///
/// When used as an axum extractor (via [`FromRequestParts`]), it performs the full
/// authentication flow: token extraction, JWT decoding, session ping, and role check.
/// See the [module-level documentation](self) for details on extraction order and
/// session refresh.
///
/// # Fields
///
/// | Field | In JWT? | Description |
/// |-------|---------|-------------|
/// | `unique_id` | Yes | UUID linking this token to a row in `saps.auth_sessions` |
/// | `time_expire` | Yes | When the JWT itself expires (separate from session expiry) |
/// | `var_handle` | No | Phantom marker for the config provider `X` |
/// | `role_handle` | No | Phantom marker for the role-check strategy `Y` |
/// | `db_handle` | No | Phantom marker for the database pool provider `Z` |
/// | `role` | No | Phantom marker for the concrete role enum `R` |
/// | `auth_session` | No | The full [`AuthSession`] loaded from the DB during extraction |
/// | `old_uuid` | No | The previous UUID when the extractor rotated the session, otherwise `None` |
#[derive(Debug, Clone)]
#[cfg_attr(feature = "openapi", derive(aide::OperationIo))]
#[cfg_attr(feature = "openapi", aide(input))]
pub struct HeaderToken<X: GetConfigVariable, Y: CheckUserRole, R: UserRole, Z: YieldPostGresPool> {
    /// The UUID that links this token to its `saps.auth_sessions` row.
    /// Stored as a string in the JWT payload.
    pub unique_id: String,
    /// The timestamp after which this JWT is considered expired.
    /// Set at creation time to `now + TOKEN_EXPIRE_MINS`.
    pub time_expire: DateTime<Utc>,
    /// Phantom marker for the config provider type `X`.
    pub var_handle: PhantomData<X>,
    /// Phantom marker for the role-check strategy type `Y`.
    pub role_handle: PhantomData<Y>,
    /// Phantom marker for the database pool provider type `Z`.
    pub db_handle: PhantomData<Z>,
    /// Phantom marker for the concrete role enum type `R`.
    pub role: PhantomData<R>,
    /// The session row loaded from `saps.auth_sessions` during extraction.
    ///
    /// `None` for freshly created tokens; the [`FromRequestParts`] impl
    /// populates this with the row returned by `saps.ping`. Handlers can
    /// reach the meta JSON via `auth_session.meta` (or the typed
    /// `AuthSession::meta_get*` helpers) and call further DAL operations
    /// without re-fetching the session.
    pub auth_session: Option<AuthSession<R>>,
    /// The previous UUID, populated only when the extractor detected a session
    /// rotation during [`FromRequestParts`]. `None` for freshly created tokens
    /// and for requests where no rotation occurred. Not part of the JWT payload —
    /// handlers can read this to log/observe rotation events.
    pub old_uuid: Option<String>,
}


impl<X: GetConfigVariable, Y: CheckUserRole, R: UserRole, Z: YieldPostGresPool>
    HeaderToken<X, Y, R, Z>
{
    /// Creates a new token with a random UUID and an expiry derived from config.
    ///
    /// Reads `TOKEN_EXPIRE_MINS` from the config provider `X` and sets `time_expire`
    /// to `now + TOKEN_EXPIRE_MINS` minutes. The `meta` field is initialized to `None`.
    ///
    /// # Config requirements
    ///
    /// - `TOKEN_EXPIRE_MINS` — integer string specifying the token lifetime in minutes.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if:
    /// - `TOKEN_EXPIRE_MINS` is not set in the config provider.
    /// - `TOKEN_EXPIRE_MINS` cannot be parsed as an `i64`.
    pub fn new<U: UserRole>() -> Result<Self, SapsError> {
        let token_expire_mins =
            match X::get_config_variable("TOKEN_EXPIRE_MINS")?.parse::<i64>() {
                Ok(num) => num,
                Err(error) => return Err(SapsError::unknown(error.to_string())),
            };
        Ok(HeaderToken {
            unique_id: Uuid::new_v4().to_string(),
            time_expire: Utc::now() + chrono::Duration::minutes(token_expire_mins),
            var_handle: PhantomData,
            role_handle: PhantomData,
            db_handle: PhantomData,
            role: PhantomData,
            auth_session: None,
            old_uuid: None,
        })
    }

    /// Sets the id. This is used if tethering the token to an auth session.
    ///
    /// # Arguments
    /// - `id`: The id to be attached to the `self.unique_id`
    ///
    /// # Returns
    /// The constructed header token
    pub fn set_uuid(mut self, id: &Uuid) -> Self {
        self.unique_id = id.to_string();
        self
    }

    /// Checks whether the token's `time_expire` has passed.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::unauthorized`] with the message `"Token has expired"` if
    /// the current time is past `time_expire`.
    pub fn check_if_expired(&self) -> Result<(), SapsError> {
        if Utc::now() > self.time_expire {
            Err(SapsError::unauthorized("Token has expired".to_string()))
        } else {
            Ok(())
        }
    }

    /// Returns a reference to the [`AuthSession`] populated by the extractor.
    ///
    /// Errors if the token has not been extracted yet (i.e. it was created
    /// via [`new`](Self::new) and has not gone through [`FromRequestParts`]).
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if `auth_session` is `None`.
    pub fn get_auth_session(&self) -> Result<&AuthSession<R>, SapsError> {
        self.auth_session
            .as_ref()
            .ok_or_else(|| SapsError::bad_request("auth session not present on token"))
    }

    /// Returns a reference to the session metadata, or an error if it was not populated.
    ///
    /// Metadata is loaded from the `meta` JSONB column in `saps.auth_sessions` during
    /// extraction. If the session has no metadata (i.e. `meta` is `NULL` in the database),
    /// this method returns an error.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded, or its `meta` column is `NULL`.
    pub fn get_meta(&self) -> Result<&serde_json::Value, SapsError> {
        self.get_auth_session()?
            .meta
            .as_ref()
            .ok_or_else(|| SapsError::bad_request("session meta not present"))
    }

    /// Deletes the auth session associated with this token from the database.
    ///
    /// Parses `unique_id` as a UUID and calls
    /// [`AuthPostGresDescriptor::<Z>::delete_auth_session`]. This is useful for
    /// implementing logout endpoints.
    ///
    /// # Returns
    ///
    /// - `Ok(true)` if the session existed and was deleted.
    /// - `Ok(false)` if no session was found with this UUID.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the UUID is malformed or the database query fails.
    pub fn delete_auth_session(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SapsError>> + Send + '_>> {
        Box::pin(delete_auth_session_inner::<Z>(&self.unique_id))
    }

    /// Replaces the entire `meta` JSON for this token's auth session.
    ///
    /// Wraps [`AuthPostGresDescriptor::<Z>::update_auth_session_meta`] using
    /// the token's `unique_id`. The cached [`auth_session`](Self::auth_session)
    /// is updated in place to reflect the new meta value, so subsequent reads
    /// via [`get_meta`](Self::get_meta) or
    /// [`get_auth_session`](Self::get_auth_session) see the change without an
    /// extra DB round-trip.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the database query fails.
    pub fn update_auth_session_meta(
        &mut self,
        meta: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send + '_>> {
        Box::pin(async move {
            update_meta_inner::<Z>(&self.unique_id, meta.clone()).await?;
            if let Some(session) = self.auth_session.as_mut() {
                session.meta = Some(meta);
            }
            Ok(())
        })
    }

    /// Sets a single top-level `key` in the auth session's `meta`, leaving all
    /// other keys intact.
    ///
    /// Wraps [`AuthPostGresDescriptor::<Z>::upsert_auth_session_meta_key`].
    /// When the DAL returns the post-update row, the cached
    /// [`auth_session`](Self::auth_session) is replaced with it; if the row
    /// no longer exists, the cached session is left untouched and the call
    /// is a silent no-op.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the database query fails.
    pub fn upsert_auth_session_meta_key<'a>(
        &'a mut self,
        key: &'a str,
        value: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send + 'a>> {
        Box::pin(async move {
            let updated = upsert_meta_key_inner::<R, Z>(&self.unique_id, key, value).await?;
            if let Some(session) = updated {
                self.auth_session = Some(session);
            }
            Ok(())
        })
    }

    /// Sets `upsert_key`/`upsert_value` on every session whose `meta` matches
    /// both `(match_key1, match_value1)` and `(match_key2, match_value2)`,
    /// returning the number of rows updated.
    ///
    /// Wraps
    /// [`AuthPostGresDescriptor::<Z>::upsert_auth_sessions_meta_key_by_meta_key_pair`].
    /// Unlike the other `*_meta_*` methods on this token, the match is keyed
    /// on the pair — not on `self.unique_id` — so the call may touch zero
    /// rows, this token's row, sibling rows, or all of the above. Pair with
    /// the composite partial unique index from
    /// [`AuthSession::generate_unique_meta_key_pair_sql`](crate::auth::dal::model::AuthSession::generate_unique_meta_key_pair_sql)
    /// to guarantee the return value is `0` or `1`.
    ///
    /// The cached [`auth_session`](Self::auth_session) is **not** updated by
    /// this call (the DAL returns a row count, not the post-update rows). If
    /// you need the latest meta on this token afterwards, call
    /// [`refresh_auth_session`](Self::refresh_auth_session) or use one of
    /// the non-`_local` `meta_get*` methods.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the database query fails.
    pub fn upsert_auth_sessions_meta_key_by_meta_key_pair<'a>(
        &'a self,
        match_key1: &'a str,
        match_value1: serde_json::Value,
        match_key2: &'a str,
        match_value2: serde_json::Value,
        upsert_key: &'a str,
        upsert_value: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<u64, SapsError>> + Send + 'a>> {
        Box::pin(upsert_pair_inner::<Z>(
            match_key1,
            match_value1,
            match_key2,
            match_value2,
            upsert_key,
            upsert_value,
        ))
    }

    /// Removes a single top-level `key` from the auth session's `meta`,
    /// leaving all other keys intact.
    ///
    /// Wraps [`AuthPostGresDescriptor::<Z>::delete_auth_session_meta_key`].
    /// When the DAL returns the post-update row, the cached
    /// [`auth_session`](Self::auth_session) is replaced with it; if the row
    /// no longer exists, the cached session is left untouched and the call
    /// is a silent no-op.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the database query fails.
    pub fn delete_auth_session_meta_key<'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SapsError>> + Send + 'a>> {
        Box::pin(async move {
            let updated = delete_meta_key_inner::<R, Z>(&self.unique_id, key).await?;
            if let Some(session) = updated {
                self.auth_session = Some(session);
            }
            Ok(())
        })
    }

    /// Atomic compare-and-swap on a single top-level `meta` key for this
    /// token's auth session.
    ///
    /// Wraps
    /// [`AuthPostGresDescriptor::<Z>::compare_and_swap_auth_session_meta`].
    /// The swap goes through only if `meta[key]` currently equals
    /// `expected`; on success the cached
    /// [`auth_session`](Self::auth_session) is updated to the post-swap row
    /// and this method returns `Ok(true)`. If the session is gone, the key
    /// is absent, or the current value differs, the cache is left
    /// untouched and this method returns `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the database query fails.
    pub fn compare_and_swap_auth_session_meta<'a>(
        &'a mut self,
        key: &'a str,
        expected: serde_json::Value,
        new_value: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SapsError>> + Send + 'a>> {
        Box::pin(async move {
            let updated =
                cas_inner::<R, Z>(&self.unique_id, key, expected, new_value).await?;
            let swapped = updated.is_some();
            if let Some(session) = updated {
                self.auth_session = Some(session);
            }
            Ok(swapped)
        })
    }

    /// Re-fetches the auth session from the database and stores it on the
    /// token, replacing any stale cached value.
    ///
    /// Use this before reading `meta` if another task may have written to the
    /// session since the extractor ran. The non-`_local` getters
    /// (e.g. [`meta_get`](Self::meta_get)) call this for you; the `_local`
    /// getters do not, and read whatever cached value is already on the
    /// token.
    ///
    /// Backed by [`GetAuthSessionStrict`], so a missing row surfaces as
    /// `sqlx::Error::RowNotFound` (wrapped in [`SapsError`]).
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if:
    /// - The session no longer exists in the database.
    /// - The query fails or the role cannot be parsed back into `R`.
    pub fn refresh_auth_session(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<&AuthSession<R>, SapsError>> + Send + '_>> {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.get_auth_session()
        })
    }

    /// Reads `meta[key]` from the **cached** auth session attached to this
    /// token. Forwards to [`AuthSession::meta_get`].
    ///
    /// **Warning:** this does not hit the database. If another task may have
    /// updated the session's `meta` since the extractor ran, the cached
    /// value here may be stale. Use [`meta_get`](Self::meta_get) for a fresh
    /// read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token (i.e. the extractor hasn't run).
    pub fn meta_get_local(&self, key: &str) -> Result<Option<&serde_json::Value>, SapsError> {
        Ok(self.get_auth_session()?.meta_get(key))
    }

    /// Reads `meta[key]` from the **cached** auth session, returning an owned
    /// clone. Forwards to [`AuthSession::meta_get_owned`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use [`meta_get_owned`](Self::meta_get_owned) for a fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token.
    pub fn meta_get_owned_local(
        &self,
        key: &str,
    ) -> Result<Option<serde_json::Value>, SapsError> {
        Ok(self.get_auth_session()?.meta_get_owned(key))
    }

    /// Reads `meta[key]` from the **cached** auth session. Forwards to
    /// [`AuthSession::meta_get_strict`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use [`meta_get_strict`](Self::meta_get_strict) for a fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token, or [`SapsError::not_found`] if `key` is not
    /// present in `meta`.
    pub fn meta_get_strict_local(&self, key: &str) -> Result<&serde_json::Value, SapsError> {
        self.get_auth_session()?.meta_get_strict(key)
    }

    /// Reads `meta[key]` from the **cached** auth session, returning an owned
    /// clone. Forwards to [`AuthSession::meta_get_strict_owned`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use [`meta_get_strict_owned`](Self::meta_get_strict_owned) for
    /// a fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token, or [`SapsError::not_found`] if `key` is not
    /// present in `meta`.
    pub fn meta_get_strict_owned_local(
        &self,
        key: &str,
    ) -> Result<serde_json::Value, SapsError> {
        self.get_auth_session()?.meta_get_strict_owned(key)
    }

    /// Reads `meta[key]` from the **cached** auth session and deserializes
    /// it as `T`. Forwards to [`AuthSession::meta_get_typed`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use [`meta_get_typed`](Self::meta_get_typed) for a fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token, or if the value at `key` cannot be decoded
    /// as `T`.
    pub fn meta_get_typed_local<'a, T>(&'a self, key: &str) -> Result<Option<T>, SapsError>
    where
        T: Deserialize<'a>,
    {
        self.get_auth_session()?.meta_get_typed(key)
    }

    /// Reads `meta[key]` from the **cached** auth session and deserializes
    /// it as `T`. Forwards to [`AuthSession::meta_get_typed_owned`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use [`meta_get_typed_owned`](Self::meta_get_typed_owned) for a
    /// fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token, or if the value at `key` cannot be decoded
    /// as `T`.
    pub fn meta_get_typed_owned_local<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, SapsError> {
        self.get_auth_session()?.meta_get_typed_owned(key)
    }

    /// Reads `meta[key]` from the **cached** auth session and deserializes
    /// it as `T`. Forwards to [`AuthSession::meta_get_typed_strict`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use [`meta_get_typed_strict`](Self::meta_get_typed_strict) for
    /// a fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token, [`SapsError::not_found`] if `key` is not
    /// present in `meta`, or [`SapsError::bad_request`] if the value cannot
    /// be decoded as `T`.
    pub fn meta_get_typed_strict_local<'a, T>(&'a self, key: &str) -> Result<T, SapsError>
    where
        T: Deserialize<'a>,
    {
        self.get_auth_session()?.meta_get_typed_strict(key)
    }

    /// Reads `meta[key]` from the **cached** auth session and deserializes
    /// it as `T`. Forwards to [`AuthSession::meta_get_typed_strict_owned`].
    ///
    /// **Warning:** this does not hit the database. The cached value may be
    /// stale. Use
    /// [`meta_get_typed_strict_owned`](Self::meta_get_typed_strict_owned) for
    /// a fresh read.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if the auth session has not been
    /// loaded onto this token, [`SapsError::not_found`] if `key` is not
    /// present in `meta`, or [`SapsError::bad_request`] if the value cannot
    /// be decoded as `T`.
    pub fn meta_get_typed_strict_owned_local<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<T, SapsError> {
        self.get_auth_session()?.meta_get_typed_strict_owned(key)
    }

    /// Refreshes the auth session from the database and reads `meta[key]`.
    ///
    /// Calls [`refresh_auth_session`](Self::refresh_auth_session) before
    /// delegating to [`meta_get_local`](Self::meta_get_local), so the value
    /// reflects the row currently in the database (immune to concurrent
    /// writes from other tasks since the extractor ran).
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails (see
    /// [`refresh_auth_session`](Self::refresh_auth_session)).
    pub fn meta_get<'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<&'a serde_json::Value>, SapsError>> + Send + 'a>>
    {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`,
    /// returning an owned clone.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails.
    pub fn meta_get_owned<'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<serde_json::Value>, SapsError>> + Send + 'a>>
    {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_owned_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails, or
    /// [`SapsError::not_found`] if `key` is not present in `meta`.
    pub fn meta_get_strict<'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<&'a serde_json::Value, SapsError>> + Send + 'a>>
    {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_strict_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`,
    /// returning an owned clone.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails, or
    /// [`SapsError::not_found`] if `key` is not present in `meta`.
    pub fn meta_get_strict_owned<'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, SapsError>> + Send + 'a>> {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_strict_owned_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`
    /// deserialized as `T`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails, or if the value at `key`
    /// cannot be decoded as `T`.
    pub fn meta_get_typed<'a, T>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<T>, SapsError>> + Send + 'a>>
    where
        T: Deserialize<'a> + Send + 'a,
    {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_typed_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`
    /// deserialized as `T`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails, or if the value at `key`
    /// cannot be decoded as `T`.
    pub fn meta_get_typed_owned<'a, T: DeserializeOwned + Send + 'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<T>, SapsError>> + Send + 'a>> {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_typed_owned_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`
    /// deserialized as `T`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails, [`SapsError::not_found`]
    /// if `key` is not present, or a decode error if the value cannot be
    /// parsed as `T`.
    pub fn meta_get_typed_strict<'a, T>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<T, SapsError>> + Send + 'a>>
    where
        T: Deserialize<'a> + Send + 'a,
    {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_typed_strict_local(key)
        })
    }

    /// Refreshes the auth session from the database and reads `meta[key]`
    /// deserialized as `T`.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the refresh fails, [`SapsError::not_found`]
    /// if `key` is not present, or a decode error if the value cannot be
    /// parsed as `T`.
    pub fn meta_get_typed_strict_owned<'a, T: DeserializeOwned + Send + 'a>(
        &'a mut self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<T, SapsError>> + Send + 'a>> {
        Box::pin(async move {
            let session = refresh_auth_session_inner::<R, Z>(&self.unique_id).await?;
            self.auth_session = Some(session);
            self.meta_get_typed_strict_owned_local(key)
        })
    }

    /// Encodes this token into a signed HS256 JWT string.
    ///
    /// Reads `SECRET_KEY` from the config provider `X` and uses it as the HMAC signing
    /// key. This method consumes `self` because the token should not be reused after
    /// encoding (the caller should work with the JWT string from this point on).
    ///
    /// # Config requirements
    ///
    /// - `SECRET_KEY` — the HMAC secret used to sign the JWT.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if:
    /// - `SECRET_KEY` is not set in the config provider.
    /// - JWT encoding fails (should not happen with valid inputs).
    pub fn encode(self) -> Result<String, SapsError> {
        // Delegate to the non-generic-over-(Y,R,Z) JwtClaims so the heavy
        // jsonwebtoken machinery is monomorphized once per `X`, not once
        // per `(X, Y, R, Z)`.
        JwtClaims {
            unique_id: self.unique_id,
            time_expire: self.time_expire,
        }
        .encode::<X>()
    }

    /// Decodes a JWT string into a `HeaderToken`.
    ///
    /// Reads `SECRET_KEY` from the config provider `X` and verifies the HS256 signature.
    /// The `exp` claim is **not** validated during decoding — expiry checking is handled
    /// separately via [`check_if_expired`](Self::check_if_expired).
    ///
    /// # Arguments
    ///
    /// * `token` — the raw JWT string to decode.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::unauthorized`] if:
    /// - `SECRET_KEY` is not set in the config provider.
    /// - The JWT signature is invalid.
    /// - The JWT payload cannot be deserialized into this struct.
    pub fn decode(token: &str) -> Result<Self, SapsError> {
        // Delegate to JwtClaims for the same monomorphization-sharing reason
        // as `encode`.
        let claims = JwtClaims::decode::<X>(token)?;
        Ok(HeaderToken {
            unique_id: claims.unique_id,
            time_expire: claims.time_expire,
            var_handle: PhantomData,
            role_handle: PhantomData,
            db_handle: PhantomData,
            role: PhantomData,
            auth_session: None,
            old_uuid: None,
        })
    }

}

/// Axum [`FromRequestParts`] implementation that performs the full authentication flow.
///
/// When `HeaderToken` is used as a handler parameter, this implementation runs before
/// the handler and performs the following steps:
///
/// 1. **Extract** the JWT from cookies, the `token` header, or the `Authorization` /
///    `Sec-WebSocket-Protocol` header (in that order).
/// 2. **Decode** the JWT and verify its HS256 signature.
/// 3. **Ping** the database session via `saps.ping(10, session_id)`. If the session
///    does not exist or has been inactive for more than 10 minutes, the request is
///    rejected with `401 Unauthorized`.
/// 4. **Check the role** by calling `Y::check_user_role(&session.role)`. If the
///    session's role does not satisfy the check strategy `Y`, the request is rejected.
/// 5. **Refresh the session** if the stored procedure regenerated the UUID (because
///    `date_created` was older than 5 minutes). When this happens, a new JWT is encoded
///    and an [`UpdatedAuthCookie`] is inserted into the request extensions.
/// 6. **Populate metadata** from the session's `meta` JSONB column.
///
/// # Rejection
///
/// Returns [`SapsError`] (which implements `IntoResponse`) with status `401 Unauthorized`
/// if any step fails.
impl<S, X, Y, R, Z> FromRequestParts<S> for HeaderToken<X, Y, R, Z>
where
    S: Send + Sync,
    X: GetConfigVariable + Send + Sync,
    Y: CheckUserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
    R: UserRole + Send + Sync,
{
    type Rejection = SapsError;

    /// Extracts and validates the authentication token from the incoming request.
    ///
    /// This method fires automatically before the handler function when `HeaderToken`
    /// is listed as a handler parameter. See the [`FromRequestParts`] implementation
    /// documentation above for the full authentication flow.
    ///
    /// # Arguments
    ///
    /// * `parts` — the HTTP request parts (headers, extensions, etc.).
    /// * `_state` — the axum router state (unused).
    ///
    /// # Returns
    ///
    /// A fully validated `HeaderToken` with `meta` populated from the database, or
    /// a [`SapsError`] rejection.
    // Thin shim over `run_auth_extraction`. The heavy lifting — token
    // extraction, JWT decode, DB ping, rotation handling — lives in
    // `run_auth_extraction` and is generic only over `(X, R, Z)`. The role
    // check is the only `Y`-dependent step and stays here.
    //
    // The boxed return type prevents the (small) wrapping async state
    // machine from being inlined into every handler call site.
    #[allow(refining_impl_trait)]
    fn from_request_parts<'p, 's>(
        parts: &'p mut Parts,
        _state: &'s S,
    ) -> Pin<Box<dyn Future<Output = Result<Self, Self::Rejection>> + Send + 'p>> {
        Box::pin(async move {
            let raw = run_auth_extraction::<X, R, Z>(parts).await?;

            // Verify the session's role satisfies the check strategy Y.
            // This is the only place `Y` is consulted in the whole flow.
            Y::check_user_role(&raw.session.role).map_err(|e| {
                auth_trace!(
                    session_id = %raw.unique_id,
                    role = %raw.session.role.to_string(),
                    error = %e.message,
                    "from_request_parts — role check FAILED",
                );
                e
            })?;
            auth_trace!(
                session_id = %raw.unique_id,
                role = %raw.session.role.to_string(),
                rotated = raw.old_uuid.is_some(),
                "from_request_parts — auth flow complete",
            );

            Ok(HeaderToken {
                unique_id: raw.unique_id,
                time_expire: raw.time_expire,
                var_handle: PhantomData,
                role_handle: PhantomData,
                db_handle: PhantomData,
                role: PhantomData,
                auth_session: Some(raw.session),
                old_uuid: raw.old_uuid,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::dal::tx_definitions::CreateAuthSession;
    use crate::auth::token::checks::{
        AdminRoleCheck, CustomerRoleCheck, ExactAdminRoleCheck, NoRoleCheck, SuperAdminRoleCheck,
    };
    use crate::auth::token::helper_methods::extract_bearer_token::extract_bearer_token;
    use crate::dal::connections::AuthPostGresDescriptor;
    use crate::{
        auth::dal::model::AuthSession, dal::connections::MockDeadPostGresPool,
        errors::saps::SapsErrorStatus,
    };
    use axum::{
        Json, Router,
        body::{self, Body, Bytes},
        http::{
            HeaderMap, HeaderValue, Request, StatusCode,
            header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL},
        },
        response::IntoResponse,
        routing::get,
    };
    use serde_json::json;
    use tower::ServiceExt;

    // -- Test role enum --
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
    enum TestRole {
        SuperAdmin,
        Admin,
        Customer,
    }

    impl std::fmt::Display for TestRole {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                TestRole::SuperAdmin => write!(f, "superadmin"),
                TestRole::Admin => write!(f, "admin"),
                TestRole::Customer => write!(f, "customer"),
            }
        }
    }

    impl TryFrom<String> for TestRole {
        type Error = SapsError;
        fn try_from(value: String) -> Result<Self, Self::Error> {
            match value.to_lowercase().as_str() {
                "superadmin" => Ok(TestRole::SuperAdmin),
                "admin" => Ok(TestRole::Admin),
                "customer" => Ok(TestRole::Customer),
                _ => Err(SapsError::bad_request(format!("Unknown role: {}", value))),
            }
        }
    }

    impl UserRole for TestRole {}

    // -- Fake config that returns hardcoded values --
    #[derive(Clone)]
    struct FakeConfig;

    impl GetConfigVariable for FakeConfig {
        fn get_config_variable(variable: &str) -> Result<String, SapsError> {
            match variable {
                "SECRET_KEY" => Ok("test_secret".to_string()),
                "TOKEN_EXPIRE_MINS" => Ok("20".to_string()),
                _ => Err(SapsError::unknown(format!(
                    "key: {} was not found",
                    variable
                ))),
            }
        }
    }

    // -- Type aliases for HeaderToken variants --
    type TkNo = HeaderToken<FakeConfig, NoRoleCheck, TestRole, MockDeadPostGresPool>;

    // -- Helper to construct a token --
    fn construct_token() -> TkNo {
        HeaderToken::<FakeConfig, NoRoleCheck, TestRole, MockDeadPostGresPool>::new::<TestRole>()
            .unwrap()
    }

    // -- Handlers --
    async fn pass_handle(tok: TkNo) -> impl IntoResponse {
        Json(json!({ "unique_id": tok.unique_id }))
    }

    // -- Helper to send a request and collect (status, body) --
    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Bytes) {
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let body = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, body)
    }

    // ===== Unit tests =====

    #[test]
    fn test_encode_decode() {
        let jwt = construct_token();
        let encoded = jwt.encode().unwrap();
        let decoded = TkNo::decode(&encoded).unwrap();
        assert!(!decoded.unique_id.is_empty());
    }

    #[test]
    fn test_decode_preserves_unique_id() {
        let jwt = construct_token();
        let original_id = jwt.unique_id.clone();
        let raw = jwt.encode().unwrap();
        let decoded = TkNo::decode(&raw).unwrap();
        assert_eq!(decoded.unique_id, original_id);
    }

    #[test]
    fn test_check_if_expired_not_expired() {
        let jwt = construct_token();
        assert!(jwt.check_if_expired().is_ok());
    }

    #[test]
    fn test_check_if_expired_is_expired() {
        let mut jwt = construct_token();
        jwt.time_expire = Utc::now() - chrono::Duration::minutes(1);
        let err = jwt.check_if_expired().unwrap_err();
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
    }

    // ===== Bearer / header extraction tests =====

    #[test]
    fn test_extract_bearer_token_from_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc123"));
        let token = extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "abc123");
    }

    #[test]
    fn test_extract_bearer_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("bearer   T0KEN"));
        let token = extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "T0KEN");
    }

    #[test]
    fn test_missing_authorization_header() {
        let headers = HeaderMap::new();
        let err = extract_bearer_token(&headers).expect_err("expected error");
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
        assert!(err.message.contains("Missing Authorization header"));
    }

    #[test]
    fn test_wrong_scheme_is_unauthorized() {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Basic Zm9vOmJhcg=="),
        );
        let err = extract_bearer_token(&headers).expect_err("expected error");
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
        assert!(err.message.contains("Expected 'Bearer <token>'"));
    }

    #[test]
    fn test_bearer_without_token_is_unauthorized() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer"));
        let err = extract_bearer_token(&headers).expect_err("expected error");
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
    }

    #[test]
    fn ws_subprotocol_bearer_simple() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("bearer, jwt123"),
        );
        let token = extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "jwt123");
    }

    #[test]
    fn ws_subprotocol_bearer_case_insensitive_and_spaces() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("BeArEr,    42-XYZ "),
        );
        let token = extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "42-XYZ");
    }

    #[test]
    fn ws_subprotocol_non_bearer_falls_back_to_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("somethingelse, token-ignored"),
        );
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer real-token"));
        let token = extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "real-token");
    }

    // ===== Extractor-through-router tests =====

    #[tokio::test]
    async fn test_fail_no_token() {
        let app = Router::new().route("/", get(pass_handle));
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            Bytes::from_static(b"\"Missing Authorization header\"")
        );
    }

    // ===== Handler + extractor tests via #[db_test] =====
    // The extractor calls AuthPostGresDescriptor::<Z>::ping_auth_session, so Z must be
    // TestDbHandle (provided by #[db_test]) for a working pool. Since TestDbHandle is
    // only available inside the #[db_test] block, we use a macro to define the handler,
    // token type, and test body together.

    macro_rules! db_handler_test {
        ($test_name:ident, $check:ty, $role:expr, $expected_status:expr) => {
            #[saps::db_test]
            async fn $test_name() {
                type Tk = HeaderToken<FakeConfig, $check, TestRole, TestDbHandle>;

                async fn handler(tok: Tk) -> impl IntoResponse {
                    Json(json!({ "unique_id": tok.unique_id }))
                }

                let token: Tk = HeaderToken::new::<TestRole>().unwrap();
                let mut session = AuthSession::new($role);
                session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
                AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
                    .await
                    .expect("failed to create session");

                let app = Router::new().route("/", get(handler));
                let req = Request::builder()
                    .uri("/")
                    .header("token", token.encode().unwrap())
                    .body(Body::empty())
                    .unwrap();
                let (status, _body) = send(&app, req).await;
                assert_eq!(status, $expected_status);
            }
        };
    }

    db_handler_test!(
        test_pass_no_role_check,
        NoRoleCheck,
        TestRole::Admin,
        StatusCode::OK
    );
    db_handler_test!(
        test_pass_admin_check,
        AdminRoleCheck,
        TestRole::Admin,
        StatusCode::OK
    );
    db_handler_test!(
        test_pass_customer_check,
        CustomerRoleCheck,
        TestRole::Admin,
        StatusCode::OK
    );
    db_handler_test!(
        test_fail_super_admin_check,
        SuperAdminRoleCheck,
        TestRole::Admin,
        StatusCode::UNAUTHORIZED
    );
    db_handler_test!(
        test_pass_exact_admin_check,
        ExactAdminRoleCheck,
        TestRole::Admin,
        StatusCode::OK
    );

    // ===== Named handler tests using TkAdm/TkSup/TkCus/TkExa =====
    // These use the db_handler_test macro with explicit handler functions and the
    // MockDeadPostGresPool-based type aliases for encode/decode, while the actual
    // extractor runs against TestDbHandle via #[db_test].

    #[saps::db_test]
    async fn test_admin_handle_passes_for_admin_role() {
        type Tk = HeaderToken<FakeConfig, AdminRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::Admin);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&app, req).await.0, StatusCode::OK);
    }

    #[saps::db_test]
    async fn test_admin_handle_rejects_customer_role() {
        type Tk = HeaderToken<FakeConfig, AdminRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::Customer);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            Bytes::from_static(b"\"Role does not have sufficient permissions\"")
        );
    }

    #[saps::db_test]
    async fn test_super_admin_handle_passes_for_super_admin_role() {
        type Tk = HeaderToken<FakeConfig, SuperAdminRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::SuperAdmin);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&app, req).await.0, StatusCode::OK);
    }

    #[saps::db_test]
    async fn test_super_admin_handle_rejects_admin_role() {
        type Tk = HeaderToken<FakeConfig, SuperAdminRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::Admin);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            Bytes::from_static(b"\"Role does not have sufficient permissions\"")
        );
    }

    #[saps::db_test]
    async fn test_customer_handle_passes_for_all_roles() {
        type Tk = HeaderToken<FakeConfig, CustomerRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        for role in [TestRole::SuperAdmin, TestRole::Admin, TestRole::Customer] {
            let token: Tk = HeaderToken::new::<TestRole>().unwrap();
            let mut session = AuthSession::new(role);
            session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
            AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
                .await
                .expect("failed to create session");

            let app = Router::new().route("/", get(handler));
            let req = Request::builder()
                .uri("/")
                .header("token", token.encode().unwrap())
                .body(Body::empty())
                .unwrap();
            assert_eq!(send(&app, req).await.0, StatusCode::OK);
        }
    }

    #[saps::db_test]
    async fn test_exact_admin_handle_rejects_super_admin() {
        type Tk = HeaderToken<FakeConfig, ExactAdminRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::SuperAdmin);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            Bytes::from_static(b"\"Role does not have sufficient permissions\"")
        );
    }

    #[saps::db_test]
    async fn test_exact_admin_handle_rejects_customer() {
        type Tk = HeaderToken<FakeConfig, ExactAdminRoleCheck, TestRole, TestDbHandle>;
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({ "unique_id": tok.unique_id }))
        }

        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::Customer);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(
            body,
            Bytes::from_static(b"\"Role does not have sufficient permissions\"")
        );
    }

    // ===== Rotation + cookie attachment integration test =====
    //
    // End-to-end test of the rotation flow:
    //   1. Create a session and backdate `date_created` to >5 min ago.
    //   2. Hit a handler through the `attach_refreshed_cookie` layer with a JWT
    //      whose `unique_id` matches the original session id.
    //   3. The `saps.ping` stored procedure rotates the row to a new UUID.
    //   4. The extractor populates `token.old_uuid` and stashes the new cookie
    //      in request extensions.
    //   5. The layer copies the cookie onto the response as `Set-Cookie`.
    //   6. We assert: 200 OK, the response carries a Set-Cookie with a JWT that
    //      decodes to the rotated UUID, and the handler observed `old_uuid` set
    //      to the original UUID.

    #[saps::db_test]
    async fn test_rotation_emits_refreshed_set_cookie_and_populates_old_uuid() {
        use crate::auth::middleware::attach_refreshed_cookie;
        use crate::constants::AUTH_TOKEN_COOKIE_KEY;
        use axum::middleware::from_fn;

        type Tk = HeaderToken<FakeConfig, NoRoleCheck, TestRole, TestDbHandle>;

        // Handler echoes back what it observed from the (post-rotation) token.
        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({
                "unique_id": tok.unique_id,
                "old_uuid": tok.old_uuid,
            }))
        }

        // 1. Create a session whose id matches the JWT's unique_id.
        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let original_uuid = token.unique_id.clone();
        let mut session = AuthSession::new(TestRole::Admin);
        session.id = uuid::Uuid::parse_str(&original_uuid).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        // 2. Backdate date_created so the ping triggers a rotation.
        saps::sqlx::query("UPDATE saps.auth_sessions SET date_created = NOW() - INTERVAL '6 minutes' WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&original_uuid).unwrap())
            .execute(pool)
            .await
            .expect("failed to backdate session");

        // 3. Build a router with the rotation layer applied.
        let app = Router::new()
            .route("/", get(handler))
            .layer(from_fn(attach_refreshed_cookie));

        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();

        // 4. Send the request and capture status + headers + body.
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let set_cookie = resp
            .headers()
            .get(axum::http::header::SET_COOKIE)
            .expect("response should carry a refreshed Set-Cookie")
            .to_str()
            .unwrap()
            .to_string();
        let body = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();

        // 5. Status is OK and the cookie is the refreshed format.
        assert_eq!(status, StatusCode::OK);
        assert!(
            set_cookie.starts_with(&format!("{}=", AUTH_TOKEN_COOKIE_KEY)),
            "expected cookie to start with {}=, got {:?}",
            AUTH_TOKEN_COOKIE_KEY,
            set_cookie,
        );
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("Path=/"));
        assert!(set_cookie.contains("Max-Age=86400"));

        // 6. The cookie's JWT must decode to a NEW UUID different from the original.
        let new_jwt = set_cookie
            .split_once('=')
            .and_then(|(_, rest)| rest.split_once(';'))
            .map(|(jwt, _)| jwt.to_string())
            .expect("cookie value should parse");
        let decoded = Tk::decode(&new_jwt).expect("new JWT should decode");
        assert_ne!(decoded.unique_id, original_uuid, "UUID should have rotated");

        // 7. The handler saw old_uuid populated with the pre-rotation UUID.
        let body_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body_json["old_uuid"],
            serde_json::Value::String(original_uuid.clone())
        );
        assert_eq!(
            body_json["unique_id"],
            serde_json::Value::String(decoded.unique_id)
        );
    }

    #[saps::db_test]
    async fn test_no_rotation_leaves_old_uuid_none_and_no_set_cookie() {
        use crate::auth::middleware::attach_refreshed_cookie;
        use axum::middleware::from_fn;

        type Tk = HeaderToken<FakeConfig, NoRoleCheck, TestRole, TestDbHandle>;

        async fn handler(tok: Tk) -> impl IntoResponse {
            Json(json!({
                "unique_id": tok.unique_id,
                "old_uuid": tok.old_uuid,
            }))
        }

        // Fresh session — date_created is `NOW()`, so no rotation happens.
        let token: Tk = HeaderToken::new::<TestRole>().unwrap();
        let mut session = AuthSession::new(TestRole::Admin);
        session.id = uuid::Uuid::parse_str(&token.unique_id).unwrap();
        AuthPostGresDescriptor::<TestDbHandle>::create_auth_session(session)
            .await
            .expect("failed to create session");

        let app = Router::new()
            .route("/", get(handler))
            .layer(from_fn(attach_refreshed_cookie));

        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get(axum::http::header::SET_COOKIE).is_none(),
            "no rotation occurred, no Set-Cookie should be attached",
        );
        let body = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body_json["old_uuid"], serde_json::Value::Null);
    }
}
