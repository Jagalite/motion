//! Real current typed history disclosure, including unavailable original media.
use super::native_application_fixture::{Fixture, OPERATOR};
use axum::http::StatusCode;
use serde_json::json;

pub async fn exercise() {
    let mut f = Fixture::new().await;
    let source = f
        .seed_preserved_timeline("history.txt", b"actual retained history source")
        .await;
    let permissions = ["catalog:read", "catalog:write"];
    let (device, token) = f.pair(&["default"], &permissions).await;
    let auth = format!("Bearer {token}");
    let (_, read_only) = f.pair(&["default"], &["catalog:read"]).await;
    let read_only = format!("Bearer {read_only}");
    let operator = format!("Bearer {OPERATOR}");
    f.cutover_source(Some(&source)).await;
    let path = "/api/v2/profiles/default/continue-watching?limit=1";
    let denied = f.call("GET", path, None, &[]).await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    for (method, expected) in [
        ("HEAD", StatusCode::SERVICE_UNAVAILABLE),
        ("POST", StatusCode::METHOD_NOT_ALLOWED),
    ] {
        let closed = f
            .call(method, path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(closed.status, expected);
    }
    let mut libraries = Vec::new();
    for (name, sources, key) in [
        (
            "History source",
            vec![source.clone()],
            "continue-source-library",
        ),
        ("Other history", Vec::new(), "continue-other-library"),
    ] {
        let created = f
            .call(
                "POST",
                "/api/v2/libraries",
                Some(json!({"name":name,"kind":"mixed","language":"en","source_ids":sources})),
                &[("authorization", &operator), ("idempotency-key", key)],
            )
            .await;
        assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
        libraries.push(created.body["id"].as_str().unwrap().to_owned());
    }
    let work = f
        .call(
            "GET",
            "/api/v2/catalog/items/work",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(work.status, StatusCode::OK, "{:?}", work.body);
    let containing: std::collections::BTreeSet<String> =
        serde_json::from_value(work.body["library_ids"].clone()).unwrap();
    assert_eq!(containing, [source.clone(), libraries[0].clone()].into());
    let timeline = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/timeline",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(timeline.status, StatusCode::OK, "{:?}", timeline.body);
    let marker = json!({"timeline_revision":timeline.body["revision"],"kind":"intro","start_ms":0,"end_ms":100,"label":"Manual intro"});
    let marker_path = "/api/v2/catalog/timelines/timeline/markers";
    let anonymous = f
        .call(
            "POST",
            marker_path,
            Some(marker.clone()),
            &[("idempotency-key", "marker-anonymous-request")],
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let forbidden = f
        .call(
            "POST",
            marker_path,
            Some(marker.clone()),
            &[
                ("authorization", &read_only),
                ("idempotency-key", "marker-read-only-request"),
            ],
        )
        .await;
    assert_eq!(
        forbidden.status,
        StatusCode::FORBIDDEN,
        "{:?}",
        forbidden.body
    );
    let head = f
        .call("HEAD", marker_path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(head.status, StatusCode::SERVICE_UNAVAILABLE);
    for (index, granted_libraries) in [
        vec![source.clone(), libraries[0].clone()],
        vec![libraries[1].clone()],
        vec![source.clone(), libraries[0].clone()],
    ]
    .into_iter()
    .enumerate()
    {
        let current = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &operator)],
            )
            .await;
        assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
        let changed=f.call("PUT",&format!("/api/v2/devices/{device}/policy"),Some(json!({"library_ids":granted_libraries,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),&[("authorization",&operator),("if-match",current.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
        let history = f.call("GET", path, None, &[("authorization", &auth)]).await;
        assert_eq!(history.status, StatusCode::OK, "{:?}", history.body);
        let rows = history.body["items"].as_array().unwrap();
        assert_eq!(rows.len(), if index == 1 { 0 } else { 1 });
        let created = f
            .call(
                "POST",
                marker_path,
                Some(marker.clone()),
                &[
                    ("authorization", &auth),
                    ("idempotency-key", "marker-same-current-request"),
                ],
            )
            .await;
        assert_eq!(
            created.status,
            if index == 1 {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CREATED
            },
            "{:?}",
            created.body
        );
        let listed = f
            .call("GET", marker_path, None, &[("authorization", &auth)])
            .await;
        assert_eq!(
            listed.status,
            if index == 1 {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::OK
            },
            "{:?}",
            listed.body
        );
        if index != 1 {
            assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
            assert_eq!(listed.body["items"][0]["id"], created.body["id"]);
            assert_eq!(created.body["provenance"], "manual");
            assert_eq!(
                created.body["timeline_revision"],
                marker["timeline_revision"]
            );
            assert_eq!(rows[0]["timeline"]["id"], "timeline");
            assert_eq!(rows[0]["item"]["id"], "work");
            assert_eq!(rows[0]["viewing"]["position_ms"], 250);
            assert_eq!(rows[0]["viewing"]["manual_epoch"], "2");
        }
    }
    // A visible shared-source work still needs every containing library for
    // curation. A fresh second real definition must not be ignored on replay.
    let shared = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Second marker owner","kind":"mixed","language":"en","source_ids":[source.clone()]})), &[("authorization", &operator),("idempotency-key","marker-second-real-library")]).await;
    assert_eq!(shared.status, StatusCode::CREATED, "{:?}", shared.body);
    let denied = f
        .call(
            "POST",
            marker_path,
            Some(marker.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "marker-same-current-request"),
            ],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{:?}", denied.body);
    assert_eq!(denied.body["code"], "outside_scope");
    let mut stale = marker.clone();
    stale["timeline_revision"] = json!("0");
    let stale = f
        .call(
            "POST",
            marker_path,
            Some(stale),
            &[
                ("authorization", &operator),
                ("idempotency-key", "marker-stale-timeline-request"),
            ],
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.body);
    assert_eq!(stale.body["code"], "timeline_changed");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM markers WHERE timeline_id='timeline'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        count, 1,
        "denied or replayed curation must not insert markers"
    );
    let before:(i64,i64,i64)=sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE timeline_id='timeline'").fetch_one(&f.app.db).await.unwrap();
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
    std::fs::remove_file(f._dir.path().join("media/history.txt")).unwrap();
    super::playscale_app::native_discovery::scan(
        &f.app,
        &source,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    for restart in [false, true] {
        if restart {
            f.close().await;
            f.open().await;
        }
        let history = f.call("GET", path, None, &[("authorization", &auth)]).await;
        assert_eq!(history.status, StatusCode::OK, "{:?}", history.body);
        let rows = history.body["items"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "offline history must not require playable original bytes"
        );
        assert_eq!(rows[0]["timeline"]["id"], "timeline");
        assert_eq!(rows[0]["item"]["availability"], "unavailable");
        assert_eq!(rows[0]["viewing"]["position_ms"], 250);
        let offline_marker = f
            .call(
                "POST",
                marker_path,
                Some(marker.clone()),
                &[
                    ("authorization", &operator),
                    ("idempotency-key", "marker-original-offline-request"),
                ],
            )
            .await;
        assert_eq!(
            offline_marker.status,
            StatusCode::CREATED,
            "{:?}",
            offline_marker.body
        );
        assert_eq!(
            offline_marker.body["timeline_revision"],
            marker["timeline_revision"]
        );
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM markers WHERE timeline_id='timeline'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        count, 2,
        "offline create and restart replay preserve identity"
    );
    let after:(i64,i64,i64)=sqlx::query_as("SELECT revision,manual_epoch,position_ms FROM timeline_viewing WHERE timeline_id='timeline'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(after, before);
    f.close().await;
}
