//! Deterministic interleaving at the same refresh/acquire boundary production uses.
use super::*;
use crate as playscale_app;
use axum::{body::Body as AxumBody, extract::FromRequestParts, http::Request};
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "../../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

async fn advance(f: &Fixture, ordinal: usize) {
    let native = f.app.native_catalog.as_ref().unwrap();
    let id = format!("admission-race-{ordinal}");
    let command = native
        .prepare_work(
            id.clone(),
            id.clone(),
            "Intervening owner".into(),
            "movie".into(),
            format!("race-request-{ordinal}"),
        )
        .await
        .unwrap();
    let tx = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let (outcome, mut tx) = native.execute_work(command.clone(), tx).await.unwrap();
    motion_catalog_backend::revision_bridge::acknowledge_fresh_in(&mut tx, &id, &command, &outcome)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn forward_generation_converges_before_handler_and_rechecks_revoked_caller() {
    let mut f = Fixture::new().await;
    let (device, token) = f
        .pair(&["default"], &["catalog:read", "playback:request"])
        .await;
    f.cutover().await;
    let (mut parts, _) = Request::builder()
        .uri("/api/v2/processing/renditions")
        .header("authorization", format!("Bearer {token}"))
        .header("host", "127.0.0.1:8787")
        .body(AxumBody::empty())
        .unwrap()
        .into_parts();
    let caller = Caller::from_request_parts(&mut parts, &f.app)
        .await
        .unwrap();
    let native = f.app.native_catalog.as_ref().unwrap().clone();
    let refreshes = AtomicUsize::new(0);
    let mut tx = begin_current_projection(&f.app.db, &native, || async {
        native
            .rebuild_projection()
            .await
            .map_err(Problem::internal)?;
        let count = refreshes.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            advance(&f, 0).await;
            let operator = format!("Bearer {OPERATOR}");
            let path = format!("/api/v2/devices/{device}");
            let current = f
                .call("GET", &path, None, &[("authorization", &operator)])
                .await;
            assert_eq!(current.status, StatusCode::OK);
            let revoked = f
                .call(
                    "DELETE",
                    &path,
                    None,
                    &[
                        ("authorization", &operator),
                        ("if-match", current.headers["etag"].to_str().unwrap()),
                    ],
                )
                .await;
            assert_eq!(revoked.status, StatusCode::NO_CONTENT);
        }
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(refreshes.load(Ordering::SeqCst), 2);
    native.validate_projection(&mut tx).await.unwrap();
    let denied = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await
        .unwrap_err();
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM preparations")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        effects, 0,
        "admission/retry must not execute a handler effect"
    );
    tx.rollback().await.unwrap();
    let response = plan(axum::extract::State(f.app.clone()), caller, plan_body()).await;
    let error = match response {
        Ok(_) => panic!("revoked captured caller received a plan"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    drop(native);
    f.close().await;
}

fn plan_body() -> super::Body<PlanInput> {
    let value = json!({"profile_id":"default","timeline_id":"not-disclosed","version_id":null,"source":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null},
        "quality":{"mode":"auto","max_bitrate_bps":null,"max_height":null,"allow_client_software":false,"hdr_policy":"preserve_if_supported"},
        "client":{"client_id":"admission-test","client_build":"1","demuxe_asset_digest":null,"transports":["http_range"],"video_codecs":["h264"],"audio_codecs":["aac"],"subtitle_modes":[],"hdr":"sdr","max_height":null,"software_decode":"none","cross_origin_isolated":false}});
    super::Body {
        digest: crate::v2::sha256(value.to_string().as_bytes()),
        value: serde_json::from_value(value).unwrap(),
    }
}

#[tokio::test]
async fn actual_plan_reauthorizes_permission_after_caller_extraction() {
    let mut f = Fixture::new().await;
    let (device, token) = f
        .pair(&["default"], &["catalog:read", "playback:request"])
        .await;
    f.cutover().await;
    let (mut parts, _) = Request::builder()
        .uri("/api/v2/playback/plans")
        .header("authorization", format!("Bearer {token}"))
        .header("host", "127.0.0.1:8787")
        .body(AxumBody::empty())
        .unwrap()
        .into_parts();
    let caller = Caller::from_request_parts(&mut parts, &f.app)
        .await
        .unwrap();
    let operator = format!("Bearer {OPERATOR}");
    let path = format!("/api/v2/devices/{device}/policy");
    let current = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK);
    let mut policy = current.body.clone();
    policy.as_object_mut().unwrap().remove("revision");
    policy["permissions"] = json!(["catalog:read"]);
    let changed = f
        .call(
            "PUT",
            &path,
            Some(policy),
            &[
                ("authorization", &operator),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{:?}", changed.body);
    let response = plan(axum::extract::State(f.app.clone()), caller, plan_body()).await;
    let error = match response {
        Ok(_) => panic!("stale PlaybackRequest permission disclosed a plan"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    f.close().await;
}

#[tokio::test]
async fn repeated_actual_forward_commits_exhaust_admission_without_handler_effects() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let native = f.app.native_catalog.as_ref().unwrap().clone();
    let refreshes = AtomicUsize::new(0);
    let result = begin_current_projection(&f.app.db, &native, || async {
        native
            .rebuild_projection()
            .await
            .map_err(Problem::internal)?;
        let ordinal = refreshes.fetch_add(1, Ordering::SeqCst);
        advance(&f, ordinal).await;
        Ok(())
    })
    .await;
    let error = match result {
        Ok(tx) => {
            tx.rollback().await.unwrap();
            panic!("continuous forward commits must not admit stale facts")
        }
        Err(error) => error,
    };
    assert_eq!(refreshes.load(Ordering::SeqCst), 3);
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.code, "native_projection_busy");
    assert_eq!(error.retry_after, Some(1));
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM preparations")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(effects, 0);
    // Exhaustion released every stale writer, so a stable refresh can proceed.
    let tx = begin_catalog_application(&f.app, false).await.unwrap();
    tx.rollback().await.unwrap();
    drop(native);
    f.close().await;
}

#[tokio::test]
async fn typed_admission_preserves_real_drain_cause_and_public_error_contract() {
    let mut f = Fixture::new().await;
    f.cutover().await;
    let tx = begin_catalog_application_typed(&f.app, false)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    f.app.native_catalog.as_ref().unwrap().begin_drain();
    let error = begin_catalog_application_typed(&f.app, false)
        .await
        .err()
        .unwrap();
    assert!(
        error
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_some()
    );
    let public = application_problem(error);
    assert_eq!(public.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(public.code, "internal_error");
    f.close().await;
}

#[test]
fn typed_admission_public_problem_mapping_preserves_fields_and_distinguishes_causes() {
    let expected = Problem {
        retry_after: Some(1),
        ..Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "native_projection_busy",
            "retry",
        )
    };
    let error: anyhow::Error = ApplicationProblem(expected).into();
    assert!(
        error
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_none()
    );
    let public = application_problem(error.context("admission boundary"));
    assert_eq!(public.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(public.code, "native_projection_busy");
    assert_eq!(public.detail, "retry");
    assert_eq!(public.retry_after, Some(1));
    let genuine = anyhow::Error::new(crate::native_catalog::OrdinaryAdmissionDraining)
        .context("admission boundary");
    assert!(
        genuine
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_some()
    );
    let same_text = anyhow::anyhow!(crate::native_catalog::OrdinaryAdmissionDraining.to_string());
    assert!(
        same_text
            .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
            .is_none()
    );
}

#[tokio::test]
async fn rendition_poll_busy_does_not_queue_a_writer_and_rechecks_revoked_caller() {
    let mut f = Fixture::new().await;
    let (device, token) = f.pair(&["default"], &["processing:request"]).await;
    f.cutover().await;
    let (mut parts, _) = Request::builder()
        .uri("/api/v2/processing/renditions")
        .header("authorization", format!("Bearer {token}"))
        .header("host", "127.0.0.1:8787")
        .body(AxumBody::empty())
        .unwrap()
        .into_parts();
    let caller = Caller::from_request_parts(&mut parts, &f.app)
        .await
        .unwrap();
    let held = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let busy = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        begin_catalog_poll(&f.app),
    )
    .await
    .expect("held Motion owner must not queue the poll")
    .unwrap_err();
    assert_eq!(busy.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.code, "native_projection_busy");
    assert_eq!(busy.retry_after, Some(1));
    held.rollback().await.unwrap();

    // Hold the actual admitted native owner. Refusing this second lock must
    // drop the already acquired Motion reservation, not leave a queued waiter.
    let native = f.app.native_catalog.as_ref().unwrap().clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let owned = native.clone();
    let worker = tokio::spawn(async move {
        owned
            .with_host(move |host| {
                host.identity()?;
                let _ = started.send(());
                wait.recv()?;
                Ok(())
            })
            .await
    });
    ready.await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        begin_catalog_poll(&f.app),
    )
    .await;
    // Release/join before assertions so a failing diagnostic cannot strand owner.
    let available = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        f.app.db.begin_with("BEGIN IMMEDIATE"),
    )
    .await;
    release.send(()).unwrap();
    worker.await.unwrap().unwrap();
    let busy = result
        .expect("native owner must not queue the poll")
        .unwrap_err();
    assert_eq!(busy.code, "native_projection_busy");
    available
        .expect("busy poll leaked Motion custody")
        .unwrap()
        .rollback()
        .await
        .unwrap();

    // Actual credential withdrawal between busy and retry defeats the captured
    // ingress caller even though current native admission now succeeds.
    let operator = format!("Bearer {OPERATOR}");
    let path = format!("/api/v2/devices/{device}");
    let current = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(current.status, StatusCode::OK);
    let revoked = f
        .call(
            "DELETE",
            &path,
            None,
            &[
                ("authorization", &operator),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let mut tx = begin_catalog_poll(&f.app).await.unwrap();
    native.validate_projection(&mut tx).await.unwrap();
    let denied = caller
        .reauthorize(&mut tx, Some(Permission::ProcessingRequest))
        .await
        .unwrap_err();
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM preparations")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(effects, 0);
    tx.rollback().await.unwrap();
    let response = f
        .call(
            "GET",
            "/api/v2/processing/renditions",
            None,
            &[("authorization", &format!("Bearer {token}"))],
        )
        .await;
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    drop(native);
    f.close().await;
}
