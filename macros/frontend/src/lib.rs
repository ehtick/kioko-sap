extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{Expr, LitStr, Result, Token, parse::Parse, parse::ParseStream, parse_macro_input};

struct MountFrontendArgs {
    path: LitStr,
    app: Expr,
}

impl Parse for MountFrontendArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let path: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let app: Expr = input.parse()?;
        Ok(Self { path, app })
    }
}

#[proc_macro]
pub fn mount_frontend(input: TokenStream) -> TokenStream {
    let MountFrontendArgs { path, app } = parse_macro_input!(input as MountFrontendArgs);

    // Resolve the path relative to the calling crate's CARGO_MANIFEST_DIR so
    // that RustEmbed's #[folder] gets an absolute path that exists at compile time.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let resolved = std::path::Path::new(&manifest_dir)
        .join(path.value())
        .canonicalize()
        .unwrap_or_else(|e| {
            panic!(
                "mount_frontend!: path '{}' resolved to '{}' which does not exist: {}",
                path.value(),
                std::path::Path::new(&manifest_dir)
                    .join(path.value())
                    .display(),
                e,
            )
        });
    let resolved_str = resolved.to_string_lossy().to_string();
    let resolved_lit = LitStr::new(&resolved_str, path.span());

    let expanded = quote! {
        mod saps_frontend_server {
            // The RustEmbed derive macro generates code referencing `rust_embed::` directly,
            // so we alias saps's re-export as the crate name the derive expects.
            use saps::rust_embed as rust_embed;

            use saps::axum::{
                body::Body,
                http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
                response::Response,
            };
            use saps::mime_guess::MimeGuess;
            use rust_embed::{Embed, RustEmbed};
            use std::borrow::Cow;

            #[derive(RustEmbed)]
            #[folder = #resolved_lit]
            struct FrontendAssets;

            fn embedded_file_response(path: &str) -> Option<Response<Body>> {
                let asset = FrontendAssets::get(path)?;

                let body = match asset.data {
                    Cow::Borrowed(bytes) => Body::from(bytes.to_vec()),
                    Cow::Owned(bytes) => Body::from(bytes),
                };

                let mime = if path.ends_with(".wasm") {
                    "application/wasm".to_string()
                } else {
                    MimeGuess::from_path(path).first_or_octet_stream().to_string()
                };

                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&mime).ok()?);

                let cache = if path == "index.html" { "no-cache" } else { "public, max-age=604800" };
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
                embedded_file_response("index.html").unwrap_or_else(|| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body("index.html missing from embedded assets".into())
                        .unwrap()
                })
            }

            /// Serve any static file by path, with SPA fallback.
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

                let looks_like_file = path.rsplit_once('.').is_some();
                if looks_like_file {
                    return Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body("404 Not Found".into())
                        .unwrap();
                }

                index().await
            }
        }

        let #app = #app.route("/", saps::axum::routing::get(saps_frontend_server::index));
        let #app = #app.fallback(saps_frontend_server::static_or_spa_fallback);
    };
    TokenStream::from(expanded)
}
