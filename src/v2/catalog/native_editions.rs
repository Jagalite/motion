//! Empty edition mutation adapter. Public route admission is staged separately.
//! Immutable allocation intent precedes dispatch; receipt baselines and HTTP ACK
//! commit under the actual Motion writer/native owner, even on caller abort.
use super::*;
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use catabolic_store::{
    catalog_commands::{CommandOutcome, ReceiptPolicy},
    catalog_edition_creation::{EmptyEditionAuthority, EmptyEditionRequest},
};
use std::time::Duration;

fn wire(request: &EmptyEditionRequest) -> Result<Value, Problem> {
    serde_json::to_value(edition_body(
        request.edition_id.clone(),
        request.work_id.clone(),
        request.label.clone(),
        1,
    ))
    .map_err(Problem::internal)
}
fn error(error: anyhow::Error) -> Problem {
    if let Some(native) = error.downcast_ref::<catabolic_runtime::host::HostError>() {
        match native.code {
            catabolic_runtime::host::ErrorCode::Conflict => {
                return Problem::new(
                    StatusCode::CONFLICT,
                    "edition_conflict",
                    "The edition request conflicts with current native owners",
                );
            }
            catabolic_runtime::host::ErrorCode::InvalidInput => {
                return Problem::invalid("invalid_edition", "The edition request is invalid");
            }
            _ => {}
        }
    }
    native_read_error(error)
}
pub(super) async fn create(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<EditionInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    if !valid_id(&id) {
        return Err(Problem::not_found());
    }
    if body.value.kind != EditionKind::Default {
        return Err(Problem::invalid(
            "unsupported_edition_kind",
            "This catalog records untyped editions only; send kind default",
        ));
    }
    let label = text("label", &body.value.label, 200)?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _guard = app.jobs.lock().await;
    loop {
        let (tx, principal, selection) =
            native_relationship_writes::authorized_writer(&app, &caller).await?;
        let mut tx = native_relationship_writes::endpoints(
            runtime,
            tx,
            &selection,
            &access::catalog_scope(&principal),
            &id,
            &id,
        )
        .await?;
        let record = Scope {
            principal: &principal.id,
            operation: "createEdition",
            target: &id,
            key: &key,
        };
        let stored = record.load(&mut tx).await?;
        if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
            == Idempotent::Replay
        {
            tx.commit().await?;
            return Ok(replayed(stored.unwrap()));
        }
        let operation = format!(
            "server-create-edition:{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&principal.id, &id, &key)).map_err(Problem::internal)?
            )
        );
        let allocation = format!("{:x}", Sha256::digest(operation.as_bytes()));
        let request = EmptyEditionRequest {
            contract_version: 1,
            work_id: id.clone(),
            edition_id: format!("edition-{allocation}"),
            timeline_id: format!("edition-{allocation}"),
            relationship_id: format!("edition-edge-{allocation}"),
            context_id: format!("edition-context-{allocation}"),
            label: label.clone(),
            request_digest: body.digest.clone(),
        };
        let payload = json!({"operation":"empty_edition_create","request":request});
        let saved: Option<String> =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(&mut *tx)
                .await?;
        let original = if let Some(saved) = saved {
            let command = motion_catalog_backend::command_attempts::decode(&saved)
                .map_err(native_read_error)?;
            if command.payload() != &payload {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "idempotency_conflict",
                    "The key is bound to another edition request",
                ));
            }
            command
        } else {
            let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM editions WHERE id=?1) OR EXISTS(SELECT 1 FROM timelines WHERE id=?1)").bind(&request.edition_id).fetch_one(&mut *tx).await?;
            if conflict {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "edition_id_conflict",
                    "The allocated edition ID is already retained",
                ));
            }
            let op = operation.clone();
            let payload = payload.clone();
            let allocation = request.clone();
            let (command, returned) = runtime
                .with_host(move |host| {
                    if !host.empty_edition_allocation_available(&allocation)? {
                        return Ok((None, tx));
                    }
                    let identity = host.identity()?;
                    let command = CommandEnvelope::new(
                        1,
                        CommandKey::new(
                            identity["catalog_id"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("catalog missing"))?,
                            host.authorization()?.namespace(),
                            op,
                        )?,
                        identity["catalog_incarnation"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("incarnation missing"))?,
                        host.catalog_generation()?,
                        &payload.to_string(),
                    )?;
                    Ok((Some(command), tx))
                })
                .await
                .map_err(error)?;
            tx = returned;
            let command = command.ok_or_else(|| {
                Problem::new(
                    StatusCode::CONFLICT,
                    "edition_id_conflict",
                    "The allocated native edition identity is already retained",
                )
            })?;
            sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?)").bind(&operation).bind(command.canonical_payload()).bind(now()).execute(&mut *tx).await?;
            tx.commit().await?;
            continue;
        };
        let (_, attempt) = motion_catalog_backend::command_attempts::latest_in(&mut tx, &original)
            .await
            .map_err(native_read_error)?;
        let saved_attempt = attempt.clone();
        let handle = tokio::runtime::Handle::current();
        let (replay, returned) = runtime
            .with_host(move |host| {
                let outcome = host.reconcile_command_receipt(&saved_attempt)?;
                if let Some(outcome) = &outcome {
                    handle.block_on(
                        motion_catalog_backend::revision_bridge::acknowledge_empty_edition_in(
                            &mut tx,
                            &saved_attempt,
                            outcome,
                        ),
                    )?;
                }
                Ok((outcome, tx))
            })
            .await
            .map_err(error)?;
        tx = returned;
        if replay.is_some() {
            caller
                .reauthorize(&mut tx, Some(Permission::CatalogWrite))
                .await?;
            let value = wire(&request)?;
            record
                .save(
                    &mut tx,
                    &body.digest,
                    StatusCode::CREATED,
                    Some(&value),
                    None,
                )
                .await?;
            tx.commit().await?;
            runtime
                .rebuild_projection()
                .await
                .map_err(native_read_error)?;
            return Ok(tagged(StatusCode::CREATED, &value, 1));
        }
        let profile = runtime.profile().to_owned();
        let principal_id = principal.id.clone();
        let target = id.clone();
        let key = key.clone();
        let digest = body.digest.clone();
        let caller = caller.clone();
        let handle = tokio::runtime::Handle::current();
        let result = runtime
            .guard_work_and_commit(tx, id.clone(), expected, move |host, tx, _issued| {
                let current = handle
                    .block_on(caller.reauthorize(&mut **tx, Some(Permission::CatalogWrite)))
                    .map_err(|p| anyhow::anyhow!(p.detail))?;
                anyhow::ensure!(
                    handle.block_on(
                        motion_catalog_backend::aggregate_revision::may_curate_work_in(
                            tx,
                            host,
                            &target,
                            &access::catalog_scope(&current)
                        )
                    )?,
                    "edition work is outside current complete write scope"
                );
                let native =
                    host.projection_named_items(host.catalog_generation()?, &[target.clone()])?;
                let head = native["items"]
                    .as_array()
                    .filter(|v| {
                        v.len() == 1 && v[0]["item_id"] == target && v[0]["retirement"].is_null()
                    })
                    .and_then(|v| v[0]["item_revision"].as_i64())
                    .ok_or_else(|| anyhow::anyhow!("edition work head missing"))?;
                let authority = EmptyEditionAuthority {
                    host_namespace: host.authorization()?.namespace().into(),
                    profile: profile.clone(),
                    work_id: target.clone(),
                    expected_work_revision: head,
                    edition_id: request.edition_id.clone(),
                    timeline_id: request.timeline_id.clone(),
                    relationship_id: request.relationship_id.clone(),
                    context_id: request.context_id.clone(),
                    context_key: format!("motion.edition:{}", request.edition_id),
                };
                let (outcome, witness) = host.execute_empty_edition_observed(
                    &attempt,
                    &authority,
                    ReceiptPolicy {
                        retention: Duration::from_secs(30 * 24 * 60 * 60),
                        max_receipts: 100000,
                        max_logical_bytes: 256 * 1024 * 1024,
                        max_hold_records: 100000,
                    },
                )?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        tx, &original, &attempt, witness,
                    ))?;
                    return Ok(None);
                }
                anyhow::ensure!(
                    matches!(
                        outcome,
                        CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. }
                    ),
                    "edition outcome requires reconciliation: {}",
                    outcome.code()
                );
                handle.block_on(
                    motion_catalog_backend::revision_bridge::acknowledge_empty_edition_in(
                        tx, &attempt, &outcome,
                    ),
                )?;
                let value = wire(&request).map_err(|p| anyhow::anyhow!(p.detail))?;
                let record = Scope {
                    principal: &principal_id,
                    operation: "createEdition",
                    target: &target,
                    key: &key,
                };
                handle
                    .block_on(record.save(tx, &digest, StatusCode::CREATED, Some(&value), None))
                    .map_err(|p| anyhow::anyhow!(p.detail))?;
                Ok(Some(value))
            })
            .await
            .map_err(error)?;
        match result {
            crate::native_catalog::Guarded::Mismatch(_) => {
                return Err(Problem::new(
                    StatusCode::PRECONDITION_FAILED,
                    "precondition_failed",
                    "The source work revision has changed",
                ));
            }
            crate::native_catalog::Guarded::Applied { value: None, .. } => continue,
            crate::native_catalog::Guarded::Applied {
                value: Some(value), ..
            } => {
                runtime
                    .rebuild_projection()
                    .await
                    .map_err(native_read_error)?;
                return Ok(tagged(StatusCode::CREATED, &value, 1));
            }
        }
    }
}
