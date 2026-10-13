use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use motion_ui::offline::CachedDownload;
use motion_ui_host::{Cache, CacheManifest, EventLog, Host};
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
                viewing_epoch: Some("server:restore".into()),
            },
            title: "<Saved title>".into(),
            duration_ms: 10000,
            sha256: digest,
            size: 10,
            content_type: "video/mp4".into(),
            resume_position_ms: None,
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
        device_time: None,
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
    let history =
        serde_json::from_slice::<EventLog>(&std::fs::read(dir.path().join("events.json")).unwrap())
            .unwrap()
            .events;
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
        device_time: None,
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
    assert!(
        !std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")),
        "failed publication must remove its temporary log"
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
    let history =
        serde_json::from_slice::<EventLog>(&std::fs::read(dir.path().join("events.json")).unwrap())
            .unwrap()
            .events;
    assert_eq!(history, vec![event]);
}

#[cfg(unix)]
#[tokio::test]
async fn publication_stays_with_open_cache_when_its_path_is_replaced() {
    let (dir, manifest) = fixture();
    let cache = Arc::new(Cache::open(dir.path()).unwrap());
    let relocated = tempfile::tempdir().unwrap();
    let moved = relocated.path().join("cache");
    std::fs::rename(dir.path(), &moved).unwrap();
    std::fs::create_dir(dir.path()).unwrap();
    let event = OfflineEvent {
        scope: manifest.scope.clone(),
        media: manifest.downloads[0].identity.clone(),
        event_id: "pinned-directory".into(),
        device_sequence: "1".into(),
        position_ms: 1000,
        status: "paused".into(),
        device_time: None,
    };
    let (app, cookie) = authenticated(cache).await;
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
    assert!(!dir.path().join("events.json").exists());
    let history =
        serde_json::from_slice::<EventLog>(&std::fs::read(moved.join("events.json")).unwrap())
            .unwrap()
            .events;
    assert_eq!(history, vec![event]);
    drop(app);
    assert!(Cache::open(&moved).is_ok());
}

/// The offline page removes a saved title through the host: authenticated
/// like every cache write, the title leaves the page and the cache, an open
/// player's ticket stops serving it, and recorded progress is kept.
#[tokio::test]
async fn a_saved_title_is_removed_from_the_offline_host() {
    let (dir, _) = fixture();
    let cache = Arc::new(Cache::open(dir.path()).unwrap());
    let (app, cookie) = authenticated(cache.clone()).await;
    let (status, _, body) = request(&app, "GET", "/", &cookie, None).await;
    assert_eq!(status, StatusCode::OK);
    let page = String::from_utf8(body).unwrap();
    assert!(
        page.contains(r#"data-remove-download="download""#),
        "{page}"
    );
    // Without the cache cookie nothing is removed.
    let (status, _, _) = request(&app, "POST", "/cache/remove/download", "", Some(json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, opened) = request(
        &app,
        "POST",
        "/cache/open/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let opened: Value = serde_json::from_slice(&opened).unwrap();
    let (status, _, _) = request(
        &app,
        "POST",
        "/cache/remove/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = request(
        &app,
        "POST",
        "/cache/remove/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, body) = request(&app, "GET", "/", &cookie, None).await;
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains("data-remove-download")
    );
    let (status, _, _) = request(
        &app,
        "GET",
        opened["media_url"].as_str().unwrap(),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!dir.path().join("blobs").read_dir().unwrap().any(|_| true));
}

/// The manifest write commits but releasing the blob fails: the removal is
/// reported, the host presents exactly what was committed and the open
/// ticket stops serving.
#[cfg(unix)]
#[tokio::test]
async fn a_partially_committed_removal_is_what_the_host_presents() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, _) = fixture();
    let cache = Arc::new(Cache::open(dir.path()).unwrap());
    let (app, cookie) = authenticated(cache.clone()).await;
    let (status, _, opened) = request(
        &app,
        "POST",
        "/cache/open/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let opened: Value = serde_json::from_slice(&opened).unwrap();
    let blobs = dir.path().join("blobs");
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (status, _, _) = request(
        &app,
        "POST",
        "/cache/remove/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    std::fs::set_permissions(&blobs, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The removal committed; only freeing the blob failed (collected later).
    assert_eq!(status, StatusCode::OK, "the committed removal is reported");
    assert!(!cache.presents("download"));
    let (_, _, body) = request(&app, "GET", "/", &cookie, None).await;
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains("data-remove-download")
    );
    let (status, _, _) = request(
        &app,
        "GET",
        opened["media_url"].as_str().unwrap(),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// An earlier removal of A recorded its intent but stopped before its copy
/// went (a crash or failed write). A is no longer presented; retrying A
/// reports success, and removing B completes A and invalidates A's ticket.
#[tokio::test]
async fn interrupted_removals_are_never_presented_and_complete_later() {
    let (dir, mut manifest) = fixture();
    let mut second = manifest.downloads[0].clone();
    second.identity.download_id = "second".into();
    manifest.downloads.push(second);
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let cache = Arc::new(Cache::open(dir.path()).unwrap());
    let (app, cookie) = authenticated(cache.clone()).await;
    let (status, _, opened) = request(
        &app,
        "POST",
        "/cache/open/download",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let opened: Value = serde_json::from_slice(&opened).unwrap();
    // The intent of an interrupted removal of "download" is durable; the
    // running host learns of it with the next removal it performs.
    std::fs::write(dir.path().join("removals.json"), br#"["download"]"#).unwrap();
    let (status, _, _) = request(
        &app,
        "POST",
        "/cache/remove/second",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!cache.presents("download") && !cache.presents("second"));
    let (status, _, _) = request(
        &app,
        "GET",
        opened["media_url"].as_str().unwrap(),
        &cookie,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the completed removal's ticket is gone"
    );
    drop(app);
    drop(cache);
    // A host opened over an intent whose copy is still there, and which
    // cannot even complete it (the root is read-only, so the manifest write
    // fails), presents nothing of it.
    let (dir, _) = fixture();
    std::fs::write(dir.path().join("removals.json"), br#"["download"]"#).unwrap();
    std::fs::write(dir.path().join("events.lock"), b"").unwrap();
    std::fs::create_dir(dir.path().join("staging")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    }
    let cache = Arc::new(Cache::open(dir.path()).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        saved["downloads"].as_array().unwrap().len(),
        1,
        "the copy is still listed"
    );
    assert!(!cache.presents("download"));
    let (app, cookie) = authenticated(cache.clone()).await;
    let (_, _, body) = request(&app, "GET", "/", &cookie, None).await;
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains("data-remove-download")
    );
}
