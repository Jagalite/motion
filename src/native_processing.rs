//! Native catalog publication for Motion-owned rendering. Job/attempt and
//! subprocess ownership remain in `processing`; these rows only order durable
//! native publication and acknowledgment. Stored JSON is never a native grant.
use anyhow::{Context, ensure};
use serde_json::Value;
use sqlx::SqliteConnection;
mod admission;
pub(crate) use admission::validate_admission_in;
mod dispatch;
pub(crate) use dispatch::publish_render;
#[cfg(test)]
pub(crate) use dispatch::reserve_render;
mod journal;
mod read;
mod recovery;
mod request;
pub(crate) use read::{current_rendition_in, validate_prepared_rendition_in};
pub(crate) use recovery::{recover_reserved, recover_reserved_for_service};

#[derive(Clone, Debug)]
pub(crate) struct PublicationPlan {
    pub operation_id: String,
    pub job_id: String,
    pub attempt: i64,
    pub source: Value,
    pub output_relative: String,
}

/// Persist before copying into the final managed root. A crash may leave bytes,
/// but cannot make their locator eligible for untracked cleanup or another job.
/// Caller commits its Motion writer before external effects.
pub(crate) async fn plan_in(
    db: &mut SqliteConnection,
    plan: &PublicationPlan,
) -> anyhow::Result<()> {
    ensure!(
        !plan.operation_id.is_empty()
            && plan.operation_id.len() <= 256
            && !plan.job_id.is_empty()
            && plan.job_id.len() <= 128
            && plan.attempt > 0
            && plan.source.is_object(),
        "invalid generated publication identity"
    );
    let relative = std::path::Path::new(&plan.output_relative);
    ensure!(
        !plan.output_relative.is_empty()
            && plan.output_relative.len() <= 1024
            && !plan.output_relative.contains('\\')
            && plan
                .output_relative
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && !plan.output_relative.chars().any(char::is_control)
            && relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
        "invalid generated output locator"
    );
    let source = serde_json::to_string(&plan.source)?;
    ensure!(
        source.len() <= 1024 * 1024,
        "publication source proof exceeds bound"
    );
    let old: Option<(String, String, String)> = sqlx::query_as(
        "SELECT operation_id,source_json,output_relative FROM catabolic_generated_publications WHERE job_id=? AND attempt=?",
    ).bind(&plan.job_id).bind(plan.attempt).fetch_optional(&mut *db).await?;
    if let Some((operation, saved_source, saved_relative)) = old {
        ensure!(
            operation == plan.operation_id
                && saved_source == source
                && saved_relative == plan.output_relative,
            "generated publication attempt already names different facts"
        );
        return Ok(());
    }
    sqlx::query("INSERT INTO catabolic_generated_publications(operation_id,job_id,attempt,source_json,output_relative,created_at) VALUES(?,?,?,?,?,unixepoch())")
        .bind(&plan.operation_id).bind(&plan.job_id).bind(plan.attempt).bind(source).bind(&plan.output_relative).execute(db).await?;
    Ok(())
}

/// Only adapter-owned fields can be filled, in SQL-enforced dependency order.
/// This is not a request-controlled SQL column or authority constructor.
pub(crate) enum PublicationRecord {
    Output,
    Command,
    Receipt,
}

pub(crate) async fn retain_in(
    db: &mut SqliteConnection,
    operation: &str,
    field: PublicationRecord,
    value: &Value,
) -> anyhow::Result<()> {
    ensure!(value.is_object(), "publication record must be an object");
    let encoded = serde_json::to_string(value)?;
    ensure!(
        encoded.len() <= 4 * 1024 * 1024,
        "publication record exceeds bound"
    );
    let (read, write) = match field {
        PublicationRecord::Output => (
            "SELECT output_json FROM catabolic_generated_publications WHERE operation_id=?",
            "UPDATE catabolic_generated_publications SET output_json=? WHERE operation_id=? AND output_json IS NULL",
        ),
        PublicationRecord::Command => (
            "SELECT command_json FROM catabolic_generated_publications WHERE operation_id=?",
            "UPDATE catabolic_generated_publications SET command_json=? WHERE operation_id=? AND command_json IS NULL",
        ),
        PublicationRecord::Receipt => (
            "SELECT receipt_json FROM catabolic_generated_publications WHERE operation_id=?",
            "UPDATE catabolic_generated_publications SET receipt_json=? WHERE operation_id=? AND receipt_json IS NULL",
        ),
    };
    let saved: Option<String> = sqlx::query_scalar(read)
        .bind(operation)
        .fetch_optional(&mut *db)
        .await?
        .context("missing generated publication intent")?;
    if let Some(saved) = saved {
        ensure!(saved == encoded, "generated publication record changed");
        return Ok(());
    }
    ensure!(
        sqlx::query(write)
            .bind(encoded)
            .bind(operation)
            .execute(db)
            .await?
            .rows_affected()
            == 1,
        "generated publication record changed during writer custody"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::{Connection, Executor};
    async fn fixture() -> sqlx::SqliteConnection {
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        db.execute(include_str!(
            "../migrations/0087_catabolic_generated_publications.sql"
        ))
        .await
        .unwrap();
        db
    }
    fn plan() -> PublicationPlan {
        PublicationPlan {
            operation_id: "publication:job:1".into(),
            job_id: "job".into(),
            attempt: 1,
            source: json!({"catalog":"native","incarnation":"original","file":"input","revision":"exact"}),
            output_relative: "job/1/output.mp4".into(),
        }
    }
    #[tokio::test]
    async fn publication_records_replay_exactly_and_never_rewrite_proof() {
        let mut db = fixture().await;
        let planned = plan();
        plan_in(&mut db, &planned).await.unwrap();
        plan_in(&mut db, &planned).await.unwrap();
        let mut different = planned.clone();
        different.source["revision"] = json!("replaced");
        assert!(plan_in(&mut db, &different).await.is_err());
        assert!(
            retain_in(
                &mut db,
                &planned.operation_id,
                PublicationRecord::Command,
                &json!({})
            )
            .await
            .is_err()
        );
        retain_in(
            &mut db,
            &planned.operation_id,
            PublicationRecord::Output,
            &json!({"sha256":"exact"}),
        )
        .await
        .unwrap();
        retain_in(
            &mut db,
            &planned.operation_id,
            PublicationRecord::Output,
            &json!({"sha256":"exact"}),
        )
        .await
        .unwrap();
        assert!(
            retain_in(
                &mut db,
                &planned.operation_id,
                PublicationRecord::Output,
                &json!({"sha256":"different"})
            )
            .await
            .is_err()
        );
        retain_in(
            &mut db,
            &planned.operation_id,
            PublicationRecord::Command,
            &json!({"envelope":"original"}),
        )
        .await
        .unwrap();
        retain_in(
            &mut db,
            &planned.operation_id,
            PublicationRecord::Receipt,
            &json!({"outcome":"applied"}),
        )
        .await
        .unwrap();
        assert!(
            retain_in(&mut db, "missing", PublicationRecord::Output, &json!({}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn dropping_observation_waiter_requests_cancel_without_detached_watcher() {
        let signal = motion_catalog_backend::sources::ScanCancellation::default();
        let watcher = tokio::spawn(std::future::pending::<()>());
        let aborted = watcher.abort_handle();
        let guard = ObservationCancellation {
            signal: signal.clone(),
            watcher,
        };
        assert!(!signal.is_cancelled());
        drop(guard);
        assert!(signal.is_cancelled());
        tokio::task::yield_now().await;
        assert!(aborted.is_finished());
    }

    #[tokio::test]
    async fn rejected_locator_and_rolled_back_intent_authorize_no_output() {
        let mut db = fixture().await;
        for path in [
            "",
            "/absolute",
            "../escape",
            "a/../../escape",
            "a\\escape",
            "a\0bad",
            "a//b",
            "a/./b",
            "a/b/",
        ] {
            let mut planned = plan();
            planned.output_relative = path.into();
            assert!(
                plan_in(&mut db, &planned).await.is_err(),
                "accepted {path:?}"
            );
        }
        let mut tx = db.begin().await.unwrap();
        plan_in(&mut tx, &plan()).await.unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_generated_publications")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            0
        );
    }
}

/// Capture exact native owners under the same Motion writer that admits the
/// preparation. This record is durable evidence, not replayable authorization.
/// Dispatch must reauthorize and reacquire all current native/physical proofs.
pub(crate) async fn capture_admission(
    app: &crate::App,
    conn: &mut SqliteConnection,
    job_id: &str,
    timeline_id: &str,
    file_id: &str,
    file_revision: &str,
) -> anyhow::Result<()> {
    use serde_json::json;
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native processing runtime missing")?;
    runtime.validate_projection(conn).await?;
    let state: (String, String, i64) = sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1")
        .fetch_one(&mut *conn).await?;
    // The public preparation contract requires a whole original source. Choose
    // deterministically if several exact original versions name the same bytes;
    // retain that exact owner rather than reselecting one on completion.
    let rows: Vec<(String, i64, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT p.version_id,p.motion_version_revision,p.motion_timeline_revision,p.native_version_revision,p.native_timeline_revision,p.manifest_json \
         FROM catabolic_projection_versions p JOIN media_versions v ON v.id=p.version_id \
         JOIN version_files b ON b.version_id=v.id JOIN catabolic_projection_files f ON f.file_id=b.file_id \
         WHERE p.timeline_id=? AND p.ready=1 AND p.generation=? \
         AND p.native_timeline_revision=p.native_current_timeline_revision \
         AND v.origin='original' AND b.file_id=? AND b.file_revision=? \
         AND b.start_ms IS NULL AND b.end_ms IS NULL AND f.available=1 AND f.generation=p.generation \
         AND (SELECT count(*) FROM version_files x WHERE x.version_id=v.id)=1 ORDER BY p.version_id LIMIT 2"
    ).bind(timeline_id).bind(state.2).bind(file_id).bind(file_revision).fetch_all(&mut *conn).await?;
    let row = rows
        .into_iter()
        .next()
        .context("no exact current native original version for preparation")?;
    let version_id = row.0.clone();
    let generation = state.2;
    let manifest = runtime
        .with_host(move |host| {
            host.projection_version_manifest(generation, &[version_id])
                .map_err(Into::into)
        })
        .await?;
    ensure!(
        manifest == serde_json::from_str::<Value>(&row.5)?,
        "native preparation manifest differs from current projection"
    );
    let mapping: String = sqlx::query_scalar("SELECT mapping_json FROM catabolic_projection_files WHERE file_id=? AND generation=? AND available=1")
        .bind(file_id).bind(state.2).fetch_one(&mut *conn).await?;
    let mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
        serde_json::from_str(&mapping)?;
    ensure!(
        mapping.motion_file["id"].as_str() == Some(file_id)
            && mapping.motion_file["revision"].as_str() == Some(file_revision),
        "native preparation occurrence differs"
    );
    let preparation: (String, String, String) = sqlx::query_as("SELECT p.recipe_digest,p.tracks_json,p.timeline_id FROM preparations p JOIN processing_jobs j ON j.id=p.job_id WHERE p.job_id=? AND j.source_file_id=? AND j.source_revision=? AND j.phase='queued'")
        .bind(job_id).bind(file_id).bind(file_revision).fetch_one(&mut *conn).await?;
    ensure!(
        preparation.2 == timeline_id,
        "preparation timeline differs from captured owner"
    );
    let capture = json!({"contract_version":1,"catalog_id":state.0,"incarnation":state.1,"generation":state.2,
        "job_id":job_id,"timeline_id":timeline_id,"version_id":row.0,
        "motion_version_revision":row.1,"motion_timeline_revision":row.2,
        "native_version_revision":row.3,"native_timeline_revision":row.4,
        "manifest":manifest,"occurrence":mapping,"recipe_digest":preparation.0,
        "tracks":serde_json::from_str::<Value>(&preparation.1)?});
    let encoded = serde_json::to_string(&capture)?;
    ensure!(
        encoded.len() <= 4 * 1024 * 1024,
        "native processing admission exceeds bound"
    );
    sqlx::query("INSERT INTO catabolic_processing_admissions(job_id,capture_json,created_at) VALUES(?,?,unixepoch())")
        .bind(job_id).bind(encoded).execute(conn).await?;
    Ok(())
}

/// Allocate one output identity per actual Motion execution attempt. The worker
/// calls this before any final-root copy, never from request-supplied output IDs.
/// Existing attempt data is replayed rather than regenerated after a crash.
pub(crate) async fn begin_attempt(
    app: &crate::App,
    job: &crate::processing::ProcessingJob,
) -> anyhow::Result<PublicationPlan> {
    use serde_json::json;
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native processing runtime missing")?;
    runtime.rebuild_projection().await?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    runtime.validate_projection(&mut tx).await?;
    let current: (String, i64, String, String) = sqlx::query_as(
        "SELECT phase,attempt,source_file_id,source_revision FROM processing_jobs WHERE id=?",
    )
    .bind(&job.id)
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        current
            == (
                "running".into(),
                job.attempt,
                job.source_file_id.clone(),
                job.source_revision.clone()
            ),
        "processing attempt is no longer current"
    );
    let saved: Option<(String,String,String)> = sqlx::query_as("SELECT operation_id,source_json,output_relative FROM catabolic_generated_publications WHERE job_id=? AND attempt=?")
        .bind(&job.id).bind(job.attempt).fetch_optional(&mut *tx).await?;
    if let Some((operation_id, source, output_relative)) = saved {
        let plan = PublicationPlan {
            operation_id,
            job_id: job.id.clone(),
            attempt: job.attempt,
            source: serde_json::from_str(&source)?,
            output_relative,
        };
        tx.commit().await?;
        return Ok(plan);
    }
    let admission: String = sqlx::query_scalar(
        "SELECT capture_json FROM catabolic_processing_admissions WHERE job_id=?",
    )
    .bind(&job.id)
    .fetch_one(&mut *tx)
    .await?;
    let admission: Value = serde_json::from_str(&admission)?;
    ensure!(
        admission["job_id"].as_str() == Some(job.id.as_str())
            && admission["occurrence"]["motion_file"]["id"].as_str()
                == Some(job.source_file_id.as_str())
            && admission["occurrence"]["motion_file"]["revision"].as_str()
                == Some(job.source_revision.as_str()),
        "processing source differs from immutable admission"
    );
    let identity = json!({"catalog_id":admission["catalog_id"],"incarnation":admission["incarnation"],"job_id":job.id,"attempt":job.attempt});
    let key = catabolic_core::sha256(serde_json::to_vec(&identity)?);
    let plan = PublicationPlan {
        operation_id: format!("motion.generated:{key}"),
        job_id: job.id.clone(),
        attempt: job.attempt,
        source: json!({"contract_version":1,"admission":admission,
            "job":{"id":job.id,"attempt":job.attempt,"source_file_id":job.source_file_id,"source_revision":job.source_revision,"recipe":job.recipe,"backend":job.backend,"video_profile":job.video_profile},
            "ids":{"file":crate::new_id(),"version":crate::new_id(),"association":crate::new_id(),"binding":crate::new_id(),"evidence":crate::new_id(),"relationship":crate::new_id()}}),
        output_relative: format!("{key}.mp4"),
    };
    plan_in(&mut tx, &plan).await?;
    tx.commit().await?;
    Ok(plan)
}

/// Dropping the async caller requests cancellation but never releases the
/// native worker's transaction/I/O custody. The worker still owns cleanup.
struct ObservationCancellation {
    signal: motion_catalog_backend::sources::ScanCancellation,
    watcher: tokio::task::JoinHandle<()>,
}
impl Drop for ObservationCancellation {
    fn drop(&mut self) {
        self.signal.cancel();
        self.watcher.abort();
    }
}

pub(crate) struct PreparedOutput {
    pub plan: PublicationPlan,
    pub proof: crate::native_generated::GeneratedOccurrenceProof,
}

/// Materialize and observe a validated render without publishing any catalog
/// identity. Native observation is owned through its exact cleanup; uncertain
/// final bytes and the planned locator are retained for receipt reconciliation.
pub(crate) async fn prepare_output(
    app: &crate::App,
    job: &crate::processing::ProcessingJob,
    completed: crate::scan::Found,
    stop: tokio_util::sync::CancellationToken,
    permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<PreparedOutput> {
    use std::{path::PathBuf, sync::Arc};
    let plan = begin_attempt(app, job).await?;
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native processing runtime missing")?
        .clone();
    let final_root = app
        .processing
        .root
        .parent()
        .context("processing root parent missing")?
        .join("native-generated");
    tokio::fs::create_dir_all(&final_root).await?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let handle = tokio::runtime::Handle::current();
    let root_path = final_root.clone();
    let (root, tx) = runtime
        .with_host(move |host| {
            let root = handle.block_on(crate::native_generated::register_output_root(
                &mut tx, host, &root_path,
            ))?;
            Ok((root, tx))
        })
        .await?;
    tx.commit().await?;
    let root = Arc::new(root);
    let staging_root = app.processing.root.clone();
    let staged_relative = PathBuf::from(&completed.relative);
    let expected_stamp = completed.fingerprint;
    let expected_sha = completed.revision;
    let expected_size = completed.bytes;
    let relative = PathBuf::from(&plan.output_relative);
    let final_relative = relative.clone();
    let copy_root = root.clone();
    let copy_stop = stop.clone();
    let copy_permit = permit.clone();
    let materialized = tokio::task::spawn_blocking(move || {
        let _io_custody = copy_permit;
        let input = crate::scan::open_file(&staging_root, &staged_relative)?;
        crate::native_generated::materialize_output(
            &copy_root,
            &final_relative,
            &input,
            &expected_stamp,
            &expected_sha,
            &copy_stop,
        )
    })
    .await
    .context("generated materializer worker failed")??;
    ensure!(
        materialized.bytes == u64::try_from(expected_size)?,
        "materialized output length changed"
    );
    let found = crate::scan::inspect_native(
        final_root,
        relative.clone(),
        &app.ffprobe,
        stop.clone(),
        permit.clone(),
    )
    .await?;
    ensure!(
        found.revision == materialized.sha256
            && found.fingerprint == materialized.fingerprint
            && found.bytes == expected_size,
        "final generated probe differs from materialized bytes"
    );
    let mut fence = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let current: (String, i64) =
        sqlx::query_as("SELECT phase,attempt FROM processing_jobs WHERE id=?")
            .bind(&job.id)
            .fetch_one(&mut *fence)
            .await?;
    ensure!(
        current == ("running".into(), job.attempt),
        "processing cancelled before output observation"
    );
    let cancel = motion_catalog_backend::sources::ScanCancellation::default();
    if stop.is_cancelled() {
        cancel.cancel();
    }
    let signal = cancel.clone();
    let watcher = tokio::spawn(async move {
        stop.cancelled().await;
        signal.cancel();
    });
    let cancellation = ObservationCancellation {
        signal: cancel.clone(),
        watcher,
    };
    let result = runtime
        .with_host(move |host| {
            // A dropped async waiter cannot release Motion writer or I/O custody
            // while the native observation helper still owns work.
            let custody = Arc::new(std::sync::Mutex::new(fence));
            let proof = crate::native_generated::observe_output(
                host,
                &root,
                &relative,
                &found,
                &cancel,
                std::time::Duration::from_secs(120),
                (permit, custody.clone()),
            )?;
            crate::native_generated::verify_current(host, &proof)?;
            Ok(proof)
        })
        .await;
    drop(cancellation);
    let proof = result?;
    let output = serde_json::json!({"mapping":proof.source(),"observed":proof.observed(),"probe":proof.probe()});
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    retain_in(
        &mut tx,
        &plan.operation_id,
        PublicationRecord::Output,
        &output,
    )
    .await?;
    tx.commit().await?;
    Ok(PreparedOutput { plan, proof })
}

/// Startup reconciles exact original receipts only. Missing outcomes retain the
/// reserved attempt and bytes; they never enqueue or dispatch another command.
pub(crate) async fn reconcile_receipts(
    runtime: &crate::native_catalog::Runtime,
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<()> {
    let mut after = String::new();
    loop {
        let ids:Vec<String>=sqlx::query_scalar("SELECT operation_id FROM catabolic_generated_publications WHERE command_json IS NOT NULL AND acknowledged=0 AND operation_id>? ORDER BY operation_id LIMIT 128")
            .bind(&after).fetch_all(pool).await?;
        if ids.is_empty() {
            return Ok(());
        }
        for operation in ids {
            after = operation.clone();
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            let intent = journal::load_in(&mut tx, &operation).await?;
            let handle = tokio::runtime::Handle::current();
            runtime
                .with_host(move |host| {
                    let outcome = journal::reconcile_only(host, &intent)?;
                    handle.block_on(async move {
                        match outcome {
                            None => tx.rollback().await?,
                            Some(outcome) => {
                                journal::record_receipt_in(&mut tx, &intent, &outcome).await?;
                                journal::acknowledge_mapping_in(&mut tx, &intent).await?;
                                tx.commit().await?;
                            }
                        }
                        Ok(())
                    })
                })
                .await?;
        }
    }
}

/// Projection already rebuilt from actual native owners. Complete only reserved
/// dispositions with a durable full receipt and exact current projected FK
/// targets. The Motion writer and native owner remain held through the job ACK.
pub(crate) async fn acknowledge_jobs(
    runtime: &crate::native_catalog::Runtime,
    pool: &sqlx::SqlitePool,
) -> anyhow::Result<()> {
    let mut after = String::new();
    loop {
        let ids:Vec<String>=sqlx::query_scalar("SELECT operation_id FROM catabolic_generated_publications WHERE receipt_json IS NOT NULL AND acknowledged=0 AND operation_id>? ORDER BY operation_id LIMIT 128")
            .bind(&after).fetch_all(pool).await?;
        if ids.is_empty() {
            return Ok(());
        }
        for operation in ids {
            after = operation.clone();
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            let intent = journal::load_in(&mut tx, &operation).await?;
            let handle = tokio::runtime::Handle::current();
            runtime
                .with_host(move |host| handle.block_on(acknowledge_job_in(tx, host, intent)))
                .await?;
        }
    }
}
async fn acknowledge_job_in(
    mut tx: sqlx::Transaction<'static, sqlx::Sqlite>,
    host: &catabolic_runtime::host::HostSession,
    intent: journal::SavedIntent,
) -> anyhow::Result<()> {
    use playscale_core::{jobs::Job, processing::State, processing_publication::CommittedReceipt};
    let state:(String,String,i64)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(&mut *tx).await?;
    let identity = host.identity()?;
    ensure!(
        identity["catalog_id"] == state.0
            && identity["catalog_incarnation"] == state.1
            && host.catalog_generation()? == state.2,
        "generated ACK projection changed"
    );
    let (encoded,job_id,attempt):(String,String,i64)=sqlx::query_as("SELECT receipt_json,job_id,attempt FROM catabolic_generated_publications WHERE operation_id=? AND acknowledged=0")
        .bind(intent.command.key().operation_id()).fetch_one(&mut *tx).await?;
    let receipt: Value = serde_json::from_str(&encoded)?;
    let result = &receipt["result"];
    let reservation = intent
        .reservation
        .record_receipt(CommittedReceipt {
            identity: intent.reservation.identity().clone(),
            receipt_digest: catabolic_core::sha256(serde_json::to_vec(&receipt)?),
        })
        .map_err(anyhow::Error::msg)?;
    let current: crate::processing::ProcessingJob =
        sqlx::query_as("SELECT * FROM processing_jobs WHERE id=?")
            .bind(&job_id)
            .fetch_one(&mut *tx)
            .await?;
    ensure!(current.attempt == attempt, "generated ACK attempt changed");
    let request = &intent.command.payload()["request"];
    ensure!(
        result["provenance"] == request["provenance"],
        "generated ACK receipt provenance changed"
    );
    let observed_job = serde_json::json!({"id":current.id,"attempt":current.attempt,
        "source_file_id":current.source_file_id,"source_revision":current.source_revision,
        "recipe":current.recipe,"backend":current.backend,"video_profile":current.video_profile});
    let reserved_job = matching_reserved_job(&request["provenance"]["job"], &observed_job)?;
    let before = State {
        job: Job {
            phase: serde_json::from_value(serde_json::json!(current.phase))?,
            attempt: current.attempt.try_into()?,
        },
        source_revision: current.source_revision.clone(),
        published_output: current.output_file_id.clone(),
    };
    let (_, completed) = reservation
        .acknowledge(&before)
        .map_err(anyhow::Error::msg)?;
    let file = completed
        .published_output
        .as_deref()
        .context("generated ACK output missing")?;
    let version = result["version_id"]
        .as_str()
        .context("generated ACK version missing")?;
    // A receipt can precede later retirement and the first application ACK.
    // Retained native history proves descriptive FK identity, never readiness.
    let owner = request["item"]["id"]
        .as_str()
        .context("generated ACK owner missing")?;
    let historical = host.historical_version(
        state.2,
        version,
        owner,
        result["version_revision"]
            .as_i64()
            .context("generated receipt version revision missing")?,
    )?;
    let retired = motion_catalog_backend::generated_publication::verify_historical(
        &intent.command,
        result,
        &historical,
    )?;
    let projected: (String, String, i64, String, i64) = if retired {
        let row: (String,String,i64,i64,String) = sqlx::query_as(
            "SELECT f.revision,e.item_id,f.available,f.native_history,v.timeline_id FROM media_files f JOIN editions e ON e.id=f.edition_id JOIN timelines t ON t.edition_id=e.id JOIN media_versions v ON v.timeline_id=t.id WHERE f.id=? AND f.generated=1 AND f.library_id=? AND v.id=?")
            .bind(file).bind(motion_catalog_backend::generated_source::SOURCE_ID).bind(version)
            .fetch_one(&mut *tx).await?;
        ensure!(
            row.2 == 0 && row.3 == 1 && result["timeline_id"] == row.4,
            "retired generated ACK lacks exact unavailable history target"
        );
        (row.0, row.1, 0, String::new(), 0)
    } else {
        let row: (String,String,i64,String,i64) = sqlx::query_as(
            "SELECT f.revision,e.item_id,f.available,p.manifest_json,p.ready FROM media_files f JOIN editions e ON e.id=f.edition_id JOIN catabolic_projection_files q ON q.file_id=f.id JOIN version_files b ON b.file_id=f.id AND b.file_revision=f.revision JOIN catabolic_projection_versions p ON p.version_id=b.version_id WHERE f.id=? AND f.generated=1 AND f.library_id=? AND b.version_id=? AND q.generation=? AND p.generation=?")
            .bind(file).bind(motion_catalog_backend::generated_source::SOURCE_ID).bind(version).bind(state.2).bind(state.2).fetch_one(&mut *tx).await?;
        motion_catalog_backend::generated_publication::verify_current_version(
            host,
            state.2,
            &row.1,
            &intent.command,
            result,
            &serde_json::from_str::<Value>(&row.3)?,
        )?;
        row
    };
    ensure!(
        projected.0
            == result["output_sha256"]
                .as_str()
                .context("generated ACK SHA missing")?,
        "generated ACK output revision changed"
    );
    let recipe = serde_json::json!({"version":1,"recipe":reserved_job["recipe"],"backend":reserved_job["backend"],"job_id":reserved_job["id"],"attempt":reserved_job["attempt"],"video_profile":reserved_job["video_profile"],
        "native_publication":intent.command.key().operation_id()});
    let rendition_id = format!("native-generated:{}", intent.command.key().operation_id());
    sqlx::query("INSERT INTO renditions(id,item_id,source,external_id,file_id,file_revision,source_file_id,source_revision,label,recipe_json,updated_at) VALUES(?,?,'playscale',?,?,?,?,?,?,?,?) ON CONFLICT(source,external_id) DO NOTHING")
        .bind(&rendition_id).bind(&projected.1).bind(&job_id).bind(file).bind(&projected.0).bind(&current.source_file_id).bind(&current.source_revision).bind(&current.recipe).bind(recipe.to_string()).bind(crate::now()).execute(&mut *tx).await?;
    let saved:(String,String,String,String,String,String,String)=sqlx::query_as("SELECT id,item_id,file_id,file_revision,source_file_id,source_revision,recipe_json FROM renditions WHERE source='playscale' AND external_id=?")
        .bind(&job_id).fetch_one(&mut *tx).await?;
    ensure!(
        saved.0 == rendition_id
            && saved.1 == projected.1
            && saved.2 == file
            && saved.3 == projected.0
            && saved.4 == current.source_file_id
            && saved.5 == current.source_revision
            && serde_json::from_str::<Value>(&saved.6)? == recipe,
        "generated application rendition acknowledgment changed"
    );
    sqlx::query("UPDATE processing_jobs SET phase='completed',output_file_id=?,error=NULL,expired=?,updated_at=? WHERE id=? AND attempt=?")
        .bind(file).bind(i64::from(projected.2!=1 || projected.4!=1)).bind(crate::now()).bind(&job_id).bind(attempt).execute(&mut *tx).await?;
    sqlx::query("UPDATE catabolic_generated_publications SET acknowledged=1 WHERE operation_id=?")
        .bind(intent.command.key().operation_id())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Job identity/options are fixed by the committed transform, not by whichever
/// mutable application row happens to exist when its delayed ACK runs.
fn matching_reserved_job<'a>(reserved: &'a Value, observed: &Value) -> anyhow::Result<&'a Value> {
    ensure!(
        reserved.is_object() && reserved == observed,
        "generated ACK source or transform changed"
    );
    Ok(reserved)
}

#[cfg(test)]
mod ack_provenance_tests {
    use super::*;
    #[test]
    fn delayed_ack_rejects_same_revision_source_substitution_and_transform_drift() {
        let saved = serde_json::json!({"id":"job","attempt":1,"source_file_id":"original","source_revision":"same-sha",
            "recipe":"remux_mp4","backend":"software","video_profile":null});
        assert_eq!(matching_reserved_job(&saved, &saved).unwrap(), &saved);
        for field in [
            "source_file_id",
            "recipe",
            "backend",
            "video_profile",
            "id",
            "attempt",
        ] {
            let mut changed = saved.clone();
            changed[field] = serde_json::json!("other");
            assert!(
                matching_reserved_job(&saved, &changed).is_err(),
                "accepted changed {field}"
            );
            assert_eq!(changed["source_revision"], saved["source_revision"]);
        }
    }
}
