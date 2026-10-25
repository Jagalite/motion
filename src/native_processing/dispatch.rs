//! Live publication and independently reauthorized exact saved-intent retry.
//! Receipt absence alone never authorizes an effect or a replacement command.
use super::{journal, request};
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use catabolic_store::{
    catalog_generated_output::GeneratedOutputAuthority,
    catalog_identity_mutation::IdentityAuthority,
    catalog_import_typed::RelationshipImportAuthority,
};
use playscale_core::processing::{Completion, State};
use std::sync::Arc;

/// Rendering remains Motion-owned. Only a validated completed file can enter
/// this live path; durable reservation precedes the native command effect.
pub(crate) async fn publish_render(
    app: &crate::App,
    job: &crate::processing::ProcessingJob,
    completed: crate::scan::Found,
    stop: tokio_util::sync::CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<()> {
    let prepared = reserve_render(app, job, completed, stop.clone(), permit.clone()).await?;
    dispatch_prepared(app, job, prepared, stop, Phase::ExecuteReserved, permit).await
}

/// Actual production phase boundary: durable reservation with independently
/// validated bytes and authorization, but no generated-output command effect.
/// Dropping its result preserves the exact intent for independently authorized
/// recovery and never grants permission to replace or rerender it.
pub(crate) async fn reserve_render(
    app: &crate::App,
    job: &crate::processing::ProcessingJob,
    completed: crate::scan::Found,
    stop: tokio_util::sync::CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<Arc<super::PreparedOutput>> {
    ensure!(
        !stop.is_cancelled(),
        "render publication cancelled before preparation"
    );
    let prepared =
        Arc::new(super::prepare_output(app, job, completed, stop.clone(), permit.clone()).await?);
    dispatch_prepared(app, job, prepared.clone(), stop, Phase::Reserve, permit).await?;
    Ok(prepared)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Reserve,
    ExecuteReserved,
}

pub(super) async fn dispatch_prepared(
    app: &crate::App,
    job: &crate::processing::ProcessingJob,
    prepared: Arc<super::PreparedOutput>,
    stop: tokio_util::sync::CancellationToken,
    phase: Phase,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<()> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native runtime missing")?
        .clone();
    // Both live execution and recovery must find the immutable saved intent
    // and reacquire current authority before native receipt-first dispatch.
    loop {
        runtime.rebuild_projection().await?;
        let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
        runtime.validate_projection(&mut tx).await?;
        let current: crate::processing::ProcessingJob =
            sqlx::query_as("SELECT * FROM processing_jobs WHERE id=?")
                .bind(&job.id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            current.phase == "running" && current.attempt == job.attempt,
            "render attempt is no longer running"
        );
        let (_, facts) = crate::v2::processing::publication_facts(&mut tx, &current)
            .await?
            .context("native render requires a captured preparation")?;
        ensure!(
            facts.requester_authorized
                && facts.source_enabled
                && facts.source_in_scope
                && facts.exact_whole_timeline_binding
                && facts.recipe_unchanged,
            "render publication authorization or source changed"
        );
        super::matching_reserved_job(
            &prepared.plan.source["job"],
            &serde_json::json!({"id":current.id,"attempt":current.attempt,"source_file_id":current.source_file_id,"source_revision":current.source_revision,"recipe":current.recipe,"backend":current.backend,"video_profile":current.video_profile}),
        )?;
        let saved: bool = sqlx::query_scalar("SELECT command_json IS NOT NULL FROM catabolic_generated_publications WHERE operation_id=?")
            .bind(&prepared.plan.operation_id).fetch_one(&mut *tx).await?;
        ensure!(
            saved == (phase == Phase::ExecuteReserved),
            "publication phase differs from durable saved intent"
        );
        if !saved {
            ensure!(
                !stop.is_cancelled(),
                "render publication cancelled before reservation"
            );
        }
        let handle = tokio::runtime::Handle::current();
        let prepared_copy = prepared.clone();
        let prepare_permit = permit.clone();
        let (intent, authority, mut tx) = runtime.with_host(move |host| {
            let _io_custody = prepare_permit;
            let generation = host.catalog_generation()?;
            let version = prepared_copy.plan.source["admission"]["version_id"].as_str()
                .context("captured native input version missing")?;
            let (input, mut typed) = host.capture_generated_input(generation, version)?;
            let file_id = prepared_copy.plan.source["ids"]["file"].as_str().context("output id missing")?;
            let output = prepared_copy.proof.admission_facts(host, file_id)?;
            let payload = request::assemble(&prepared_copy.plan, &input, &output, prepared_copy.proof.probe())?;
            let namespace = host.authorization()?.namespace().to_owned();
            typed.identity = Some(IdentityAuthority {
                authority_namespace: input.context.authority_namespace.clone(),
                context_keys: [input.context.context_key.clone()].into_iter().collect(),
                allow_declared_evidence: true,
                allow_verified_evidence: false,
            });
            typed.physical_files.insert(input.profile.clone(), [output.output.exact.file_id.clone()].into_iter().collect());
            typed.relationships = Some(RelationshipImportAuthority {
                host_namespace: namespace.clone(),
                edges: payload.relationships.iter().map(|e| (e.source_item_id.clone(), e.target_item_id.clone(), e.kind.clone())).collect(),
            });
            let authority = GeneratedOutputAuthority { host_namespace: namespace.clone(), request: payload.clone(), output_file: output.output_file, typed };
            let intent = handle.block_on(async {
                if saved {
                    let intent = journal::load_in(&mut tx, &prepared_copy.plan.operation_id).await?;
                    ensure!(intent.command.payload()["request"] == serde_json::to_value(&payload)?, "reserved render proof changed");
                    intent.reservation.authorize_exact_retry(
                        intent.reservation.identity(),
                        &State { job: playscale_core::jobs::Job { phase: serde_json::from_value(serde_json::json!(current.phase))?, attempt: current.attempt.try_into()? }, source_revision: current.source_revision.clone(), published_output: current.output_file_id.clone() },
                        &Completion { attempt: current.attempt.try_into()?, current_source_revision: Some(current.source_revision.clone()), validated_output: Some(file_id.to_owned()) },
                        &facts,
                    ).map_err(anyhow::Error::msg)?;
                    Ok(intent)
                } else {
                    let command = CommandEnvelope::new(1, CommandKey::new(&input.catalog_id, namespace, &prepared_copy.plan.operation_id)?,
                        &input.incarnation, generation, &serde_json::json!({"operation":"generated_output_admit","request":payload}).to_string())?;
                    journal::reserve_command_in(&mut tx, &prepared_copy.plan, command,
                        State { job: playscale_core::jobs::Job { phase: serde_json::from_value(serde_json::json!(current.phase))?, attempt: current.attempt.try_into()? }, source_revision: current.source_revision.clone(), published_output: current.output_file_id.clone() },
                        Completion { attempt: current.attempt.try_into()?, current_source_revision: Some(current.source_revision.clone()), validated_output: Some(file_id.to_owned()) }, facts).await
                }
            })?;
            Ok((intent, authority, tx))
        }).await?;
        if !saved {
            // No native effect until this durable disposition is committed.
            tx.commit().await?;
            // The caller releases this phase and then reauthorizes the exact
            // saved command. No native publication effect occurred here.
            return Ok(());
        }
        let handle = tokio::runtime::Handle::current();
        let dispatch_permit = permit.clone();
        let retry = runtime
            .with_host(move |host| {
                let _io_custody = dispatch_permit;
                // The queued blocking worker owns both writer and output descriptor.
                // Dropping its async receiver cannot release either early.
                let (outcome, witness) =
                    host.execute_generated_output_observed(&intent.attempt, &authority)?;
                handle.block_on(async move {
                    if let Some(witness) = witness {
                        journal::record_next_in(&mut tx, &intent, witness).await?;
                        tx.commit().await?;
                        return Ok(true);
                    }
                    journal::record_receipt_in(&mut tx, &intent, &outcome).await?;
                    journal::acknowledge_mapping_in(&mut tx, &intent).await?;
                    tx.commit().await?;
                    Ok(false)
                })
            })
            .await?;
        if retry {
            continue;
        }
        runtime.rebuild_projection().await?;
        super::acknowledge_jobs(&runtime, &app.db).await?;
        return Ok(());
    }
}
