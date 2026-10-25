//! Recovery may retry an exact saved command only after reacquiring independent
//! current authority and actual output custody. It never rerenders or replaces
//! an uncertain business operation based on receipt absence.
use super::{PreparedOutput, PublicationPlan};
use anyhow::{Context, ensure};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) async fn recover_reserved(
    app: &crate::App,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    recover_reserved_for_service(app, stop, None).await
}
pub(crate) async fn recover_reserved_for_service(
    app: &crate::App,
    stop: CancellationToken,
    intent: Option<&crate::native_service::ServiceStopIntent>,
) -> anyhow::Result<()> {
    #[cfg(test)]
    return recover_reserved_inner(app, stop, intent, None).await;
    #[cfg(not(test))]
    return recover_reserved_inner(app, stop, intent).await;
}

async fn recover_reserved_inner(
    app: &crate::App,
    stop: CancellationToken,
    intent: Option<&crate::native_service::ServiceStopIntent>,
    #[cfg(test)] mut refused: Option<tokio::sync::oneshot::Sender<()>>,
) -> anyhow::Result<()> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native runtime missing")?;
    super::reconcile_receipts(runtime, &app.db)
        .await
        .context("startup publication receipt reconciliation")?;
    // Only pre-effect admission is retryable. A false result has dropped both
    // possible reservations without rebuilding or queuing native work. Never
    // retry receipt reconciliation, projection effects or a commit error.
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        match runtime.try_rebuild_projection().await {
            Ok(true) => break,
            Ok(false) => {}
            Err(error)
                if error
                    .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
                    .is_some()
                    && intent.is_some_and(|proof| proof.requested_for_native(runtime)) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(
                    error.context("startup publication recovery projection admission/rebuild")
                );
            }
        }
        #[cfg(test)]
        if let Some(refused) = refused.take() {
            let _ = refused.send(());
        }
        tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
    if stop.is_cancelled() {
        return Ok(());
    }
    // Fresh receipt queries and exact current host/projection generation checks
    // remain inside acknowledge_jobs. No pre-wait empty result is reused here.
    super::acknowledge_jobs(runtime, &app.db)
        .await
        .context("startup publication job acknowledgement")?;
    let mut after = String::new();
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        let operations: Vec<String> = sqlx::query_scalar("SELECT p.operation_id FROM catabolic_generated_publications p JOIN processing_jobs j ON j.id=p.job_id AND j.attempt=p.attempt WHERE p.command_json IS NOT NULL AND p.receipt_json IS NULL AND p.acknowledged=0 AND j.phase='running' AND p.operation_id>? ORDER BY p.operation_id LIMIT 32")
            .bind(&after).fetch_all(&app.db).await?;
        if operations.is_empty() {
            return Ok(());
        }
        for operation in operations {
            after = operation.clone();
            if stop.is_cancelled() {
                return Ok(());
            }
            if let Err(error) = recover_one(app, &operation, stop.clone()).await {
                tracing::warn!(operation_id=%operation, error=%format!("{error:#}"), "native publication remains reserved; exact recovery authority unavailable");
            }
        }
    }
}

async fn recover_one(
    app: &crate::App,
    operation: &str,
    stop: CancellationToken,
) -> anyhow::Result<()> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native runtime missing")?
        .clone();
    let permit = tokio::select! {
        _ = stop.cancelled() => anyhow::bail!("publication recovery cancelled"),
        permit = app.processing.io.clone().acquire_owned() => Arc::new(permit?),
    };
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let row: (String, i64, String, String) = sqlx::query_as("SELECT job_id,attempt,source_json,output_relative FROM catabolic_generated_publications WHERE operation_id=? AND command_json IS NOT NULL AND receipt_json IS NULL AND acknowledged=0")
        .bind(operation).fetch_one(&mut *tx).await?;
    let plan = PublicationPlan {
        operation_id: operation.into(),
        job_id: row.0,
        attempt: row.1,
        source: serde_json::from_str(&row.2)?,
        output_relative: row.3,
    };
    let job: crate::processing::ProcessingJob =
        sqlx::query_as("SELECT * FROM processing_jobs WHERE id=?")
            .bind(&plan.job_id)
            .fetch_one(&mut *tx)
            .await?;
    ensure!(
        job.phase == "running" && job.attempt == plan.attempt,
        "reserved attempt changed"
    );
    let saved = crate::native_generated::load_saved_output_in(&mut tx, operation).await?;
    let mapping = crate::native_generated::mapping_in(&mut tx)
        .await?
        .context("private generated source missing")?;
    let root = app
        .processing
        .root
        .parent()
        .context("processing root parent missing")?
        .join("native-generated");
    let custody = permit.clone();
    let validation_stop = stop.clone();
    let proof = runtime
        .with_host(move |host| {
            let root = crate::native_generated::GeneratedRoot::from_retained_verified_mapping(
                host, &root, mapping,
            )?;
            crate::native_generated::revalidate_saved_output(
                host,
                &root,
                saved,
                &validation_stop,
                (custody, tx),
            )
        })
        .await?;
    ensure!(
        !stop.is_cancelled(),
        "publication recovery cancelled before dispatch admission"
    );
    // `permit` remains owned through the actual detached dispatch completion;
    // the dispatch closure separately owns its Motion writer and output FD.
    let result = super::dispatch::dispatch_prepared(
        app,
        &job,
        Arc::new(PreparedOutput { plan, proof }),
        stop,
        super::dispatch::Phase::ExecuteReserved,
        permit.clone(),
    )
    .await;
    drop(permit);
    result
}

#[cfg(test)]
use crate as playscale_app;
#[cfg(test)]
#[path = "../../tests/support/native_application_fixture.rs"]
mod recovery_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use recovery_fixture::Fixture;

    #[tokio::test]
    async fn startup_projection_busy_releases_then_recovers_current_state() {
        let mut f = Fixture::new().await;
        f.cutover().await;
        let held = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (signal, observed) = tokio::sync::oneshot::channel();
        let app = f.app.clone();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let task = tokio::spawn(async move {
            recover_reserved_inner(&app, task_stop, None, Some(signal)).await
        });
        let saw_busy = tokio::time::timeout(std::time::Duration::from_secs(5), observed).await;
        // Always relinquish the real writer before reporting a diagnostic.
        held.rollback().await.unwrap();
        if !matches!(&saw_busy, Ok(Ok(()))) {
            stop.cancel();
        }
        let settled = task.await.unwrap();
        saw_busy
            .expect("recovery queued behind held Motion writer")
            .unwrap();
        settled.unwrap();
        let mut writer = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        f.app
            .native_catalog
            .as_ref()
            .unwrap()
            .validate_projection(&mut writer)
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM catabolic_generated_publications")
                .fetch_one(&mut *writer)
                .await
                .unwrap();
        assert_eq!(count, 0);
        writer.rollback().await.unwrap();
        f.close().await;
    }

    #[tokio::test]
    async fn startup_projection_busy_cancel_settles_without_queued_owner() {
        let mut f = Fixture::new().await;
        f.cutover().await;
        let held = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (signal, observed) = tokio::sync::oneshot::channel();
        let app = f.app.clone();
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let mut task = tokio::spawn(async move {
            recover_reserved_inner(&app, task_stop, None, Some(signal)).await
        });
        let saw_busy = tokio::time::timeout(std::time::Duration::from_secs(5), observed).await;
        stop.cancel();
        let settled = tokio::time::timeout(std::time::Duration::from_secs(1), &mut task).await;
        held.rollback().await.unwrap();
        if settled.is_err() {
            let _ = task.await;
        }
        saw_busy
            .expect("recovery did not observe actual held writer")
            .unwrap();
        settled
            .expect("cancelled recovery retained a queued owner")
            .unwrap()
            .unwrap();
        let writer = f.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        writer.rollback().await.unwrap();
        f.app
            .native_catalog
            .as_ref()
            .unwrap()
            .identity()
            .await
            .unwrap();
        f.close().await;
    }
}
