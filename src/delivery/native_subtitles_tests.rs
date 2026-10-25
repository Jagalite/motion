//! Actual native original observation and supervised text conversion. These
//! private owner tests do not enable public subtitle pins or artifact grants.
use super::native_subtitles::{Selection, convert};
use crate as playscale_app;
use crate::v2::auth::Caller;
use axum::{
    body::Body,
    extract::FromRequestParts,
    http::{Request, StatusCode},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
#[path = "../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR, observer_binary};

async fn caller(f: &Fixture, token: &str) -> Caller {
    let (mut parts, _) = Request::builder()
        .uri("/api/v2/downloads")
        .header("host", "127.0.0.1:8787")
        .header("authorization", token)
        .body(Body::empty())
        .unwrap()
        .into_parts();
    Caller::from_request_parts(&mut parts, &f.app)
        .await
        .unwrap()
}
async fn fixture(paused: bool) -> (Fixture, Selection, String, String, std::path::PathBuf) {
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("subtitle-source");
    std::fs::create_dir(&root).unwrap();
    let text = f._dir.path().join("source.srt");
    std::fs::write(
        &text,
        "1\n00:00:00,100 --> 00:00:00,500\nActual native subtitle\n",
    )
    .unwrap();
    let media = root.join("text.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=32x32:r=10",
            "-i",
        ])
        .arg(&text)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:s:0",
            "-t",
            "1",
            "-c:v",
            "mpeg4",
            "-c:s",
            "mov_text",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(&media)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    if paused {
        use std::os::unix::fs::PermissionsExt;
        let script = f._dir.path().join("controlled-ffmpeg");
        let started = f._dir.path().join("converter-started");
        let release = f._dir.path().join("converter-release");
        std::fs::write(&script,format!("#!/bin/sh\nprintf started > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.05; done\nexec ffmpeg \"$@\"\n",started.display(),release.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::sync::Arc::get_mut(&mut f.app.processing)
            .unwrap()
            .settings
            .ffmpeg = script;
    }
    let (device, token) = f
        .pair(
            &["default"],
            &["system:admin", "catalog:read", "downloads:manage"],
        )
        .await;
    f.pair(&["default"], &["system:admin", "catalog:read"])
        .await;
    let auth = format!("Bearer {token}");
    f.cutover().await;
    let operator = format!("Bearer {OPERATOR}");
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Actual text","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "native-text-source-request"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{:?}", source.body);
    let binary = observer_binary();
    let observer =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .register_observer(observer)
        .await
        .unwrap();
    crate::native_discovery::scan(
        &f.app,
        source.body["id"].as_str().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (timeline,version,file,revision):(String,String,String,String)=sqlx::query_as("SELECT v.timeline_id,v.id,f.id,f.revision FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files f ON f.id=b.file_id WHERE v.origin='original'").fetch_one(&f.app.db).await.unwrap();
    (
        f,
        Selection {
            profile: "default".into(),
            timeline,
            version,
            file,
            revision,
            subtitle: 0,
        },
        device,
        auth,
        media,
    )
}
async fn revoke(f: &Fixture, device: &str) {
    let operator = format!("Bearer {OPERATOR}");
    let path = format!("/api/v2/devices/{device}");
    let current = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK);
    let changed = f.call("PUT",&format!("{path}/policy"),Some(json!({"library_ids":[],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read"]})),&[("authorization",&operator),("if-match",current.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
}
#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_conversion_reauthorizes_and_refuses_replaced_descriptor() {
    let (mut f, selected, device, auth, media) = fixture(false).await;
    let actual = caller(&f, &auth).await;
    {
        let converted = convert(
            f.app.clone(),
            actual.clone(),
            selected.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            std::str::from_utf8(&converted.bytes)
                .unwrap()
                .contains("Actual native subtitle")
        );
        assert!(
            playscale_core::subtitles::parse_webvtt(std::str::from_utf8(&converted.bytes).unwrap())
                .is_some()
        );
        converted.writer.commit().await.unwrap();
        drop((converted.descriptor, converted.lease));
    }
    // No download/artifact grant is inferred from private process completion.
    let artifacts: i64 = sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(artifacts, 0);
    std::fs::write(&media, b"same path is not the old subtitle occurrence").unwrap();
    let replaced = convert(
        f.app.clone(),
        actual.clone(),
        selected.clone(),
        CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(replaced.code, "source_revision_changed");
    revoke(&f, &device).await;
    let denied = convert(f.app.clone(), actual, selected, CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    f.close().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_conversion_checks_current_permission_after_process_exit() {
    let (mut f, selected, device, auth, _) = fixture(true).await;
    let actual = caller(&f, &auth).await;
    let task = tokio::spawn(convert(
        f.app.clone(),
        actual,
        selected,
        CancellationToken::new(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !f._dir.path().join("converter-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual supervised converter did not reach the controlled boundary");
    revoke(&f, &device).await;
    std::fs::write(f._dir.path().join("converter-release"), b"release").unwrap();
    let denied = task.await.unwrap().err().unwrap();
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let artifacts: i64 = sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(artifacts, 0);
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    f.close().await;
}

async fn admit(
    f: &Fixture,
    actual: &Caller,
    device: &str,
    selected: &Selection,
    key: &str,
) -> serde_json::Value {
    use axum::extract::FromRequest;
    use http_body_util::BodyExt;
    let value = json!({"device_id":device,"profile_id":selected.profile,"timeline_id":selected.timeline,"version_id":selected.version,"recipe_id":null,"tracks":{"audio_component_id":null,"subtitle_component_id":null,"audio_track_id":null,"subtitle_track_id":"s0","subtitle_policy":"require"}});
    let request = Request::builder()
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap();
    let body =
        crate::v2::Body::<crate::v2::downloads::DownloadInput>::from_request(request, &f.app)
            .await
            .unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("idempotency-key", key.parse().unwrap());
    let response = crate::v2::downloads::admit_native_original_text(&f.app, actual, &headers, body)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_pin_cache_uses_shared_admission_and_survives_restart() {
    use crate::v2::downloads::native_subtitles::ensure;
    let (mut f, selected, device, auth, media) = fixture(false).await;
    let actual = caller(&f, &auth).await;
    let download = admit(&f, &actual, &device, &selected, "native-private-text-pin").await;
    let id = download["id"].as_str().unwrap();
    let bytes = ensure(
        f.app.clone(),
        actual.clone(),
        id.into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("Actual native subtitle")
    );
    let replay = admit(&f, &actual, &device, &selected, "native-private-text-pin").await;
    assert_eq!(replay["id"], download["id"]);
    assert_eq!(replay["revision"], "2");
    let stored: (String, i64, String) = sqlx::query_as(
        "SELECT state,size_bytes,digest FROM download_subtitle_artifacts WHERE download_id=?",
    )
    .bind(id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(stored.0, "ready");
    assert_eq!(stored.1, bytes.len() as i64);
    assert_eq!(stored.2, catabolic_core::sha256(&bytes));
    let manifest = crate::v2::downloads::native_subtitles::manifest(
        f.app.clone(),
        actual.clone(),
        id.into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(manifest["download_id"], id);
    assert_eq!(manifest["download_revision"], "2");
    assert_eq!(manifest["files"].as_array().unwrap().len(), 2);
    assert_eq!(
        manifest["files"][1]["digest"],
        format!("sha256:{}", stored.2)
    );
    assert_eq!(manifest["files"][1]["size_bytes"], bytes.len().to_string());
    assert!(manifest.get("metadata").is_none());
    // This internal candidate is deliberately not a public capability yet.
    let public = crate::v2::downloads::manifest(
        axum::extract::State(f.app.clone()),
        actual.clone(),
        axum::extract::Path(id.into()),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(public.code, "native_manifest_unavailable");
    f.close().await;
    f.open().await;
    let actual = caller(&f, &auth).await;
    let cached = ensure(
        f.app.clone(),
        actual.clone(),
        id.into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(cached, bytes);
    let restarted_manifest = crate::v2::downloads::native_subtitles::manifest(
        f.app.clone(),
        actual.clone(),
        id.into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(restarted_manifest, manifest);

    let (_, foreign) = f
        .pair(
            &["default"],
            &["system:admin", "catalog:read", "downloads:manage"],
        )
        .await;
    let foreign = caller(&f, &format!("Bearer {foreign}")).await;
    assert_eq!(
        ensure(f.app.clone(), foreign, id.into(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::NOT_FOUND
    );
    let path = f
        .app
        .processing
        .root
        .parent()
        .unwrap()
        .join("download-subtitles")
        .join(format!("{id}.vtt"));
    std::fs::write(&path, b"WEBVTT\n\nuntrusted cached replacement\n").unwrap();
    let corrupt = ensure(
        f.app.clone(),
        actual.clone(),
        id.into(),
        CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(corrupt.code, "subtitle_artifact_changed");
    std::fs::write(&media, b"changed original occurrence").unwrap();
    let changed = ensure(
        f.app.clone(),
        actual.clone(),
        id.into(),
        CancellationToken::new(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(changed.code, "source_revision_changed");
    revoke(&f, &device).await;
    assert_eq!(
        ensure(f.app.clone(), actual, id.into(), CancellationToken::new())
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::FORBIDDEN
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_cache_caller_abort_keeps_recovery_witness_until_exit() {
    use crate::v2::downloads::native_subtitles::ensure;
    let (mut f, selected, device, auth, _) = fixture(true).await;
    let actual = caller(&f, &auth).await;
    let pin = admit(&f, &actual, &device, &selected, "native-text-aborted-pin").await;
    let id = pin["id"].as_str().unwrap().to_owned();
    let task = tokio::spawn(ensure(
        f.app.clone(),
        actual,
        id.clone(),
        CancellationToken::new(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !f._dir.path().join("converter-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual converter did not start");
    let held = crate::processing::held_witnesses(&f.app.processing.root).unwrap();
    assert_eq!(
        held.len(),
        1,
        "ordinary recursive recovery must find the actual subtitle .owner"
    );
    // Cold processing recovery shares the already admitted native owner. This
    // exercises the real recovery routine, not a claim of full server restart.
    let mut recovered = f.app.clone();
    let mut processing = crate::processing::Runtime::new(
        f.app.processing.root.clone(),
        f.app.processing.settings.clone(),
    );
    processing.supervisor = observer_binary();
    recovered.processing = std::sync::Arc::new(processing);
    crate::processing::recover(&recovered).await.unwrap();
    assert_eq!(recovered.processing.execution.snapshot().used, 1);
    assert!(
        f.app
            .processing
            .execution
            .close_admission_if_idle()
            .is_err()
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while f.app.processing.execution.snapshot().used != 0
            || recovered.processing.execution.snapshot().used != 0
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("aborted conversion or recovered witness did not settle");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        !f.app
            .processing
            .root
            .parent()
            .unwrap()
            .join("download-subtitles")
            .join(format!("{id}.vtt"))
            .exists()
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_cache_service_drain_waits_without_artifact_ack() {
    use crate::v2::downloads::native_subtitles::ensure;
    let (mut f, selected, device, auth, _) = fixture(true).await;
    let actual = caller(&f, &auth).await;
    let pin = admit(&f, &actual, &device, &selected, "native-text-drained-pin").await;
    let stop = CancellationToken::new();
    let task = tokio::spawn(ensure(
        f.app.clone(),
        actual,
        pin["id"].as_str().unwrap().into(),
        stop.clone(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !f._dir.path().join("converter-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual converter did not start");
    f.app.processing.execution.begin_drain();
    assert!(
        f.app
            .processing
            .execution
            .close_admission_if_idle()
            .is_err()
    );
    stop.cancel();
    assert!(task.await.unwrap().is_err());
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("service drain did not retain and settle converter custody");
    f.app
        .processing
        .execution
        .close_admission_if_idle()
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_cache_refuses_path_replacement_after_descriptor_acquisition() {
    use crate::v2::downloads::native_subtitles::ensure;
    let (mut f, selected, device, auth, media) = fixture(true).await;
    let actual = caller(&f, &auth).await;
    let pin = admit(&f, &actual, &device, &selected, "native-text-replaced-pin").await;
    let task = tokio::spawn(ensure(
        f.app.clone(),
        actual,
        pin["id"].as_str().unwrap().into(),
        CancellationToken::new(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !f._dir.path().join("converter-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual converter did not acquire its input descriptor");
    // FFmpeg retains the original open inode. An identical-byte replacement at
    // its old name must still fail final exact occurrence/descriptor custody.
    let retained = f._dir.path().join("retained-original.mp4");
    std::fs::rename(&media, &retained).unwrap();
    std::fs::copy(&retained, &media).unwrap();
    std::fs::write(f._dir.path().join("converter-release"), b"release").unwrap();
    let refused = task.await.unwrap().err().unwrap();
    assert_eq!(refused.code, "source_revision_changed");
    let artifacts: i64 = sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(artifacts, 0);
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    f.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_native_text_cache_collision_does_not_overwrite_or_ack() {
    use crate::v2::downloads::native_subtitles::ensure;
    let (mut f, selected, device, auth, _) = fixture(true).await;
    let actual = caller(&f, &auth).await;
    let pin = admit(
        &f,
        &actual,
        &device,
        &selected,
        "native-text-output-collision",
    )
    .await;
    let id = pin["id"].as_str().unwrap().to_owned();
    let task = tokio::spawn(ensure(
        f.app.clone(),
        actual,
        id.clone(),
        CancellationToken::new(),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !f._dir.path().join("converter-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual converter did not start");
    let root = f
        .app
        .processing
        .root
        .parent()
        .unwrap()
        .join("download-subtitles");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{id}.vtt"));
    std::fs::write(&path, b"replacement inserted during real conversion").unwrap();
    std::fs::write(f._dir.path().join("converter-release"), b"release").unwrap();
    let error = task.await.unwrap().err().unwrap();
    assert_eq!(error.code, "subtitle_artifact_changed");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"replacement inserted during real conversion"
    );
    let artifacts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM download_subtitle_artifacts WHERE download_id=?")
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(artifacts, 0);
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM downloads WHERE id=?")
        .bind(&id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(revision, 1);
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    let cancelled = f
        .call(
            "DELETE",
            &format!("/api/v2/downloads/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(
        cancelled.status,
        StatusCode::NO_CONTENT,
        "{:?}",
        cancelled.body
    );
    crate::v2::downloads::release_subtitles(f.app.clone()).await;
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"replacement inserted during real conversion",
        "cancellation must not unlink an unknown native cache entry"
    );
    f.close().await;
}
