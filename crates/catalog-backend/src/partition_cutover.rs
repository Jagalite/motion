//! Complete-set handoff: no prefix of a partitioned source can admit serving.
use crate::{
    HOST_NAMESPACE, NativeCatalog,
    cutover::{ServerAdmission, native_checkpoint},
    decode_envelope, ensure_same_request,
    motion_export::MotionExportPlan,
    receipt_policy, status,
};
use anyhow::{Context, bail, ensure};
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility};
use catabolic_runtime::host::{
    HostSession, PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION, RecordedImportHandshake,
    RecordedImportSet,
};
use catabolic_store::{
    Store,
    catalog_commands::{self, CatalogAuthority, CommandOutcome, TrustedCommandAuthority},
    catalog_import::{
        self as native, ActivationBinding, ImportAuthority, ImportMutation, ImportReceipt,
        ImportStep, Reconciliation, TrustedImportActivation,
    },
    catalog_import_typed::TypedAuthority,
};
use playscale_core::{
    catalog_backend::Identity,
    catalog_cutover::{Checkpoint, Phase},
    catalog_source::Mode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use std::{collections::BTreeSet, path::PathBuf, time::Duration};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetManifest {
    pub identity: Identity,
    pub freeze_revision: u64,
    pub plan: MotionExportPlan,
}
fn digest(m: &SetManifest) -> anyhow::Result<String> {
    Ok(catabolic_core::sha256(
        catabolic_core::encode_with(&serde_json::to_value(m)?, false, true).as_bytes(),
    ))
}
pub(crate) async fn saved(db: &SqlitePool) -> anyhow::Result<(SetManifest, String, String)> {
    let (root,hash,text,epoch,phase):(String,String,String,i64,String)=sqlx::query_as("SELECT root_import_id,manifest_digest,manifest_json,freeze_revision,phase FROM catabolic_cutover_sets WHERE singleton=1").fetch_one(db).await?;
    let manifest: SetManifest = serde_json::from_str(&text)?;
    ensure!(
        manifest.plan.root_import_id == root
            && manifest.freeze_revision == epoch as u64
            && digest(&manifest)? == hash,
        "cutover manifest changed"
    );
    ensure!(
        !manifest.plan.partitions.is_empty()
            && manifest.plan.partitions.len()
                <= catabolic_runtime::host::MAX_IMPORT_RESTART_PARTITIONS,
        "invalid partition set size"
    );
    let mut ids = BTreeSet::new();
    for (index, p) in manifest.plan.partitions.iter().enumerate() {
        ensure!(
            p.index as usize == index
                && ids.insert(&p.import_id)
                && p.dependency_indices.iter().all(|d| *d < p.index),
            "invalid partition dependency order"
        );
    }
    Ok((manifest, hash, phase))
}
async fn require_seal(db: &SqlitePool, m: &SetManifest) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    let state = crate::source_freeze::read(&mut tx).await?;
    ensure!(
        matches!(state.mode, Mode::Sealed { .. })
            && state.active_freeze() == Some((m.plan.root_import_id.as_str(), m.freeze_revision)),
        "partition source seal mismatch"
    );
    crate::source_freeze::verify(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn open_server(
    db: SqlitePool,
    path: PathBuf,
    profile: String,
) -> anyhow::Result<ServerAdmission> {
    let (manifest, hash, phase) = saved(&db).await?;
    ensure!(
        phase == "activated",
        "complete partition set is not acknowledged"
    );
    require_seal(&db, &manifest).await?;
    let binding: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&db)
            .await?;
    manifest
        .identity
        .admit(&serde_json::from_str(&binding)?)
        .map_err(|e| anyhow::anyhow!("binding: {e:?}"))?;
    let rows:Vec<(String,i64,String,String,String)>=sqlx::query_as("SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts ORDER BY ordinal").fetch_all(&db).await?;
    ensure!(
        rows.len() == manifest.plan.partitions.len(),
        "incomplete partition receipt set"
    );
    let mut handshakes = Vec::new();
    let mut ids = BTreeSet::new();
    for ((id, ordinal, checkpoint, envelope, phase), part) in
        rows.into_iter().zip(&manifest.plan.partitions)
    {
        let cp: Checkpoint = serde_json::from_str(&checkpoint)?;
        ensure!(
            id == part.import_id
                && ordinal == part.index as i64
                && phase == "activated"
                && cp.phase == Phase::Activated
                && cp.import_id == id
                && cp.input_digest == part.snapshot_digest
                && cp.freeze_revision == manifest.freeze_revision
                && cp.catalog_id == manifest.identity.catalog_id
                && cp.incarnation == manifest.identity.incarnation
                && cp.valid(),
            "partition checkpoint mismatch"
        );
        ids.insert(id);
        handshakes.push(RecordedImportHandshake {
            activation_intent: decode_envelope(&envelope)?,
            checkpoint: native_checkpoint(&cp),
        });
    }
    let set = RecordedImportSet::from_durable_manifest(hash.clone(), &ids, handshakes)?;
    let effective = crate::native_upgrade::effective_identity(&db, &manifest.identity).await?;
    tokio::task::spawn_blocking(move || {
        let mut admission = HostSession::open_after_import_cutovers(
            &path,
            &profile,
            PARTITIONED_IMPORT_RESTART_CONTRACT_VERSION,
            &set,
            &hash,
            TrustedCommandAuthority {
                host_namespace: HOST_NAMESPACE,
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: false,
                identity_authority: None,
            },
            uuid::Uuid::new_v4().to_string(),
            receipt_policy(),
        )?;
        ensure!(
            admission.decision() == Reconciliation::AdmitCatabolicOnly,
            "partition receipt set does not admit server"
        );
        crate::cutover::verify_admitted_identity(admission.admitted_host()?, &effective)?;
        Ok(ServerAdmission {
            admission,
            identity: effective,
        })
    })
    .await?
}
impl NativeCatalog {
    pub async fn prepare_motion_partition_cutover(
        &self,
        root: &str,
    ) -> anyhow::Result<SetManifest> {
        let plan = self.motion_export_plan(root).await?;
        ensure!(
            !plan.partitions.is_empty()
                && plan.partitions.len() <= catabolic_runtime::host::MAX_IMPORT_RESTART_PARTITIONS,
            "partition set exceeds native restart contract"
        );
        let state = self.source_state().await?;
        let (owner, epoch) = state.active_freeze().context("source is not frozen")?;
        ensure!(owner == root, "source freeze owner mismatch");
        ensure!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_cutover")
                .fetch_one(&self.journal)
                .await?
                == 0,
            "single cutover already owns host"
        );
        for part in &plan.partitions {
            self.require_frozen_export(&part.import_id).await?;
            ensure!(
                self.motion_reference_inventory(&part.import_id)
                    .await?
                    .activation_ready,
                "unresolved partition reference obligations"
            );
        }
        let manifest = SetManifest {
            identity: self.identity.clone(),
            freeze_revision: epoch,
            plan,
        };
        let hash = digest(&manifest)?;
        let text = serde_json::to_string(&manifest)?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        ensure!(
            crate::source_freeze::read(&mut tx).await? == state,
            "freeze changed"
        );
        crate::source_freeze::verify(&mut tx).await?;
        sqlx::query("INSERT INTO catabolic_cutover_sets VALUES(1,?,?,?,?,'prepared') ON CONFLICT(singleton) DO NOTHING").bind(root).bind(&hash).bind(&text).bind(epoch as i64).execute(&mut *tx).await?;
        let retained: (String, String) = sqlx::query_as(
            "SELECT manifest_digest,manifest_json FROM catabolic_cutover_sets WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            retained == (hash, text),
            "different partition set already owns host"
        );
        tx.commit().await?;
        Ok(manifest)
    }
    /// Restartable operator-only execution. A durable intent always precedes its
    /// native effect. Cancellation leaves the source sealed and serving closed.
    pub async fn activate_motion_partition_cutover(
        &self,
        timeout: Duration,
    ) -> anyhow::Result<SetManifest> {
        self.activate_partition_prefix(timeout, usize::MAX).await
    }

    // A bounded prefix is used only by the in-module recovery test. Public
    // operation always executes the complete durable plan.
    async fn activate_partition_prefix(
        &self,
        timeout: Duration,
        max_parts: usize,
    ) -> anyhow::Result<SetManifest> {
        let (manifest, _, _phase) = saved(&self.journal).await?;
        manifest
            .identity
            .admit(&self.identity)
            .map_err(|e| anyhow::anyhow!("binding: {e:?}"))?;
        if !matches!(self.source_state().await?.mode, Mode::Sealed { .. }) {
            self.seal_motion_freeze(manifest.freeze_revision).await?;
        }
        self.drain_witness(timeout).await?;
        require_seal(&self.journal, &manifest).await?;
        for part in &manifest.plan.partitions {
            let existing:Option<(String,String)>=sqlx::query_as("SELECT checkpoint_json,activation_envelope FROM catabolic_cutover_parts WHERE import_id=?").bind(&part.import_id).fetch_optional(&self.journal).await?;
            let (snapshot, input) = self.import_snapshot(&part.import_id).await?;
            ensure!(input == part.snapshot_digest, "partition snapshot changed");
            let typed = self.motion_import_authority(&part.import_id).await?;
            let (checkpoint, envelope) = if let Some((cp, encoded)) = existing {
                (
                    serde_json::from_str::<Checkpoint>(&cp)?,
                    decode_envelope(&encoded)?,
                )
            } else {
                self.sealed_import_step(
                    &manifest,
                    &snapshot.manifest,
                    ImportStep::Prepare,
                    ImportMutation::Prepare {
                        manifest: snapshot.manifest.clone(),
                        input_digest: input.clone(),
                    },
                    None,
                    None,
                )
                .await?;
                for (index, batch) in snapshot.batches.iter().enumerate() {
                    self.sealed_import_step(
                        &manifest,
                        &snapshot.manifest,
                        ImportStep::Batch(index as u32),
                        ImportMutation::Stage {
                            import_id: part.import_id.clone(),
                            input_digest: input.clone(),
                            batch_index: index as u32,
                            batch: batch.clone(),
                        },
                        None,
                        None,
                    )
                    .await?;
                }
                let receipt = self
                    .sealed_import_step(
                        &manifest,
                        &snapshot.manifest,
                        ImportStep::Validate,
                        ImportMutation::Validate {
                            import_id: part.import_id.clone(),
                            input_digest: input.clone(),
                        },
                        Some(typed.clone()),
                        None,
                    )
                    .await?;
                let ImportReceipt::ValidatedStaging {
                    validation_generation,
                    mapping_digest,
                    unresolved_evidence: 0,
                    activation_supported: true,
                    ..
                } = receipt
                else {
                    bail!("partition owner validation unresolved")
                };
                let cp = Checkpoint {
                    catalog_id: self.identity.catalog_id.clone(),
                    incarnation: self.identity.incarnation.clone(),
                    import_id: part.import_id.clone(),
                    input_digest: input.clone(),
                    mapping_digest,
                    reference_ack_id: format!("motion-ref-{}", manifest.plan.audit_digest),
                    freeze_revision: manifest.freeze_revision,
                    validation_generation,
                    phase: Phase::Validated,
                };
                let mutation = ImportMutation::Activate {
                    import_id: cp.import_id.clone(),
                    input_digest: input.clone(),
                    reference_ack_id: cp.reference_ack_id.clone(),
                    mapping_digest: cp.mapping_digest.clone(),
                };
                let operation = native::operation_id(&cp.import_id, ImportStep::Activate)?;
                let envelope = self
                    .sealed_prepare(
                        &manifest,
                        &operation,
                        json!({"operation":"import","mutation":mutation}),
                    )
                    .await?;
                ensure!(
                    envelope.expected_generation() == validation_generation,
                    "partition generation changed"
                );
                let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
                sqlx::query("INSERT INTO catabolic_cutover_parts VALUES(?,?,?,?, 'validated') ON CONFLICT(import_id) DO NOTHING").bind(&cp.import_id).bind(part.index as i64).bind(serde_json::to_string(&cp)?).bind(envelope.canonical_payload()).execute(&mut *tx).await?;
                let retained:(i64,String,String)=sqlx::query_as("SELECT ordinal,checkpoint_json,activation_envelope FROM catabolic_cutover_parts WHERE import_id=?").bind(&cp.import_id).fetch_one(&mut *tx).await?;
                ensure!(
                    retained
                        == (
                            part.index as i64,
                            serde_json::to_string(&cp)?,
                            envelope.canonical_payload().to_owned()
                        ),
                    "concurrent partition checkpoint differs"
                );
                tx.commit().await?;
                (cp, envelope)
            };
            ensure!(
                checkpoint.valid()
                    && checkpoint.import_id == part.import_id
                    && checkpoint.input_digest == input
                    && checkpoint.freeze_revision == manifest.freeze_revision
                    && checkpoint.catalog_id == self.identity.catalog_id
                    && checkpoint.incarnation == self.identity.incarnation,
                "partition checkpoint changed"
            );
            let witness = self.drain_witness(timeout).await?;
            let grant = TrustedImportActivation::after_drain(
                &self.authorization,
                witness,
                ActivationBinding {
                    catalog_id: checkpoint.catalog_id.clone(),
                    incarnation: checkpoint.incarnation.clone(),
                    import_id: checkpoint.import_id.clone(),
                    input_digest: input,
                    source_snapshot_id: snapshot.manifest.source_snapshot_id.clone(),
                    validation_generation: checkpoint.validation_generation,
                    reference_ack_id: checkpoint.reference_ack_id.clone(),
                    mapping_digest: checkpoint.mapping_digest.clone(),
                },
            )?;
            let receipt = self
                .sealed_execute(
                    &manifest,
                    &snapshot.manifest,
                    envelope,
                    Some(typed),
                    Some(grant),
                )
                .await?;
            let decision = native::reconcile_activation(
                Some(&receipt),
                &native_checkpoint(&checkpoint),
                &self.identity.catalog_id,
                &self.identity.incarnation,
            );
            ensure!(
                matches!(
                    decision,
                    Reconciliation::FinishHostActivation | Reconciliation::AdmitCatabolicOnly
                ),
                "partition receipt mismatch"
            );
            let mut active = checkpoint.clone();
            active.phase = Phase::Activated;
            checkpoint
                .acknowledge(&active)
                .map_err(anyhow::Error::msg)?;
            let mut ack_tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
            crate::revision_bridge::acknowledge_import_in(&mut ack_tx, &receipt).await?;
            sqlx::query("UPDATE catabolic_cutover_parts SET checkpoint_json=?,phase='activated' WHERE import_id=? AND checkpoint_json=?").bind(serde_json::to_string(&active)?).bind(&checkpoint.import_id).bind(serde_json::to_string(&checkpoint)?).execute(&mut *ack_tx).await?;
            ack_tx.commit().await?;
            ensure!(
                (part.index as usize + 1) < max_parts
                    || part.index as usize + 1 == manifest.plan.partitions.len(),
                "partition execution interrupted after durable acknowledgment"
            );
        }
        require_seal(&self.journal, &manifest).await?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT checkpoint_json FROM catabolic_cutover_parts ORDER BY ordinal",
        )
        .fetch_all(&mut *tx)
        .await?;
        let observations: Vec<Checkpoint> = rows
            .iter()
            .map(|text| serde_json::from_str(text))
            .collect::<Result<_, _>>()?;
        let expected: Vec<_> = manifest
            .plan
            .partitions
            .iter()
            .map(|part| (part.import_id.clone(), part.snapshot_digest.clone()))
            .collect();
        playscale_core::catalog_cutover::admit_complete_set(
            &manifest.identity.catalog_id,
            &manifest.identity.incarnation,
            manifest.freeze_revision,
            &expected,
            &observations,
        )
        .map_err(anyhow::Error::msg)?;
        sqlx::query("UPDATE catabolic_cutover_sets SET phase='activated' WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(manifest)
    }
    async fn sealed_prepare(
        &self,
        m: &SetManifest,
        operation: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<CommandEnvelope> {
        require_seal(&self.journal, m).await?;
        let requested = CommandEnvelope::new(
            1,
            CommandKey::new(&self.identity.catalog_id, HOST_NAMESPACE, operation)?,
            &self.identity.incarnation,
            0,
            &payload.to_string(),
        )?;
        if let Some(existing) = self.recorded(operation).await? {
            ensure_same_request(&existing, &requested)?;
            return Ok(existing);
        }
        let inspected = crate::inspect((*self.path).clone()).await?;
        self.identity
            .admit(&inspected.identity)
            .map_err(|e| anyhow::anyhow!("binding: {e:?}"))?;
        let envelope = CommandEnvelope::new(
            1,
            requested.key().clone(),
            &self.identity.incarnation,
            inspected.generation,
            &payload.to_string(),
        )?;
        sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?) ON CONFLICT(operation_id) DO NOTHING").bind(operation).bind(envelope.canonical_payload()).bind(crate::now()).execute(&self.journal).await?;
        let saved = self.recorded(operation).await?.context("intent missing")?;
        ensure_same_request(&saved, &requested)?;
        Ok(saved)
    }
    async fn sealed_import_step(
        &self,
        m: &SetManifest,
        manifest: &native::SnapshotManifest,
        step: ImportStep,
        mutation: ImportMutation,
        typed: Option<TypedAuthority>,
        grant: Option<TrustedImportActivation>,
    ) -> anyhow::Result<ImportReceipt> {
        let id = native::operation_id(&manifest.import_id, step)?;
        let envelope = self
            .sealed_prepare(m, &id, json!({"operation":"import","mutation":mutation}))
            .await?;
        self.sealed_execute(m, manifest, envelope, typed, grant)
            .await
    }
    async fn sealed_execute(
        &self,
        m: &SetManifest,
        manifest: &native::SnapshotManifest,
        envelope: CommandEnvelope,
        typed: Option<TypedAuthority>,
        activation: Option<TrustedImportActivation>,
    ) -> anyhow::Result<ImportReceipt> {
        require_seal(&self.journal, m).await?;
        let permit = self.worker.clone().acquire_owned().await?;
        let path = self.path.clone();
        let identity = self.identity.clone();
        let imports = ImportAuthority {
            host_namespace: HOST_NAMESPACE.into(),
            import_id: manifest.import_id.clone(),
            source_namespace: manifest.source_namespace.clone(),
        };
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let store = Store::open(&path, true, false)?;
            identity
                .admit(&status(&store)?.identity)
                .map_err(|e| anyhow::anyhow!("binding: {e:?}"))?;
            let mut authority = CatalogAuthority::new(TrustedCommandAuthority {
                host_namespace: HOST_NAMESPACE,
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None,
            })
            .with_imports(&imports);
            if let Some(t) = &typed {
                authority = authority.with_import_typed(t)
            }
            if let Some(a) = &activation {
                authority = authority.with_import_activation(a)
            }
            match catalog_commands::execute_with_authority(
                &store,
                &envelope,
                authority,
                receipt_policy(),
            )? {
                CommandOutcome::Committed { result, .. }
                | CommandOutcome::Replay { result, .. } => Ok(serde_json::from_value(result)?),
                other => bail!("partition requires reconciliation: {}", other.code()),
            }
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn interrupted_prefix_never_admits_and_resumes_saved_intents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite3");
        crate::initialize(path.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(crate::projection::configure_connection(connection))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(dir.path().join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let mut tx = db.begin().await.unwrap();
        for n in 0..1100 {
            let id = format!("item-{n:04}");
            sqlx::query("INSERT INTO items(id,title,kind) VALUES(?,?,'video')")
                .bind(&id)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO item_origins VALUES(?,?)")
                .bind(&id)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let identity = crate::inspect(path.clone()).await.unwrap().identity;
        let backend = NativeCatalog::bind(db.clone(), path.clone(), identity.clone())
            .await
            .unwrap();
        backend
            .freeze_motion_source("interrupted", 0)
            .await
            .unwrap();
        let plan = backend
            .capture_motion_export_plan("interrupted")
            .await
            .unwrap();
        assert!(plan.partitions.len() > 1);
        backend
            .prepare_motion_partition_cutover("interrupted")
            .await
            .unwrap();
        let error = backend
            .activate_partition_prefix(Duration::from_secs(2), 1)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("interrupted after durable acknowledgment"),
            "{error:#}"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM catabolic_cutover_parts WHERE phase='activated'"
            )
            .fetch_one(&db)
            .await
            .unwrap(),
            1
        );
        assert!(
            open_server(db.clone(), path.clone(), "default".into())
                .await
                .is_err()
        );
        drop(backend);
        let reopened = NativeCatalog::bind(db.clone(), path.clone(), identity)
            .await
            .unwrap();
        reopened
            .activate_motion_partition_cutover(Duration::from_secs(2))
            .await
            .unwrap();
        let mut admitted = open_server(db.clone(), path, "default".into())
            .await
            .unwrap();
        // Reuse the genuine multi-part activation above: descriptive source
        // completeness must reconcile every part, including a proven zero set.
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let prepared = crate::imported_sources::historical::prepare_in(&mut tx)
            .await
            .unwrap();
        let inventory = prepared
            .verify_in(&mut tx, admitted.admission.admitted_host().unwrap())
            .await
            .unwrap();
        assert!(inventory.physical_source_ids.is_empty());
        assert!(inventory.historical_source_ids.is_empty());
        let host = admitted.admission.admitted_host().unwrap();
        let generation = host.catalog_generation().unwrap();
        let absence_subjects =
            std::collections::BTreeSet::from(["item-0000".to_owned(), "item-1099".to_owned()]);
        let absence = crate::nfo_bootstrap::imported_absence::prepare_in(
            &mut tx,
            host,
            generation,
            &absence_subjects,
        )
        .await
        .unwrap();
        assert_eq!(absence.subjects().len(), 2);
        assert_eq!(absence.root_import_id(), "interrupted");
        drop(absence);
        let saved_trigger: String = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='trigger' AND name='catabolic_cutover_parts_no_delete'")
            .fetch_one(&mut *tx).await.unwrap();
        let saved_parts: Vec<(String, i64, String, String, String)> = sqlx::query_as("SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts ORDER BY ordinal")
            .fetch_all(&mut *tx).await.unwrap();
        // Ordinary owner connections cannot delete activation evidence.
        assert!(
            sqlx::query("DELETE FROM catabolic_cutover_parts WHERE ordinal=0")
                .execute(&mut *tx)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();

        // Deliberate corruption in this rolled-back test writer only. Never
        // fabricate an activation or alter native receipts. Rollback restores
        // both the immutable-delete trigger and its genuine saved part.
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("DROP TRIGGER catabolic_cutover_parts_no_delete")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query("DELETE FROM catabolic_cutover_parts WHERE ordinal=0")
                .execute(&mut *tx)
                .await
                .unwrap()
                .rows_affected(),
            1
        );
        let error = crate::imported_sources::historical::prepare_in(&mut tx)
            .await
            .err()
            .expect("missing activated part must refuse inventory");
        assert!(
            error.to_string().contains("partitions incomplete"),
            "{error:#}"
        );
        assert!(
            crate::nfo_bootstrap::imported_absence::prepare_in(
                &mut tx,
                admitted.admission.admitted_host().unwrap(),
                generation,
                &absence_subjects,
            )
            .await
            .is_err(),
            "missing root part cannot prove original absence"
        );
        tx.rollback().await.unwrap();

        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let prepared = crate::imported_sources::historical::prepare_in(&mut tx)
            .await
            .unwrap();
        let restored_trigger: String = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type='trigger' AND name='catabolic_cutover_parts_no_delete'")
            .fetch_one(&mut *tx).await.unwrap();
        let restored_parts: Vec<(String, i64, String, String, String)> = sqlx::query_as("SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts ORDER BY ordinal")
            .fetch_all(&mut *tx).await.unwrap();
        assert_eq!(restored_trigger, saved_trigger);
        assert_eq!(restored_parts, saved_parts);
        let restored = prepared
            .verify_in(&mut tx, admitted.admission.admitted_host().unwrap())
            .await
            .unwrap();
        assert_eq!(restored.activation_digest, inventory.activation_digest);
        assert!(
            sqlx::query("DELETE FROM catabolic_cutover_parts WHERE ordinal=0")
                .execute(&mut *tx)
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        admitted.admission.admitted_host().unwrap().close().unwrap();
    }
}
