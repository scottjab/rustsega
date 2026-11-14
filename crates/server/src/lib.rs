//! The HTTP half of the single binary: serve the built frontend out of memory
//! with the MIME types, compression, caching and security headers a browser
//! needs (M8-01, #155).
//!
//! Routing is a function of an [`AssetSource`], so the tests drive it with a
//! `HashMap` and neither trunk nor `crates/web/dist` is involved.

pub mod assets;
pub mod cli;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::assets::{Asset, AssetSource};

/// Versioned files - every name trunk gives a script, a stylesheet and the wasm
/// carries a content hash - are the same bytes forever, so a cache may hold one
/// for a year and will only refetch it when the name changes.
const LONG_CACHE: &str = "public, max-age=31536000, immutable";

/// The two files whose names trunk does not hash: `index.html` is what names all
/// the hashed ones, and `manifest.webmanifest` is fetched by the browser by its
/// fixed name. Both have to be revalidated before every use.
const NO_CACHE: &str = "no-cache";

/// The content type of `/api/info`.
const JSON: &str = "application/json";

/// The document policy of every response: same-origin for everything, with wasm
/// allowed to compile (`wasm-unsafe-eval`) and inline styles allowed because
/// Leptos writes `style=` attributes.
///
/// `script-src` ends in `'unsafe-inline'`, one step looser than M8-01 (#155)
/// asked for, and only for the single inline `<script type="module">` trunk
/// injects into `dist/index.html` to call the wasm loader. Without it the policy
/// is enforced exactly as written and the app never starts: in the dev shell's
/// Firefox 159, every subresource of the page loaded (`wasm-bindgen`'s `.js` and
/// the `.wasm` both 200, brotli-compressed) while the bootstrap did not run, so
/// `window.wasmBindings` stayed `undefined` and no canvas was ever mounted.
///
/// A per-file `'sha256-...'` of that script would be narrower and is the right
/// fix; it is not taken here because hashing it means `sha2` and `base64` as
/// direct dependencies, which #155 does not list.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'";

/// The CSP header name, which the `http` crate does not define for us.
const CONTENT_SECURITY_POLICY_HEADER: &str = "content-security-policy";

/// What every handler shares, behind an `Arc` because the router clones its state
/// for each request.
///
/// A router gets exactly one state type, so the frontend and the ROM library
/// travel together and a handler takes the half it needs.
#[derive(Clone)]
pub struct AppState {
    /// The frontend to serve: the embedded one, or the page that says it was
    /// built without one.
    pub assets: Arc<dyn AssetSource>,
    /// The `--rom-dir` of this process, already checked by [`cli::check_rom_dir`].
    ///
    /// `None` is no library, which is what `/api/info` reports as
    /// `"library": false`. M8-03 (#157) is the ticket that lists it.
    pub rom_dir: Option<PathBuf>,
}

impl AppState {
    /// State serving `assets` with no ROM library.
    pub fn new(assets: Arc<dyn AssetSource>) -> Self {
        Self {
            assets,
            rom_dir: None,
        }
    }
}

/// A router serving `state`: the health check, the info endpoint, one handler for
/// every other GET, then the headers every response carries, then compression,
/// then a trace of the request.
///
/// Layers are added innermost-first, so the security headers are on the response
/// before the compressor looks at it, and tracing sees the finished response.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/info", get(info))
        .fallback(get(serve))
        .with_state(state)
        .layer(SetResponseHeaderLayer::overriding(
            X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static(CONTENT_SECURITY_POLICY_HEADER),
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        // tower-http's default predicate compresses anything that is not an
        // image, gRPC or an event stream, and skips bodies under 32 bytes: both
        // the wasm and the stylesheet are worth it.
        .layer(CompressionLayer::new())
        .layer(TraceLayer::new_for_http())
}

/// Answer one request out of the asset tree.
async fn serve(State(state): State<Arc<AppState>>, request: Request) -> Response {
    match resolve(&state.assets, request.uri().path()) {
        Some(served) => asset_response(served),
        None => not_found(),
    }
}

/// `GET /healthz`: alive, in three bytes.
///
/// For the probe of a container supervisor or a load balancer. It reads nothing -
/// not the asset tree, not the ROM directory - because a health check that can
/// fail for a reason unrelated to serving makes the whole server look like the
/// thing that is broken.
async fn healthz() -> Response {
    (StatusCode::OK, "ok").into_response()
}

/// `GET /api/info`: what this binary is, for a status line in the frontend and
/// for whoever is deploying it to curl.
async fn info(State(state): State<Arc<AppState>>) -> Response {
    let body = info_body(env!("CARGO_PKG_VERSION"), state.rom_dir.is_some());
    (
        StatusCode::OK,
        [(CONTENT_TYPE, HeaderValue::from_static(JSON))],
        Body::from(body),
    )
        .into_response()
}

/// The `/api/info` body.
///
/// Written out rather than serialized: #156 allows one new dependency, `clap`,
/// and a document of a version string and a `bool` has no escaping to get wrong.
/// M8-03 (#157) is the ticket whose responses stop being two scalars; that one
/// should bring `serde` and answer with `axum::Json`.
fn info_body(version: &str, library: bool) -> String {
    format!("{{\"version\":\"{version}\",\"library\":{library}}}")
}

/// A file that is going to be served, and the key it was found under - the key
/// decides the cache header, so it travels with the bytes.
struct Served {
    key: String,
    asset: Asset,
}

/// Resolve a request path to an asset: the file itself, or `index.html` when the
/// path is a client-side route rather than a request for a file.
fn resolve(source: &Arc<dyn AssetSource>, path: &str) -> Option<Served> {
    let key = asset_key(path)?;
    if let Some(asset) = source.get(&key) {
        return Some(Served { key, asset });
    }
    if has_extension(&key) {
        // A missing file stays a 404. Answering it with the shell would hide a
        // broken URL behind a page that looks as if it had loaded.
        return None;
    }
    // SPA fallback: any other path is a route the frontend owns, so it gets the
    // app.
    let asset = source.get("index.html")?;
    Some(Served {
        key: "index.html".to_owned(),
        asset,
    })
}

/// The request path as an asset key: no leading `/`, and `index.html` for the
/// root.
///
/// Returns `None` when any component is `.` or `..`, so a request cannot name a
/// file outside the tree it is serving. Percent-decoding is deliberately not
/// done: a `%2f` cannot become a separator, so an encoded traversal attempt is
/// simply a file that does not exist. `rust-embed` guards its own dynamic mode
/// against traversal, but the guard belongs here, because an [`AssetSource`] is
/// allowed to resolve a key against a real directory.
fn asset_key(path: &str) -> Option<String> {
    let mut key = String::with_capacity(path.len());
    for part in path.split('/') {
        match part {
            "" => {}
            "." | ".." => return None,
            part => {
                if !key.is_empty() {
                    key.push('/');
                }
                key.push_str(part);
            }
        }
    }
    if key.is_empty() {
        key.push_str("index.html");
    }
    Some(key)
}

/// Whether the last component of `key` names a file with an extension.
///
/// A leading dot is a hidden file rather than an extension, so `robots.txt` is a
/// file request and `.well-known` is a route.
fn has_extension(key: &str) -> bool {
    let name = file_name(key);
    !name.starts_with('.') && name.contains('.')
}

fn file_name(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

/// Write an asset to a response: its MIME type, and the cache rule its name
/// implies.
fn asset_response(served: Served) -> Response {
    let cache = if uncached(&served.key) {
        NO_CACHE
    } else {
        LONG_CACHE
    };
    (
        [
            (CONTENT_TYPE, content_type(&served.asset.mime)),
            (CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        Body::from(served.asset.bytes),
    )
        .into_response()
}

/// Whether a file has to be revalidated rather than cached: the two names trunk
/// does not hash.
fn uncached(key: &str) -> bool {
    matches!(file_name(key), "index.html" | "manifest.webmanifest")
}

/// `mime` plus `; charset=utf-8` when it is a text type with no parameter of its
/// own.
///
/// `rust-embed` guesses the type from the extension and never adds a charset, and
/// a `text/html` without one is a page whose encoding the browser has to guess.
/// `application/wasm` is left exactly as it is: that is the string the loader
/// checks.
fn content_type(mime: &str) -> HeaderValue {
    let value = if mime.starts_with("text/") && !mime.contains(';') {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_owned()
    };
    HeaderValue::try_from(value)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

/// 404 with a one-line body. Nothing from the request is echoed back, so there is
/// nothing to reflect: the `AssetSource` either has the file or it does not.
fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}
