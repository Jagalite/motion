//! Ordered native timeline references remain usable as descriptive organization
//! even when the physical source is withdrawn. This is not playback authority.
use axum::http::StatusCode;
use playscale as playscale_app;
use serde_json::{Value, json};
#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
#[path = "support/native_retirement.rs"]
mod native_retirement;
use native_application_fixture::{Fixture, OPERATOR};
fn input(name: &str, entries: Vec<Value>) -> Value {
    json!({"name":name,"profile_id":"default","entries":entries})
}
#[tokio::test]
async fn native_playlist_typed_unavailable_identity_hidden_order_cas_and_restart() {
    let mut f = Fixture::new().await;
    let source = f
        .seed_preserved_timeline(
            "original.txt",
            b"exact preserved native playlist source observation",
        )
        .await;
    f.cutover_source(Some(&source)).await;
    let operator = format!("Bearer {OPERATOR}");
    // Keep a genuine usable administrator while revoking the tested grant.
    let _reserve_admin = f.pair(&["default"], &["system:admin"]).await;
    let (device, token) = f
        .pair(
            &["default"],
            &["catalog:read", "collections:write", "system:admin"],
        )
        .await;
    let auth = format!("Bearer {token}");
    let entries = vec![
        json!({"entry_id":"first","timeline_id":"timeline"}),
        json!({"entry_id":"second","timeline_id":"timeline"}),
    ];
    let body = input("Queue later", entries.clone());
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/playlists",
            Some(body.clone()),
            &[("idempotency-key", "native-playlist-create-0001")]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    let mut wrong = body.clone();
    wrong["profile_id"] = json!("not-granted");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/playlists",
            Some(wrong),
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-playlist-profile-0001")
            ]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let created = f
        .call(
            "POST",
            "/api/v2/playlists",
            Some(body.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-playlist-create-0001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["entries"], json!(entries));
    let id = created.body["id"].as_str().unwrap();
    let path = format!("/api/v2/playlists/{id}");
    let tag = created.headers["etag"].to_str().unwrap().to_owned();
    let invalid = input(
        "Foreign",
        vec![json!({"entry_id":"foreign","timeline_id":"unknown-timeline"})],
    );
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
    let library_path = format!("/api/v2/libraries/{source}");
    let library = f
        .call("GET", &library_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(library.status, StatusCode::OK, "{}", library.body);
    let library_tag = library.headers["etag"].to_str().unwrap();
    let detached = f.call("PUT", &library_path, Some(json!({"name":library.body["name"],"kind":library.body["kind"],"language":library.body["language"],"source_ids":[]})), &[("authorization", &operator), ("if-match", library_tag), ("idempotency-key", "native-playlist-detach-0001")]).await;
    assert_eq!(detached.status, StatusCode::OK, "{}", detached.body);
    let source_path = format!("/api/v2/sources/{source}");
    let source_row = f
        .call("GET", &source_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(source_row.status, StatusCode::OK, "{}", source_row.body);
    let source_tag = source_row.headers["etag"].to_str().unwrap();
    let withdrawn = f
        .call(
            "DELETE",
            &source_path,
            None,
            &[
                ("authorization", &operator),
                ("if-match", source_tag),
                ("idempotency-key", "native-playlist-withdraw-0001"),
            ],
        )
        .await;
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
    let versions = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/timeline/versions",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(versions.status, StatusCode::OK, "{}", versions.body);
    assert_eq!(versions.body["items"][0]["availability"], "unavailable");
    // This is still a real visible timeline, so organization does not require a
    // current file or borrow playback readiness to admit its stable reference.
    let reordered = input(
        "Unavailable but saved",
        vec![entries[1].clone(), entries[0].clone()],
    );
    let reordered = f
        .call(
            "PUT",
            &path,
            Some(reordered),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(reordered.status, StatusCode::OK, "{}", reordered.body);
    let next = reordered.headers["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        f.call(
            "PUT",
            &path,
            Some(body.clone()),
            &[("authorization", &auth), ("if-match", &tag)]
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let policy=f.call("PUT",&format!("/api/v2/devices/{device}/policy"),Some(json!({"library_ids":[],"allow_unrated":false,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read","collections:write"]})),&[("authorization",&operator),("if-match","\"r-1\"")]).await;
    assert_eq!(policy.status, StatusCode::OK, "{}", policy.body);
    let replay = f
        .call(
            "POST",
            "/api/v2/playlists",
            Some(body.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "native-playlist-create-0001"),
            ],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.body);
    assert_eq!(replay.body["entries"], json!([]));
    let hidden = f
        .call("GET", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.body);
    assert_eq!(hidden.body["entries"], json!([]));
    assert_eq!(
        f.call(
            "PUT",
            &path,
            Some(body),
            &[("authorization", &auth), ("if-match", &next)]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let renamed = f
        .call(
            "PUT",
            &path,
            Some(input("Private entries preserved", vec![])),
            &[("authorization", &auth), ("if-match", &next)],
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    let full = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(full.body["entries"], json!([entries[1], entries[0]]));
    let listed = f
        .call(
            "GET",
            "/api/v2/playlists",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"][0]["entries"], json!([]));
    assert_eq!(
        f.call(
            "GET",
            "/api/v2/playback/queues",
            None,
            &[("authorization", &operator)]
        )
        .await
        .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        f.call(
            "POST",
            &format!("{path}/play"),
            None,
            &[("authorization", &operator)]
        )
        .await
        .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    f.close().await;
    f.open().await;
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &operator)])
            .await
            .body,
        full.body
    );
    // A genuine native structural owner deletion is not interpreted as a
    // still-current timeline grant through the retained compatibility rows.
    f.close().await;
    native_retirement::detach_version(
        f.app.db.clone(),
        f._dir.path().join("native.sqlite3"),
        "work".into(),
        "version".into(),
    )
    .await;
    f.open().await;
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &operator)])
            .await
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let final_tag = renamed.headers["etag"].to_str().unwrap();
    assert_eq!(
        f.call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", final_tag)]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    f.close().await;
}
