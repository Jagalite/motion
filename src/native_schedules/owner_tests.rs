//! Actual legacy creation, qualified native adoption and guarded scheduled
//! admission through the canonical companion schema and full public router.
use super::*;
use crate as playscale_app;
use axum::http::StatusCode;
use serde_json::{Value, json};
#[path = "../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

#[tokio::test]
async fn retained_schedule_admission_binds_occurrence_sources_and_current_lifecycle() {
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("scheduled-source");
    std::fs::create_dir(&root).unwrap();
    let auth = format!("Bearer {OPERATOR}");
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Scheduled source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "schedule-fixture-source"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let source_id = source.body["id"].as_str().unwrap().to_owned();
    let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Scheduled library","kind":"movies","language":"en","source_ids":[source_id]})), &[("authorization",&auth),("idempotency-key","schedule-fixture-library")]).await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let library_id = library.body["id"].as_str().unwrap().to_owned();
    let schedule = f
        .call(
            "POST",
            "/api/v2/schedules",
            Some(
                json!({"library_id":library_id,"task":"scan","interval_seconds":60,"enabled":true}),
            ),
            &[
                ("authorization", &auth),
                ("idempotency-key", "schedule-fixture-definition"),
            ],
        )
        .await;
    assert_eq!(schedule.status, StatusCode::CREATED, "{}", schedule.body);
    let schedule_id = schedule.body["id"].as_str().unwrap().to_owned();
    let retained_other = f.call("POST", "/api/v2/schedules", Some(json!({
        "library_id":library_id,"task":"metadata_refresh","interval_seconds":60,"enabled":false
    })), &[("authorization", &auth),("idempotency-key","retained-nonscan-schedule-key")]).await;
    assert_eq!(
        retained_other.status,
        StatusCode::CREATED,
        "{}",
        retained_other.body
    );
    let other_id = retained_other.body["id"].as_str().unwrap().to_owned();
    let original: String = sqlx::query_scalar(LEGACY_ROW)
        .bind(&schedule_id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    f.cutover_source(Some(&source_id)).await;
    assert_eq!(
        adopt_retained(&f.app, &schedule_id).await.unwrap(),
        Adoption::Adopted
    );
    assert_eq!(
        adopt_retained(&f.app, &other_id).await.unwrap(),
        Adoption::PendingUnsupportedTask
    );
    let retained_read = schedule_call(
        &f,
        "GET",
        &format!("/schedules/{other_id}"),
        None,
        &[("authorization", &auth)],
    )
    .await;
    assert_eq!(
        retained_read.status,
        StatusCode::OK,
        "{}",
        retained_read.body
    );
    assert_eq!(retained_read.body, retained_other.body);
    let pending_change = schedule_call(
        &f,
        "DELETE",
        &format!("/schedules/{other_id}"),
        None,
        &[
            ("authorization", &auth),
            ("if-match", retained_read.headers["etag"].to_str().unwrap()),
        ],
    )
    .await;
    assert_eq!(pending_change.status, StatusCode::SERVICE_UNAVAILABLE);
    let listed = schedule_call(&f, "GET", "/schedules", None, &[("authorization", &auth)]).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: std::collections::BTreeSet<_> = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(schedule_id.as_str()) && ids.contains(other_id.as_str()));
    let now = serde_json::from_str::<RetainedSchedule>(&original)
        .unwrap()
        .next_run;
    let Fire::Admitted(request_id) = fire_one(&f.app, &schedule_id, now).await.unwrap() else {
        panic!("due schedule was not admitted")
    };
    assert_eq!(
        fire_one(&f.app, &schedule_id, now).await.unwrap(),
        Fire::Idle
    );
    let scope: String =
        sqlx::query_scalar("SELECT scope_json FROM catabolic_scan_requests WHERE id=?")
            .bind(&request_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let scope: Value = serde_json::from_str(&scope).unwrap();
    let mut tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    verify_execution_in(&mut tx, &scope, &request_id, &source_id)
        .await
        .unwrap();
    let mut missing = scope.clone();
    missing["schedule_occurrence"]["due_at"] = json!(now + 1);
    assert!(
        verify_execution_in(&mut tx, &missing, &request_id, &source_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("occurrence missing")
    );
    let mut altered = scope.clone();
    altered["library_id"] = json!("different-library");
    assert!(
        verify_execution_in(&mut tx, &altered, &request_id, &source_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("occurrence differs")
    );
    assert!(
        verify_admission_in(&mut tx, &scope, &[])
            .await
            .unwrap_err()
            .to_string()
            .contains("source set differs")
    );
    let inventory = retained_inventory_in(&f.app, &mut tx).await.unwrap();
    assert_eq!(
        inventory
            .iter()
            .find(|v| v.row.id == schedule_id)
            .unwrap()
            .row
            .next_run,
        now + 60
    );
    let saved: String = sqlx::query_scalar(
        "SELECT original_json FROM catabolic_scan_schedule_adoptions WHERE schedule_id=?",
    )
    .bind(&schedule_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap(),
        serde_json::from_str::<Value>(&original).unwrap()
    );
    tx.rollback().await.unwrap();

    // Revoke through the real queue cancellation, library detach and source
    // withdrawal owners. No synthetic epoch or permission rows are inserted.
    let cancelled = f
        .call(
            "POST",
            &format!("/api/v2/scans/{request_id}/cancel"),
            None,
            &[
                ("authorization", &auth),
                ("idempotency-key", "schedule-fixture-cancel"),
            ],
        )
        .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    let current = f
        .call(
            "GET",
            &format!("/api/v2/libraries/{library_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let tag = current.headers["etag"].to_str().unwrap();
    let detached = f
        .call(
            "PUT",
            &format!("/api/v2/libraries/{library_id}"),
            Some(
                json!({"name":"Scheduled library","kind":"movies","language":"en","source_ids":[]}),
            ),
            &[("authorization", &auth), ("if-match", tag)],
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
    let tag = current.headers["etag"].to_str().unwrap();
    let withdrawn = f
        .call(
            "DELETE",
            &format!("/api/v2/sources/{source_id}"),
            None,
            &[
                ("authorization", &auth),
                ("if-match", tag),
                ("idempotency-key", "schedule-fixture-withdraw"),
            ],
        )
        .await;
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
    let mut tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let current = motion_catalog_backend::source_lifecycle::current_in(&mut tx, &source_id)
        .await
        .unwrap();
    let (_, accepted_lifecycle) = scheduled_proof(&scope).unwrap().unwrap();
    assert_eq!(current.epoch, accepted_lifecycle[0].epoch + 1);
    assert_eq!(
        current.state,
        playscale_core::source_lifecycle::State::Withdrawn
    );
    assert!(
        verify_execution_in(&mut tx, &scope, &request_id, &source_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("source admission withdrawn")
    );
    tx.rollback().await.unwrap();
    f.close().await;
}

/// Exercise the complete production router, including the native gate and auth.
async fn schedule_call(
    f: &Fixture,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> native_application_fixture::Reply {
    f.call(method, &format!("/api/v2{path}"), body, headers)
        .await
}

#[tokio::test]
async fn fresh_schedule_crud_tick_scan_completion_and_restart_preserve_identity() {
    let mut f = Fixture::new().await;
    let (device, token) = f
        .pair(
            &["default"],
            &["system:admin", "sources:manage", "catalog:read"],
        )
        .await;
    // A genuine second administrator permits policy revocation without evading
    // the product's last-administrator protection.
    let _other_admin = f.pair(&["default"], &["system:admin"]).await;
    f.cutover().await;
    let auth = format!("Bearer {token}");
    let root = f._dir.path().join("fresh-scheduled-source");
    std::fs::create_dir(&root).unwrap();
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Fresh schedule source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "fresh-schedule-source-key"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Fresh schedule library","kind":"movies","language":"en","source_ids":[source.body["id"]]})), &[("authorization",&auth),("idempotency-key","fresh-schedule-library-key")]).await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let input =
        json!({"library_id":library.body["id"],"task":"scan","interval_seconds":60,"enabled":true});
    let headers = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "fresh-schedule-create-key"),
    ];
    let created = schedule_call(&f, "POST", "/schedules", Some(input.clone()), &headers).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert!(created.body.get("native_status").is_none());
    let id = created.body["id"].as_str().unwrap().to_owned();
    let path = format!("/schedules/{id}");
    let replay = schedule_call(&f, "POST", "/schedules", Some(input.clone()), &headers).await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.body);
    assert_eq!(replay.body, created.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let listed = f
        .call(
            "GET",
            "/api/v2/schedules",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let unauthorized = schedule_call(&f, "GET", &path, None, &[]).await;
    assert_eq!(unauthorized.status, StatusCode::UNAUTHORIZED);
    let initial_tag = created.headers["etag"].to_str().unwrap().to_owned();
    let unchanged = schedule_call(
        &f,
        "PUT",
        &path,
        Some(input.clone()),
        &[("authorization", &auth), ("if-match", &initial_tag)],
    )
    .await;
    assert_eq!(unchanged.status, StatusCode::OK, "{}", unchanged.body);
    assert_eq!(unchanged.body, created.body);
    let mut changed_input = input.clone();
    changed_input["interval_seconds"] = json!(120);
    let changed = schedule_call(
        &f,
        "PUT",
        &path,
        Some(changed_input.clone()),
        &[("authorization", &auth), ("if-match", &initial_tag)],
    )
    .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(changed.body["revision"], "2");
    let stale = schedule_call(
        &f,
        "PUT",
        &path,
        Some(input),
        &[("authorization", &auth), ("if-match", &initial_tag)],
    )
    .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    let due: i64 = sqlx::query_scalar(
        "SELECT due_at FROM catabolic_scan_schedule_cursors WHERE schedule_id=?",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    // Saved requester JSON cannot mint current authority. Revoke and restore
    // using the genuine policy CAS owner; the exact due occurrence survives.
    for (permissions, refused) in [
        (json!(["catalog:read"]), true),
        (
            json!(["system:admin", "sources:manage", "catalog:read"]),
            false,
        ),
    ] {
        let admin = format!("Bearer {OPERATOR}");
        let current = f
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &admin)],
            )
            .await;
        assert_eq!(current.status, StatusCode::OK, "{}", current.body);
        let policy = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({
            "library_ids":[],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions
        })), &[("authorization", &admin),("if-match", current.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(policy.status, StatusCode::OK, "{}", policy.body);
        if refused {
            let denied = tick_once(&f.app, &stop, due).await.unwrap();
            assert!(denied.admitted.is_empty());
            assert_eq!(denied.refused.len(), 1);
            let requests: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM catabolic_scan_schedule_occurrences WHERE schedule_id=?",
            )
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
            assert_eq!(requests, 0);
            let unchanged: i64 = sqlx::query_scalar(
                "SELECT due_at FROM catabolic_scan_schedule_cursors WHERE schedule_id=?",
            )
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
            assert_eq!(unchanged, due);
            assert_eq!(
                schedule_call(&f, "GET", &path, None, &[("authorization", &auth)])
                    .await
                    .status,
                StatusCode::FORBIDDEN
            );
        }
    }
    let report = tick_once(&f.app, &stop, due).await.unwrap();
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(report.admitted.len(), 1);
    let request = report.admitted[0].clone();
    assert!(
        tick_once(&f.app, &stop, due)
            .await
            .unwrap()
            .admitted
            .is_empty()
    );
    // A stopped sweep creates no extra request and does not fabricate cancel.
    let cancelled = tokio_util::sync::CancellationToken::new();
    cancelled.cancel();
    assert!(
        tick_once(&f.app, &cancelled, due + 120)
            .await
            .unwrap()
            .admitted
            .is_empty()
    );
    let binary = native_application_fixture::observer_binary();
    let artifact =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .register_observer(artifact)
        .await
        .unwrap();
    let worker_stop = tokio_util::sync::CancellationToken::new();
    let worker = tokio::spawn(crate::native_scans::worker(
        f.app.clone(),
        worker_stop.clone(),
    ));
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let view = f
                .call(
                    "GET",
                    &format!("/api/v2/scans/{request}"),
                    None,
                    &[("authorization", &auth)],
                )
                .await;
            assert_eq!(view.status, StatusCode::OK, "{}", view.body);
            if view.body["status"] == "complete" {
                return;
            }
            assert_ne!(view.body["status"], "failed", "{}", view.body);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await;
    worker_stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(120), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    outcome.unwrap();
    f.close().await;
    let missing_runtime = schedule_call(&f, "GET", &path, None, &[("authorization", &auth)]).await;
    assert_eq!(missing_runtime.status, StatusCode::SERVICE_UNAVAILABLE);
    f.open().await;
    assert!(
        tick_once(&f.app, &stop, due)
            .await
            .unwrap()
            .admitted
            .is_empty()
    );
    let after = schedule_call(&f, "GET", &path, None, &[("authorization", &auth)]).await;
    assert_eq!(after.status, StatusCode::OK, "{}", after.body);
    assert_eq!(after.body, changed.body);
    let tag = after.headers["etag"].to_str().unwrap();
    let deleted = schedule_call(
        &f,
        "DELETE",
        &path,
        None,
        &[("authorization", &auth), ("if-match", tag)],
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert!(
        tick_once(&f.app, &stop, due + 1000)
            .await
            .unwrap()
            .admitted
            .is_empty()
    );
    assert_eq!(
        schedule_call(&f, "GET", &path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let retained: (i64,i64,i64) = sqlx::query_as("SELECT tombstoned,(SELECT count(*) FROM catabolic_scan_schedule_occurrences WHERE schedule_id=d.id),(SELECT count(*) FROM api_schedules WHERE id=d.id) FROM catabolic_scan_schedule_definitions d WHERE id=?").bind(&id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(retained, (1, 1, 0));
    f.close().await;
    f.open().await;
    assert_eq!(
        schedule_call(&f, "GET", &path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    f.close().await;
}

#[tokio::test]
async fn real_schedule_service_timer_admits_scans_and_drains_before_restart() {
    let mut f = Fixture::new().await;
    let auth = format!("Bearer {OPERATOR}");
    let root = f._dir.path().join("timer-source");
    std::fs::create_dir(&root).unwrap();
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Timer source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "timer-native-source-key"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let source_id = source.body["id"].as_str().unwrap();
    let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Timer library","kind":"movies","language":"en","source_ids":[source_id]})), &[("authorization",&auth),("idempotency-key","timer-native-library-key")]).await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let schedule = f.call("POST", "/api/v2/schedules", Some(json!({"library_id":library.body["id"],"task":"scan","interval_seconds":60,"enabled":true})), &[("authorization",&auth),("idempotency-key","timer-native-schedule-key")]).await;
    assert_eq!(schedule.status, StatusCode::CREATED, "{}", schedule.body);
    let id = schedule.body["id"].as_str().unwrap().to_owned();
    f.cutover_source(Some(source_id)).await;
    assert_eq!(
        adopt_retained(&f.app, &id).await.unwrap(),
        Adoption::Adopted
    );
    let due: i64 = sqlx::query_scalar(
        "SELECT due_at FROM catabolic_scan_schedule_cursors WHERE schedule_id=?",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    // This is the unchanged production timer loop: wall clock, 30-second sleep,
    // real 60-second definition. No clock virtualization or due-row mutation.
    let budget = std::time::Duration::from_secs(
        u64::try_from(due.saturating_sub(crate::now()).max(0)).unwrap() + 65,
    );
    let binary = native_application_fixture::observer_binary();
    let artifact =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .register_observer(artifact)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(f.app.storage.root.join("server.lock"))
        .unwrap();
    let startup = crate::native_service::startup_observation::observe(&f.app).unwrap();
    let service = crate::native_service::NativeService::start(
        f.app.clone(),
        None,
        listener,
        None,
        lock,
        tokio_util::sync::CancellationToken::new(),
    )
    .unwrap();
    // Every fallible observation below returns before assertions so the actual
    // service stop/close owns cleanup on timeout, HTTP failure and success.
    let observed: anyhow::Result<(String, i64)> = async {
        tokio::time::timeout(std::time::Duration::from_secs(10), startup.wait()).await??;
        let client = reqwest::Client::new();
        let url = format!("http://{address}/api/v2/schedules/{id}");
        let response = client.get(&url).header("host", "127.0.0.1:8787")
            .header("authorization", &auth).send().await?;
        ensure!(response.status().as_u16() == 200, "public service schedule read refused");
        let etag = response.headers().get("etag").context("schedule ETag missing")?.to_str()?.to_owned();
        let read: Value = serde_json::from_str(&response.text().await?)?;
        ensure!(read["id"] == id, "service returned different schedule");
        let (request, observed_due) = tokio::time::timeout(budget, async {
            loop {
                let occurrence: Option<(String,i64)> = sqlx::query_as("SELECT request_id,due_at FROM catabolic_scan_schedule_occurrences WHERE schedule_id=? ORDER BY due_at")
                    .bind(&id).fetch_optional(&f.app.db).await?;
                if let Some(value) = occurrence { return Ok::<_,sqlx::Error>(value); }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }).await??;
        // Disable through real HTTP/CAS before another due occurrence, without
        // changing the accepted request or cursor using test SQL.
        let disabled = client.put(&url).header("host", "127.0.0.1:8787")
            .header("authorization", &auth).header("if-match", etag)
            .header("content-type", "application/json")
            .body(json!({"library_id":library.body["id"],"task":"scan","interval_seconds":60,"enabled":false}).to_string())
            .send().await?;
        ensure!(disabled.status().as_u16() == 200, "service schedule disable refused: {}", disabled.text().await?);
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                let response = client.get(format!("http://{address}/api/v2/scans/{request}"))
                    .header("host", "127.0.0.1:8787").header("authorization", &auth).send().await?;
                ensure!(response.status().as_u16() == 200, "service scan read refused");
                let view: Value = serde_json::from_str(&response.text().await?)?;
                if view["status"] == "complete" { return Ok::<_,anyhow::Error>(()); }
                ensure!(view["status"] != "failed", "scheduled scan failed: {view}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }).await??;
        Ok((request, observed_due))
    }.await;
    let stopped = service.stop().await;
    let closed = match stopped {
        Ok(stopped) => stopped.close().await,
        Err(error) => Err(error),
    };
    closed.expect("actual native service did not drain/close");
    // NativeService closes the shared pool only after all actual owners join.
    // Restart uses a fresh pool rather than querying a closed clone.
    f.app.db = crate::db::connect_catalog_control(&f._dir.path().join("db.sqlite"))
        .await
        .unwrap();
    let (request, observed_due) = observed.expect("public scheduled service flow failed");
    assert_eq!(observed_due, due);
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_scan_schedule_occurrences WHERE schedule_id=?",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(rows, 1);
    let next: i64 = sqlx::query_scalar(
        "SELECT due_at FROM catabolic_scan_schedule_cursors WHERE schedule_id=?",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert!(next > due);
    // Service close already shut down this exact retained runtime.
    drop(f.app.native_catalog.take());
    f.open().await;
    let again = fire_one(&f.app, &id, due).await.unwrap();
    assert_eq!(again, Fire::Idle);
    let saved:String=sqlx::query_scalar("SELECT request_id FROM catabolic_scan_schedule_occurrences WHERE schedule_id=? AND due_at=?").bind(&id).bind(due).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(saved, request);
    let retained = schedule_call(
        &f,
        "GET",
        &format!("/schedules/{id}"),
        None,
        &[("authorization", &auth)],
    )
    .await;
    assert_eq!(retained.status, StatusCode::OK, "{}", retained.body);
    assert_eq!(retained.body["enabled"], false);
    let completed = f
        .call(
            "GET",
            &format!("/api/v2/scans/{request}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
    assert_eq!(completed.body["status"], "complete");
    f.close().await;
}

#[tokio::test]
async fn service_drain_refuses_inflight_tick_without_new_demand() {
    let mut f = Fixture::new().await;
    f.cutover_source(None).await;
    let stop = tokio_util::sync::CancellationToken::new();
    let runtime = f.app.native_catalog.as_ref().unwrap().clone();
    let (host_entered, entered) = tokio::sync::oneshot::channel();
    let (release_host, hold_host) = std::sync::mpsc::channel();
    let holder = tokio::spawn(async move {
        runtime
            .with_host(move |_| {
                let _ = host_entered.send(());
                hold_host
                    .recv()
                    .map_err(|_| anyhow::anyhow!("host test barrier dropped"))?;
                Ok(())
            })
            .await
    });
    entered.await.unwrap();
    let (read_entered, entered) = tokio::sync::oneshot::channel();
    let (release_tick, hold_tick) = tokio::sync::oneshot::channel();
    let app = f.app.clone();
    let tick_stop = stop.clone();
    let tick = tokio::spawn(async move {
        super::tick::tick_after_read_barrier(&app, &tick_stop, read_entered, hold_tick).await
    });
    // Genuine read transaction is acquired. We deliberately do not claim its
    // host callback has queued: the separate Runtime gate test covers that race.
    let entered = tokio::time::timeout(std::time::Duration::from_secs(10), entered).await;
    f.app.native_catalog.as_ref().unwrap().begin_drain();
    stop.cancel();
    let _ = release_tick.send(());
    let _ = release_host.send(());
    let held_result = holder.await;
    let result = tick.await;
    // Release and join every actual owner before assertions on either outcome.
    entered
        .expect("tick did not acquire read transaction")
        .unwrap();
    held_result.unwrap().unwrap();
    let refused = result
        .unwrap()
        .expect_err("draining tick must not obtain admission");
    assert!(super::tick::cancelled_before_admission(&stop, &refused));
    for table in [
        "catabolic_scan_requests",
        "catabolic_scan_schedule_occurrences",
        "catabolic_scan_schedule_cursors",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.app.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "tick changed {table}");
    }
    f.close().await;
}
