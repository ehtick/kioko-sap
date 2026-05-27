//! Extracts the JWT from the `Cookie` header using [`AUTH_TOKEN_COOKIE_KEY`].

use axum::http::HeaderMap;

use crate::{
    auth::token::helper_methods::parse_cookie_value::parse_cookie_value,
    constants::AUTH_TOKEN_COOKIE_KEY, errors::saps::SapsError,
};

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
pub fn extract_token_from_cookies(headers: &HeaderMap) -> Result<Option<String>, SapsError> {
    let cookie_header = match headers.get(axum::http::header::COOKIE) {
        Some(cookies) => cookies,
        None => return Ok(None),
    };

    let cookies_str = cookie_header
        .to_str()
        .map_err(|_| SapsError::unauthorized("Invalid cookie format".to_string()))?;
    Ok(parse_cookie_value(cookies_str, AUTH_TOKEN_COOKIE_KEY))
}
