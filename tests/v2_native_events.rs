//! Real outer SSE hints use current native work membership, never frozen joins.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale as playscale_app;
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;
#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

struct Stream {
    body: Body,
    buffer: String,
}
impl Stream {
    async fn next(&mut self) -> Option<Value> {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                while let Some(end) = self.buffer.find("\n\n") {
                    let record: String = self.buffer.drain(..end + 2).collect();
                    if let Some(data) = record.lines().find_map(|l| l.strip_prefix("data:")) {
                        return Some(serde_json::from_str(data.trim()).unwrap());
                    }
                }
                let frame = self.body.frame().await?.unwrap();
                if let Ok(bytes) = frame.into_data() {
                    self.buffer.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
        })
        .await
        .expect("SSE current disclosure deadline")
    }
}
async fn stream(f: &Fixture, auth: &str, cursor: Option<&str>) -> Stream {
    let mut request = Request::builder()
        .uri("/api/v2/events")
        .header("host", "127.0.0.1:8787")
        .header("authorization", auth);
    if let Some(cursor) = cursor {
        request = request.header("last-event-id", cursor);
    }
    let response = playscale_app::api::router(f.app.clone(), None)
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    Stream {
        body: response.into_body(),
        buffer: String::new(),
    }
}
async fn policy(f: &Fixture, device: &str, libraries: &[&str], permissions: &[&str]) {
    let auth = format!("Bearer {OPERATOR}");
    let current = f
        .call(
            "GET",
            &format!("/api/v2/devices/{device}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let changed = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({
        "library_ids":libraries,"permissions":permissions,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]
    })), &[("authorization", &auth), ("if-match", current.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
}
async fn change_work(f: &Fixture, title: &str) {
    let auth = format!("Bearer {OPERATOR}");
    let path = "/api/v2/catalog/items/work/metadata";
    let current = f.call("GET", path, None, &[("authorization", &auth)]).await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let changed = f
        .call(
            "PUT",
            &format!("{path}/contributions/local"),
            Some(json!({
                "values":{"title":title},"tags":[],"excluded_tags":[],"locked_fields":[]
            })),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
}
#[tokio::test]
async fn native_events_current_scope_replay_policy_revocation_and_restart() {
    let mut f = Fixture::new().await;
    let library = f
        .seed_preserved_timeline("original.txt", b"native event exact source")
        .await;
    f.cutover_source(Some(&library)).await;
    let operator = format!("Bearer {OPERATOR}");
    let _reserve = f.pair(&["default"], &["system:admin"]).await;
    let other = f
        .call(
            "POST",
            "/api/v2/libraries",
            Some(json!({"name":"Hidden scope","kind":"mixed","language":"en","source_ids":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "native-event-library-0001"),
            ],
        )
        .await;
    assert_eq!(other.status, StatusCode::CREATED, "{}", other.body);
    let other_id = other.body["id"].as_str().unwrap().to_owned();
    let (device, token) = f.pair(&["default"], &["events:read", "catalog:read"]).await;
    let (foreign_device, foreign_token) =
        f.pair(&["default"], &["events:read", "catalog:read"]).await;
    policy(&f, &device, &[&library], &["events:read", "catalog:read"]).await;
    policy(
        &f,
        &foreign_device,
        &[&other_id],
        &["events:read", "catalog:read"],
    )
    .await;
    let auth = format!("Bearer {token}");
    let foreign_auth = format!("Bearer {foreign_token}");
    assert_eq!(
        f.call("GET", "/api/v2/events", None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.call("HEAD", "/api/v2/events", None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mut allowed = stream(&f, &auth, None).await;
    let initial = allowed.next().await.unwrap();
    assert_eq!(initial["reason"], "initial_snapshot");
    let cursor = initial["cursor"].as_str().unwrap().to_owned();
    let mut foreign = stream(&f, &foreign_auth, None).await;
    assert_eq!(foreign.next().await.unwrap()["reason"], "initial_snapshot");
    let before: i64 = sqlx::query_scalar("SELECT coalesce(max(id),0) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    change_work(&f, "Visible to one current native scope").await;
    // Exact native ACK hint plus disposable item-projection update hint.
    let backlog: Vec<(String, String)> =
        sqlx::query_as("SELECT topic,resource_id FROM change_events WHERE id>? ORDER BY id")
            .bind(before)
            .fetch_all(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        backlog,
        vec![
            ("catalog".into(), "work".into()),
            ("catalog".into(), "work".into())
        ]
    );
    let mut prior_cursor = None;
    for _ in 0..2 {
        let hint = allowed.next().await.unwrap();
        assert_eq!(hint["resource_id"], "work");
        assert_eq!(hint["resource_type"], "catalog_item");
        assert_ne!(prior_cursor.as_ref(), Some(&hint["cursor"]));
        prior_cursor = Some(hint["cursor"].clone());
        let hidden = foreign.next().await.unwrap();
        assert_eq!(hidden["reason"], "scope_unknown");
        assert!(
            hidden["resource_id"].is_null(),
            "hidden object must not be named"
        );
    }
    // Profile authority is independent of catalog scope. A hidden profile is
    // skipped, while a later granted profile mutation is a delivered sentinel.
    let hidden_profile = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Private profile"})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "native-event-profile-0001"),
            ],
        )
        .await;
    assert_eq!(
        hidden_profile.status,
        StatusCode::CREATED,
        "{}",
        hidden_profile.body
    );
    let default = f
        .call(
            "GET",
            "/api/v2/profiles/default",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(default.status, StatusCode::OK);
    let updated = f
        .call(
            "PUT",
            "/api/v2/profiles/default",
            Some(json!({"name":"Visible profile"})),
            &[
                ("authorization", &operator),
                ("if-match", default.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    for stream in [&mut allowed, &mut foreign] {
        let profile = stream.next().await.unwrap();
        assert_eq!(profile["resource_type"], "profile");
        assert_eq!(profile["resource_id"], "default");
    }
    drop(allowed);
    let mut replay = stream(&f, &auth, Some(&cursor)).await;
    assert_eq!(replay.next().await.unwrap()["resource_id"], "work");
    // A foreign cursor cannot replay another device's visible history.
    let mut foreign_cursor = stream(&f, &foreign_auth, Some(&cursor)).await;
    let foreign_reset = foreign_cursor.next().await.unwrap();
    assert_eq!(foreign_reset["reason"], "principal_changed");
    assert!(foreign_reset["resource_id"].is_null());
    drop(foreign_cursor);
    // Suspend the client, remove membership authority, then change the work.
    policy(&f, &device, &[&other_id], &["events:read", "catalog:read"]).await;
    change_work(&f, "No longer visible to the suspended subscriber").await;
    let reset = replay.next().await.unwrap();
    assert_eq!(reset["reason"], "policy_changed");
    assert!(reset["resource_id"].is_null());
    let reset_cursor = reset["cursor"].as_str().unwrap().to_owned();
    drop((replay, foreign));
    f.close().await;
    f.open().await;
    let mut reopened = stream(&f, &auth, Some(&reset_cursor)).await;
    change_work(&f, "Hidden after actual native owner restart").await;
    let hidden = reopened.next().await.unwrap();
    assert_eq!(hidden["reason"], "scope_unknown");
    assert!(hidden["resource_id"].is_null());
    policy(&f, &device, &[&other_id], &["catalog:read"]).await;
    assert!(
        reopened.next().await.is_none(),
        "current EventsRead loss ends stream"
    );
    assert_eq!(
        f.call("GET", "/api/v2/events", None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    f.close().await;
}

#[tokio::test]
async fn native_events_schedule_moves_and_scan_requests_use_current_owners() {
    let mut f = Fixture::new().await;
    let library = f
        .seed_preserved_timeline("source.txt", b"native scan event source")
        .await;
    f.cutover_source(Some(&library)).await;
    let operator = format!("Bearer {OPERATOR}");
    let (_, admin_token) = f.pair(&["default"], &["system:admin"]).await;
    let admin = format!("Bearer {admin_token}");
    let other_root = f._dir.path().join("other-schedule-source");
    std::fs::create_dir(&other_root).unwrap();
    let other_source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Other schedule source","root_path":other_root,"exclusions":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "event-schedule-source-01"),
            ],
        )
        .await;
    assert_eq!(
        other_source.status,
        StatusCode::CREATED,
        "{}",
        other_source.body
    );
    let other = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Other schedule scope","kind":"mixed","language":"en","source_ids":[other_source.body["id"]]})),
        &[("authorization", &operator), ("idempotency-key", "event-schedule-library-01")]).await;
    assert_eq!(other.status, StatusCode::CREATED, "{}", other.body);
    let other_id = other.body["id"].as_str().unwrap();
    let permissions = ["events:read", "catalog:read", "sources:manage"];
    let (device, token) = f.pair(&["default"], &permissions).await;
    let (foreign_device, foreign_token) = f.pair(&["default"], &permissions).await;
    policy(&f, &device, &[&library], &permissions).await;
    policy(&f, &foreign_device, &[other_id], &permissions).await;
    let auth = format!("Bearer {token}");
    let foreign_auth = format!("Bearer {foreign_token}");
    let mut allowed = stream(&f, &auth, None).await;
    let mut foreign = stream(&f, &foreign_auth, None).await;
    assert_eq!(allowed.next().await.unwrap()["reason"], "initial_snapshot");
    assert_eq!(foreign.next().await.unwrap()["reason"], "initial_snapshot");
    let input = json!({"library_id":library,"task":"scan","interval_seconds":3600,"enabled":false});
    let headers = [
        ("authorization", admin.as_str()),
        ("idempotency-key", "native-event-schedule-01"),
    ];
    let created = f
        .call("POST", "/api/v2/schedules", Some(input.clone()), &headers)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap();
    assert_eq!(allowed.next().await.unwrap()["resource_id"], id);
    // Hidden schedule creation is withheld; the later scope move is its first visible hint.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM change_events WHERE topic='schedules' AND resource_id=?",
    )
    .bind(id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let replay = f
        .call("POST", "/api/v2/schedules", Some(input), &headers)
        .await;
    assert_eq!(replay.body, created.body);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM change_events WHERE topic='schedules' AND resource_id=?"
        )
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap(),
        count
    );
    let path = format!("/api/v2/schedules/{id}");
    let moved = f.call("PUT", &path, Some(json!({"library_id":other_id,"task":"scan","interval_seconds":3600,"enabled":false})),
        &[("authorization", &admin), ("if-match", created.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);
    let reset = allowed.next().await.unwrap();
    assert_eq!(reset["reason"], "scope_unknown");
    assert!(reset["resource_id"].is_null());
    assert_eq!(foreign.next().await.unwrap()["resource_id"], id);
    let deleted = f
        .call(
            "DELETE",
            &path,
            None,
            &[
                ("authorization", &admin),
                ("if-match", moved.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    let reset = foreign.next().await.unwrap();
    assert_eq!(reset["reason"], "scope_unknown");
    assert!(reset["resource_id"].is_null());
    // An actual queued request belongs to the requester and current library.
    let path = format!("/api/v2/libraries/{library}/scans");
    let input = json!({"mode":"verify","require_complete":true});
    let headers = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "native-event-scan-0001"),
    ];
    let created = f.call("POST", &path, Some(input.clone()), &headers).await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap();
    assert_eq!(allowed.next().await.unwrap()["resource_id"], id);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM change_events WHERE topic='scan' AND resource_id=?",
    )
    .bind(id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let replay = f.call("POST", &path, Some(input), &headers).await;
    assert_eq!(replay.body, created.body);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM change_events WHERE topic='scan' AND resource_id=?"
        )
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap(),
        count
    );
    let cancelled = f
        .call(
            "POST",
            &format!("/api/v2/scans/{id}/cancel"),
            None,
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-event-cancel-001"),
            ],
        )
        .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(allowed.next().await.unwrap()["resource_id"], id);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM change_events WHERE topic='scan' AND resource_id=?",
    )
    .bind(id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let replay = f
        .call(
            "POST",
            &format!("/api/v2/scans/{id}/cancel"),
            None,
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-event-cancel-001"),
            ],
        )
        .await;
    assert_eq!(replay.body, cancelled.body);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM change_events WHERE topic='scan' AND resource_id=?"
        )
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap(),
        count
    );
    // A known visible profile sentinel proves hidden request hints were skipped.
    let current = f
        .call(
            "GET",
            "/api/v2/profiles/default",
            None,
            &[("authorization", &admin)],
        )
        .await;
    let sentinel = f
        .call(
            "PUT",
            "/api/v2/profiles/default",
            Some(json!({"name":"Scan complete sentinel"})),
            &[
                ("authorization", &admin),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(sentinel.status, StatusCode::OK);
    assert_eq!(foreign.next().await.unwrap()["resource_id"], "default");
    drop((allowed, foreign));
    f.close().await;
}

async fn event_count(f: &Fixture, topic: &str, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM change_events WHERE topic=? AND resource_id=?")
        .bind(topic)
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap()
}
async fn profile_sentinel(f: &Fixture, name: &str) {
    let auth = format!("Bearer {OPERATOR}");
    let current = f
        .call(
            "GET",
            "/api/v2/profiles/default",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(current.status, StatusCode::OK);
    let changed = f
        .call(
            "PUT",
            "/api/v2/profiles/default",
            Some(json!({"name":name})),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
}
#[tokio::test]
async fn native_events_source_library_ack_rollback_replay_and_hidden_scope() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let operator = format!("Bearer {OPERATOR}");
    let _reserve = f.pair(&["default"], &["system:admin"]).await;
    let permissions = ["events:read", "catalog:read", "sources:manage"];
    let (device, token) = f.pair(&["default"], &permissions).await;
    let (_, hidden_token) = f.pair(&["default"], &["events:read", "catalog:read"]).await;
    let auth = format!("Bearer {token}");
    let hidden_auth = format!("Bearer {hidden_token}");
    let mut visible = stream(&f, &auth, None).await;
    let mut hidden = stream(&f, &hidden_auth, None).await;
    assert_eq!(visible.next().await.unwrap()["reason"], "initial_snapshot");
    assert_eq!(hidden.next().await.unwrap()["reason"], "initial_snapshot");
    let root = f._dir.path().join("event-source");
    std::fs::create_dir(&root).unwrap();
    let input = json!({"name":"Event source","root_path":root,"exclusions":[]});
    let headers = [
        ("authorization", operator.as_str()),
        ("idempotency-key", "native-event-source-0001"),
    ];
    let source = f
        .call("POST", "/api/v2/sources", Some(input.clone()), &headers)
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let source_id = source.body["id"].as_str().unwrap();
    let hint = visible.next().await.unwrap();
    assert_eq!(hint["resource_type"], "source");
    assert_eq!(hint["resource_id"], source_id);
    let replay = f
        .call("POST", "/api/v2/sources", Some(input), &headers)
        .await;
    assert_eq!(replay.body, source.body);
    assert_eq!(event_count(&f, "sources", source_id).await, 1);
    let input =
        json!({"name":"Event library","kind":"mixed","language":"en","source_ids":[source_id]});
    let headers = [
        ("authorization", operator.as_str()),
        ("idempotency-key", "native-event-library-0002"),
    ];
    let library = f
        .call("POST", "/api/v2/libraries", Some(input.clone()), &headers)
        .await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let library_id = library.body["id"].as_str().unwrap();
    let replay = f
        .call("POST", "/api/v2/libraries", Some(input.clone()), &headers)
        .await;
    assert_eq!(replay.body, library.body);
    assert_eq!(event_count(&f, "libraries", library_id).await, 1);
    profile_sentinel(&f, "Source/library hidden sentinel").await;
    assert_eq!(hidden.next().await.unwrap()["resource_id"], "default");
    assert_eq!(visible.next().await.unwrap()["resource_id"], "default");
    policy(&f, &device, &[library_id], &permissions).await;
    assert_eq!(visible.next().await.unwrap()["reason"], "policy_changed");
    let path = format!("/api/v2/libraries/{library_id}");
    let mut replacement = input.clone();
    replacement["name"] = json!("Changed native library");
    // A real ACK-storage refusal may follow the native effect. No hint escapes
    // the rolled-back Motion transaction; retry reconciles that exact receipt.
    sqlx::query("CREATE TRIGGER event_fixture_refuse_library BEFORE INSERT ON change_events WHEN NEW.topic='libraries' BEGIN SELECT RAISE(ABORT,'injected event ACK failure'); END")
        .execute(&f.app.db).await.unwrap();
    let headers = [
        ("authorization", operator.as_str()),
        ("if-match", library.headers["etag"].to_str().unwrap()),
    ];
    let failed = f
        .call("PUT", &path, Some(replacement.clone()), &headers)
        .await;
    assert!(
        failed.status.is_server_error(),
        "{} {}",
        failed.status,
        failed.body
    );
    assert_eq!(event_count(&f, "libraries", library_id).await, 1);
    sqlx::query("DROP TRIGGER event_fixture_refuse_library")
        .execute(&f.app.db)
        .await
        .unwrap();
    let changed = f
        .call("PUT", &path, Some(replacement.clone()), &headers)
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(visible.next().await.unwrap()["resource_id"], library_id);
    assert_eq!(event_count(&f, "libraries", library_id).await, 2);
    let replay = f
        .call("PUT", &path, Some(replacement.clone()), &headers)
        .await;
    assert_eq!(replay.body, changed.body);
    assert_eq!(event_count(&f, "libraries", library_id).await, 2);
    let stale = f.call("PUT", &path, Some(input), &headers).await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    assert_eq!(event_count(&f, "libraries", library_id).await, 2);
    replacement["source_ids"] = json!([]);
    let detached = f
        .call(
            "PUT",
            &path,
            Some(replacement),
            &[
                ("authorization", &operator),
                ("if-match", changed.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(detached.status, StatusCode::OK, "{}", detached.body);
    assert_eq!(visible.next().await.unwrap()["resource_id"], library_id);
    let source_path = format!("/api/v2/sources/{source_id}");
    let current = f
        .call("GET", &source_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK);
    let headers = [
        ("authorization", operator.as_str()),
        ("if-match", current.headers["etag"].to_str().unwrap()),
    ];
    let removed = f.call("DELETE", &source_path, None, &headers).await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
    let hint = visible.next().await.unwrap();
    assert_eq!(hint["resource_id"], source_id);
    assert_eq!(hint["kind"], "deleted");
    assert_eq!(
        f.call("DELETE", &source_path, None, &headers).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(event_count(&f, "sources", source_id).await, 2);
    profile_sentinel(&f, "Withdrawal hidden sentinel").await;
    assert_eq!(hidden.next().await.unwrap()["resource_id"], "default");
    drop((visible, hidden));
    f.close().await;
    f.open().await;
    assert_eq!(event_count(&f, "sources", source_id).await, 2);
    assert_eq!(event_count(&f, "libraries", library_id).await, 3);
    f.close().await;
}
