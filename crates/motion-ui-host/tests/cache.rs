use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use motion_ui::offline::CachedDownload;
use motion_ui_host::{Cache, CacheManifest, Host};
use playscale_core::offline::{CacheScope, DownloadIdentity, OfflineEvent};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
fn fixture() -> (tempfile::TempDir, CacheManifest) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("blobs")).unwrap();
    let digest = format!("{:x}", Sha256::digest(b"0123456789"));
    std::fs::write(dir.path().join("blobs").join(&digest), b"0123456789").unwrap();
    let manifest = CacheManifest {
        protocol: 1,
        scope: CacheScope {
            server_id: "s".into(),
            principal_id: "p".into(),
            profile_id: "profile".into(),
            device_id: "device".into(),
        },
        downloads: vec![CachedDownload {
            identity: DownloadIdentity {
                download_id: "download".into(),
                timeline_id: "timeline".into(),
                timeline_revision: "1".into(),
                source_revision: "sha256-source".into(),
                base_viewing_revision: "4".into(),
                base_manual_epoch: "2".into(),
            },
            title: "<Saved title>".into(),
            duration_ms: 10000,
            sha256: digest,
            size: 10,
            content_type: "video/mp4".into(),
        }],
    };
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    (dir, manifest)
}
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "127.0.0.1:1234")
        .header("Cookie", cookie)
        .header("Content-Type", "application/json")
        .header("X-Motion-Cache", "1")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    (
        status,
        headers,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}
async fn authenticated(cache: Arc<Cache>) -> (Router, String) {
    let app = Host::new(cache, "http://127.0.0.1:1234".into(), "secret".into()).router();
    let (status, headers, _) = request(
        &app,
        "POST",
        "/cache/bootstrap",
        "",
        Some(json!({"credential":"secret"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (
        app,
        headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned(),
    )
}
#[tokio::test]
async fn origin_authentication_and_no_catalog_or_path_access() {
    let (dir, _) = fixture();
    let cache = Arc::new(Cache::open(dir.path()).unwrap());
    assert!(Cache::open(dir.path()).is_err());
    let (app, cookie) = authenticated(cache).await;
    assert_eq!(
        request(
            &app,
            "POST",
            "/cache/bootstrap",
            "",
            Some(json!({"credential":"secret"}))
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&app, "GET", "/", "", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for path in [
        "/api/v2/catalog/items",
        "/motion.sqlite",
        "/cache/media/../../secret",
        "/cache/media/unknown",
    ] {
        assert_eq!(
            request(&app, "GET", path, &cookie, None).await.0,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let req = Request::builder()
        .uri("/")
        .header("Host", "evil.test")
        .header("Cookie", &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let req = Request::builder()
        .method("POST")
        .uri("/cache/open/download")
        .header("Host", "127.0.0.1:1234")
        .header("Cookie", &cookie)
        .header("Origin", "https://evil.test")
        .header("X-Motion-Cache", "1")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let (status, headers, body) = request(&app, "GET", "/", &cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    let text = String::from_utf8(body).unwrap();
    assert!(text.contains("&lt;Saved title&gt;"));
    assert!(!text.contains("<Saved title>"));
}
#[tokio::test]
async fn verified_snapshot_range_and_mutation_are_isolated() {
    let (dir, manifest) = fixture();
    let (app, cookie) = authenticated(Arc::new(Cache::open(dir.path()).unwrap())).await;
    let (_, _, body) = request(
        &app,
        "POST",
        "/cache/open/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    let opened: Value = serde_json::from_slice(&body).unwrap();
    let path = opened["media_url"].as_str().unwrap();
    std::fs::write(
        dir.path().join("blobs").join(&manifest.downloads[0].sha256),
        b"CORRUPTED!",
    )
    .unwrap();
    let req = Request::builder()
        .uri(path)
        .header("Host", "127.0.0.1:1234")
        .header("Cookie", &cookie)
        .header("Range", "bytes=2-5")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 2-5/10");
    assert_eq!(
        &response.into_body().collect().await.unwrap().to_bytes()[..],
        b"2345"
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/cache/open/download",
            &cookie,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
#[tokio::test]
async fn ordered_events_are_durable_exact_and_never_rebased() {
    let (dir, manifest) = fixture();
    let event = OfflineEvent {
        scope: manifest.scope.clone(),
        media: manifest.downloads[0].identity.clone(),
        event_id: "event1".into(),
        device_sequence: "1".into(),
        position_ms: 5000,
        status: "paused".into(),
    };
    {
        let (app, cookie) = authenticated(Arc::new(Cache::open(dir.path()).unwrap())).await;
        for _ in 0..2 {
            assert_eq!(
                request(
                    &app,
                    "POST",
                    "/cache/events",
                    &cookie,
                    Some(serde_json::to_value(&event).unwrap())
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        let mut stale = event.clone();
        stale.position_ms = 6000;
        assert_eq!(
            request(
                &app,
                "POST",
                "/cache/events",
                &cookie,
                Some(serde_json::to_value(&stale).unwrap())
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        stale = event.clone();
        stale.event_id = "event2".into();
        stale.device_sequence = "3".into();
        assert_eq!(
            request(
                &app,
                "POST",
                "/cache/events",
                &cookie,
                Some(serde_json::to_value(&stale).unwrap())
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    }
    let history: Vec<OfflineEvent> =
        serde_json::from_slice(&std::fs::read(dir.path().join("events.json")).unwrap()).unwrap();
    assert_eq!(history, vec![event]);
    let (app, cookie) = authenticated(Arc::new(Cache::open(dir.path()).unwrap())).await;
    let (_, _, body) = request(
        &app,
        "POST",
        "/cache/open/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    let opened: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(opened["position_ms"], 5000);
    assert_eq!(opened["next_sequence"], "2");
    assert!(!dir.path().join("motion.sqlite").exists());
}
#[cfg(unix)]
#[tokio::test]
async fn cache_symlinks_cannot_escape_to_source_roots() {
    let (dir, manifest) = fixture();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), b"0123456789").unwrap();
    let blob = dir.path().join("blobs").join(&manifest.downloads[0].sha256);
    std::fs::remove_file(&blob).unwrap();
    std::os::unix::fs::symlink(outside.path(), blob).unwrap();
    let (app, cookie) = authenticated(Arc::new(Cache::open(dir.path()).unwrap())).await;
    assert_eq!(
        request(
            &app,
            "POST",
            "/cache/open/download",
            &cookie,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn failed_publication_cannot_acknowledge_or_consume_a_sequence() {
    let (dir, manifest) = fixture();
    let (app, cookie) = authenticated(Arc::new(Cache::open(dir.path()).unwrap())).await;
    let event = OfflineEvent {
        scope: manifest.scope.clone(),
        media: manifest.downloads[0].identity.clone(),
        event_id: "retry".into(),
        device_sequence: "1".into(),
        position_ms: 1000,
        status: "paused".into(),
    };
    // Real rename failure, before the in-memory log is published.
    std::fs::create_dir(dir.path().join("events.json")).unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/cache/events",
            &cookie,
            Some(serde_json::to_value(&event).unwrap())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    std::fs::remove_dir(dir.path().join("events.json")).unwrap();
    let (a, b) = tokio::join!(
        request(
            &app,
            "POST",
            "/cache/events",
            &cookie,
            Some(serde_json::to_value(&event).unwrap())
        ),
        request(
            &app,
            "POST",
            "/cache/events",
            &cookie,
            Some(serde_json::to_value(&event).unwrap())
        )
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::OK);
    let history: Vec<OfflineEvent> =
        serde_json::from_slice(&std::fs::read(dir.path().join("events.json")).unwrap()).unwrap();
    assert_eq!(history, vec![event]);
}
