//! Motion-owned access administration through the full native-mode router.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use serde_json::{Value, json};

async fn proof(f: &Fixture, auth: &str, action: &str, target: &str) -> String {
    let created = f
        .call(
            "POST",
            "/api/v2/auth/proofs",
            Some(json!({"action":action,"target":target})),
            &[("authorization", auth)],
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap();
    let approved = f
        .call(
            "POST",
            &format!("/api/v2/auth/proofs/{id}/approve"),
            Some(json!({"code":created.body["code"]})),
            &[("authorization", &format!("Bearer {OPERATOR}"))],
        )
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
    id.to_owned()
}

pub async fn exercise() {
    let mut f = Fixture::new().await;
    let (admin_id, admin_token) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let (_reserve, _) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let (device, low_token) = f.pair(&["default"], &["catalog:read"]).await;
    let auth = format!("Bearer {admin_token}");
    let low = format!("Bearer {low_token}");
    let operator = format!("Bearer {OPERATOR}");
    f.cutover().await;
    for path in [
        "/api/v2/users",
        "/api/v2/access/setup",
        "/api/v2/admin/metrics",
    ] {
        assert_eq!(
            f.call("GET", path, None, &[]).await.status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.call("GET", path, None, &[("authorization", &low)])
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let allowed = f.call("GET", path, None, &[("authorization", &auth)]).await;
        assert_eq!(allowed.status, StatusCode::OK, "{path}: {:?}", allowed.body);
    }
    let setup = f
        .call(
            "POST",
            "/api/v2/access/setup",
            Some(json!({"name":"Native owner"})),
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(setup.status, StatusCode::CREATED, "{:?}", setup.body);
    let setup_replay = f
        .call(
            "POST",
            "/api/v2/access/setup",
            Some(json!({"name":"Native owner"})),
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(
        setup_replay.status,
        StatusCode::OK,
        "{:?}",
        setup_replay.body
    );
    assert_eq!(setup_replay.body["owner"]["id"], setup.body["owner"]["id"]);
    let library = f
        .call(
            "POST",
            "/api/v2/libraries",
            Some(json!({"name":"Access library","kind":"mixed","language":"en","source_ids":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "access-admin-native-library"),
            ],
        )
        .await;
    assert_eq!(library.status, StatusCode::CREATED, "{:?}", library.body);
    let input = json!({"name":"Native viewer","disabled":false,"profile_ids":["default"],"permissions":["catalog:read"],"policy":{"library_ids":[library.body["id"]],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]}});
    let key = "native-access-user-create";
    let no_proof = f
        .call(
            "POST",
            "/api/v2/users",
            Some(input.clone()),
            &[("authorization", &auth), ("idempotency-key", key)],
        )
        .await;
    assert_eq!(
        no_proof.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        no_proof.body
    );
    assert_eq!(no_proof.body["code"], "recent_auth_required");
    let grant = proof(&f, &auth, "user.admin", "new").await;
    let created = f
        .call(
            "POST",
            "/api/v2/users",
            Some(input.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", key),
                ("x-motion-action-proof", &grant),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/users/{id}");
    let listed = f
        .call("GET", "/api/v2/users", None, &[("authorization", &auth)])
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{:?}", listed.body);
    assert!(
        listed.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|user| user["id"] == id)
    );
    let replay = f
        .call(
            "POST",
            "/api/v2/users",
            Some(input.clone()),
            &[("authorization", &auth), ("idempotency-key", key)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{:?}", replay.body);
    assert_eq!(replay.body["id"], id);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let mut invalid = input.clone();
    invalid["policy"]["library_ids"] = json!(["absent-native-library"]);
    let rejected = f
        .call(
            "POST",
            "/api/v2/users",
            Some(invalid),
            &[
                ("authorization", &operator),
                ("idempotency-key", "native-access-unknown-library"),
            ],
        )
        .await;
    assert_eq!(rejected.body["code"], "unknown_library");
    let current = f
        .call("GET", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
    let old_tag = current.headers["etag"].to_str().unwrap();
    let mut renamed = input.clone();
    renamed["name"] = json!("Renamed native viewer");
    let updated = f
        .call(
            "PUT",
            &path,
            Some(renamed.clone()),
            &[("authorization", &operator), ("if-match", old_tag)],
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{:?}", updated.body);
    let stale = f
        .call(
            "PUT",
            &path,
            Some(input.clone()),
            &[("authorization", &operator), ("if-match", old_tag)],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    let device_path = format!("/api/v2/devices/{device}");
    let current = f
        .call("GET", &device_path, None, &[("authorization", &operator)])
        .await;
    let assignment = json!({"user_id":id,"user_revision":updated.body["revision"]});
    let denied = f
        .call(
            "PUT",
            &format!("{device_path}/user"),
            Some(assignment.clone()),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(denied.body["code"], "recent_auth_required");
    let grant = proof(&f, &auth, "device.assign", &device).await;
    let assigned = f
        .call(
            "PUT",
            &format!("{device_path}/user"),
            Some(assignment),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
                ("x-motion-action-proof", &grant),
            ],
        )
        .await;
    assert_eq!(assigned.status, StatusCode::OK, "{:?}", assigned.body);
    assert_eq!(assigned.body["user_id"], id);
    let stale_assignment = f
        .call(
            "PUT",
            &format!("{device_path}/user"),
            Some(json!({"user_id":id,"user_revision":updated.body["revision"]})),
            &[
                ("authorization", &operator),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(
        stale_assignment.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale_assignment.body
    );
    renamed["disabled"] = Value::Bool(true);
    let disabled = f
        .call(
            "PUT",
            &path,
            Some(renamed),
            &[
                ("authorization", &operator),
                ("if-match", updated.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK, "{:?}", disabled.body);
    assert_eq!(
        f.call("GET", "/api/v2/me", None, &[("authorization", &low)])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let current_admin = f
        .call(
            "GET",
            &format!("/api/v2/devices/{admin_id}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(
        current_admin.status,
        StatusCode::OK,
        "{:?}",
        current_admin.body
    );
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{admin_id}"),
            None,
            &[
                ("authorization", &operator),
                ("if-match", current_admin.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{:?}", revoked.body);
    let revoked_replay = f
        .call(
            "POST",
            "/api/v2/users",
            Some(input),
            &[("authorization", &auth), ("idempotency-key", key)],
        )
        .await;
    assert_eq!(revoked_replay.status, StatusCode::UNAUTHORIZED);
    f.close().await;
    f.open().await;
    let retained = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(retained.status, StatusCode::OK, "{:?}", retained.body);
    assert_eq!(retained.body["disabled"], true);
    f.close().await;
}
