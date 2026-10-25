//! Current native artwork references. Blob reads and edits additionally require
//! their dedicated retained owner proofs; source markers alone grant neither.
use super::super::catalog::{native_read_error, native_unavailable};
use super::*;
use anyhow::Context;
use playscale_core::artwork::native::{ReferenceState, resolve_role};

fn roles(state: &ReferenceState) -> Result<Vec<ResolvedArtwork>, Problem> {
    ["poster", "backdrop", "thumbnail", "banner"]
        .into_iter()
        .map(|role| {
            let resolved = resolve_role(state, role).map_err(Problem::internal)?;
            Ok(ResolvedArtwork {
                role: resolved.role,
                asset_id: resolved.asset_id,
                selection_revision: resolved.selection_revision.to_string(),
                pinned: resolved.pinned,
                sources: resolved.sources,
                conflict: resolved.conflict,
            })
        })
        .collect()
}

pub(super) async fn selection(app: &App, caller: &Caller, id: &str) -> Result<Response, Problem> {
    let tx = native::scoped_transaction(app, caller, id, Permission::CatalogRead).await?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let (snapshot, revision) = runtime
        .metadata_and_commit(tx, id.into())
        .await
        .map_err(native_read_error)?;
    let state = crate::native_catalog::artwork::references(&snapshot).map_err(native_read_error)?;
    let body =
        serde_json::json!({"item_id":id,"revision":revision.to_string(),"roles":roles(&state)?});
    Ok(tagged(StatusCode::OK, &body, revision))
}

pub(super) async fn list(
    app: &App,
    caller: &Caller,
    id: &str,
    query: &PageQuery,
) -> Result<Json<Page<AssetBody>>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?.to_owned();
    let mut tx = native::scoped_transaction(app, caller, id, Permission::CatalogRead).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let subject = id.to_owned();
    let handle = tokio::runtime::Handle::current();
    let result = runtime
        .with_host(move |host| {
            let snapshot = host.metadata_contributions(&subject)?;
            let state = crate::native_catalog::artwork::references(&snapshot)?;
            let listed: BTreeSet<&str> = state
                .contributions()
                .iter()
                .map(|source| source.asset_id.as_str())
                .chain(state.associated_asset_ids().iter().map(String::as_str))
                .collect();
            let rows = listed
                .into_iter()
                .filter(|id| *id > after.as_str())
                .take(limit as usize + 1)
                .map(|id| {
                    let asset = state
                        .asset_reference(id)
                        .context("native artwork descriptor absent")?;
                    let mut body = asset_body(id.into(), asset.mime.clone(), asset.size_bytes);
                    body.digest = format!("sha256:{}", asset.content_sha256);
                    Ok((id.to_owned(), body))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let body = handle
                .block_on(page(&mut tx, &principal, rows, limit))
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            handle.block_on(tx.commit())?;
            Ok(body)
        })
        .await
        .map_err(native_read_error)?;
    Ok(Json(result))
}

pub(super) async fn select(
    app: &App,
    caller: &Caller,
    id: &str,
    expected: u64,
    input: ArtworkSelectionInput,
) -> Result<Response, Problem> {
    use crate::native_catalog::Guarded;
    use catabolic_store::catalog_commands::CommandOutcome;
    use sha2::{Digest, Sha256};
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _jobs = app.jobs.lock().await;
    let mut tx = native::scoped_transaction(app, caller, id, Permission::CatalogWrite).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = core::catalog_scope(&principal);
    let digest = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(input.role.as_str(), input.asset_id.as_str()))
                .map_err(Problem::internal)?
        )
    );
    let operation = format!(
        "server-select-artwork:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(principal.id.as_str(), id, expected, &digest))
                .map_err(Problem::internal)?
        )
    );
    let saved: Option<String> =
        sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
            .bind(&operation)
            .fetch_optional(&mut *tx)
            .await?;
    let recorded = saved
        .as_deref()
        .map(motion_catalog_backend::command_attempts::decode)
        .transpose()
        .map_err(Problem::internal)?;
    if let Some(original) = &recorded {
        let request = &original.payload()["request"];
        let evidence: Value = serde_json::from_str(
            request["evidence_json"]
                .as_str()
                .ok_or_else(native_unavailable)?,
        )
        .map_err(Problem::internal)?;
        if original.payload()["operation"] != "metadata_system_replace"
            || original.key().operation_id() != operation
            || request["subject"] != id
            || request["namespace"] != "motion.artwork"
            || request["record_id"] != "source_projection"
            || evidence["owner"] != "motion.artwork.references.v1"
            || evidence["request_digest"] != digest
            || evidence["expected_aggregate_revision"] != expected
        {
            return Err(native_unavailable());
        }
        let (_, attempt) = motion_catalog_backend::command_attempts::latest_in(&mut tx, original)
            .await
            .map_err(Problem::internal)?;
        let subject = id.to_owned();
        let check_scope = scope.clone();
        let handle = tokio::runtime::Handle::current();
        let (receipt, returned) = runtime
            .with_host(move |host| {
                if !handle.block_on(
                    motion_catalog_backend::aggregate_revision::may_curate_work_in(
                        &mut tx,
                        host,
                        &subject,
                        &check_scope,
                    ),
                )? {
                    return Err(anyhow::Error::new(native::OutsideCuration));
                }
                Ok((host.reconcile_command_receipt(&attempt)?, tx))
            })
            .await
            .map_err(native::mutation_error)?;
        tx = returned;
        if let Some(outcome) = receipt {
            if !matches!(outcome, CommandOutcome::Replay { .. }) {
                return Err(native_unavailable());
            }
            tx.commit().await?;
            let mut response = super::super::catalog::get_native_item(app, caller, id).await?;
            response
                .headers_mut()
                .insert("idempotent-replayed", HeaderValue::from_static("true"));
            return Ok(response);
        }
    }
    let subject = id.to_owned();
    let check_scope = scope.clone();
    let handle = tokio::runtime::Handle::current();
    let observed = runtime
        .guard_work_and_commit(tx, id.into(), expected, move |host, tx, _| {
            if !handle.block_on(
                motion_catalog_backend::aggregate_revision::may_curate_work_in(
                    tx,
                    host,
                    &subject,
                    &check_scope,
                ),
            )? {
                return Err(anyhow::Error::new(native::OutsideCuration));
            }
            crate::native_catalog::artwork::references(&host.metadata_contributions(&subject)?)
        })
        .await
        .map_err(native::mutation_error)?;
    let state = match observed {
        Guarded::Mismatch(_) => return Err(AccessError::StaleRevision.into()),
        Guarded::Applied { value, .. } => value,
    };
    let current = resolve_role(&state, &input.role).map_err(Problem::internal)?;
    let planned = playscale_core::artwork::native::plan_select(
        &state,
        &input.role,
        current.selection_revision,
        Some(&input.asset_id),
    )
    .map_err(|_| {
        Problem::invalid(
            "invalid_artwork_selection",
            "Select an asset owned by this item",
        )
    })?;
    let command = match recorded {
        Some(command) => command,
        None => runtime
            .prepare_artwork(operation, id.into(), planned.state, digest, expected)
            .await
            .map_err(native_read_error)?,
    };
    loop {
        let mut tx = native::scoped_transaction(app, caller, id, Permission::CatalogWrite).await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::CatalogWrite))
            .await?;
        if core::catalog_scope(&principal) != scope {
            return Err(native_unavailable());
        }
        let original = command.clone();
        let subject = id.to_owned();
        let check_scope = scope.clone();
        let handle = tokio::runtime::Handle::current();
        let result = runtime
            .guard_work_and_commit(tx, id.into(), expected, move |host, tx, _| {
                if !handle.block_on(
                    motion_catalog_backend::aggregate_revision::may_curate_work_in(
                        tx,
                        host,
                        &subject,
                        &check_scope,
                    ),
                )? {
                    return Err(anyhow::Error::new(native::OutsideCuration));
                }
                let (_, attempt) = handle.block_on(
                    motion_catalog_backend::command_attempts::latest_in(tx, &original),
                )?;
                let (outcome, witness) = crate::native_catalog::Runtime::execute_artwork_with_host(
                    host, &attempt, &subject,
                )?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        tx, &original, &attempt, witness,
                    ))?;
                    return Ok(true);
                }
                anyhow::ensure!(
                    matches!(
                        outcome,
                        CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. }
                    ),
                    "artwork command requires reconciliation: {}",
                    outcome.code()
                );
                handle.block_on(
                    sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('catalog',?)")
                        .bind(&subject)
                        .execute(&mut **tx),
                )?;
                Ok(false)
            })
            .await
            .map_err(native::mutation_error)?;
        match result {
            Guarded::Mismatch(_) => return Err(AccessError::StaleRevision.into()),
            Guarded::Applied { value: true, .. } => continue,
            Guarded::Applied { value: false, .. } => break,
        }
    }
    super::super::catalog::get_native_item(app, caller, id).await
}

/// Candidate IDs come only from the disposable projection. Each positive match
/// is independently authorized and re-proven against the current native owner;
/// neither cache membership nor retained legacy associations grants byte access.
pub(super) async fn content(
    app: &App,
    caller: &Caller,
    asset_id: &str,
) -> Result<(String, Vec<u8>), Problem> {
    caller.require(Permission::CatalogRead)?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    let mut hints = app.db.begin_with("BEGIN IMMEDIATE").await?;
    caller
        .reauthorize(&mut hints, Some(Permission::CatalogRead))
        .await?;
    runtime
        .validate_projection(&mut hints)
        .await
        .map_err(native_read_error)?;
    let candidates: Vec<String> = sqlx::query_scalar("SELECT native_id FROM catabolic_projection_items p WHERE EXISTS(SELECT 1 FROM json_each(p.metadata_json,'$.metadata.motion_artwork.assets') a WHERE json_extract(a.value,'$.asset_id')=?) ORDER BY native_id LIMIT 1001")
        .bind(asset_id).fetch_all(&mut *hints).await?;
    if candidates.len() > 1000 {
        return Err(native_unavailable());
    }
    hints.commit().await?;
    let mut remaining = 4 * 1024 * 1024usize;
    for subject in candidates {
        let mut tx = match native::scoped_transaction(
            app,
            caller,
            &subject,
            Permission::CatalogRead,
        )
        .await
        {
            Ok(tx) => tx,
            Err(error) if error.status == StatusCode::NOT_FOUND => continue,
            Err(error) => return Err(error),
        };
        let state: (String, String, i64) = sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(&mut *tx).await?;
        let asset_id = asset_id.to_owned();
        let handle = tokio::runtime::Handle::current();
        let (found, proof_bytes) = runtime.with_host(move |host| {
            let identity = host.identity()?;
            anyhow::ensure!(identity["catalog_id"].as_str() == Some(state.0.as_str()) && identity["catalog_incarnation"].as_str() == Some(state.1.as_str()) && host.catalog_generation()? == state.2, "artwork content native generation changed");
            let snapshot = host.metadata_contributions(&subject)?;
            let proof_bytes = serde_json::to_vec(&snapshot)?.len();
                anyhow::ensure!(proof_bytes <= remaining, "artwork content proof budget exhausted");
                let references = crate::native_catalog::artwork::references(&snapshot)?;
            let associated = references.associated_asset_ids().contains(&asset_id) || references.contributions().iter().any(|source| source.asset_id == asset_id) || references.selections().iter().any(|selection| selection.asset_id.as_deref() == Some(asset_id.as_str()));
            if !associated { handle.block_on(tx.commit())?; return Ok((None, proof_bytes)); }
            let descriptor = references.asset_reference(&asset_id).context("associated artwork descriptor absent")?;
            let row: Option<(String,i64,i64,Vec<u8>)> = handle.block_on(sqlx::query_as("SELECT mime,width,height,bytes FROM artwork_assets WHERE id=? AND length(bytes)<=8388608").bind(&asset_id).fetch_optional(&mut *tx))?;
            let (mime,width,height,bytes) = row.context("current native artwork bytes unavailable")?;
            anyhow::ensure!(mime == descriptor.mime && width == descriptor.width && height == descriptor.height && bytes.len() as i64 == descriptor.size_bytes && super::super::sha256(&bytes) == descriptor.content_sha256, "current native artwork bytes do not match descriptor");
            let (digest,decoded_mime,decoded_width,decoded_height,verified) = handle.block_on(crate::artwork::decode_upload(bytes.clone().into())).map_err(|_| anyhow::anyhow!("current artwork image decode failed"))?;
            anyhow::ensure!(digest == descriptor.content_sha256 && decoded_mime == descriptor.mime && i64::from(decoded_width) == width && i64::from(decoded_height) == height && verified.as_ref() == bytes.as_slice(), "decoded artwork descriptor mismatch");
            handle.block_on(tx.commit())?;
            Ok((Some((mime, bytes)), proof_bytes))
        }).await.map_err(native_read_error)?;
        remaining -= proof_bytes;
        if let Some(found) = found {
            return Ok(found);
        }
    }
    Err(Problem::not_found())
}
