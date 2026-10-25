//! Exercise the real outer HTML router, never a synthetic presentation principal.
use super::native_application_fixture::Fixture;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

pub async fn player(f: &Fixture, timeline: &str, auth: &str) -> (StatusCode, String) {
    let mut request = Request::builder()
        .uri(format!("/play/{timeline}"))
        .header("host", "127.0.0.1:8787");
    if !auth.is_empty() {
        request = request.header("authorization", auth);
    }
    let response = super::playscale_app::api::router(f.app.clone(), None)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}
