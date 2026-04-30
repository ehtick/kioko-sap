//! Login helper that creates an authenticated session and returns a cookie-bearing response.
//!
//! This module provides a single async function, [`login`], that handles the full login flow:
//!
//! 1. Generates a new [`HeaderToken`] with a fresh UUID and expiry.
//! 2. Persists an [`AuthSession`] to the database with the same UUID, the caller's role,
//!    and optional JSON metadata.
//! 3. Encodes the token as a JWT.
//! 4. Returns an axum-compatible tuple `(StatusCode, HeaderMap, Json<LoginResponse>)` with
//!    a `Set-Cookie` header so the client's browser stores the JWT automatically.
//!
//! # Cookie details
//!
//! The cookie is set with the following attributes:
//! - **Name**: `AUTH_TOKEN_COOKIE_KEY` (defined in `crate::constants`)
//! - **Value**: the encoded JWT string
//! - **HttpOnly**: prevents client-side JavaScript from reading the cookie
//! - **Path=/**: the cookie is sent with every request to the server
//! - **Max-Age=86400**: the cookie expires after 24 hours (86 400 seconds)
//!
//! # Generic parameters
//!
//! | Parameter | Constraint | Purpose |
//! |-----------|-----------|---------|
//! | `X` | [`GetConfigVariable`] | Provides `SECRET_KEY` and `TOKEN_EXPIRE_MINS` for JWT encoding |
//! | `R` | [`UserRole`] | The concrete role enum (e.g. `MyRole::Admin`) |
//! | `Z` | [`YieldPostGresPool`] | Provides the database connection pool for session persistence |
//! | `M` | [`Serialize`] | The type of optional metadata attached to the session |
//!
//! Note: the [`HeaderToken`] type requires a [`CheckUserRole`](crate::auth::token::checks::CheckUserRole) parameter, but since login
//! is minting a new session (not gating access), this function always uses [`NoRoleCheck`]
//! internally.
//!
//! # Example
//!
//! ```ignore
//! use saps::auth::token::login::login;
//! use saps::auth::token::checks::NoRoleCheck;
//! use axum::response::IntoResponse;
//!
//! async fn handle_login() -> Result<impl IntoResponse, SapsError> {
//!     let meta = serde_json::json!({"user_id": 42, "department": "engineering"});
//!     login::<MyConfig, MyRole, LivePostGresPool, _>(
//!         MyRole::Customer,
//!         Some(meta),
//!     ).await
//! }
//!
//! // Login without metadata:
//! async fn handle_login_no_meta() -> Result<impl IntoResponse, SapsError> {
//!     login::<MyConfig, MyRole, LivePostGresPool, ()>(
//!         MyRole::Admin,
//!         None,
//!     ).await
//! }
//! ```
use crate::{
    auth::{
        dal::{model::AuthSession, tx_definitions::CreateAuthSession},
        token::{
            checks::{NoRoleCheck, UserRole},
            header_token::HeaderToken,
        },
    },
    config::GetConfigVariable,
    constants::AUTH_TOKEN_COOKIE_KEY,
    dal::connections::{AuthPostGresDescriptor, YieldPostGresPool},
    errors::saps::SapsError,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use serde::Serialize;

/// The JSON body returned to the client on a successful login.
///
/// # Fields
///
/// * `token` — the encoded JWT string. Clients that don't rely on cookies (e.g. mobile apps
///   or CLI tools) can store this and send it in the `Authorization: Bearer <token>` header
///   or the `token` header on subsequent requests.
/// * `unique_id` — the UUID that identifies this session in the `saps.auth_sessions` table.
///   Useful for debugging or for explicitly deleting the session later via
///   [`HeaderToken::delete_auth_session`].
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    /// The encoded JWT string for this session.
    pub token: String,
    /// The UUID identifying this session in the database.
    pub unique_id: String,
}

/// Creates a new authenticated session and returns a cookie-bearing HTTP response.
///
/// This is the primary login entry point. It performs the full login flow:
///
/// 1. **Token creation** — generates a [`HeaderToken`] with a random UUID and an expiry
///    derived from the `TOKEN_EXPIRE_MINS` config variable.
/// 2. **Session persistence** — inserts an [`AuthSession`] into `saps.auth_sessions` with
///    the token's UUID as the primary key. If `meta` is `Some`, the serialized JSON is
///    stored in the `meta` JSONB column.
/// 3. **JWT encoding** — encodes the token using the `SECRET_KEY` config variable.
/// 4. **Response assembly** — builds a `(StatusCode::OK, HeaderMap, Json<LoginResponse>)`
///    tuple with a `Set-Cookie` header containing the JWT.
///
/// # Arguments
///
/// * `role` — the role to assign to the new session (e.g. `MyRole::Admin`).
/// * `meta` — optional metadata to attach to the session. Pass `None` (with a concrete
///   type like `Option<()>`) if no metadata is needed.
///
/// # Returns
///
/// An axum-compatible response tuple on success:
/// - **Status**: `200 OK`
/// - **Headers**: a single `Set-Cookie` header with the JWT
/// - **Body**: JSON containing the `token` and `unique_id`
///
/// # Errors
///
/// Returns [`SapsError`] if:
/// - The `TOKEN_EXPIRE_MINS` or `SECRET_KEY` config variables are missing or invalid.
/// - The database insert fails (e.g. connection error, constraint violation).
/// - JWT encoding fails.
/// - The `Set-Cookie` header value contains invalid characters.
///
/// # Example
///
/// ```ignore
/// use saps::auth::token::login::login;
///
/// // With metadata:
/// let response = login::<MyConfig, MyRole, LivePool, _>(
///     MyRole::Admin,
///     Some(serde_json::json!({"user_id": 1})),
/// ).await?;
///
/// // Without metadata:
/// let response = login::<MyConfig, MyRole, LivePool, ()>(
///     MyRole::Customer,
///     None,
/// ).await?;
/// ```
pub async fn login<X, R, Z, M>(
    role: R,
    meta: Option<M>,
) -> Result<(StatusCode, HeaderMap, axum::Json<LoginResponse>), SapsError>
where
    X: GetConfigVariable + Send + Sync,
    R: UserRole + Send + Sync,
    Z: YieldPostGresPool + Send + Sync,
    M: Serialize,
{
    // 1. Create a new token with a random UUID and expiry.
    //    NoRoleCheck is used because login mints a new session — no role gating needed.
    let token = HeaderToken::<X, NoRoleCheck, R, Z>::new::<R>()?;

    // 2. Build an AuthSession whose primary key matches the token's UUID.
    let mut session = AuthSession::new(role);
    session.id =
        uuid::Uuid::parse_str(&token.unique_id).map_err(|e| SapsError::unknown(e.to_string()))?;
    if let Some(m) = meta {
        session = session.with_meta(m);
    }

    // 3. Persist the session to the database.
    AuthPostGresDescriptor::<Z>::create_auth_session(session).await?;

    // 4. Encode the token as a JWT.
    let unique_id = token.unique_id.clone();
    let encoded = token.encode()?;

    // 5. Assemble the response with a Set-Cookie header.
    let login_response = LoginResponse {
        token: encoded.clone(),
        unique_id,
    };

    let cookie = format!(
        "{}={}; HttpOnly; Path=/; Max-Age=86400",
        AUTH_TOKEN_COOKIE_KEY, login_response.token
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|e| SapsError::unknown(e.to_string()))?,
    );
    Ok((StatusCode::OK, headers, axum::Json(login_response)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::dal::tx_definitions::GetAllAuthSessions;
    use crate::auth::token::checks::NoRoleCheck;
    use crate::auth::token::header_token::HeaderToken;
    use crate::dal::connections::AuthPostGresDescriptor;
    use crate::errors::saps::SapsErrorStatus;

    // -- Test role enum (mirrors the pattern used in header_token and model tests) --

    #[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
    enum TestRole {
        Admin,
        Customer,
    }

    impl std::fmt::Display for TestRole {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                TestRole::Admin => write!(f, "admin"),
                TestRole::Customer => write!(f, "customer"),
            }
        }
    }

    impl TryFrom<String> for TestRole {
        type Error = SapsError;
        fn try_from(value: String) -> Result<Self, Self::Error> {
            match value.to_lowercase().as_str() {
                "admin" => Ok(TestRole::Admin),
                "customer" => Ok(TestRole::Customer),
                _ => Err(SapsError::bad_request(format!("Unknown role: {}", value))),
            }
        }
    }

    impl UserRole for TestRole {}

    // -- Fake config that returns hardcoded values for SECRET_KEY and TOKEN_EXPIRE_MINS --

    #[derive(Clone)]
    struct FakeConfig;

    impl GetConfigVariable for FakeConfig {
        fn get_config_variable(variable: String) -> Result<String, SapsError> {
            match variable.as_str() {
                "SECRET_KEY" => Ok("test_secret".to_string()),
                "TOKEN_EXPIRE_MINS" => Ok("20".to_string()),
                _ => Err(SapsError::unknown(format!(
                    "key: {} was not found",
                    variable
                ))),
            }
        }
    }

    // -- Fake config that is missing TOKEN_EXPIRE_MINS to test error path --

    #[derive(Clone)]
    struct BrokenConfig;

    impl GetConfigVariable for BrokenConfig {
        fn get_config_variable(variable: String) -> Result<String, SapsError> {
            match variable.as_str() {
                "SECRET_KEY" => Ok("test_secret".to_string()),
                _ => Err(SapsError::unknown(format!(
                    "key: {} was not found",
                    variable
                ))),
            }
        }
    }

    /// Verifies that a successful login with no metadata:
    /// - returns HTTP 200
    /// - includes a Set-Cookie header with the correct cookie name
    /// - returns a JSON body with a non-empty token and unique_id
    /// - persists exactly one session in the database
    #[saps::db_test]
    async fn test_login_creates_session_without_meta() {
        // Ensure the table is empty before login.
        let all = AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<TestRole>()
            .await
            .expect("failed to get sessions");
        assert_eq!(all.len(), 0);

        let (status, headers, body) =
            login::<FakeConfig, TestRole, TestDbHandle, ()>(TestRole::Admin, None)
                .await
                .expect("login should succeed");

        // Status is 200 OK.
        assert_eq!(status, StatusCode::OK);

        // Set-Cookie header is present and starts with the correct cookie name.
        let cookie = headers
            .get(header::SET_COOKIE)
            .expect("Set-Cookie header should be present")
            .to_str()
            .expect("cookie should be valid UTF-8");
        assert!(cookie.starts_with(&format!("{}=", AUTH_TOKEN_COOKIE_KEY)));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Path=/"));
        assert!(cookie.contains("Max-Age=86400"));

        // Response body contains a non-empty token and unique_id.
        let response = body.0;
        assert!(!response.token.is_empty());
        assert!(!response.unique_id.is_empty());

        // Exactly one session exists in the database.
        let all = AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<TestRole>()
            .await
            .expect("failed to get sessions");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].role, TestRole::Admin);
        assert!(all[0].meta.is_none());
    }

    /// Verifies that login with metadata correctly persists the JSON in the session.
    #[saps::db_test]
    async fn test_login_creates_session_with_meta() {
        let meta = serde_json::json!({"user_id": 42, "department": "engineering"});

        let (_status, _headers, body) =
            login::<FakeConfig, TestRole, TestDbHandle, _>(TestRole::Customer, Some(meta.clone()))
                .await
                .expect("login should succeed");

        // Session in the database has the correct role and metadata.
        let all = AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<TestRole>()
            .await
            .expect("failed to get sessions");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].role, TestRole::Customer);
        assert_eq!(all[0].meta, Some(meta));

        // The unique_id in the response matches the session id in the database.
        assert_eq!(body.0.unique_id, all[0].id.to_string());
    }

    /// Verifies that the token returned by login can be decoded back into a valid
    /// HeaderToken with the correct unique_id.
    #[saps::db_test]
    async fn test_login_token_is_decodable() {
        let (_status, _headers, body) =
            login::<FakeConfig, TestRole, TestDbHandle, ()>(TestRole::Admin, None)
                .await
                .expect("login should succeed");

        // Decode the JWT and verify the unique_id matches.
        let decoded =
            HeaderToken::<FakeConfig, NoRoleCheck, TestRole, TestDbHandle>::decode(&body.0.token)
                .expect("token should decode successfully");
        assert_eq!(decoded.unique_id, body.0.unique_id);
    }

    /// Verifies that the session's primary key in the database matches the unique_id
    /// embedded in the JWT, ensuring the token and session are correctly linked.
    #[saps::db_test]
    async fn test_login_session_id_matches_token_id() {
        let (_status, _headers, body) =
            login::<FakeConfig, TestRole, TestDbHandle, ()>(TestRole::Customer, None)
                .await
                .expect("login should succeed");

        let all = AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<TestRole>()
            .await
            .expect("failed to get sessions");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id.to_string(), body.0.unique_id);
    }

    /// Verifies that calling login twice creates two independent sessions.
    #[saps::db_test]
    async fn test_multiple_logins_create_separate_sessions() {
        let (_, _, body1) = login::<FakeConfig, TestRole, TestDbHandle, ()>(TestRole::Admin, None)
            .await
            .expect("first login should succeed");

        let (_, _, body2) =
            login::<FakeConfig, TestRole, TestDbHandle, ()>(TestRole::Customer, None)
                .await
                .expect("second login should succeed");

        // Two distinct sessions exist.
        let all = AuthPostGresDescriptor::<TestDbHandle>::get_all_auth_sessions::<TestRole>()
            .await
            .expect("failed to get sessions");
        assert_eq!(all.len(), 2);

        // The two sessions have different UUIDs.
        assert_ne!(body1.0.unique_id, body2.0.unique_id);
    }

    /// Verifies that login fails with a meaningful error when the config is missing
    /// the TOKEN_EXPIRE_MINS variable.
    #[saps::db_test]
    async fn test_login_fails_with_missing_config() {
        let result = login::<BrokenConfig, TestRole, TestDbHandle, ()>(TestRole::Admin, None).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, SapsErrorStatus::Unknown);
    }
}
