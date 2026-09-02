//! Tower/axum middleware for the saps auth flow.
//!
//! This module hosts the response-side layers that pair with the
//! [`HeaderToken`](crate::auth::token::header_token::HeaderToken) extractor.
//!
//! # Why a layer is needed
//!
//! When `HeaderToken::from_request_parts` detects that the database has rotated
//! the session UUID (because `date_created` was older than 5 minutes — see the
//! `saps.ping` stored procedure in [`AuthSession::generate_migration_sql`](
//! crate::auth::dal::model::AuthSession::generate_migration_sql)), it needs to
//! send the new JWT back to the client as a `Set-Cookie` header. The extractor
//! itself only sees the request, so a response-side layer has to do the actual
//! attachment — that's what [`attach_refreshed_cookie`] does.
//!
//! # How the signal flows
//!
//! Tower middleware in axum can't read request-extension mutations that happen
//! *inside* `next.run(req)` — `next.run` consumes the request and the layer
//! never sees it again. To get a signal back from the extractor, this layer
//! installs a shared [`CookieSlot`] in the request extensions *before* calling
//! `next.run`. The extractor finds that slot and writes the new cookie value
//! into it via the slot's interior mutability. After the handler returns the
//! layer reads the slot and, if populated, attaches the cookie to the response.
//!
//! # Usage
//!
//! Apply the layer at the router level wherever `HeaderToken` is used:
//!
//! ```ignore
//! use saps::auth::middleware::attach_refreshed_cookie;
//! use axum::{Router, middleware::from_fn, routing::get};
//!
//! let app = Router::new()
//!     .route("/me", get(my_handler))
//!     .layer(from_fn(attach_refreshed_cookie));
//! ```
//!
//! The layer is a no-op for requests where no rotation occurred, so it is safe
//! to apply broadly — even on routes that don't use `HeaderToken` at all.
//! Only tokens using the default
//! [`AutoRefresh`](crate::auth::token::refresh_policy::AutoRefresh) policy
//! ever write to the slot; a
//! [`NonRefreshToken`](crate::auth::token::header_token::NonRefreshToken)
//! never rotates, so the layer is always a no-op for those.

use crate::auth::auth_trace;
use crate::auth::token::header_token::UpdatedAuthCookie;
use axum::{extract::Request, http::header, middleware::Next, response::Response};
use std::sync::{Arc, Mutex};

/// Shared slot installed by [`attach_refreshed_cookie`] into the request
/// extensions so the [`HeaderToken`](crate::auth::token::header_token::HeaderToken)
/// extractor can hand a refreshed cookie value back to the response-side layer.
///
/// The slot is `Arc<Mutex<Option<UpdatedAuthCookie>>>` so both the layer (after
/// `next.run`) and the extractor (during `from_request_parts`) share the same
/// allocation: the extractor writes the cookie into the `Mutex`, the layer
/// reads it afterwards.
#[derive(Clone, Default)]
pub struct CookieSlot(Arc<Mutex<Option<UpdatedAuthCookie>>>);

impl CookieSlot {
    /// Stores a refreshed cookie value in the slot. Called by the extractor
    /// when it detects a session rotation. Silently no-ops if the mutex is
    /// poisoned (which would mean another thread panicked while holding it —
    /// an unrecoverable state we don't want to escalate from a request handler).
    pub fn set(&self, cookie: UpdatedAuthCookie) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = Some(cookie);
        } else {
            // No request context available here — the caller in
            // `from_request_parts` already logs the rotation with session_id
            // before this point, so a session_id-less warning is sufficient.
            auth_trace!("CookieSlot::set — mutex poisoned, dropping cookie");
        }
    }

    /// Removes and returns the stored cookie value, if any. Called by the
    /// layer after the handler finishes.
    fn take(&self) -> Option<UpdatedAuthCookie> {
        self.0.lock().ok().and_then(|mut guard| guard.take())
    }
}

/// Response-side layer that attaches the refreshed `Set-Cookie` header when the
/// extractor rotated the session UUID.
///
/// Inserts a fresh [`CookieSlot`] into the request extensions before forwarding
/// the request. After the handler returns, takes whatever the extractor wrote
/// into the slot and — if present — parses it as a [`HeaderValue`](axum::http::HeaderValue)
/// and inserts it as `Set-Cookie` on the response.
///
/// If no rotation occurred (slot empty), the response is returned unchanged. If
/// the cookie string fails to parse as a valid header value (effectively
/// impossible — the extractor builds the string itself), the response is also
/// returned unchanged rather than failing the request.
pub async fn attach_refreshed_cookie(mut req: Request, next: Next) -> Response {
    let slot = CookieSlot::default();
    // Capture method+uri up-front so every trace from this layer carries the
    // same identifying context, even after `req` is consumed by `next.run`.
    #[cfg(feature = "auth-tracing")]
    let method = req.method().clone();
    #[cfg(feature = "auth-tracing")]
    let uri = req.uri().clone();
    auth_trace!(
        method = %method,
        uri = %uri,
        "attach_refreshed_cookie — installing CookieSlot",
    );
    req.extensions_mut().insert(slot.clone());

    let mut response = next.run(req).await;

    match slot.take() {
        Some(UpdatedAuthCookie(cookie)) => match cookie.parse() {
            Ok(value) => {
                auth_trace!(
                    method = %method,
                    uri = %uri,
                    cookie = %cookie,
                    status = %response.status(),
                    "attach_refreshed_cookie — attaching Set-Cookie (rotation occurred)",
                );
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            Err(_) => {
                auth_trace!(
                    method = %method,
                    uri = %uri,
                    cookie = %cookie,
                    "attach_refreshed_cookie — refreshed cookie failed to parse as HeaderValue, leaving response unchanged",
                );
            }
        },
        None => {
            auth_trace!(
                method = %method,
                uri = %uri,
                status = %response.status(),
                "attach_refreshed_cookie — no rotation, response unchanged",
            );
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::Request as ExtractRequest,
        http::{Request, StatusCode},
        middleware::from_fn,
        response::IntoResponse,
        routing::get,
    };
    use tower::ServiceExt;

    async fn ok_handler() -> &'static str {
        "ok"
    }

    /// Write to the slot installed by `attach_refreshed_cookie` to simulate
    /// what the `HeaderToken` extractor would do during a rotation.
    async fn inject_updated_cookie(req: ExtractRequest, next: Next) -> Response {
        if let Some(slot) = req.extensions().get::<CookieSlot>() {
            slot.set(UpdatedAuthCookie(
                "saps-token=newjwt; HttpOnly; Path=/; Max-Age=86400".to_string(),
            ));
        }
        next.run(req).await
    }

    #[tokio::test]
    async fn attaches_set_cookie_when_extension_present() {
        // Layer order matters: `attach_refreshed_cookie` is added LAST so it
        // becomes the OUTERMOST layer — it installs the slot before
        // `inject_updated_cookie` (an inner layer) tries to read it.
        let app = Router::new()
            .route("/", get(ok_handler))
            .layer(from_fn(inject_updated_cookie))
            .layer(from_fn(attach_refreshed_cookie));

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::SET_COOKIE).unwrap(),
            "saps-token=newjwt; HttpOnly; Path=/; Max-Age=86400",
        );
    }

    #[tokio::test]
    async fn no_set_cookie_when_extension_absent() {
        let app = Router::new()
            .route("/", get(ok_handler))
            .layer(from_fn(attach_refreshed_cookie));

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(header::SET_COOKIE).is_none());
    }

    #[tokio::test]
    async fn replaces_existing_set_cookie_from_handler() {
        async fn handler_with_cookie() -> Response {
            let mut resp = "ok".into_response();
            resp.headers_mut()
                .insert(header::SET_COOKIE, "other=value".parse().unwrap());
            resp
        }

        let app = Router::new()
            .route("/", get(handler_with_cookie))
            .layer(from_fn(inject_updated_cookie))
            .layer(from_fn(attach_refreshed_cookie));

        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Layer uses `insert`, so the refreshed cookie replaces the handler's.
        let cookies: Vec<_> = resp.headers().get_all(header::SET_COOKIE).iter().collect();
        assert_eq!(cookies.len(), 1);
        assert_eq!(
            cookies[0],
            "saps-token=newjwt; HttpOnly; Path=/; Max-Age=86400",
        );
    }
}
