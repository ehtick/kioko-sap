//! Procedural macro that bakes a frontend build directory into the server
//! binary and wires it into an axum [`Router`](saps::axum::Router) at compile time.
//!
//! # What it does
//!
//! `mount_frontend!("path/to/dist", app, cache_seconds)` expands to:
//!
//! 1. A nested `mod saps_frontend_server` containing a [`RustEmbed`] struct
//!    whose `#[folder]` points at `path/to/dist`. RustEmbed walks that folder
//!    at compile time and embeds every file as a `&'static [u8]`, so the
//!    final binary is fully self-contained — no runtime filesystem access for
//!    serving the frontend.
//! 2. Two axum handlers:
//!    - `index()` returns the embedded `index.html`.
//!    - `static_or_spa_fallback()` serves any embedded asset by URL path, or
//!      falls back to `index.html` for paths that look like SPA routes.
//! 3. A pair of `let #app = #app.route(...)` rebindings that mount those
//!    handlers onto the caller's router. The `app` argument names the
//!    `Router` binding in the caller's scope; we rebind it in place so the
//!    macro reads like an extension method.
//!
//! # Why a proc macro?
//!
//! [`RustEmbed`]'s `#[folder]` attribute requires a string literal at the
//! site of the derive — it can't be passed through a generic function
//! parameter or a const. To let consumers pick the folder per call site, we
//! generate a fresh `RustEmbed` struct inside the macro expansion. The same
//! constraint is why the path is resolved at expansion time rather than at
//! runtime.
//!
//! # Caching
//!
//! Every embedded asset other than `index.html` is served with
//! `Cache-Control: public, max-age=<cache_seconds>`. `index.html` itself is
//! always `Cache-Control: no-cache` so that a deploy with new hashed asset
//! filenames is picked up by browsers immediately. The hashed assets it
//! references can stay in the cache for `cache_seconds` because their
//! filenames change on every build.
//!
//! # Example
//!
//! ```ignore
//! use saps::axum::{Router, response::IntoResponse, routing::get};
//! use saps::mount_frontend;
//!
//! async fn health() -> impl IntoResponse { "OK" }
//!
//! #[tokio::main]
//! async fn main() {
//!     let app = Router::new().route("/health", get(health));
//!
//!     // 604_800 = 7 days. Pass any non-negative integer literal you want.
//!     mount_frontend!("frontend/web/public", app, 604800);
//!
//!     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
//!     saps::axum::serve(listener, app).await.unwrap();
//! }
//! ```
//!
//! # Routing semantics
//!
//! - `GET /` → `index.html` (embedded).
//! - `GET /<path>` where `<path>` matches an embedded asset → that asset
//!   with the correct MIME type.
//! - `GET /api/...` → 404. The macro doesn't know your API routes, so it
//!   refuses to fall back to `index.html` for them; otherwise an unmatched
//!   API route would silently return the SPA shell with a 200 and your
//!   client would parse HTML where it expected JSON. Mount your real API
//!   routes on `app` *before* `mount_frontend!` and they'll match before
//!   the fallback runs.
//! - `GET /<path>` where `<path>` looks like a file (has a `.` in the last
//!   segment) and is not embedded → 404.
//! - `GET /<path>` with no extension and no embedded match → `index.html`.
//!   This is the SPA fallback so client-side routes (`/users`, `/orders/42`)
//!   resolve to the SPA shell.

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Ident, LitInt, LitStr, Result, Token, parse::Parse, parse::ParseStream, parse_macro_input,
};

/// Parsed form of the macro arguments: `("path", app_ident, cache_seconds)`.
///
/// - `path` is a string literal so we can resolve it against the calling
///   crate's `CARGO_MANIFEST_DIR` at expansion time.
/// - `app` is an [`Ident`] (not [`syn::Expr`]) because the generated code
///   uses it as the LHS of `let #app = ...`. `let` patterns only accept
///   identifiers, so allowing arbitrary expressions would silently produce
///   broken expansions.
/// - `cache_seconds` is a [`LitInt`] so we can validate it parses as a
///   non-negative `u64` and inline it into the `Cache-Control` header value
///   at expansion time.
struct MountFrontendArgs {
    path: LitStr,
    app: Ident,
    cache_seconds: LitInt,
}

impl Parse for MountFrontendArgs {
    /// Hand-rolled parse rather than `Punctuated` because the three fields
    /// have different syn types — we want each one type-checked at the
    /// lexer level so a wrong-shape arg points at the offending token
    /// rather than failing later inside `quote!`.
    fn parse(input: ParseStream) -> Result<Self> {
        let path: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let app: Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let cache_seconds: LitInt = input.parse()?;
        Ok(Self { path, app, cache_seconds })
    }
}

/// `mount_frontend!(path, app, cache_seconds)` — see the module-level docs
/// for the full contract.
#[proc_macro]
pub fn mount_frontend(input: TokenStream) -> TokenStream {
    let MountFrontendArgs { path, app, cache_seconds } =
        parse_macro_input!(input as MountFrontendArgs);

    // ── Validate `cache_seconds` and pre-format the Cache-Control value ──
    //
    // `LitInt` accepts any integer literal token (including hex, octal, and
    // negative literals via leading `-`). We force-parse it as a `u64` here
    // so something like `mount_frontend!("…", app, -1)` or `0xfffffffffffff
    // ffff` fails with a clean error pointing at the literal, rather than
    // expanding into surprising HTTP headers.
    //
    // Pre-formatting the header value here means the expansion gets a plain
    // `&'static str` literal and the consuming crate doesn't have to ship
    // any formatting code at runtime.
    let cache_value = cache_seconds.base10_parse::<u64>().unwrap_or_else(|e| {
        panic!(
            "mount_frontend!: cache_seconds must be a non-negative integer literal: {e}",
        )
    });
    let cache_header_value = format!("public, max-age={cache_value}");
    let cache_lit = LitStr::new(&cache_header_value, cache_seconds.span());

    // ── Resolve the asset folder to an absolute path ──
    //
    // RustEmbed's `#[folder = "..."]` attribute is interpreted relative to
    // the proc-macro crate's manifest dir, which is *this* crate, not the
    // caller's. To make `mount_frontend!("frontend/web/public", ...)` mean
    // "relative to the caller's Cargo.toml", we read the caller's
    // `CARGO_MANIFEST_DIR` (which Cargo sets per crate at build time) and
    // canonicalize the join. The resulting absolute path is then passed
    // verbatim to RustEmbed.
    //
    // We canonicalize so the macro fails fast at expansion time if the
    // folder doesn't exist — RustEmbed's own error for a missing folder is
    // less helpful and points at generated code.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let resolved = std::path::Path::new(&manifest_dir)
        .join(path.value())
        .canonicalize()
        .unwrap_or_else(|e| {
            panic!(
                "mount_frontend!: path '{}' resolved to '{}' which does not exist: {}",
                path.value(),
                std::path::Path::new(&manifest_dir).join(path.value()).display(),
                e,
            )
        });
    let resolved_str = resolved.to_string_lossy().to_string();
    let resolved_lit = LitStr::new(&resolved_str, path.span());

    // ── Generated code ──
    //
    // Everything below runs in the *caller's* crate. The expansion lives in
    // a private `saps_frontend_server` submodule so it doesn't pollute the
    // caller's namespace, and so we can keep helper functions private. The
    // module is non-pub: only `index` and `static_or_spa_fallback` (the
    // route handlers) are pub, because we need to reference them in the
    // route registrations at the bottom.
    let expanded = quote! {
        mod saps_frontend_server {
            // RustEmbed's derive emits code referencing `::rust_embed::…`
            // directly, so it requires a crate visible under that exact
            // name. We re-export it via saps and alias it locally so
            // consumers don't need to add `rust_embed` as a direct dep.
            use saps::rust_embed as rust_embed;

            use saps::axum::{
                body::Body,
                http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
                response::Response,
            };
            use saps::mime_guess::MimeGuess;
            use rust_embed::{Embed, RustEmbed};
            use std::borrow::Cow;

            // RustEmbed walks `#resolved_lit` at compile time and emits a
            // `FrontendAssets::get(path)` associated fn that returns an
            // `Option<EmbeddedFile>` whose `.data` is a `Cow<'static, [u8]>`.
            // The `&'static` borrow case is when the bytes are linked
            // verbatim (release builds); `Cow::Owned` happens for assets
            // RustEmbed has to materialize at runtime (debug builds with
            // the `debug-embed` feature off, etc).
            #[derive(RustEmbed)]
            #[folder = #resolved_lit]
            struct FrontendAssets;

            /// Look `path` up in the embedded asset bundle and turn the
            /// hit into an HTTP response with the right `Content-Type`
            /// and `Cache-Control` headers. Returns `None` when the asset
            /// isn't present so the caller can decide how to fall back.
            fn embedded_file_response(path: &str) -> Option<Response<Body>> {
                let asset = FrontendAssets::get(path)?;

                // Move the bytes into an axum `Body`. `Cow::Borrowed`
                // hits the static-bytes path; `Cow::Owned` hands its
                // backing `Vec<u8>` straight to `Body::from`.
                let body = match asset.data {
                    Cow::Borrowed(bytes) => Body::from(bytes.to_vec()),
                    Cow::Owned(bytes) => Body::from(bytes),
                };

                // `mime_guess` covers most extensions but emits the
                // wrong type for `.wasm` on some platforms — wasm-pack
                // and friends require `application/wasm` exactly or the
                // browser refuses to instantiate the module via
                // `WebAssembly.instantiateStreaming`.
                let mime = if path.ends_with(".wasm") {
                    "application/wasm".to_string()
                } else {
                    MimeGuess::from_path(path).first_or_octet_stream().to_string()
                };

                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime).ok()?);

                // `index.html` MUST NOT be cached — it's the entry point
                // and references hashed asset filenames that change every
                // build. Caching it would freeze users on the previous
                // deploy. The hashed assets themselves are immutable, so
                // we cache them aggressively for `cache_seconds`.
                let cache = if path == "index.html" { "no-cache" } else { #cache_lit };
                headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));

                let mut resp = Response::builder()
                    .status(StatusCode::OK)
                    .body(body)
                    .unwrap();
                *resp.headers_mut() = headers;
                Some(resp)
            }

            /// Serve `/` as `index.html` from the embedded assets.
            pub async fn index() -> Response<Body> {
                // 500 rather than 404 — if `index.html` is missing the
                // build is broken, not the request.
                embedded_file_response("index.html").unwrap_or_else(|| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body("index.html missing from embedded assets".into())
                        .unwrap()
                })
            }

            /// Axum fallback handler. Decides between four cases by URL
            /// shape:
            ///
            /// 1. `/api/...`  → 404. API routes were supposed to match
            ///    earlier. Returning the SPA shell here would make
            ///    every typo in an API URL look like a working JSON
            ///    endpoint that returns HTML.
            /// 2. empty path  → `index.html`.
            /// 3. embedded match → serve the asset.
            /// 4. file-shaped (has `.`) but no embedded match → 404.
            ///    Don't fall back to the SPA, because clients fetching
            ///    `/main-deadbeef.js` after the deploy hash changes
            ///    would otherwise receive the SPA shell with `200 OK`
            ///    and a `text/html` content type, which JS engines
            ///    mis-parse and silently break.
            /// 5. anything else → `index.html`. This is the SPA route
            ///    fallback (`/users`, `/orders/42`, etc).
            pub async fn static_or_spa_fallback(uri: Uri) -> Response<Body> {
                let path = uri.path().trim_start_matches('/');

                if uri.path().starts_with("/api/") {
                    return Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("Not Found".into())
                        .unwrap();
                }

                if path.is_empty() {
                    return index().await;
                }

                if let Some(resp) = embedded_file_response(path) {
                    return resp;
                }

                // Heuristic: anything with a `.` in its last segment is
                // a file the client expected to fetch. Don't SPA-fall-back
                // for these — see case 4 in the docstring above.
                let looks_like_file = path.rsplit_once('.').is_some();
                if looks_like_file {
                    return Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("404 Not Found".into())
                        .unwrap();
                }

                // SPA fallback for client-side route segments.
                index().await
            }
        }

        // Mount the two handlers on the caller's `app` Router. Rebinding
        // via `let #app = #app...` keeps the macro's effect contained to
        // the surrounding scope and lets the caller treat `mount_frontend!`
        // like a fluent extension. Order matters: `route("/")` adds the
        // explicit GET-/ handler, and `fallback(...)` registers the
        // catch-all that handles every other URL. Real API routes mounted
        // BEFORE `mount_frontend!` win against the fallback.
        let #app = #app.route("/", saps::axum::routing::get(saps_frontend_server::index));
        let #app = #app.fallback(saps_frontend_server::static_or_spa_fallback);
    };
    TokenStream::from(expanded)
}
