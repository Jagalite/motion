//! Public processing-job observations over a real retained native runtime.
//! Seeded phases below exercise transport, not worker execution/publication.
//! The real renderer's test helper separately checks genuine worker outcomes.
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
};
use http_body_util::BodyExt;
use playscale as playscale_app;
use serde_json::{Value, json};

#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

async fn seed_job(f: &Fixture, id: &str, requester: &str, phase: &str) {
    let revision: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id='file'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,attempt,requester_id,error,output_file_id,created_at,updated_at) VALUES(?,'file',?,'remux_mp4','software',?,?,2,?,?,?,1,1)")
        .bind(id).bind(revision).bind(format!("read-fixture-{id}")).bind(phase).bind(requester)
        .bind((phase == "failed").then_some("private worker path and diagnostic"))
        .bind((phase == "completed").then_some("file"))
        .execute(&f.app.db).await.unwrap();
}

async fn admitted_caller(f: &Fixture, auth: &str) -> playscale_app::v2::auth::Caller {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", auth.parse().unwrap());
    headers.insert("host", "127.0.0.1:8787".parse().unwrap());
    playscale_app::v2::auth::resolve(&f.app, &Method::GET, &headers)
        .await
        .unwrap()
}

#[tokio::test]
async fn native_processing_job_reads_keep_ownership_pagination_and_historical_results() {
    let mut f = Fixture::new().await;
    let source = f
        .seed_preserved_timeline("jobs.txt", b"real source for descriptive job history")
        .await;
    let (owner, token) = f.pair(&["default"], &["catalog:read"]).await;
    let (other, other_token) = f.pair(&["default"], &["catalog:read"]).await;
    let auth = format!("Bearer {token}");
    let other_auth = format!("Bearer {other_token}");
    let operator = format!("Bearer {OPERATOR}");
    f.cutover_source(Some(&source)).await;
    // This qualified physical-history export excludes nonempty legacy job
    // tables. Do not bypass its inventory or fabricate retained frozen rows.
    // The activated owner must still reject new legacy scan/metadata writes.
    assert!(sqlx::query("INSERT INTO jobs(id,library_id,phase,requester_id,created_at) VALUES('frozen-scan',?,'failed',?,1)")
        .bind(&source).bind(&owner).execute(&f.app.db).await.is_err());
    assert!(sqlx::query("INSERT INTO metadata_jobs(id,item_id,requester_id,phase,created_at) VALUES('frozen-metadata','work',?,'failed',1)")
        .bind(&owner).execute(&f.app.db).await.is_err());
    // Backup history is application-owned after activation, not catalog or
    // native processing authority. It remains outside generic native controls.
    sqlx::query("INSERT INTO backup_jobs(id,requester_id,include_generated_media,phase,created_at) VALUES('history-backup',?,0,'failed',1)")
        .bind(&owner).execute(&f.app.db).await.unwrap();

    // These are inert application history rows, not processing admissions.
    // Their source FK is real, but none may create native output authority.
    seed_job(&f, "00-hidden", &other, "queued").await;
    seed_job(&f, "15-legacy", "legacy", "completed").await;
    let phases = [
        "queued",
        "running",
        "cancelling",
        "cancelled",
        "completed",
        "failed",
    ];
    let mut owned = Vec::new();
    for (index, phase) in phases.iter().enumerate() {
        let id = format!("{}-owned-{phase}", 10 + index * 10);
        seed_job(&f, &id, &owner, phase).await;
        owned.push(id);
    }
    // Another application-owned job kind is deliberately outside this cohort.
    sqlx::query("INSERT INTO preview_jobs(id,file_id,file_revision,requester_id,created_at) VALUES('preview-only','file','historical',?,1)")
        .bind(&owner).execute(&f.app.db).await.unwrap();
    let before: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM catabolic_generated_publications),(SELECT count(*) FROM catabolic_processing_admissions),(SELECT count(*) FROM processing_jobs)")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(before, (0, 0, 8));

    for path in ["/api/v2/jobs", "/api/v2/jobs/10-owned-queued"] {
        assert_eq!(
            f.call("GET", path, None, &[]).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    let mut cursor: Option<String> = None;
    let mut observed = Vec::new();
    loop {
        let path = match &cursor {
            Some(cursor) => format!("/api/v2/jobs?limit=2&cursor={cursor}"),
            None => "/api/v2/jobs?limit=2".into(),
        };
        let response = f
            .call("GET", &path, None, &[("authorization", &auth)])
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert!(response.body["read_revision"].is_string());
        assert!(response.body["event_cursor"].is_string());
        let items = response.body["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        observed.extend(
            items
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned()),
        );
        cursor = response.body["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert_eq!(cursor.as_deref(), observed.last().map(String::as_str));
    }
    assert_eq!(observed, owned);
    for (id, phase) in owned.iter().zip(phases) {
        let detail = f
            .call(
                "GET",
                &format!("/api/v2/jobs/{id}"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
        assert_eq!(detail.body["id"], *id);
        assert_eq!(detail.body["kind"], "rendition");
        assert_eq!(detail.body["phase"], phase);
        assert_eq!(detail.body["attempt_generation"], "2");
        assert_eq!(detail.body["requester_id"], owner);
        assert_eq!(
            detail.body["error_code"],
            if phase == "failed" {
                json!("worker_failed")
            } else {
                Value::Null
            }
        );
        assert_eq!(
            detail.body["result_ids"],
            if phase == "completed" {
                json!(["file"])
            } else {
                json!([])
            }
        );
        assert_eq!(detail.headers["etag"], "\"r-1\"");
        assert!(!detail.body.to_string().contains("private worker"));
        assert!(detail.body.get("source_file_id").is_none());
        assert!(detail.body.get("ready").is_none());
        let denied = f
            .call(
                "GET",
                &format!("/api/v2/jobs/{id}"),
                None,
                &[("authorization", &other_auth)],
            )
            .await;
        assert_eq!(denied.status, StatusCode::NOT_FOUND);
    }
    for id in ["00-hidden", "15-legacy", "preview-only", "unknown"] {
        let denied = f
            .call(
                "GET",
                &format!("/api/v2/jobs/{id}"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(
            denied.status,
            StatusCode::NOT_FOUND,
            "{id}: {}",
            denied.body
        );
    }
    // Admins retain known ownership semantics, including unattributed history.
    let all = f
        .call("GET", "/api/v2/jobs", None, &[("authorization", &operator)])
        .await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert_eq!(all.body["items"].as_array().unwrap().len(), 8);
    assert!(
        all.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["kind"] == "rendition")
    );
    let legacy = f
        .call(
            "GET",
            "/api/v2/jobs/15-legacy",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(legacy.status, StatusCode::OK, "{}", legacy.body);
    assert_eq!(legacy.body["requester_id"], "legacy");
    let unsupported = f
        .call(
            "GET",
            "/api/v2/jobs/preview-only",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(unsupported.status, StatusCode::NOT_FOUND);

    for path in [
        "/api/v2/jobs?limit=0",
        "/api/v2/jobs?limit=201",
        "/api/v2/jobs?cursor=",
    ] {
        assert_eq!(
            f.call("GET", path, None, &[("authorization", &auth)])
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    for path in [
        "/api/v2/jobs/10-owned-queued/cancel",
        "/api/v2/jobs/60-owned-failed/retry",
        "/api/v2/jobs/frozen-scan/cancel",
        "/api/v2/jobs/frozen-metadata/retry",
        "/api/v2/jobs/history-backup/retry",
        "/api/v2/jobs/preview-only/cancel",
    ] {
        let refused = f
            .call(
                "POST",
                path,
                None,
                &[
                    ("authorization", &operator),
                    ("idempotency-key", "unsupported-job-control"),
                    ("if-match", "\"r-1\""),
                ],
            )
            .await;
        assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.body);
        // Public control routes never adopt these inert history rows.
        assert_eq!(refused.body["code"], "not_found");
    }
    for path in ["/api/v2/jobs", "/api/v2/jobs/10-owned-queued"] {
        assert_eq!(
            f.call("HEAD", path, None, &[("authorization", &auth)])
                .await
                .status,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    let capabilities = f
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(capabilities.status, StatusCode::OK, "{}", capabilities.body);
    let capability = capabilities.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|feature| feature["id"] == "processing.native_job_reads")
        .unwrap();
    assert_eq!(capability["implemented"], true);
    assert_eq!(capability["enabled"], true);
    assert_eq!(capability["qualification"], "unqualified");
    assert_eq!(capability["limits"]["operations"], "list,get");
    assert_eq!(capability["limits"]["kinds"], "rendition");
    assert_eq!(
        capability["limits"]["unsupported_operations"],
        "cancel,retry"
    );
    assert_eq!(
        capability["limits"]["unsupported_kinds"],
        "scan,metadata,backup,preview"
    );
    assert_eq!(
        capability["limits"]["results"],
        "historical_ids_not_catalog_or_byte_authority"
    );
    let controls = capabilities.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|feature| feature["id"] == "processing.native_preparation_controls")
        .unwrap();
    assert_eq!(controls["implemented"], true);
    assert_eq!(controls["enabled"], true);
    assert_eq!(controls["qualification"], "unqualified");
    assert_eq!(controls["limits"]["contract_version"], "1");
    assert_eq!(controls["limits"]["cohort"], "native_preparation");
    assert_eq!(controls["limits"]["operations"], "cancel,retry");

    let presentation = f
        .call("GET", "/processing", None, &[("authorization", &operator)])
        .await;
    assert_eq!(presentation.status, StatusCode::OK, "{}", presentation.body);
    let html = presentation.body.as_str().unwrap();
    assert!(html.contains("Read-only job history"));
    assert!(html.contains("10-owned-queued") && html.contains("60-owned-failed"));
    assert!(!html.contains("data-command=\"POST /api/v2/jobs/"));
    assert!(!html.contains("Failed or cancelled work can be retried here"));

    // Neither completed-looking rows nor reads can manufacture a publication.
    let after: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM catabolic_generated_publications),(SELECT count(*) FROM catabolic_processing_admissions),(SELECT count(*) FROM processing_jobs)")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(after, before);
    for table in ["jobs", "metadata_jobs"] {
        let unchanged: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.app.db)
            .await
            .unwrap();
        assert_eq!(unchanged, 0, "native controls cannot create frozen {table}");
    }
    let backup: (String, i64) =
        sqlx::query_as("SELECT phase,attempt FROM backup_jobs WHERE id='history-backup'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(backup, ("failed".to_owned(), 0));
    assert!(
        sqlx::query(
            "INSERT INTO items(id,title,kind) VALUES('forged-job-read','Forbidden','movie')"
        )
        .execute(&f.app.db)
        .await
        .is_err()
    );
    f.close().await;
    let closed = f
        .call("GET", "/api/v2/jobs", None, &[("authorization", &auth)])
        .await;
    assert_eq!(closed.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(closed.body["code"], "native_runtime_unavailable");
    f.open().await;
    let reopened = f
        .call(
            "GET",
            "/api/v2/jobs/15-legacy",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(reopened.status, StatusCode::OK, "{}", reopened.body);
    assert_eq!(reopened.body, legacy.body);
    assert_eq!(reopened.headers["etag"], legacy.headers["etag"]);
    f.close().await;
}

#[tokio::test]
async fn native_job_reads_reauthenticate_after_admission_downgrade_and_revocation() {
    let mut f = Fixture::new().await;
    let source = f
        .seed_preserved_timeline("jobs.txt", b"real source for job reauthorization")
        .await;
    let (owner, token) = f.pair(&["default"], &["catalog:read"]).await;
    let (admin, admin_token) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let _reserve_admin = f.pair(&["default"], &["system:admin"]).await;
    f.cutover_source(Some(&source)).await;
    seed_job(&f, "owned", &owner, "queued").await;
    let auth = format!("Bearer {token}");
    let admin_auth = format!("Bearer {admin_token}");
    let operator = format!("Bearer {OPERATOR}");
    let cached_admin = admitted_caller(&f, &admin_auth).await;
    assert!(cached_admin.principal.is_admin());
    let device = f
        .call(
            "GET",
            &format!("/api/v2/devices/{admin}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(device.status, StatusCode::OK, "{}", device.body);
    let downgrade = f.call("PUT", &format!("/api/v2/devices/{admin}/policy"), Some(json!({
        "library_ids":[],"permissions":["catalog:read"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]
    })), &[("authorization", &operator), ("if-match", device.headers["etag"].to_str().unwrap())]).await;
    assert_eq!(downgrade.status, StatusCode::OK, "{}", downgrade.body);
    // Call after the admission snapshot was captured, not a fresh extractor.
    let denied = playscale_app::v2::jobs::get_job(
        State(f.app.clone()),
        cached_admin.clone(),
        Path("owned".into()),
    )
    .await;
    assert_eq!(denied.err().unwrap().status, StatusCode::NOT_FOUND);
    let list = playscale_app::v2::jobs::list(
        State(f.app.clone()),
        cached_admin,
        Query(playscale_app::v2::PageQuery {
            cursor: None,
            limit: None,
        }),
    )
    .await
    .unwrap();
    let body: Value =
        serde_json::from_slice(&list.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["items"], json!([]));

    let cached_owner = admitted_caller(&f, &auth).await;
    sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
        .bind(&owner)
        .execute(&f.app.db)
        .await
        .unwrap();
    let detail = playscale_app::v2::jobs::get_job(
        State(f.app.clone()),
        cached_owner.clone(),
        Path("owned".into()),
    )
    .await;
    assert_eq!(detail.err().unwrap().status, StatusCode::UNAUTHORIZED);
    let list = playscale_app::v2::jobs::list(
        State(f.app.clone()),
        cached_owner,
        Query(playscale_app::v2::PageQuery {
            cursor: None,
            limit: None,
        }),
    )
    .await;
    assert_eq!(list.err().unwrap().status, StatusCode::UNAUTHORIZED);
    for path in ["/api/v2/jobs", "/api/v2/jobs/owned"] {
        assert_eq!(
            f.call("GET", path, None, &[("authorization", &auth)])
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    f.close().await;
}
