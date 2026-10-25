//! Historical offline reconciliation, distinct from transfer/byte eligibility.
//! Exercises exact public offline sync without claiming restored-history reanchor.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn sync(f: &Fixture, auth: &str, epoch: &str, input: Value) -> (StatusCode, Value) {
    let r = super::playscale_app::api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/offline/sync")
                .header("authorization", auth)
                .header("host", "127.0.0.1:8787")
                .header("content-type", "application/json")
                .header("x-motion-viewing-epoch", epoch)
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn state(f: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'").fetch_one(&f.app.db).await.unwrap()
}
pub async fn exercise() {
    let mut f = Fixture::new().await;
    // Exact preserved original bytes and typed imported timeline; no codec or
    // renderer capability is claimed by this offline-history fixture.
    let source = f
        .seed_preserved_timeline("offline.mp4", b"retained exact original download bytes")
        .await;
    let (device, token) = f
        .pair(
            &["default"],
            &[
                "system:admin",
                "catalog:read",
                "downloads:manage",
                "viewing:write",
            ],
        )
        .await;
    let auth = format!("Bearer {token}");
    let _reserve = f
        .pair(
            &["default"],
            &["system:admin", "catalog:read", "viewing:write"],
        )
        .await;
    let (hidden_device, hidden_token) = f
        .pair(&["default"], &["catalog:read", "viewing:write"])
        .await;
    let hidden = format!("Bearer {hidden_token}");
    f.cutover_source(Some(&source)).await;
    let download=f.download_adapter("POST","/downloads",&auth,Some(json!({"device_id":device,"profile_id":"default","timeline_id":"timeline","version_id":"version","recipe_id":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}})),Some("offline-real-native-download")).await;
    assert_eq!(download.status, StatusCode::ACCEPTED, "{:?}", download.body);
    let id = download.body["id"].as_str().unwrap().to_owned();
    let manifest = f.manifest_adapter(&id, &auth).await;
    assert_eq!(manifest.status, StatusCode::OK, "{:?}", manifest.body);
    let epoch = manifest.body["viewing_epoch"].as_str().unwrap().to_owned();
    let event = |sequence: u64, pos: u64| {
        json!({"event_id":format!("native-offline-{sequence}"),"device_sequence":sequence.to_string(),"download_id":id,"profile_id":"default","timeline_id":"timeline",
        "timeline_revision":manifest.body["timeline_revision"],"base_viewing_revision":manifest.body["viewing_revision"],"base_manual_epoch":manifest.body["manual_epoch"],
        "position_ms":pos,"status":"paused","device_time":"2026-10-21T00:00:00Z"})
    };
    let batch = |e: Value| json!({"device_id":device,"events":[e],"acknowledged_through":null,"reanchor":null});
    let before = state(&f).await;
    let anonymous = sync(&f, "", &epoch, batch(event(1, 300))).await;
    assert_eq!(anonymous.0, StatusCode::UNAUTHORIZED);
    assert_eq!(state(&f).await, before);
    let missing_epoch = f
        .call(
            "POST",
            "/api/v2/offline/sync",
            Some(batch(event(1, 300))),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(missing_epoch.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(missing_epoch.body["code"], "viewing_epoch_required");
    let stale = sync(&f, &auth, "other-server:old", batch(event(1, 300))).await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["code"], "viewing_epoch_changed");
    let mut reanchor = batch(event(1, 300));
    reanchor["acknowledged_through"] = json!("0");
    reanchor["reanchor"] = json!({"from_viewing_epoch":epoch,"through":"0"});
    let refused = sync(&f, &auth, &epoch, reanchor).await;
    assert_eq!(refused.0, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.1["code"], "invalid_reanchor");
    assert_eq!(state(&f).await, before);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM offline_sequences WHERE device_id=?")
        .bind(&device)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let first = sync(&f, &auth, &epoch, batch(event(1, 300))).await;
    assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);
    assert_eq!(first.1["results"][0]["outcome"], "applied");
    let applied = state(&f).await;
    assert_eq!(applied, (before.0 + 1, before.1, 300));
    let duplicate = sync(&f, &auth, &epoch, batch(event(1, 300))).await;
    assert_eq!(duplicate.1["results"][0]["outcome"], "duplicate");
    assert_eq!(state(&f).await, applied);
    let conflict = sync(&f, &auth, &epoch, batch(event(1, 301))).await;
    assert_eq!(conflict.1["results"][0]["code"], "event_conflict");
    let gap = sync(&f, &auth, &epoch, batch(event(3, 350))).await;
    assert_eq!(gap.1["results"][0]["code"], "sequence_gap");
    assert_eq!(gap.1["next_device_sequence"], "2");
    let mut denied = batch(event(1, 400));
    denied["device_id"] = json!(hidden_device);
    let denied = sync(&f, &hidden, &epoch, denied).await;
    assert_eq!(denied.0, StatusCode::OK, "{:?}", denied.1);
    assert_eq!(denied.1["results"][0]["code"], "not_found");
    assert!(denied.1["results"][0]["current_viewing"].is_null());
    assert_eq!(state(&f).await, applied);
    let binary = super::native_application_fixture::observer_binary();
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
    std::fs::remove_file(f._dir.path().join("media/offline.mp4")).unwrap();
    super::playscale_app::native_discovery::scan(
        &f.app,
        &source,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    let expired = f
        .download_adapter("GET", &format!("/downloads/{id}"), &auth, None, None)
        .await;
    assert_eq!(expired.body["state"], "expired");
    let offline = sync(&f, &auth, &epoch, batch(event(2, 400))).await;
    assert_eq!(offline.0, StatusCode::OK, "{:?}", offline.1);
    assert_eq!(offline.1["results"][0]["outcome"], "applied");
    assert_eq!(state(&f).await, (before.0 + 2, before.1, 400));
    f.close().await;
    f.open().await;
    let replay = sync(&f, &auth, &epoch, batch(event(2, 400))).await;
    assert_eq!(replay.1["results"][0]["outcome"], "duplicate");
    assert_eq!(replay.1["next_device_sequence"], "3");
    let operator = format!("Bearer {OPERATOR}");
    let device_path = format!("/api/v2/devices/{device}");
    let current = f
        .call("GET", &device_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
    let revoked = f
        .call(
            "DELETE",
            &device_path,
            None,
            &[
                ("authorization", &operator),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    // A genuinely paired reserve administrator is necessary for this revocation.
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{:?}", revoked.body);
    let denied = sync(&f, &auth, &epoch, batch(event(3, 500))).await;
    assert_eq!(denied.0, StatusCode::UNAUTHORIZED);
    assert_eq!(state(&f).await, (before.0 + 2, before.1, 400));
    f.close().await;
}
