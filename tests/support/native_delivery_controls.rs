//! Candidate exact public controls through the full router, including native
//! gates, auth extraction, current native custody and existing reducers.
//! Source-staged coverage is not qualification until the actual fixture passes.
use super::native_application_fixture::{Fixture, Reply};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(
    f: &Fixture,
    method: &str,
    path: &str,
    auth: &str,
    body: Option<Value>,
    tag: Option<&str>,
) -> Reply {
    call_app(&f.app, method, path, auth, body, tag).await
}

async fn call_app(
    app: &playscale::App,
    method: &str,
    path: &str,
    auth: &str,
    body: Option<Value>,
    tag: Option<&str>,
) -> Reply {
    let (server, epoch) = playscale::v2::system::server_identity(app).await.unwrap();
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v2{path}"))
        .header("host", "127.0.0.1:8787")
        .header("authorization", auth)
        .header("x-motion-viewing-epoch", format!("{server}:{epoch}"));
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(tag) = tag {
        request = request.header("if-match", tag);
    }
    let response = playscale::api::router(app.clone(), None)
        .oneshot(
            request
                .body(body.map(|b| Body::from(b.to_string())).unwrap_or_default())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    Reply {
        status,
        headers,
        body,
    }
}

pub async fn exercise(
    f: &Fixture,
    auth: &str,
    other: &str,
    delivery: &Value,
    session: &Value,
    plan_token: &Value,
) {
    let id = delivery["id"].as_str().unwrap();
    let generation = delivery["active"]["generation"].as_str().unwrap();
    let session_id = session["id"].as_str().unwrap();
    let path = format!("/playback/delivery-sessions/{id}");
    let session_path = format!("/playback/viewing-sessions/{session_id}");
    let read = call(f, "GET", &path, auth, None, None).await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
    assert_eq!(read.body["id"], id);
    assert_eq!(read.body["source"], delivery["source"]);
    assert_eq!(
        call(f, "GET", &path, other, None, None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(f, "GET", &path, "", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let current = call(f, "GET", &session_path, auth, None, None).await;
    assert_eq!(current.status, StatusCode::OK);
    assert_eq!(current.body["id"], session_id);
    assert_eq!(
        call(f, "GET", &session_path, "", None, None).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Viewing sessions are profile-owned, not principal-owned; a second
    // authorized device on the same profile may read the retained session.
    assert_eq!(
        call(f, "GET", &session_path, other, None, None)
            .await
            .status,
        StatusCode::OK
    );
    let timeline = session["timeline_id"].as_str().unwrap();
    let before:(i64,i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?").bind(timeline).fetch_one(&f.app.db).await.unwrap();
    let heartbeat_path = format!("{path}/heartbeat");
    let heartbeat =
        json!({"active_generation":generation,"playhead":{"sequence":"1","position_ms":125}});
    assert_eq!(
        call(
            f,
            "POST",
            &heartbeat_path,
            other,
            Some(heartbeat.clone()),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            f,
            "POST",
            &heartbeat_path,
            "",
            Some(heartbeat.clone()),
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let beat = call(f, "POST", &heartbeat_path, auth, Some(heartbeat), None).await;
    assert_eq!(beat.status, StatusCode::OK, "{:?}", beat.body);
    assert_eq!(beat.body["active"]["generation"], generation);
    let stale = call(
        f,
        "POST",
        &heartbeat_path,
        auth,
        Some(json!({"active_generation":"999999","playhead":null})),
        None,
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.body);
    let second = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            auth,
            Some(json!({"plan_token":plan_token,"start_ms":0})),
            Some(&format!("candidate-controls-{id}")),
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{:?}", second.body);
    let second_id = second.body["id"].as_str().unwrap();
    let rebind_path = format!("{session_path}/delivery");
    let tag = current.headers["etag"].to_str().unwrap();
    assert_eq!(
        call(
            f,
            "PUT",
            &rebind_path,
            "",
            Some(json!({"delivery_id":second_id})),
            Some(tag)
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    for (method, path) in [
        ("HEAD", path.clone()),
        ("HEAD", session_path.clone()),
        ("HEAD", format!("{path}/changes")),
        ("HEAD", format!("{path}/generations/{generation}/activate")),
    ] {
        assert_eq!(
            call(f, method, &path, auth, None, None).await.status,
            if method == "HEAD" && (path.ends_with("/changes") || path.ends_with("/activate")) {
                StatusCode::METHOD_NOT_ALLOWED
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            },
            "unqualified sibling {method} {path}"
        );
    }
    let foreign = call(
        f,
        "PUT",
        &rebind_path,
        other,
        Some(json!({"delivery_id":second_id})),
        Some(tag),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{:?}", foreign.body);
    let rebound = call(
        f,
        "PUT",
        &rebind_path,
        auth,
        Some(json!({"delivery_id":second_id})),
        Some(tag),
    )
    .await;
    assert_eq!(rebound.status, StatusCode::OK, "{:?}", rebound.body);
    assert_eq!(rebound.body["delivery_id"], second_id);
    let stale = call(
        f,
        "PUT",
        &rebind_path,
        auth,
        Some(json!({"delivery_id":id})),
        Some(tag),
    )
    .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    let restored = call(
        f,
        "PUT",
        &rebind_path,
        auth,
        Some(json!({"delivery_id":id})),
        Some(rebound.headers["etag"].to_str().unwrap()),
    )
    .await;
    assert_eq!(restored.status, StatusCode::OK);
    assert_eq!(restored.body["delivery_id"], id);
    let after:(i64,i64,i64)=sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?").bind(timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        after, before,
        "control and rebind must not rewrite viewing progress"
    );
    let closed = f
        .playback_adapter(
            "DELETE",
            &format!("/playback/delivery-sessions/{second_id}"),
            auth,
            None,
            None,
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    let ended = call(
        f,
        "GET",
        &format!("/playback/delivery-sessions/{second_id}"),
        auth,
        None,
        None,
    )
    .await;
    assert_eq!(ended.status, StatusCode::OK);
    assert_eq!(ended.body["status"], "closed");
    // Cold application view, not a full restart: ordinary empty delivery
    // runtime, the same admitted native owner/database, and genuine closed
    // records produced above. No workers or recovery effects are started.
    let mut cold = f.app.clone();
    let mut processing =
        playscale::processing::Runtime::new(f.app.processing.root.clone(), Default::default());
    processing.supervisor = f.app.processing.supervisor.clone();
    cold.processing = std::sync::Arc::new(processing);
    let cold_path = format!("/playback/delivery-sessions/{second_id}");
    let recorded = call_app(&cold, "GET", &cold_path, auth, None, None).await;
    assert_eq!(recorded.status, StatusCode::OK, "{:?}", recorded.body);
    assert_eq!(recorded.body["id"], second_id);
    assert_eq!(recorded.body["status"], "closed");
    assert_eq!(recorded.body["source"], ended.body["source"]);
    assert_eq!(
        call_app(&cold, "GET", &cold_path, other, None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call_app(&cold, "GET", &cold_path, "", None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(
        call(
            f,
            "POST",
            &format!("/playback/delivery-sessions/{second_id}/heartbeat"),
            auth,
            Some(json!({"active_generation":second.body["active"]["generation"],"playhead":null})),
            None
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
}

pub async fn withdrawn(f: &Fixture, auth: &str, delivery: &Value, session: &Value) {
    let id = delivery["id"].as_str().unwrap();
    let session_id = session["id"].as_str().unwrap();
    for (path, body) in [
        (
            format!("/playback/delivery-sessions/{id}/changes"),
            json!({"kind":"seek","expected_generation":delivery["active"]["generation"],"position_ms":0}),
        ),
        (
            format!(
                "/playback/delivery-sessions/{id}/generations/{}/activate",
                delivery["active"]["generation"].as_str().unwrap()
            ),
            json!({"expected_active_generation":delivery["active"]["generation"]}),
        ),
    ] {
        let result = f
            .playback_adapter(
                "POST",
                &path,
                auth,
                Some(body),
                Some("native-withdrawn-control"),
            )
            .await;
        assert_eq!(result.status, StatusCode::FORBIDDEN, "{:?}", result.body);
    }
    for (method, path, body, tag) in [
        (
            "GET",
            format!("/playback/delivery-sessions/{id}"),
            None,
            None,
        ),
        (
            "GET",
            format!("/playback/viewing-sessions/{session_id}"),
            None,
            None,
        ),
        (
            "POST",
            format!("/playback/delivery-sessions/{id}/heartbeat"),
            Some(json!({"active_generation":delivery["active"]["generation"],"playhead":null})),
            None,
        ),
        (
            "PUT",
            format!("/playback/viewing-sessions/{session_id}/delivery"),
            Some(json!({"delivery_id":id})),
            Some("\"r-1\""),
        ),
    ] {
        let result = call(f, method, &path, auth, body, tag).await;
        assert_eq!(
            result.status,
            StatusCode::FORBIDDEN,
            "{path}: {:?}",
            result.body
        );
    }
}

/// Real Fixture close/open plus startup recovery has already run at the callsite.
pub async fn after_restart(
    f: &Fixture,
    auth: &str,
    other: &str,
    delivery: &Value,
    session: &Value,
) {
    let id = delivery["id"].as_str().unwrap();
    let path = format!("/playback/delivery-sessions/{id}");
    let view = call(f, "GET", &path, auth, None, None).await;
    assert_eq!(view.status, StatusCode::OK, "{:?}", view.body);
    assert_eq!(view.body["status"], "closed");
    assert_eq!(view.body["source"], delivery["source"]);
    assert_eq!(
        call(f, "GET", &path, other, None, None).await.status,
        StatusCode::NOT_FOUND
    );
    let session_path = format!(
        "/playback/viewing-sessions/{}",
        session["id"].as_str().unwrap()
    );
    let retained = call(f, "GET", &session_path, auth, None, None).await;
    assert_eq!(retained.status, StatusCode::OK, "{:?}", retained.body);
    assert_eq!(retained.body["id"], session["id"]);
    assert_eq!(retained.body["timeline_id"], session["timeline_id"]);
}
