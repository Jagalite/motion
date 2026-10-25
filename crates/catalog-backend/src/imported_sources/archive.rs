//! Complete frozen archive descriptions shared by provenance consumers.
//! These tokens prove archive custody and actual activation receipts only, not
//! NFO attribution, source registration, current bytes or permission to mutate.
use super::*;
use std::collections::BTreeSet;
mod retained;
pub(crate) use retained::{
    ImportedLibraryBridge, RetainedImportedArchive, TransferredImportedArchive,
};

pub(crate) struct Entry {
    pub(super) checkpoint: Checkpoint,
    pub(super) snapshot: crate::migration::Snapshot,
    pub(super) audit_raw: String,
}
impl Entry {
    pub(crate) fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }
    pub(crate) fn snapshot(&self) -> &crate::migration::Snapshot {
        &self.snapshot
    }
    pub(crate) fn audit_raw(&self) -> &str {
        &self.audit_raw
    }
}
pub(crate) struct PreparedArchive {
    pub(super) control: Control,
    pub(super) identity: Identity,
    root: String,
    freeze_revision: u64,
    checkpoints: Vec<(Checkpoint, String)>,
    pub(super) entries: Vec<Entry>,
}
/// Existing full-activation-result cohort. A compacted generic receipt is not
/// silently treated as replay: consumers needing compacted provenance must use
/// the native permanent activation verifier with these exact archived inputs.
/// This token alone does not establish NFO identity/origin/tag completeness.
pub(crate) struct VerifiedArchive {
    prepared: PreparedArchive,
}
impl VerifiedArchive {
    pub(crate) fn identity(&self) -> &Identity {
        &self.prepared.identity
    }
    pub(crate) fn activation_digest(&self) -> anyhow::Result<String> {
        Ok(catabolic_core::sha256(serde_json::to_vec(
            &self.prepared.control,
        )?))
    }
    pub(crate) fn root_import_id(&self) -> &str {
        &self.prepared.root
    }
    pub(crate) fn root_manifest_digest(&self) -> Option<&str> {
        self.prepared.control.set.as_ref().map(|row| row.1.as_str())
    }
    pub(crate) fn activations(&self) -> impl Iterator<Item = (&Checkpoint, &str)> {
        self.prepared
            .checkpoints
            .iter()
            .map(|(checkpoint, raw)| (checkpoint, raw.as_str()))
    }
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.prepared.entries
    }
}
async fn budget(db: &mut SqliteConnection) -> anyhow::Result<()> {
    let (count, bytes): (i64,i64) = sqlx::query_as("SELECT count(*),COALESCE(sum(bytes),0) FROM (SELECT length(CAST(import_id AS BLOB))+length(CAST(checkpoint_json AS BLOB))+length(CAST(activation_envelope AS BLOB))+length(CAST(phase AS BLOB)) AS bytes FROM catabolic_cutover UNION ALL SELECT length(CAST(root_import_id AS BLOB))+length(CAST(manifest_digest AS BLOB))+length(CAST(manifest_json AS BLOB))+length(CAST(phase AS BLOB)) AS bytes FROM catabolic_cutover_sets UNION ALL SELECT length(CAST(import_id AS BLOB))+length(CAST(checkpoint_json AS BLOB))+length(CAST(activation_envelope AS BLOB))+length(CAST(phase AS BLOB)) AS bytes FROM catabolic_cutover_parts)").fetch_one(&mut *db).await?;
    ensure!(
        count <= catabolic_runtime::host::MAX_IMPORT_RESTART_PARTITIONS as i64 + 2
            && bytes <= 8 * 1024 * 1024,
        "historical activation inventory exceeds bounds"
    );
    let (count, bytes): (i64,i64) = sqlx::query_as("SELECT count(*),COALESCE(sum(length(CAST(library_id AS BLOB))+length(CAST(mapping_json AS BLOB))),0) FROM catabolic_source_mappings").fetch_one(&mut *db).await?;
    ensure!(
        count <= 10000 && bytes <= 16 * 1024 * 1024,
        "historical source mapping inventory exceeds bounds"
    );
    Ok(())
}
fn prepare_control(
    captured: &Control,
    identity: &Identity,
) -> anyhow::Result<(
    Vec<(Checkpoint, String)>,
    Option<crate::motion_export::MotionExportPlan>,
    String,
    u64,
)> {
    let mut checkpoints = Vec::new();
    let mut aggregate_plan = None;
    let (root, freeze_revision) = if let Some((root, hash, raw, epoch, phase)) = &captured.set {
        let manifest: crate::partition_cutover::SetManifest = serde_json::from_str(raw)?;
        ensure!(
            phase == "activated"
                && manifest.plan.contract_version == 1
                && root == &manifest.plan.root_import_id
                && u64::try_from(*epoch)? == manifest.freeze_revision
                && catabolic_core::sha256(
                    catabolic_core::encode_with(&serde_json::to_value(&manifest)?, false, true)
                        .as_bytes()
                ) == *hash,
            "historical activation manifest mismatch"
        );
        identity
            .admit(&manifest.identity)
            .map_err(|e| anyhow::anyhow!("historical identity: {e:?}"))?;
        ensure!(
            !manifest.plan.partitions.is_empty()
                && manifest.plan.partitions.len()
                    <= catabolic_runtime::host::MAX_IMPORT_RESTART_PARTITIONS
                && captured.parts.len() == manifest.plan.partitions.len(),
            "historical activation partitions incomplete"
        );
        let mut ids = BTreeSet::new();
        for (index, ((id, ordinal, raw, envelope, phase), part)) in captured
            .parts
            .iter()
            .zip(&manifest.plan.partitions)
            .enumerate()
        {
            let cp: Checkpoint = serde_json::from_str(raw)?;
            ensure!(
                part.index as usize == index
                    && ids.insert(id.clone())
                    && part.dependency_indices == (0..part.index).collect::<Vec<_>>()
                    && id == &part.import_id
                    && *ordinal == part.index as i64
                    && phase == "activated"
                    && cp.valid()
                    && cp.phase == Phase::Activated
                    && cp.import_id == *id
                    && cp.input_digest == part.snapshot_digest
                    && cp.freeze_revision == manifest.freeze_revision
                    && cp.catalog_id == identity.catalog_id
                    && cp.incarnation == identity.incarnation,
                "historical partition checkpoint mismatch"
            );
            checkpoints.push((cp, envelope.clone()));
        }
        aggregate_plan = Some(manifest.plan);
        (root.clone(), manifest.freeze_revision)
    } else {
        ensure!(
            captured.parts.is_empty(),
            "unexpected historical activation parts"
        );
        let (id, raw, envelope, phase) = captured
            .single
            .as_ref()
            .context("historical activation missing")?;
        let cp: Checkpoint = serde_json::from_str(raw)?;
        ensure!(
            phase == "activated"
                && cp.valid()
                && cp.phase == Phase::Activated
                && cp.import_id == *id
                && cp.catalog_id == identity.catalog_id
                && cp.incarnation == identity.incarnation,
            "historical activation incomplete"
        );
        let root = cp.import_id.clone();
        let epoch = cp.freeze_revision;
        checkpoints.push((cp, envelope.clone()));
        (root, epoch)
    };
    Ok((checkpoints, aggregate_plan, root, freeze_revision))
}
fn prepare_entry(
    cp: &Checkpoint,
    snapshot: crate::migration::Snapshot,
    digest: String,
    audit_digest: String,
    raw: String,
    aggregate_plan: Option<&crate::motion_export::MotionExportPlan>,
) -> anyhow::Result<Entry> {
    ensure!(
        digest == cp.input_digest
            && snapshot
                .batches
                .iter()
                .all(|b| b.unresolved_evidence.is_empty()),
        "historical snapshot/checkpoint mismatch"
    );
    ensure!(audit_digest == digest, "historical audit input mismatch");
    if let Some(plan) = &aggregate_plan {
        ensure!(
            snapshot.manifest.source_snapshot_id == plan.source_snapshot_id
                && snapshot.manifest.source_namespace == plan.source_namespace
                && catabolic_core::sha256(raw.as_bytes()) == plan.audit_digest,
            "historical partition source or audit differs from activated root"
        );
    }
    Ok(Entry {
        checkpoint: cp.clone(),
        snapshot,
        audit_raw: raw,
    })
}
pub(crate) async fn prepare_in(db: &mut SqliteConnection) -> anyhow::Result<PreparedArchive> {
    budget(db).await?;
    let captured = control(db).await?;
    let identity_bytes: i64 = sqlx::query_scalar(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
    )
    .fetch_one(&mut *db)
    .await?;
    ensure!(
        (0..=65536).contains(&identity_bytes),
        "historical identity exceeds bound"
    );
    let raw: String = sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
        .fetch_one(&mut *db)
        .await?;
    ensure!(raw.len() <= 65536, "historical identity exceeds bound");
    let identity: Identity = serde_json::from_str(&raw)?;
    let (checkpoints, aggregate_plan, root, freeze_revision) =
        prepare_control(&captured, &identity)?;
    let mut entries = Vec::new();
    let mut total_bytes = 0i64;
    for (cp, _) in &checkpoints {
        let bytes: i64 = sqlx::query_scalar("SELECT length(CAST(s.snapshot_json AS BLOB))+length(CAST(s.input_digest AS BLOB))+length(CAST(a.audit_json AS BLOB))+length(CAST(a.input_digest AS BLOB))+COALESCE(length(CAST(e.input_digest AS BLOB)),0) FROM catabolic_import_snapshots s JOIN catabolic_reference_audits a USING(import_id) LEFT JOIN catabolic_motion_exports e USING(import_id) WHERE s.import_id=?").bind(&cp.import_id).fetch_one(&mut *db).await?;
        total_bytes = total_bytes
            .checked_add(bytes)
            .context("historical byte budget overflow")?;
        ensure!(
            bytes >= 0 && total_bytes <= 64 * 1024 * 1024,
            "historical frozen inventory exceeds byte bound"
        );
        let (snapshot, digest) =
            crate::motion_export::trusted_snapshot_in(db, &cp.import_id).await?;
        let (audit_digest, raw): (String, String) = sqlx::query_as(
            "SELECT input_digest,audit_json FROM catabolic_reference_audits WHERE import_id=?",
        )
        .bind(&cp.import_id)
        .fetch_one(&mut *db)
        .await?;
        entries.push(prepare_entry(
            cp,
            snapshot,
            digest,
            audit_digest,
            raw,
            aggregate_plan.as_ref(),
        )?);
    }
    Ok(PreparedArchive {
        control: captured,
        identity,
        root,
        freeze_revision,
        checkpoints,
        entries,
    })
}
impl PreparedArchive {
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }
    pub(crate) fn root_import_id(&self) -> &str {
        &self.root
    }
    pub(crate) fn root_manifest_digest(&self) -> Option<&str> {
        self.control.set.as_ref().map(|row| row.1.as_str())
    }
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
    /// Archive custody only. Permanent native activation proof remains separate.
    pub(crate) async fn verify_custody_in(
        &self,
        db: &mut SqliteConnection,
        host: &mut HostSession,
    ) -> anyhow::Result<()> {
        budget(db).await?;
        ensure!(
            control(db).await? == self.control,
            "historical activation changed during preparation"
        );
        let freeze = crate::source_freeze::read(db).await?;
        ensure!(
            matches!(freeze.mode, Mode::Sealed { .. })
                && freeze.active_freeze() == Some((self.root.as_str(), self.freeze_revision)),
            "historical source seal mismatch"
        );
        crate::source_freeze::verify(db).await?;
        let identity = host.identity()?;
        ensure!(
            identity["catalog_id"] == self.identity.catalog_id
                && identity["catalog_incarnation"] == self.identity.incarnation,
            "historical retained host mismatch"
        );
        Ok(())
    }
    pub(crate) fn activation_envelopes(&self) -> impl Iterator<Item = (&Checkpoint, &str)> {
        self.checkpoints
            .iter()
            .map(|(checkpoint, raw)| (checkpoint, raw.as_str()))
    }

    pub(crate) async fn verify_in(
        self,
        db: &mut SqliteConnection,
        host: &mut HostSession,
    ) -> anyhow::Result<VerifiedArchive> {
        self.verify_custody_in(db, host).await?;
        for (cp, raw) in &self.checkpoints {
            let command = crate::decode_envelope(raw)?;
            ensure!(
                command.key().operation_id()
                    == catabolic_store::catalog_import::operation_id(
                        &cp.import_id,
                        catabolic_store::catalog_import::ImportStep::Activate
                    )?,
                "historical activation operation mismatch"
            );
            let result = match host.reconcile_command_receipt(&command)? {
                Some(
                    CommandOutcome::Committed { result, .. }
                    | CommandOutcome::Replay { result, .. },
                ) => result,
                _ => anyhow::bail!("historical actual activation receipt unavailable"),
            };
            let receipt: ImportReceipt = serde_json::from_value(result)?;
            ensure!(
                catabolic_store::catalog_import::reconcile_activation(
                    Some(&receipt),
                    &crate::cutover::native_checkpoint(cp),
                    &self.identity.catalog_id,
                    &self.identity.incarnation
                ) == Reconciliation::AdmitCatabolicOnly,
                "historical activation receipt mismatch"
            );
        }
        Ok(VerifiedArchive { prepared: self })
    }
}
