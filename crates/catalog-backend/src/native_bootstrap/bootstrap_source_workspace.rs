//! Fixed source94 operations borrowing the actual retained bootstrap tail.
//! The tail's old SQLite and independently guarded outputs are never visited.
use super::*;
use crate::offline_control_upgrade::{StageWitness, UpgradeReference};
use crate::source_relocations::owned_operation::producer::RelocationProducer;
use catabolic_store::{
    catalog_commands::{CommandStoreError, TrustedCommandAuthority},
    migration::offline_bootstrap::{CompletedCatalogSession, SourceRenewalPreparation, SourceRenewalReceipt},
};

impl RetainedBootstrapTail {
    pub(crate) fn invalidate_source_workspace94(&mut self) {
        self.source_execute_qualification = None;
        self.source_capture_qualification = None;
    }
    pub(crate) fn refresh_source_workspace94(&mut self, native: &mut CompletedCatalogSession,
        reference: &UpgradeReference, witness: &StageWitness, forwarded: u64,
    ) -> anyhow::Result<()> {
        let owner = self.owner.as_mut().context("source bootstrap tail owner missing")?;
        // Refresh only changes Native's external reservation and does not mutate
        // any split-P owner. Its own fresh purpose-bound proof is consumed now;
        // a pending Execute certificate survives all read-only host scans/awaits.
        owner.ready.operation.ready.operation.control.refresh_source_workspace94(
            reference, witness, &owner.binding, native, forwarded)
    }
    pub(crate) fn refresh_image_workspace94(&mut self,
        native: &mut catabolic_store::migration::offline_bootstrap::CurrentCatalogImage,
        reference: &UpgradeReference, witness: &StageWitness, forwarded: u64,
    ) -> anyhow::Result<()> {
        let owner = self.owner.as_mut().context("source bootstrap tail owner missing")?;
        owner.ready.operation.ready.operation.control.refresh_image_workspace94(
            reference, witness, &owner.binding, native, forwarded)
    }
    pub(crate) fn prepare_source_workspace94(&mut self, native: &mut CompletedCatalogSession,
        reference: &UpgradeReference, witness: &StageWitness, profile: &str, source: &str,
        path: &Path, cancel: &AtomicBool,
    ) -> RelocationProducer<SourceRenewalPreparation, catabolic_store::Error> {
        self.invalidate_source_workspace94();
        let Some(owner) = self.owner.as_mut() else {
            return RelocationProducer::admission(anyhow::anyhow!("source bootstrap tail owner missing"));
        };
        owner.ready.operation.ready.operation.control.prepare_source_workspace94(
            reference, witness, &owner.binding, native, profile, source, path, cancel)
    }
    pub(crate) fn receipt_source_workspace94(&mut self, native: &CompletedCatalogSession,
        reference: &UpgradeReference, witness: &StageWitness, command: &CommandEnvelope,
        authority: TrustedCommandAuthority<'_>,
    ) -> RelocationProducer<Option<SourceRenewalReceipt>, catabolic_store::Error> {
        self.invalidate_source_workspace94();
        let Some(owner) = self.owner.as_mut() else {
            return RelocationProducer::admission(anyhow::anyhow!("source bootstrap tail owner missing"));
        };
        owner.ready.operation.ready.operation.control.receipt_source_workspace94(
            reference, witness, &owner.binding, native, command, authority)
    }
    pub(crate) fn preflight_source_execute_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness,
    ) -> anyhow::Result<()> {
        self.invalidate_source_workspace94();
        let owner = self.owner.as_mut().context("source bootstrap tail owner missing")?;
        let qualification = owner.ready.operation.ready.operation.control
            .preflight_source_execute_workspace94(reference, witness, &owner.binding)?;
        self.source_execute_qualification = Some(qualification);
        Ok(())
    }
    pub(crate) fn execute_source_workspace94(&mut self, native: &mut CompletedCatalogSession,
        command: &CommandEnvelope, authority: TrustedCommandAuthority<'_>,
        policy: ReceiptPolicy, cancel: &AtomicBool,
    ) -> RelocationProducer<CommandOutcome, CommandStoreError> {
        // Take BEFORE every fallible check/Native entry. No retry, unwind or
        // producer error can leave a consumable proof, and no capacity is walked.
        let Some(qualification) = self.source_execute_qualification.take() else {
            return RelocationProducer::admission(anyhow::anyhow!("source execute workspace preflight missing"));
        };
        let Some(owner) = self.owner.as_mut() else {
            return RelocationProducer::admission(anyhow::anyhow!("source bootstrap tail owner missing"));
        };
        owner.ready.operation.ready.operation.control.execute_source_workspace94(
            qualification, native, command, authority, policy, cancel)
    }
    #[cfg(test)]
    pub(crate) fn source_workspace_qualification94_for_test(&self) -> Option<bool> {
        self.source_execute_qualification.as_ref().map(|q| q.eligible_for_test())
    }
    #[cfg(test)]
    pub(crate) fn enlarge_source_workspace94_for_test(&mut self) {
        self.invalidate_source_workspace94();
        self.owner.as_mut().unwrap().ready.operation.ready.operation.control
            .enlarge_source_workspace94_for_test();
    }
}

impl RetainedBootstrapTail {
    pub(crate) fn preflight_source_capture_workspace94(&mut self, reference: &UpgradeReference,
        witness: &StageWitness,
    ) -> anyhow::Result<()> {
        self.invalidate_source_workspace94();
        let owner = self.owner.as_mut().context("source bootstrap tail owner missing")?;
        let qualification = owner.ready.operation.ready.operation.control
            .preflight_source_capture_workspace94(reference, witness, &owner.binding)?;
        // The original RecordedControlOperation remains at the SAME address in
        // this pre-existing Box. Moving Stage Inputs into Parts moves this Box's
        // pointer, not its contents. Host authorization only reads other owners;
        // no split-P field is mutated until this certificate is consumed.
        self.source_capture_qualification = Some(qualification);
        Ok(())
    }
    pub(crate) fn capture_source_workspace94(&mut self,
        operation: &mut catabolic_store::migration::offline_bootstrap::CurrentImageCaptureOperation,
        cancel: &AtomicBool,
    ) -> Result<catabolic_store::migration::offline_bootstrap::OfflineRetainedResult<()>,
        catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused>
    {
        use catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused;
        let qualification = self.source_capture_qualification.take().ok_or(OfflineWorkspaceAdmissionRefused)?;
        let owner = self.owner.as_mut().ok_or(OfflineWorkspaceAdmissionRefused)?;
        owner.ready.operation.ready.operation.control.capture_source_workspace94(
            qualification, operation, cancel)
    }
}
