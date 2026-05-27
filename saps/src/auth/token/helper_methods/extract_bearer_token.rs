//! Extracts the JWT from the `Authorization` or `Sec-WebSocket-Protocol` header.

use axum::http::{
    HeaderMap,
    header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL},
};

use crate::errors::saps::SapsError;

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
        let s = raw
            .to_str()
            .map_err(|_| SapsError::unauthorized("Invalid Sec-WebSocket-Protocol header"))?;

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
