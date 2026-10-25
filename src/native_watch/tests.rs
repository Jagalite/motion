use super::*;
use crate as playscale_app;
use axum::http::{HeaderMap, Method, StatusCode};
#[path = "../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

#[test]
fn reserved_fields_cannot_downgrade_or_mix_acceptance_contracts() {
    assert!(accepted(&json!({"contract_version":1})).unwrap().is_none());
    for key in [
        "watch_contract_version",
        "watch_occurrence",
        "watch_source_lifecycle",
    ] {
        let mut scope = json!({});
        scope[key] = json!(null);
        assert!(accepted(&scope).is_err());
    }
    assert!(accepted(&json!({"watch_contract_version":1,"watch_occurrence":{},"watch_source_lifecycle":{},"schedule_occurrence":{}})).is_err());
}

#[tokio::test]
async fn real_watch_hints_link_distinct_generations_replay_and_restart() {
    let mut f = Fixture::new().await;
    let (device, token) = f
        .pair(
            &["default"],
            &["system:admin", "sources:manage", "catalog:read"],
        )
        .await;
    let _other_admin = f.pair(&["default"], &["system:admin"]).await;
    let root = f._dir.path().join("watched-source");
    std::fs::create_dir(&root).unwrap();
    let auth = format!("Bearer {OPERATOR}");
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Watched","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "watch-real-source-create"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let source_id = source.body["id"].as_str().unwrap().to_owned();
    let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Watched library","kind":"movies","language":"en","source_ids":[source_id]})), &[("authorization", &auth),("idempotency-key","watch-real-library-create")]).await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let library_id = library.body["id"].as_str().unwrap().to_owned();
    f.cutover_source(Some(&source_id)).await;
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let caller = crate::v2::auth::resolve(&f.app, &Method::POST, &headers)
        .await
        .unwrap();
    let mut admin_headers = HeaderMap::new();
    admin_headers.insert("authorization", auth.parse().unwrap());
    let administrator = crate::v2::auth::resolve(&f.app, &Method::GET, &admin_headers)
        .await
        .unwrap();
    let before = inventory(&f.app, &administrator).await.unwrap();
    assert!(!before.historical_inventory_pending);
    assert!(before.historical_unregistered_source_ids.is_empty());
    assert!(before.unresolved_configuration_ids.is_empty());
    let original = before
        .sources
        .iter()
        .find(|row| row.source_id == source_id)
        .unwrap();
    assert!(original.awaiting_authorization);
    assert_eq!(original.logical_library_ids, vec![library_id.clone()]);
    assert!(original.configurations.is_empty());
    let definition = configure(&f.app, &caller, "real-watch", None, &library_id, &source_id)
        .await
        .unwrap();
    assert_eq!(definition.revision, 1);
    let configured = inventory(&f.app, &administrator).await.unwrap();
    let row = configured
        .sources
        .iter()
        .find(|row| row.source_id == source_id)
        .unwrap();
    assert!(!row.awaiting_authorization);
    assert_eq!(row.configurations[0].id, "real-watch");
    assert_eq!(row.configurations[0].revision, 1);
    assert_eq!(
        row.configurations[0].state,
        ConfigurationState::CurrentActorAndScope
    );

    let mut monitor = monitor(&f.app, "real-watch").await.unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    for (permissions, refused) in [
        (json!(["catalog:read"]), true),
        (
            json!(["system:admin", "sources:manage", "catalog:read"]),
            false,
        ),
    ] {
        let current = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(current.status, StatusCode::OK, "{}", current.body);
        let changed = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({"library_ids":[],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})), &[("authorization", &auth),("if-match", current.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
        if refused {
            let retained = inventory(&f.app, &administrator).await.unwrap();
            let row = retained
                .sources
                .iter()
                .find(|row| row.source_id == source_id)
                .unwrap();
            assert_eq!(
                row.configurations[0].state,
                ConfigurationState::NeedsReauthorization
            );
            assert!(!row.awaiting_authorization);
            assert!(admit(&f.app, "real-watch", None).await.is_err());
            let links: i64 =
                sqlx::query_scalar("SELECT count(*) FROM catabolic_source_watch_admissions")
                    .fetch_one(&f.app.db)
                    .await
                    .unwrap();
            assert_eq!(links, 0);
        }
    }

    std::fs::write(root.join("first-hint.txt"), b"first").unwrap();
    let batch = monitor
        .collect(std::time::Duration::from_millis(500), &cancel)
        .unwrap();
    let (first, ack) = admit(&f.app, "real-watch", Some(batch)).await.unwrap();
    let Admission::Linked(first_id) = first else {
        panic!("first hint did not link")
    };
    // The retained native sequence is acknowledged only after the durable link exists.
    let persisted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_source_watch_admissions WHERE request_id=?",
    )
    .bind(&first_id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(persisted, 1);
    let raw: String =
        sqlx::query_scalar("SELECT scope_json FROM catabolic_scan_requests WHERE id=?")
            .bind(&first_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let proof: Value = serde_json::from_str(&raw).unwrap();
    let mut tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    verify_execution_in(&mut tx, &proof, &first_id, &source_id)
        .await
        .unwrap();
    assert!(
        verify_execution_in(&mut tx, &proof, "missing-request", &source_id)
            .await
            .is_err()
    );
    assert!(
        verify_execution_in(&mut tx, &proof, &first_id, "different-source")
            .await
            .is_err()
    );
    let mut altered = proof.clone();
    altered["watch_occurrence"]["native_dirty_generation"] = json!(
        proof["watch_occurrence"]["native_dirty_generation"]
            .as_i64()
            .unwrap()
            + 1
    );
    assert!(
        verify_execution_in(&mut tx, &altered, &first_id, &source_id)
            .await
            .is_err()
    );
    altered = proof.clone();
    altered
        .as_object_mut()
        .unwrap()
        .remove("watch_source_lifecycle");
    assert!(
        verify_execution_in(&mut tx, &altered, &first_id, &source_id)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();

    monitor.acknowledge(ack.unwrap()).unwrap();
    assert_eq!(
        admit(&f.app, "real-watch", None).await.unwrap().0,
        Admission::Linked(first_id.clone())
    );
    let binary = native_application_fixture::observer_binary();
    let artifact =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    let runtime = f.app.native_catalog.as_ref().unwrap();
    runtime.register_observer(artifact).await.unwrap();
    let prepared = prepare(&f.app, "real-watch").await.unwrap();
    let native_source = prepared.mapping.native_source.clone();
    let (handle, prepared) = runtime
        .with_host(move |host| Ok((host.start_scan(&native_source, ())?, prepared)))
        .await
        .unwrap();
    prepared.tx.commit().await.unwrap();
    std::fs::write(root.join("during-scan.txt"), b"during actual detached scan").unwrap();
    let pending = monitor
        .collect(std::time::Duration::from_millis(500), &cancel)
        .unwrap();
    let refused = admit(&f.app, "real-watch", Some(pending)).await;
    // Always join the real child before asserting the detached admission result.
    let joining = handle.clone();
    let mut terminal = runtime
        .with_host(move |host| Ok(host.cancel_scan(&joining)?))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    while terminal.is_none() {
        let joining = handle.clone();
        terminal = runtime
            .with_host(move |host| {
                Ok(host.poll_scan(&joining, std::time::Duration::from_millis(50))?)
            })
            .await
            .unwrap();
        assert!(
            tokio::time::Instant::now() < deadline,
            "actual scan cleanup did not finish"
        );
    }
    assert!(
        refused.is_err(),
        "detached native ownership cannot invalidate or acknowledge events"
    );
    std::fs::write(root.join("later-hint.txt"), b"later").unwrap();
    let later = monitor
        .collect(std::time::Duration::from_millis(500), &cancel)
        .unwrap();
    let (second, ack) = admit(&f.app, "real-watch", Some(later)).await.unwrap();
    let Admission::Linked(second_id) = second else {
        panic!("later hint did not link")
    };
    assert_ne!(
        first_id, second_id,
        "an outstanding earlier demand must not consume a later generation"
    );
    monitor.acknowledge(ack.unwrap()).unwrap();
    let generations: Vec<i64> = sqlx::query_scalar("SELECT native_dirty_generation FROM catabolic_source_watch_admissions WHERE watch_id='real-watch' ORDER BY native_dirty_generation").fetch_all(&f.app.db).await.unwrap();
    assert_eq!(generations.len(), 2);
    assert!(generations[1] > generations[0]);
    drop(monitor);
    f.close().await;
    f.open().await;
    assert_eq!(
        admit(&f.app, "real-watch", None).await.unwrap().0,
        Admission::Linked(second_id.clone())
    );
    for (index, request) in [&first_id, &second_id].into_iter().enumerate() {
        let cancelled = f
            .call(
                "POST",
                &format!("/api/v2/scans/{request}/cancel"),
                None,
                &[
                    ("authorization", &auth),
                    ("idempotency-key", &format!("watch-cancel-request-{index}")),
                ],
            )
            .await;
        assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    }
    let current = f
        .call(
            "GET",
            &format!("/api/v2/libraries/{library_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let detached = f
        .call(
            "PUT",
            &format!("/api/v2/libraries/{library_id}"),
            Some(json!({"name":"Watched library","kind":"movies","language":"en","source_ids":[]})),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(detached.status, StatusCode::OK, "{}", detached.body);
    let current = f
        .call(
            "GET",
            &format!("/api/v2/sources/{source_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let withdrawn = f
        .call(
            "DELETE",
            &format!("/api/v2/sources/{source_id}"),
            None,
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
                ("idempotency-key", "watch-withdraw-real-source"),
            ],
        )
        .await;
    // Retained cancelled demands keep this source as unavailable history.
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
    let retained = inventory(&f.app, &administrator).await.unwrap();
    let row = retained
        .sources
        .iter()
        .find(|row| row.source_id == source_id)
        .unwrap();
    assert_eq!(
        row.configurations[0].state,
        ConfigurationState::BlockedLifecycle
    );
    assert_eq!(
        row.lifecycle.state,
        playscale_core::source_lifecycle::State::Withdrawn
    );
    assert!(row.logical_library_ids.is_empty());

    assert!(admit(&f.app, "real-watch", None).await.is_err());
    let mut tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        verify_execution_in(&mut tx, &proof, &first_id, &source_id)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let caller = crate::v2::auth::resolve(&f.app, &Method::POST, &headers)
        .await
        .unwrap();
    disable(&f.app, &caller, "real-watch", 1, true)
        .await
        .unwrap();
    assert!(admit(&f.app, "real-watch", None).await.is_err());
    assert!(
        configure(
            &f.app,
            &caller,
            "real-watch",
            Some(2),
            &library_id,
            &source_id
        )
        .await
        .is_err()
    );
    f.close().await;
}

#[tokio::test]
async fn real_inventory_preserves_orphans_and_all_logical_memberships() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let root = f._dir.path().join("inventory-source");
    std::fs::create_dir(&root).unwrap();
    let auth = format!("Bearer {OPERATOR}");
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Inventory source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "inventory-real-source"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let source_id = source.body["id"].as_str().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("authorization", auth.parse().unwrap());
    let caller = crate::v2::auth::resolve(&f.app, &Method::GET, &headers)
        .await
        .unwrap();
    let orphan = inventory(&f.app, &caller).await.unwrap();
    let row = orphan
        .sources
        .iter()
        .find(|row| row.source_id == source_id)
        .unwrap();
    assert!(row.logical_library_ids.is_empty());
    assert!(row.awaiting_authorization);
    assert!(!orphan.historical_inventory_pending);
    assert!(orphan.historical_unregistered_source_ids.is_empty());
    let mut libraries = Vec::new();
    for index in 0..2 {
        let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":format!("Inventory library {index}"),"kind":"movies","language":"en","source_ids":[source_id]})), &[("authorization", &auth),("idempotency-key", &format!("inventory-real-library-{index}"))]).await;
        assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
        libraries.push(library.body["id"].as_str().unwrap().to_owned());
    }
    libraries.sort();
    let shared = inventory(&f.app, &caller).await.unwrap();
    let row = shared
        .sources
        .iter()
        .find(|row| row.source_id == source_id)
        .unwrap();
    assert_eq!(row.logical_library_ids, libraries);
    assert!(row.configurations.is_empty());
    f.close().await;
    assert!(
        inventory(&f.app, &caller).await.is_err(),
        "bound without runtime cannot enumerate through legacy fallback"
    );
}
