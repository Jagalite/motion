//! Motion's Topcoat presentation layer (A11, design 1.1.0).
//!
//! `router` builds the Topcoat page router over a [`UiQueryFacade`].
//! [`mount`] composes it into an Axum application the way the plan requires
//! (section 11.7): the public API keeps JSON errors and is never answered by
//! a UI fallback, presentation assets and Demuxe are separate reserved trees,
//! media/API responses are untouched by presentation headers, and Topcoat
//! receives the full original URI and the genuine peer address.

pub mod assets;
mod app;
pub mod facade;
pub mod mock;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router as AxumRouter,
    body::Body,
    extract::{ConnectInfo, Path, Request},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use topcoat::router::{RemoteAddr, tower::TowerService};

pub use facade::{Facade, UiPrincipal, UiQueryFacade};

/// The strict baseline policy for server-rendered HTML (plan section 12.5).
/// No `unsafe-eval`/`unsafe-inline` scripts; Topcoat's reactive runtime is not
/// admitted. `wasm-unsafe-eval` is for Demuxe's WebAssembly providers only and
/// does not allow JavaScript string compilation.
pub const CONTENT_SECURITY_POLICY: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self' 'wasm-unsafe-eval'; ",
    "worker-src 'self' blob:; ",
    // The pinned Demuxe archive renders one static shadow-root <style>; allow
    // exactly that text by hash (see DEMUXE_STYLE_HASH), not 'unsafe-inline'.
    "style-src 'self' 'sha256-smNQpTGGdipSJJHMEztOmlzNusICCiQBd+JnOrZepKs='; ",
    "img-src 'self' data: blob:; ",
    "media-src 'self' blob:; ",
    "connect-src 'self'; ",
    "font-src 'self'; ",
    "object-src 'none'; ",
    "base-uri 'none'; ",
    "frame-ancestors 'none'; ",
    "form-action 'self'"
);

/// sha256 of the player stylesheet text rendered by the installed Demuxe
/// archive (demuxe 1.1.0, archive sha256 ddb82eb5…f66f). Changing the Demuxe
/// archive requires re-deriving it from the browser's CSP report.
pub const DEMUXE_STYLE_HASH: &str = "sha256-smNQpTGGdipSJJHMEztOmlzNusICCiQBd+JnOrZepKs=";

pub fn router(facade: Arc<dyn UiQueryFacade>) -> topcoat::router::Router {
    app::router(Facade(facade))
}

/// Compose the presentation with an API router whose routes use full paths
/// under `/api/v2`. `demuxe_dir` is the verified Demuxe package directory.
pub fn mount(api: AxumRouter, facade: Arc<dyn UiQueryFacade>, demuxe_dir: Option<PathBuf>) -> AxumRouter {
    let presentation = TowerService::new(router(facade));
    let mut app = AxumRouter::new()
        .merge(api)
        // Unknown API paths stay JSON Problem Details, never HTML.
        .route("/api/v2", any(api_not_found))
        .route("/api/v2/", any(api_not_found))
        .route("/api/v2/{*rest}", any(api_not_found))
        .route("/ui/{file}", get(ui_asset));
    if let Some(dir) = demuxe_dir {
        app = app.nest_service("/assets/demuxe", tower_http::services::ServeDir::new(dir).append_index_html_on_directories(false));
    }
    app.fallback_service(
        AxumRouter::new()
            .fallback_service(presentation)
            .layer(middleware::from_fn(presentation_headers))
            .layer(middleware::map_request(remote_addr)),
    )
}

async fn api_not_found(request: Request) -> Response {
    let body = serde_json::json!({
        "type": "about:blank",
        "title": "Not Found",
        "status": 404,
        "code": "not_found",
        "detail": format!("no operation at {}", request.uri().path()),
        "request_id": "ui-composition",
        "retryable": false,
    });
    (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response()
}

async fn ui_asset(Path(file): Path<String>) -> Response {
    match assets::lookup(&file) {
        Some(asset) => (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::HeaderName::from_static("cross-origin-resource-policy"), "same-origin"),
            ],
            asset.bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Topcoat reads the transport peer from `RemoteAddr`; in-process embedding adds no proxy hop.
async fn remote_addr(mut request: Request) -> Request {
    if let Some(ConnectInfo(addr)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        let addr = *addr;
        request.extensions_mut().insert(RemoteAddr(addr));
    }
    request
}

/// Security and caching headers for presentation responses only.
async fn presentation_headers(request: Request, next: Next) -> Response {
    let head = request.method() == Method::HEAD;
    let mut response = next.run(request).await;
    // HEAD carries headers only, even when the page handler produced a body.
    if head {
        *response.body_mut() = Body::empty();
    }
    let headers = response.headers_mut();
    let set = |headers: &mut axum::http::HeaderMap, name: &'static str, value: &'static str| {
        headers.insert(header::HeaderName::from_static(name), HeaderValue::from_static(value));
    };
    set(headers, "content-security-policy", CONTENT_SECURITY_POLICY);
    set(headers, "cross-origin-opener-policy", "same-origin");
    set(headers, "cross-origin-embedder-policy", "require-corp");
    set(headers, "cross-origin-resource-policy", "same-origin");
    set(headers, "referrer-policy", "no-referrer");
    set(headers, "x-content-type-options", "nosniff");
    // Authenticated HTML is private and never cached (plan section 12.5).
    set(headers, "cache-control", "private, no-store");
    response
}
