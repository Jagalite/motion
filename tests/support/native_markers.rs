//! Actual container chapters plus Motion-owned manual corrections, never fake probe facts.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use serde_json::json;

pub async fn exercise() {
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("chapters");
    std::fs::create_dir(&root).unwrap();
    let metadata = f._dir.path().join("chapters.ffmetadata");
    std::fs::write(&metadata, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1000\ntitle=Opening\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=1000\nEND=2000\ntitle=Ending\n").unwrap();
    let media = root.join("chapters.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=32x32:r=10",
            "-f",
            "ffmetadata",
            "-i",
        ])
        .arg(&metadata)
        .args([
            "-map",
            "0:v:0",
            "-map_metadata",
            "1",
            "-map_chapters",
            "1",
            "-t",
            "2",
            "-c:v",
            "mpeg4",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(&media)
        .output()
        .expect("real FFmpeg required for chapter qualification");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.cutover().await;
    let auth = format!("Bearer {OPERATOR}");
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Chapter source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "real-chapter-source-request"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{:?}", source.body);
    let source_id = source.body["id"].as_str().unwrap();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_playscale"));
    let observer =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            binary,
            &catabolic_core::sha256(std::fs::read(binary).unwrap()),
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
    playscale::native_discovery::scan(
        &f.app,
        source_id,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    let timeline: String =
        sqlx::query_scalar("SELECT timeline_id FROM media_versions WHERE origin='original'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let path = format!("/api/v2/catalog/timelines/{timeline}/markers");
    let anonymous = f.call("GET", &path, None, &[]).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let first = f
        .call(
            "GET",
            &format!("{path}?effective=true&limit=1"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{:?}", first.body);
    assert_eq!(first.body["items"][0]["label"], "Opening");
    assert_eq!(first.body["items"][0]["provenance"], "embedded");
    let revision = first.body["items"][0]["timeline_revision"].clone();
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let second = f
        .call(
            "GET",
            &format!("{path}?effective=true&limit=1&cursor={cursor}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(second.status, StatusCode::OK, "{:?}", second.body);
    assert_eq!(second.body["items"][0]["label"], "Ending");
    let manual = f.call("POST", &path, Some(json!({"timeline_revision":revision,"kind":"chapter","start_ms":0,"end_ms":null,"label":"Manual opening"})), &[("authorization",&auth),("idempotency-key","manual-chapter-correction")]).await;
    assert_eq!(manual.status, StatusCode::CREATED, "{:?}", manual.body);
    let changed = f
        .call(
            "GET",
            &format!("{path}?effective=true&limit=1&cursor={cursor}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(changed.status, StatusCode::CONFLICT, "{:?}", changed.body);
    assert_eq!(changed.body["code"], "markers_changed");
    let effective = f
        .call(
            "GET",
            &format!("{path}?effective=true"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(effective.status, StatusCode::OK, "{:?}", effective.body);
    assert!(
        effective.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == manual.body["id"])
    );
    std::fs::remove_file(&media).unwrap();
    playscale::native_discovery::scan(
        &f.app,
        source_id,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    for restart in [false, true] {
        if restart {
            f.close().await;
            f.open().await;
        }
        let offline = f
            .call("GET", &path, None, &[("authorization", &auth)])
            .await;
        assert_eq!(offline.status, StatusCode::OK, "{:?}", offline.body);
        let rows = offline.body["items"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "unavailable original contributes no embedded chapters"
        );
        assert_eq!(rows[0], manual.body);
    }
    f.close().await;
}
