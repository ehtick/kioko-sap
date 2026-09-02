//! Refresh policies for the [`HeaderToken`] extractor.
//!
//! The fifth generic parameter on [`HeaderToken`] (`M`) selects how the
//! extractor treats the session row and the JWT's own expiry on every
//! request. Two policies ship with saps:
//!
//! | Policy | Session row | JWT `time_expire` | Rotation / `Set-Cookie` |
//! |--------|-------------|-------------------|-------------------------|
//! | [`AutoRefresh`] (default) | `saps.ping` — bumps `last_interacted`, deletes idle sessions, rotates the UUID after 5 minutes | **ignored** | yes, via [`attach_refreshed_cookie`] |
//! | [`NoRefresh`] | read-only `get_auth_session` — no bump, no idle deletion | enforced — past expiry is rejected with **403 Forbidden** | never |
//!
//! `AutoRefresh` is the classic cookie-session behaviour: the server keeps
//! the session alive for as long as the client keeps talking to it and
//! transparently re-issues the cookie when the UUID rotates.
//!
//! `NoRefresh` is the OAuth-style behaviour: the access token is valid for
//! exactly `TOKEN_EXPIRE_MINS` and then stops working. It is the client's
//! job to obtain a new token (typically via a refresh-token endpoint you
//! write); the extractor never mints one on the client's behalf.
//!
//! # Why the mode is a `const`
//!
//! The policy is surfaced as a plain [`RefreshMode`] value via
//! [`TokenRefreshPolicy::MODE`] and passed *by value* into
//! `run_auth_extraction`. That keeps the heavy JWT/DB code monomorphized once
//! per `(X, R, Z)` rather than once per `(X, R, Z, M)` — the same reason the
//! role-check strategy `Y` is kept out of that function.
//!
//! # Choosing a policy
//!
//! You normally don't spell `M` yourself. Use the aliases exported from
//! [`header_token`](crate::auth::token::header_token):
//!
//! - [`RefreshToken<X, Y, R, Z>`](crate::auth::token::header_token::RefreshToken)
//!   — identical to `HeaderToken<X, Y, R, Z>`.
//! - [`NonRefreshToken<X, Y, R, Z>`](crate::auth::token::header_token::NonRefreshToken)
//!   — `HeaderToken<X, Y, R, Z, NoRefresh>`.
//!
//! [`HeaderToken`]: crate::auth::token::header_token::HeaderToken
//! [`attach_refreshed_cookie`]: crate::auth::middleware::attach_refreshed_cookie

/// The runtime value of a [`TokenRefreshPolicy`].
///
/// Passed by value into `run_auth_extraction` so the JWT/DB code is not
/// monomorphized per policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    /// Ping `saps.ping`: bump `last_interacted`, delete idle sessions,
    /// rotate the UUID after 5 minutes and re-issue the cookie. The JWT's
    /// `time_expire` is **not** checked.
    Auto,
    /// Read-only `get_auth_session` lookup. The JWT's `time_expire` is
    /// enforced (past expiry → `403 Forbidden`). Never rotates, never
    /// touches `last_interacted`, never writes a cookie.
    Never,
}

/// Marker trait selecting the extractor's session-refresh behaviour.
///
/// Implemented by the unit structs [`AutoRefresh`] and [`NoRefresh`]. You
/// typically don't implement this yourself — pick one of the two markers (or
/// the `RefreshToken` / `NonRefreshToken` aliases that wrap them).
pub trait TokenRefreshPolicy: Send + Sync {
    /// The runtime mode this policy resolves to.
    const MODE: RefreshMode;
}

/// Default policy: today's cookie-session behaviour.
///
/// Every request pings the session row (extending it and rotating the UUID
/// after 5 minutes) and, on rotation, a refreshed `Set-Cookie` is handed to
/// the [`attach_refreshed_cookie`](crate::auth::middleware::attach_refreshed_cookie)
/// layer. The JWT's own `time_expire` claim is ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutoRefresh;

impl TokenRefreshPolicy for AutoRefresh {
    const MODE: RefreshMode = RefreshMode::Auto;
}

/// OAuth-style policy: no refresh, no rotation, expired JWT → `403`.
///
/// The session row is loaded read-only. If `Utc::now()` is past the JWT's
/// `time_expire` the request is rejected with `403 Forbidden` before the
/// database is consulted. Nothing is ever written back to the client.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoRefresh;

impl TokenRefreshPolicy for NoRefresh {
    const MODE: RefreshMode = RefreshMode::Never;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_refresh_resolves_to_auto_mode() {
        assert_eq!(AutoRefresh::MODE, RefreshMode::Auto);
    }

    #[test]
    fn no_refresh_resolves_to_never_mode() {
        assert_eq!(NoRefresh::MODE, RefreshMode::Never);
    }

    #[test]
    fn markers_are_send_sync_clone_debug() {
        fn assert_marker<T: Send + Sync + Clone + Copy + std::fmt::Debug + Default>() {}
        assert_marker::<AutoRefresh>();
        assert_marker::<NoRefresh>();
    }
}
