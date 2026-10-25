//! Bounded controls for native-admitted processing jobs.
//! No catalog mutation, publication recovery, process signal, or cleanup occurs
//! here. The existing worker retains all actual execution and byte custody.
use super::*;
use playscale_core::native_processing_control::{self as policy, Control, Publication, Refusal};

/// Historical cohort identity only. Never constructs native authority, checks
/// current source health, or changes the cancellation/replay boundary.
async fn cohort_in(conn: &mut sqlx::SqliteConnection, id: &str) -> Result<bool, Problem> {
    let capture: Option<(String, String, String)> = sqlx::query_as("SELECT a.capture_json,j.source_file_id,j.source_revision FROM catabolic_processing_admissions a JOIN preparations p ON p.job_id=a.job_id JOIN processing_jobs j ON j.id=a.job_id WHERE a.job_id=?")
        .bind(id).fetch_optional(conn).await?;
    let Some((raw, source, revision)) = capture else {
        return Ok(false);
    };
    let Ok(capture) = serde_json::from_str::<Value>(&raw) else {
        return Ok(false);
    };
    Ok(capture["contract_version"] == 1
        && capture["job_id"].as_str() == Some(id)
        && capture["occurrence"]["motion_file"]["id"].as_str() == Some(source.as_str())
        && capture["occurrence"]["motion_file"]["revision"].as_str() == Some(revision.as_str()))
}

/// Descriptive hints share the caller's current read/write snapshot. They
/// never replace handler authorization, publication barriers or exact replay.
pub(super) async fn wire_in(
    conn: &mut sqlx::SqliteConnection,
    principal: &access::Principal,
    row: &Row,
) -> Result<Value, Problem> {
    let cohort = cohort_in(conn, &row.id).await?;
    let may_control = access::owns_job(principal, &row.requester_id)
        && principal.allows(Permission::ProcessingRequest);
    let mut hints = policy::RequestHints::default();
    if cohort && may_control {
        if let (Ok(phase), Ok(attempt)) = (
            serde_json::from_value(json!(row.phase)),
            row.attempt.try_into(),
        ) {
            hints = policy::request_hints(
                &Job { phase, attempt },
                row.result_id.is_some(),
                publication_in(conn, &row.id).await?,
                true,
                true,
            );
        }
    }
    let mut body = row.wire();
    body["controls"] = json!({"family":"native_preparation","version":1,"cohort":cohort,
        "cancel_request_available":hints.cancel_request_available,"retry_request_available":hints.retry_request_available});
    Ok(body)
}

async fn publication_in(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Publication, Problem> {
    // Every historical attempt matters, including acknowledged commands. An
    // absent local receipt never proves the native command had no effect.
    // Unknown outcomes take precedence over a receipt in some other attempt.
    let barrier: i64 = sqlx::query_scalar(
        "SELECT coalesce(max(CASE WHEN command_json IS NOT NULL AND receipt_json IS NULL THEN 3 WHEN receipt_json IS NOT NULL AND acknowledged=0 THEN 2 WHEN acknowledged=1 THEN 1 ELSE 0 END),0) FROM catabolic_generated_publications WHERE job_id=?",
    ).bind(id).fetch_one(conn).await?;
    Ok(match barrier {
        0 => Publication::Unreserved,
        1 => Publication::Acknowledged,
        2 => Publication::ReceiptRecorded,
        3 => Publication::Reserved,
        _ => return Err(Problem::internal("invalid publication disposition")),
    })
}

fn refusal(reason: Refusal) -> Problem {
    match reason {
        Refusal::StaleRevision => access::AccessError::StaleRevision.into(),
        Refusal::NotRetryable => Problem::new(
            StatusCode::CONFLICT,
            "not_retryable",
            "Only failed or cancelled jobs with attempts remaining can retry",
        ),
        Refusal::PublicationCommitting => Problem::new(
            StatusCode::CONFLICT,
            "publication_committing",
            "Publication is reserved and may already have committed; reconcile the original receipt",
        ),
        Refusal::PublicationRecorded => Problem::new(
            StatusCode::CONFLICT,
            "publication_recorded",
            "A durable publication receipt is retained; this control cannot change its attempt",
        ),
        Refusal::OutputRecorded => Problem::new(
            StatusCode::CONFLICT,
            "processing_output_recorded",
            "This job records an output and cannot change its attempt",
        ),
    }
}

pub(super) async fn control(
    app: &App,
    caller: &Caller,
    id: &str,
    headers: &HeaderMap,
    retry: bool,
) -> Result<Response, Problem> {
    let key = idempotency_key(headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = if retry {
        // Projection refresh precedes the writer. Never rebuild while holding
        // it, and never make cancellation depend on a readable/current source.
        super::super::playback::begin_catalog_application(app, true).await?
    } else {
        begin_write(&app.db).await?
    };
    let principal = caller.reauthorize(&mut tx, None).await?;
    let old = super::native::row_in(&mut tx, &principal, id).await?;
    if !principal.allows(Permission::ProcessingRequest) {
        return Err(access::AccessError::Forbidden(Permission::ProcessingRequest).into());
    }
    let job: crate::processing::ProcessingJob =
        sqlx::query_as("SELECT * FROM processing_jobs WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    // Descriptive legacy/history rows remain readable, not adopted into native
    // execution by a control. This immutable record identifies the cohort; it
    // is not itself fresh native authority or a byte/publication grant.
    if !cohort_in(&mut tx, id).await? {
        return Err(Problem::not_found());
    }
    let reviewed = if retry {
        Some(if_match(headers)?)
    } else {
        None
    };
    // Separate from legacy controls so cutover cannot reinterpret an older
    // backend's response as a native control admission.
    let record = Scope {
        principal: &principal.id,
        operation: if retry {
            "retryNativeProcessingJob"
        } else {
            "cancelNativeProcessingJob"
        },
        target: id,
        key: &key,
    };
    let digest = super::super::sha256(b"");
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|r| &r.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        let stored = stored.unwrap();
        let body = stored.body.unwrap_or_default();
        let mut response = (stored.status, Json(body.clone())).into_response();
        if let Some(revision) = body["revision"].as_str().and_then(|s| s.parse().ok()) {
            response.headers_mut().insert(header::ETAG, etag(revision));
        }
        response
            .headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(response);
    }
    let state = Job {
        phase: serde_json::from_value(json!(old.phase)).map_err(Problem::internal)?,
        attempt: old.attempt.try_into().map_err(Problem::internal)?,
    };
    let action = match reviewed {
        Some(reviewed_revision) => Control::Retry {
            current_revision: old.revision.try_into().map_err(Problem::internal)?,
            reviewed_revision,
        },
        None => Control::Cancel,
    };
    let next = policy::decide(
        &state,
        job.output_file_id.is_some(),
        publication_in(&mut tx, id).await?,
        action,
    )
    .map_err(refusal)?;
    if retry {
        let (_, facts) = super::super::processing::publication_facts(&mut tx, &job)
            .await
            .map_err(Problem::internal)?
            .ok_or_else(Problem::not_found)?;
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM processing_jobs WHERE phase IN ('queued','running','cancelling')",
        )
        .fetch_one(&mut *tx)
        .await?;
        policy::admit_retry_work(&facts, queued).map_err(|code| {
            Problem::new(
                if code == "processing_limit" {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::CONFLICT
                },
                code,
                "The original preparation cannot currently be queued",
            )
        })?;
        // Reuse the actual renderer's immutable version/occurrence admission,
        // including current native manifest, lifecycle and source identity.
        // The worker must still reacquire real source descriptors before I/O.
        crate::native_processing::validate_admission_in(
            app.native_catalog.as_ref().ok_or_else(Problem::not_found)?,
            &mut tx,
            &job,
        )
        .await
        .map_err(|_| {
            Problem::new(
                StatusCode::CONFLICT,
                "preparation_native_changed",
                "The original native preparation is no longer current",
            )
        })?;
    }
    if next != state {
        let changed = sqlx::query("UPDATE processing_jobs SET phase=?,error=NULL,cache_cleaned=0,updated_at=? WHERE id=? AND revision=? AND phase=? AND attempt=?")
            .bind(crate::db::phase_name(next.phase)).bind(crate::now()).bind(id)
            .bind(old.revision).bind(&old.phase).bind(old.attempt).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(Problem::internal(
                "native processing control changed under writer custody",
            ));
        }
    }
    let updated = super::native::row_in(&mut tx, &principal, id).await?;
    let body = wire_in(&mut tx, &principal, &updated).await?;
    record
        .save(&mut tx, &digest, StatusCode::OK, Some(&body), None)
        .await?;
    tx.commit().await?;
    Ok(super::native::response(&updated, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    async fn native_control_hints_need_matching_admission_and_current_permission() {
        let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        // Only application-owned history. No native projection, api_jobs,
        // source ownership, catalog rows, byte grants or native runtime exist.
        sqlx::raw_sql("CREATE TABLE processing_jobs(id TEXT PRIMARY KEY,source_file_id TEXT,source_revision TEXT);
            CREATE TABLE preparations(job_id TEXT PRIMARY KEY);
            INSERT INTO processing_jobs VALUES('job','source','revision');")
            .execute(&mut conn).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0087_catabolic_generated_publications.sql"
        ))
        .execute(&mut conn)
        .await
        .unwrap();
        let mut row = Row {
            id: "job".into(),
            revision: 1,
            kind: "rendition".into(),
            phase: "queued".into(),
            attempt: 0,
            error: None,
            result_id: None,
            requester_id: "owner".into(),
        };
        let mut owner = access::Principal::operator();
        owner.id = "owner".into();
        owner.grant.permissions = [Permission::ProcessingRequest].into();
        let history = wire_in(&mut conn, &owner, &row).await.unwrap();
        assert_eq!(
            history["controls"],
            json!({"family":"native_preparation","version":1,"cohort":false,"cancel_request_available":false,"retry_request_available":false})
        );
        let capture = json!({"contract_version":1,"job_id":"job","occurrence":{"motion_file":{"id":"source","revision":"revision"}}});
        sqlx::query("INSERT INTO catabolic_processing_admissions VALUES('job',?,1)")
            .bind(capture.to_string())
            .execute(&mut conn)
            .await
            .unwrap();
        assert!(
            !cohort_in(&mut conn, "job").await.unwrap(),
            "admission alone is not a preparation"
        );
        sqlx::query("INSERT INTO preparations VALUES('job')")
            .execute(&mut conn)
            .await
            .unwrap();
        let admitted = wire_in(&mut conn, &owner, &row).await.unwrap();
        assert_eq!(admitted["controls"]["cohort"], true);
        assert_eq!(admitted["controls"]["cancel_request_available"], true);
        assert_eq!(admitted["controls"]["retry_request_available"], false);
        row.phase = "cancelled".into();
        assert_eq!(
            wire_in(&mut conn, &owner, &row).await.unwrap()["controls"]["retry_request_available"],
            true
        );
        let mut reader = owner.clone();
        reader.grant.permissions = [Permission::CatalogRead].into();
        let restricted = wire_in(&mut conn, &reader, &row).await.unwrap();
        assert_eq!(restricted["controls"]["cohort"], true);
        assert_eq!(restricted["controls"]["retry_request_available"], false);
        owner.id = "another-owner".into();
        assert_eq!(
            wire_in(&mut conn, &owner, &row).await.unwrap()["controls"]["retry_request_available"],
            false
        );
        owner.id = "owner".into();
        sqlx::query("INSERT INTO catabolic_generated_publications(operation_id,job_id,attempt,source_json,output_relative,output_json,command_json,created_at) VALUES('reserved','job',1,'{}','out.mp4','{}','{}',1)").execute(&mut conn).await.unwrap();
        assert_eq!(
            wire_in(&mut conn, &owner, &row).await.unwrap()["controls"]["retry_request_available"],
            false
        );
        assert!(
            sqlx::query(
                "INSERT INTO catabolic_processing_admissions VALUES('invalid-json','not json',1)"
            )
            .execute(&mut conn)
            .await
            .is_err()
        );
        for (index, invalid) in ["{}".to_owned(), json!({"contract_version":1,"job_id":"foreign","occurrence":{"motion_file":{"id":"source","revision":"revision"}}}).to_string()].into_iter().enumerate() {
            row.id = format!("invalid-{index}");
            sqlx::query("INSERT INTO processing_jobs VALUES(?,'source','revision')").bind(&row.id).execute(&mut conn).await.unwrap();
            sqlx::query("INSERT INTO preparations VALUES(?)").bind(&row.id).execute(&mut conn).await.unwrap();
            // Each malformed/mismatched capture is a separate retained record;
            // even a fixture must not overwrite immutable admission history.
            sqlx::query("INSERT INTO catabolic_processing_admissions VALUES(?,?,1)").bind(&row.id).bind(invalid).execute(&mut conn).await.unwrap();
            assert_eq!(wire_in(&mut conn, &owner, &row).await.unwrap()["controls"]["cohort"], false);
        }
    }

    #[tokio::test]
    async fn native_control_publication_barrier_includes_every_historical_attempt() {
        let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0087_catabolic_generated_publications.sql"
        ))
        .execute(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            publication_in(&mut conn, "job").await.unwrap(),
            Publication::Unreserved
        );
        sqlx::query("INSERT INTO catabolic_generated_publications(operation_id,job_id,attempt,source_json,output_relative,created_at) VALUES('old','job',1,'{}','old.mp4',1)")
            .execute(&mut conn).await.unwrap();
        assert_eq!(
            publication_in(&mut conn, "job").await.unwrap(),
            Publication::Unreserved
        );
        sqlx::query("UPDATE catabolic_generated_publications SET output_json='{}',command_json='{}' WHERE operation_id='old'")
            .execute(&mut conn).await.unwrap();
        assert_eq!(
            publication_in(&mut conn, "job").await.unwrap(),
            Publication::Reserved
        );
        sqlx::query("UPDATE catabolic_generated_publications SET receipt_json='{}' WHERE operation_id='old'")
            .execute(&mut conn).await.unwrap();
        assert_eq!(
            publication_in(&mut conn, "job").await.unwrap(),
            Publication::ReceiptRecorded
        );
        sqlx::query(
            "UPDATE catabolic_generated_publications SET acknowledged=1 WHERE operation_id='old'",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        sqlx::query("INSERT INTO catabolic_generated_publications(operation_id,job_id,attempt,source_json,output_relative,created_at) VALUES('later','job',2,'{}','later.mp4',2)")
            .execute(&mut conn).await.unwrap();
        assert_eq!(
            publication_in(&mut conn, "job").await.unwrap(),
            Publication::Acknowledged
        );
        sqlx::query("UPDATE catabolic_generated_publications SET output_json='{}',command_json='{}' WHERE operation_id='later'")
            .execute(&mut conn).await.unwrap();
        assert_eq!(
            publication_in(&mut conn, "job").await.unwrap(),
            Publication::Reserved,
            "an older ACK cannot characterize a later unknown outcome"
        );
        assert_eq!(
            publication_in(&mut conn, "other").await.unwrap(),
            Publication::Unreserved
        );
    }
}
