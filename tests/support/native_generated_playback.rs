//! Actual outer-router generated playback, session, and ticket acceptance.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use http_body_util::BodyExt;
use serde_json::{Value, json};

pub async fn exercise(
    f: &Fixture,
    mut input: Value,
    version: &str,
    file: &str,
    revision: &str,
    auth: &str,
    other: &str,
    expected: &[u8],
    suffix: &str,
) {
    input["version_id"] = json!(version);
    input["source"] = json!({"file_id":file,"file_revision":revision});
    if suffix == "online" {
        let mut original_only = input.clone();
        original_only["quality"]["mode"] = json!("original");
        let blocked = f
            .call(
                "POST",
                "/api/v2/playback/plans",
                Some(original_only),
                &[("authorization", auth)],
            )
            .await;
        assert_eq!(blocked.status, StatusCode::OK, "{:?}", blocked.body);
        assert_eq!(blocked.body["status"], "blocked");
        assert_eq!(
            blocked.body["reason_codes"],
            json!(["selected_version_unavailable"])
        );
        assert!(blocked.body["plan_token"].is_null());
    }
    // Auto permits prepared delivery, while the explicit version/output pin
    // still forbids silently substituting the original or another rendition.
    input["quality"]["mode"] = json!("auto");
    let plan = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(input.clone()),
            &[("authorization", auth)],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    assert_eq!(plan.body["status"], "ready", "{:?}", plan.body);
    assert_eq!(plan.body["operation"], "prepared");
    assert_eq!(plan.body["source"]["file_id"], file);
    let create = json!({"plan_token":plan.body["plan_token"],"start_ms":0});
    if suffix == "online" {
        let unauthenticated = f
            .playback_adapter(
                "POST",
                "/playback/delivery-sessions",
                "",
                Some(create.clone()),
                Some("unauthenticated-delivery"),
            )
            .await;
        assert_eq!(unauthenticated.status, StatusCode::UNAUTHORIZED);
    }
    let foreign = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            other,
            Some(create.clone()),
            Some(&format!("foreign-generated-{suffix}")),
        )
        .await;
    assert_eq!(foreign.status, StatusCode::UNPROCESSABLE_ENTITY);
    let delivery = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            auth,
            Some(create.clone()),
            Some(&format!("generated-delivery-{suffix}")),
        )
        .await;
    assert_eq!(delivery.status, StatusCode::CREATED, "{:?}", delivery.body);
    assert_eq!(delivery.body["active"]["transport"], "http_range");
    let id = delivery.body["id"].as_str().unwrap();
    let generation = delivery.body["active"]["generation"].as_str().unwrap();
    let replay = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            auth,
            Some(create),
            Some(&format!("generated-delivery-{suffix}")),
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body["id"], id);
    if suffix == "online" {
        let stream = format!("/streams/{id}/{generation}/content");
        assert_eq!(
            f.playback_raw("GET", &stream, "", None, None, true)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.playback_raw("GET", &stream, other, None, None, true)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let viewing = json!({"delivery_id":id,"expected_viewing_revision":"0"});
        let event = json!({"event_id":"unauthenticated-event","sequence":"1","delivery_generation":generation,"position_ms":0,"status":"playing"});
        let ticket = json!({"purpose":"playback","delivery_id":id,"generation":generation,"file":null,"download_id":null,"ttl_seconds":60});
        for (method, path, body) in [
            ("DELETE", format!("/playback/delivery-sessions/{id}"), None),
            (
                "POST",
                "/playback/viewing-sessions".to_owned(),
                Some(viewing),
            ),
            (
                "POST",
                "/playback/viewing-sessions/unknown/events".to_owned(),
                Some(event),
            ),
            ("POST", "/content-access".to_owned(), Some(ticket)),
        ] {
            let denied = f
                .playback_adapter(method, &path, "", body, Some("unauthenticated-command"))
                .await;
            assert_eq!(
                denied.status,
                StatusCode::UNAUTHORIZED,
                "{path}: {:?}",
                denied.body
            );
        }
        for (method, path) in [
            ("HEAD", stream),
            ("HEAD", format!("/playback/delivery-sessions/{id}")),
            ("HEAD", format!("/playback/delivery-sessions/{id}/changes")),
            (
                "HEAD",
                format!("/playback/delivery-sessions/{id}/generations/{generation}/activate"),
            ),
            ("GET", format!("/streams/{id}/{generation}/master.m3u8")),
            ("GET", format!("/streams/{id}/{generation}/subtitles.vtt")),
        ] {
            let denied = f.playback_adapter(method, &path, auth, None, None).await;
            assert_eq!(
                denied.status,
                if method == "HEAD" && (path.ends_with("/changes") || path.ends_with("/activate")) {
                    StatusCode::METHOD_NOT_ALLOWED
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                "{path}: {:?}",
                denied.body
            );
        }
    }
    exercise_replan(f, auth, other, &plan.body["plan_token"], expected, suffix).await;
    let timeline = input["timeline_id"].as_str().unwrap();
    let before:(i64,i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?").bind(timeline).fetch_one(&f.app.db).await.unwrap();
    let session = f
        .playback_adapter(
            "POST",
            "/playback/viewing-sessions",
            auth,
            Some(json!({"delivery_id":id,"expected_viewing_revision":before.2.to_string()})),
            Some(&format!("generated-viewing-{suffix}")),
        )
        .await;
    assert_eq!(session.status, StatusCode::CREATED, "{:?}", session.body);
    assert_eq!(session.body["timeline_id"], timeline);
    let after:(i64,i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?").bind(timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!((after.0, after.1), (before.0, before.1));
    assert_eq!(after.2, before.2 + 1);
    if suffix == "online" {
        super::native_delivery_controls::exercise(
            f,
            auth,
            other,
            &delivery.body,
            &session.body,
            &plan.body["plan_token"],
        )
        .await;
    }
    let ticket=f.playback_adapter("POST","/content-access",auth,Some(json!({"purpose":"playback","delivery_id":id,"generation":generation,"file":null,"download_id":null,"ttl_seconds":60})),Some(&format!("generated-ticket-{suffix}"))).await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{:?}", ticket.body);
    let url = ticket.body["url"].as_str().unwrap();
    let bytes = f.playback_raw("GET", url, "", None, None, true).await;
    let status = bytes.status();
    let bytes = bytes.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::PARTIAL_CONTENT,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(bytes.as_ref(), &expected[..16]);
    if suffix == "online" {
        // Both fixture callers are genuinely paired admins. Temporarily remove
        // this owner's playback/viewing authority; the other remains usable.
        let me = f
            .call("GET", "/api/v2/me", None, &[("authorization", auth)])
            .await;
        assert_eq!(me.status, StatusCode::OK);
        let device = me.body["device_id"].as_str().unwrap();
        let policy_path = format!("/api/v2/devices/{device}/policy");
        let operator = format!("Bearer {OPERATOR}");
        let current = f
            .call("GET", &policy_path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(current.status, StatusCode::OK);
        let mut original = current.body.clone();
        original.as_object_mut().unwrap().remove("revision");
        let mut reduced = original.clone();
        reduced["permissions"] = json!(["catalog:read"]);
        let changed = f
            .call(
                "PUT",
                &policy_path,
                Some(reduced),
                &[
                    ("authorization", &operator),
                    ("if-match", current.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
        super::native_delivery_controls::withdrawn(f, auth, &delivery.body, &session.body).await;
        assert_eq!(
            f.playback_raw("GET", url, "", None, None, true)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        let events = format!(
            "/playback/viewing-sessions/{}/events",
            session.body["id"].as_str().unwrap()
        );
        let denied = f.playback_adapter("POST", &events, auth, Some(json!({"event_id":"withdrawn-playback-event","sequence":"1","delivery_generation":generation,"position_ms":750,"status":"playing"})), None).await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
        assert_eq!(
            f.playback_adapter(
                "DELETE",
                &format!("/playback/delivery-sessions/{id}"),
                auth,
                None,
                None
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        let unchanged:(i64,i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?").bind(timeline).fetch_one(&f.app.db).await.unwrap();
        assert_eq!(unchanged, after, "withdrawn caller must not mutate history");
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
    }
    let closed = f
        .playback_adapter(
            "DELETE",
            &format!("/playback/delivery-sessions/{id}"),
            auth,
            None,
            None,
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        f.playback_raw("GET", url, "", None, None, true)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

/// Same-version prepared replan, activation and byte proof through the adapter.
async fn exercise_replan(
    f: &Fixture,
    auth: &str,
    other: &str,
    token: &Value,
    expected: &[u8],
    suffix: &str,
) {
    let created = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            auth,
            Some(json!({"plan_token":token,"start_ms":0})),
            Some(&format!("prepared-replan-create-{suffix}")),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap();
    let generation = &created.body["active"]["generation"];
    let path = format!("/playback/delivery-sessions/{id}/changes");
    let seek = json!({"kind":"seek","expected_generation":generation,"position_ms":0});
    let sought = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(seek.clone()),
            Some("prepared-seek-command"),
        )
        .await;
    assert_eq!(sought.status, StatusCode::ACCEPTED, "{:?}", sought.body);
    assert_eq!(sought.body["pending"]["operation"], "prepared");
    assert_eq!(sought.body["pending"]["transport"], "http_range");
    let seek_replay = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(seek),
            Some("prepared-seek-command"),
        )
        .await;
    assert_eq!(seek_replay.body, sought.body);
    assert_eq!(seek_replay.headers["idempotent-replayed"], "true");
    let seek_generation = sought.body["pending"]["generation"].as_str().unwrap();
    let seek_active = f
        .playback_adapter(
            "POST",
            &format!("/playback/delivery-sessions/{id}/generations/{seek_generation}/activate"),
            auth,
            Some(json!({"expected_active_generation":generation})),
            Some("prepared-seek-activate"),
        )
        .await;
    assert_eq!(seek_active.status, StatusCode::OK, "{:?}", seek_active.body);
    assert_eq!(seek_active.body["active"]["generation"], seek_generation);
    let bytes = f
        .playback_raw(
            "GET",
            &format!("/streams/{id}/{seek_generation}/content"),
            auth,
            None,
            None,
            true,
        )
        .await;
    assert_eq!(bytes.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        bytes
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &expected[..16]
    );
    let generation = &seek_active.body["active"]["generation"];
    let body = json!({"kind":"replan","expected_generation":generation,"position_ms":0,"plan_token":token});
    for (actor, status) in [
        ("", StatusCode::UNAUTHORIZED),
        (other, StatusCode::NOT_FOUND),
    ] {
        let denied = f
            .playback_adapter(
                "POST",
                &path,
                actor,
                Some(body.clone()),
                Some("prepared-replan-command"),
            )
            .await;
        assert_eq!(denied.status, status, "{:?}", denied.body);
    }
    let staged = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(body.clone()),
            Some("prepared-replan-command"),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{:?}", staged.body);
    assert_eq!(staged.body["pending"]["operation"], "prepared");
    assert_eq!(staged.body["pending"]["transport"], "http_range");
    let replay = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(body.clone()),
            Some("prepared-replan-command"),
        )
        .await;
    assert_eq!(replay.body, staged.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let mut changed = body;
    changed["position_ms"] = json!(1);
    let conflict = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(changed),
            Some("prepared-replan-command"),
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.body["code"], "idempotency_conflict");
    let next = staged.body["pending"]["generation"].as_str().unwrap();
    let activated = f
        .playback_adapter(
            "POST",
            &format!("/playback/delivery-sessions/{id}/generations/{next}/activate"),
            auth,
            Some(json!({"expected_active_generation":generation})),
            Some("prepared-replan-activate"),
        )
        .await;
    assert_eq!(activated.status, StatusCode::OK, "{:?}", activated.body);
    assert_eq!(activated.body["active"]["generation"], next);
    let bytes = f
        .playback_raw(
            "GET",
            &format!("/streams/{id}/{next}/content"),
            auth,
            None,
            None,
            true,
        )
        .await;
    assert_eq!(bytes.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        bytes
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &expected[..16]
    );
    let closed = f
        .playback_adapter(
            "DELETE",
            &format!("/playback/delivery-sessions/{id}"),
            auth,
            None,
            None,
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
}
