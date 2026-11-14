//! The router, driven from memory (M8-01, #155).
//!
//! Everything here goes through [`server::router`] with an in-memory
//! [`AssetSource`], so the routing, MIME and cache rules are tested without
//! trunk, without `crates/web/dist`, and without opening a socket.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderName, Request, Response, StatusCode};
use http_body_util::BodyExt;
use server::AppState;
use server::assets::{Asset, AssetSource};
use tower::ServiceExt;

/// The page trunk builds, in miniature.
const INDEX: &[u8] = b"<!doctype html><title>rustsega</title>";
/// The stylesheet name trunk would hash into the URL. Over 32 bytes because
/// tower-http does not compress anything smaller.
const CSS: &[u8] = b"body{margin:0;background:#0b0d12;color:#e6e6e6;padding:0}";

/// A wasm module header, padded past the 32 byte floor tower-http will not
/// compress under (`SizeAbove::DEFAULT_MIN_SIZE`).
fn wasm_bytes() -> Vec<u8> {
    let mut bytes = vec![0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
    bytes.resize(64, 0);
    bytes
}

fn asset(bytes: impl Into<Cow<'static, [u8]>>, mime: &str) -> Asset {
    Asset {
        bytes: bytes.into(),
        mime: mime.to_owned(),
    }
}

/// An [`AssetSource`] over a `HashMap`, standing in for `crates/web/dist`.
struct MemoryAssets(HashMap<&'static str, Asset>);

impl AssetSource for MemoryAssets {
    fn get(&self, path: &str) -> Option<Asset> {
        self.0.get(path).cloned()
    }
}

/// The frontend behind the router state of #156, with no ROM directory: the
/// asset tree is all this file asks the router about.
fn state() -> Arc<AppState> {
    Arc::new(AppState::new(frontend()))
}

/// `index.html` and two content-hashed assets, the way trunk names them.
fn frontend() -> Arc<dyn AssetSource> {
    Arc::new(MemoryAssets(HashMap::from([
        ("index.html", asset(Cow::Borrowed(INDEX), "text/html")),
        (
            "app-abc123.wasm",
            asset(Cow::Owned(wasm_bytes()), "application/wasm"),
        ),
        ("style-def456.css", asset(Cow::Borrowed(CSS), "text/css")),
    ])))
}

async fn request(uri: &str, headers: &[(HeaderName, &str)]) -> Response<Body> {
    let mut builder = Request::get(uri);
    for (name, value) in headers {
        builder = builder.header(name.clone(), *value);
    }
    server::router(state())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .expect("the router serves every request without failing")
}

async fn get(uri: &str) -> Response<Body> {
    request(uri, &[]).await
}

/// The full body, drained off a response.
async fn body(response: Response<Body>) -> Vec<u8> {
    response.collect().await.unwrap().to_bytes().to_vec()
}

/// A header as a string, empty when it is absent.
fn header(response: &Response<Body>, name: HeaderName) -> String {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn root_serves_index_html_and_must_not_be_cached() {
    let response = get("/").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, CONTENT_TYPE), "text/html; charset=utf-8");
    assert_eq!(header(&response, CACHE_CONTROL), "no-cache");
    assert_eq!(body(response).await, INDEX);
}

#[tokio::test]
async fn hashed_wasm_is_typed_and_cached_forever() {
    let response = get("/app-abc123.wasm").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, CONTENT_TYPE), "application/wasm");
    assert_eq!(
        header(&response, CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(body(response).await, wasm_bytes());
}

#[tokio::test]
async fn unknown_route_without_an_extension_falls_back_to_index() {
    let response = get("/settings/buttons").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header(&response, CONTENT_TYPE), "text/html; charset=utf-8");
    // The fallback *is* index.html, so it inherits its cache rule: serving a
    // stale shell is how the app never updates.
    assert_eq!(header(&response, CACHE_CONTROL), "no-cache");
    assert_eq!(body(response).await, INDEX);
}

#[tokio::test]
async fn unknown_file_with_an_extension_is_404() {
    let response = get("/missing-9a8b7c.css").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn wasm_and_css_are_gzipped_when_the_client_accepts_it() {
    for uri in ["/app-abc123.wasm", "/style-def456.css"] {
        let response = request(uri, &[(ACCEPT_ENCODING, "gzip")]).await;

        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(header(&response, CONTENT_ENCODING), "gzip", "{uri}");
        assert!(
            !body(response).await.is_empty(),
            "{uri}: a compressed body must still arrive"
        );
    }
}

#[tokio::test]
async fn dot_dot_in_a_path_never_escapes_the_asset_tree() {
    // Only the first of these is the ticket's case; the others are the same
    // claim once a client has tried to be clever about it.
    for uri in [
        "/../Cargo.toml",
        "/./../Cargo.toml",
        "/assets/../../Cargo.toml",
    ] {
        let response = get(uri).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        let bytes = body(response).await;
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("rust-embed"),
            "{uri} returned a real file: {text}"
        );
    }
}

#[tokio::test]
async fn every_response_carries_the_security_headers() {
    // Pinned verbatim, including the `'unsafe-inline'` on `script-src` that the
    // inline bootstrap script of trunk needs: see CONTENT_SECURITY_POLICY.
    let csp = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline'; \
               style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
               connect-src 'self'";

    for uri in ["/", "/missing-9a8b7c.css"] {
        let response = get(uri).await;

        assert_eq!(
            header(&response, X_CONTENT_TYPE_OPTIONS),
            "nosniff",
            "{uri}"
        );
        assert_eq!(header(&response, REFERRER_POLICY), "no-referrer", "{uri}");
        assert_eq!(
            header(
                &response,
                HeaderName::from_static("content-security-policy")
            ),
            csp,
            "{uri}"
        );
    }
}
