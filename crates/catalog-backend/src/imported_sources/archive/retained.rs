//! Descriptive archive + immutable Motion publication bridge, never a native
//! grant. The fixed conversion phase must additionally bind the original93
//! image/control97 evidence. Native permanent input/history validation remains
//! separate from this host-owned publication baseline.
use super::*;
use catabolic_core::catalog_command::CommandEnvelope;
use rusqlite::{Connection, OptionalExtension};
use std::{sync::atomic::AtomicBool, time::Instant};
mod budget;
mod capacities;
#[cfg(test)]
mod tests;

pub(crate) struct ImportedLibraryBridge {
    import_id: String,
    item_id: String,
    catalog_id: String,
    incarnation: String,
    motion_base: i64,
    native_base: i64,
    proof_raw: String,
}
impl ImportedLibraryBridge {
    pub(crate) fn import_id(&self) -> &str {
        &self.import_id
    }
    pub(crate) fn item_id(&self) -> &str {
        &self.item_id
    }
    pub(crate) fn catalog_id(&self) -> &str {
        &self.catalog_id
    }
    pub(crate) fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub(crate) fn motion_base(&self) -> i64 {
        self.motion_base
    }
    pub(crate) fn native_base(&self) -> i64 {
        self.native_base
    }
    pub(crate) fn proof_raw(&self) -> &str {
        &self.proof_raw
    }
    fn same(&self, other: &Self) -> bool {
        self.import_id == other.import_id
            && self.item_id == other.item_id
            && self.catalog_id == other.catalog_id
            && self.incarnation == other.incarnation
            && self.motion_base == other.motion_base
            && self.native_base == other.native_base
            && self.proof_raw == other.proof_raw
    }
}
pub(crate) struct RetainedImportedArchive {
    archive: PreparedArchive,
    activations: Vec<CommandEnvelope>,
    bridges: Vec<ImportedLibraryBridge>,
    retained: u64,
    connection: usize,
}
impl RetainedImportedArchive {
    /// `remaining` is the aggregate allowance after all other retained owners.
    /// No optional/empty witness is synthesized when archive publication is absent.
    pub(crate) fn capture_in(
        db: &Connection,
        expected: &Identity,
        remaining: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Self> {
        crate::motion_snapshot::source_reader::verify_rusqlite_writer_main(db)?;
        expected
            .validate()
            .map_err(|e| anyhow::anyhow!("archive expected identity: {e:?}"))?;
        ensure!(
            expected.schema_version == 40,
            "archive requires original native40 identity"
        );
        let mut budget = budget::Budget::new(remaining, 0, deadline, cancel)?;
        preflight(db, &mut budget)?;
        crate::source_freeze::verify_sync(db)?;
        let identity: Identity = serde_json::from_str(&db.query_row::<String, _, _>(
            "SELECT identity_json FROM catabolic_binding WHERE id=1",
            [],
            |r| r.get(0),
        )?)?;
        ensure!(&identity == expected, "archive original identity differs");
        let captured = control_sync(db)?;
        let (checkpoints, aggregate_plan, root, freeze_revision) =
            prepare_control(&captured, &identity)?;
        let state: playscale_core::catalog_source::State =
            serde_json::from_str(&db.query_row::<String, _, _>(
                "SELECT state_json FROM catabolic_source_state WHERE singleton=1",
                [],
                |r| r.get(0),
            )?)?;
        ensure!(
            state.active_freeze() == Some((root.as_str(), freeze_revision))
                && matches!(state.mode, Mode::Sealed { .. }),
            "archive sealed publication differs"
        );
        let mut entries = Vec::with_capacity(checkpoints.len());
        let mut activations = Vec::with_capacity(checkpoints.len());
        for (cp, raw) in &checkpoints {
            budget.alive()?;
            let (digest,text):(String,String)=db.query_row("SELECT input_digest,snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",[&cp.import_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let provenance:Option<(String,i64)>=db.query_row("SELECT input_digest,format_version FROM catabolic_motion_exports WHERE import_id=?",[&cp.import_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let (snapshot, digest) = crate::motion_export::validate_trusted_snapshot(
                &cp.import_id,
                digest,
                &text,
                provenance,
            )?;
            let (audit_digest, audit): (String, String) = db.query_row(
                "SELECT input_digest,audit_json FROM catabolic_reference_audits WHERE import_id=?",
                [&cp.import_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            entries.push(prepare_entry(
                cp,
                snapshot,
                digest,
                audit_digest,
                audit,
                aggregate_plan.as_ref(),
            )?);
            let activation = crate::decode_envelope(raw)?;
            ensure!(
                activation.key().host_namespace() == crate::HOST_NAMESPACE
                    && activation.key().operation_id()
                        == catabolic_store::catalog_import::operation_id(
                            &cp.import_id,
                            catabolic_store::catalog_import::ImportStep::Activate
                        )?
                    && activation.admission_incarnation() == identity.incarnation,
                "archive activation envelope identity differs"
            );
            let expected_payload = serde_json::json!({"operation":"import","mutation":catabolic_store::catalog_import::ImportMutation::Activate {
                import_id: cp.import_id.clone(), input_digest: cp.input_digest.clone(),
                reference_ack_id: cp.reference_ack_id.clone(), mapping_digest: cp.mapping_digest.clone(),
            }});
            ensure!(
                activation.key().catalog_id() == identity.catalog_id
                    && activation.payload() == &expected_payload,
                "archive activation differs from exact checkpoint"
            );
            activations.push(activation);
        }
        let archive = PreparedArchive {
            control: captured,
            identity,
            root,
            freeze_revision,
            checkpoints,
            entries,
        };
        let bridges = capture_bridges(db, &archive, &budget)?;
        budget.alive()?;
        // Handle identity is retained only within the fixed same-connection phase;
        // it is not a reusable/reopen token. No raw connection is exported.
        let connection = unsafe { db.handle() } as usize;
        let owner = Self {
            archive,
            activations,
            bridges,
            retained: budget.retained(),
            connection,
        };
        owner.retained_allocations()?;
        Ok(owner)
    }
    pub(crate) fn parts(
        &self,
    ) -> impl ExactSizeIterator<Item = catabolic_store::migration::offline_bootstrap::HostImportPart<'_>>
    {
        self.archive
            .entries
            .iter()
            .zip(&self.activations)
            .map(|(entry, activation)| {
                catabolic_store::migration::offline_bootstrap::HostImportPart {
                    manifest: &entry.snapshot.manifest,
                    batches: &entry.snapshot.batches,
                    activation,
                }
            })
    }
    pub(crate) fn library_bridge(&self, item: &str) -> anyhow::Result<&ImportedLibraryBridge> {
        self.bridges
            .iter()
            .find(|bridge| bridge.item_id == item)
            .context("authenticated imported library bridge unavailable")
    }
    pub(crate) fn retained_allocations(&self) -> anyhow::Result<u64> {
        // Checked observable capacities must fit the admitted immutable-model
        // reserve; unknown private nested capacities retain the node convention.
        ensure!(
            capacities::observed(self)? <= self.retained,
            "archive observed capacity exceeds admitted retention"
        );
        Ok(self.retained)
    }
    /// Admission includes BOTH this retained witness and the complete reread.
    /// Caller supplies remaining before this witness, not a fresh second budget.
    pub(crate) fn verify_in(
        &self,
        db: &Connection,
        expected: &Identity,
        remaining: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            unsafe { db.handle() } as usize == self.connection,
            "archive writer owner changed"
        );
        self.verify_contents_in(db, expected, remaining, deadline, cancel)
    }
    /// Final exact original-connection verification precedes relinquishing that
    /// connection. The returned descriptive owner can only be reread through
    /// an admitted same-stage/current-pair source writer, never arbitrary reopen.
    pub(crate) fn into_transferred(
        self,
        db: &Connection,
        expected: &Identity,
        remaining: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<TransferredImportedArchive> {
        self.verify_in(db, expected, remaining, deadline, cancel)?;
        Ok(TransferredImportedArchive { original: self })
    }
    fn verify_contents_in(
        &self,
        db: &Connection,
        expected: &Identity,
        remaining: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let available = remaining
            .checked_sub(self.retained_allocations()?)
            .context("archive simultaneous retention exceeded")?;
        let other = Self::capture_in(db, expected, available, deadline, cancel)?;
        ensure!(
            self.archive.control == other.archive.control
                && self.archive.identity == other.archive.identity
                && self.archive.root == other.archive.root
                && self.archive.freeze_revision == other.archive.freeze_revision
                && self.archive.entries.len() == other.archive.entries.len()
                && self.bridges.len() == other.bridges.len(),
            "archive publication changed"
        );
        for (left, right) in self.archive.entries.iter().zip(&other.archive.entries) {
            ensure!(
                left.checkpoint.input_digest == right.checkpoint.input_digest
                    && left.audit_raw == right.audit_raw,
                "archive inputs changed"
            );
        }
        ensure!(
            self.bridges
                .iter()
                .zip(&other.bridges)
                .all(|(a, b)| a.same(b)),
            "immutable library bridge changed"
        );
        ensure!(
            Instant::now() < deadline && !cancel.load(std::sync::atomic::Ordering::Acquire),
            "archive verification interrupted"
        );
        Ok(())
    }
}
/// Moved, never reconstructed from serialized fields. Host baseline evidence
/// remains distinct from independently verified native permanent import inputs.
pub(crate) struct TransferredImportedArchive {
    original: RetainedImportedArchive,
}
impl TransferredImportedArchive {
    pub(crate) fn retained_allocations(&self) -> anyhow::Result<u64> {
        self.original.retained_allocations()
    }
    pub(crate) fn parts(
        &self,
    ) -> impl ExactSizeIterator<Item = catabolic_store::migration::offline_bootstrap::HostImportPart<'_>>
    {
        self.original.parts()
    }
    pub(crate) fn library_bridge(&self, item: &str) -> anyhow::Result<&ImportedLibraryBridge> {
        self.original.library_bridge(item)
    }
    pub(crate) fn verify_source_in(
        &self,
        db: &Connection,
        admission: &crate::offline_control_upgrade::AdmittedSourceArchiveRead<'_>,
        remaining: u64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let original_identity = admission.validate_in(db)?;
        self.original
            .verify_contents_in(db, original_identity, remaining, deadline, cancel)?;
        admission.validate_in(db)?;
        Ok(())
    }
}

fn control_sync(db: &Connection) -> anyhow::Result<Control> {
    Ok(Control{
        single:db.query_row("SELECT import_id,checkpoint_json,activation_envelope,phase FROM catabolic_cutover WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?,
        set:db.query_row("SELECT root_import_id,manifest_digest,manifest_json,freeze_revision,phase FROM catabolic_cutover_sets WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?,
        parts:db.prepare("SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts ORDER BY ordinal")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?.collect::<rusqlite::Result<_>>()?,
    })
}
fn capture_bridges(
    db: &Connection,
    archive: &PreparedArchive,
    budget: &budget::Budget<'_>,
) -> anyhow::Result<Vec<ImportedLibraryBridge>> {
    let mut bridges = Vec::new();
    for entry in &archive.entries {
        for metadata in entry
            .snapshot
            .batches
            .iter()
            .filter_map(|b| b.typed.as_ref())
            .flat_map(|b| &b.metadata)
        {
            for contribution in &metadata.contributions {
                budget.alive()?;
                if contribution.namespace != "motion.structure" {
                    continue;
                }
                let Some(library) = contribution.fields.get("motion_library") else {
                    continue;
                };
                ensure!(
                    library["contract_version"] == 1,
                    "unsupported archived library definition"
                );
                let id = library["id"]
                    .as_str()
                    .context("archived library id unavailable")?;
                ensure!(
                    metadata.item_id.strip_prefix("motion.library:") == Some(id),
                    "archived library item differs"
                );
                let base = library["catalog_revision"]
                    .as_i64()
                    .filter(|n| *n >= 1)
                    .context("archived library revision unavailable")?;
                ensure!(
                    !bridges
                        .iter()
                        .any(|b: &ImportedLibraryBridge| b.item_id == metadata.item_id),
                    "ambiguous archived library producer"
                );
                let (catalog,inc,motion,native,raw):(String,String,i64,i64,String)=db.query_row("SELECT catalog_id,incarnation,motion_base,native_base,proof_json FROM catabolic_revision_bridges WHERE native_id=?",[&metadata.item_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.context("immutable imported library bridge missing")?;
                let proof: Value = serde_json::from_str(&raw)?;
                ensure!(
                    catalog == archive.identity.catalog_id
                        && inc == archive.identity.incarnation
                        && motion == base
                        && native >= 1
                        && proof
                            == serde_json::json!({"kind":"native_import_activation","import_id":entry.checkpoint.import_id,"native_base":native}),
                    "immutable imported library bridge differs"
                );
                bridges.push(ImportedLibraryBridge {
                    import_id: entry.checkpoint.import_id.clone(),
                    item_id: metadata.item_id.clone(),
                    catalog_id: catalog,
                    incarnation: inc,
                    motion_base: motion,
                    native_base: native,
                    proof_raw: raw,
                });
            }
        }
    }
    Ok(bridges)
}
fn preflight(db: &Connection, budget: &mut budget::Budget<'_>) -> anyhow::Result<()> {
    // Fixed complete inputs, including guard reconstruction BEFORE verify_sync.
    // Extra archived rows consume admission but never establish producer authority.
    for (sql, json) in [
        (
            "SELECT identity_json FROM catabolic_binding LIMIT 16385",
            &[0][..],
        ),
        (
            "SELECT revision,writes_allowed,state_json FROM catabolic_source_state LIMIT 16385",
            &[2][..],
        ),
        (
            "SELECT name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' LIMIT 16385",
            &[][..],
        ),
        (
            "SELECT trigger_name,table_name,trigger_sql FROM catabolic_source_guards LIMIT 16385",
            &[][..],
        ),
        (
            "SELECT import_id,checkpoint_json,activation_envelope,phase FROM catabolic_cutover LIMIT 16385",
            &[1, 2][..],
        ),
        (
            "SELECT root_import_id,manifest_digest,manifest_json,freeze_revision,phase FROM catabolic_cutover_sets LIMIT 16385",
            &[2][..],
        ),
        (
            "SELECT import_id,ordinal,checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts LIMIT 16385",
            &[2, 3][..],
        ),
        (
            "SELECT import_id,input_digest,snapshot_json FROM catabolic_import_snapshots LIMIT 16385",
            &[2][..],
        ),
        (
            "SELECT import_id,input_digest,audit_json FROM catabolic_reference_audits LIMIT 16385",
            &[][..],
        ),
        (
            "SELECT import_id,input_digest,format_version FROM catabolic_motion_exports LIMIT 16385",
            &[][..],
        ),
        (
            "SELECT native_id,catalog_id,incarnation,motion_base,native_base,proof_json FROM catabolic_revision_bridges LIMIT 16385",
            &[5][..],
        ),
    ] {
        let retained = !sql.contains("sqlite_schema")
            && !sql.contains("catabolic_source_guards")
            && !sql.contains("catabolic_source_state");
        budget.scan(db, sql, json, retained)?;
    }
    Ok(())
}
