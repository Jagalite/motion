//! Finite positive94 target-close checkpoint. No source move, new writer,
//! capture authorization, native capture, seal, reservation or95 transition.
use super::*;
use std::future::Future;

impl OwnedSourceRelocation94 {
    /// Eligibility only, not an authorization grant. Ready94 is established by
    /// normal completion AND explicit retirement of the last94 authorization.
    /// A terminally settled source/target cannot re-enter through this query.
    pub fn capture_handoff_ready(&self) -> bool {
        self.phase == SourceRelocation94Phase::Ready94
            && self.active.is_none()
            && self.completed
            && self.capture_history_complete()
            && self.close.is_none()
            && self.source.as_ref().is_some_and(|source| source.inner.source_close_admitted94())
            && self.target.is_some()
    }

    /// Historical checkpoint, not permission to capture or extract the source.
    /// A missing target/source is never positive evidence of retained closure.
    pub fn capture_input_ready(&self) -> bool {
        self.phase == SourceRelocation94Phase::CaptureInputReady
            && self.active.is_none()
            && self.completed
            && self.capture_history_complete()
            && self.close.is_none()
            && self.source.as_ref().is_some_and(|source| source.inner.source_close_admitted94())
            && self.target.as_ref().is_some_and(|target| target.settled() && target.closed())
    }

    // Ready94/completed alone is not journal evidence. Retain and inspect the
    // actual prepared ACK, latest genuine receipt, original command and exact
    // intent/ACK completion paths. In particular needs_insert()==false can
    // also mean no ACK was prepared; that must never create this checkpoint.
    pub(super) fn capture_history_complete(&self) -> bool {
        let (Some(command), Some(ack), Some(target)) =
            (&self.command, &self.prepared_ack, &self.target)
        else { return false; };
        let Some(Ok(Some(receipt))) = self.receipts.last().map(RelocationProducer::result) else { return false; };
        if self.receipt_attempted != self.receipts.len()
            || command.canonical_payload() != receipt.command().canonical_payload()
            || command.key().operation_id() != self.admission.request.operation.as_str()
            || command.key().catalog_id() != self.admission.request.expected.catalog_id.as_str()
            || command.key().host_namespace() != crate::HOST_NAMESPACE
            || command.admission_incarnation() != self.admission.request.expected.incarnation.as_str()
        {
            return false;
        }
        let intent = SourceRelocation94Purpose::Intent.index();
        let intent_complete = if self.intent_saved {
            !self.insert_attempted[intent]
                && target.commit_state(Purpose::Intent) == SourceRelocation94CommitState::NotAttempted
                && target.rollback_completed(Purpose::Intent)
        } else {
            self.prepared_intent.as_ref().is_some_and(|prepared|
                prepared.command.canonical_payload() == command.canonical_payload())
                && self.insert_attempted[intent]
                && matches!(&self.insert_result[intent], Some(Ok(())))
                && target.commit_state(Purpose::Intent) == SourceRelocation94CommitState::ReturnedOk
        };
        let purpose = if self.reconcile_used {
            SourceRelocation94Purpose::Reconcile
        } else {
            SourceRelocation94Purpose::Ack
        };
        let index = purpose.index();
        let ack_complete = if ack.saved {
            !self.insert_attempted[index]
                && target.commit_state(purpose.fixed()) == SourceRelocation94CommitState::NotAttempted
                && target.rollback_completed(purpose.fixed())
        } else {
            self.insert_attempted[index]
                && matches!(&self.insert_result[index], Some(Ok(())))
                && target.commit_state(purpose.fixed()) == SourceRelocation94CommitState::ReturnedOk
        };
        intent_complete && ack_complete
    }

    /// Claims synchronously, BEFORE constructing the borrowing future. Once a
    /// Ready94 claim has been made, an unpolled/dropped waiter, cancellation,
    /// expiry, error or unwind leaves this owner close-only. This method cannot
    /// resume/retry that claim. Terminal cleanup may still settle the exact
    /// installed frames, but cannot turn its own success into capture readiness.
    ///
    /// The same94 target must both positively observe its transaction ended and
    /// retain a successful close ACK. Resource settlement does not reinterpret a
    /// failed/unknown COMMIT as94 success: only the already Ready94 path enters.
    /// Saved receipt replay and saved ACK rollback are valid Ready94 paths; no
    /// new native outcome or unconditional COMMIT success is required here.
    pub fn prepare_capture_handoff<'a>(
        &'a mut self,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let claimed = self.capture_handoff_ready();
        if claimed {
            self.phase = SourceRelocation94Phase::ClosingTargetForCapture;
        }
        async move {
            ensure!(claimed, "owned94 positive target-close claim is not available");
            alive(self.deadline(), cancel)?;
            self.settle_retained_target().await?;
            // Every open/configuration/statement/inspection/close frame and
            // typed outcome remains in this SAME target. settled() requires a
            // real same-handle observation; closed() requires Returned(Ok).
            ensure!(self.target.as_ref().is_some_and(|target| target.settled() && target.closed()),
                "owned94 target closure is not positively retained");
            ensure!(self.active.is_none() && self.completed && self.capture_history_complete() && self.close.is_none()
                && self.source.as_ref().is_some_and(|source| source.inner.source_close_admitted94()),
                "owned94 original source custody or retirement changed");
            alive(self.deadline(), cancel)?;
            self.phase = SourceRelocation94Phase::CaptureInputReady;
            Ok(())
        }
    }
}

/// Measure only the concrete borrowed wrapper TYPE. It is included in the
/// genuine existing admit_owned94 host storage before any new work can exist.
/// All target frames were already covered by fixed_frame_bytes. Closing creates
/// no new host claim and never re-adds the predecessor94 share to prior H or K.
/// This is logical layout accounting, not allocator/RSS qualification.
pub(super) fn frame_bytes() -> usize {
    fn size<F, R>(_: F) -> usize
    where
        F: FnOnce(&'static mut OwnedSourceRelocation94, &'static AtomicBool) -> R,
        R: Future,
    {
        std::mem::size_of::<R>()
    }
    size(OwnedSourceRelocation94::prepare_capture_handoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_phase_and_completed_flag_without_retained_history_are_not_capture_input() {
        // Deliberately incomplete metadata only: no SourceRelocationSession,
        // native owner, retained close admission, receipt or ACK is fabricated.
        // This is a negative helper test, not a genuine positive94 fixture.
        let request = SourceRelocation94Request {
            operation: "missing-history".into(),
            expected: LifecycleProof {
                source_id: "source".into(), epoch: 0, catalog_id: "catalog".into(),
                incarnation: "incarnation".into(), mapping_digest: "digest".into(),
                source_revision: 1, binding_revision: 1,
                state: playscale_core::source_lifecycle::State::Active,
            },
            candidate: PathBuf::from("/missing-history"),
        };
        let mut owner = OwnedSourceRelocation94 {
            source: None, close: None,
            admission: Box::new(Admission {
                request, deadline: Instant::now() + std::time::Duration::from_secs(60),
                accounting: RelocationAccounting { host: 0 }, prospective: 0,
                original_cancel: 0, capture_ingress: None,
            }),
            target: None, phase: SourceRelocation94Phase::Ready94,
            active: None, completed: true, guard: authorization::Guard::new(),
            command: None, preparation: None, preparation_attempted: false,
            receipt_attempted: 0, prepared_intent: None, prepared_ack: None,
            native_outcome: None, receipts: Vec::new(), intent_saved: false,
            insert_attempted: [false; 4], insert_result: std::array::from_fn(|_| None),
            dispatch_ready: false, reconcile_used: false,
        };
        assert!(!owner.capture_history_complete());
        assert!(!owner.capture_handoff_ready());
        // Even a phase lookalike cannot make absence a successful close ACK or
        // turn missing receipt/ACK graphs into genuine onward proof.
        owner.phase = SourceRelocation94Phase::CaptureInputReady;
        assert!(!owner.capture_input_ready());
    }
}
