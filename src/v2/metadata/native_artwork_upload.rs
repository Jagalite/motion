//! Upload bytes are staged by Motion's immutable asset owner. Only an exact
//! decoded descriptor crosses into the native atomic association owner.
use super::super::catalog::{native_read_error, native_unavailable};
use super::*;
use anyhow::Context;
use catabolic_store::catalog_commands::CommandOutcome;
use playscale_core::artwork::native::{ASSET_OWNER, AssetReference};
use sha2::{Digest, Sha256};

pub(super) async fn upload(
    app: &App,
    caller: &Caller,
    id: &str,
    key: &str,
    bytes: axum::body::Bytes,
) -> Result<Response, Problem> {
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _jobs = app.jobs.lock().await;
    let digest = super::super::sha256(&bytes);
    let mut tx = native::scoped_transaction(app, caller, id, Permission::CatalogWrite).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = core::catalog_scope(&principal);
    let subject = id.to_owned();
    let check_scope = scope.clone();
    let handle = tokio::runtime::Handle::current();
    tx = runtime
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
            Ok(tx)
        })
        .await
        .map_err(native::mutation_error)?;
    let record = Scope {
        principal: &principal.id,
        operation: "uploadArtwork",
        target: id,
        key,
    };
    let stored = record.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        tx.commit().await?;
        return Ok(replayed(stored.unwrap()));
    }
    // Expired HTTP keys are reusable. The last durable ACK expiry is a stable
    // incarnation anchor across lost-ACK retries, rather than a wall-clock bucket.
    let key_epoch = stored.as_ref().map(|record| record.record.expires_at);
    let restore_epoch: String = sqlx::query_scalar("SELECT restore_epoch FROM server_identity")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    let (asset_id, mime, width, height, bytes) = crate::artwork::decode_upload(bytes)
        .await
        .map_err(|error| {
            let (status, _, message) = error.parts();
            Problem::new(status, "invalid_image", message)
        })?;
    let asset = AssetReference {
        asset_id: asset_id.clone(),
        mime: mime.into(),
        width: i64::from(width),
        height: i64::from(height),
        size_bytes: bytes.len() as i64,
        content_sha256: digest.clone(),
        owner: ASSET_OWNER.into(),
    };
    let operation = format!(
        "server-attach-artwork:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(principal.id.as_str(), id, key, key_epoch, &restore_epoch))
                .map_err(Problem::internal)?
        )
    );
    let mut tx = native::scoped_transaction(app, caller, id, Permission::CatalogWrite).await?;
    let current = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    if current.id != principal.id || core::catalog_scope(&current) != scope {
        return Err(native_unavailable());
    }
    let subject = id.to_owned();
    let check_scope = scope.clone();
    let staged = asset.clone();
    let handle = tokio::runtime::Handle::current();
    tx = runtime.with_host(move |host| {
        if !handle.block_on(motion_catalog_backend::aggregate_revision::may_curate_work_in(&mut tx, host, &subject, &check_scope))? { return Err(anyhow::Error::new(native::OutsideCuration)); }
        handle.block_on(sqlx::query("INSERT OR IGNORE INTO artwork_assets(id,mime,width,height,bytes) VALUES(?,?,?,?,?)")
            .bind(&staged.asset_id).bind(&staged.mime).bind(staged.width).bind(staged.height).bind(bytes.as_ref()).execute(&mut *tx))?;
        verify_staged(&handle, &mut tx, &staged)?;
        Ok(tx)
    }).await.map_err(native::mutation_error)?;
    let current_restore_epoch: String =
        sqlx::query_scalar("SELECT restore_epoch FROM server_identity")
            .fetch_one(&mut *tx)
            .await?;
    if current_restore_epoch != restore_epoch {
        return Err(native_unavailable());
    }
    let staged_record = Scope {
        principal: &principal.id,
        operation: "uploadArtwork",
        target: id,
        key,
    };
    let staged_ack = staged_record.load(&mut tx).await?;
    if core::idempotency(
        staged_ack.as_ref().map(|stored| &stored.record),
        &digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        tx.commit().await?;
        return Ok(replayed(staged_ack.unwrap()));
    }
    if staged_ack.as_ref().map(|stored| stored.record.expires_at) != key_epoch {
        return Err(native_unavailable());
    }
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
    if let Some(command) = &recorded {
        let request: catabolic_store::catalog_metadata_store::MetadataArtworkAttach =
            serde_json::from_value(command.payload()["request"].clone())
                .map_err(Problem::internal)?;
        if command.payload()["operation"] != "metadata_artwork_attach"
            || command.key().operation_id() != operation
            || request.subject != id
            || request.asset != asset
        {
            return Err(conflict(
                "idempotency_conflict",
                "This idempotency key was used for a different upload",
            ));
        }
    }
    tx.commit().await?;
    let command = match recorded {
        Some(command) => command,
        None => runtime
            .prepare_artwork_attach(operation, id.into(), asset.clone())
            .await
            .map_err(native_read_error)?,
    };
    let value = serde_json::to_value(asset_body(
        asset.asset_id.clone(),
        asset.mime.clone(),
        asset.size_bytes,
    ))
    .map_err(Problem::internal)?;
    let recovered_replay;
    loop {
        let mut tx = native::scoped_transaction(app, caller, id, Permission::CatalogWrite).await?;
        let current = caller
            .reauthorize(&mut tx, Some(Permission::CatalogWrite))
            .await?;
        if current.id != principal.id || core::catalog_scope(&current) != scope {
            return Err(native_unavailable());
        }
        let current_restore_epoch: String =
            sqlx::query_scalar("SELECT restore_epoch FROM server_identity")
                .fetch_one(&mut *tx)
                .await?;
        if current_restore_epoch != restore_epoch {
            return Err(native_unavailable());
        }
        let original = command.clone();
        let subject = id.to_owned();
        let check_scope = scope.clone();
        let verified = asset.clone();
        let principal_id = principal.id.clone();
        let key = key.to_owned();
        let digest = digest.clone();
        let value = value.clone();
        let handle = tokio::runtime::Handle::current();
        let (retry, replay) = runtime
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
                let (_, attempt) = handle.block_on(
                    motion_catalog_backend::command_attempts::latest_in(&mut tx, &original),
                )?;
                verify_staged(&handle, &mut tx, &verified)?;
                let outcome = if let Some(receipt) = host.reconcile_command_receipt(&attempt)? {
                    receipt
                } else {
                    let (outcome, witness) =
                        crate::native_catalog::Runtime::execute_artwork_attach_with_host(
                            host, &attempt, &subject, &verified,
                        )?;
                    if let Some(witness) = witness {
                        handle.block_on(
                            motion_catalog_backend::command_attempts::record_next_in(
                                &mut tx, &original, &attempt, witness,
                            ),
                        )?;
                        handle.block_on(tx.commit())?;
                        return Ok((true, false));
                    }
                    outcome
                };
                anyhow::ensure!(
                    matches!(
                        outcome,
                        CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. }
                    ),
                    "artwork attachment requires reconciliation: {}",
                    outcome.code()
                );
                let replay = matches!(outcome, CommandOutcome::Replay { .. });
                let record = Scope {
                    principal: &principal_id,
                    operation: "uploadArtwork",
                    target: &subject,
                    key: &key,
                };
                handle
                    .block_on(record.save(
                        &mut tx,
                        &digest,
                        StatusCode::CREATED,
                        Some(&value),
                        None,
                    ))
                    .map_err(|error| anyhow::anyhow!("{error:?}"))?;
                handle.block_on(
                    sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('catalog',?)")
                        .bind(&subject)
                        .execute(&mut *tx),
                )?;
                handle.block_on(tx.commit())?;
                Ok((false, replay))
            })
            .await
            .map_err(native::mutation_error)?;
        if !retry {
            recovered_replay = replay;
            break;
        }
    }
    let mut response = tagged(StatusCode::CREATED, &value, ASSET_REVISION);
    if recovered_replay {
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    Ok(response)
}

fn verify_staged(
    handle: &tokio::runtime::Handle,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    asset: &AssetReference,
) -> anyhow::Result<()> {
    let row: Option<(String,i64,i64,Vec<u8>)> = handle.block_on(sqlx::query_as("SELECT mime,width,height,bytes FROM artwork_assets WHERE id=? AND length(bytes)<=8388608").bind(&asset.asset_id).fetch_optional(&mut **tx))?;
    let (mime, width, height, bytes) = row.context("staged artwork asset missing")?;
    anyhow::ensure!(
        mime == asset.mime
            && width == asset.width
            && height == asset.height
            && bytes.len() as i64 == asset.size_bytes
            && super::super::sha256(&bytes) == asset.content_sha256,
        "staged artwork descriptor or bytes changed"
    );
    Ok(())
}
