//! Extracts the JWT from the custom `token` header.

use axum::http::HeaderMap;

use crate::errors::saps::SapsError;

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
pub fn extract_token_from_header(headers: &HeaderMap) -> Result<Option<String>, SapsError> {
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
