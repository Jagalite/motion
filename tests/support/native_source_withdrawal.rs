//! Actual deployment withdrawal without changing native catalog generation.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use serde_json::{Value, json};

pub async fn exercise(
    f: &mut Fixture,
    source: &str,
    input_file: &str,
    input_revision: &str,
    output_file: &str,
    output_revision: &str,
    output_bytes: &[u8],
    auth: &str,
    mut plan_input: Value,
) {
    let operator = format!("Bearer {OPERATOR}");
    let path = format!("/api/v2/sources/{source}");
    let before = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(before.status, StatusCode::OK, "{:?}", before.body);
    let tag = before.headers["etag"].to_str().unwrap().to_owned();
    let original_path =
        format!("/api/v2/media/files/{input_file}/content?revision={input_revision}");
    let cached = f
        .call(
            "GET",
            &original_path,
            None,
            &[("authorization", auth), ("range", "bytes=0-15")],
        )
        .await;
    assert_eq!(
        cached.status,
        StatusCode::PARTIAL_CONTENT,
        "{:?}",
        cached.body
    );
    let (timeline, version): (String, String) = sqlx::query_as(
        "SELECT v.timeline_id,v.id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=? AND v.origin='original' LIMIT 1",
    ).bind(input_file).fetch_one(&f.app.db).await.unwrap();
    plan_input["timeline_id"] = json!(timeline);
    plan_input["version_id"] = json!(version);
    plan_input["source"] = json!({"file_id":input_file,"file_revision":input_revision});
    plan_input["quality"]["mode"] = json!("original");
    let plan = f
        .call(
            "POST",
            "/api/v2/playback/plans",
            Some(plan_input),
            &[("authorization", auth)],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    assert_eq!(plan.body["status"], "ready", "{:?}", plan.body);
    let created = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            auth,
            Some(json!({"plan_token":plan.body["plan_token"],"start_ms":0})),
            Some("withdrawal-replan-create"),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let changes = format!(
        "/playback/delivery-sessions/{}/changes",
        created.body["id"].as_str().unwrap()
    );
    let replan = json!({"kind":"replan","expected_generation":created.body["active"]["generation"],"position_ms":0,"plan_token":plan.body["plan_token"]});
    let staged = f
        .playback_adapter(
            "POST",
            &changes,
            auth,
            Some(replan.clone()),
            Some("withdrawal-replan"),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{:?}", staged.body);
    let generation: i64 =
        sqlx::query_scalar("SELECT generation FROM catabolic_projection_state WHERE singleton=1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let history: (i64, i64) = sqlx::query_as(
        "SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let removed = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", &tag)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.body);
    assert_eq!(removed.body["availability"], "unavailable");
    assert_eq!(
        removed.body["binding_revision"],
        before.body["binding_revision"]
    );
    assert_ne!(removed.body["revision"], before.body["revision"]);
    let denied = f
        .call(
            "GET",
            &original_path,
            None,
            &[("authorization", auth), ("range", "bytes=0-15")],
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND, "{:?}", denied.body);
    // Actual lifecycle withdrawal revokes both a new effect and historical
    // receipt replay. Removing the scheduling predicate must not weaken this.
    for key in ["withdrawal-replan", "withdrawal-replan-new"] {
        let denied = f
            .playback_adapter("POST", &changes, auth, Some(replan.clone()), Some(key))
            .await;
        assert_eq!(denied.status, StatusCode::NOT_FOUND, "{:?}", denied.body);
    }
    super::assert_generated_range(f, output_file, output_revision, auth, output_bytes).await;
    let after: i64 =
        sqlx::query_scalar("SELECT generation FROM catabolic_projection_state WHERE singleton=1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        after, generation,
        "Motion withdrawal does not impersonate a native mutation"
    );
    let replay = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", &tag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{:?}", replay.body);
    assert_eq!(replay.body, removed.body);
    f.close().await;
    f.open().await;
    let current = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
    assert_eq!(current.body["availability"], "unavailable");
    assert_eq!(current.body["revision"], removed.body["revision"]);
    assert_eq!(
        f.call("GET", &original_path, None, &[("authorization", auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    super::assert_generated_range(f, output_file, output_revision, auth, output_bytes).await;
    let retained: (i64, i64) = sqlx::query_as(
        "SELECT position_ms,manual_epoch FROM timeline_viewing WHERE profile_id='default'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(retained, history);
}
