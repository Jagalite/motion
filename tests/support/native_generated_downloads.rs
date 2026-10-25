//! Public prepared-download ownership and media-only manifest coverage.
//! Subtitle siblings remain independently closed.
use super::native_application_fixture::Fixture;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use serde_json::{Value, json};

type Pin = (String, i64, String, String, String);
pub struct Saved {
    pub id: String,
    pub input: Value,
    pin: Pin,
}

pub async fn input(f: &Fixture, auth: &str, timeline: &str, version: &str, recipe: &str) -> Value {
    let me = f
        .call("GET", "/api/v2/me", None, &[("authorization", auth)])
        .await;
    assert_eq!(me.status, StatusCode::OK);
    json!({"device_id":me.body["device_id"],"profile_id":"default","timeline_id":timeline,"version_id":version,"recipe_id":recipe,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}})
}

pub async fn create(
    f: &Fixture,
    auth: &str,
    other: &str,
    timeline: &str,
    version: &str,
    file: &str,
    revision: &str,
    recipe: &str,
    bytes: &[u8],
    suffix: &str,
) -> Saved {
    let input = input(f, auth, timeline, version, recipe).await;
    let key = format!("generated-download-{suffix}");
    let created = f
        .download_adapter("POST", "/downloads", auth, Some(input.clone()), Some(&key))
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    assert_eq!(created.body["state"], "ready");
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(created.body["size_bytes"], bytes.len().to_string());
    let replay = f
        .download_adapter("POST", "/downloads", auth, Some(input.clone()), Some(&key))
        .await;
    assert_eq!(replay.status, StatusCode::ACCEPTED);
    assert_eq!(replay.body["id"], id);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(
        f.download_adapter("GET", &format!("/downloads/{id}"), other, None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let pin:Pin=sqlx::query_as("SELECT timeline_id,timeline_revision,version_id,file_id,file_revision FROM downloads WHERE id=?").bind(&id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        (
            pin.0.as_str(),
            pin.2.as_str(),
            pin.3.as_str(),
            pin.4.as_str()
        ),
        (timeline, version, file, revision)
    );
    let saved = Saved { id, input, pin };
    check(f, auth, &saved, "ready", Some(bytes)).await;
    if suffix == "online" {
        let (job, original_recipe): (String, String) = sqlx::query_as(
            "SELECT j.id,j.recipe FROM processing_jobs j JOIN renditions r ON r.external_id=j.id AND r.source='playscale' WHERE r.file_id=? AND r.file_revision=?",
        ).bind(file).bind(revision).fetch_one(&f.app.db).await.unwrap();
        match sqlx::query("UPDATE processing_jobs SET recipe='audio_aac' WHERE id=?")
            .bind(&job)
            .execute(&f.app.db)
            .await
        {
            Ok(changed) => {
                assert_eq!(changed.rows_affected(), 1);
                let refused = f
                    .download_adapter("GET", &format!("/downloads/{}", saved.id), auth, None, None)
                    .await;
                sqlx::query("UPDATE processing_jobs SET recipe=? WHERE id=?")
                    .bind(&original_recipe)
                    .bind(&job)
                    .execute(&f.app.db)
                    .await
                    .unwrap();
                assert_eq!(
                    refused.status,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "altered application recipe must not authorize an existing prepared pin: {:?}",
                    refused.body
                );
            }
            Err(error) => assert!(
                error
                    .as_database_error()
                    .is_some_and(|e| e.code().as_deref() == Some("1811")),
                "only immutable storage refusal qualifies: {error}"
            ),
        }
        check(f, auth, &saved, "ready", Some(bytes)).await;
    }
    assert_eq!(
        f.manifest_adapter(&saved.id, "").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.manifest_adapter(&saved.id, other).await.status,
        StatusCode::NOT_FOUND
    );
    let mut unsupported = saved.input.clone();
    unsupported["recipe_id"] = json!("audio_aac");
    let denied = f
        .download_adapter(
            "POST",
            "/downloads",
            auth,
            Some(unsupported),
            Some(&format!("wrong-generated-recipe-{suffix}")),
        )
        .await;
    assert_eq!(
        denied.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{:?}",
        denied.body
    );
    saved
}

pub async fn listed(f: &Fixture, auth: &str, active: bool) -> Vec<Value> {
    let mut cursor = None::<String>;
    let mut items = Vec::new();
    loop {
        // limit=1 genuinely exercises filtering before pagination. All current
        // fixture download IDs are server-generated URL-safe UUIDs.
        let path = format!(
            "/api/v2/downloads?limit=1&active_only={active}{}",
            cursor
                .as_ref()
                .map(|c| format!("&cursor={c}"))
                .unwrap_or_default()
        );
        let page = f.call("GET", &path, None, &[("authorization", auth)]).await;
        assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
        let rows = page.body["items"].as_array().unwrap();
        assert!(rows.len() <= 1);
        for row in rows {
            if let Some(previous) = items.last() {
                let previous: &Value = previous;
                assert!(previous["id"].as_str().unwrap() < row["id"].as_str().unwrap());
            }
            items.push(row.clone());
        }
        let next = page.body["next_cursor"].as_str().map(str::to_owned);
        if let Some(next) = next {
            assert_eq!(rows.last().unwrap()["id"], next);
            assert!(cursor.as_ref().is_none_or(|old| old < &next));
            cursor = Some(next);
        } else {
            break;
        }
        assert!(
            items.len() < 30,
            "this finite fixture creates fewer than30 demands"
        );
    }
    items
}

pub async fn assert_listed(f: &Fixture, auth: &str, id: &str, state: &str) {
    let page = f
        .call(
            "GET",
            "/api/v2/downloads?limit=200",
            None,
            &[("authorization", auth)],
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    let row = page.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .expect("owned demand remains listed");
    assert_eq!(row["state"], state);
}

pub async fn check(f: &Fixture, auth: &str, saved: &Saved, state: &str, bytes: Option<&[u8]>) {
    let current = f
        .download_adapter("GET", &format!("/downloads/{}", saved.id), auth, None, None)
        .await;
    assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
    assert_eq!(current.body["state"], state);
    assert_listed(f, auth, &saved.id, state).await;
    if state == "expired" {
        // active_only is stored demand state, not a promise of present bytes.
        let active = f
            .call(
                "GET",
                "/api/v2/downloads?limit=200&active_only=true",
                None,
                &[("authorization", auth)],
            )
            .await;
        assert_eq!(active.status, StatusCode::OK, "{:?}", active.body);
        let row = active.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == saved.id)
            .expect("expired ready demand is retained");
        assert_eq!(row["state"], "expired");
    }
    let retained:Pin=sqlx::query_as("SELECT timeline_id,timeline_revision,version_id,file_id,file_revision FROM downloads WHERE id=?").bind(&saved.id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        retained, saved.pin,
        "native prepared pins must never silently rebind"
    );
    let manifest = f.manifest_adapter(&saved.id, auth).await;
    if state == "ready" {
        assert_eq!(manifest.status, StatusCode::OK, "{:?}", manifest.body);
        assert_eq!(manifest.body["files"].as_array().unwrap().len(), 1);
        assert_eq!(manifest.body["files"][0]["asset_id"], saved.pin.3);
        assert_eq!(
            manifest.body["files"][0]["digest"],
            format!("sha256:{}", saved.pin.4)
        );
        assert_eq!(manifest.body["timeline_revision"], saved.pin.1.to_string());
        let url = manifest.body["files"][0]["media_url"].as_str().unwrap();
        let response = f.playback_raw("GET", url, auth, None, None, true).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let actual = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(actual.as_ref(), &bytes.unwrap()[..16]);
    } else {
        assert_eq!(manifest.status, StatusCode::CONFLICT, "{:?}", manifest.body);
        assert_eq!(manifest.body["code"], "download_expired");
    }
}

// Candidate exact controls use the full router. Actual qualification remains
// separate from merely staging these public acceptance assertions.
async fn control(
    f: &Fixture,
    method: &str,
    path: &str,
    auth: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v2{path}"))
        .header("host", "127.0.0.1:8787")
        .header("authorization", auth);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let response = super::playscale_app::api::router(f.app.clone(), None)
        .oneshot(
            request
                .body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

pub async fn controls(
    f: &Fixture,
    auth: &str,
    other: &str,
    timeline: &str,
    version: &str,
    file: &str,
    revision: &str,
    bytes: &[u8],
) {
    let saved = create(
        f,
        auth,
        other,
        timeline,
        version,
        file,
        revision,
        "remux_mp4",
        bytes,
        "public-controls",
    )
    .await;
    // Real foreign demand and a second profile demand establish ownership and
    // profile filtering before the limit-one cursor boundary.
    let foreign_input = input(f, other, timeline, version, "remux_mp4").await;
    let foreign = f
        .download_adapter(
            "POST",
            "/downloads",
            other,
            Some(foreign_input),
            Some("list-genuine-foreign-demand"),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::ACCEPTED, "{:?}", foreign.body);
    let foreign_id = foreign.body["id"].as_str().unwrap();
    let profile = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Download list profile"})),
            &[
                ("authorization", auth),
                ("idempotency-key", "download-list-other-profile"),
            ],
        )
        .await;
    assert_eq!(profile.status, StatusCode::CREATED, "{:?}", profile.body);
    let mut alternate_input = saved.input.clone();
    alternate_input["profile_id"] = profile.body["id"].clone();
    let alternate = f
        .download_adapter(
            "POST",
            "/downloads",
            auth,
            Some(alternate_input),
            Some("list-genuine-other-profile"),
        )
        .await;
    assert_eq!(
        alternate.status,
        StatusCode::ACCEPTED,
        "{:?}",
        alternate.body
    );
    let alternate_id = alternate.body["id"].as_str().unwrap();
    let own = listed(f, auth, false).await;
    assert!(own.len() >= 2);
    assert!(own.iter().any(|row| row["id"] == saved.id));
    assert!(own.iter().any(|row| row["id"] == alternate_id));
    assert!(!own.iter().any(|row| row["id"] == foreign_id));
    let foreign_list = listed(f, other, false).await;
    assert!(foreign_list.iter().any(|row| row["id"] == foreign_id));
    assert!(!foreign_list.iter().any(|row| row["id"] == saved.id));
    assert_eq!(
        f.call("GET", "/api/v2/downloads", None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let operator = format!("Bearer {}", super::native_application_fixture::OPERATOR);
    let no_device = f
        .call(
            "GET",
            "/api/v2/downloads",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(no_device.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(no_device.body["code"], "device_required");
    assert_eq!(
        f.call(
            "GET",
            "/api/v2/downloads?limit=0",
            None,
            &[("authorization", &operator)]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let invalid_limit = f
        .call(
            "GET",
            "/api/v2/downloads?limit=0",
            None,
            &[("authorization", auth)],
        )
        .await;
    assert_eq!(invalid_limit.status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid_limit.body["code"], "invalid_limit");
    let before: (i64,i64,i64) = sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(timeline).fetch_one(&f.app.db).await.unwrap();
    let path = format!("/downloads/{}/transfer", saved.id);
    let report = |sequence: u64, rev: &str, state: &str, n: usize| json!({"sequence":sequence.to_string(),"download_revision":rev,"state":state,"bytes_received":n.to_string()});
    let first = report(1, "1", "transferring", 16);
    assert_eq!(
        control(f, "PUT", &path, other, Some(first.clone())).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        control(f, "PUT", &path, "", Some(first.clone())).await.0,
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..2 {
        let r = control(f, "PUT", &path, auth, Some(first.clone())).await;
        assert_eq!(r.0, StatusCode::OK, "{:?}", r.1);
        assert_eq!(r.1["device_transfer"]["sequence"], "1");
    }
    let conflict = control(
        f,
        "PUT",
        &path,
        auth,
        Some(report(1, "1", "transferring", 17)),
    )
    .await;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(conflict.1["code"], "transfer_report_conflict");
    let verified = control(
        f,
        "PUT",
        &path,
        auth,
        Some(report(3, "1", "verified_local", bytes.len())),
    )
    .await;
    assert_eq!(verified.0, StatusCode::OK, "{:?}", verified.1);
    let stale = control(
        f,
        "PUT",
        &path,
        auth,
        Some(report(2, "1", "transferring", 16)),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["code"], "transfer_report_stale");
    let ticket=f.playback_adapter("POST","/content-access",auth,Some(json!({"purpose":"download","delivery_id":null,"generation":null,"file":null,"download_id":saved.id,"ttl_seconds":60})),Some("generated-download-control-ticket")).await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{:?}", ticket.body);
    let url = ticket.body["url"].as_str().unwrap();
    assert_eq!(
        f.playback_raw("GET", url, "", None, None, true)
            .await
            .status(),
        StatusCode::PARTIAL_CONTENT
    );
    let cancel = format!("/downloads/{}", saved.id);
    assert_eq!(
        control(f, "DELETE", &cancel, "", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Revoke through the actual current policy owner while another genuine
    // paired administrator remains. No direct credential or policy edits.
    let me = f
        .call("GET", "/api/v2/me", None, &[("authorization", auth)])
        .await;
    assert_eq!(me.status, StatusCode::OK);
    let policy_path = format!(
        "/api/v2/devices/{}/policy",
        me.body["device_id"].as_str().unwrap()
    );
    let operator = format!("Bearer {}", super::native_application_fixture::OPERATOR);
    let current = f
        .call("GET", &policy_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK);
    let mut original = current.body.clone();
    original.as_object_mut().unwrap().remove("revision");
    // The paired grant names default only. Dropping administrator authority
    // therefore hides the real alternate-profile pin before pagination, while
    // retaining DownloadsManage and the caller's original default demands.
    let mut limited = original.clone();
    limited["permissions"] = json!(["catalog:read", "downloads:manage"]);
    let profile_limited = f
        .call(
            "PUT",
            &policy_path,
            Some(limited),
            &[
                ("authorization", &operator),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(
        profile_limited.status,
        StatusCode::OK,
        "{:?}",
        profile_limited.body
    );
    let filtered = listed(f, auth, false).await;
    assert!(filtered.iter().any(|row| row["id"] == saved.id));
    assert!(!filtered.iter().any(|row| row["id"] == alternate_id));
    assert!(!filtered.iter().any(|row| row["id"] == foreign_id));
    let mut reduced = original.clone();
    reduced["permissions"] = json!(["catalog:read"]);
    let changed = f
        .call(
            "PUT",
            &policy_path,
            Some(reduced),
            &[
                ("authorization", &operator),
                (
                    "if-match",
                    profile_limited.headers["etag"].to_str().unwrap(),
                ),
            ],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
    assert_eq!(
        control(f, "DELETE", &cancel, auth, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        control(f, "PUT", &path, auth, Some(report(4, "1", "removed", 0)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.playback_raw("GET", url, "", None, None, true)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.manifest_adapter(&saved.id, auth).await.status,
        StatusCode::FORBIDDEN,
        "manifest disclosure must reauthorize current DownloadsManage"
    );
    assert_eq!(
        f.call("GET", "/api/v2/downloads", None, &[("authorization", auth)])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let unchanged:(String,i64,i64)=sqlx::query_as("SELECT d.state,d.revision,t.sequence FROM downloads d JOIN download_transfers t ON t.download_id=d.id WHERE d.id=?").bind(&saved.id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(unchanged, ("ready".into(), 1, 3));
    let restored = f
        .call(
            "PUT",
            &policy_path,
            Some(original),
            &[
                ("authorization", &operator),
                ("if-match", changed.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(restored.status, StatusCode::OK, "{:?}", restored.body);
    assert_eq!(
        control(f, "DELETE", &cancel, other, None).await.0,
        StatusCode::NOT_FOUND
    );
    for _ in 0..2 {
        assert_eq!(
            control(f, "DELETE", &cancel, auth, None).await.0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        f.playback_raw("GET", url, "", None, None, true)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let current = f.download_adapter("GET", &cancel, auth, None, None).await;
    assert_eq!(current.body["state"], "cancelled");
    assert_eq!(current.body["revision"], "2");
    let cancelled_manifest = f.manifest_adapter(&saved.id, auth).await;
    assert_eq!(cancelled_manifest.status, StatusCode::CONFLICT);
    assert_eq!(cancelled_manifest.body["code"], "download_cancelled");
    assert_listed(f, auth, &saved.id, "cancelled").await;
    assert!(
        !listed(f, auth, true)
            .await
            .iter()
            .any(|row| row["id"] == saved.id)
    );
    let ended = control(
        f,
        "PUT",
        &path,
        auth,
        Some(report(4, "2", "verified_local", bytes.len())),
    )
    .await;
    assert_eq!(ended.0, StatusCode::CONFLICT);
    assert_eq!(ended.1["code"], "download_ended");
    let removed = control(f, "PUT", &path, auth, Some(report(4, "2", "removed", 0))).await;
    assert_eq!(removed.0, StatusCode::OK, "{:?}", removed.1);
    assert_eq!(removed.1["device_transfer"]["state"], "removed");
    let after: (i64,i64,i64) = sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        after, before,
        "download transfer/cancellation cannot rewrite viewing history"
    );
    for (method, p) in [
        ("HEAD", format!("/api/v2{cancel}")),
        ("HEAD", "/api/v2/downloads".into()),
        ("HEAD", format!("/api/v2{cancel}/manifest")),
        ("GET", format!("/api/v2{cancel}/subtitles")),
    ] {
        assert_eq!(
            f.call(method, &p, Some(first.clone()), &[("authorization", auth)])
                .await
                .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
