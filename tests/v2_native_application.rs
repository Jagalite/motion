//! Application-owned state survives real native activation. No fake native
//! runtime, media tool skip, or compatibility catalog authority is used.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::api;
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "support/native_delivery_changes.rs"]
mod native_delivery_changes;
#[path = "support/native_delivery_controls.rs"]
mod native_delivery_controls;
#[path = "support/native_generated_downloads.rs"]
mod native_generated_downloads;
#[path = "support/native_generated_playback.rs"]
mod native_generated_playback;
#[path = "support/native_processing_jobs.rs"]
mod native_processing_jobs;
#[path = "support/native_presentation.rs"]
mod native_presentation;
#[path = "support/native_retirement.rs"]
mod native_retirement;
#[path = "support/native_scoped_physical.rs"]
mod native_scoped_physical;
#[path = "support/native_scoped_renditions.rs"]
mod native_scoped_renditions;
#[path = "support/native_source_withdrawal.rs"]
mod native_source_withdrawal;

use playscale as playscale_app;
#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

#[path = "support/native_markers.rs"]
mod native_markers;

#[tokio::test]
async fn native_markers_use_current_container_facts_and_keep_manual_offline_history() {
    native_markers::exercise().await;
}

#[path = "support/native_continue.rs"]
mod native_continue;
#[path = "support/native_offline_sync.rs"]
mod native_offline_sync;

#[tokio::test]
async fn native_offline_sync_preserves_history_without_current_original_bytes() {
    native_offline_sync::exercise().await;
}

#[path = "support/native_access_admin.rs"]
mod native_access_admin;

#[tokio::test]
async fn native_access_administration_keeps_proof_cas_scope_and_revocation() {
    native_access_admin::exercise().await;
}

#[tokio::test]
async fn native_continue_watching_keeps_unavailable_history_under_current_scope() {
    native_continue::exercise().await;
}

async fn assert_generated_range(
    f: &Fixture,
    file: &str,
    revision: &str,
    auth: &str,
    expected: &[u8],
) {
    let response = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v2/media/files/{file}/content?revision={revision}"
                ))
                .header("host", "127.0.0.1:8787")
                .header("authorization", auth)
                .header("range", "bytes=0-15")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::PARTIAL_CONTENT,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(bytes.as_ref(), &expected[..16]);
}

#[tokio::test]
async fn native_preferences_preserve_scope_cas_and_restart_without_catalog_writes() {
    let mut f = Fixture::new().await;
    sqlx::query("INSERT INTO profiles(id,name) VALUES ('private','Private')")
        .execute(&f.app.db)
        .await
        .unwrap();
    // Credentials and grants exist before the source is sealed, as on an
    // actual migrated installation. Native metadata never supplies grants.
    let (device, token) = f
        .pair(&["default"], &["catalog:read", "viewing:write"])
        .await;
    let auth = format!("Bearer {token}");
    let (_, read_token) = f.pair(&["default"], &["catalog:read"]).await;
    let read_auth = format!("Bearer {read_token}");
    f.cutover().await;
    let path = "/api/v2/profiles/default/preferences";
    let original = f.call("GET", path, None, &[("authorization", &auth)]).await;
    assert_eq!(original.status, StatusCode::OK, "{:?}", original.body);
    let old_tag = original.headers["etag"].to_str().unwrap();
    let forbidden = f
        .call(
            "PUT",
            path,
            Some(original.body.clone()),
            &[("authorization", &read_auth), ("if-match", old_tag)],
        )
        .await;
    assert_eq!(
        forbidden.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        forbidden.body
    );

    let mut document = original.body.clone();
    document["completion_percent"] = json!(75.0);
    document["audio_languages"] = json!(["en", "fr"]);
    let changed = f
        .call(
            "PUT",
            path,
            Some(document.clone()),
            &[("authorization", &auth), ("if-match", old_tag)],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
    assert_eq!(changed.body, document);
    let stale = f
        .call(
            "PUT",
            path,
            Some(document.clone()),
            &[("authorization", &auth), ("if-match", old_tag)],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    for hidden in ["private", "missing"] {
        let hidden_path = format!("/api/v2/profiles/{hidden}/preferences");
        let read = f
            .call("GET", &hidden_path, None, &[("authorization", &auth)])
            .await;
        assert_eq!(read.status, StatusCode::NOT_FOUND, "{:?}", read.body);
        let write = f
            .call(
                "PUT",
                &hidden_path,
                Some(document.clone()),
                &[("authorization", &auth), ("if-match", "\"r-0\"")],
            )
            .await;
        assert_eq!(write.status, StatusCode::NOT_FOUND, "{:?}", write.body);
    }
    // Application writes do not unseal native-authoritative catalog tables.
    assert!(
        sqlx::query("INSERT INTO items(id,title,kind) VALUES ('forged','Forged','movie')")
            .execute(&f.app.db)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM items")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    f.close().await;
    f.open().await;
    let reopened = f.call("GET", path, None, &[("authorization", &auth)]).await;
    assert_eq!(reopened.status, StatusCode::OK, "{:?}", reopened.body);
    assert_eq!(reopened.body, changed.body);
    assert_eq!(reopened.headers["etag"], changed.headers["etag"]);
    // Current Motion revocation still owns permission after native restart.
    sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
        .bind(&device)
        .execute(&f.app.db)
        .await
        .unwrap();
    let denied = f
        .call(
            "PUT",
            path,
            Some(document),
            &[
                ("authorization", &auth),
                ("if-match", changed.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED, "{:?}", denied.body);
    let stored: String = sqlx::query_scalar(
        "SELECT document_json FROM playback_preferences WHERE profile_id='default'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&stored).unwrap()["completion_percent"],
        json!(75.0)
    );
    f.close().await;
}

/// A real native binding with no admitted projection must never reinterpret
/// retained legacy rows as current native identity or as a byte grant.
#[tokio::test]
async fn bound_unprojected_catalog_refuses_legacy_viewing_without_rebinding_history() {
    let f = Fixture::new().await;
    f.seed_preserved_timeline("one.mp4", b"physical identity fixture bytes")
        .await;
    let auth = format!("Bearer {OPERATOR}");
    let path = "/api/v2/profiles/default/timelines/timeline/viewing";
    let legacy = f.call("GET", path, None, &[("authorization", &auth)]).await;
    assert_eq!(legacy.status, StatusCode::OK, "{:?}", legacy.body);
    assert_eq!(legacy.body["revision"], "4");
    let native = f._dir.path().join("native.sqlite3");
    motion_catalog_backend::initialize(native.clone())
        .await
        .unwrap();
    let backend = motion_catalog_backend::NativeCatalog::bind(
        f.app.db.clone(),
        native.clone(),
        motion_catalog_backend::inspect(native)
            .await
            .unwrap()
            .identity,
    )
    .await
    .unwrap();
    let denied = f.call("GET", path, None, &[("authorization", &auth)]).await;
    assert_eq!(
        denied.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        denied.body
    );
    let (server, epoch) = playscale::v2::system::server_identity(&f.app)
        .await
        .unwrap();
    let identity = format!("{server}:{epoch}");
    let write = f
        .call(
            "PUT",
            path,
            Some(json!({"manual_watched":true,"reset_position":false})),
            &[
                ("authorization", &auth),
                ("if-match", "\"r-4\""),
                ("x-motion-viewing-epoch", &identity),
            ],
        )
        .await;
    assert_eq!(
        write.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        write.body
    );
    let bytes = f
        .call(
            "GET",
            "/api/v2/media/files/file/content",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(
        bytes.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        bytes.body
    );
    let history: (i64, i64, i64, Option<bool>) = sqlx::query_as(
        "SELECT revision,manual_epoch,position_ms,manual_watched FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(history, (4, 2, 250, None));
    drop(backend);
}

/// The physical qualifier is deliberately exercised, not patched around. This
/// test must remain failing if the reference inventory cannot prove admission.
/// The text payload qualifies physical identity/history, not video decoding.
#[tokio::test]
async fn native_measured_timeline_preserves_history_then_refuses_replaced_source() {
    let mut f = Fixture::new().await;
    let source = f
        .seed_preserved_timeline("one.txt", b"original measured physical bytes")
        .await;
    let (_, limited) = f
        .pair(&["default"], &["catalog:read", "viewing:write"])
        .await;
    // Legacy structure triggers advance the work head while seeding. Preserve
    // the actual final source baseline, not its initial INSERT value.
    let source_work_revision: i64 =
        sqlx::query_scalar("SELECT catalog_revision FROM items WHERE id='work'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let source_timeline_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM timelines WHERE id='timeline'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    // Migration35 timeline_version_insert advances7 to8 on version creation.
    assert_eq!(source_timeline_revision, 8);
    let source_path = format!("/api/v2/sources/{source}");
    let source_before = f
        .call(
            "GET",
            &source_path,
            None,
            &[("authorization", &format!("Bearer {OPERATOR}"))],
        )
        .await;
    assert_eq!(
        source_before.status,
        StatusCode::OK,
        "{:?}",
        source_before.body
    );
    f.cutover_source(Some(&source)).await;
    let source_after = f
        .call(
            "GET",
            &source_path,
            None,
            &[("authorization", &format!("Bearer {OPERATOR}"))],
        )
        .await;
    assert_eq!(
        source_after.status,
        StatusCode::OK,
        "{:?}",
        source_after.body
    );
    for field in [
        "id",
        "revision",
        "binding_revision",
        "name",
        "root_path",
        "volume_identity",
        "exclusions",
    ] {
        assert!(
            !source_before.body[field].is_null(),
            "missing source field {field}"
        );
        assert_eq!(
            source_after.body[field], source_before.body[field],
            "source field {field} must retain exact activated ownership"
        );
    }
    let revisions: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT i.catalog_revision,e.revision,t.revision,v.revision FROM items i JOIN editions e ON e.item_id=i.id JOIN timelines t ON t.edition_id=e.id JOIN media_versions v ON v.timeline_id=t.id WHERE v.id='version'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        revisions,
        (source_work_revision, 5, source_timeline_revision, 9)
    );
    let auth = format!("Bearer {OPERATOR}");
    for path in [
        "/api/v2/catalog/items/work/editions",
        "/api/v2/catalog/items/work/timelines",
        "/api/v2/catalog/timelines/timeline",
        "/api/v2/catalog/timelines/timeline/versions",
    ] {
        let anonymous = f.call("GET", path, None, &[]).await;
        assert_eq!(
            anonymous.status,
            StatusCode::UNAUTHORIZED,
            "{path}: {:?}",
            anonymous.body
        );
        let detail = f.call("GET", path, None, &[("authorization", &auth)]).await;
        assert_eq!(detail.status, StatusCode::OK, "{path}: {:?}", detail.body);
        if path.ends_with("/editions") {
            assert_eq!(detail.body["items"][0]["id"], "edition");
            assert_eq!(detail.body["items"][0]["revision"], "5");
            assert_eq!(detail.body["items"][0]["timeline_ids"], json!(["timeline"]));
        } else if path.ends_with("/versions") {
            let version = &detail.body["items"][0];
            assert_eq!(version["id"], "version");
            assert_eq!(version["revision"], "9");
            assert_eq!(version["availability"], "available");
            assert_eq!(
                version["files"],
                json!([{"file":{"file_id":"file","file_revision":catabolic_core::sha256(b"original measured physical bytes")},"part":1,"start_ms":null,"end_ms":null}])
            );
        } else {
            let timeline = if path.ends_with("/timelines") {
                &detail.body["items"][0]
            } else {
                &detail.body
            };
            assert_eq!(timeline["id"], "timeline");
            assert_eq!(timeline["revision"], source_timeline_revision.to_string());
            assert_eq!(timeline["duration_ms"], 1000);
            assert_eq!(timeline["version_ids"], json!(["version"]));
        }
    }
    let initial = f.viewing_adapter("GET", &auth, None, None).await;
    assert_eq!(initial.status, StatusCode::OK, "{:?}", initial.body);
    assert_eq!(initial.body["timeline_id"], "timeline");
    assert_eq!(initial.body["revision"], "4");
    assert_eq!(initial.body["manual_epoch"], "2");
    assert_eq!(initial.body["position_ms"], 250);
    assert_eq!(initial.body["watched"], false);
    let head = f.viewing_adapter("HEAD", &auth, None, None).await;
    assert_eq!(head.status, StatusCode::OK, "{:?}", head.body);
    assert_eq!(head.body, Value::Null);
    assert_eq!(head.headers["etag"], initial.headers["etag"]);
    let unauthenticated = f
        .call(
            "GET",
            "/api/v2/profiles/default/timelines/timeline/viewing",
            None,
            &[],
        )
        .await;
    assert_eq!(
        unauthenticated.status,
        StatusCode::UNAUTHORIZED,
        "{:?}",
        unauthenticated.body
    );
    let unauthenticated_head = f
        .call(
            "HEAD",
            "/api/v2/profiles/default/timelines/timeline/viewing",
            None,
            &[],
        )
        .await;
    assert_eq!(unauthenticated_head.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthenticated_head.body, Value::Null);
    let blocked_sibling = f
        .call(
            "POST",
            "/api/v2/playback/delivery-sessions/closed/unsupported",
            Some(json!({})),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(
        blocked_sibling.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{:?}",
        blocked_sibling.body
    );
    let event_count: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    for _ in 0..2 {
        let unchanged = f.viewing_adapter("GET", &auth, None, None).await;
        assert_eq!(unchanged.status, StatusCode::OK, "{:?}", unchanged.body);
        assert_eq!(unchanged.body, initial.body);
        assert_eq!(unchanged.headers["etag"], initial.headers["etag"]);
    }
    let after_reads: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        after_reads, event_count,
        "unchanged native projection reads must not publish spurious catalog changes"
    );
    let restricted = f
        .viewing_adapter("GET", &format!("Bearer {limited}"), None, None)
        .await;
    assert_eq!(
        restricted.status,
        StatusCode::NOT_FOUND,
        "{:?}",
        restricted.body
    );
    let changed = f
        .viewing_adapter(
            "PUT",
            &auth,
            Some(json!({"manual_watched":true,"reset_position":false})),
            Some("\"r-4\""),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
    assert_eq!(changed.body["revision"], "5");
    assert_eq!(changed.body["manual_epoch"], "3");
    assert_eq!(changed.body["position_ms"], 250);
    assert_eq!(changed.body["watched"], true);
    let stale = f
        .viewing_adapter(
            "PUT",
            &auth,
            Some(json!({"manual_watched":false,"reset_position":true})),
            Some("\"r-4\""),
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    f.close().await;
    f.open().await;
    let source_reopened = f
        .call("GET", &source_path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(
        source_reopened.status,
        StatusCode::OK,
        "{:?}",
        source_reopened.body
    );
    assert_eq!(source_reopened.body, source_after.body);
    let reopened = f.viewing_adapter("GET", &auth, None, None).await;
    assert_eq!(reopened.status, StatusCode::OK, "{:?}", reopened.body);
    assert_eq!(reopened.body, changed.body);
    assert_eq!(reopened.headers["etag"], changed.headers["etag"]);

    // The actual production dispatcher observes replacement. No stale source
    // claim or synthetic native grant is used to mark the old reference gone.
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
    std::fs::write(
        f._dir.path().join("media/one.txt"),
        b"replacement has different exact physical identity",
    )
    .unwrap();
    playscale::native_discovery::scan(&f.app, &source, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let denied = f.viewing_adapter("GET", &auth, None, None).await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND, "{:?}", denied.body);
    let write_denied = f
        .viewing_adapter(
            "PUT",
            &auth,
            Some(json!({"manual_watched":false,"reset_position":true})),
            Some("\"r-5\""),
        )
        .await;
    assert_eq!(
        write_denied.status,
        StatusCode::NOT_FOUND,
        "{:?}",
        write_denied.body
    );
    let history: (i64, i64, i64, bool) = sqlx::query_as(
        "SELECT revision,manual_epoch,position_ms,manual_watched FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(history, (5, 3, 250, true));
    let proof: (bool, bool) = sqlx::query_as(
        "SELECT f.available,v.ready FROM catabolic_projection_files f JOIN catabolic_projection_versions v ON v.version_id='version' WHERE f.file_id='file'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(proof, (false, false));
    f.close().await;
}

/// Real MP4 bytes are scanned by the packaged native observer, imported through
/// normal admission, and pinned by the unchanged production download policy.
/// Complete public HTTP qualification, including the exact finite native gates.
#[tokio::test]
async fn native_download_pin_survives_restart_and_expires_on_missing_occurrence() {
    let mut f = Fixture::new().await;
    let generated = f._dir.path().join("generated.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=32x32:r=10",
            "-t",
            "1",
            "-c:v",
            "mpeg4",
            "-y",
        ])
        .arg(&generated)
        .output()
        .expect("ffmpeg is required for physical integration qualification");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(&generated).unwrap();
    let source = f.seed_preserved_timeline("one.mp4", &bytes).await;
    let (device, token) = f
        .pair(
            &["default"],
            &[
                "catalog:read",
                "downloads:manage",
                "viewing:write",
                "system:admin",
            ],
        )
        .await;
    let (_, other_token) = f
        .pair(
            &["default"],
            &["catalog:read", "downloads:manage", "system:admin"],
        )
        .await;
    let (restricted_device, restricted_token) = f
        .pair(&["default"], &["catalog:read", "downloads:manage"])
        .await;
    let restricted_auth = format!("Bearer {restricted_token}");
    let auth = format!("Bearer {token}");
    let source_timeline_revision: i64 =
        sqlx::query_scalar("SELECT revision FROM timelines WHERE id='timeline'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(source_timeline_revision, 8);
    let request = json!({"device_id":device,"profile_id":"default","timeline_id":"timeline","version_id":"version","recipe_id":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}});
    let historical = f
        .download_adapter(
            "POST",
            "/downloads",
            &auth,
            Some(request.clone()),
            Some("historical-download-request"),
        )
        .await;
    assert_eq!(
        historical.status,
        StatusCode::ACCEPTED,
        "{:?}",
        historical.body
    );
    let historical_id = historical.body["id"].as_str().unwrap().to_owned();
    let historical_path = format!("/downloads/{historical_id}");
    let old_pin: (String,i64,String,String,String) = sqlx::query_as("SELECT timeline_id,timeline_revision,version_id,file_id,file_revision FROM downloads WHERE id=?")
        .bind(&historical_id).fetch_one(&f.app.db).await.unwrap();
    f.cutover_source(Some(&source)).await;
    let unauthenticated_create = f
        .download_adapter(
            "POST",
            "/downloads",
            "",
            Some(request.clone()),
            Some("unauthenticated-download"),
        )
        .await;
    assert_eq!(
        unauthenticated_create.status,
        StatusCode::UNAUTHORIZED,
        "{:?}",
        unauthenticated_create.body
    );
    let restricted_create = f.download_adapter("POST", "/downloads", &restricted_auth, Some(json!({"device_id":"wrong-device","profile_id":"default","timeline_id":"timeline","version_id":"version","recipe_id":null,"tracks":{"subtitle_policy":"off"}})), Some("restricted-download")).await;
    // Device ownership is checked before scope; foreign device requests cannot
    // use native catalog qualification to expand authority.
    assert_eq!(
        restricted_create.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        restricted_create.body
    );
    let mut own_restricted_request = request.clone();
    own_restricted_request["device_id"] = json!(restricted_device);
    let own_restricted = f
        .download_adapter(
            "POST",
            "/downloads",
            &restricted_auth,
            Some(own_restricted_request),
            Some("restricted-own-download"),
        )
        .await;
    assert_eq!(
        own_restricted.status,
        StatusCode::NOT_FOUND,
        "{:?}",
        own_restricted.body
    );
    let unauthenticated_read = f
        .download_adapter("GET", &historical_path, "", None, None)
        .await;
    assert_eq!(
        unauthenticated_read.status,
        StatusCode::UNAUTHORIZED,
        "{:?}",
        unauthenticated_read.body
    );
    let historical_read = f
        .download_adapter("GET", &historical_path, &auth, None, None)
        .await;
    assert_eq!(
        historical_read.status,
        StatusCode::OK,
        "{:?}",
        historical_read.body
    );
    assert_eq!(historical_read.body["state"], "ready");
    let historical_replay = f
        .download_adapter(
            "POST",
            "/downloads",
            &auth,
            Some(request.clone()),
            Some("historical-download-request"),
        )
        .await;
    assert_eq!(
        historical_replay.status,
        StatusCode::ACCEPTED,
        "{:?}",
        historical_replay.body
    );
    assert_eq!(historical_replay.body["id"], historical_id);
    assert_eq!(historical_replay.headers["idempotent-replayed"], "true");
    // Explicit native subtitle requests must fail before pin/receipt/artifact
    // creation. This tests truthful admission, not subtitle codec support.
    let before_subtitle: (i64,i64,i64,i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM downloads),(SELECT count(*) FROM idempotency_records),         (SELECT count(*) FROM download_subtitle_artifacts),(SELECT count(*) FROM content_tickets)")
        .fetch_one(&f.app.db).await.unwrap();
    let before_subtitle_view: (i64,i64,i64) = sqlx::query_as(
        "SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'")
        .fetch_one(&f.app.db).await.unwrap();
    let mut subtitle_request = request.clone();
    subtitle_request["tracks"]["subtitle_track_id"] = json!("s0");
    subtitle_request["tracks"]["subtitle_policy"] = json!("off");
    let invalid_subtitle = f
        .download_adapter(
            "POST",
            "/downloads",
            &auth,
            Some(subtitle_request.clone()),
            Some("native-original-subtitle-invalid"),
        )
        .await;
    assert_eq!(invalid_subtitle.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid_subtitle.body["code"], "download_tracks_unavailable");
    subtitle_request["tracks"]["subtitle_policy"] = json!("require");
    let unsupported_subtitle = f
        .download_adapter(
            "POST",
            "/downloads",
            &auth,
            Some(subtitle_request),
            Some("native-original-subtitle-refusal"),
        )
        .await;
    assert_eq!(unsupported_subtitle.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        unsupported_subtitle.body["code"],
        "native_download_selection_unavailable"
    );
    let after_subtitle: (i64,i64,i64,i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM downloads),(SELECT count(*) FROM idempotency_records),         (SELECT count(*) FROM download_subtitle_artifacts),(SELECT count(*) FROM content_tickets)")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(after_subtitle, before_subtitle);
    let after_subtitle_view: (i64,i64,i64) = sqlx::query_as(
        "SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(after_subtitle_view, before_subtitle_view);
    let created = f
        .download_adapter(
            "POST",
            "/downloads",
            &auth,
            Some(request.clone()),
            Some("native-download-request"),
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    assert_eq!(created.body["state"], "ready");
    let id = created.body["id"].as_str().unwrap();
    let path = format!("/downloads/{id}");
    let pin: (String,i64,String,String,String) = sqlx::query_as("SELECT timeline_id,timeline_revision,version_id,file_id,file_revision FROM downloads WHERE id=?")
        .bind(id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        pin,
        (
            "timeline".into(),
            source_timeline_revision,
            "version".into(),
            "file".into(),
            catabolic_core::sha256(&bytes)
        )
    );
    let replay = f
        .download_adapter(
            "POST",
            "/downloads",
            &auth,
            Some(request),
            Some("native-download-request"),
        )
        .await;
    assert_eq!(replay.status, StatusCode::ACCEPTED, "{:?}", replay.body);
    assert_eq!(replay.body["id"], id);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let foreign = f
        .download_adapter("GET", &path, &format!("Bearer {other_token}"), None, None)
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{:?}", foreign.body);
    f.close().await;
    f.open().await;
    let read = f.download_adapter("GET", &path, &auth, None, None).await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
    assert_eq!(read.body["state"], "ready");
    native_generated_downloads::assert_listed(&f, &auth, id, "ready").await;
    let (range_status, range_bytes) = f.content_adapter(&auth, &pin.4).await;
    assert_eq!(
        range_status,
        StatusCode::PARTIAL_CONTENT,
        "{}",
        String::from_utf8_lossy(&range_bytes)
    );
    assert_eq!(range_bytes, bytes[..16]);
    // Qualify the public native media-only manifest. Optional unsupported
    // legacy enrichment is absent.
    let manifest = f.manifest_adapter(id, &auth).await;
    assert_eq!(manifest.status, StatusCode::OK, "{:?}", manifest.body);
    assert_eq!(manifest.body["title"], "Work");
    assert_eq!(
        manifest.body["timeline_revision"],
        source_timeline_revision.to_string()
    );
    assert_eq!(manifest.body["files"].as_array().unwrap().len(), 1);
    assert_eq!(
        manifest.body["files"][0]["digest"],
        format!("sha256:{}", pin.4)
    );
    assert_eq!(manifest.body["files"][0]["role"], "media");
    assert!(manifest.body.get("metadata").is_none());
    let media_url = manifest.body["files"][0]["media_url"].as_str().unwrap();
    let served = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(media_url)
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .header("range", "bytes=0-15")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(served.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        served
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &bytes[..16]
    );
    let anonymous_manifest = f.manifest_adapter(id, "").await;
    assert_eq!(anonymous_manifest.status, StatusCode::UNAUTHORIZED);
    let foreign_manifest = f
        .manifest_adapter(id, &format!("Bearer {other_token}"))
        .await;
    assert_eq!(foreign_manifest.status, StatusCode::NOT_FOUND);

    let (unauthenticated_status, _) = f.content_adapter("", &pin.4).await;
    assert_eq!(unauthenticated_status, StatusCode::UNAUTHORIZED);
    let (restricted_status, _) = f.content_adapter(&restricted_auth, &pin.4).await;
    assert_eq!(restricted_status, StatusCode::NOT_FOUND);
    for (method, path) in [
        ("HEAD", "/api/v2/downloads".to_owned()),
        ("HEAD", format!("/api/v2/downloads/{id}/manifest")),
        ("HEAD", "/api/v2/media/files/file/content".to_owned()),
    ] {
        let sibling = f
            .call(method, &path, None, &[("authorization", &auth)])
            .await;
        assert_eq!(
            sibling.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {path}: {:?}",
            sibling.body
        );
    }

    let (wrong_status, _) = f.content_adapter(&auth, &"0".repeat(64)).await;
    assert_eq!(wrong_status, StatusCode::CONFLICT);
    native_scoped_physical::exercise(&mut f, &source, &bytes).await;
    // The scoped fixture deliberately performs one authorized manual-watched
    // override. Subsequent disappearance must preserve that new history, not
    // restore the seed that preceded the user's write.
    let history_after_scope: (i64, i64, i64) = sqlx::query_as(
        "SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE timeline_id='timeline'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(history_after_scope, (5, 3, 250));
    // Native ExactFileRevision deliberately omits ctime. An in-place rewrite
    // with original size/mtime must still fail the actual descriptor check,
    // even before a fresh native observation exists.
    let media_path = f._dir.path().join("media/one.mp4");
    let before = std::fs::metadata(&media_path).unwrap();
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&media_path)
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
    }
    let after = std::fs::metadata(&media_path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_ne!(
        playscale::scan::fingerprint(&before),
        playscale::scan::fingerprint(&after)
    );
    let (ctime_status, _) = f.content_adapter(&auth, &pin.4).await;
    assert_eq!(ctime_status, StatusCode::CONFLICT);
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
    std::fs::remove_file(f._dir.path().join("media/one.mp4")).unwrap();
    playscale::native_discovery::scan(&f.app, &source, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let expired = f.download_adapter("GET", &path, &auth, None, None).await;
    assert_eq!(expired.status, StatusCode::OK, "{:?}", expired.body);
    assert_eq!(expired.body["state"], "expired");
    let expired_manifest = f.manifest_adapter(id, &auth).await;
    assert_eq!(
        expired_manifest.status,
        StatusCode::CONFLICT,
        "{:?}",
        expired_manifest.body
    );
    assert_eq!(expired_manifest.body["code"], "download_expired");

    let (missing_status, _) = f.content_adapter(&auth, &pin.4).await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    let preserved: (String,i64,String,String,String) = sqlx::query_as("SELECT timeline_id,timeline_revision,version_id,file_id,file_revision FROM downloads WHERE id=?")
        .bind(id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        preserved, pin,
        "history must never rebind to replacement media"
    );
    let historical_expired = f
        .download_adapter("GET", &historical_path, &auth, None, None)
        .await;
    assert_eq!(
        historical_expired.status,
        StatusCode::OK,
        "{:?}",
        historical_expired.body
    );
    assert_eq!(historical_expired.body["state"], "expired");
    let old_preserved: (String,i64,String,String,String) = sqlx::query_as("SELECT timeline_id,timeline_revision,version_id,file_id,file_revision FROM downloads WHERE id=?")
        .bind(&historical_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        old_preserved, old_pin,
        "pre-cutover application references must not silently rebind"
    );
    let history: (i64,i64,i64) = sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE timeline_id='timeline'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(history, history_after_scope);
    f.close().await;
}

/// An offline editorial command enters through the real committed-cutover
/// reconciliation, never a direct Store reopen or invented catalog grant.
#[tokio::test]
async fn reconciled_native_timeline_revision_preserves_history_without_rebinding() {
    edited_timeline_fixture(false).await;
}

#[tokio::test]
async fn native_shorter_slice_retains_history_expires_pin_and_core_start_clamps() {
    edited_timeline_fixture(true).await;
}

async fn edited_timeline_fixture(shorter: bool) {
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
    use catabolic_core::catalog_identity::{EquivalenceBasis, EquivalenceEvidence};
    use catabolic_store::catalog_commands::{ReceiptPolicy, TrustedCommandAuthority};
    use catabolic_store::catalog_identity_mutation::{
        EquivalenceClaim, IdentityAuthority, IdentityEvidence, IdentityMutation,
    };
    let mut f = Fixture::new().await;
    let source = if shorter {
        let media = f._dir.path().join("editorial.mp4");
        let generated = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=32x32:r=10",
                "-t",
                "1",
                "-c:v",
                "mpeg4",
                "-y",
            ])
            .arg(&media)
            .output()
            .expect("ffmpeg required for exact slice fixture");
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        f.seed_preserved_timeline("one.mp4", &std::fs::read(media).unwrap())
            .await
    } else {
        f.seed_preserved_timeline("one.txt", b"exact native editorial source")
            .await
    };
    let original_position = if shorter { 750 } else { 250 };
    if shorter {
        sqlx::query("UPDATE timeline_viewing SET position_ms=750 WHERE timeline_id='timeline'")
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    let download_owner = if shorter {
        Some(
            f.pair(
                &["default"],
                &["catalog:read", "downloads:manage", "system:admin"],
            )
            .await,
        )
    } else {
        None
    };
    f.cutover_source(Some(&source)).await;
    let prior_download = if let Some((device, token)) = download_owner {
        let auth = format!("Bearer {token}");
        let created = f.download_adapter("POST", "/downloads", &auth, Some(json!({"device_id":device,"profile_id":"default","timeline_id":"timeline","version_id":"version","recipe_id":null,"tracks":{"subtitle_policy":"off"}})), Some("before-shorter-slice")).await;
        assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
        Some((created.body["id"].as_str().unwrap().to_owned(), auth))
    } else {
        None
    };
    let auth = format!("Bearer {OPERATOR}");
    let initial = f.viewing_adapter("GET", &auth, None, None).await;
    assert_eq!(initial.status, StatusCode::OK, "{:?}", initial.body);
    let old_timeline: i64 =
        sqlx::query_scalar("SELECT revision FROM timelines WHERE id='timeline'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let snapshots: Vec<String> = sqlx::query_scalar(
        "SELECT snapshot_json FROM catabolic_import_snapshots ORDER BY import_id",
    )
    .fetch_all(&f.app.db)
    .await
    .unwrap();
    let mut mutation: IdentityMutation = snapshots
        .iter()
        .find_map(|raw| {
            let snapshot: Value = serde_json::from_str(raw).unwrap();
            snapshot["batches"].as_array()?.iter().find_map(|batch| {
                batch["typed"]["native"]["mutations"]
                    .as_array()?
                    .iter()
                    .find_map(|value| {
                        let mutation: IdentityMutation =
                            serde_json::from_value(value.clone()).ok()?;
                        (mutation.context.context_key == "work").then_some(mutation)
                    })
            })
        })
        .expect("trusted exporter retained the exact typed identity command");
    assert_eq!(mutation.versions.len(), 1);
    mutation.context.expected_revision = Some(1);
    for timeline in &mut mutation.timelines {
        timeline.expected_revision = Some(1);
    }
    for version in &mut mutation.versions {
        version.expected_revision = Some(1);
        version.expected_timeline_revision = 2;
        for revision in version.expected_binding_revisions.values_mut() {
            *revision = Some(1);
        }
        version.equivalence = Some(EquivalenceEvidence {
            basis: EquivalenceBasis::Declared,
            evidence_id: "fixture-reviewed-same-extent".into(),
        });
    }
    mutation.evidence = vec![IdentityEvidence {
        id: "fixture-reviewed-same-extent".into(),
        basis: EquivalenceBasis::Declared,
        claims: mutation
            .versions
            .iter()
            .map(|v| EquivalenceClaim {
                edition_item_id: v.version.edition_item_id.clone(),
                timeline_id: v.version.timeline_id.clone(),
                timeline_revision: 2,
                version_id: v.version.version_id.clone(),
                version_revision: 2,
                exact_files: v.version.bindings.iter().map(|b| b.file.clone()).collect(),
            })
            .collect(),
        evidence: json!({"source":"explicit offline fixture editorial review","decision":"same exact extent"}),
    }];
    f.close().await;
    let fence = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let mut reconciled = motion_catalog_backend::cutover::open_server(
        f.app.db.clone(),
        f._dir.path().join("native.sqlite3"),
        "default".into(),
    )
    .await
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let _fence = fence;
        let host = reconciled.admission.admitted_host().unwrap();
        let identity = host.identity().unwrap();
        let generation = host.catalog_generation().unwrap();
        // Import rewrites source association IDs into native item_files IDs.
        // Obtain those exact retained bindings from the admitted native owner,
        // not the pre-import snapshot or a guessed association naming rule.
        let ids = mutation
            .versions
            .iter()
            .map(|v| v.version.version_id.clone())
            .collect::<Vec<_>>();
        let current_manifest = host.projection_version_manifest(generation, &ids).unwrap();
        for change in &mut mutation.versions {
            let current = current_manifest["manifest"]["versions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["version"]["version_id"] == change.version.version_id)
                .unwrap()["version"]
                .clone();
            let mut original = serde_json::to_value(&change.version).unwrap();
            for binding in original["bindings"].as_array_mut().unwrap() {
                let retained = current["bindings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|b| b["binding_id"] == binding["binding_id"])
                    .unwrap();
                binding["association_id"] = retained["association_id"].clone();
            }
            assert_eq!(
                original, current,
                "only the imported association identity translation may differ"
            );
            change.version = serde_json::from_value(current).unwrap();
        }
        if shorter {
            let extent = catabolic_core::catalog_identity::IntervalUs::new(0, 500_000).unwrap();
            for timeline in &mut mutation.timelines { timeline.coverage = Some(extent); }
            for change in &mut mutation.versions {
                change.version.declared_coverage = Some(extent);
                assert_eq!(change.version.bindings.len(), 1);
                change.version.bindings[0].source = catabolic_core::catalog_identity::SourceExtent::Slice { interval: extent };
                change.version.bindings[0].timeline_interval = Some(extent);
            }
            mutation.evidence[0].evidence = json!({"source":"explicit offline fixture editorial review","decision":"shorten exact source slice to500ms"});
        }
        let namespace = host.authorization().unwrap().namespace().to_owned();
        assert_eq!(mutation.context.authority_namespace, namespace);
        let scope = IdentityAuthority {
            authority_namespace: namespace.clone(),
            context_keys: ["work".into()].into_iter().collect(),
            allow_declared_evidence: true,
            allow_verified_evidence: false,
        };
        let payload = json!({"operation":"identity","mutation":mutation});
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(
                identity["catalog_id"].as_str().unwrap(),
                &namespace,
                "fixture-same-timeline-edit",
            )
            .unwrap(),
            identity["catalog_incarnation"].as_str().unwrap(),
            generation,
            &payload.to_string(),
        )
        .unwrap();
        let outcome = host
            .execute_command(
                &command,
                TrustedCommandAuthority {
                    host_namespace: &namespace,
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: true,
                    identity_authority: Some(&scope),
                },
                ReceiptPolicy {
                    retention: std::time::Duration::from_secs(3600),
                    max_receipts: 1000,
                    max_logical_bytes: 16 * 1024 * 1024,
                    max_hold_records: 1000,
                },
            )
            .unwrap();
        assert_eq!(outcome.code(), "committed");
        host.begin_drain();
        while !host.drain(std::time::Duration::from_millis(50)).unwrap() {}
    })
    .await
    .unwrap();
    f.open().await;
    // Current facts project mapped revisions, not historical application state.
    let current: (i64,i64,i64) = sqlx::query_as("SELECT t.revision,t.duration_ms,v.revision FROM timelines t JOIN media_versions v ON v.timeline_id=t.id WHERE t.id='timeline'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        current,
        (old_timeline + 1, if shorter { 500 } else { 1000 }, 10)
    );
    let history: (i64,i64,i64,bool) = sqlx::query_as("SELECT revision,manual_epoch,position_ms,automatic_watched FROM timeline_viewing WHERE timeline_id='timeline'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(history, (4, 2, original_position, false));
    if let Some((download, auth)) = prior_download {
        let extent: (Option<i64>,Option<i64>,bool) = sqlx::query_as("SELECT b.start_ms,b.end_ms,p.ready FROM version_files b JOIN catabolic_projection_versions p ON p.version_id=b.version_id WHERE b.version_id='version'").fetch_one(&f.app.db).await.unwrap();
        assert_eq!(
            extent,
            (Some(0), Some(500), true),
            "only exact current typed bounds become serving projection"
        );
        let expired = f
            .download_adapter("GET", &format!("/downloads/{download}"), &auth, None, None)
            .await;
        assert_eq!(expired.status, StatusCode::OK, "{:?}", expired.body);
        assert_eq!(expired.body["state"], "expired");
        let pinned_revision: i64 =
            sqlx::query_scalar("SELECT timeline_revision FROM downloads WHERE id=?")
                .bind(&download)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        assert_eq!(pinned_revision, old_timeline);
        // This executes the production reducer against real retained state and
        // current native-projected duration; it is not a fabricated live delivery
        // or a claim that playback HTTP admission has been qualified.
        let retained = playscale_core::timeline_viewing::View {
            revision: history.0 as u64,
            manual_epoch: history.1 as u64,
            position_ms: history.2 as u64,
            automatic_watched: history.3,
            manual_watched: None,
            session: None,
        };
        let started = playscale_core::timeline_viewing::start(
            &retained,
            retained.revision,
            "explicit-new-session".into(),
            "current-exact-timeline-delivery".into(),
            Some(current.1 as u64),
        )
        .unwrap();
        assert_eq!(started.view.position_ms, 500);
        assert_eq!(started.view.revision, 5);
        assert_eq!(started.view.manual_epoch, 2);
        assert_eq!(retained.position_ms, 750);
        assert_eq!(started.view.watched(), retained.watched());
    }
    // Whole originals use exact current typed facts; edited slices remain
    // behind the separate HTTP delivery qualification boundary.
    let before_reads: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    for _ in 0..2 {
        let bounded = f.viewing_adapter("GET", &auth, None, None).await;
        assert_eq!(
            bounded.status,
            if shorter {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            },
            "{:?}",
            bounded.body
        );
    }
    let after_reads: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(after_reads, before_reads);
    let unchanged: (i64,i64,i64) = sqlx::query_as("SELECT t.revision,t.duration_ms,v.revision FROM timelines t JOIN media_versions v ON v.timeline_id=t.id WHERE t.id='timeline'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(unchanged, current);
    let retained: (i64,i64,i64,bool) = sqlx::query_as("SELECT revision,manual_epoch,position_ms,automatic_watched FROM timeline_viewing WHERE timeline_id='timeline'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(retained, history);
    f.close().await;
}

#[tokio::test]
async fn native_library_scope_uses_current_physical_membership_without_rating_grants() {
    let mut f = Fixture::new().await;
    let library = f
        .seed_preserved_timeline("one.txt", b"scoped native catalog source")
        .await;
    // A real empty logical definition is exported and resolved natively. It is
    // not an invented policy ID or fabricated physical source membership.
    sqlx::query(
        "INSERT INTO catalog_libraries(id,name,kind) VALUES('other-library','Other','mixed')",
    )
    .execute(&f.app.db)
    .await
    .unwrap();
    let permissions = ["catalog:read", "downloads:manage"];
    let mut tokens = Vec::new();
    let mut devices = Vec::new();
    for (libraries, allow_unrated, blocked) in [
        (vec![library.clone()], true, Vec::<String>::new()),
        (vec!["other-library".to_owned()], true, vec![]),
        (vec![library.clone()], false, vec![]),
        (
            vec![library.clone()],
            true,
            vec!["restricted-label".to_owned()],
        ),
    ] {
        let (device, token) = f.pair(&["default"], &permissions).await;
        let row = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &format!("Bearer {OPERATOR}"))],
            )
            .await;
        assert_eq!(row.status, StatusCode::OK, "{:?}", row.body);
        let policy = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({"library_ids":libraries,"allow_unrated":allow_unrated,"allowed_ratings":[],"blocked_labels":blocked,"permissions":permissions})), &[("authorization", &format!("Bearer {OPERATOR}")), ("if-match", row.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
        tokens.push(format!("Bearer {token}"));
        devices.push(device);
    }
    f.cutover_source(Some(&library)).await;
    for path in [
        "/api/v2/catalog/items/work/editions",
        "/api/v2/catalog/items/work/timelines",
        "/api/v2/catalog/timelines/timeline",
        "/api/v2/catalog/timelines/timeline/versions",
    ] {
        let detail = f
            .call("GET", path, None, &[("authorization", &tokens[0])])
            .await;
        assert_eq!(
            detail.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}: {:?}",
            detail.body
        );
        assert_eq!(detail.body["code"], "native_scope_unavailable");
    }
    // Policy changes resolve current native definitions, not frozen source rows.
    let path = format!("/api/v2/devices/{}/policy", devices[0]);
    let device_path = format!("/api/v2/devices/{}", devices[0]);
    let operator = format!("Bearer {OPERATOR}");
    let row = f
        .call("GET", &device_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(row.status, StatusCode::OK);
    let tag = row.headers["etag"].to_str().unwrap().to_owned();
    let policy = json!({"library_ids":[library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions});
    let anonymous = f
        .call("PUT", &path, Some(policy.clone()), &[("if-match", &tag)])
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let forbidden = f
        .call(
            "PUT",
            &path,
            Some(policy.clone()),
            &[("authorization", &tokens[0]), ("if-match", &tag)],
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    let unchanged = f
        .call(
            "PUT",
            &path,
            Some(policy.clone()),
            &[("authorization", &operator), ("if-match", &tag)],
        )
        .await;
    assert_eq!(unchanged.status, StatusCode::OK, "{:?}", unchanged.body);
    assert_eq!(unchanged.headers["etag"], tag);
    let stale = f
        .call(
            "PUT",
            &path,
            Some(policy.clone()),
            &[("authorization", &operator), ("if-match", "\"0\"")],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    let mut unknown = policy;
    unknown["library_ids"] = json!(["missing-native-library"]);
    let denied = f
        .call(
            "PUT",
            &path,
            Some(unknown),
            &[("authorization", &operator), ("if-match", &tag)],
        )
        .await;
    assert_eq!(denied.body["code"], "unknown_library", "{:?}", denied.body);
    let after = f
        .call("GET", &device_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(after.headers["etag"], tag);

    let allowed = f
        .call(
            "GET",
            "/api/v2/catalog/items/work",
            None,
            &[("authorization", &tokens[0])],
        )
        .await;
    assert_eq!(allowed.status, StatusCode::OK, "{:?}", allowed.body);
    assert_eq!(allowed.body["id"], "work");
    assert_eq!(allowed.body["library_ids"], json!([library]));
    let browse = f
        .call(
            "GET",
            "/api/v2/catalog/items",
            None,
            &[("authorization", &tokens[0])],
        )
        .await;
    assert_eq!(browse.status, StatusCode::OK, "{:?}", browse.body);
    assert_eq!(browse.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(browse.body["items"][0]["id"], "work");
    let hidden = f
        .call(
            "GET",
            "/api/v2/catalog/items/work",
            None,
            &[("authorization", &tokens[1])],
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{:?}", hidden.body);
    let empty = f
        .call(
            "GET",
            "/api/v2/catalog/items",
            None,
            &[("authorization", &tokens[1])],
        )
        .await;
    assert_eq!(empty.status, StatusCode::OK, "{:?}", empty.body);
    assert_eq!(empty.body["items"], json!([]));
    for token in &tokens[2..] {
        let constrained = f
            .call(
                "GET",
                "/api/v2/catalog/items/work",
                None,
                &[("authorization", token)],
            )
            .await;
        assert_eq!(
            constrained.status,
            StatusCode::NOT_FOUND,
            "{:?}",
            constrained.body
        );
        let constrained_browse = f
            .call(
                "GET",
                "/api/v2/catalog/items",
                None,
                &[("authorization", token)],
            )
            .await;
        assert_eq!(
            constrained_browse.status,
            StatusCode::OK,
            "{:?}",
            constrained_browse.body
        );
        assert_eq!(constrained_browse.body["items"], json!([]));
    }
    let revision: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id='file'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let (physical, body) = f.content_adapter(&tokens[0], &revision).await;
    assert_eq!(physical, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, &b"scoped native catalog source"[..16]);
    let (wrong_library, _) = f.content_adapter(&tokens[1], &revision).await;
    assert_eq!(wrong_library, StatusCode::NOT_FOUND);
    for token in &tokens[2..] {
        let (constrained, _) = f.content_adapter(token, &revision).await;
        assert_eq!(constrained, StatusCode::SERVICE_UNAVAILABLE);
    }
    f.close().await;
}

#[tokio::test]
async fn native_faststart_original_plan_delivery_session_and_ticket_use_real_observation() {
    native_original_renderer_fixture(0).await;
}

#[tokio::test]
async fn native_generated_receipt_survives_application_ack_storage_failure_without_rerender() {
    native_original_renderer_fixture(1).await;
}

#[tokio::test]
async fn native_generated_receipt_ack_preserves_retired_edition_history_without_rebinding() {
    native_original_renderer_fixture(2).await;
}

async fn native_original_renderer_fixture(recovery_case: u8) {
    let fail_first_ack = recovery_case != 0;
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_test_writer()
        .try_init();
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("compatible");
    std::fs::create_dir(&root).unwrap();
    let media = root.join("original.mp4");
    let generated = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            "-y",
        ])
        .arg(&media)
        .output()
        .expect("ffmpeg with libx264/AAC required for compatible original fixture");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let source_bytes = std::fs::read(&media).unwrap();
    let (_, token) = f
        .pair(
            &["default"],
            &[
                "catalog:read",
                "playback:request",
                "viewing:write",
                "system:admin",
                "processing:request",
            ],
        )
        .await;
    let (_, other) = f
        .pair(
            &["default"],
            &[
                "catalog:read",
                "playback:request",
                "viewing:write",
                "system:admin",
            ],
        )
        .await;
    let (_, no_processing) = f.pair(&["default"], &["catalog:read"]).await;
    let no_processing_auth = format!("Bearer {no_processing}");
    let auth = format!("Bearer {token}");
    let other_auth = format!("Bearer {other}");
    f.cutover().await;
    let capabilities = f
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(
        capabilities.status,
        StatusCode::OK,
        "{:?}",
        capabilities.body
    );
    let features = capabilities.body["features"].as_array().unwrap();
    let playback = features.iter().find(|f| f["id"] == "playback.v2").unwrap();
    assert_eq!(playback["implemented"], true);
    assert_eq!(playback["enabled"], true);
    assert_eq!(playback["limits"]["routes"], "original,prepared");
    assert_eq!(playback["limits"]["timeline_binding"], "whole");
    assert_eq!(playback["limits"]["subtitles"], "off");
    assert_eq!(playback["limits"]["adaptive_bitrate"], "disabled");
    let sources = features
        .iter()
        .find(|f| f["id"] == "catalog.native_source_bindings")
        .unwrap();
    assert!(
        sources["limits"]["operations"]
            .as_str()
            .unwrap()
            .contains("explicit_scans")
    );
    assert!(
        !sources["limits"]["operations"]
            .as_str()
            .unwrap()
            .contains("unavailable")
    );

    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Compatible source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &format!("Bearer {OPERATOR}")),
                ("idempotency-key", "compatible-source-request"),
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
    let (timeline, version, file): (String,String,String) = sqlx::query_as("SELECT v.timeline_id,v.id,b.file_id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.part=1").fetch_one(&f.app.db).await.unwrap();
    let plan_input = json!({"profile_id":"default","timeline_id":timeline,"version_id":version,"source":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null},
        "quality":{"mode":"original","max_bitrate_bps":null,"max_height":null,"allow_client_software":false,"hdr_policy":"preserve_if_supported"},
        "client":{"client_id":"native-original-test","client_build":"1","demuxe_asset_digest":null,"transports":["http_range"],"video_codecs":["avc1"],"audio_codecs":["mp4a"],"subtitle_modes":["text"],"hdr":"unknown","max_height":null,"software_decode":"unknown","cross_origin_isolated":false},"failed_candidate_ids":[]});
    let unauthenticated = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(plan_input.clone()),
            &[],
        )
        .await;
    assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    let forbidden_plan = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(plan_input.clone()),
            &[("authorization", &no_processing_auth)],
        )
        .await;
    assert_eq!(forbidden_plan.status, StatusCode::FORBIDDEN);
    let plan = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(plan_input.clone()),
            &[("authorization", &auth)],
        )
        .await;
    let mut conversion = plan_input.clone();
    conversion["quality"]["mode"] = json!("convert");
    conversion["client"]["transports"] = json!(["hls"]);
    let unqualified = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(conversion),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(unqualified.status, StatusCode::OK, "{:?}", unqualified.body);
    assert_eq!(unqualified.body["status"], "blocked");
    assert!(unqualified.body["plan_token"].is_null());
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    assert_eq!(plan.body["status"], "ready");
    assert_eq!(plan.body["operation"], "original");
    assert_eq!(plan.body["transport"], "http_range");
    assert_eq!(plan.body["source"]["file_id"], file);
    if recovery_case == 0 {
        let (status, html) = native_presentation::player(&f, &timeline, &auth).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(
            html.contains("value=\"a0\"") && html.contains("aac"),
            "actual measured AAC must be presented: {html}"
        );
        assert!(html.contains(&format!("data-file-id=\"{file}\"")));
        let (_, denied) = native_presentation::player(&f, &timeline, &no_processing_auth).await;
        assert!(
            !denied.contains("value=\"a0\""),
            "catalog-only caller cannot obtain player tracks"
        );
    }

    let create = json!({"plan_token":plan.body["plan_token"],"start_ms":0});
    let stolen = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            &other_auth,
            Some(create.clone()),
            Some("stolen-native-plan-request"),
        )
        .await;
    assert_eq!(
        stolen.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{:?}",
        stolen.body
    );
    let delivery = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            &auth,
            Some(create.clone()),
            Some("native-original-delivery"),
        )
        .await;
    assert_eq!(delivery.status, StatusCode::CREATED, "{:?}", delivery.body);
    assert_eq!(delivery.body["status"], "ready");
    assert_eq!(delivery.body["active"]["transport"], "http_range");
    let id = delivery.body["id"].as_str().unwrap();
    let generation = delivery.body["active"]["generation"].as_str().unwrap();
    let replay = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            &auth,
            Some(create),
            Some("native-original-delivery"),
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{:?}", replay.body);
    assert_eq!(replay.body["id"], id);
    let session = f
        .playback_adapter(
            "POST",
            "/playback/viewing-sessions",
            &auth,
            Some(json!({"delivery_id":id,"expected_viewing_revision":"0"})),
            Some("native-original-viewing"),
        )
        .await;
    assert_eq!(session.status, StatusCode::CREATED, "{:?}", session.body);
    assert_eq!(session.body["timeline_id"], timeline);
    // Actual session history writes use the native-fenced adapter, including
    // receipt replay and conflicting-sequence refusal, not a reducer shortcut.
    let session_id = session.body["id"].as_str().unwrap();
    let events = format!("/playback/viewing-sessions/{session_id}/events");
    let event = json!({"event_id":"native-original-event","sequence":"1","delivery_generation":generation,"position_ms":500,"status":"playing"});
    let progress = f
        .playback_adapter("POST", &events, &auth, Some(event.clone()), None)
        .await;
    assert_eq!(progress.status, StatusCode::OK, "{:?}", progress.body);
    assert_eq!(progress.body["duplicate"], false);
    assert_eq!(progress.body["viewing"]["position_ms"], 500);
    let duplicate = f
        .playback_adapter("POST", &events, &auth, Some(event.clone()), None)
        .await;
    assert_eq!(duplicate.status, StatusCode::OK, "{:?}", duplicate.body);
    assert_eq!(duplicate.body["duplicate"], true);
    let mut conflict = event;
    conflict["position_ms"] = json!(600);
    let rejected = f
        .playback_adapter("POST", &events, &auth, Some(conflict), None)
        .await;
    assert_eq!(rejected.status, StatusCode::CONFLICT, "{:?}", rejected.body);
    assert_eq!(rejected.body["code"], "event_conflict");
    let persisted: (i64,i64) = sqlx::query_as("SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(&timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(persisted, (500, 0));
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM viewing_events WHERE session_id=?")
            .bind(session_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(receipts, 1);

    let original_control_receipts = if recovery_case == 0 {
        Some(
            native_delivery_changes::exercise(
                &f,
                &auth,
                &other_auth,
                &plan.body["plan_token"],
                &timeline,
                &source_bytes,
            )
            .await,
        )
    } else {
        None
    };
    if recovery_case == 0 {
        native_delivery_controls::exercise(
            &f,
            &auth,
            &other_auth,
            &delivery.body,
            &session.body,
            &plan.body["plan_token"],
        )
        .await;
    }
    let ticket = f.playback_adapter("POST", "/content-access", &auth, Some(json!({"purpose":"playback","delivery_id":id,"generation":generation,"file":null,"download_id":null,"ttl_seconds":60})), Some("native-original-ticket")).await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{:?}", ticket.body);
    let url = ticket.body["url"].as_str().unwrap();
    let bytes = f.playback_raw("GET", url, "", None, None, true).await;
    assert_eq!(bytes.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        bytes
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &source_bytes[..16]
    );
    let ended = f
        .playback_adapter(
            "DELETE",
            &format!("/playback/delivery-sessions/{id}"),
            &auth,
            None,
            None,
        )
        .await;
    assert_eq!(ended.status, StatusCode::NO_CONTENT, "{:?}", ended.body);
    let revoked = f.playback_raw("GET", url, "", None, None, true).await;
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    // Establish the real playlist before capturing the immutable render admission.
    let retirement_playlist = if recovery_case == 2 {
        let created = f
            .call(
                "POST",
                "/api/v2/playlists",
                Some(json!({"name":"Retained history","profile_id":"default","entries":[{"entry_id":"retired-entry","timeline_id":timeline}]})),
                &[
                    ("authorization", &format!("Bearer {OPERATOR}")),
                    ("idempotency-key", "retired-playlist-history"),
                ],
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
        Some(created.body["id"].as_str().unwrap().to_owned())
    } else {
        None
    };
    let preparation_input = json!({"source":plan.body["source"],"timeline_id":timeline,
        "recipe_id":"remux_mp4","backend":"software","video_profile":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"audio_track_id":null,"subtitle_track_id":null,"subtitle_policy":"off"}});
    let public_preparation = f
        .call(
            "GET",
            "/api/v2/processing/preparations",
            Some(preparation_input.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-preparation-request"),
            ],
        )
        .await;
    assert_eq!(public_preparation.status, StatusCode::METHOD_NOT_ALLOWED);
    let forbidden_preparation = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(preparation_input.clone()),
            &[
                ("authorization", &no_processing_auth),
                ("idempotency-key", "forbidden-preparation-request"),
            ],
        )
        .await;
    assert_eq!(
        forbidden_preparation.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        forbidden_preparation.body
    );
    let preparation = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(preparation_input.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-preparation-request"),
            ],
        )
        .await;
    assert_eq!(
        preparation.status,
        StatusCode::ACCEPTED,
        "{:?}",
        preparation.body
    );
    let preparation_id = preparation.body["id"].as_str().unwrap();
    let capture_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_processing_admissions WHERE job_id=?")
            .bind(preparation_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(capture_count, 1);
    let captured: String = sqlx::query_scalar(
        "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
    )
    .bind(preparation_id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let captured: Value = serde_json::from_str(&captured).unwrap();
    assert_eq!(captured["contract_version"], 1);
    assert_eq!(captured["job_id"], preparation_id);
    assert_eq!(captured["timeline_id"], timeline);
    assert_eq!(captured["version_id"], version);
    assert_eq!(captured["native_version_revision"], 1);
    assert_eq!(captured["native_timeline_revision"], 1);
    assert_eq!(captured["occurrence"]["motion_file"]["id"], file);
    assert_eq!(
        captured["occurrence"]["motion_file"]["revision"],
        plan.body["source"]["file_revision"]
    );
    let expected_digest: String =
        sqlx::query_scalar("SELECT recipe_digest FROM preparations WHERE job_id=?")
            .bind(preparation_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(captured["recipe_digest"], expected_digest);
    let current: (i64,i64,String) = sqlx::query_as("SELECT motion_version_revision,motion_timeline_revision,manifest_json FROM catabolic_projection_versions WHERE version_id=? AND ready=1")
        .bind(&version).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(captured["motion_version_revision"], current.0);
    assert_eq!(captured["motion_timeline_revision"], current.1);
    assert_eq!(
        captured["manifest"],
        serde_json::from_str::<Value>(&current.2).unwrap()
    );
    let mapping: String = sqlx::query_scalar(
        "SELECT mapping_json FROM catabolic_projection_files WHERE file_id=? AND available=1",
    )
    .bind(&file)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let mapping: Value = serde_json::from_str(&mapping).unwrap();
    assert_eq!(
        captured["occurrence"]["physical"]["exact"],
        mapping["physical"]["exact"]
    );
    assert_eq!(
        captured["occurrence"]["motion_file"]["fingerprint"],
        playscale::scan::fingerprint(&std::fs::metadata(&media).unwrap())
    );
    let recorded_tracks: String =
        sqlx::query_scalar("SELECT tracks_json FROM preparations WHERE job_id=?")
            .bind(preparation_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        captured["tracks"],
        serde_json::from_str::<Value>(&recorded_tracks).unwrap()
    );
    let preparation_replay = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(preparation_input.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-preparation-request"),
            ],
        )
        .await;
    assert_eq!(
        preparation_replay.status,
        StatusCode::ACCEPTED,
        "{:?}",
        preparation_replay.body
    );
    assert_eq!(preparation_replay.body["id"], preparation_id);
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM processing_jobs WHERE id=? AND phase='queued'")
            .bind(preparation_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        queued, 1,
        "admission alone does not claim native output publication"
    );
    native_processing_jobs::observed(&f, preparation_id, &auth).await;
    let generated_outputs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM media_files WHERE generated=1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        generated_outputs, 0,
        "original route cannot publish generated catalog output"
    );
    // Real Motion rendering plus native publication. No direct catalog insert,
    // synthetic output file, or test-harness supervisor substitutes for workers.
    playscale::processing::recover(&f.app).await.unwrap();
    if fail_first_ack {
        // A real app storage failure after native commit, never a forged receipt.
        sqlx::query("CREATE TRIGGER fixture_fail_generated_ack BEFORE UPDATE OF phase ON processing_jobs WHEN NEW.phase='completed' BEGIN SELECT RAISE(ABORT,'fixture application ACK storage failure'); END")
            .execute(&f.app.db).await.unwrap();
    }
    let stop = tokio_util::sync::CancellationToken::new();
    let worker = tokio::spawn(playscale::processing::worker(f.app.clone(), stop.clone()));
    let terminal =
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                let state:(String,Option<String>,Option<String>,i64,bool) = sqlx::query_as(
                "SELECT phase,output_file_id,error,attempt,expired FROM processing_jobs WHERE id=?")
                .bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
                let receipt_waiting: bool = if fail_first_ack {
                    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_generated_publications WHERE job_id=? AND receipt_json IS NOT NULL AND acknowledged=0)")
                        .bind(preparation_id).fetch_one(&f.app.db).await.unwrap()
                } else { false };
                // Lease return proves the publication/ACK iteration has ended;
                // receipt persistence alone could precede the attempted ACK.
                if (receipt_waiting && f.app.processing.execution.snapshot().used == 0) || matches!(state.0.as_str(), "completed" | "failed" | "cancelled")
                    || worker.is_finished()
                {
                    break state;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;
    stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(30), worker)
        .await
        .expect("native render worker did not drain")
        .unwrap()
        .unwrap();
    let mut terminal = terminal.expect("native render did not reach terminal state");
    native_processing_jobs::observed(&f, preparation_id, &auth).await;
    if fail_first_ack {
        assert_ne!(terminal.0, "completed");
        assert_eq!(terminal.3, 1);
        let saved: (String,String,bool) = sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
            .bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
        assert!(!saved.2);
        let no_ack: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM renditions WHERE source='playscale' AND external_id=?",
        )
        .bind(preparation_id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
        assert_eq!(
            no_ack, 0,
            "failed ACK must roll back its entire app transaction"
        );
        let old_owners: (String,String) = sqlx::query_as("SELECT e.id,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?")
            .bind(&timeline).fetch_one(&f.app.db).await.unwrap();
        f.close().await;
        if recovery_case == 2 {
            let (_, native_timeline, removed_edges, retirement) =
                native_retirement::retire_edition(
                    f.app.db.clone(),
                    f._dir.path().join("native.sqlite3"),
                    old_owners.1.clone(),
                    version.clone(),
                )
                .await;
            assert_eq!(native_timeline, timeline);
            assert!(!removed_edges.is_empty());
            assert!(retirement.is_object());
        }
        sqlx::query("DROP TRIGGER fixture_fail_generated_ack")
            .execute(&f.app.db)
            .await
            .unwrap();
        f.open().await;
        playscale::processing::recover(&f.app).await.unwrap();
        terminal = sqlx::query_as(
            "SELECT phase,output_file_id,error,attempt,expired FROM processing_jobs WHERE id=?",
        )
        .bind(preparation_id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
        let recovered: (String,String,bool) = sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
            .bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
        assert_eq!(
            recovered,
            (saved.0, saved.1, true),
            "restart must ACK the exact receipt without dispatch or render"
        );
        native_processing_jobs::observed(&f, preparation_id, &auth).await;
        if recovery_case == 2 {
            assert_eq!(terminal.0, "completed");
            let playlist = f
                .call(
                    "GET",
                    &format!(
                        "/api/v2/playlists/{}",
                        retirement_playlist.as_ref().unwrap()
                    ),
                    None,
                    &[("authorization", &format!("Bearer {OPERATOR}"))],
                )
                .await;
            assert_eq!(playlist.status, StatusCode::OK, "{:?}", playlist.body);
            assert_eq!(playlist.body["entries"], json!([]));
            let continued = f
                .call(
                    "GET",
                    "/api/v2/profiles/default/continue-watching",
                    None,
                    &[("authorization", &format!("Bearer {OPERATOR}"))],
                )
                .await;
            assert_eq!(continued.status, StatusCode::OK, "{:?}", continued.body);
            assert_eq!(continued.body["items"], json!([]));
            assert_eq!(terminal.3, 1);
            assert!(
                terminal.4,
                "retired receipt ACK must be expired, never ready"
            );
            let output = terminal.1.as_ref().unwrap();
            let historical: (String, bool, bool) = sqlx::query_as(
                "SELECT edition_id,available,native_history FROM media_files WHERE id=?",
            )
            .bind(output)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
            assert_eq!(historical, (old_owners.0, false, true));
            let history: (i64,i64) = sqlx::query_as("SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
                .bind(&timeline).fetch_one(&f.app.db).await.unwrap();
            assert_eq!(history, persisted);
            let history_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM timeline_viewing WHERE profile_id='default'",
            )
            .fetch_one(&f.app.db)
            .await
            .unwrap();
            assert_eq!(
                history_count, 1,
                "split cannot reassign old viewing state to replacement timelines"
            );
            let ready: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_projection_versions WHERE ready=1 AND version_id IN (SELECT version_id FROM version_files WHERE file_id=?)")
                .bind(output).fetch_one(&f.app.db).await.unwrap();
            assert_eq!(ready, 0);
            let descriptive = f
                .call(
                    "GET",
                    &format!("/api/v2/processing/renditions?timeline_id={timeline}"),
                    None,
                    &[("authorization", &auth)],
                )
                .await;
            assert_eq!(descriptive.status, StatusCode::OK, "{:?}", descriptive.body);
            let retained = descriptive.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == preparation_id)
                .expect("All-scope retained rendition history remains visible");
            assert_eq!(retained["status"], "expired");
            assert_eq!(retained["output"]["file_id"], output.as_str());
            let refused = f
                .call(
                    "GET",
                    &format!("/api/v2/media/files/{output}/content"),
                    None,
                    &[("authorization", &auth)],
                )
                .await;
            assert_eq!(refused.status, StatusCode::NOT_FOUND);
            let original_refused = f
                .call(
                    "GET",
                    &format!(
                        "/api/v2/media/files/{}/content?revision={}",
                        plan.body["source"]["file_id"].as_str().unwrap(),
                        plan.body["source"]["file_revision"].as_str().unwrap()
                    ),
                    None,
                    &[("authorization", &auth)],
                )
                .await;
            assert_eq!(
                original_refused.status,
                StatusCode::NOT_FOUND,
                "an available source occurrence and library membership cannot grant a retired owner's original bytes: {:?}",
                original_refused.body
            );

            f.close().await;
            f.open().await;
            let again: (String, String, i64, bool) = sqlx::query_as(
                "SELECT phase,output_file_id,attempt,expired FROM processing_jobs WHERE id=?",
            )
            .bind(preparation_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
            assert_eq!(again, ("completed".into(), output.clone(), 1, true));
            f.close().await;
            return;
        }
    }
    assert_eq!(
        terminal.0, "completed",
        "native render failed: {terminal:?}"
    );
    assert_eq!(terminal.3, 1);
    assert!(!terminal.4);
    let output_file = terminal.1.as_deref().expect("completed output identity");
    let publication:(String,i64,String,String,String,bool) = sqlx::query_as(
        "SELECT operation_id,attempt,output_json,command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
        .bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(publication.1, 1);
    assert!(publication.5);
    assert!(
        serde_json::from_str::<Value>(&publication.2)
            .unwrap()
            .is_object()
    );
    let command: Value = serde_json::from_str(&publication.3).unwrap();
    assert_eq!(
        command["command"]["payload"]["operation"],
        "generated_output_admit"
    );
    assert_eq!(
        command["command"]["payload"]["request"]["job_id"],
        preparation_id
    );
    let receipt: Value = serde_json::from_str(&publication.4).unwrap();
    let (output_revision,root,relative):(String,String,String) = sqlx::query_as(
        "SELECT f.revision,l.root,f.relative_path FROM media_files f JOIN libraries l ON l.id=f.library_id WHERE f.id=? AND f.generated=1 AND f.available=1")
        .bind(output_file).fetch_one(&f.app.db).await.unwrap();
    let output_bytes = std::fs::read(std::path::Path::new(&root).join(&relative)).unwrap();
    assert_eq!(catabolic_core::sha256(&output_bytes), output_revision);
    assert_eq!(receipt["result"]["output_sha256"], output_revision);
    let output_version:String = sqlx::query_scalar("SELECT v.id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=? AND v.origin='generated'")
        .bind(output_file).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(receipt["result"]["version_id"], output_version);
    let ready:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_projection_versions p JOIN catabolic_projection_state s ON s.generation=p.generation WHERE p.version_id=? AND p.ready=1)")
        .bind(&output_version).fetch_one(&f.app.db).await.unwrap();
    assert!(ready);
    let promoted:(i64,i64,i64,i64) = sqlx::query_as("SELECT native_version_revision,native_timeline_revision,motion_version_revision,motion_timeline_revision FROM catabolic_projection_versions WHERE version_id=?")
        .bind(&version).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        promoted,
        (
            2,
            2,
            captured["motion_version_revision"].as_i64().unwrap() + 1,
            captured["motion_timeline_revision"].as_i64().unwrap() + 1
        )
    );
    let rendition:(String,String,String,String) = sqlx::query_as("SELECT file_id,file_revision,source_file_id,source_revision FROM renditions WHERE source='playscale' AND external_id=?")
        .bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        rendition,
        (
            output_file.to_owned(),
            output_revision.clone(),
            file.clone(),
            plan.body["source"]["file_revision"]
                .as_str()
                .unwrap()
                .to_owned()
        )
    );
    let history_after_render:(i64,i64) = sqlx::query_as("SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(&timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        history_after_render, persisted,
        "native publication cannot rewrite viewing history"
    );
    // Promotion changes native heads, not the exact original's byte identity.
    // Exercise current production admission/session adapters, not just the reducer.
    let promoted_plan = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(plan_input.clone()),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(
        promoted_plan.status,
        StatusCode::OK,
        "{:?}",
        promoted_plan.body
    );
    assert_eq!(promoted_plan.body["status"], "ready");
    assert_eq!(promoted_plan.body["operation"], "original");
    assert_eq!(promoted_plan.body["source"], plan.body["source"]);
    let promoted_delivery = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            &auth,
            Some(json!({"plan_token":promoted_plan.body["plan_token"],"start_ms":500})),
            Some("promoted-original-delivery"),
        )
        .await;
    assert_eq!(
        promoted_delivery.status,
        StatusCode::CREATED,
        "{:?}",
        promoted_delivery.body
    );
    let current_view_revision: i64 = sqlx::query_scalar(
        "SELECT revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?",
    )
    .bind(&timeline)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let promoted_session = f.playback_adapter("POST", "/playback/viewing-sessions", &auth,
        Some(json!({"delivery_id":promoted_delivery.body["id"],"expected_viewing_revision":current_view_revision.to_string()})), Some("promoted-original-viewing")).await;
    assert_eq!(
        promoted_session.status,
        StatusCode::CREATED,
        "{:?}",
        promoted_session.body
    );
    assert_eq!(promoted_session.body["timeline_id"], timeline);
    let promoted_history: (i64,i64,i64) = sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(&timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!((promoted_history.0, promoted_history.1), persisted);
    assert_eq!(promoted_history.2, current_view_revision + 1);
    let closed_public = f
        .call(
            "POST",
            "/api/v2/playback/delivery-sessions/closed/unsupported",
            Some(json!({"plan_token":promoted_plan.body["plan_token"],"start_ms":0})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "still-closed-public-delivery"),
            ],
        )
        .await;
    assert_eq!(closed_public.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_generated_range(&f, output_file, &output_revision, &auth, &output_bytes).await;
    let generated_download = if recovery_case == 0 {
        native_generated_downloads::controls(
            &f,
            &auth,
            &other_auth,
            &timeline,
            &output_version,
            output_file,
            &output_revision,
            &output_bytes,
        )
        .await;
        Some(
            native_generated_downloads::create(
                &f,
                &auth,
                &other_auth,
                &timeline,
                &output_version,
                output_file,
                &output_revision,
                "remux_mp4",
                &output_bytes,
                "online",
            )
            .await,
        )
    } else {
        None
    };
    if recovery_case == 0 {
        native_generated_playback::exercise(
            &f,
            plan_input.clone(),
            &output_version,
            output_file,
            &output_revision,
            &auth,
            &other_auth,
            &output_bytes,
            "online",
        )
        .await;
    }
    f.close().await;
    f.open().await;
    playscale::processing::recover(&f.app).await.unwrap();
    if let Some(receipts) = &original_control_receipts {
        native_delivery_changes::after_restart(&f, &auth, &other_auth, receipts).await;
    }
    if recovery_case == 0 {
        native_delivery_controls::after_restart(
            &f,
            &auth,
            &other_auth,
            &delivery.body,
            &session.body,
        )
        .await;
    }
    let after_restart: (String, String, i64) =
        sqlx::query_as("SELECT phase,output_file_id,attempt FROM processing_jobs WHERE id=?")
            .bind(preparation_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        after_restart,
        ("completed".into(), output_file.to_owned(), 1)
    );
    native_processing_jobs::observed(&f, preparation_id, &auth).await;
    let replayed_publication:(String,String,bool) = sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
        .bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        replayed_publication,
        (publication.3.clone(), publication.4.clone(), true)
    );
    let outputs: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files WHERE generated=1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        outputs, 1,
        "restart must not render or publish another output"
    );
    assert_generated_range(&f, output_file, &output_revision, &auth, &output_bytes).await;
    if let Some(download) = &generated_download {
        native_generated_downloads::check(&f, &auth, download, "ready", Some(&output_bytes)).await;
    }
    if recovery_case == 0 {
        let scoped_generated_auth = native_scoped_renditions::exercise(
            &mut f,
            source_id,
            &timeline,
            preparation_id,
            preparation_input,
            plan_input.clone(),
            &output_bytes,
            &other_auth,
        )
        .await;
        let observer = catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(binary, &catabolic_core::sha256(std::fs::read(binary).unwrap()), catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2).unwrap();
        f.app
            .native_catalog
            .as_ref()
            .unwrap()
            .register_observer(observer)
            .await
            .unwrap();
        std::fs::rename(&media, f._dir.path().join("old-original.mp4")).unwrap();
        playscale::native_discovery::scan(
            &f.app,
            source_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        native_delivery_changes::missing_source(
            &f,
            &auth,
            original_control_receipts.as_ref().unwrap(),
        )
        .await;
        native_generated_downloads::check(
            &f,
            &auth,
            generated_download.as_ref().unwrap(),
            "expired",
            None,
        )
        .await;
        assert_generated_range(&f, output_file, &output_revision, &auth, &output_bytes).await;
        native_generated_playback::exercise(
            &f,
            plan_input.clone(),
            &output_version,
            output_file,
            &output_revision,
            &auth,
            &other_auth,
            &output_bytes,
            "offline",
        )
        .await;
        assert_generated_range(
            &f,
            output_file,
            &output_revision,
            &scoped_generated_auth,
            &output_bytes,
        )
        .await;
        native_generated_playback::exercise(
            &f,
            plan_input.clone(),
            &output_version,
            output_file,
            &output_revision,
            &scoped_generated_auth,
            &auth,
            &output_bytes,
            "scoped-offline",
        )
        .await;
        let inherited_list = f
            .call(
                "GET",
                &format!("/api/v2/processing/renditions?timeline_id={timeline}"),
                None,
                &[("authorization", &scoped_generated_auth)],
            )
            .await;
        assert_eq!(
            inherited_list.status,
            StatusCode::OK,
            "{:?}",
            inherited_list.body
        );
        let retained_rendition = inherited_list.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == preparation_id)
            .expect("current generated membership must retain authorized descriptive rendition");
        assert_eq!(retained_rendition["output"]["file_id"], output_file);
        assert_eq!(retained_rendition["status"], "stale");
        let work: String = sqlx::query_scalar(
            "SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
        )
        .bind(&timeline)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
        let work_read = f
            .call(
                "GET",
                &format!("/api/v2/catalog/items/{work}"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(work_read.status, StatusCode::OK, "{:?}", work_read.body);
        assert_eq!(work_read.body["availability"], "degraded");
        let details = f
            .call(
                "GET",
                &format!("/api/v2/catalog/timelines/{timeline}/versions"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(details.status, StatusCode::OK, "{:?}", details.body);
        let versions = details.body["items"].as_array().unwrap();
        let original = versions.iter().find(|v| v["id"] == version).unwrap();
        assert_eq!(original["availability"], "unavailable");
        assert_eq!(original["timeline_equivalence"], "unknown");
        let generated = versions.iter().find(|v| v["id"] == output_version).unwrap();
        assert_eq!(generated["availability"], "available");
        let (_, unavailable) = native_presentation::player(&f, &timeline, &auth).await;
        assert!(
            !unavailable.contains("value=\"a0\""),
            "missing source cannot disclose stale track choices"
        );
        std::fs::write(&media, &source_bytes).unwrap();
        playscale::native_discovery::scan(
            &f.app,
            source_id,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        let (_, replaced) = native_presentation::player(&f, &timeline, &auth).await;
        assert!(
            !replaced.contains("value=\"a0\""),
            "a new occurrence cannot silently restore old timeline track identity"
        );
        let retained:(String,String,String,String)=sqlx::query_as("SELECT source_file_id,source_revision,file_id,file_revision FROM renditions WHERE source='playscale' AND external_id=?").bind(preparation_id).fetch_one(&f.app.db).await.unwrap();
        assert_eq!(
            retained,
            (
                file.clone(),
                plan.body["source"]["file_revision"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                output_file.to_owned(),
                output_revision.clone()
            )
        );
        let immutable: (String, String) = sqlx::query_as(
            "SELECT command_json,receipt_json FROM catabolic_generated_publications WHERE job_id=?",
        )
        .bind(preparation_id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
        assert_eq!(immutable, (publication.3.clone(), publication.4.clone()));
        let mut old_original = plan_input.clone();
        old_original["source"] = plan.body["source"].clone();
        let no_rebind = f
            .call(
                "POST",
                "/api/v2/playback/plans",
                Some(old_original),
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(no_rebind.status, StatusCode::OK, "{:?}", no_rebind.body);
        assert_eq!(no_rebind.body["status"], "blocked");
        assert!(no_rebind.body["plan_token"].is_null());
        // New bytes at the old path are a distinct occurrence. The independently
        // pinned prepared output still names its immutable old input, never it.
        native_generated_playback::exercise(
            &f,
            plan_input.clone(),
            &output_version,
            output_file,
            &output_revision,
            &auth,
            &other_auth,
            &output_bytes,
            "replacement-path",
        )
        .await;
        // A path with identical bytes is not the privately admitted managed
        // root. Exercise actual descriptor replacement without native SQL edits.
        let managed = std::path::Path::new(&root);
        let parked = f._dir.path().join("parked-managed-output");
        std::fs::rename(managed, &parked).unwrap();
        std::fs::create_dir_all(managed.join(&relative).parent().unwrap()).unwrap();
        std::fs::write(managed.join(&relative), &output_bytes).unwrap();
        let denied = f
            .call(
                "GET",
                &format!("/api/v2/media/files/{output_file}/content?revision={output_revision}"),
                None,
                &[("authorization", &auth), ("range", "bytes=0-15")],
            )
            .await;
        assert!(
            denied.status.is_client_error() || denied.status.is_server_error(),
            "replacement managed root must never yield bytes: {:?}",
            denied.status
        );
        std::fs::remove_dir_all(managed).unwrap();
        std::fs::rename(&parked, managed).unwrap();
    }
    if recovery_case == 1 {
        native_source_withdrawal::exercise(
            &mut f,
            source_id,
            &file,
            plan.body["source"]["file_revision"].as_str().unwrap(),
            output_file,
            &output_revision,
            &output_bytes,
            &auth,
            plan_input.clone(),
        )
        .await;
    }
    f.close().await;
}

#[tokio::test]
async fn native_motion_owned_profiles_and_device_revocation_keep_authorization_and_cas() {
    let mut f = Fixture::new().await;
    let (device, token) = f.pair(&["default"], &["catalog:read"]).await;
    let restricted = format!("Bearer {token}");
    let admin = format!("Bearer {OPERATOR}");
    f.cutover().await;
    let before: (i64,i64) = sqlx::query_as("SELECT (SELECT generation FROM catabolic_projection_state WHERE singleton=1),(SELECT count(*) FROM items)")
        .fetch_one(&f.app.db).await.unwrap();
    for path in [
        "/api/v2/profiles",
        "/api/v2/profiles/default",
        "/api/v2/devices",
    ] {
        let denied = f.call("GET", path, None, &[]).await;
        assert_eq!(
            denied.status,
            StatusCode::UNAUTHORIZED,
            "{path}: {:?}",
            denied.body
        );
    }
    let profiles = f
        .call(
            "GET",
            "/api/v2/profiles",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(profiles.status, StatusCode::OK, "{:?}", profiles.body);
    assert!(
        profiles.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "default")
    );
    let forbidden = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Forbidden"})),
            &[
                ("authorization", &restricted),
                ("idempotency-key", "native-profile-denied"),
            ],
        )
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    let created = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Native app profile"})),
            &[
                ("authorization", &admin),
                ("idempotency-key", "native-profile-create"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap();
    let path = format!("/api/v2/profiles/{id}");
    let initial_etag = created.headers["etag"].to_str().unwrap();
    let replay = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Native app profile"})),
            &[
                ("authorization", &admin),
                ("idempotency-key", "native-profile-create"),
            ],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body, created.body);
    let hidden = f
        .call("GET", &path, None, &[("authorization", &restricted)])
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let changed = f
        .call(
            "PUT",
            &path,
            Some(json!({"name":"Renamed native profile"})),
            &[("authorization", &admin), ("if-match", initial_etag)],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
    let current_etag = changed.headers["etag"].to_str().unwrap();
    let stale = f
        .call(
            "PUT",
            &path,
            Some(json!({"name":"Stale mutation"})),
            &[("authorization", &admin), ("if-match", initial_etag)],
        )
        .await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let no_op = f
        .call(
            "PUT",
            &path,
            Some(json!({"name":"Renamed native profile"})),
            &[("authorization", &admin), ("if-match", current_etag)],
        )
        .await;
    assert_eq!(no_op.status, StatusCode::OK);
    assert_eq!(no_op.headers["etag"], changed.headers["etag"]);
    f.close().await;
    f.open().await;
    let saved = f
        .call("GET", &path, None, &[("authorization", &admin)])
        .await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(saved.body, changed.body);
    let removed = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &admin), ("if-match", current_etag)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{:?}", removed.body);
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &admin)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let default = f
        .call(
            "GET",
            "/api/v2/profiles/default",
            None,
            &[("authorization", &admin)],
        )
        .await;
    let protected = f
        .call(
            "DELETE",
            "/api/v2/profiles/default",
            None,
            &[
                ("authorization", &admin),
                ("if-match", default.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(protected.status, StatusCode::CONFLICT);
    assert_eq!(protected.body["code"], "default_profile");
    assert_eq!(
        f.call(
            "GET",
            "/api/v2/devices",
            None,
            &[("authorization", &restricted)]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let listed = f
        .call("GET", "/api/v2/devices", None, &[("authorization", &admin)])
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert!(
        listed.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == device)
    );
    let device_path = format!("/api/v2/devices/{device}");
    let policy_path = format!("{device_path}/policy");
    assert_eq!(
        f.call("GET", &policy_path, None, &[("authorization", &restricted)])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let policy = f
        .call("GET", &policy_path, None, &[("authorization", &admin)])
        .await;
    assert_eq!(policy.status, StatusCode::OK);
    let device_etag = policy.headers["etag"].to_str().unwrap();
    let forbidden_revoke = f
        .call(
            "DELETE",
            &device_path,
            None,
            &[("authorization", &restricted), ("if-match", device_etag)],
        )
        .await;
    assert_eq!(forbidden_revoke.status, StatusCode::FORBIDDEN);
    let stale_revoke = f
        .call(
            "DELETE",
            &device_path,
            None,
            &[("authorization", &admin), ("if-match", "\"r-999999\"")],
        )
        .await;
    assert_eq!(stale_revoke.status, StatusCode::PRECONDITION_FAILED);
    let revoked = f
        .call(
            "DELETE",
            &device_path,
            None,
            &[("authorization", &admin), ("if-match", device_etag)],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{:?}", revoked.body);
    assert_eq!(
        f.call(
            "GET",
            "/api/v2/profiles",
            None,
            &[("authorization", &restricted)]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let after: (i64,i64) = sqlx::query_as("SELECT (SELECT generation FROM catabolic_projection_state WHERE singleton=1),(SELECT count(*) FROM items)")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        after, before,
        "Motion-owned identity operations cannot mutate catalog authority"
    );
    f.close().await;
}

#[tokio::test]
async fn native_pairing_policy_validates_current_library_without_legacy_rows() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let admin = format!("Bearer {OPERATOR}");
    let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Native policy library","kind":"mixed","language":"en","source_ids":[]})),
        &[("authorization", &admin),("idempotency-key","native-policy-library-create")]).await;
    assert_eq!(library.status, StatusCode::CREATED, "{:?}", library.body);
    let library_id = library.body["id"].as_str().unwrap();
    let legacy: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog_libraries WHERE id=?")
        .bind(library_id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        legacy, 0,
        "native policy validation cannot depend on a compatibility library row"
    );
    let pair = f
        .call(
            "POST",
            "/api/v2/auth/pairings",
            Some(json!({"device_name":"Native scoped device","client_name":"native-policy-test"})),
            &[],
        )
        .await;
    assert_eq!(pair.status, StatusCode::CREATED);
    let pair_id = pair.body["id"].as_str().unwrap();
    let path = format!("/api/v2/auth/pairings/{pair_id}/approve");
    let mut approval = json!({"user_code":pair.body["user_code"],"profile_ids":["default"],"permissions":["catalog:read"],
        "policy":{"library_ids":["missing-native-library"],"allow_unrated":true,"allowed_ratings":[],"deny_rated":false,"blocked_labels":[]}});
    let unknown = f
        .call(
            "POST",
            &path,
            Some(approval.clone()),
            &[
                ("authorization", &admin),
                ("idempotency-key", "native-policy-unknown-library"),
            ],
        )
        .await;
    assert_eq!(
        unknown.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{:?}",
        unknown.body
    );
    approval["policy"]["library_ids"] = json!([library_id]);
    let approved = f
        .call(
            "POST",
            &path,
            Some(approval.clone()),
            &[
                ("authorization", &admin),
                ("idempotency-key", "native-policy-known-library"),
            ],
        )
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
    let replay = f
        .call(
            "POST",
            &path,
            Some(approval),
            &[
                ("authorization", &admin),
                ("idempotency-key", "native-policy-known-library"),
            ],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body, approved.body);
    let claimed = f
        .call(
            "POST",
            &format!("/api/v2/auth/pairings/{pair_id}/claim"),
            Some(json!({"device_code":pair.body["device_code"]})),
            &[],
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{:?}", claimed.body);
    let device = claimed.body["device_id"].as_str().unwrap();
    f.close().await;
    f.open().await;
    let policy = f
        .call(
            "GET",
            &format!("/api/v2/devices/{device}/policy"),
            None,
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
    assert_eq!(policy.body["library_ids"], json!([library_id]));
    f.close().await;
}

#[tokio::test]
async fn native_relationship_curation_requires_all_containing_libraries() {
    let mut f = Fixture::new().await;
    let bytes = b"exact native relationship membership source";
    let library = f.seed_preserved_timeline("first.txt", bytes).await;
    let second_path = f._dir.path().join("media/second.txt");
    std::fs::write(&second_path, bytes).unwrap();
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('second-work','Second work','video');
        INSERT INTO item_origins(item_id,title) VALUES('second-work','Second work');
        INSERT INTO editions(id,item_id,label) VALUES('second-edition','second-work','Original');
        INSERT INTO timelines(id,edition_id,duration_ms) VALUES('second-timeline','second-edition',1000);
        INSERT INTO media_versions(id,timeline_id,label,origin,equivalence) VALUES('second-version','second-timeline','Original','original','declared');
        INSERT INTO catalog_libraries(id,name,kind) VALUES('hidden-library','Hidden library','mixed');")
        .execute(&f.app.db).await.unwrap();
    sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds) VALUES('second-file','second-edition',?,'second.txt',?,?,?,1.0)")
        .bind(&library).bind(catabolic_core::sha256(bytes)).bind(playscale::scan::fingerprint(&std::fs::metadata(&second_path).unwrap()))
        .bind(bytes.len() as i64).execute(&f.app.db).await.unwrap();
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES('second-version',1,'second-file',?)")
        .bind(catabolic_core::sha256(bytes)).execute(&f.app.db).await.unwrap();
    sqlx::query("INSERT INTO library_sources(library_id,source_id) VALUES('hidden-library',?)")
        .bind(&library)
        .execute(&f.app.db)
        .await
        .unwrap();
    let operator = format!("Bearer {OPERATOR}");
    let mut credentials = Vec::new();
    for ids in [
        vec![library.clone()],
        vec![library.clone(), "hidden-library".into()],
    ] {
        let (device, token) = f
            .pair(&["default"], &["catalog:read", "catalog:write"])
            .await;
        let row = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &operator)],
            )
            .await;
        let changed=f.call("PUT",&format!("/api/v2/devices/{device}/policy"),Some(json!({"library_ids":ids,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read","catalog:write"]})),
            &[("authorization",&operator),("if-match",row.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
        credentials.push(format!("Bearer {token}"));
    }
    f.cutover_source(Some(&library)).await;
    let memberships: Vec<String> = sqlx::query_scalar(
        "SELECT p.library_id FROM catabolic_projection_file_libraries p JOIN catabolic_projection_state s ON s.generation=p.generation WHERE p.file_id='file' ORDER BY p.library_id",
    ).fetch_all(&f.app.db).await.unwrap();
    let mut expected_memberships = vec![library.clone(), "hidden-library".into()];
    expected_memberships.sort();
    assert_eq!(memberships, expected_memberships);
    assert!(
        sqlx::query("DELETE FROM catabolic_projection_file_libraries WHERE file_id='file'")
            .execute(&f.app.db)
            .await
            .is_err(),
        "ordinary connections cannot rewrite native membership proof"
    );
    let limited = &credentials[0];
    let complete = &credentials[1];
    for id in ["work", "second-work"] {
        let visible = f
            .call(
                "GET",
                &format!("/api/v2/catalog/items/{id}"),
                None,
                &[("authorization", limited)],
            )
            .await;
        assert_eq!(visible.status, StatusCode::OK, "{:?}", visible.body);
    }
    let path = "/api/v2/catalog/items/work";
    let limited_view = f
        .call("GET", path, None, &[("authorization", limited)])
        .await;
    let input = json!({"source_item_id":"work","target_item_id":"second-work","kind":"part_of","position":null});
    let denied = f
        .call(
            "POST",
            &format!("{path}/relationships"),
            Some(input.clone()),
            &[
                ("authorization", limited),
                ("idempotency-key", "limited-shared-work-edge"),
                ("if-match", limited_view.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    let all_view = f
        .call("GET", path, None, &[("authorization", complete)])
        .await;
    let created = f
        .call(
            "POST",
            &format!("{path}/relationships"),
            Some(input),
            &[
                ("authorization", complete),
                ("idempotency-key", "complete-shared-work-edge"),
                ("if-match", all_view.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let edge = format!(
        "/api/v2/catalog/relationships/{}",
        created.body["id"].as_str().unwrap()
    );
    let etag = created.headers["etag"].to_str().unwrap();
    let denied = f
        .call(
            "DELETE",
            &edge,
            None,
            &[("authorization", limited), ("if-match", etag)],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    let removed = f
        .call(
            "DELETE",
            &edge,
            None,
            &[("authorization", complete), ("if-match", etag)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{:?}", removed.body);
    // Visibility through one shared library is not authority to curate every
    // containing library, including when replaying a completed native receipt.
    let metadata = "/api/v2/catalog/items/work/metadata";
    let contribution = format!("{metadata}/contributions/local");
    let initial = f
        .call("GET", metadata, None, &[("authorization", limited)])
        .await;
    assert_eq!(initial.status, StatusCode::OK, "{:?}", initial.body);
    let initial_etag = initial.headers["etag"].to_str().unwrap();
    let input = json!({"values":{"title":"Shared native curation"},"tags":[],"excluded_tags":[],"locked_fields":[]});
    let denied = f
        .call(
            "PUT",
            &contribution,
            Some(input.clone()),
            &[("authorization", limited), ("if-match", initial_etag)],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    let written = f
        .call(
            "PUT",
            &contribution,
            Some(input.clone()),
            &[("authorization", complete), ("if-match", initial_etag)],
        )
        .await;
    assert_eq!(written.status, StatusCode::OK, "{:?}", written.body);
    assert_eq!(written.body["values"]["title"], "Shared native curation");
    let replay = f
        .call(
            "PUT",
            &contribution,
            Some(input.clone()),
            &[("authorization", complete), ("if-match", initial_etag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{:?}", replay.body);
    assert_eq!(replay.body, written.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let denied_replay = f
        .call(
            "PUT",
            &contribution,
            Some(input),
            &[("authorization", limited), ("if-match", initial_etag)],
        )
        .await;
    assert_eq!(
        denied_replay.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        denied_replay.body
    );
    let current = f
        .call("GET", path, None, &[("authorization", limited)])
        .await;
    assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
    let artwork = f
        .call(
            "PUT",
            &format!("{path}/artwork/selection"),
            Some(json!({"role":"poster","asset_id":"not-associated"})),
            &[
                ("authorization", limited),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(artwork.status, StatusCode::FORBIDDEN, "{:?}", artwork.body);
    assert_eq!(artwork.body["code"], "outside_scope");
    f.close().await;
}

#[tokio::test]
async fn native_detached_version_edge_revokes_bytes_without_changing_file_or_heads() {
    let mut f = Fixture::new().await;
    let bytes = b"unchanged exact original content behind native structural edge";
    let source = f.seed_preserved_timeline("original.txt", bytes).await;
    f.cutover_source(Some(&source)).await;
    let auth = format!("Bearer {OPERATOR}");
    let revision = catabolic_core::sha256(bytes);
    let (before, body) = f.content_adapter(&auth, &revision).await;
    assert_eq!(before, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, &bytes[..16]);
    let heads:(i64,i64)=sqlx::query_as("SELECT native_version_revision,native_timeline_revision FROM catabolic_projection_versions WHERE version_id='version'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(heads, (1, 1));
    f.close().await;
    let receipt = native_retirement::detach_version(
        f.app.db.clone(),
        f._dir.path().join("native.sqlite3"),
        "work".into(),
        "version".into(),
    )
    .await;
    assert!(receipt.is_object());
    f.open().await;
    let (denied, _) = f.content_adapter(&auth, &revision).await;
    assert_eq!(
        denied,
        StatusCode::NOT_FOUND,
        "a detached native version must not retain a file grant"
    );
    let viewing = f.viewing_adapter("GET", &auth, None, None).await;
    assert_eq!(viewing.status, StatusCode::NOT_FOUND, "{:?}", viewing.body);
    assert_eq!(
        std::fs::read(f._dir.path().join("media/original.txt")).unwrap(),
        bytes
    );
    let projected:(bool,i64,i64)=sqlx::query_as("SELECT ready,native_version_revision,native_timeline_revision FROM catabolic_projection_versions WHERE version_id='version'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(projected, (false, heads.0, heads.1));
    let history:(i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default' AND timeline_id='timeline'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(history, (250, 2));
    f.close().await;
    f.open().await;
    let (denied, _) = f.content_adapter(&auth, &revision).await;
    assert_eq!(denied, StatusCode::NOT_FOUND);
    f.close().await;
}
