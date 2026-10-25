//! Whole-original range controls through the actual public router.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use http_body_util::BodyExt;
use serde_json::{Value, json};

pub struct Receipts {
    id: String,
    replan: Value,
    replan_ack: Value,
    seek: Value,
    seek_ack: Value,
    activate: Value,
    activate_path: String,
    activate_ack: Value,
}

pub async fn exercise(
    f: &Fixture,
    auth: &str,
    foreign: &str,
    token: &Value,
    timeline: &str,
    bytes: &[u8],
) -> Receipts {
    let before: (i64,i64,i64) = sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(timeline).fetch_one(&f.app.db).await.unwrap();
    let created = f
        .playback_adapter(
            "POST",
            "/playback/delivery-sessions",
            auth,
            Some(json!({"plan_token":token,"start_ms":0})),
            Some("native-range-control-create"),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    let generation = created.body["active"]["generation"].clone();
    let path = format!("/playback/delivery-sessions/{id}/changes");
    let seek = json!({"kind":"seek","expected_generation":generation,"position_ms":500});
    for (actor, status) in [
        ("", StatusCode::UNAUTHORIZED),
        (foreign, StatusCode::NOT_FOUND),
    ] {
        let denied = f
            .playback_adapter(
                "POST",
                &path,
                actor,
                Some(seek.clone()),
                Some("native-range-control-seek"),
            )
            .await;
        assert_eq!(denied.status, status, "{:?}", denied.body);
    }
    let replan = json!({"kind":"replan","expected_generation":generation,"position_ms":500,"plan_token":token});
    let replanned = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(replan.clone()),
            Some("native-range-control-replan"),
        )
        .await;
    assert_eq!(
        replanned.status,
        StatusCode::ACCEPTED,
        "{:?}",
        replanned.body
    );
    assert_eq!(replanned.body["active"]["generation"], generation);
    assert_eq!(replanned.body["pending"]["transport"], "http_range");
    assert_eq!(replanned.body["pending"]["operation"], "original");
    let replan_retry = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(replan.clone()),
            Some("native-range-control-replan"),
        )
        .await;
    assert_eq!(replan_retry.body, replanned.body);
    assert_eq!(replan_retry.headers["idempotent-replayed"], "true");
    let staged = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(seek.clone()),
            Some("native-range-control-seek"),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{:?}", staged.body);
    assert_eq!(staged.body["active"]["generation"], generation);
    assert_eq!(staged.body["pending"]["transport"], "http_range");
    assert_eq!(staged.body["pending"]["operation"], "original");
    let next = staged.body["pending"]["generation"].as_str().unwrap();
    let replay = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(seek.clone()),
            Some("native-range-control-seek"),
        )
        .await;
    assert_eq!(replay.body, staged.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let mut changed = seek.clone();
    changed["position_ms"] = json!(600);
    assert_eq!(
        f.playback_adapter(
            "POST",
            &path,
            auth,
            Some(changed),
            Some("native-range-control-seek")
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    let activate_path = format!("/playback/delivery-sessions/{id}/generations/{next}/activate");
    let activate = json!({"expected_active_generation":generation});
    // A genuine second paired administrator remains usable. Remove this
    // owner's playback permission through the actual policy CAS, then restore.
    let me = f
        .call("GET", "/api/v2/me", None, &[("authorization", auth)])
        .await;
    assert_eq!(me.status, StatusCode::OK);
    let policy_path = format!(
        "/api/v2/devices/{}/policy",
        me.body["device_id"].as_str().unwrap()
    );
    let operator = format!("Bearer {OPERATOR}");
    let policy = f
        .call("GET", &policy_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(policy.status, StatusCode::OK);
    let mut original = policy.body.clone();
    original.as_object_mut().unwrap().remove("revision");
    let mut reduced = original.clone();
    reduced["permissions"] = json!(["catalog:read"]);
    let removed = f
        .call(
            "PUT",
            &policy_path,
            Some(reduced),
            &[
                ("authorization", &operator),
                ("if-match", policy.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.body);
    for (target, body) in [
        (&path, &seek),
        (&path, &replan),
        (&activate_path, &activate),
    ] {
        let denied = f
            .playback_adapter(
                "POST",
                target,
                auth,
                Some(body.clone()),
                Some("native-range-revoked-command"),
            )
            .await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    }
    let restored = f
        .call(
            "PUT",
            &policy_path,
            Some(original),
            &[
                ("authorization", &operator),
                ("if-match", removed.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(restored.status, StatusCode::OK, "{:?}", restored.body);

    for (actor, status) in [
        ("", StatusCode::UNAUTHORIZED),
        (foreign, StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            f.playback_adapter(
                "POST",
                &activate_path,
                actor,
                Some(activate.clone()),
                Some("native-range-control-activate")
            )
            .await
            .status,
            status
        );
    }
    assert_eq!(
        f.playback_adapter(
            "POST",
            &activate_path,
            auth,
            Some(json!({"expected_active_generation":"999999"})),
            Some("native-range-control-stale")
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    let activated = f
        .playback_adapter(
            "POST",
            &activate_path,
            auth,
            Some(activate.clone()),
            Some("native-range-control-activate"),
        )
        .await;
    assert_eq!(activated.status, StatusCode::OK, "{:?}", activated.body);
    assert_eq!(activated.body["active"]["generation"], next);
    assert!(activated.body["pending"].is_null());
    let activation_replay = f
        .playback_adapter(
            "POST",
            &activate_path,
            auth,
            Some(activate.clone()),
            Some("native-range-control-activate"),
        )
        .await;
    assert_eq!(activation_replay.status, StatusCode::OK);
    assert_eq!(activation_replay.body, activated.body);
    assert_eq!(activation_replay.headers["idempotent-replayed"], "true");
    let conflict = f
        .playback_adapter(
            "POST",
            &activate_path,
            auth,
            Some(json!({"expected_active_generation":"999999"})),
            Some("native-range-control-activate"),
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{:?}", conflict.body);
    assert_eq!(conflict.body["code"], "idempotency_conflict");

    let stale = f
        .playback_adapter(
            "POST",
            &path,
            auth,
            Some(seek.clone()),
            Some("native-range-control-stale-seek"),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.body);
    let ticket = f.playback_adapter("POST","/content-access",auth,
        Some(json!({"purpose":"playback","delivery_id":id,"generation":next,"file":null,"download_id":null,"ttl_seconds":60})),Some("native-range-control-ticket")).await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{:?}", ticket.body);
    let range = f
        .playback_raw(
            "GET",
            ticket.body["url"].as_str().unwrap(),
            "",
            None,
            None,
            true,
        )
        .await;
    assert_eq!(range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &bytes[..16]
    );
    let after: (i64,i64,i64) = sqlx::query_as("SELECT position_ms,manual_epoch,revision FROM timeline_viewing WHERE profile_id='default' AND timeline_id=?")
        .bind(timeline).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        after, before,
        "delivery controls do not submit viewing progress"
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
    assert_eq!(closed.status, StatusCode::NO_CONTENT, "{:?}", closed.body);
    Receipts {
        id,
        replan,
        replan_ack: replanned.body,
        seek,
        seek_ack: staged.body,
        activate,
        activate_path,
        activate_ack: activated.body,
    }
}

pub async fn after_restart(f: &Fixture, auth: &str, foreign: &str, r: &Receipts) {
    for (path, body, key, status, ack) in [
        (
            format!("/playback/delivery-sessions/{}/changes", r.id),
            &r.replan,
            "native-range-control-replan",
            StatusCode::ACCEPTED,
            &r.replan_ack,
        ),
        (
            format!("/playback/delivery-sessions/{}/changes", r.id),
            &r.seek,
            "native-range-control-seek",
            StatusCode::ACCEPTED,
            &r.seek_ack,
        ),
        (
            r.activate_path.clone(),
            &r.activate,
            "native-range-control-activate",
            StatusCode::OK,
            &r.activate_ack,
        ),
    ] {
        let replay = f
            .playback_adapter("POST", &path, auth, Some(body.clone()), Some(key))
            .await;
        assert_eq!(replay.status, status, "{:?}", replay.body);
        assert_eq!(&replay.body, ack);
        assert_eq!(replay.headers["idempotent-replayed"], "true");
        assert_eq!(
            f.playback_adapter("POST", &path, foreign, Some(body.clone()), Some(key))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        let new_command = f
            .playback_adapter(
                "POST",
                &path,
                auth,
                Some(body.clone()),
                Some("native-range-new-after-restart"),
            )
            .await;
        assert_eq!(
            new_command.status,
            StatusCode::CONFLICT,
            "new command after restart at {path}: {:?}",
            new_command.body
        );
    }
}

/// Even exact historical receipts require current source visibility. This is
/// called after real discovery has observed the original occurrence missing.
pub async fn missing_source(f: &Fixture, auth: &str, r: &Receipts) {
    for (path, body, key) in [
        (
            format!("/playback/delivery-sessions/{}/changes", r.id),
            &r.replan,
            "native-range-control-replan",
        ),
        (
            format!("/playback/delivery-sessions/{}/changes", r.id),
            &r.seek,
            "native-range-control-seek",
        ),
        (
            r.activate_path.clone(),
            &r.activate,
            "native-range-control-activate",
        ),
    ] {
        let refused = f
            .playback_adapter("POST", &path, auth, Some(body.clone()), Some(key))
            .await;
        assert_eq!(refused.status, StatusCode::NOT_FOUND, "{:?}", refused.body);
    }
}
