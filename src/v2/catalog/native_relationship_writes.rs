//! Native edge writes. Endpoint visibility is independent from native graph
//! authority; aggregate work preconditions never replace exact edge CAS.
use super::*;
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey},
    catalog_relationship::{RelationshipEdge, RelationshipKindRule},
};
use catabolic_store::{
    catalog_commands::{CommandOutcome, ReceiptPolicy},
    catalog_relationships::RelationshipAuthority,
    catalog_scope::TrustedSelection,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

pub(super) async fn authorized_writer(
    app: &App,
    caller: &Caller,
) -> Result<
    (
        sqlx::Transaction<'static, sqlx::Sqlite>,
        playscale_core::access::Principal,
        TrustedSelection,
    ),
    Problem,
> {
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let mut tx = app.db.begin().await?;
    let initial = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&initial);
    if scope == CatalogScope::Nothing {
        return Err(outside_scope());
    }
    tx.commit().await?;
    let scoped = match &scope {
        CatalogScope::Libraries(ids) => Some(
            runtime
                .prepare_library_read_scope(ids.clone())
                .await
                .map_err(native_read_error)?,
        ),
        _ => None,
    };
    let selection = match &scoped {
        Some(scope) => scope.selection.clone(),
        None => runtime
            .prepare_all_scope()
            .await
            .map_err(native_read_error)?,
    };
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    if access::catalog_scope(&principal) != scope {
        return Err(outside_scope());
    }
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(native_read_error)?;
    if let Some(scope) = &scoped {
        runtime
            .validate_library_read_scope(&mut tx, scope)
            .await
            .map_err(native_read_error)?;
    }
    Ok((tx, principal, selection))
}
pub(super) async fn endpoints(
    runtime: &crate::native_catalog::Runtime,
    mut tx: sqlx::Transaction<'static, sqlx::Sqlite>,
    selection: &TrustedSelection,
    scope: &CatalogScope,
    source: &str,
    target: &str,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, Problem> {
    for id in [source, target] {
        if !valid_id(id)
            || runtime
                .read_projectable_items(&mut tx, vec![selection.clone()], Some(id.into()))
                .await
                .map_err(native_read_error)?
                .is_empty()
        {
            return Err(Problem::not_found());
        }
    }
    let subjects = [source.to_owned(), target.to_owned()];
    let scope = scope.clone();
    let handle = tokio::runtime::Handle::current();
    let (tx, writable) = runtime
        .with_host(move |host| {
            for subject in subjects {
                if !handle.block_on(
                    motion_catalog_backend::aggregate_revision::may_curate_work_in(
                        &mut tx, host, &subject, &scope,
                    ),
                )? {
                    return Ok((tx, false));
                }
            }
            Ok((tx, true))
        })
        .await
        .map_err(native_read_error)?;
    if !writable {
        return Err(outside_scope());
    }
    Ok(tx)
}
fn rule(kind: &str) -> Option<RelationshipKindRule> {
    match kind {
        "custom:motion_part_of"
        | "custom:motion_edition_of"
        | "custom:motion_extra_of"
        | "custom:motion_derived_from" => Some(RelationshipKindRule::Acyclic),
        "custom:motion_created_by" | "custom:motion_performed_by" => {
            Some(RelationshipKindRule::Directed)
        }
        _ => None,
    }
}
fn wire(result: &Value) -> anyhow::Result<Value> {
    let row = &result["relationship"];
    let native = json!({"id":row["id"],"source_id":row["source"],"target_id":row["target"],"kind":row["kind"],"position":row["position"],"metadata":row["metadata"].to_string(),"active":1});
    let mut body = catabolic_store::catalog_work_relationship::decode_record(&native)?
        .ok_or_else(|| anyhow::anyhow!("native relationship result domain changed"))?;
    body.as_object_mut().unwrap().remove("active");
    body["revision"] = json!(
        row["revision"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| anyhow::anyhow!("native edge revision missing"))?
            .to_string()
    );
    Ok(body)
}
fn write_error(error: anyhow::Error) -> Problem {
    if let Some(native) = error.downcast_ref::<catabolic_runtime::host::HostError>() {
        match native.code {
            catabolic_runtime::host::ErrorCode::Conflict => {
                return Problem::new(
                    StatusCode::CONFLICT,
                    "relationship_conflict",
                    "The relationship conflicts with the current native graph",
                );
            }
            catabolic_runtime::host::ErrorCode::InvalidInput => {
                return Problem::invalid(
                    "invalid_relationship",
                    "The relationship request is invalid",
                );
            }
            _ => {}
        }
    }
    native_read_error(error)
}
fn full_result(outcome: CommandOutcome) -> anyhow::Result<Value> {
    match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => {
            Ok(result)
        }
        other => anyhow::bail!(
            "native edge command requires reconciliation: {}",
            other.code()
        ),
    }
}
fn dispatch(
    host: &mut catabolic_runtime::host::HostSession,
    command: &CommandEnvelope,
    authority: &RelationshipAuthority,
) -> anyhow::Result<(
    CommandOutcome,
    Option<catabolic_runtime::host::StaleGenerationWitness>,
)> {
    let namespace = host.authorization()?.namespace().to_owned();
    anyhow::ensure!(
        authority.host_namespace == namespace,
        "relationship namespace changed"
    );
    Ok(host.execute_relationship_command_observed(
        command,
        authority,
        ReceiptPolicy {
            retention: Duration::from_secs(30 * 24 * 60 * 60),
            max_receipts: 100_000,
            max_logical_bytes: 256 * 1024 * 1024,
            max_hold_records: 100_000,
        },
    )?)
}

pub(super) async fn create(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<RelationshipInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    if input.source_item_id != id {
        return Err(Problem::invalid(
            "source_mismatch",
            "Path must name source_item_id",
        ));
    }
    let kind = serde_json::to_value(input.kind)
        .map_err(Problem::internal)?
        .as_str()
        .ok_or_else(native_unavailable)?
        .to_owned();
    let native_kind = format!("custom:motion_{kind}");
    let edge = RelationshipEdge {
        source: id.clone(),
        target: input.target_item_id.clone(),
        kind: native_kind.clone(),
    };
    edge.validate()
        .map_err(|e| Problem::invalid("invalid_relationship", e.to_string()))?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _guard = app.jobs.lock().await;
    loop {
        let (mut tx, principal, selection) = authorized_writer(&app, &caller).await?;
        tx = endpoints(
            runtime,
            tx,
            &selection,
            &access::catalog_scope(&principal),
            &id,
            &input.target_item_id,
        )
        .await?;
        let record = Scope {
            principal: &principal.id,
            operation: "createRelationship",
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
            "server-create-edge:{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&principal.id, &id, &key)).map_err(Problem::internal)?
            )
        );
        let edge_id = format!(
            "edge-{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&operation, &body.digest)).map_err(Problem::internal)?
            )
        );
        let payload = json!({"operation":"relationships","mutation":{"action":"create","id":edge_id,"source":id,"target":input.target_item_id,"kind":native_kind,"metadata":{"motion_relationship":{"contract_version":1,"id":edge_id,"kind":kind,"position":input.position}}}});
        let saved: Option<String> =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(&mut *tx)
                .await?;
        let command = if let Some(saved) = saved {
            let command = motion_catalog_backend::command_attempts::decode(&saved)
                .map_err(native_read_error)?;
            if command.payload() != &payload {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "idempotency_conflict",
                    "The key is bound to another relationship request",
                ));
            }
            command
        } else {
            let op = operation.clone();
            let payload = payload.clone();
            let (command, returned) = runtime
                .with_host(move |host| {
                    let identity = host.identity()?;
                    let namespace = host.authorization()?.namespace().to_owned();
                    let command = CommandEnvelope::new(
                        1,
                        CommandKey::new(
                            identity["catalog_id"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("catalog identity missing"))?,
                            namespace,
                            op,
                        )?,
                        identity["catalog_incarnation"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("incarnation missing"))?,
                        host.catalog_generation()?,
                        &payload.to_string(),
                    )?;
                    Ok((command, tx))
                })
                .await
                .map_err(native_read_error)?;
            tx = returned;
            sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?)").bind(&operation).bind(command.canonical_payload()).bind(now()).execute(&mut *tx).await?;
            tx.commit().await?;
            continue;
        };
        let (_, attempt) = motion_catalog_backend::command_attempts::latest_in(&mut tx, &command)
            .await
            .map_err(native_read_error)?;
        let replay_attempt = attempt.clone();
        let (replayed_outcome, returned) = runtime
            .with_host(move |host| Ok((host.reconcile_command_receipt(&replay_attempt)?, tx)))
            .await
            .map_err(native_read_error)?;
        tx = returned;
        if let Some(outcome) = replayed_outcome {
            // The native owner can be queued behind another operation. Keep
            // credential expiry current before disclosing its saved result.
            caller
                .reauthorize(&mut tx, Some(Permission::CatalogWrite))
                .await?;
            let value = wire(&full_result(outcome).map_err(native_read_error)?)
                .map_err(native_read_error)?;
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
            return Ok(tagged(
                StatusCode::CREATED,
                &value,
                value["revision"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .map_err(Problem::internal)?,
            ));
        }
        let original = command.clone();
        let owned_principal = principal.id.clone();
        let target = id.clone();
        let key = key.clone();
        let digest = body.digest.clone();
        let edge = edge.clone();
        let handle = tokio::runtime::Handle::current();
        let dispatch_caller = caller.clone();
        let result = runtime
            .guard_work_and_commit(tx, id.clone(), expected, move |host, tx, _| {
                handle
                    .block_on(
                        dispatch_caller.reauthorize(&mut **tx, Some(Permission::CatalogWrite)),
                    )
                    .map_err(|e| anyhow::anyhow!(e.detail))?;
                let namespace = host.authorization()?.namespace().to_owned();
                let authority = RelationshipAuthority {
                    host_namespace: namespace,
                    create_edges: BTreeSet::from([edge.clone()]),
                    kind_rules: BTreeMap::from([(
                        edge.kind.clone(),
                        rule(&edge.kind)
                            .ok_or_else(|| anyhow::anyhow!("unsupported relationship kind"))?,
                    )]),
                    delete_edges: BTreeMap::new(),
                };
                let (outcome, witness) = dispatch(host, &attempt, &authority)?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        tx, &original, &attempt, witness,
                    ))?;
                    return Ok(None);
                }
                let value = wire(&full_result(outcome)?)?;
                let record = Scope {
                    principal: &owned_principal,
                    operation: "createRelationship",
                    target: &target,
                    key: &key,
                };
                handle
                    .block_on(record.save(tx, &digest, StatusCode::CREATED, Some(&value), None))
                    .map_err(|e| anyhow::anyhow!(e.detail))?;
                Ok(Some(value))
            })
            .await
            .map_err(write_error)?;
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
                return Ok(tagged(
                    StatusCode::CREATED,
                    &value,
                    value["revision"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .map_err(Problem::internal)?,
                ));
            }
        }
    }
}

pub(super) async fn delete(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    if !valid_id(&id) {
        return Err(Problem::not_found());
    }
    let expected = if_match(&headers)?;
    let native_expected = i64::try_from(expected).map_err(|_| {
        Problem::bad(
            "invalid_revision",
            "Relationship revision exceeds native range",
        )
    })?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _guard = app.jobs.lock().await;
    loop {
        let (mut tx, principal, selection) = authorized_writer(&app, &caller).await?;
        let operation = format!(
            "server-delete-edge:{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&principal.id, &id, expected)).map_err(Problem::internal)?
            )
        );
        let saved: Option<String> =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(&mut *tx)
                .await?;
        let command = if let Some(saved) = saved {
            motion_catalog_backend::command_attempts::decode(&saved).map_err(native_read_error)?
        } else {
            let requested = id.clone();
            let (capture, returned) = runtime
                .with_host(move |host| {
                    Ok((
                        host.projection_work_relationship(host.catalog_generation()?, &requested)?,
                        tx,
                    ))
                })
                .await
                .map_err(native_read_error)?;
            tx = returned;
            let row = capture["relationships"]
                .as_array()
                .and_then(|rows| rows.first())
                .filter(|row| row["active"] == 1)
                .ok_or_else(Problem::not_found)?;
            let source = row["source_item_id"]
                .as_str()
                .ok_or_else(native_unavailable)?
                .to_owned();
            let target = row["target_item_id"]
                .as_str()
                .ok_or_else(native_unavailable)?
                .to_owned();
            tx = endpoints(
                runtime,
                tx,
                &selection,
                &access::catalog_scope(&principal),
                &source,
                &target,
            )
            .await?;
            access::revise(
                row["revision"].as_u64().ok_or_else(native_unavailable)?,
                expected,
                true,
            )?;
            let edge = RelationshipEdge {
                source,
                target,
                kind: row["native_kind"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .into(),
            };
            let payload = json!({"operation":"relationships","mutation":{"action":"delete","id":id,"expected_revision":native_expected,"expected_edge":edge}});
            let op = operation.clone();
            let (command, returned) = runtime
                .with_host(move |host| {
                    let identity = host.identity()?;
                    let command = CommandEnvelope::new(
                        1,
                        CommandKey::new(
                            identity["catalog_id"]
                                .as_str()
                                .ok_or_else(|| anyhow::anyhow!("catalog identity missing"))?,
                            host.authorization()?.namespace(),
                            op,
                        )?,
                        identity["catalog_incarnation"]
                            .as_str()
                            .ok_or_else(|| anyhow::anyhow!("incarnation missing"))?,
                        host.catalog_generation()?,
                        &payload.to_string(),
                    )?;
                    Ok((command, tx))
                })
                .await
                .map_err(native_read_error)?;
            tx = returned;
            sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?)").bind(&operation).bind(command.canonical_payload()).bind(now()).execute(&mut *tx).await?;
            tx.commit().await?;
            continue;
        };
        if command.payload()["operation"] != "relationships"
            || command.payload()["mutation"]["action"] != "delete"
            || command.payload()["mutation"]["id"] != id
            || command.payload()["mutation"]["expected_revision"] != native_expected
        {
            return Err(native_unavailable());
        }
        let edge: RelationshipEdge =
            serde_json::from_value(command.payload()["mutation"]["expected_edge"].clone())
                .map_err(Problem::internal)?;
        tx = endpoints(
            runtime,
            tx,
            &selection,
            &access::catalog_scope(&principal),
            &edge.source,
            &edge.target,
        )
        .await?;
        let (_, attempt) = motion_catalog_backend::command_attempts::latest_in(&mut tx, &command)
            .await
            .map_err(native_read_error)?;
        let replay_attempt = attempt.clone();
        let (receipt, returned) = runtime
            .with_host(move |host| Ok((host.reconcile_command_receipt(&replay_attempt)?, tx)))
            .await
            .map_err(native_read_error)?;
        tx = returned;
        if let Some(outcome) = receipt {
            let result = full_result(outcome).map_err(native_read_error)?;
            if result["action"] != "delete" || result["relationship"]["id"] != id {
                return Err(native_unavailable());
            }
            caller
                .reauthorize(&mut tx, Some(Permission::CatalogWrite))
                .await?;
            tx.commit().await?;
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        let source = edge.source.clone();
        let original = command.clone();
        let requested = id.clone();
        let dispatch_caller = caller.clone();
        let handle = tokio::runtime::Handle::current();
        let guarded = runtime
            .mutate_work_and_commit(tx, source, move |host, tx, _| {
                handle
                    .block_on(
                        dispatch_caller.reauthorize(&mut **tx, Some(Permission::CatalogWrite)),
                    )
                    .map_err(|e| anyhow::anyhow!(e.detail))?;
                let authority = RelationshipAuthority {
                    host_namespace: host.authorization()?.namespace().into(),
                    create_edges: BTreeSet::new(),
                    kind_rules: BTreeMap::new(),
                    delete_edges: BTreeMap::from([(
                        requested.clone(),
                        catabolic_store::catalog_relationships::AuthorizedRelationshipDelete {
                            expected_revision: native_expected,
                            edge: edge.clone(),
                        },
                    )]),
                };
                let (outcome, witness) = dispatch(host, &attempt, &authority)?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        tx, &original, &attempt, witness,
                    ))?;
                    return Ok(true);
                }
                let result = full_result(outcome)?;
                anyhow::ensure!(
                    result["action"] == "delete" && result["relationship"]["id"] == requested,
                    "native delete receipt changed"
                );
                Ok(false)
            })
            .await
            .map_err(write_error)?;
        match guarded {
            crate::native_catalog::Guarded::Applied { value: true, .. } => continue,
            crate::native_catalog::Guarded::Applied { value: false, .. } => {
                runtime
                    .rebuild_projection()
                    .await
                    .map_err(native_read_error)?;
                return Ok(StatusCode::NO_CONTENT.into_response());
            }
            crate::native_catalog::Guarded::Mismatch(_) => return Err(native_unavailable()),
        }
    }
}
