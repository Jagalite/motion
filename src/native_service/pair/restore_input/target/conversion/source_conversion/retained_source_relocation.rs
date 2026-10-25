//! Finite service-owned94. Same source session, close proof, workspace, original
//! writer and absolute deadline; never a SourceConversionActor/global sink.
//! SQL authorization is checked at the actual first effect-capable poll. Queued
//! SQL worker execution and native internal checkpoints are not host preemption.
use super::{
    current_authorization_error, retained_native_bootstrap::ConnectedSource96,
    retained_native_intent::AuthorizationWorkspace,
};
use crate::v2::auth::{
    Caller,
    retained::{CurrentSourceAuthentication, RelocationTargetAuthentication},
};
use anyhow::ensure;
use motion_catalog_backend::{
    OwnedSourceRelocation94, SourceRelocation94Purpose as Purpose, SourceRelocation94Request,
    SourceRelocationRequester, motion_snapshot::FreshWriter,
};
use playscale_core::access::Permission;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Admitted,
    Interrupted,
    IntentSaved,
    ReceiptPending,
    Ready94,
    ClosingTargetForCapture,
    CaptureInputReady,
    Closing,
    Closed,
}

/// Kept inline in the actual ServiceContinuation. No Clone, owner extractor,
/// arbitrary retry, pool, callback grant, or Drop recovery transfer exists.
pub(in crate::native_service::pair) struct RetainedSourceRelocation94 {
    operation: OwnedSourceRelocation94,
    workspace: AuthorizationWorkspace,
    phase: Phase,
    reconcile_used: bool,
}
impl RetainedSourceRelocation94 {
    pub(super) fn storage_reservation() -> u64 {
        #[cfg(test)]
        let fixture_handoff = capture_ingress_fixture::storage_reservation();
        #[cfg(not(test))]
        let fixture_handoff = 0;
        (2 * std::mem::size_of::<Self>()
            + 2 * std::mem::size_of::<Result<Self, ConnectedSource96>>()
            + std::mem::size_of::<(OwnedSourceRelocation94, AuthorizationWorkspace)>()
            + std::mem::size_of::<
                Result<(OwnedSourceRelocation94, AuthorizationWorkspace), ConnectedSource96>,
            >()
            + 2 * std::mem::size_of::<RelocationTargetAuthentication<'static>>()
            + std::mem::size_of::<
                Result<RelocationTargetAuthentication<'static>, crate::v2::Problem>,
            >()
            + std::mem::size_of::<CurrentSourceAuthentication<'static>>()
            + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>(
            )
            + 2 * std::mem::size_of::<SourceRelocationRequester>()
            + std::mem::size_of::<SourceRelocation94Request>()
            + 2 * std::mem::size_of::<Result<(), anyhow::Error>>()) as u64
            // Actual target/requester capacities are checked before copying.
            // Exactly one app-owned typed requester is cloned. Its separate
            // requester ID is included in the measured share. Backend encoded
            // rows/decoder graphs are admitted separately; auth snapshots stay
            // in the original named workspace. Original Caller is not subtracted.
            + Caller::RETAINED_STORAGE_LIMIT
            + fixture_handoff
            + service_capture_ingress::storage_reservation()
    }
    pub(in crate::native_service::pair) fn admit(
        source: &mut ConnectedSource96,
        stopped: &crate::native_service::StoppedService,
        request: SourceRelocation94Request,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        source.admit_relocation94(stopped, request, deadline, cancel)
    }
    pub(in crate::native_service::pair) fn from_admitted(
        source: ConnectedSource96,
    ) -> Result<Self, ConnectedSource96> {
        source
            .begin_relocation94()
            .map(|(operation, workspace)| Self {
                operation,
                workspace,
                phase: Phase::Admitted,
                reconcile_used: false,
            })
    }
    pub(in crate::native_service::pair) fn deadline(&self) -> Instant {
        self.operation.deadline()
    }
    pub(in crate::native_service::pair) fn receipt_pending(&self) -> bool {
        self.phase == Phase::ReceiptPending
            && self.workspace.reusable()
            && self.operation.clean_receipt_pending()
    }
    pub(in crate::native_service::pair) fn reconcile_claimed(&self) -> bool {
        self.reconcile_used
    }
    pub(in crate::native_service::pair) fn ready(&self) -> bool {
        self.phase == Phase::Ready94
            && self.workspace.reusable()
            && self.operation.capture_handoff_ready()
    }
    /// Synchronous service claim. Reusable alone is not authority: terminal
    /// cleanup deliberately may leave an already retired workspace Reusable.
    pub(in crate::native_service::pair) fn claim_capture_input(
        &mut self,
        stopped: &crate::native_service::StoppedService,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline(), cancel)?;
        self.workspace.verify_service(stopped)?;
        ensure!(self.ready(), "source94 capture input requires actual Ready94 and normal reader retirement");
        self.phase = Phase::ClosingTargetForCapture;
        Ok(())
    }

    /// No source extraction or capture grant. Both app and backend claims are
    /// made before returning this waiter; dropping it cannot restore Ready94.
    /// Exact SQLx frames remain in the backend, never in this borrowing waiter.
    pub(in crate::native_service::pair) fn prepare_capture_input<'a>(
        &'a mut self,
        stopped: &'a crate::native_service::StoppedService,
        cancel: &'a AtomicBool,
        #[cfg(test)] shared: &'a super::super::source_image::supervisor::Shared,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let deadline = self.deadline();
        let claimed = self.phase == Phase::ClosingTargetForCapture;
        if claimed {
            self.phase = Phase::Interrupted;
        }
        let start = (|| {
            ensure!(claimed, "source94 target close is one-shot");
            check(deadline, cancel)?;
            self.workspace.verify_service(stopped)?;
            ensure!(self.workspace.reusable() && self.operation.capture_handoff_ready(),
                "source94 target close requires inactive purpose and acknowledged normal reader retirement");
            Ok(())
        })();
        let Self { operation, workspace, phase, .. } = self;
        let close = start.map(|()| operation.prepare_capture_handoff(cancel));
        async move {
            close?.await?;
            #[cfg(test)]
            boundary(shared, 19);
            check(deadline, cancel)?;
            workspace.verify_service(stopped)?;
            ensure!(workspace.reusable(), "source94 original workspace no longer normally retired");
            *phase = Phase::CaptureInputReady;
            Ok(())
        }
    }
    pub(in crate::native_service::pair) fn capture_input_ready(&self) -> bool {
        self.phase == Phase::CaptureInputReady
            && self.workspace.reusable()
            && self.operation.capture_input_ready()
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn custody_evidence(&self) -> (usize, usize) {
        (self as *const Self as usize, &self.workspace as *const AuthorizationWorkspace as usize)
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn capture_input_evidence(&self) -> (bool, bool) {
        (self.capture_input_ready(), self.operation.capture_input_ready())
    }
    pub(in crate::native_service::pair) fn claim_reconcile(
        &mut self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline(), cancel)?;
        ensure!(
            self.receipt_pending() && !self.reconcile_used,
            "source94 only permits one clean same-owner receipt reconciliation"
        );
        self.reconcile_used = true;
        Ok(())
    }
    pub(in crate::native_service::pair) fn authorize_service<'a>(
        &'a mut self,
        purpose: Purpose,
        caller: &'a Caller,
        writer: &'a mut FreshWriter,
        cancel: &'a AtomicBool,
        #[cfg(test)] shared: &'a super::super::source_image::supervisor::Shared,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        // Claim the one exact cohort synchronously, before a borrower future or
        // source reader exists. No unpolled or failed call restores eligibility.
        let start = (|| {
            let eligible = match purpose {
                Purpose::Intent => self.phase == Phase::Admitted,
                Purpose::Dispatch => self.phase == Phase::IntentSaved,
                Purpose::Ack => self.receipt_pending() && !self.reconcile_used,
                Purpose::Reconcile => self.receipt_pending() && self.reconcile_used,
            };
            ensure!(
                eligible,
                "source94 purpose cannot retry, skip or revive a terminal phase"
            );
            check(self.deadline(), cancel)?;
            caller.retained_storage(cancel, self.deadline())?;
            ensure!(
                self.workspace.has_service_provenance(),
                "source94 requires original service provenance"
            );
            self.phase = Phase::Interrupted;
            self.workspace.begin()
        })();
        // Claim before either the outer waiter or target-open await can be
        // abandoned. The actual source slot remains on the original writer.
        let source = start.map(|()| writer.authorization_reader());
        async move {
            let source = source?;
            let deadline = self.deadline();
            if matches!(purpose, Purpose::Intent) {
                self.operation.open(cancel).await?;
            }
            check(deadline, cancel)?;
            let source = source.await?;
            let mut current =
                CurrentSourceAuthentication::prepare(source, caller, Permission::SourcesManage)
                    .await
                    .map_err(current_authorization_error)?;
            let token = self.operation.authorization(purpose)?;
            #[cfg(test)]
            if shared.unprepared_saved94(purpose)
            {
                // Fixed negative sequence through public backend methods. The
                // whole real retained96/94 owner remains in this service slot;
                // no proof, receipt, ACK, phase or readiness flag is fabricated.
                let mut token = token;
                token.authorize_connection(cancel).await?;
                token.verify(cancel).await?;
                if purpose != Purpose::Intent {
                    assert!(!token.needs_insert(), "no insert is not a saved-work proof");
                }
                let error = token.finish_saved(cancel).await
                    .expect_err("unprepared public saved completion must be refused");
                // This verifies the same actual WRITE remains open: rejection
                // must precede rollback, not merely refuse a later handoff.
                token.verify(cancel).await?;
                drop(token);
                assert!(self.operation.finish_authorization(purpose).is_err());
                assert_eq!(self.operation.phase(), motion_catalog_backend::SourceRelocation94Phase::Interrupted);
                assert!(self.operation.successor().is_none());
                anyhow::bail!("saved94 fixture refused unprepared {purpose:?}: {error:#}");
            }
            let mut target = RelocationTargetAuthentication::prepare(token, caller, cancel)
                .await
                .map_err(auth_error)?;
            let requester = target.relocation_requester(caller, deadline, cancel)?;
            check(deadline, cancel)?;
            match purpose {
                Purpose::Intent => {
                    target.prepare_intent(&requester, cancel).await?;
                    if target.needs_insert() {
                        target
                            .refresh(caller, &mut current, cancel)
                            .await
                            .map_err(auth_error)?;
                        #[cfg(test)]
                        boundary(shared, 10);
                        target.insert_intent(&current, deadline, cancel).await?;
                        // Separate full fresh checks AFTER pending insertion and
                        // its postchecks, never reuse INSERT's authorization.
                        target
                            .refresh(caller, &mut current, cancel)
                            .await
                            .map_err(auth_error)?;
                        #[cfg(test)]
                        boundary(shared, 11);
                        target.commit(&current, deadline, cancel).await?;
                    } else {
                        target.finish_saved(cancel).await?;
                    }
                }
                Purpose::Dispatch => {
                    target.preflight_execute(&requester, cancel).await?;
                    target
                        .refresh(caller, &mut current, cancel)
                        .await
                        .map_err(auth_error)?;
                    #[cfg(test)]
                    boundary(shared, 12);
                    target.execute(&current, deadline, cancel)?;
                    // The backend already retained the genuine native outcome,
                    // including Err, BEFORE this post-return boundary/await.
                    #[cfg(test)]
                    boundary(shared, 16);
                    target.observe_receipt(&requester, cancel).await?;
                    #[cfg(test)]
                    boundary(shared, 17);
                    target.finish_dispatch(cancel).await?;
                }
                Purpose::Ack | Purpose::Reconcile => {
                    // Reconcile is a distinct backend purpose: only committed
                    // original intent+real existing receipt; never dispatch.
                    target.prepare_ack(&requester, cancel).await?;
                    if target.needs_insert() {
                        target
                            .refresh(caller, &mut current, cancel)
                            .await
                            .map_err(auth_error)?;
                        #[cfg(test)]
                        boundary(shared, 13);
                        target.insert_ack(&current, deadline, cancel).await?;
                        target
                            .refresh(caller, &mut current, cancel)
                            .await
                            .map_err(auth_error)?;
                        #[cfg(test)]
                        boundary(shared, 14);
                        target.commit(&current, deadline, cancel).await?;
                    } else {
                        target.finish_saved(cancel).await?;
                    }
                }
            }
            drop(target);
            drop(requester);
            #[cfg(test)]
            boundary(shared, 15);
            // A successful effect cannot impersonate reader retirement ACK.
            // Failure here leaves Interrupted and the actual reader/close slot.
            self.workspace
                .retire_current(current, deadline, cancel)
                .await?;
            self.operation.finish_authorization(purpose)?;
            check(deadline, cancel)?;
            self.phase = match purpose {
                Purpose::Intent => Phase::IntentSaved,
                Purpose::Dispatch => Phase::ReceiptPending,
                Purpose::Ack | Purpose::Reconcile => Phase::Ready94,
            };
            Ok(())
        }
    }
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut FreshWriter,
    ) -> anyhow::Result<()> {
        if self.phase == Phase::Closed {
            return Ok(());
        }
        self.phase = Phase::Closing;
        // Never first-poll an unstarted commit here. The backend either settles
        // the original started runner or retains uncertainty and refuses close.
        self.operation.settle_target().await?;
        self.workspace.settle_service_close(writer).await?;
        // This whole owner retains the actual terminal Settled workspace and
        // every94 tail beside86 close. No forged Reusable96 close gate.
        self.operation.close_source()?;
        ensure!(
            self.operation.closed(),
            "source94 source close lacks exact ACK"
        );
        self.phase = Phase::Closed;
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_evidence(
        &self,
    ) -> (
        motion_catalog_backend::SourceRelocation94Phase,
        bool,
        bool,
        bool,
    ) {
        (
            self.operation.phase(),
            self.workspace.reusable(),
            self.reconcile_used,
            self.operation.closed(),
        )
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn retained_evidence(
        &self,
    ) -> (
        Option<bool>,
        bool,
        bool,
        motion_catalog_backend::SourceRelocation94CommitState,
        motion_catalog_backend::SourceRelocation94CommitState,
    ) {
        (
            self.operation.retained_outcome().map(|result| result.is_ok()),
            self.operation.receipt_observed(),
            self.operation.source_close_admitted(),
            self.operation.intent_commit_state(),
            self.operation.ack_commit_state(),
        )
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn successor(
        &self,
    ) -> Option<&motion_catalog_backend::source_lifecycle::LifecycleProof> {
        self.operation.successor()
    }
}
fn check(deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "source94 original cancellation set"
    );
    ensure!(
        Instant::now() < deadline,
        "source94 original deadline expired"
    );
    Ok(())
}
fn auth_error(error: crate::v2::Problem) -> anyhow::Error {
    anyhow::anyhow!("source94 target authorization: {error:?}")
}
#[cfg(test)]
fn boundary(shared: &super::super::source_image::supervisor::Shared, stage: u8) {
    super::super::source_image::supervisor::source94::source94_boundary(shared, stage)
}

// Test-only backend integration, not a production service continuation.
#[cfg(test)]
#[path = "retained_source_relocation/capture_ingress_fixture.rs"]
mod capture_ingress_fixture;
#[cfg(test)]
pub(in crate::native_service::pair) use capture_ingress_fixture::CaptureIngressFixture94;

// Production moves-only service custody. No acquisition or capture grant.
#[path = "retained_source_relocation/service_capture_ingress.rs"]
mod service_capture_ingress;
pub(in crate::native_service::pair) use service_capture_ingress::RetainedServiceCaptureIngress94;
