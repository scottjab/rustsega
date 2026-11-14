//! The two endpoints that are not the frontend (M8-02, #156).
//!
//! `crates/server/tests/static_files.rs` owns the frontend half. These are the
//! routes an operator or a supervisor curls rather than a browser, and they are
//! answered from [`AppState`] instead of from the asset tree - which is why they
//! are here and not in that file.
//!
//! The assets themselves are the placeholder source, so neither trunk nor
//! `crates/web/dist` is needed: what is under test is that `/healthz` and
//! `/api/info` are routed to their own handlers rather than to the SPA fallback,
//! and that they answer from the [`AppState`] they were given. How that state gets
//! filled in from the command line and the environment is a started process's
//! business, and it is `crates/server/tests/running_binary.rs`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use server::AppState;
use server::assets::PlaceholderAssets;
use server::library::LibrarySlot;
use tower::ServiceExt;

/// State with no ROM library: what `rustsega` is with no `--rom-dir`.
fn state() -> Arc<AppState> {
    Arc::new(AppState::new(Arc::new(PlaceholderAssets)))
}

async fn get(state: Arc<AppState>, uri: &str) -> Response<Body> {
    request(state, Method::GET, uri).await
}

async fn request(state: Arc<AppState>, method: Method, uri: &str) -> Response<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    server::router(state)
        .oneshot(request)
        .await
        .expect("the router answers without failing")
}

async fn body(response: Response<Body>) -> String {
    let bytes = response.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).expect("both endpoints answer in ASCII")
}

#[tokio::test]
async fn healthz_answers_ok() {
    let response = get(state(), "/healthz").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "text/plain; charset=utf-8"
    );
    assert_eq!(body(response).await, "ok");
}

#[tokio::test]
async fn api_info_reports_the_binary_version_and_no_library() {
    let response = get(state(), "/api/info").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        body(response).await,
        format!(
            "{{\"version\":\"{}\",\"library\":false}}",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[tokio::test]
async fn api_info_reports_a_library_when_one_was_asked_for() {
    // Whether there is a library is M8-02's business; the directory is scanned
    // here rather than named, because from #157 a library is a scan.
    let rom_dir = tempfile::tempdir().expect("a temp dir to pretend to be a collection");
    let with_library = Arc::new(AppState {
        assets: Arc::new(PlaceholderAssets),
        library: LibrarySlot::scan(rom_dir.path()).expect("the empty temp dir scans"),
    });

    let response = get(with_library, "/api/info").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body(response).await,
        format!(
            "{{\"version\":\"{}\",\"library\":true}}",
            env!("CARGO_PKG_VERSION")
        )
    );
}

/// A path that is an endpoint is not an unknown path, whatever method it arrives
/// with: axum's `MethodRouter` answers 405 for it, and #418 must not have turned
/// that into the 404 of an unknown `/api/` path. The two are different news, and
/// an operator or a client is entitled to hear which one it is.
#[tokio::test]
async fn wrong_method_on_an_api_route_is_method_not_allowed() {
    for (method, uri) in [
        (Method::POST, "/api/info"),
        (Method::PUT, "/api/info"),
        (Method::POST, "/api/roms"),
        (Method::GET, "/api/roms/rescan"),
        (
            Method::DELETE,
            "/api/roms/0000000000000000000000000000000000000000",
        ),
    ] {
        let response = request(state(), method.clone(), uri).await;

        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri}"
        );
    }
}
