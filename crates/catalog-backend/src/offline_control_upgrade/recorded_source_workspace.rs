//! Fresh source94 continuation of the actual original control64 allocation.
//! Settlement split P between this owner, the source witness/reference, and the
//! bootstrap binding. Never consult the old database or reuse Ready97's proof.
use super::*;
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::{
    catalog_commands::{CommandOutcome, CommandStoreError, ReceiptPolicy, TrustedCommandAuthority},
    migration::offline_bootstrap::{CompletedCatalogSession, OfflineAllocationReservation,
        SourceRenewalPreparation, SourceRenewalReceipt},
};
use crate::source_relocations::owned_operation::producer::RelocationProducer;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceWorkspacePurpose { Refresh, ImageRefresh, Prepare, Receipt, Execute, Capture }

/// No clone, public constructor, allowance or backing getter. The actual tail
/// owns this value; its owner identity is an additional check, not authority.
pub(crate) struct SourceWorkspaceQualification {
    owner: usize,
    purpose: SourceWorkspacePurpose,
    eligible: bool,
}
impl SourceWorkspaceQualification {
    #[cfg(test)]
    pub(crate) fn eligible_for_test(&self) -> bool { self.eligible }
}

fn add(total: &mut u64, bytes: u64) -> anyhow::Result<()> {
    *total = total.checked_add(bytes).context("source control workspace capacity overflow")?;
    Ok(())
}
fn settled(operation: &RecordedControlOperation) -> anyhow::Result<()> {
    ensure!(operation.phase == RecordedControlPhase::Settled
        && operation.prepared.is_none() && operation.workspace_qualification.is_none(),
        "source control workspace requires extracted Settled owner");
    ensure!(operation.db.is_some() && operation.directory.is_some()
        && operation.control_workspace.is_some(), "source control workspace custody missing");
    Ok(())
}
fn measure(
    operation: &RecordedControlOperation,
    reference: &UpgradeReference,
    witness: &StageWitness,
    binding: &String,
    purpose: SourceWorkspacePurpose,
) -> anyhow::Result<SourceWorkspaceQualification> {
    settled(operation)?;
    // Exactly the former recorded_workspace::retained cohort, at its current
    // owners. Inline headers are conservatively included even where separately
    // admitted. SQLx readers, SQLite DBSTATUS, bootstrap outputs, publication H
    // and every external/service claim stay fully charged and are NOT lent.
    let mut bytes = std::mem::size_of::<RecordedControlOperation>() as u64;
    add(&mut bytes, std::mem::size_of::<PreparedRecordedControlUpgrade>() as u64)?;
    add(&mut bytes, std::mem::size_of::<SourceControlWorkspaceUse<'_>>() as u64)?;
    add(&mut bytes, std::mem::size_of::<SourceWorkspaceQualification>() as u64)?;
    add(&mut bytes, reference.0.retained_allocations()?)?;
    add(&mut bytes, operation.original.as_ref()
        .context("source original preservation missing")?.retained_allocations()?)?;
    add(&mut bytes, operation.before.as_ref()
        .context("source original before rows missing")?.retained_allocations()?)?;
    add(&mut bytes, operation.progress.additions.retained_allocations()?)?;
    add(&mut bytes, binding.capacity() as u64)?;
    add(&mut bytes, witness.path.capacity() as u64)?;
    add(&mut bytes, witness.initial_sha.capacity() as u64)?;
    let anchor = operation.anchor.as_ref().context("source original anchor missing")?;
    for text in [&anchor.operation_id, &anchor.selected_pair_sha256,
        &anchor.original93_image_sha256, &anchor.original93_inventory_sha256,
        &anchor.frozen_binding_sha256] {
        add(&mut bytes, text.capacity() as u64)?;
    }
    let publication = operation.progress.publication.as_ref()
        .context("source control publication missing")?;
    ensure!(publication.phase() == "durable evidence transferred",
        "source control publication is not transferred");
    // DurableRecord and its conservative two verification buffers moved with
    // the witness, not into the old tail's now-empty prepared slot.
    add(&mut bytes, witness.record.as_ref()
        .context("source control durable record missing")?.retained_and_verification_bytes()?)?;
    Ok(SourceWorkspaceQualification {
        owner: operation as *const RecordedControlOperation as usize,
        purpose,
        eligible: bytes <= OfflineControlWorkspaceBacking::RETAINED_LIMIT,
    })
}

enum SourceControlWorkspaceUse<'a> {
    Qualified(&'a mut RecordedControlOperation),
    FullCharge(&'a mut RecordedControlOperation),
}
impl<'a> SourceControlWorkspaceUse<'a> {
    fn borrow(
        operation: &'a mut RecordedControlOperation,
        qualification: SourceWorkspaceQualification,
        purpose: SourceWorkspacePurpose,
    ) -> anyhow::Result<Self> {
        // Constant-time consume only. In particular Execute performs no P walk
        // after the application's final genuine current+target temporal fence.
        settled(operation)?;
        ensure!(qualification.owner == operation as *const RecordedControlOperation as usize
            && qualification.purpose == purpose, "source control workspace owner or purpose differs");
        Ok(if qualification.eligible { Self::Qualified(operation) } else { Self::FullCharge(operation) })
    }
    fn refresh(self, native: &mut CompletedCatalogSession, forwarded: u64) -> anyhow::Result<()> {
        match self {
            Self::Qualified(operation) => native.refresh_external_reservation_with_control_workspace(
                forwarded, operation.control_workspace.as_mut().unwrap())?,
            Self::FullCharge(_operation) => native.refresh_external_reservation(
                OfflineAllocationReservation::new(forwarded)?)?,
        }
        Ok(())
    }
    fn prepare(self, native: &mut CompletedCatalogSession, profile: &str, source: &str,
        path: &Path, cancel: &AtomicBool,
    ) -> RelocationProducer<SourceRenewalPreparation, catabolic_store::Error> {
        match self {
            Self::Qualified(operation) => RelocationProducer::workspace(
                native.prepare_source_succession_with_control_workspace(profile, source, path,
                    cancel, operation.control_workspace.as_mut().unwrap())),
            Self::FullCharge(_operation) => RelocationProducer::ordinary(
                native.prepare_source_succession(profile, source, path, cancel)),
        }
    }
    fn receipt(self, native: &CompletedCatalogSession, command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>,
    ) -> RelocationProducer<Option<SourceRenewalReceipt>, catabolic_store::Error> {
        match self {
            Self::Qualified(operation) => RelocationProducer::workspace(
                native.source_succession_receipt_with_control_workspace(command, authority,
                    operation.control_workspace.as_mut().unwrap())),
            Self::FullCharge(_operation) => RelocationProducer::ordinary(
                native.source_succession_receipt(command, authority)),
        }
    }
    fn execute(self, native: &mut CompletedCatalogSession, command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>, policy: ReceiptPolicy, cancel: &AtomicBool,
    ) -> RelocationProducer<CommandOutcome, CommandStoreError> {
        match self {
            Self::Qualified(operation) => RelocationProducer::workspace(
                native.execute_source_succession_with_control_workspace(command, authority, policy,
                    cancel, operation.control_workspace.as_mut().unwrap())),
            Self::FullCharge(_operation) => RelocationProducer::ordinary(
                native.execute_source_succession(command, authority, policy, cancel)),
        }
    }
}

impl RecordedAdmittedControl {
    pub(crate) fn refresh_source_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness, binding: &String, native: &mut CompletedCatalogSession, forwarded: u64,
    ) -> anyhow::Result<()> {
        let qualification = measure(&self.operation, reference, witness, binding, SourceWorkspacePurpose::Refresh)?;
        SourceControlWorkspaceUse::borrow(&mut self.operation, qualification, SourceWorkspacePurpose::Refresh)?
            .refresh(native, forwarded)
    }
    pub(crate) fn refresh_image_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness, binding: &String,
        native: &mut catabolic_store::migration::offline_bootstrap::CurrentCatalogImage, forwarded: u64,
    ) -> anyhow::Result<()> {
        let qualification = measure(&self.operation, reference, witness, binding, SourceWorkspacePurpose::ImageRefresh)?;
        match SourceControlWorkspaceUse::borrow(&mut self.operation, qualification, SourceWorkspacePurpose::ImageRefresh)? {
            SourceControlWorkspaceUse::Qualified(operation) => native.refresh_external_reservation_with_control_workspace(
                forwarded, operation.control_workspace.as_mut().unwrap())?,
            SourceControlWorkspaceUse::FullCharge(_) => native.refresh_external_reservation(
                OfflineAllocationReservation::new(forwarded)?)?,
        }
        Ok(())
    }
    pub(crate) fn prepare_source_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness, binding: &String, native: &mut CompletedCatalogSession,
        profile: &str, source: &str, path: &Path, cancel: &AtomicBool,
    ) -> RelocationProducer<SourceRenewalPreparation, catabolic_store::Error> {
        let qualification = match measure(&self.operation, reference, witness, binding, SourceWorkspacePurpose::Prepare) {
            Ok(qualification) => qualification,
            Err(error) => return RelocationProducer::admission(error),
        };
        match SourceControlWorkspaceUse::borrow(&mut self.operation, qualification, SourceWorkspacePurpose::Prepare) {
            Ok(workspace) => workspace.prepare(native, profile, source, path, cancel),
            Err(error) => RelocationProducer::admission(error),
        }
    }
    pub(crate) fn receipt_source_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness, binding: &String, native: &CompletedCatalogSession,
        command: &CommandEnvelope, authority: TrustedCommandAuthority<'_>,
    ) -> RelocationProducer<Option<SourceRenewalReceipt>, catabolic_store::Error> {
        let qualification = match measure(&self.operation, reference, witness, binding, SourceWorkspacePurpose::Receipt) {
            Ok(qualification) => qualification,
            Err(error) => return RelocationProducer::admission(error),
        };
        match SourceControlWorkspaceUse::borrow(&mut self.operation, qualification, SourceWorkspacePurpose::Receipt) {
            Ok(workspace) => workspace.receipt(native, command, authority),
            Err(error) => RelocationProducer::admission(error),
        }
    }
    pub(crate) fn preflight_source_execute_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness, binding: &String,
    ) -> anyhow::Result<SourceWorkspaceQualification> {
        measure(&self.operation, reference, witness, binding, SourceWorkspacePurpose::Execute)
    }
    pub(crate) fn execute_source_workspace94(&mut self, qualification: SourceWorkspaceQualification,
        native: &mut CompletedCatalogSession, command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>, policy: ReceiptPolicy, cancel: &AtomicBool,
    ) -> RelocationProducer<CommandOutcome, CommandStoreError> {
        match SourceControlWorkspaceUse::borrow(&mut self.operation, qualification, SourceWorkspacePurpose::Execute) {
            Ok(workspace) => workspace.execute(native, command, authority, policy, cancel),
            Err(error) => RelocationProducer::admission(error),
        }
    }
    #[cfg(test)]
    pub(crate) fn enlarge_source_workspace94_for_test(&mut self) {
        self.operation.anchor.as_mut().unwrap().operation_id
            .reserve(OfflineControlWorkspaceBacking::RETAINED_LIMIT as usize);
    }
}

impl RecordedAdmittedControl {
    pub(crate) fn preflight_source_capture_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness, binding: &String,
    ) -> anyhow::Result<SourceWorkspaceQualification> {
        measure(&self.operation, reference, witness, binding, SourceWorkspacePurpose::Capture)
    }
    pub(crate) fn capture_source_workspace94(&mut self, qualification: SourceWorkspaceQualification,
        operation: &mut catabolic_store::migration::offline_bootstrap::CurrentImageCaptureOperation,
        cancel: &AtomicBool,
    ) -> Result<catabolic_store::migration::offline_bootstrap::OfflineRetainedResult<()>,
        catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused>
    {
        use catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused;
        // Fixed final seam: consume the measured purpose certificate, check only
        // immutable owner identity/phase, then open the original exclusive loan.
        // No P traversal, allocation construction, refresh or callback occurs.
        let workspace = SourceControlWorkspaceUse::borrow(&mut self.operation,
            qualification, SourceWorkspacePurpose::Capture)
            .map_err(|_| OfflineWorkspaceAdmissionRefused)?;
        match workspace {
            SourceControlWorkspaceUse::Qualified(owner) => {
                let loan = operation.borrow_control_workspace(owner.control_workspace.as_mut().unwrap())?;
                Ok(operation.capture_with_control_workspace(cancel, loan))
            }
            SourceControlWorkspaceUse::FullCharge(_) => Err(OfflineWorkspaceAdmissionRefused),
        }
    }
}
