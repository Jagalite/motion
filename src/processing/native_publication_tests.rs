//! Real renderer reservation/recovery at the private production phase boundary.
use super::*;
use crate as playscale_app;
use axum::http::StatusCode;
use serde_json::json;
#[path = "../../tests/support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR, observer_binary};

async fn control_caller(f: &Fixture, auth: &str) -> crate::v2::auth::Caller {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", auth.parse().unwrap());
    headers.insert("host", "127.0.0.1:8787".parse().unwrap());
    crate::v2::auth::resolve(&f.app, &axum::http::Method::POST, &headers)
        .await
        .unwrap()
}

async fn private_control(
    f: &Fixture,
    job: &str,
    caller: crate::v2::auth::Caller,
    key: &str,
    reviewed: Option<&str>,
    retry: bool,
) -> Result<native_application_fixture::Reply, crate::v2::Problem> {
    use http_body_util::BodyExt;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("idempotency-key", key.parse().unwrap());
    if let Some(reviewed) = reviewed {
        headers.insert("if-match", reviewed.parse().unwrap());
    }
    let response = if retry {
        crate::v2::jobs::retry(State(f.app.clone()), caller, Path(job.into()), headers).await?
    } else {
        crate::v2::jobs::cancel(State(f.app.clone()), caller, Path(job.into()), headers).await?
    };
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Ok(native_application_fixture::Reply {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap(),
    })
}

async fn job_etag(f: &Fixture, job: &str) -> String {
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM processing_jobs WHERE id=?")
        .bind(job)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    format!("\"r-{revision}\"")
}

// Preserve the production check and expose its original diagnostic only in
// this fixture. The public 409 intentionally does not reveal native internals.
async fn assert_native_admission_current(f: &Fixture, job: &str, stage: &str) {
    let row = load(&f.app, job).await.unwrap();
    let mut tx = crate::v2::playback::begin_catalog_application(&f.app, true)
        .await
        .unwrap();
    let checked = crate::native_processing::validate_admission_in(
        f.app.native_catalog.as_ref().unwrap(),
        &mut tx,
        &row,
    )
    .await;
    tx.rollback().await.unwrap();
    if let Err(error) = checked {
        // This is an actual local fixture with no concurrently running native
        // worker. Retain the failed production check, then expose both native
        // manifests only in its test diagnostic. In particular, an epoch-only
        // change must not be mistaken for permission to ignore that fence.
        let raw: String = sqlx::query_scalar(
            "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
        )
        .bind(job)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
        let capture: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let version = capture["version_id"].as_str().unwrap().to_owned();
        let current = f
            .app
            .native_catalog
            .as_ref()
            .unwrap()
            .with_host(move |host| {
                host.projection_version_manifest(host.catalog_generation()?, &[version])
                    .map_err(Into::into)
            })
            .await;
        panic!(
            "native admission at {stage}: {error:#}; captured_manifest={}; current_manifest={current:?}",
            capture["manifest"]
        );
    }
}

async fn public_control(
    f: &Fixture,
    job: &str,
    auth: &str,
    key: &str,
    reviewed: Option<&str>,
    retry: bool,
) -> native_application_fixture::Reply {
    let mut headers = vec![("authorization", auth), ("idempotency-key", key)];
    if let Some(reviewed) = reviewed {
        headers.push(("if-match", reviewed));
    }
    f.call(
        "POST",
        &format!(
            "/api/v2/jobs/{job}/{}",
            if retry { "retry" } else { "cancel" }
        ),
        None,
        &headers,
    )
    .await
}

#[tokio::test]
async fn native_processing_controls_keep_restart_scope_stable_and_reject_new_epoch() {
    // Direct API admission precedes every presentation request. Startup must
    // establish the fixed sidebar selection, not rely on a UI warm-up fixture.
    let (mut f, job, _, auth) = admitted_fixture().await;
    assert_native_admission_current(&f, &job, "direct admission before restart or UI").await;
    let before = job_etag(&f, &job).await;
    for _ in 0..2 {
        f.close().await;
        f.open().await;
        assert_native_admission_current(&f, &job, "repeated native startup").await;
        assert_eq!(job_etag(&f, &job).await, before);
    }
    let presentation = f
        .call("GET", "/processing", None, &[("authorization", &auth)])
        .await;
    assert_eq!(presentation.status, StatusCode::OK, "{}", presentation.body);
    assert_native_admission_current(&f, &job, "first processing navigation after restart").await;
    assert_eq!(job_etag(&f, &job).await, before);

    let key = "native-scope-stable-cancel";
    let cancelled = public_control(&f, &job, &auth, key, None, false).await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    let reviewed = cancelled.headers["etag"].to_str().unwrap();
    // A genuinely new native definition still advances the retained global
    // fence. Eager fixed-scope preparation must not exempt later mutations or
    // rewrite an existing admission to make a retry appear current.
    f.app
        .native_catalog
        .as_ref()
        .unwrap()
        .with_host(|host| {
            host.prepare_selection(
                "native-control-new-epoch",
                &json!({"version":1,"mode":"selection","selection":{
                    "language":"sql","query":"SELECT item_id FROM catalog_items WHERE 0"
                }}),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let refused = public_control(
        &f,
        &job,
        &auth,
        "native-scope-stale-retry",
        Some(reviewed),
        true,
    )
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    assert_eq!(refused.body["code"], "preparation_native_changed");
    let current = load(&f.app, &job).await.unwrap();
    assert_eq!(current.phase, "cancelled");
    assert_eq!(current.attempt, 0);
    assert_eq!(job_etag(&f, &job).await, reviewed);
    // This is response recovery only: new native state cannot turn an exact
    // old cancellation receipt into either a new action or an unapproved retry.
    let replay = public_control(&f, &job, &auth, key, None, false).await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.body, cancelled.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_records WHERE target=? AND operation IN ('cancelNativeProcessingJob','retryNativeProcessingJob')")
        .bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(receipts, 1);
    f.close().await;
}

#[tokio::test]
async fn native_processing_controls_public_routes_hints_and_original_key_replay() {
    let (mut f, job, _, auth) = admitted_fixture().await;
    assert_native_admission_current(&f, &job, "initial public fixture").await;
    let job_path = format!("/api/v2/jobs/{job}");
    let before = f
        .call("GET", &job_path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    assert_eq!(
        before.body["controls"],
        json!({"family":"native_preparation","version":1,"cohort":true,"cancel_request_available":true,"retry_request_available":false})
    );
    let before_etag = before.headers["etag"].to_str().unwrap().to_owned();
    assert_native_admission_current(&f, &job, "public job detail").await;
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,requester_id,created_at,updated_at) SELECT 'public-history',source_file_id,source_revision,recipe,backend,'native-public-history-row','failed',requester_id,1,1 FROM processing_jobs WHERE id=?")
        .bind(&job).execute(&f.app.db).await.unwrap();
    assert_native_admission_current(&f, &job, "historical processing row insertion").await;
    sqlx::query("INSERT INTO preview_jobs(id,file_id,file_revision,requester_id,created_at) SELECT 'public-preview',source_file_id,source_revision,requester_id,1 FROM processing_jobs WHERE id=?")
        .bind(&job).execute(&f.app.db).await.unwrap();
    assert_native_admission_current(&f, &job, "unsupported preview row insertion").await;
    let list = f
        .call("GET", "/api/v2/jobs", None, &[("authorization", &auth)])
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    let history = list.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "public-history")
        .unwrap();
    assert_eq!(history["kind"], "rendition");
    assert_eq!(history["controls"]["cohort"], false);
    assert_eq!(history["controls"]["retry_request_available"], false);
    assert_native_admission_current(&f, &job, "mixed history job list").await;
    let presentation = f
        .call("GET", "/processing", None, &[("authorization", &auth)])
        .await;
    assert_eq!(presentation.status, StatusCode::OK, "{}", presentation.body);
    let html = presentation.body.as_str().unwrap();
    assert!(html.contains(&format!("data-command=\"POST /api/v2/jobs/{job}/cancel\"")));
    assert!(html.contains("Read-only job history"));
    assert!(!html.contains("data-command=\"POST /api/v2/jobs/public-history/"));
    assert_native_admission_current(&f, &job, "mixed history and processing presentation").await;
    for unsupported in ["public-history", "public-preview", "unknown-job"] {
        for retry in [false, true] {
            let refused = public_control(
                &f,
                unsupported,
                &auth,
                "native-public-unsupported",
                Some("\"r-1\""),
                retry,
            )
            .await;
            assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.body);
        }
    }
    assert_native_admission_current(&f, &job, "unsupported public controls").await;
    let (_, token) = f
        .pair(&["default"], &["catalog:read", "processing:request"])
        .await;
    let foreign = public_control(
        &f,
        &job,
        &format!("Bearer {token}"),
        "native-public-foreign",
        None,
        false,
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(
        f.call(
            "POST",
            &format!("{job_path}/cancel"),
            None,
            &[("idempotency-key", "native-public-anonymous")]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("CREATE TRIGGER public_control_receipt_failure BEFORE INSERT ON idempotency_records WHEN NEW.operation='cancelNativeProcessingJob' BEGIN SELECT RAISE(ABORT,'injected public control receipt failure'); END")
        .execute(&f.app.db).await.unwrap();
    let rolled_back =
        public_control(&f, &job, &auth, "native-public-cancel-exact", None, false).await;
    assert_eq!(
        rolled_back.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        rolled_back.body
    );
    assert_eq!(load(&f.app, &job).await.unwrap().phase, "queued");
    assert_eq!(job_etag(&f, &job).await, before_etag);
    sqlx::query("DROP TRIGGER public_control_receipt_failure")
        .execute(&f.app.db)
        .await
        .unwrap();
    let cancelled =
        public_control(&f, &job, &auth, "native-public-cancel-exact", None, false).await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.body["phase"], "cancelled");
    assert_eq!(cancelled.body["controls"]["retry_request_available"], true);
    let replay = public_control(&f, &job, &auth, "native-public-cancel-exact", None, false).await;
    assert_eq!(replay.body, cancelled.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(
        public_control(&f, &job, &auth, "native-public-missing-review", None, true)
            .await
            .status,
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        public_control(
            &f,
            &job,
            &auth,
            "native-public-stale-review",
            Some(&before_etag),
            true
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let reviewed = cancelled.headers["etag"].to_str().unwrap().to_owned();
    assert_native_admission_current(&f, &job, "cancelled public job before exact retry").await;
    let retry = public_control(
        &f,
        &job,
        &auth,
        "native-public-retry-exact",
        Some(&reviewed),
        true,
    )
    .await;
    assert_eq!(retry.status, StatusCode::OK, "{}", retry.body);
    assert_eq!(retry.body["phase"], "queued");
    assert_eq!(retry.body["attempt_generation"], "0");
    assert_eq!(retry.body["controls"]["retry_request_available"], false);
    assert_eq!(
        public_control(
            &f,
            &job,
            &auth,
            "native-public-retry-new-key",
            Some(&reviewed),
            true
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    // The response may have been lost to the requester. Its original intent
    // remains replayable even once the worker has published and hints are false.
    let stop = CancellationToken::new();
    let worker = tokio::spawn(super::worker(f.app.clone(), stop.clone()));
    finish_worker(&f, &job, stop, worker).await;
    let completed = f
        .call("GET", &job_path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(completed.body["phase"], "completed");
    assert_eq!(completed.body["attempt_generation"], "1");
    assert_eq!(completed.body["controls"]["cohort"], true);
    assert_eq!(
        completed.body["controls"]["cancel_request_available"],
        false
    );
    assert_eq!(completed.body["controls"]["retry_request_available"], false);
    let replay = public_control(
        &f,
        &job,
        &auth,
        "native-public-retry-exact",
        Some(&reviewed),
        true,
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.body, retry.body);
    assert_eq!(replay.headers["etag"], retry.headers["etag"]);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let replay = public_control(&f, &job, &auth, "native-public-cancel-exact", None, false).await;
    assert_eq!(replay.body, cancelled.body);
    for action in [false, true] {
        let denied = public_control(
            &f,
            &job,
            &auth,
            "native-public-recorded-effect",
            Some(completed.headers["etag"].to_str().unwrap()),
            action,
        )
        .await;
        assert_eq!(denied.status, StatusCode::CONFLICT, "{}", denied.body);
        assert_eq!(denied.body["code"], "publication_recorded");
    }
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM catabolic_generated_publications WHERE job_id=?),(SELECT count(*) FROM idempotency_records WHERE target=? AND operation IN ('cancelNativeProcessingJob','retryNativeProcessingJob'))")
        .bind(&job).bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(counts, (1, 2));
    assert_eq!(load(&f.app, &job).await.unwrap().attempt, 1);
    assert_eq!(
        load(&f.app, "public-history").await.unwrap().phase,
        "failed"
    );
    f.close().await;
    assert_eq!(
        public_control(&f, &job, &auth, "native-public-owner-missing", None, false)
            .await
            .status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn native_processing_controls_public_reauthorize_replay_after_withdrawal() {
    let (mut f, job, device, auth) = admitted_fixture().await;
    let operator = format!("Bearer {OPERATOR}");
    let (admin, admin_token) = f
        .pair(
            &["default"],
            &["catalog:read", "processing:request", "system:admin"],
        )
        .await;
    let admin_auth = format!("Bearer {admin_token}");
    let cancelled = public_control(
        &f,
        &job,
        &admin_auth,
        "native-public-admin-cancel",
        None,
        false,
    )
    .await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    let current = f
        .call(
            "GET",
            &format!("/api/v2/devices/{admin}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    let downgraded = f.call("PUT", &format!("/api/v2/devices/{admin}/policy"), Some(json!({"library_ids":[],"permissions":["catalog:read","processing:request"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]})), &[("authorization", &operator), ("if-match", current.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(downgraded.status, StatusCode::OK, "{}", downgraded.body);
    assert_eq!(
        public_control(
            &f,
            &job,
            &admin_auth,
            "native-public-admin-cancel",
            None,
            false
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let owner_receipt =
        public_control(&f, &job, &auth, "native-public-owner-cancel", None, false).await;
    assert_eq!(
        owner_receipt.status,
        StatusCode::OK,
        "{}",
        owner_receipt.body
    );
    let current = f
        .call(
            "GET",
            &format!("/api/v2/devices/{device}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    let downgraded = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({"library_ids":[],"permissions":["catalog:read"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]})), &[("authorization", &operator), ("if-match", current.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(downgraded.status, StatusCode::OK, "{}", downgraded.body);
    let read = f
        .call(
            "GET",
            &format!("/api/v2/jobs/{job}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body["controls"]["cohort"], true);
    assert_eq!(read.body["controls"]["retry_request_available"], false);
    assert_eq!(
        public_control(&f, &job, &auth, "native-public-owner-cancel", None, false)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let reviewed = job_etag(&f, &job).await;
    let denied = public_control(
        &f,
        &job,
        &operator,
        "native-public-owner-revoked",
        Some(&reviewed),
        true,
    )
    .await;
    assert_eq!(denied.status, StatusCode::CONFLICT, "{}", denied.body);
    assert_eq!(denied.body["code"], "preparation_authorization_changed");
    let current = f
        .call(
            "GET",
            &format!("/api/v2/devices/{device}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{device}"),
            None,
            &[
                ("authorization", &operator),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    assert_eq!(
        public_control(&f, &job, &auth, "native-public-owner-cancel", None, false)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let source: String = sqlx::query_scalar("SELECT json_extract(capture_json,'$.occurrence.source.library_id') FROM catabolic_processing_admissions WHERE job_id=?")
        .bind(&job).fetch_one(&f.app.db).await.unwrap();
    let path = format!("/api/v2/sources/{source}");
    let current = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    let withdrawn = f
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
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
    assert_eq!(withdrawn.body["availability"], "unavailable");
    let stopped = public_control(
        &f,
        &job,
        &operator,
        "native-public-withdrawn-cancel",
        None,
        false,
    )
    .await;
    assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.body);
    assert_eq!(stopped.body["phase"], "cancelled");
    let replay = public_control(
        &f,
        &job,
        &operator,
        "native-public-withdrawn-cancel",
        None,
        false,
    )
    .await;
    assert_eq!(replay.body, stopped.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(
        public_control(
            &f,
            &job,
            &operator,
            "native-public-withdrawn-retry",
            Some(&reviewed),
            true
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(load(&f.app, &job).await.unwrap().attempt, 0);
    let publications: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications WHERE job_id=?")
            .bind(&job)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(publications, 0);
    f.close().await;
}

async fn finish_worker(
    f: &Fixture,
    job: &str,
    stop: CancellationToken,
    worker: tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT phase FROM processing_jobs WHERE id=?")
                .bind(job)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
            if matches!(phase.as_str(), "completed" | "failed" | "cancelled")
                || worker.is_finished()
            {
                break phase;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(30), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap(), "completed");
}

#[tokio::test]
async fn native_processing_controls_are_private_transactional_reviewed_and_replayable() {
    let (mut f, job, _, auth) = admitted_fixture().await;
    let caller = control_caller(&f, &auth).await;
    let (_, outsider_token) = f
        .pair(&["default"], &["catalog:read", "processing:request"])
        .await;
    let outsider = control_caller(&f, &format!("Bearer {outsider_token}")).await;
    assert_eq!(
        private_control(&f, &job, outsider, "native-control-foreign", None, false)
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::NOT_FOUND
    );
    // Historical processing rows can be read but cannot acquire a native
    // execution admission from this transport. The frozen api_jobs union is not used.
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at) SELECT 'legacy-history',source_file_id,source_revision,recipe,backend,'native-control-legacy-history','failed',1,1 FROM processing_jobs WHERE id=?")
        .bind(&job).execute(&f.app.db).await.unwrap();
    assert_eq!(
        private_control(
            &f,
            "legacy-history",
            caller.clone(),
            "native-control-legacy",
            None,
            false
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::NOT_FOUND
    );
    let before = job_etag(&f, &job).await;
    for action in ["cancel", "retry"] {
        let response = f
            .call(
                "GET",
                &format!("/api/v2/jobs/{job}/{action}"),
                None,
                &[
                    ("authorization", &auth),
                    ("idempotency-key", "native-control-wrong-method"),
                    ("if-match", "\"r-1\""),
                ],
            )
            .await;
        assert_eq!(
            response.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{}",
            response.body
        );
        // Axum's method router rejects GET before the POST-only handler's
        // native admission. A matched route does not grant another method.
        assert_eq!(response.body["code"], "method_not_allowed");
        assert_eq!(job_etag(&f, &job).await, before);
    }
    let control_receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_records WHERE target=? AND operation IN ('cancelNativeProcessingJob','retryNativeProcessingJob')")
        .bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(control_receipts, 0);
    sqlx::query("CREATE TRIGGER reject_private_control_receipt BEFORE INSERT ON idempotency_records WHEN NEW.operation='cancelNativeProcessingJob' BEGIN SELECT RAISE(ABORT,'injected control receipt failure'); END")
        .execute(&f.app.db).await.unwrap();
    let receipt_error = private_control(
        &f,
        &job,
        caller.clone(),
        "native-control-cancel-exact",
        None,
        false,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(receipt_error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(load(&f.app, &job).await.unwrap().phase, "queued");
    assert_eq!(job_etag(&f, &job).await, before);
    sqlx::query("DROP TRIGGER reject_private_control_receipt")
        .execute(&f.app.db)
        .await
        .unwrap();
    let cancelled = private_control(
        &f,
        &job,
        caller.clone(),
        "native-control-cancel-exact",
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(cancelled.body["phase"], "cancelled");
    assert_eq!(cancelled.body["attempt_generation"], "0");
    let replay = private_control(
        &f,
        &job,
        caller.clone(),
        "native-control-cancel-exact",
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(replay.body, cancelled.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(
        private_control(
            &f,
            &job,
            caller.clone(),
            "native-control-retry-missing",
            None,
            true
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        private_control(
            &f,
            &job,
            caller.clone(),
            "native-control-retry-stale",
            Some(&before),
            true
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let reviewed = job_etag(&f, &job).await;
    assert_native_admission_current(&f, &job, "cancelled private job before exact retry").await;
    let retried = private_control(
        &f,
        &job,
        caller.clone(),
        "native-control-retry-exact",
        Some(&reviewed),
        true,
    )
    .await
    .unwrap();
    assert_eq!(retried.body["phase"], "queued");
    assert_eq!(retried.body["attempt_generation"], "0");
    assert_eq!(
        private_control(
            &f,
            &job,
            caller.clone(),
            "native-control-retry-new-key-stale",
            Some(&reviewed),
            true
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let replay = private_control(
        &f,
        &job,
        caller,
        "native-control-retry-exact",
        Some(&reviewed),
        true,
    )
    .await
    .unwrap();
    assert_eq!(replay.body, retried.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM idempotency_records WHERE target=? AND operation IN ('cancelNativeProcessingJob','retryNativeProcessingJob')),(SELECT count(*) FROM catabolic_generated_publications WHERE job_id=?)")
        .bind(&job).bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(counts, (2, 0));
    // Only the real worker starts attempt 1 and creates the native output.
    let stop = CancellationToken::new();
    let worker = tokio::spawn(super::worker(f.app.clone(), stop.clone()));
    finish_worker(&f, &job, stop, worker).await;
    let completed = load(&f.app, &job).await.unwrap();
    assert_eq!(completed.attempt, 1);
    assert!(completed.output_file_id.is_some());
    f.close().await;
}

#[tokio::test]
async fn native_processing_controls_reauthenticate_replay_and_original_requester() {
    let (mut f, job, device, auth) = admitted_fixture().await;
    let caller = control_caller(&f, &auth).await;
    private_control(
        &f,
        &job,
        caller.clone(),
        "native-control-auth-cancel",
        None,
        false,
    )
    .await
    .unwrap();
    let operator_auth = format!("Bearer {OPERATOR}");
    let operator = control_caller(&f, &operator_auth).await;
    let (other_admin, other_admin_token) = f
        .pair(
            &["default"],
            &["catalog:read", "processing:request", "system:admin"],
        )
        .await;
    let cached_admin = control_caller(&f, &format!("Bearer {other_admin_token}")).await;
    let current_admin = f
        .call(
            "GET",
            &format!("/api/v2/devices/{other_admin}"),
            None,
            &[("authorization", &operator_auth)],
        )
        .await;
    let downgraded = f.call("PUT", &format!("/api/v2/devices/{other_admin}/policy"), Some(json!({
        "library_ids":[],"permissions":["catalog:read", "processing:request"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]
    })), &[("authorization", &operator_auth), ("if-match", current_admin.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(downgraded.status, StatusCode::OK, "{}", downgraded.body);
    assert_eq!(
        private_control(
            &f,
            &job,
            cached_admin,
            "native-control-downgraded-admin",
            None,
            false
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::NOT_FOUND
    );
    let current = f
        .call(
            "GET",
            &format!("/api/v2/devices/{device}"),
            None,
            &[("authorization", &operator_auth)],
        )
        .await;
    let policy = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({
        "library_ids":[],"permissions":["catalog:read"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]
    })), &[("authorization", &operator_auth), ("if-match", current.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(policy.status, StatusCode::OK, "{}", policy.body);
    assert_eq!(
        private_control(
            &f,
            &job,
            caller.clone(),
            "native-control-auth-cancel",
            None,
            false
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::FORBIDDEN
    );
    let reviewed = job_etag(&f, &job).await;
    let refused = private_control(
        &f,
        &job,
        operator.clone(),
        "native-control-admin-retry",
        Some(&reviewed),
        true,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code, "preparation_authorization_changed");
    sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
        .bind(&device)
        .execute(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        private_control(&f, &job, caller, "native-control-auth-cancel", None, false)
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::UNAUTHORIZED
    );
    // A current administrator may still request cancellation after the
    // preparation's original credential was revoked, without reauthorizing it.
    let cancelled = private_control(
        &f,
        &job,
        operator.clone(),
        "native-control-admin-cancel",
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(cancelled.body["phase"], "cancelled");
    assert_eq!(load(&f.app, &job).await.unwrap().attempt, 0);
    let source: String = sqlx::query_scalar("SELECT json_extract(capture_json,'$.occurrence.source.library_id') FROM catabolic_processing_admissions WHERE job_id=?")
        .bind(&job).fetch_one(&f.app.db).await.unwrap();
    let source_path = format!("/api/v2/sources/{source}");
    let current_source = f
        .call(
            "GET",
            &source_path,
            None,
            &[("authorization", &operator_auth)],
        )
        .await;
    assert_eq!(
        current_source.status,
        StatusCode::OK,
        "{}",
        current_source.body
    );
    // The real source owner refuses withdrawal while processing is active;
    // this terminal retained preparation permits an actual withdrawal.
    let withdrawn = f
        .call(
            "DELETE",
            &source_path,
            None,
            &[
                ("authorization", &operator_auth),
                ("if-match", current_source.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(withdrawn.status, StatusCode::OK, "{}", withdrawn.body);
    assert_eq!(withdrawn.body["availability"], "unavailable");
    assert_eq!(
        private_control(
            &f,
            &job,
            operator.clone(),
            "native-control-after-withdrawal-cancel",
            None,
            false
        )
        .await
        .unwrap()
        .body["phase"],
        "cancelled"
    );
    assert_eq!(
        private_control(
            &f,
            &job,
            operator.clone(),
            "native-control-admin-cancel",
            None,
            false
        )
        .await
        .unwrap()
        .headers["idempotent-replayed"],
        "true"
    );
    let reviewed = job_etag(&f, &job).await;
    assert_eq!(
        private_control(
            &f,
            &job,
            operator,
            "native-control-after-withdrawal-retry",
            Some(&reviewed),
            true
        )
        .await
        .err()
        .unwrap()
        .status,
        StatusCode::CONFLICT
    );
    f.close().await;
}

#[tokio::test]
async fn native_processing_controls_keep_real_worker_capacity_until_owner_release() {
    let (mut f, id, _, auth) = admitted_fixture().await;
    let caller = control_caller(&f, &auth).await;
    recover(&f.app).await.unwrap();
    let lease = f
        .app
        .processing
        .execution
        .reserve(
            "control-owned-attempt".into(),
            playscale_core::work::Class::Preparation,
            1,
        )
        .await
        .unwrap();
    let permit = Arc::new(f.app.processing.io.clone().acquire_owned().await.unwrap());
    let job = claim_queued(&f.app).await.unwrap().unwrap();
    assert_eq!(job.id, id);
    let cancelled = private_control(
        &f,
        &id,
        caller.clone(),
        "native-control-cancel-running",
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(cancelled.body["phase"], "cancelling");
    assert_eq!(cancelled.body["attempt_generation"], "1");
    assert_eq!(f.app.processing.execution.snapshot().used, 1);
    assert_eq!(f.app.processing.io.available_permits(), 0);
    // Exercise the actual worker pre-I/O fence, not a synthetic terminal row.
    assert!(
        convert(
            &f.app,
            &job,
            &CancellationToken::new(),
            permit.clone(),
            &lease
        )
        .await
        .is_err()
    );
    finish_native_failure(&f.app, &job).await.unwrap();
    assert_eq!(load(&f.app, &id).await.unwrap().phase, "cancelled");
    let reviewed = job_etag(&f, &id).await;
    let retry = private_control(
        &f,
        &id,
        caller,
        "native-control-retry-running",
        Some(&reviewed),
        true,
    )
    .await
    .unwrap();
    assert_eq!(retry.body["phase"], "queued");
    assert_eq!(retry.body["attempt_generation"], "1");
    let stop = CancellationToken::new();
    let worker = tokio::spawn(super::worker(f.app.clone(), stop.clone()));
    let blocked = tokio::time::timeout(Duration::from_secs(10), async {
        while f.app.processing.execution.snapshot().waiting == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let waiting = load(&f.app, &id).await.unwrap();
    drop(permit);
    drop(lease);
    if blocked.is_err() {
        stop.cancel();
    }
    finish_worker(&f, &id, stop, worker).await;
    blocked.unwrap();
    assert_eq!((waiting.phase.as_str(), waiting.attempt), ("queued", 1));
    assert_eq!(load(&f.app, &id).await.unwrap().attempt, 2);
    f.close().await;
}

#[tokio::test]
async fn native_processing_controls_preserve_reserved_effect_and_lost_ack_recovery() {
    let (mut f, job, command, _) = reserved_fixture().await;
    f.open().await;
    let caller = control_caller(&f, &format!("Bearer {OPERATOR}")).await;
    let reviewed = job_etag(&f, &job).await;
    for retry in [false, true] {
        let denied = private_control(
            &f,
            &job,
            caller.clone(),
            if retry {
                "native-control-reserved-retry"
            } else {
                "native-control-reserved-cancel"
            },
            Some(&reviewed),
            retry,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(denied.status, StatusCode::CONFLICT);
        assert_eq!(denied.code, "publication_committing");
        let public = public_control(
            &f,
            &job,
            &format!("Bearer {OPERATOR}"),
            "native-public-reserved-barrier",
            Some(&reviewed),
            retry,
        )
        .await;
        assert_eq!(public.status, StatusCode::CONFLICT, "{}", public.body);
        assert_eq!(public.body["code"], "publication_committing");
    }
    assert_eq!(load(&f.app, &job).await.unwrap().phase, "running");
    // Actual native effect commits; the application ACK alone fails. No
    // forged receipt or rewritten job phase stands in for the crash boundary.
    sqlx::query("CREATE TRIGGER control_fail_generated_ack BEFORE UPDATE OF phase ON processing_jobs WHEN NEW.phase='completed' BEGIN SELECT RAISE(ABORT,'injected application ACK failure'); END")
        .execute(&f.app.db).await.unwrap();
    crate::native_processing::recover_reserved(&f.app, CancellationToken::new())
        .await
        .unwrap();
    let saved: (String, String, bool) = sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
        .bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(saved.0, command);
    assert!(!saved.2);
    let pending = load(&f.app, &job).await.unwrap();
    assert_eq!(
        (
            pending.phase.as_str(),
            pending.attempt,
            pending.output_file_id
        ),
        ("running", 1, None)
    );
    for retry in [false, true] {
        let denied = private_control(
            &f,
            &job,
            caller.clone(),
            if retry {
                "native-control-recorded-retry"
            } else {
                "native-control-recorded-cancel"
            },
            Some(&reviewed),
            retry,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(denied.code, "publication_recorded");
        let public = public_control(
            &f,
            &job,
            &format!("Bearer {OPERATOR}"),
            "native-public-receipt-barrier",
            Some(&reviewed),
            retry,
        )
        .await;
        assert_eq!(public.status, StatusCode::CONFLICT, "{}", public.body);
        assert_eq!(public.body["code"], "publication_recorded");
    }
    sqlx::query("DROP TRIGGER control_fail_generated_ack")
        .execute(&f.app.db)
        .await
        .unwrap();
    crate::native_processing::recover_reserved(&f.app, CancellationToken::new())
        .await
        .unwrap();
    let completed = load(&f.app, &job).await.unwrap();
    assert_eq!(
        (completed.phase.as_str(), completed.attempt),
        ("completed", 1)
    );
    assert!(completed.output_file_id.is_some());
    let recovered: (String, String, bool) = sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
        .bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        (recovered.0.as_str(), recovered.1.as_str(), recovered.2),
        (saved.0.as_str(), saved.1.as_str(), true)
    );
    let reviewed = job_etag(&f, &job).await;
    for retry in [false, true] {
        assert_eq!(
            private_control(
                &f,
                &job,
                caller.clone(),
                if retry {
                    "native-control-completed-retry"
                } else {
                    "native-control-completed-cancel"
                },
                Some(&reviewed),
                retry
            )
            .await
            .err()
            .unwrap()
            .code,
            "publication_recorded"
        );
    }
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM catabolic_generated_publications WHERE job_id=?),(SELECT count(*) FROM idempotency_records WHERE target=? AND operation IN ('cancelNativeProcessingJob','retryNativeProcessingJob'))")
        .bind(&job).bind(&job).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(counts, (1, 0));
    f.close().await;
    f.open().await;
    assert_eq!(load(&f.app, &job).await.unwrap().phase, "completed");
    f.close().await;
}

async fn admitted_fixture() -> (Fixture, String, String, String) {
    let mut f = Fixture::new().await;
    let root = f._dir.path().join("reserved-source");
    std::fs::create_dir(&root).unwrap();
    let media = root.join("original.mp4");
    let generated = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
        ])
        .arg(&media)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let (device, token) = f
        .pair(
            &["default"],
            &["system:admin", "catalog:read", "processing:request"],
        )
        .await;
    // Policy recovery must never rely on bypassing the last-administrator rule.
    f.pair(&["default"], &["system:admin", "catalog:read"])
        .await;
    let auth = format!("Bearer {token}");
    f.cutover().await;
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Reservation source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &format!("Bearer {OPERATOR}")),
                ("idempotency-key", "reserved-output-source"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{:?}", source.body);
    let binary = observer_binary();
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
    crate::native_discovery::scan(
        &f.app,
        source.body["id"].as_str().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (timeline,file,revision):(String,String,String)=sqlx::query_as("SELECT v.timeline_id,f.id,f.revision FROM media_versions v JOIN version_files b ON b.version_id=v.id JOIN media_files f ON f.id=b.file_id WHERE v.origin='original'")
        .fetch_one(&f.app.db).await.unwrap();
    let admitted=f.call("POST","/api/v2/processing/preparations",Some(json!({"source":{"file_id":file,"file_revision":revision},"timeline_id":timeline,"recipe_id":"remux_mp4","backend":"software","video_profile":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"audio_track_id":null,"subtitle_track_id":null,"subtitle_policy":"off"}})),
        &[("authorization",&auth),("idempotency-key","reserved-output-preparation")]).await;
    assert_eq!(admitted.status, StatusCode::ACCEPTED, "{:?}", admitted.body);
    let job_id = admitted.body["id"].as_str().unwrap().to_owned();
    (f, job_id, device, auth)
}

async fn reserved_fixture() -> (Fixture, String, String, String) {
    let (mut f, job_id, device, _) = admitted_fixture().await;
    recover(&f.app).await.unwrap();
    let lease = f
        .app
        .processing
        .execution
        .reserve(
            "reservation-fixture".into(),
            playscale_core::work::Class::Preparation,
            1,
        )
        .await
        .unwrap();
    let permit = Arc::new(f.app.processing.io.clone().acquire_owned().await.unwrap());
    let job = claim_queued(&f.app).await.unwrap().unwrap();
    assert_eq!(job.id, job_id);
    let stop = CancellationToken::new();
    let (completed, _) = convert(&f.app, &job, &stop, permit.clone(), &lease)
        .await
        .unwrap();
    let reserved =
        crate::native_processing::reserve_render(&f.app, &job, completed, stop, permit.clone())
            .await
            .unwrap();
    let operation = reserved.plan.operation_id.clone();
    // This exact production function returns only after durable reservation,
    // and has not dispatched generated-output admission. Drop all I/O custody.
    drop(reserved);
    drop(permit);
    drop(lease);
    let state:(String,Option<String>,bool)=sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE operation_id=?")
        .bind(&operation).fetch_one(&f.app.db).await.unwrap();
    assert!(state.1.is_none() && !state.2);
    let outputs: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files WHERE generated=1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(outputs, 0);
    f.close().await;
    (f, job_id, state.0, device)
}

#[tokio::test]
async fn exact_reserved_render_recovers_after_shutdown_without_rerender() {
    let (mut f, job_id, command, _) = reserved_fixture().await;
    f.open().await;
    // Ordinary startup worker owns revalidation and exact saved dispatch.
    recover(&f.app).await.unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(worker(f.app.clone(), stop.clone()));
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let row: (String, i64, Option<String>) = sqlx::query_as(
                "SELECT phase,attempt,output_file_id FROM processing_jobs WHERE id=?",
            )
            .bind(&job_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
            if row.0 == "completed" || task.is_finished() {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(30), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let result = result.expect("exact saved publication recovery timed out");
    assert_eq!(result.0, "completed");
    assert_eq!(result.1, 1);
    assert!(result.2.is_some());
    let saved:(String,Option<String>,bool)=sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
        .bind(&job_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(saved.0, command);
    assert!(saved.1.is_some() && saved.2);
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications WHERE job_id=?")
            .bind(&job_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    f.close().await;
    f.open().await;
    let again: (String, i64, String) =
        sqlx::query_as("SELECT phase,attempt,output_file_id FROM processing_jobs WHERE id=?")
            .bind(&job_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(again, ("completed".into(), 1, result.2.unwrap()));
    f.close().await;
}

#[tokio::test]
async fn changed_reserved_output_cannot_publish_or_rerender_on_recovery() {
    let (mut f, job_id, command, _) = reserved_fixture().await;
    let relative: String = sqlx::query_scalar(
        "SELECT output_relative FROM catabolic_generated_publications WHERE job_id=?",
    )
    .bind(&job_id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let path = f
        .app
        .processing
        .root
        .parent()
        .unwrap()
        .join("native-generated")
        .join(relative);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    f.open().await;
    recover(&f.app).await.unwrap();
    crate::native_processing::recover_reserved(&f.app, CancellationToken::new())
        .await
        .unwrap();
    let row: (String, i64, Option<String>) =
        sqlx::query_as("SELECT phase,attempt,output_file_id FROM processing_jobs WHERE id=?")
            .bind(&job_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(row, ("running".into(), 1, None));
    let saved:(String,Option<String>,bool)=sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
        .bind(&job_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(saved, (command, None, false));
    let outputs: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files WHERE generated=1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(outputs, 0);
    f.close().await;
}

#[tokio::test]
async fn reserved_render_requires_restored_current_requester_authority() {
    let (mut f, job_id, command, device) = reserved_fixture().await;
    f.open().await;
    let auth = format!("Bearer {OPERATOR}");
    for (permissions, expected_ack) in [
        (json!(["catalog:read"]), false),
        (
            json!(["system:admin", "catalog:read", "processing:request"]),
            true,
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
        assert_eq!(current.status, StatusCode::OK, "{:?}", current.body);
        let policy = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({
            "library_ids":[],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions
        })), &[("authorization", &auth),("if-match", current.headers["etag"].to_str().unwrap())]).await;
        assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
        crate::native_processing::recover_reserved(&f.app, CancellationToken::new())
            .await
            .unwrap();
        let saved:(String,Option<String>,bool)=sqlx::query_as("SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?")
            .bind(&job_id).fetch_one(&f.app.db).await.unwrap();
        assert_eq!(saved.0, command);
        assert_eq!(saved.1.is_some(), expected_ack);
        assert_eq!(saved.2, expected_ack);
        let job: (String, i64, Option<String>) =
            sqlx::query_as("SELECT phase,attempt,output_file_id FROM processing_jobs WHERE id=?")
                .bind(&job_id)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        assert_eq!(job.0, if expected_ack { "completed" } else { "running" });
        assert_eq!(job.1, 1);
        assert_eq!(job.2.is_some(), expected_ack);
    }
    let attempts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications WHERE job_id=?")
            .bind(&job_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
    f.close().await;
}

#[tokio::test]
async fn changed_reserved_input_cannot_publish_exact_saved_output() {
    let (mut f, job_id, command, _) = reserved_fixture().await;
    let relative: String = sqlx::query_scalar(
        "SELECT output_relative FROM catabolic_generated_publications WHERE job_id=?",
    )
    .bind(&job_id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let output = f
        .app
        .processing
        .root
        .parent()
        .unwrap()
        .join("native-generated")
        .join(relative);
    let saved_output = std::fs::read(&output).unwrap();
    let input = f._dir.path().join("reserved-source/original.mp4");
    let mut bytes = std::fs::read(&input).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&input, bytes).unwrap();
    f.open().await;
    recover(&f.app).await.unwrap();
    crate::native_processing::recover_reserved(&f.app, CancellationToken::new())
        .await
        .unwrap();
    let row: (String, i64, Option<String>) =
        sqlx::query_as("SELECT phase,attempt,output_file_id FROM processing_jobs WHERE id=?")
            .bind(&job_id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(row, ("running".into(), 1, None));
    let saved:(String,Option<String>,bool)=sqlx::query_as(
        "SELECT command_json,receipt_json,acknowledged FROM catabolic_generated_publications WHERE job_id=?"
    ).bind(&job_id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(saved, (command, None, false));
    assert_eq!(std::fs::read(output).unwrap(), saved_output);
    let outputs: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files WHERE generated=1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(outputs, 0);
    f.close().await;
}
