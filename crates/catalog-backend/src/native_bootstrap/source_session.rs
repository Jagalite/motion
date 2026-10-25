//! Fixed96→94 transfer retaining the genuine completed native owner.
use super::*;
use catabolic_store::migration::offline_bootstrap::CompletedCatalogSession;
use std::sync::atomic::AtomicBool;

pub(crate) struct SourcePairProof {
    close_admission: Option<
        super::prepare_operation::apply_operation::bootstrap_operation::SourceCloseAdmission,
    >,
    intent: Intent,
    raw: String,
    command: CommandEnvelope,
    ack: String,
    allocation_provenance: std::sync::Mutex<SourceAllocationProvenance>,
    imported_archive: Option<TransferredImportedArchive>,
    // Fixed old Motion DBSTATUS category. It was already included in accepted
    // K at transfer, but not H. Later fresh arithmetic must add it alongside
    // the NEW SQLx writer's DBSTATUS, exactly once, without rereading old SQLite.
    retained_target_sqlite: u64,
    // Inherited H covers original preservation/publication and admitted frames.
    // No method queries/mutates the settled connection after transfer.
    _retained_bootstrap: Option<
        Box<super::prepare_operation::apply_operation::bootstrap_operation::RetainedBootstrapTail>,
    >,
}
// H is host-only retention. K is the combined native external high-water
// successfully established by this owner. They have different meanings even
// when both are numerically monotonic; K must never be added back into H.
struct SourceAllocationProvenance {
    host: u64,
    forwarded: u64,
}
impl SourceAllocationProvenance {
    fn new(host: u64, forwarded: u64) -> anyhow::Result<Self> {
        ensure!(
            host <= forwarded && forwarded <= 256 * 1024 * 1024,
            "source allocation transfer provenance differs"
        );
        Ok(Self { host, forwarded })
    }
    fn reconcile(&mut self, observed: u64) -> anyhow::Result<u64> {
        ensure!(
            observed >= self.forwarded && observed <= 256 * 1024 * 1024,
            "source allocation high-water changed unexpectedly"
        );
        if observed > self.forwarded {
            self.host = self.host.max(observed);
            self.forwarded = observed;
        }
        Ok(self.host)
    }
    // Called only AFTER the actual native owner accepts this reservation.
    fn record_success(&mut self, previous: u64, forwarded: u64) -> anyhow::Result<()> {
        ensure!(
            previous == self.forwarded
                && forwarded >= previous
                && forwarded >= self.host
                && forwarded <= 256 * 1024 * 1024,
            "source allocation forwarding provenance differs"
        );
        self.forwarded = forwarded;
        Ok(())
    }
}
/// Descriptive commitments from this exact retained96 owner, never a constructor
/// or replacement for verifying the actual writer and permanent journal rows.
#[derive(serde::Serialize)]
pub(crate) struct SourcePairCommitments {
    pub(crate) identity: Identity,
    pub(crate) transition_id: String,
    pub(crate) intent_sha256: String,
    pub(crate) command_sha256: String,
    pub(crate) acknowledgment_sha256: String,
    pub(crate) frozen_binding_sha256: String,
}
fn retained_command_bytes(command: &CommandEnvelope) -> anyhow::Result<u64> {
    // Dense payload nodes/capacities are independent of canonical text length.
    // Core strings expose slices only: use the native owner's 2x byte convention.
    let mut bytes = accounting::value_bytes(command.payload())?;
    for text in [
        command.canonical_payload(),
        command.key().catalog_id(),
        command.key().host_namespace(),
        command.key().operation_id(),
        command.admission_incarnation(),
        command.digest().as_str(),
    ] {
        let storage = u64::try_from(text.len())?
            .checked_mul(2)
            .context("source pair command allocation overflow")?;
        bytes = bytes
            .checked_add(storage)
            .context("source pair allocation overflow")?;
    }
    Ok(bytes)
}

impl SourcePairProof {
    pub(crate) fn has_retained_close_admission(&self) -> bool {
        self.close_admission.is_some()
    }
    #[cfg(test)]
    pub(crate) fn retained_bootstrap_slot_for_test(&self) -> (Option<usize>, usize) {
        (
            self._retained_bootstrap
                .as_ref()
                .map(|tail| tail.as_ref() as *const _ as usize),
            std::mem::size_of_val(&self._retained_bootstrap),
        )
    }
    #[cfg(test)]
    pub(crate) fn imported_archive_summary(&self) -> anyhow::Result<Option<(usize, u64)>> {
        self.imported_archive
            .as_ref()
            .map(|archive| Ok((archive.parts().len(), archive.retained_allocations()?)))
            .transpose()
    }
    /// Reconcile the actual native high-water with this owner's known forwards.
    /// An unknown increase is conservatively a whole external host claim, not
    /// an attributable delta or permission to subtract a prior category.
    pub(crate) fn capture_external_baseline(&self, observed: u64) -> anyhow::Result<u64> {
        self.allocation_provenance
            .lock()
            .map_err(|_| anyhow::anyhow!("source allocation provenance poisoned"))?
            .reconcile(observed)
    }
    pub(crate) fn record_allocation_forward(
        &self,
        previous: u64,
        forwarded: u64,
    ) -> anyhow::Result<()> {
        self.allocation_provenance
            .lock()
            .map_err(|_| anyhow::anyhow!("source allocation provenance poisoned"))?
            .record_success(previous, forwarded)
    }
    pub(crate) fn retain_external_claim(&self, claim: u64) -> anyhow::Result<()> {
        let mut state = self
            .allocation_provenance
            .lock()
            .map_err(|_| anyhow::anyhow!("source allocation provenance poisoned"))?;
        ensure!(
            claim <= 256 * 1024 * 1024,
            "source external claim exceeds allocation bound"
        );
        state.host = state.host.max(claim);
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn allocation_provenance(&self) -> (u64, u64) {
        let state = self.allocation_provenance.lock().unwrap();
        (state.host, state.forwarded)
    }

    pub(crate) fn seal_commitments_in(
        &self,
        db: &Connection,
    ) -> anyhow::Result<SourcePairCommitments> {
        self.verify_in(db)?;
        Ok(SourcePairCommitments {
            identity: self.intent.to.clone(),
            transition_id: self.intent.transition_id.clone(),
            intent_sha256: hash(&self.raw),
            command_sha256: hash(self.command.canonical_payload()),
            acknowledgment_sha256: hash(&self.ack),
            frozen_binding_sha256: self.intent.frozen_binding_sha256.clone(),
        })
    }
    pub(crate) fn retained_allocations(&self) -> anyhow::Result<u64> {
        self.retained_metadata_allocations()?
            .checked_add(self.retained_target_sqlite)
            .filter(|bytes| *bytes <= 256 * 1024 * 1024)
            .context("source settled target retention exceeds cohort")
    }
    #[cfg(test)]
    pub(crate) fn retained_tail_accounting_for_test(&self) -> anyhow::Result<(u64, u64)> {
        Ok((
            self.retained_target_sqlite,
            self.retained_metadata_allocations()?,
        ))
    }
    fn retained_metadata_allocations(&self) -> anyhow::Result<u64> {
        let mut bytes = accounting::value_bytes(&self.intent.plan)?
            .checked_add(retained_command_bytes(&self.command)?)
            .context("source pair allocation overflow")?;
        for additional in [
            self.raw.capacity() as u64,
            self.ack.capacity() as u64,
            64 * 1024,
        ] {
            bytes = bytes
                .checked_add(additional)
                .context("source pair allocation overflow")?;
        }
        if let Some(archive) = &self.imported_archive {
            bytes = bytes
                .checked_add(archive.retained_allocations()?)
                .context("source archive retention overflow")?;
        }
        ensure!(
            bytes <= 256 * 1024 * 1024,
            "source pair retention exceeds cohort"
        );
        Ok(bytes)
    }
    pub(crate) fn identity(&self) -> &Identity {
        &self.intent.to
    }
    pub(crate) fn verify_in(&self, db: &Connection) -> anyhow::Result<()> {
        Self::verify_retained_parts(db, &self.intent, &self.raw, &self.command, &self.ack)
    }
    pub(super) fn verify_retained_parts(
        db: &Connection,
        intent: &Intent,
        expected_raw: &str,
        command: &CommandEnvelope,
        ack: &str,
    ) -> anyhow::Result<()> {
        ensure!(!db.is_autocommit(), "source pair requires retained writer");
        ensure!(
            unsafe { rusqlite::ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) } == 2,
            "source pair requires actual WRITE state"
        );
        journal::verify(db)?;
        crate::source_freeze::verify_sync(db)?;
        let (_, raw) = saved_for_recovery(db, &intent.native_path, &intent.transition_id)?;
        ensure!(
            raw == expected_raw,
            "source pair current terminal intent differs"
        );
        ensure!(
            journal::record(db, "command", &intent.transition_id)?.as_deref()
                == Some(command.canonical_payload()),
            "source pair command differs"
        );
        ensure!(
            journal::record(db, "ack", &intent.transition_id)?.as_deref() == Some(ack),
            "source pair ACK differs"
        );
        Ok(())
    }
}
impl AcknowledgedNativeBootstrap {
    pub(crate) fn into_source_parts(
        mut self,
    ) -> anyhow::Result<(CompletedCatalogSession, SourcePairProof)> {
        if let Some(archive) = &self.imported_archive {
            archive.check()?;
        }
        let raw = journal::record(&self.db, "intent", &self.intent.transition_id)?
            .context("source pair intent missing")?;
        let ack = journal::record(&self.db, "ack", &self.intent.transition_id)?
            .context("source pair ACK missing")?;
        ensure!(
            hash(&ack) == self.ack_digest,
            "source pair ACK commitment differs"
        );
        let parsed: Acknowledgment = decode(&ack)?;
        validate_ack(&self.intent, &raw, &self.command, &parsed)?;
        let (external_host, forwarded_candidate) = self.allocation.source_pair_reservation();
        // A prior failed refresh may leave a computed candidate in Allocation.
        // Establish actual native acceptance before transferring provenance;
        // never relabel an attempted reservation as successfully forwarded.
        self.native.refresh_external_reservation(
            catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(
                forwarded_candidate,
            )?,
        )?;
        let successful_forwarded = forwarded_candidate;
        let mut proof = SourcePairProof {
            close_admission: None,
            retained_target_sqlite: 0,
            _retained_bootstrap: None,
            intent: self.intent,
            raw,
            command: self.command,
            ack,
            imported_archive: None,
            allocation_provenance: std::sync::Mutex::new(SourceAllocationProvenance::new(
                external_host,
                successful_forwarded,
            )?),
        };
        proof.verify_in(&self.db)?;
        // Genuine pristine Complete is consumed exactly once. Subsequent94
        // writes use current completed-owner admission, not old40 table equality.
        let mut native = self.native.into_completed_catalog(&proof.command)?;
        proof.verify_in(&self.db)?;
        if let Some(archive) = self.imported_archive {
            ensure!(
                native.external_retained_reservation()? == successful_forwarded,
                "source archive transfer has unknown native reservation"
            );
            let remaining = self
                .allocation
                .archive_validation_remaining(native.retained_allocations()?)?;
            proof.imported_archive =
                Some(archive.into_transferred(&self.db, &proof.intent.from, remaining)?);
            let reservation = self.allocation.reservation(
                &self.db,
                Some(&proof.intent.plan),
                Some(&proof.command),
            )?;
            native.refresh_external_reservation(reservation)?;
            let (_, forwarded) = self.allocation.source_pair_reservation();
            proof.record_allocation_forward(successful_forwarded, forwarded)?;
            proof.verify_in(&self.db)?;
        }
        self.db.execute_batch("ROLLBACK")?;
        drop(self.db);
        Ok((native, proof))
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;
    #[test]
    fn absent_retained_bootstrap_tail_is_pointer_sized() {
        // Tie the assertion to the actual proof field type, including the
        // legacy None case, rather than only measuring a lookalike type.
        fn assert_absent_slot<T>(_: fn(&SourcePairProof) -> &Option<Box<T>>) {
            let absent: Option<Box<T>> = None;
            assert_eq!(std::mem::size_of_val(&absent), std::mem::size_of::<usize>());
        }
        assert_absent_slot(|proof| &proof._retained_bootstrap);
    }
    #[test]
    fn known_forward_retry_preserves_host_provenance() {
        let mut state = SourceAllocationProvenance::new(1024, 8192).unwrap();
        assert_eq!(state.reconcile(8192).unwrap(), 1024);
        state.record_success(8192, 16384).unwrap();
        for _ in 0..3 {
            assert_eq!(state.reconcile(16384).unwrap(), 1024);
            state.record_success(16384, 16384).unwrap();
        }
        assert_eq!((state.host, state.forwarded), (1024, 16384));
    }
    #[test]
    fn unknown_external_claim_is_not_subtracted_or_reclassified() {
        let mut state = SourceAllocationProvenance::new(1024, 8192).unwrap();
        let unknown = 224 * 1024 * 1024;
        assert_eq!(state.reconcile(unknown).unwrap(), unknown);
        assert_eq!((state.host, state.forwarded), (unknown, unknown));
        state.record_success(unknown, unknown + 1024).unwrap();
        assert_eq!(state.reconcile(unknown + 1024).unwrap(), unknown);
        assert!(state.reconcile(unknown).is_err());
        assert_eq!((state.host, state.forwarded), (unknown, unknown + 1024));
    }
    #[test]
    fn unaccepted_candidates_never_advance_known_forwarding() {
        let mut state = SourceAllocationProvenance::new(1024, 8192).unwrap();
        // A computed candidate is not recorded before native success. A refusal
        // leaves this state unchanged; wrong/stale recording also refuses.
        let _unaccepted_candidate = 16384u64;
        assert_eq!(state.reconcile(8192).unwrap(), 1024);
        assert!(state.record_success(8191, 16384).is_err());
        assert!(state.record_success(8192, 8191).is_err());
        assert!(state.record_success(8192, u64::MAX).is_err());
        assert_eq!((state.host, state.forwarded), (1024, 8192));
        assert!(SourceAllocationProvenance::new(8193, 8192).is_err());
    }
    #[test]
    fn dense_command_counts_payload_nodes_separately_from_canonical_bytes() {
        let payload = serde_json::json!({"entries": vec![Value::Null; 128]});
        let command = CommandEnvelope::new(
            1,
            CommandKey::new("catalog", crate::HOST_NAMESPACE, "dense-accounting").unwrap(),
            "incarnation",
            0,
            &payload.to_string(),
        )
        .unwrap();
        let nodes = accounting::value_bytes(command.payload()).unwrap();
        let measured = retained_command_bytes(&command).unwrap();
        assert!(measured >= nodes + 2 * command.canonical_payload().len() as u64);
        assert!(measured > 4 * command.canonical_payload().len() as u64);
    }
}

/// Admission is produced only after the retained81 writer/proof/accounting
/// preflight; final construction consumes only actual prevalidated owned parts.
pub(crate) struct RetainedProofAdmission {
    provenance: SourceAllocationProvenance,
    retained_target_sqlite: u64,
}
impl RetainedProofAdmission {
    pub(super) fn new(
        host: u64,
        forwarded: u64,
        retained_target_sqlite: u64,
    ) -> anyhow::Result<Self> {
        ensure!(
            host.checked_add(retained_target_sqlite)
                .is_some_and(|known| known <= forwarded),
            "source settled target category exceeds accepted forwarding"
        );
        Ok(Self {
            provenance: SourceAllocationProvenance::new(host, forwarded)?,
            retained_target_sqlite,
        })
    }
    pub(super) fn finish(
        self,
        intent: Intent,
        raw: String,
        command: CommandEnvelope,
        ack: String,
        tail: Box<
            super::prepare_operation::apply_operation::bootstrap_operation::RetainedBootstrapTail,
        >,
        close_admission: super::prepare_operation::apply_operation::bootstrap_operation::SourceCloseAdmission,
    ) -> SourcePairProof {
        SourcePairProof {
            close_admission: Some(close_admission),
            intent,
            raw,
            command,
            ack,
            allocation_provenance: std::sync::Mutex::new(self.provenance),
            retained_target_sqlite: self.retained_target_sqlite,
            imported_archive: None,
            _retained_bootstrap: Some(tail),
        }
    }
}

// Only these fixed calls can reach the original control backing. A legacy
// source with no genuine retained tail keeps every ordinary full-charge API.
impl SourcePairProof {
    pub(crate) fn invalidate_source_workspace94(&mut self) {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.invalidate_source_workspace94();
        }
    }
    pub(crate) fn refresh_source_workspace94(&mut self, native: &mut CompletedCatalogSession,
        reference: &crate::offline_control_upgrade::UpgradeReference,
        witness: &crate::offline_control_upgrade::StageWitness, forwarded: u64,
    ) -> anyhow::Result<()> {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.refresh_source_workspace94(native, reference, witness, forwarded)
        } else {
            native.refresh_external_reservation(
                catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(forwarded)?)?;
            Ok(())
        }
    }
    pub(crate) fn refresh_image_workspace94(&mut self,
        native: &mut catabolic_store::migration::offline_bootstrap::CurrentCatalogImage,
        reference: &crate::offline_control_upgrade::UpgradeReference,
        witness: &crate::offline_control_upgrade::StageWitness, forwarded: u64,
    ) -> anyhow::Result<()> {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.refresh_image_workspace94(native, reference, witness, forwarded)
        } else {
            native.refresh_external_reservation(
                catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation::new(forwarded)?)?;
            Ok(())
        }
    }
    pub(crate) fn prepare_source_workspace94(&mut self, native: &mut CompletedCatalogSession,
        reference: &crate::offline_control_upgrade::UpgradeReference,
        witness: &crate::offline_control_upgrade::StageWitness,
        profile: &str, source: &str, path: &Path, cancel: &AtomicBool,
    ) -> crate::source_relocations::owned_operation::producer::RelocationProducer<
        catabolic_store::migration::offline_bootstrap::SourceRenewalPreparation, catabolic_store::Error,
    > {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.prepare_source_workspace94(native, reference, witness, profile, source, path, cancel)
        } else {
            crate::source_relocations::owned_operation::producer::RelocationProducer::ordinary(
                native.prepare_source_succession(profile, source, path, cancel))
        }
    }
    pub(crate) fn receipt_source_workspace94(&mut self, native: &CompletedCatalogSession,
        reference: &crate::offline_control_upgrade::UpgradeReference,
        witness: &crate::offline_control_upgrade::StageWitness, command: &CommandEnvelope,
        authority: catabolic_store::catalog_commands::TrustedCommandAuthority<'_>,
    ) -> crate::source_relocations::owned_operation::producer::RelocationProducer<
        Option<catabolic_store::migration::offline_bootstrap::SourceRenewalReceipt>, catabolic_store::Error,
    > {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.receipt_source_workspace94(native, reference, witness, command, authority)
        } else {
            crate::source_relocations::owned_operation::producer::RelocationProducer::ordinary(
                native.source_succession_receipt(command, authority))
        }
    }
    pub(crate) fn preflight_source_execute_workspace94(&mut self,
        reference: &crate::offline_control_upgrade::UpgradeReference,
        witness: &crate::offline_control_upgrade::StageWitness,
    ) -> anyhow::Result<()> {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.preflight_source_execute_workspace94(reference, witness)?;
        }
        Ok(())
    }
    pub(crate) fn execute_source_workspace94(&mut self, native: &mut CompletedCatalogSession,
        command: &CommandEnvelope, authority: catabolic_store::catalog_commands::TrustedCommandAuthority<'_>,
        policy: catabolic_store::catalog_commands::ReceiptPolicy, cancel: &AtomicBool,
    ) -> crate::source_relocations::owned_operation::producer::RelocationProducer<
        catabolic_store::catalog_commands::CommandOutcome, catabolic_store::catalog_commands::CommandStoreError,
    > {
        if let Some(tail) = self._retained_bootstrap.as_mut() {
            tail.execute_source_workspace94(native, command, authority, policy, cancel)
        } else {
            crate::source_relocations::owned_operation::producer::RelocationProducer::ordinary(
                native.execute_source_succession(command, authority, policy, cancel))
        }
    }
    #[cfg(test)]
    pub(crate) fn source_workspace_qualification94_for_test(&self) -> Option<bool> {
        self._retained_bootstrap.as_ref().and_then(|tail| tail.source_workspace_qualification94_for_test())
    }
    #[cfg(test)]
    pub(crate) fn enlarge_source_workspace94_for_test(&mut self) {
        self._retained_bootstrap.as_mut().unwrap().enlarge_source_workspace94_for_test();
    }
}

impl SourcePairProof {
    pub(crate) fn preflight_source_capture_workspace94(&mut self,
        reference: &crate::offline_control_upgrade::UpgradeReference,
        witness: &crate::offline_control_upgrade::StageWitness,
    ) -> anyhow::Result<()> {
        self._retained_bootstrap.as_mut().context("capture requires original control backing")?
            .preflight_source_capture_workspace94(reference, witness)
    }
    pub(crate) fn capture_source_workspace94(&mut self,
        operation: &mut catabolic_store::migration::offline_bootstrap::CurrentImageCaptureOperation,
        cancel: &AtomicBool,
    ) -> Result<catabolic_store::migration::offline_bootstrap::OfflineRetainedResult<()>,
        catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused>
    {
        self._retained_bootstrap.as_mut()
            .ok_or(catabolic_store::migration::offline_bootstrap::OfflineWorkspaceAdmissionRefused)?
            .capture_source_workspace94(operation, cancel)
    }
}
