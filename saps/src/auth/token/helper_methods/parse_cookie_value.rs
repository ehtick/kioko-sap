//! Parses a single cookie value from a raw `Cookie` header string.

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
pub fn parse_cookie_value(cookies: &str, target_name: &str) -> Option<String> {
    cookies
        .split(';')
        .filter_map(|cookie| {
            let cookie = cookie.trim();
            cookie.split_once('=')
        })
        .find(|(name, _)| name.trim() == target_name)
        .map(|(_, value)| value.trim().to_string())
}
