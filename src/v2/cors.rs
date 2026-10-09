//! Approved third-party browser origins (plan §12.3). Off unless configured.
//! Approved origins may call `/api/v2` with bearer tokens only: no cookies,
//! never credentialed CORS, never a wildcard, and a narrow method/header set.
use axum::{
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};

const METHODS: &str = "GET, HEAD, POST, PUT, DELETE";
const HEADERS: &str = "authorization, content-type, idempotency-key, if-match, last-event-id";
const EXPOSED: &str = "etag, x-request-id, retry-after, idempotent-replayed";

pub fn approved<'a>(origins: &[String], headers: &'a HeaderMap) -> Option<&'a str> {
    let origin = headers.get(header::ORIGIN)?.to_str().ok()?;
    origins.iter().any(|o| o == origin).then_some(origin)
}

/// A cross-origin request is acceptable only when it authenticates with a
/// bearer token and carries no cookie, so the browser's ambient session can
/// never be used by another site.
pub fn bearer_only(headers: &HeaderMap) -> bool {
    !headers.contains_key(header::COOKIE)
        && headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer "))
}

pub fn is_preflight(method: &Method, headers: &HeaderMap) -> bool {
    method == Method::OPTIONS && headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
}

pub fn preflight(origin: &str) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    decorate(&mut response, origin);
    let h = response.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(METHODS),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(HEADERS),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("600"),
    );
    response
}

pub fn decorate(response: &mut Response, origin: &str) {
    let h = response.headers_mut();
    if let Ok(origin) = HeaderValue::from_str(origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED),
    );
    h.append(header::VARY, HeaderValue::from_static("origin"));
}
