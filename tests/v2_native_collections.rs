//! Public manual collections keep Motion state, never Motion catalog authority.
use axum::http::StatusCode;
use playscale as playscale_app;
use serde_json::{Value, json};
#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};
fn input(name: &str, items: Vec<Value>) -> Value {
    json!({"name":name,"profile_id":"default","kind":"manual","item_ids":items,"filter_id":null})
}

#[tokio::test]
async fn native_manual_collections_preserve_hidden_members_reauthorize_replay_and_restart() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let operator = format!("Bearer {OPERATOR}");
    let mut works = Vec::new();
    for (title, key) in [
        ("Zulu", "collections-work-zulu-0001"),
        ("Alpha", "collections-work-alpha-0001"),
    ] {
        let r = f
            .call(
                "POST",
                "/api/v2/catalog/items",
                Some(json!({"kind":"movie","title":title,"library_ids":[]})),
                &[("authorization", &operator), ("idempotency-key", key)],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        works.push(r.body["id"].clone());
    }
    // Keep a genuine usable administrator while revoking the tested grant.
    let _reserve_admin = f.pair(&["default"], &["system:admin"]).await;
    let (device, token) = f
        .pair(
            &["default"],
            &["catalog:read", "collections:write", "system:admin"],
        )
        .await;
    let auth = format!("Bearer {token}");
    let mut wrong_profile = input("Wrong profile", vec![]);
    wrong_profile["profile_id"] = json!("not-granted");
    let denied = f
        .call(
            "POST",
            "/api/v2/collections",
            Some(wrong_profile),
            &[
                ("authorization", &auth),
                ("idempotency-key", "collections-wrong-profile-0001"),
            ],
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND, "{}", denied.body);
    let request = input("Saved", works.clone());
    let created = f
        .call(
            "POST",
            "/api/v2/collections",
            Some(request.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "collections-create-0001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["item_ids"], json!([works[1], works[0]]));
    let id = created.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/collections/{id}");
    let tag = created.headers["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        f.call("GET", &path, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let bad = f
        .call(
            "PUT",
            &path,
            Some(input("Bad", vec![json!("unknown-work")])),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.body);
    let policy=f.call("PUT",&format!("/api/v2/devices/{device}/policy"),Some(json!({"library_ids":[],"allow_unrated":false,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read","collections:write"]})),&[("authorization",&operator),("if-match","\"r-1\"")]).await;
    assert_eq!(policy.status, StatusCode::OK, "{}", policy.body);
    let replay = f
        .call(
            "POST",
            "/api/v2/collections",
            Some(request),
            &[
                ("authorization", &auth),
                ("idempotency-key", "collections-create-0001"),
            ],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.body);
    assert_eq!(replay.body["id"], id);
    assert_eq!(replay.body["item_ids"], json!([]));
    let hidden = f
        .call("GET", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.body);
    assert_eq!(hidden.body["item_ids"], json!([]));
    let inject = f
        .call(
            "PUT",
            &path,
            Some(input("Inject", vec![works[0].clone()])),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(
        inject.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        inject.body
    );
    let renamed = f
        .call(
            "PUT",
            &path,
            Some(input("Renamed", vec![])),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    assert_eq!(renamed.body["item_ids"], json!([]));
    let current_tag = renamed.headers["etag"].to_str().unwrap().to_owned();
    assert_ne!(current_tag, tag);
    let stale = f
        .call(
            "PUT",
            &path,
            Some(input("Stale", vec![])),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    let full = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(full.body["item_ids"], json!([works[1], works[0]]));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM collection_items WHERE collection_id=?")
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap(),
        2
    );
    let listed = f
        .call(
            "GET",
            "/api/v2/collections",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"][0]["item_ids"], json!([]));
    for (method, path) in [
        ("HEAD", "/api/v2/catalog/filters"),
        ("HEAD", "/api/v2/playlists"),
        ("GET", "/api/v2/playback/queues"),
        ("HEAD", "/api/v2/collections"),
    ] {
        assert_eq!(
            f.call(method, path, None, &[("authorization", &operator)])
                .await
                .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    let smart=f.call("POST","/api/v2/collections",Some(json!({"name":"Smart","profile_id":"default","kind":"smart","item_ids":[],"filter_id":"filter"})),&[("authorization",&operator),("idempotency-key","collections-smart-0001")]).await;
    assert_eq!(
        smart.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        smart.body
    );
    f.close().await;
    f.open().await;
    let after = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(after.body, full.body);
    let device_read = f
        .call(
            "GET",
            &format!("/api/v2/devices/{device}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    let device_tag = device_read.headers["etag"].to_str().unwrap();
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{device}"),
            None,
            &[("authorization", &operator), ("if-match", device_tag)],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let removed = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", &current_tag)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &operator)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    f.close().await;
}

#[tokio::test]
async fn native_saved_filter_ast_crud_is_profile_owned_not_catalog_authority() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let operator = format!("Bearer {OPERATOR}");
    let (_, token) = f.pair(&["default"], &["collections:write"]).await;
    let auth = format!("Bearer {token}");
    let (_, reader) = f.pair(&["default"], &["catalog:read"]).await;
    let reader = format!("Bearer {reader}");
    let original = json!({"name":"Movies","profile_id":"default","all":[{"field":"kind","operator":"eq","value":"movie"},{"field":"year","operator":"gte","value":2000},{"field":"person_id","operator":"eq","value":"opaque-person-not-a-grant"}]});
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/catalog/filters",
            Some(original.clone()),
            &[("idempotency-key", "saved-filter-create-0001")]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/catalog/filters",
            Some(original.clone()),
            &[
                ("authorization", &reader),
                ("idempotency-key", "saved-filter-create-0001")
            ]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let mut hidden = original.clone();
    hidden["profile_id"] = json!("not-granted");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/catalog/filters",
            Some(hidden),
            &[
                ("authorization", &auth),
                ("idempotency-key", "saved-filter-hidden-0001")
            ]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/filters",
            Some(original.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "saved-filter-create-0001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["all"], original["all"]);
    let path = format!(
        "/api/v2/catalog/filters/{}",
        created.body["id"].as_str().unwrap()
    );
    let tag = created.headers["etag"].to_str().unwrap().to_owned();
    let replay = f
        .call(
            "POST",
            "/api/v2/catalog/filters",
            Some(original.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "saved-filter-create-0001"),
            ],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.body);
    assert_eq!(replay.body, created.body);
    let mut changed = original.clone();
    changed["name"] = json!("Changed");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/catalog/filters",
            Some(changed.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "saved-filter-create-0001")
            ]
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    let mut invalid = original.clone();
    invalid["all"][0]["operator"] = json!("contains");
    assert_eq!(
        f.call(
            "PUT",
            &path,
            Some(invalid),
            &[("authorization", &auth), ("if-match", &tag)]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let updated = f
        .call(
            "PUT",
            &path,
            Some(changed.clone()),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    let next = updated.headers["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        f.call(
            "PUT",
            &path,
            Some(original),
            &[("authorization", &auth), ("if-match", &tag)]
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let profile = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Private filter profile"})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "saved-filter-profile-0001"),
            ],
        )
        .await;
    assert_eq!(profile.status, StatusCode::CREATED, "{}", profile.body);
    let mut private = changed.clone();
    private["profile_id"] = profile.body["id"].clone();
    let private = f
        .call(
            "POST",
            "/api/v2/catalog/filters",
            Some(private),
            &[
                ("authorization", &operator),
                ("idempotency-key", "saved-filter-private-0001"),
            ],
        )
        .await;
    assert_eq!(private.status, StatusCode::CREATED, "{}", private.body);
    let private_path = format!(
        "/api/v2/catalog/filters/{}",
        private.body["id"].as_str().unwrap()
    );
    assert_eq!(
        f.call("GET", &private_path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call(
            "DELETE",
            &private_path,
            None,
            &[("authorization", &auth), ("if-match", "\"r-1\"")]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let listed = f
        .call(
            "GET",
            "/api/v2/catalog/filters",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"], json!([updated.body]));
    // A stored predicate is not a query grant or an admitted smart evaluator.
    assert_eq!(
        f.call(
            "GET",
            &format!("{path}/evaluate"),
            None,
            &[("authorization", &operator)]
        )
        .await
        .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let smart = json!({"name":"Smart","profile_id":"default","kind":"smart","item_ids":[],"filter_id":created.body["id"]});
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/collections",
            Some(smart),
            &[
                ("authorization", &auth),
                ("idempotency-key", "saved-filter-smart-0001")
            ]
        )
        .await
        .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    f.close().await;
    f.open().await;
    let loaded = f
        .call("GET", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(loaded.status, StatusCode::OK, "{}", loaded.body);
    assert_eq!(loaded.body, updated.body);
    assert_eq!(
        f.call(
            "DELETE",
            &path,
            None,
            &[("authorization", &auth), ("if-match", &tag)]
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        f.call(
            "DELETE",
            &path,
            None,
            &[("authorization", &auth), ("if-match", &next)]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    f.close().await;
}
