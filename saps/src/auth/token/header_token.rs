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
use std::marker::PhantomData;
use axum::{
    extract::FromRequestParts,
    http::{
        header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL},
        request::Parts,
        HeaderMap,
    },
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{
    decode,
    encode,
    Algorithm,
    DecodingKey,
    EncodingKey,
    Header,
    Validation,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::{
    auth::{
        dal::tx_definitions::{PingAuthSession, DeleteAuthSession},
        token::checks::{CheckUserRole, UserRole},
    },
    config::GetConfigVariable,
    constants::AUTH_TOKEN_COOKIE_KEY,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};


/// A `Set-Cookie` value inserted into request extensions when the stored procedure
/// regenerates the session UUID (because `date_created` was older than 5 minutes).
///
/// Handlers or middleware can extract this from `request.extensions()` to include
/// the updated cookie in the response, ensuring the client's cookie stays in sync
/// with the new session ID.
///
/// # Usage in middleware
///
/// ```ignore
/// if let Some(cookie) = request.extensions().get::<UpdatedAuthCookie>() {
///     response.headers_mut().insert(
///         axum::http::header::SET_COOKIE,
///         cookie.0.parse().unwrap(),
///     );
/// }
/// ```
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
/// | `meta` | No | Session metadata loaded from the DB during extraction |
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeaderToken<X: GetConfigVariable, Y: CheckUserRole, R: UserRole, Z: YieldPostGresPool> {
    /// The UUID that links this token to its `saps.auth_sessions` row.
    /// Stored as a string in the JWT payload.
    pub unique_id: String,
    /// The timestamp after which this JWT is considered expired.
    /// Set at creation time to `now + TOKEN_EXPIRE_MINS`.
    pub time_expire: DateTime<Utc>,
    /// Phantom marker for the config provider type `X`.
    #[serde(skip)]
    pub var_handle: PhantomData<X>,
    /// Phantom marker for the role-check strategy type `Y`.
    #[serde(skip)]
    pub role_handle: PhantomData<Y>,
    /// Phantom marker for the database pool provider type `Z`.
    #[serde(skip)]
    pub db_handle: PhantomData<Z>,
    /// Phantom marker for the concrete role enum type `R`.
    #[serde(skip)]
    pub role: PhantomData<R>,
    /// Optional session metadata loaded from the `meta` JSONB column in
    /// `saps.auth_sessions` during extraction. This is `None` for freshly
    /// created tokens and is populated by the [`FromRequestParts`] implementation.
    #[serde(skip)]
    pub meta: Option<serde_json::Value>,
}

impl<X: GetConfigVariable, Y: CheckUserRole, R: UserRole, Z: YieldPostGresPool> HeaderToken<X, Y, R, Z> {

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
        let token_expire_mins = match X::get_config_variable("TOKEN_EXPIRE_MINS".into())?.parse::<i64>() {
            Ok(num) => num,
            Err(error) => return Err(SapsError::unknown(error.to_string()))
        };
        Ok(HeaderToken {
            unique_id: Uuid::new_v4().to_string(),
            time_expire: Utc::now() + chrono::Duration::minutes(token_expire_mins),
            var_handle: PhantomData,
            role_handle: PhantomData,
            db_handle: PhantomData,
            role: PhantomData,
            meta: None,
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

    /// Returns a reference to the session metadata, or an error if it was not populated.
    ///
    /// Metadata is loaded from the `meta` JSONB column in `saps.auth_sessions` during
    /// extraction. If the session has no metadata (i.e. `meta` is `NULL` in the database),
    /// this method returns an error.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::bad_request`] if `meta` is `None`.
    pub fn get_meta(&self) -> Result<&serde_json::Value, SapsError> {
        self.meta.as_ref().ok_or_else(|| SapsError::bad_request("session meta not present"))
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
    pub async fn delete_auth_session(&self) -> Result<bool, SapsError> {
        let session_id = uuid::Uuid::parse_str(&self.unique_id)
            .map_err(|e| SapsError::unknown(e.to_string()))?;
        let deleted = AuthPostGresDescriptor::<Z>::delete_auth_session(session_id).await?;
        Ok(deleted)
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
        let key_str = X::get_config_variable("SECRET_KEY".to_string())?;
        let key = EncodingKey::from_secret(key_str.as_ref());
        match encode(&Header::default(), &self, &key) {
            Ok(token) => Ok(token),
            Err(error) => Err(SapsError::unauthorized(error.to_string())),
        }
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
        let key_str = <X>::get_config_variable("SECRET_KEY".to_string())?;
        let key = DecodingKey::from_secret(key_str.as_ref());
        let mut validation = Validation::new(Algorithm::HS256);
        validation.required_spec_claims.remove("exp");

        match decode::<Self>(token, &key, &validation) {
            Ok(token_data) => Ok(token_data.claims),
            Err(error) => Err(SapsError::unauthorized(error.to_string())),
        }
    }

    /// Extracts the JWT from the `Cookie` header using [`AUTH_TOKEN_COOKIE_KEY`].
    ///
    /// Parses the `Cookie` header value as a semicolon-separated list of `name=value`
    /// pairs and looks for one matching [`AUTH_TOKEN_COOKIE_KEY`] (`saps-token`).
    ///
    /// # Arguments
    ///
    /// * `headers` — the request header map.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(token))` if the cookie was found.
    /// - `Ok(None)` if the `Cookie` header is absent or doesn't contain the key.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::unauthorized`] if the `Cookie` header contains non-UTF-8 bytes.
    fn extract_token_from_cookies(headers: &HeaderMap) -> Result<Option<String>, SapsError> {
        let cookie_header = match headers.get(axum::http::header::COOKIE) {
            Some(cookies) => cookies,
            None => return Ok(None),
        };

        let cookies_str = cookie_header
            .to_str()
            .map_err(|_| SapsError::unauthorized("Invalid cookie format".to_string()))?;
        Ok(Self::parse_cookie_value(cookies_str, AUTH_TOKEN_COOKIE_KEY))
    }

    /// Extracts the JWT from the custom `token` header.
    ///
    /// This is a legacy extraction method for clients that send the JWT in a header
    /// named `token` rather than using cookies or the `Authorization` header.
    ///
    /// # Arguments
    ///
    /// * `headers` — the request header map.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(token))` if the `token` header was found.
    /// - `Ok(None)` if the header is absent.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::unauthorized`] if the header value contains non-UTF-8 bytes.
    fn extract_token_from_header(headers: &HeaderMap) -> Result<Option<String>, SapsError> {
        let raw_data = match headers.get("token") {
            Some(token) => token,
            None => return Ok(None),
        };
        let token = raw_data
            .to_str()
            .map_err(|_| SapsError::unauthorized("token not a valid string".to_string()))
            .map(|s| s.to_string())?;
        Ok(Some(token))
    }

    /// Extracts the JWT from the `Authorization` or `Sec-WebSocket-Protocol` header.
    ///
    /// This is the final fallback in the extraction chain. It checks two locations:
    ///
    /// 1. **`Sec-WebSocket-Protocol: bearer, <JWT>`** — used by WebSocket clients that
    ///    cannot set custom headers. The subprotocol value must start with `bearer`
    ///    (case-insensitive), followed by a comma and the JWT.
    /// 2. **`Authorization: Bearer <JWT>`** — the standard OAuth 2.0 bearer token scheme.
    ///
    /// If neither header is present or valid, this method returns an error (unlike the
    /// cookie and `token` header methods which return `Ok(None)`).
    ///
    /// # Arguments
    ///
    /// * `headers` — the request header map.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError::unauthorized`] if:
    /// - Neither `Sec-WebSocket-Protocol` nor `Authorization` headers are present.
    /// - The `Authorization` header uses a scheme other than `Bearer`.
    /// - The `Authorization` header has `Bearer` but no token value.
    /// - Either header contains non-UTF-8 bytes.
    pub fn extract_bearer_token(headers: &HeaderMap) -> Result<String, SapsError> {
        // Prefer subprotocol: Sec-WebSocket-Protocol: bearer, <JWT>
        if let Some(raw) = headers.get(SEC_WEBSOCKET_PROTOCOL) {
            let s = raw.to_str().map_err(|_| {
                SapsError::unauthorized("Invalid Sec-WebSocket-Protocol header")
            })?;

            if let Some((p1, p2)) = s.split_once(',')
                && p1.trim().eq_ignore_ascii_case("bearer")
            {
                let jwt = p2.trim();
                if !jwt.is_empty() {
                    return Ok(jwt.to_owned());
                }
            }
        }

        // Fallback: Authorization: Bearer <token>
        let raw = headers
            .get(AUTHORIZATION)
            .ok_or_else(|| SapsError::unauthorized("Missing Authorization header"))?;

        let s = raw
            .to_str()
            .map_err(|_| SapsError::unauthorized("Invalid Authorization header"))?;

        let mut parts = s.split_whitespace();
        let scheme = parts.next().unwrap_or("");
        let token = parts.next().unwrap_or("");

        if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
            return Err(SapsError::unauthorized("Expected 'Bearer <token>'"));
        }
        Ok(token.to_owned())
    }

    /// Parses a single cookie value from a raw `Cookie` header string.
    ///
    /// Splits the header on semicolons, trims whitespace, and returns the value
    /// of the first cookie whose name matches `target_name`.
    ///
    /// # Arguments
    ///
    /// * `cookies` — the raw `Cookie` header string (e.g. `"foo=bar; saps-token=abc"`).
    /// * `target_name` — the cookie name to search for.
    ///
    /// # Returns
    ///
    /// `Some(value)` if found, `None` otherwise.
    fn parse_cookie_value(cookies: &str, target_name: &str) -> Option<String> {
        cookies
            .split(';')
            .filter_map(|cookie| {
                let cookie = cookie.trim();
                cookie.split_once('=')
            })
            .find(|(name, _)| name.trim() == target_name)
            .map(|(_, value)| value.trim().to_string())
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
    R: UserRole + Send + Sync
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
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let headers = &parts.headers;

        // Try cookie first, then the `token` header, then bearer as a fallback.
        let raw_token = match Self::extract_token_from_cookies(headers)? {
            Some(token) => token,
            None => match Self::extract_token_from_header(headers)? {
                Some(token) => token,
                None => Self::extract_bearer_token(headers)?,
            },
        };

        // Decode the JWT and verify the signature.
        let token = Self::decode(&raw_token)?;

        let existing_id = token.unique_id.clone();

        // Ping the session to keep it alive and check if it still exists.
        // Sessions inactive for more than 10 minutes are deleted by the stored procedure.
        let session = match AuthPostGresDescriptor::<Z>::ping_auth_session::<R>(
            10, &token.unique_id
        ).await? {
            Some(session) => session,
            None => return Err(SapsError::unauthorized("session not present"))
        };

        // Verify the session's role satisfies the check strategy Y.
        Y::check_user_role(&session.role)?;

        let mut token = token;

        // If the stored procedure regenerated the UUID (date_created was older than
        // 5 minutes), update the token and insert a new cookie into extensions.
        if session.id.to_string() != existing_id {
            token.unique_id = session.id.to_string();
            let new_jwt = token.encode()?;
            let cookie = format!(
                "{}={}; HttpOnly; Path=/; Max-Age=86400",
                AUTH_TOKEN_COOKIE_KEY, new_jwt
            );
            parts.extensions.insert(UpdatedAuthCookie(cookie));
            // re-decode the token so we return a valid struct (encode consumes self)
            token = Self::decode(&new_jwt)?;
        }
        token.meta = session.meta;
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::dal::tx_definitions::CreateAuthSession;
    use crate::{
        auth::dal::{
            model::AuthSession,
        },
        dal::connections::MockDeadPostGresPool,
        errors::saps::SapsErrorStatus,
    };
    use crate::auth::token::checks::{
        AdminRoleCheck, CustomerRoleCheck, ExactAdminRoleCheck, NoRoleCheck,
        SuperAdminRoleCheck,
    };
    use axum::{
        Json, Router,
        body::{self, Body, Bytes},
        http::{HeaderValue, Request, StatusCode},
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
        fn get_config_variable(variable: String) -> Result<String, SapsError> {
            match variable.as_str() {
                "SECRET_KEY" => Ok("test_secret".to_string()),
                "TOKEN_EXPIRE_MINS" => Ok("20".to_string()),
                _ => Err(SapsError::unknown(format!("key: {} was not found", variable))),
            }
        }
    }

    // -- Type aliases for HeaderToken variants --
    type TkNo = HeaderToken<FakeConfig, NoRoleCheck, TestRole, MockDeadPostGresPool>;

    // -- Helper to construct a token --
    fn construct_token() -> TkNo {
        HeaderToken::<FakeConfig, NoRoleCheck, TestRole, MockDeadPostGresPool>::new::<TestRole>().unwrap()
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
        let token = TkNo::extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "abc123");
    }

    #[test]
    fn test_extract_bearer_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("bearer   T0KEN"));
        let token = TkNo::extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "T0KEN");
    }

    #[test]
    fn test_missing_authorization_header() {
        let headers = HeaderMap::new();
        let err = TkNo::extract_bearer_token(&headers).expect_err("expected error");
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
        assert!(err.message.contains("Missing Authorization header"));
    }

    #[test]
    fn test_wrong_scheme_is_unauthorized() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Basic Zm9vOmJhcg=="));
        let err = TkNo::extract_bearer_token(&headers).expect_err("expected error");
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
        assert!(err.message.contains("Expected 'Bearer <token>'"));
    }

    #[test]
    fn test_bearer_without_token_is_unauthorized() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer"));
        let err = TkNo::extract_bearer_token(&headers).expect_err("expected error");
        assert_eq!(err.status, SapsErrorStatus::Unauthorized);
    }

    #[test]
    fn ws_subprotocol_bearer_simple() {
        let mut headers = HeaderMap::new();
        headers.insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("bearer, jwt123"));
        let token = TkNo::extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "jwt123");
    }

    #[test]
    fn ws_subprotocol_bearer_case_insensitive_and_spaces() {
        let mut headers = HeaderMap::new();
        headers.insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("BeArEr,    42-XYZ "));
        let token = TkNo::extract_bearer_token(&headers).expect("token");
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
        let token = TkNo::extract_bearer_token(&headers).expect("token");
        assert_eq!(token, "real-token");
    }

    // ===== Extractor-through-router tests =====

    #[tokio::test]
    async fn test_fail_no_token() {
        let app = Router::new().route("/", get(pass_handle));
        let req = Request::builder()
            .uri("/")
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, Bytes::from_static(b"\"Missing Authorization header\""));
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

    db_handler_test!(test_pass_no_role_check, NoRoleCheck, TestRole::Admin, StatusCode::OK);
    db_handler_test!(test_pass_admin_check, AdminRoleCheck, TestRole::Admin, StatusCode::OK);
    db_handler_test!(test_pass_customer_check, CustomerRoleCheck, TestRole::Admin, StatusCode::OK);
    db_handler_test!(test_fail_super_admin_check, SuperAdminRoleCheck, TestRole::Admin, StatusCode::UNAUTHORIZED);
    db_handler_test!(test_pass_exact_admin_check, ExactAdminRoleCheck, TestRole::Admin, StatusCode::OK);

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
            .await.expect("failed to create session");

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
            .await.expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, Bytes::from_static(b"\"Role does not have sufficient permissions\""));
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
            .await.expect("failed to create session");

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
            .await.expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, Bytes::from_static(b"\"Role does not have sufficient permissions\""));
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
                .await.expect("failed to create session");

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
            .await.expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, Bytes::from_static(b"\"Role does not have sufficient permissions\""));
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
            .await.expect("failed to create session");

        let app = Router::new().route("/", get(handler));
        let req = Request::builder()
            .uri("/")
            .header("token", token.encode().unwrap())
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, Bytes::from_static(b"\"Role does not have sufficient permissions\""));
    }
}
