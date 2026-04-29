//! HTTP cookie helpers for the saps authentication token.
//!
//! The saps auth flow stores the issued JWT in a cookie named after
//! [`AUTH_TOKEN_COOKIE_KEY`](crate::constants::AUTH_TOKEN_COOKIE_KEY) (currently `"saps-token"`).
//! [`AuthTokenCookie`] is a zero-copy wrapper that borrows the token string and produces
//! the matching `Set-Cookie` header value, ready to be merged into an Axum response.
//!
//! # Cookie attributes
//!
//! Cookies emitted by this module use the following attributes:
//!
//! | Attribute | Value | Reason |
//! |-----------|-------|--------|
//! | `HttpOnly` | — | Prevents JavaScript from reading the token (XSS mitigation). |
//! | `Path` | `/` | Sent with every request to the same origin. |
//! | `Max-Age` | `86400` (issue) / `0` (wipe) | 24 hour lifetime, or immediate expiry on logout. |
//!
//! `Secure` and `SameSite` are **not** set by this module. If you need them, either
//! construct the `Set-Cookie` value yourself or extend [`AuthTokenCookie::yield_cookie_string`]
//! and [`AuthTokenCookie::wipe_from_cookies`].
//!
//! # Lifetime model
//!
//! [`AuthTokenCookie`] borrows the token rather than owning it, so the wrapper carries
//! a lifetime `'a` tied to the underlying string. Construct it via the [`From`] impls
//! for `&str` and `&String` — both produce an `AuthTokenCookie<'a>` whose lifetime is
//! bound to the input slice.
//!
//! # Typical use
//!
//! Issuing a fresh cookie on login:
//!
//! ```ignore
//! use saps::auth::token::cookies::AuthTokenCookie;
//!
//! let token = "eyJhbGciOi...";
//! let headers = AuthTokenCookie::from(token).generate_header()?;
//! // attach `headers` to your Axum response
//! ```
//!
//! Clearing the cookie on logout (the token value is irrelevant — `wipe_from_cookies`
//! emits an empty value with `Max-Age=0`):
//!
//! ```ignore
//! let headers = AuthTokenCookie::from("").wipe_from_cookies()?;
//! ```
//!
//! Merging into an existing header map (e.g. one built by middleware):
//!
//! ```ignore
//! let mut headers = HeaderMap::new();
//! AuthTokenCookie::from(token).add_to_headers(&mut headers)?;
//! ```

use axum::http::{HeaderMap, HeaderValue, header};
use crate::constants::AUTH_TOKEN_COOKIE_KEY;
use crate::errors::saps::SapsError;

/// A borrowed view over an auth token, used to produce the matching `Set-Cookie` header.
///
/// `AuthTokenCookie` does not allocate or copy the token — it holds a `&'a str` internally
/// and only formats the cookie string on demand. The lifetime `'a` is tied to whatever
/// string slice was passed in via the [`From`] impls.
///
/// Construct via:
///
/// - `AuthTokenCookie::from("token-string")` — borrows from a `&str`
/// - `AuthTokenCookie::from(&owned_string)` — borrows from a `&String`
///
/// See the [module-level documentation](self) for the cookie attribute set used.
pub struct AuthTokenCookie<'a> {
    /// The auth token to embed in the cookie value. Borrowed for the lifetime of the wrapper.
    token: &'a str
}


impl <'a>AuthTokenCookie<'a> {

    /// Builds the raw `Set-Cookie` value for the wrapped token.
    ///
    /// Format: `"{AUTH_TOKEN_COOKIE_KEY}={token}; HttpOnly; Path=/; Max-Age=86400"`
    ///
    /// This is the single source of truth for the issued cookie's attributes —
    /// any change to the cookie format (adding `Secure`, changing `Max-Age`, etc.)
    /// should be made here.
    pub(crate) fn yield_cookie_string(&self) -> String {
        format!(
            "{}={}; HttpOnly; Path=/; Max-Age=86400",
            AUTH_TOKEN_COOKIE_KEY, self.token
        )
    }

    /// Inserts the `Set-Cookie` header for this token into an existing [`HeaderMap`].
    ///
    /// Uses [`HeaderMap::insert`], which **replaces** any prior `Set-Cookie` value in
    /// the map. If you need to append (e.g. setting multiple cookies in one response),
    /// build the `HeaderValue` yourself via [`yield_cookie_string`](Self::yield_cookie_string)
    /// and call [`HeaderMap::append`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] if the formatted cookie string contains bytes that are not
    /// valid in an HTTP header value (control characters, NUL, etc.). For well-formed
    /// JWT tokens this should never occur in practice.
    pub fn add_to_headers(&self, headers: &mut HeaderMap) -> Result<(), SapsError> {
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&self.yield_cookie_string()).map_err(|e| SapsError::unknown(e.to_string()))?,
        );
        Ok(())
    }

    /// Builds a fresh [`HeaderMap`] containing only the `Set-Cookie` header for this token.
    ///
    /// Convenience wrapper around [`add_to_headers`](Self::add_to_headers) for the common
    /// case where the response carries no other headers from this layer.
    ///
    /// # Errors
    ///
    /// Same as [`add_to_headers`](Self::add_to_headers).
    pub fn generate_header(&self) -> Result<HeaderMap, SapsError> {
        let mut headers = HeaderMap::new();
        self.add_to_headers(&mut headers)?;
        Ok(headers)
    }

    /// Builds a [`HeaderMap`] that instructs the browser to delete the auth cookie.
    ///
    /// The emitted cookie has the same name and `Path` as the issued one but with an
    /// empty value and `Max-Age=0`, which is the standard mechanism for cookie deletion.
    /// The wrapped token value is **ignored** — you can call this on any
    /// `AuthTokenCookie`, including one built from an empty string.
    ///
    /// # Errors
    ///
    /// Returns [`SapsError`] only in the (effectively impossible) case that the static
    /// wipe string fails to parse as a [`HeaderValue`].
    pub fn wipe_from_cookies(&self) -> Result<HeaderMap, SapsError> {
        let cookie = format!("{AUTH_TOKEN_COOKIE_KEY}=; HttpOnly; Path=/; Max-Age=0");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(|e| SapsError::unknown(e.to_string()))?,
        );
        Ok(headers)
    }

}


/// Borrows from an owned `String` to produce an `AuthTokenCookie`.
///
/// The resulting wrapper's lifetime is bound to the borrow, so the source `String`
/// must outlive any use of the cookie.
impl<'a> From<&'a String> for AuthTokenCookie<'a> {

    fn from(value: &'a String) -> Self {
        Self { token: value.as_str() }
    }

}

/// Borrows from a `&str` to produce an `AuthTokenCookie`.
///
/// This is the most common entry point — pass the JWT slice directly.
impl<'a> From<&'a str> for AuthTokenCookie<'a> {

    fn from(value: &'a str) -> Self {
        Self { token: value }
    }

}

#[cfg(test)]
mod tests {

    use super::*;

    const TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.payload.signature";

    fn expected_set_cookie(token: &str) -> String {
        format!("{AUTH_TOKEN_COOKIE_KEY}={token}; HttpOnly; Path=/; Max-Age=86400")
    }

    #[test]
    fn from_str_borrows_token_through_to_cookie_string() {
        let cookie = AuthTokenCookie::from(TOKEN);
        assert_eq!(cookie.yield_cookie_string(), expected_set_cookie(TOKEN));
    }

    #[test]
    fn from_string_borrows_token_through_to_cookie_string() {
        let owned = TOKEN.to_string();
        let cookie = AuthTokenCookie::from(&owned);
        assert_eq!(cookie.yield_cookie_string(), expected_set_cookie(TOKEN));
    }

    #[test]
    fn yield_cookie_string_handles_empty_token() {
        let cookie = AuthTokenCookie::from("");
        assert_eq!(
            cookie.yield_cookie_string(),
            format!("{AUTH_TOKEN_COOKIE_KEY}=; HttpOnly; Path=/; Max-Age=86400"),
        );
    }

    #[test]
    fn add_to_headers_inserts_set_cookie() {
        let mut headers = HeaderMap::new();
        AuthTokenCookie::from(TOKEN)
            .add_to_headers(&mut headers)
            .expect("valid token should not error");

        let value = headers
            .get(header::SET_COOKIE)
            .expect("Set-Cookie header should be present")
            .to_str()
            .expect("header should be ascii");

        assert_eq!(value, expected_set_cookie(TOKEN));
    }

    #[test]
    fn add_to_headers_preserves_unrelated_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));

        AuthTokenCookie::from(TOKEN)
            .add_to_headers(&mut headers)
            .expect("valid token should not error");

        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/json",
        );
        assert!(headers.get(header::SET_COOKIE).is_some());
    }

    #[test]
    fn add_to_headers_replaces_prior_set_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SET_COOKIE,
            HeaderValue::from_static("other=value"),
        );

        AuthTokenCookie::from(TOKEN)
            .add_to_headers(&mut headers)
            .expect("valid token should not error");

        // `insert` replaces, so only the new cookie remains.
        let cookies: Vec<_> = headers.get_all(header::SET_COOKIE).iter().collect();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].to_str().unwrap(), expected_set_cookie(TOKEN));
    }

    #[test]
    fn add_to_headers_errors_on_token_with_invalid_header_bytes() {
        // Newline is not allowed in an HTTP header value.
        let bad_token = "abc\ndef";
        let mut headers = HeaderMap::new();
        let result = AuthTokenCookie::from(bad_token).add_to_headers(&mut headers);
        assert!(result.is_err());
        assert!(headers.get(header::SET_COOKIE).is_none());
    }

    #[test]
    fn generate_header_returns_fresh_map_with_only_cookie() {
        let headers = AuthTokenCookie::from(TOKEN)
            .generate_header()
            .expect("valid token should not error");

        assert_eq!(headers.len(), 1);
        assert_eq!(
            headers.get(header::SET_COOKIE).unwrap().to_str().unwrap(),
            expected_set_cookie(TOKEN),
        );
    }

    #[test]
    fn wipe_from_cookies_emits_empty_value_with_max_age_zero() {
        let headers = AuthTokenCookie::from(TOKEN)
            .wipe_from_cookies()
            .expect("static wipe string should always parse");

        let value = headers
            .get(header::SET_COOKIE)
            .expect("Set-Cookie header should be present")
            .to_str()
            .unwrap();

        assert_eq!(
            value,
            format!("{AUTH_TOKEN_COOKIE_KEY}=; HttpOnly; Path=/; Max-Age=0"),
        );
    }

    #[test]
    fn wipe_from_cookies_ignores_wrapped_token() {
        let from_real = AuthTokenCookie::from(TOKEN).wipe_from_cookies().unwrap();
        let from_empty = AuthTokenCookie::from("").wipe_from_cookies().unwrap();

        assert_eq!(
            from_real.get(header::SET_COOKIE).unwrap(),
            from_empty.get(header::SET_COOKIE).unwrap(),
        );
    }
}
