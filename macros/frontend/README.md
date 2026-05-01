# Frontend Macro

This macro bakes a frontend build directory into your server binary at compile time and wires it into an [axum](https://docs.rs/axum) [`Router`](https://docs.rs/axum/latest/axum/struct.Router.html) for you. It is published as part of the [`saps`](https://crates.io/crates/saps) framework and re-exported as `saps::mount_frontend!` — that's the path you'll usually import from.

Under the hood it uses [`rust-embed`](https://crates.io/crates/rust-embed) to walk the folder at compile time and embed every file as `&'static [u8]`, so the final binary serves the frontend without touching the filesystem at runtime.

# Usage

```rust
use saps::axum::{Router, response::IntoResponse, routing::get};
use saps::mount_frontend;

async fn health() -> impl IntoResponse { "OK" }

#[tokio::main]
async fn main() {
    // Mount real API routes BEFORE mount_frontend! — the macro registers
    // a fallback that catches everything else.
    let app = Router::new().route("/health", get(health));

    // Args: (folder path relative to your crate's Cargo.toml, the Router
    // binding to extend in place, max-age in seconds for static assets).
    mount_frontend!("frontend/web/public", app, 604800);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    saps::axum::serve(listener, app).await.unwrap();
}
```

The `app` argument names the `Router` binding in the surrounding scope. The macro rebinds it in place via `let app = app.route(...).fallback(...)`, so subsequent uses of `app` see the mounted frontend.

# What this expands to

Conceptually the expansion is two `Router` extensions plus a private submodule that holds the embedded assets:

```rust,ignore
mod saps_frontend_server {
    use saps::rust_embed::RustEmbed;

    #[derive(RustEmbed)]
    #[folder = "<absolute path resolved at compile time>"]
    struct FrontendAssets;

    pub async fn index() -> Response<Body> { /* serves index.html */ }
    pub async fn static_or_spa_fallback(uri: Uri) -> Response<Body> {
        /* see Routing below */
    }
}

let app = app.route("/", saps::axum::routing::get(saps_frontend_server::index));
let app = app.fallback(saps_frontend_server::static_or_spa_fallback);
```

The folder path is resolved against the calling crate's `CARGO_MANIFEST_DIR` (so `"frontend/web/public"` is interpreted relative to *your* `Cargo.toml`, not the macro's) and canonicalized at expansion time. A non-existent folder fails the build with a clear error pointing at the literal you passed.

# Routing

The fallback handler resolves every unmatched request through the following decision tree:

1. **`GET /api/...`** → `404`. The macro doesn't know your API routes, so it refuses to fall back to `index.html` for anything under `/api/`. Without this, a typo in an API URL would silently return the SPA shell with `200 OK`, and your client would parse HTML where it expected JSON. Mount your real API routes on `app` *before* `mount_frontend!` and they win against this rule.
2. **`GET /`** → `index.html`.
3. **`GET /<embedded asset path>`** → the asset, with the right `Content-Type` (`mime_guess` for everything except `.wasm`, which is forced to `application/wasm` so browsers will accept it via `WebAssembly.instantiateStreaming`).
4. **`GET /<file-shaped path that is not embedded>`** → `404`. "File-shaped" means the last segment contains a `.`. This catches stale hashed asset filenames (e.g. a client cached `/main-deadbeef.js` from before a deploy) — they get a real 404 instead of being silently fed the SPA shell.
5. **Anything else** (no extension, no embedded match) → `index.html`. This is the SPA fallback: client-side routes like `/users` or `/orders/42` resolve to your SPA shell so the router on the page can take over.

# Caching

Every embedded asset is served with a `Cache-Control` header. The strategy is:

- **`index.html`** is always `Cache-Control: no-cache`. It's the entry point and references hashed asset filenames that change every build, so caching it would freeze users on the previous deploy.
- **Every other asset** is served with `Cache-Control: public, max-age=<cache_seconds>`. Hashed asset filenames change on every build, so they're effectively immutable and safe to cache aggressively.

The third macro argument is the value substituted into `<cache_seconds>`. It must be a non-negative integer literal — `mount_frontend!("…", app, -1)` or a value that doesn't fit in a `u64` fails the build.

Common values:

```rust,ignore
mount_frontend!("dist", app, 604800);   // 7 days
mount_frontend!("dist", app, 31536000); // 1 year
mount_frontend!("dist", app, 0);        // effectively disable caching
```

# Notes

- The macro does not invoke your frontend build — the `dist`/`public`/whatever folder must already exist when `cargo build` runs. Wire your frontend build into a `build.rs` or an external script if you need it to run automatically.
- All assets are embedded into the binary, so binary size grows with frontend size. This is by design — the goal is a single self-contained executable. If you need a larger frontend served separately, `mount_frontend!` is not the right tool.
- The fallback handler is registered with `app.fallback(...)`. If you also call `app.fallback(...)` elsewhere, the last call wins. Apply `mount_frontend!` last if you want the SPA fallback to win.
