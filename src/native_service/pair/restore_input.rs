//! Consumed private restore input, not a restored deployment. Both extracted
//! databases remain anonymous and independently inspected; no incarnation,
//! binding, journal or worker evidence is modified.
pub(super) mod selected;
mod target;
use super::*;
pub(in crate::native_service) use target::{RestoreProof, RestoreSupervisor};

pub(super) struct InspectedRestoreInput {
    motion: File,
    native: File,
    manifest: PairManifest,
    _motion_facts: motion_catalog_backend::motion_snapshot::image_read::InspectedImage,
    _native_facts: catabolic_store::catalog_backup::image_read::InspectedImage,
    archive: ArchiveCustody,
    current_native_facts: Option<catabolic_store::catalog_backup::image_read::InspectedImage>,
    // Fresh current authorization custody is distinct from historical selected
    // bytes. Declaration order releases extracted/archive evidence before these
    // genuine stopped-service/writer/native/IO/server-lock owners.
    current: PreparedPair,
}
enum ArchiveCustody {
    Published(crate::native_service::publication_file::Outcome),
    Selected(selected::SelectedArtifact),
}
impl InspectedRestoreInput {
    fn pair_sha256(&self) -> &str {
        match &self.archive {
            ArchiveCustody::Published(outcome) => &outcome.evidence().artifact.sha256,
            ArchiveCustody::Selected(artifact) => artifact.sha256(),
        }
    }
    fn archive_reservation(&self) -> u64 {
        match &self.archive {
            ArchiveCustody::Published(_) => 0,
            ArchiveCustody::Selected(_) => selected::RETAINED_METADATA * 2,
        }
    }
    fn verify_archive(&mut self, flag: &AtomicBool) -> anyhow::Result<()> {
        match &mut self.archive {
            ArchiveCustody::Published(outcome) => outcome.read_verified(|_| Ok(())),
            ArchiveCustody::Selected(artifact) => artifact.revalidate(flag),
        }
    }
    fn additional_archive_parents(&self) -> Vec<&File> {
        match &self.archive {
            ArchiveCustody::Published(_) => Vec::new(),
            ArchiveCustody::Selected(artifact) => artifact.parents().to_vec(),
        }
    }
}
#[cfg(test)]
impl InspectedRestoreInput {
    pub(super) fn manifest(&self) -> &PairManifest {
        &self.manifest
    }
}
impl PublishedPair {
    pub(super) async fn inspect_restore_input(
        self,
        flag: Arc<AtomicBool>,
    ) -> anyhow::Result<InspectedRestoreInput> {
        // Retain the entire published pair in the real blocking owner. Dropping
        // the waiter requests cancellation, but cannot release its live writer,
        // native export, stopped service or I/O/lock custody during inspection.
        let mut cancellation = PairCancellation(flag.clone(), true);
        let result =
            tokio::task::spawn_blocking(move || self.inspect_restore_input_blocking(&flag))
                .await
                .context("restore input inspection owner panicked")?;
        cancellation.1 = false;
        result
    }

    fn inspect_restore_input_blocking(
        mut self,
        flag: &AtomicBool,
    ) -> anyhow::Result<InspectedRestoreInput> {
        let held = self.held.as_mut().context("publication custody missing")?;
        held._pair._stopped.verify_lock()?;
        cancelled(flag)?;
        ensure!(
            identity(&held._pair.parent)? == held._pair.parent_identity,
            "restore staging parent changed"
        );
        // Connection allocations are measured under the retained writer. The
        // closed native owner cannot acquire new ordinary work during inspection.
        let reserved = held
            ._pair
            ._writer
            .retained_allocations_blocking()?
            .checked_add(held._pair.retained_native_allocations()?)
            .and_then(|n| n.checked_add(OVERHEAD))
            .context("restore input accounting overflow")?;
        let mut motion = anonymous(&held._pair.parent)?;
        let mut native = anonymous(&held._pair.parent)?;
        let manifest = held.publication.read_verified(|reader| {
            super::super::pair_container::extract(reader, &mut motion, &mut native, flag)
        })?;
        ensure!(
            manifest == held._pair.manifest,
            "restore input differs from retained pair commitments"
        );
        motion.sync_all()?;
        native.sync_all()?;
        let (motion_facts, native_facts) =
            inspect_images(&mut motion, &mut native, &manifest, reserved, flag)?;
        held._pair._stopped.verify_lock()?;
        cancelled(flag)?;
        ensure!(
            held.publication.phase() == playscale_core::paired_backup::publication::Phase::Durable,
            "restore input requires independently durable publication"
        );
        let PublishedCustody {
            publication,
            _pair: current,
        } = self.held.take().context("publication custody missing")?;
        Ok(InspectedRestoreInput {
            motion,
            native,
            manifest,
            _motion_facts: motion_facts,
            _native_facts: native_facts,
            archive: ArchiveCustody::Published(publication),
            current_native_facts: None,
            current,
        })
    }
}

fn inspect_images(
    motion: &mut File,
    native: &mut File,
    manifest: &PairManifest,
    reserved: u64,
    flag: &AtomicBool,
) -> anyhow::Result<(
    motion_catalog_backend::motion_snapshot::image_read::InspectedImage,
    catabolic_store::catalog_backup::image_read::InspectedImage,
)> {
    verify_file(motion, &manifest.motion_database, flag)?;
    verify_file(native, &manifest.native_database, flag)?;
    motion.seek(SeekFrom::Start(0))?;
    let motion_facts = motion_catalog_backend::motion_snapshot::image_read::inspect(
        &mut *motion,
        manifest.motion_database.bytes,
        reserved,
        flag,
    )?;
    native.seek(SeekFrom::Start(0))?;
    let native_facts = catabolic_store::catalog_backup::image_read::inspect(
        &mut *native,
        manifest.native_database.bytes,
        reserved,
        flag,
    )?;
    let actual_native = NativeIdentity {
        catalog_id: native_facts.catalog_id().into(),
        incarnation: native_facts.incarnation().into(),
        generation: u64::try_from(native_facts.generation())?,
        schema_version: native_facts.schema_version(),
        // Profile is an export selection context, not a database incarnation.
        // Its exact bytes are bound by the retained verified pair manifest.
        profile: manifest.native_identity.profile.clone(),
    };
    manifest
        .validate(&actual_native, motion_facts.binding().effective_identity())
        .map_err(|e| anyhow::anyhow!("independently inspected restore identity mismatch: {e:?}"))?;
    ensure!(
        motion_facts.bytes() == manifest.motion_database.bytes
            && motion_facts.sha256() == manifest.motion_database.sha256
            && motion_facts.schema_version() == manifest.motion_schema_version
            && motion_facts.inventory_sha256() == manifest.motion_control_journals_sha256
            && motion_facts.migration_ledger_sha256() == manifest.motion_migration_ledger_sha256
            && motion_facts.binding().evidence_sha256() == manifest.motion_binding_sha256,
        "independent Motion restore commitments mismatch"
    );
    ensure!(
        native_facts.bytes() == manifest.native_database.bytes
            && native_facts.sha256() == manifest.native_database.sha256
            && native_facts.inventory_sha256() == manifest.native_control_journals_sha256
            && native_facts.migration_ledger_sha256() == manifest.native_migration_ledger_sha256,
        "independent native restore commitments mismatch"
    );
    // Complete inventories retain all pending commands/receipts and worker
    // ownership. This reader does not infer resolved cleanup or redispatch.
    verify_file(motion, &manifest.motion_database, flag)?;
    verify_file(native, &manifest.native_database, flag)?;
    Ok((motion_facts, native_facts))
}
