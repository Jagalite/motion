//! Production service-owned capture-input acquisition. The exact whole94 and
//! workspace remain retained through fresh current/target SystemAdmin checks.
//! Finite Native capture remains opaque; no writer/image export or95 grant.
use super::*;
use motion_catalog_backend::SourceImageCaptureOperation;
use std::sync::Arc;
use crate::v2::auth::retained::TargetAuthentication;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IngressPhase {
    Retained, Claimed, Interrupted, Prepared,
    NativeClaimed, NativeInterrupted, NativeCapturedUnretired, NativeCaptured,
    PairedClaimed, PairedInterrupted, PairedCapturedUnretired, PairedCaptured,
    Closing, Closed,
}
pub(in crate::native_service::pair) struct RetainedServiceCaptureIngress94 {
    capture: SourceImageCaptureOperation,
    workspace: AuthorizationWorkspace,
    original_phase: Phase,
    reconcile_used: bool,
    phase: IngressPhase,
}
fn cleanup_frame_size<F, R>(_: F) -> u64
where
    F: FnOnce(&'static mut RetainedServiceCaptureIngress94, &'static mut FreshWriter) -> R,
    R: Future,
{
    std::mem::size_of::<R>() as u64
}
#[cfg(not(test))]
fn native_frame_size<F, R>(_: F) -> u64
where
    F: FnOnce(&'static mut RetainedServiceCaptureIngress94, &'static Caller,
        &'static mut FreshWriter, &'static AtomicBool) -> R,
    R: Future,
{
    std::mem::size_of::<R>() as u64
}
#[cfg(test)]
fn native_frame_size<F, R>(_: F) -> u64
where
    F: FnOnce(&'static mut RetainedServiceCaptureIngress94, &'static Caller,
        &'static mut FreshWriter, &'static AtomicBool,
        &'static super::super::super::source_image::supervisor::Shared) -> R,
    R: Future,
{
    std::mem::size_of::<R>() as u64
}
pub(super) fn storage_reservation() -> u64 {
    // The stable Option is already counted by the original ServiceContinuation
    // layouts. These name NEW transient constructor/move and complete return
    // destinations. No compiler-elision credit or unknown old-H subtraction.
    // The nested cleanup frame share deliberately covers inline overlap, not
    // another dynamic graph or arbitrary error/panic heap allocation.
    2 * std::mem::size_of::<RetainedServiceCaptureIngress94>() as u64
        + std::mem::size_of::<Result<RetainedServiceCaptureIngress94, RetainedSourceRelocation94>>() as u64
        // Move-helper, target, catch closure, supervisor and NativeService
        // returned Result destinations. These are distinct conservative slots.
        + 5 * std::mem::size_of::<anyhow::Result<()>>() as u64
        + std::mem::size_of::<Result<anyhow::Result<()>, Box<dyn std::any::Any + Send>>>() as u64
        + 2 * cleanup_frame_size(RetainedServiceCaptureIngress94::settle_service)
        + std::mem::size_of::<TargetAuthentication<'static>>() as u64
        + std::mem::size_of::<Result<TargetAuthentication<'static>, crate::v2::Problem>>() as u64
        + std::mem::size_of::<CurrentSourceAuthentication<'static>>() as u64
        + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>() as u64
        + 4 * std::mem::size_of::<anyhow::Result<()>>() as u64
        // Independently admit the exact nested borrower plus constructor/move
        // overlap before it exists. The outer service task's original concrete
        // size gate remains required; it is not a substitute for this layout.
        + 2 * native_frame_size(RetainedServiceCaptureIngress94::authorize_native_capture)
        // The paired borrower is a distinct concrete layout, admitted before
        // constructing it; no renewed generic authorization cohort or deadline.
        + 2 * native_frame_size(RetainedServiceCaptureIngress94::authorize_paired_capture)
        + 4 * std::mem::size_of::<anyhow::Result<()>>() as u64
        // Native preflight, fixed-call return, current retirement and public
        // checkpoint return are distinct simultaneously live result slots.
        + 4 * std::mem::size_of::<anyhow::Result<()>>() as u64
}
impl RetainedSourceRelocation94 {
    pub(in crate::native_service::pair) fn admit_service_capture_ingress(
        &mut self,
        stopped: &crate::native_service::StoppedService,
        cancel: &Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        check(self.deadline(), cancel)?;
        ensure!(self.capture_input_ready(), "service ingress requires positive original94 close and retired reader");
        self.workspace.verify_service(stopped)?;
        // Numeric native-reservation refresh/witness checks only. This does not
        // construct or poll SQLx acquisition and is not fresh authentication.
        self.operation.admit_capture_ingress(cancel)
    }
    pub(in crate::native_service::pair) fn begin_service_capture_ingress(
        self,
    ) -> Result<RetainedServiceCaptureIngress94, Self> {
        if !self.capture_input_ready() { return Err(self); }
        let Self { operation, workspace, phase, reconcile_used } = self;
        // Only total field moves occur between take and caller installation.
        match operation.begin_capture_ingress() {
            Ok(capture) => Ok(RetainedServiceCaptureIngress94 {
                capture, workspace, original_phase: phase, reconcile_used,
                phase: IngressPhase::Retained,
            }),
            Err(operation) => Err(Self { operation, workspace, phase, reconcile_used }),
        }
    }
}
impl RetainedServiceCaptureIngress94 {
    pub(in crate::native_service::pair) fn deadline(&self) -> Instant {
        self.capture.deadline()
    }
    pub(in crate::native_service::pair) fn verify_service(
        &self,
        stopped: &crate::native_service::StoppedService,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == IngressPhase::Retained
            && self.original_phase == Phase::CaptureInputReady && self.workspace.reusable(),
            "service ingress needs the original normally retired checkpoint");
        self.workspace.verify_service(stopped)
    }
    pub(in crate::native_service::pair) fn claim_acquisition(
        &mut self, stopped: &crate::native_service::StoppedService, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.verify_service(stopped)?;
        check(self.deadline(), cancel)?;
        self.phase = IngressPhase::Claimed;
        Ok(())
    }
    pub(in crate::native_service::pair) fn authorize_acquisition<'a>(
        &'a mut self, caller: &'a Caller, writer: &'a mut FreshWriter, cancel: &'a AtomicBool,
        #[cfg(test)] shared: &'a super::super::super::source_image::supervisor::Shared,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let start = (|| {
            ensure!(self.phase == IngressPhase::Claimed, "capture acquisition cannot retry");
            self.phase = IngressPhase::Interrupted;
            check(self.deadline(), cancel)?;
            caller.retained_storage(cancel, self.deadline())?;
            ensure!(self.workspace.has_service_provenance(), "capture original workspace missing");
            self.workspace.begin()
        })();
        // Claim the actual current reader synchronously too. An unpolled
        // waiter leaves a known-never-opened reader owner for terminal cleanup,
        // not an Active workspace with no matching source claim.
        let source = start.map(|()| writer.authorization_reader());
        async move {
            let source = source?.await?;
            let deadline = self.deadline();
            let mut current = CurrentSourceAuthentication::prepare(source, caller, Permission::SystemAdmin)
                .await.map_err(current_authorization_error)?;
            let reader = self.capture.retained_input_authorization().await?;
            #[cfg(test)]
            if shared.unpolled_capture_target_prepare94() {
                drop(TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin));
                anyhow::bail!("injected unpolled target authentication preparation");
            }
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await.map_err(auth_error)?;
            target.preflight_capture_acquire()?;
            let measured = target.refresh_capture(caller, &mut current).await?;
            self.workspace.verify_capture_cohort(measured)?;
            #[cfg(test)]
            super::boundary(shared, 22);
            target.acquire_capture_input(&current, deadline, cancel).await?;
            target.prepare_capture_begin().await?;
            let measured = target.refresh_capture(caller, &mut current).await?;
            self.workspace.verify_capture_cohort(measured)?;
            #[cfg(test)]
            if shared.unpolled_capture_target_close94() {
                drop(target.close_capture_reader());
                anyhow::bail!("injected unpolled capture target-close waiter");
            }
            target.close_capture_reader().await?;
            #[cfg(test)]
            super::boundary(shared, 23);
            target.begin_capture_input(&current, deadline, cancel).await?;
            target.finish_capture_input().await?;
            drop(target);
            #[cfg(test)]
            super::boundary(shared, 24);
            self.workspace.retire_current(current, deadline, cancel).await?;
            check(deadline, cancel)?;
            ensure!(self.capture.retained_input_prepared(), "capture typed input not prepared");
            self.phase = IngressPhase::Prepared;
            Ok(())
        }
    }
    pub(in crate::native_service::pair) fn input_prepared(&self) -> bool {
        self.phase == IngressPhase::Prepared && self.workspace.reusable() && self.capture.retained_input_prepared()
    }
    /// A one-way claim of this exact prepared service owner. Readiness is not
    /// inferred from an image or a Joined target slot, and never survives error.
    pub(in crate::native_service::pair) fn claim_native_capture(
        &mut self, stopped: &crate::native_service::StoppedService, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(self.input_prepared() && self.original_phase == Phase::CaptureInputReady,
            "native capture requires actual prepared input and normal current-reader ACK");
        self.workspace.verify_service(stopped)?;
        check(self.deadline(), cancel)?;
        self.phase = IngressPhase::NativeClaimed;
        self.capture.claim_retained_native_capture()
    }
    pub(in crate::native_service::pair) fn authorize_native_capture<'a>(
        &'a mut self, caller: &'a Caller, writer: &'a mut FreshWriter, cancel: &'a AtomicBool,
        #[cfg(test)] shared: &'a super::super::super::source_image::supervisor::Shared,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let deadline = self.deadline();
        let start = (|| {
            ensure!(self.phase == IngressPhase::NativeClaimed, "native capture cannot retry");
            self.phase = IngressPhase::NativeInterrupted;
            check(deadline, cancel)?;
            caller.retained_storage(cancel, deadline)?;
            ensure!(self.workspace.has_service_provenance(), "native capture original workspace missing");
            self.workspace.begin()
        })();
        // The synchronous service claim already installed backend Stage Inputs.
        // Claim the original current reader before this borrower's first poll;
        // it stays known-unpolled during heavy preflight. No new auth grant.
        let source = start.map(|()| writer.authorization_reader());
        async move {
            let source = source?;
            // Admission construction now sees its independently retained Inputs
            // owner. Unpolled admission cannot strand the legacy close path.
            let prepare = self.capture.prepare_retained_native_capture();
            #[cfg(test)]
            if shared.unpolled_native_capture94(9) {
                drop(prepare);
                anyhow::bail!("injected unpolled native capture preflight");
            }
            prepare.await?;
            #[cfg(test)]
            shared.record_native_capture_deadline94(self.capture.retained_native_custody().unwrap().native_deadline);
            #[cfg(test)]
            super::boundary(shared, 25);
            let source = source.await?;
            let mut current = CurrentSourceAuthentication::prepare(source, caller, Permission::SystemAdmin)
                .await.map_err(current_authorization_error)?;
            let reader = self.capture.retained_native_authorization().await?;
            #[cfg(test)]
            if shared.unpolled_native_capture94(10) {
                drop(TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin));
                anyhow::bail!("injected unpolled native capture target preparation");
            }
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await.map_err(auth_error)?;
            // Genuine fresh target/current policy and lineage reads occur only
            // after all heavy writer/host/P preflight has completed.
            let measured = target.refresh_capture(caller, &mut current).await?;
            self.workspace.verify_capture_cohort(measured)?;
            #[cfg(test)]
            super::boundary(shared, 26);
            #[cfg(test)]
            if shared.unpolled_native_capture94(11) {
                drop(target.close_native_capture_reader());
                anyhow::bail!("injected unpolled native capture target close");
            }
            target.close_native_capture_reader().await?;
            #[cfg(test)]
            super::boundary(shared, 27);
            // The method's pure fence immediately enters the fixed backend call.
            // No host work follows its fence before Native's retained outcome.
            target.capture_native(&current, deadline, cancel)?;
            self.phase = IngressPhase::NativeCapturedUnretired;
            drop(target);
            #[cfg(test)]
            super::boundary(shared, 28);
            #[cfg(test)]
            if shared.unpolled_native_capture94(12) {
                drop(self.workspace.retire_current(current, deadline, cancel));
                anyhow::bail!("injected unpolled native capture current retirement");
            }
            self.workspace.retire_current(current, deadline, cancel).await?;
            check(deadline, cancel)?;
            ensure!(self.capture.retained_native_captured(), "native capture output missing");
            self.phase = IngressPhase::NativeCaptured;
            Ok(())
        }
    }
    pub(in crate::native_service::pair) fn native_captured(&self) -> bool {
        self.phase == IngressPhase::NativeCaptured && self.workspace.reusable()
            && self.capture.retained_native_captured()
    }
    /// Claim this exact native result only after its genuine current reader
    /// retired. A previous Joined target reader is never paired authority.
    pub(in crate::native_service::pair) fn claim_paired_capture(
        &mut self, stopped: &crate::native_service::StoppedService, cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(self.native_captured() && self.original_phase == Phase::CaptureInputReady,
            "paired capture requires actual NativeCaptured and normal current-reader ACK");
        self.workspace.verify_service(stopped)?;
        check(self.deadline(), cancel)?;
        self.phase = IngressPhase::PairedClaimed;
        self.capture.claim_retained_paired_capture()
    }
    pub(in crate::native_service::pair) fn authorize_paired_capture<'a>(
        &'a mut self, caller: &'a Caller, writer: &'a mut FreshWriter, cancel: &'a AtomicBool,
        #[cfg(test)] shared: &'a super::super::super::source_image::supervisor::Shared,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let deadline = self.deadline();
        let start = (|| {
            ensure!(self.phase == IngressPhase::PairedClaimed, "paired capture cannot retry");
            self.phase = IngressPhase::PairedInterrupted;
            check(deadline, cancel)?;
            caller.retained_storage(cancel, deadline)?;
            ensure!(self.workspace.has_service_provenance(), "paired capture original workspace missing");
            self.workspace.begin()
        })();
        // Claim without polling before the expensive closure scan. Cancellation
        // or an unpolled waiter retains a matching known-never-opened owner.
        let source = start.map(|()| writer.authorization_reader());
        async move {
            let source = source?;
            let prepare = self.capture.prepare_retained_paired_capture();
            #[cfg(test)]
            if shared.unpolled_paired_capture94(14) {
                drop(prepare);
                anyhow::bail!("injected unpolled paired capture preflight");
            }
            // Full original writer/pair/image-closure and accounting checks
            // precede both genuinely fresh authorization snapshots.
            prepare.await?;
            #[cfg(test)]
            super::boundary(shared, 29);
            let source = source.await?;
            #[cfg(not(test))]
            let current = CurrentSourceAuthentication::prepare(source, caller, Permission::SystemAdmin).await;
            #[cfg(test)]
            let current = if let Some(observation) = shared.paired_capture_auth_observation94(1) {
                CurrentSourceAuthentication::prepare_observed(source, caller, Permission::SystemAdmin, &observation).await
            } else {
                CurrentSourceAuthentication::prepare(source, caller, Permission::SystemAdmin).await
            };
            let mut current = current.map_err(current_authorization_error)?;
            let reader = self.capture.retained_paired_authorization().await?;
            #[cfg(test)]
            if shared.unpolled_paired_capture94(15) {
                drop(TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin));
                anyhow::bail!("injected unpolled paired capture target preparation");
            }
            #[cfg(not(test))]
            let target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin).await;
            #[cfg(test)]
            let target = if let Some(observation) = shared.paired_capture_auth_observation94(2) {
                TargetAuthentication::prepare_observed(reader, caller, Permission::SystemAdmin, &observation).await
            } else {
                TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin).await
            };
            let mut target = target.map_err(auth_error)?;
            // These fixed auth adapters are restricted read-only. Their fresh
            // policy/lineage reads cannot change the already verified closure.
            let measured = target.refresh_capture(caller, &mut current).await?;
            self.workspace.verify_capture_cohort(measured)?;
            #[cfg(test)]
            super::boundary(shared, 30);
            #[cfg(test)]
            if shared.unpolled_paired_capture94(16) {
                drop(target.close_paired_capture_reader());
                anyhow::bail!("injected unpolled paired capture target close");
            }
            target.close_paired_capture_reader().await?;
            #[cfg(test)]
            super::boundary(shared, 31);
            // The final pure fence immediately enters the fixed moves-free
            // backend transition. No verification scan or callback intervenes.
            target.complete_paired_capture(&current, deadline, cancel)?;
            self.phase = IngressPhase::PairedCapturedUnretired;
            drop(target);
            #[cfg(test)]
            super::boundary(shared, 32);
            #[cfg(test)]
            if shared.unpolled_paired_capture94(17) {
                drop(self.workspace.retire_current(current, deadline, cancel));
                anyhow::bail!("injected unpolled paired capture current retirement");
            }
            self.workspace.retire_current(current, deadline, cancel).await?;
            check(deadline, cancel)?;
            ensure!(self.capture.retained_paired_captured(), "paired capture output missing");
            self.phase = IngressPhase::PairedCaptured;
            Ok(())
        }
    }
    pub(in crate::native_service::pair) fn paired_captured(&self) -> bool {
        self.phase == IngressPhase::PairedCaptured && self.workspace.reusable()
            && self.capture.retained_paired_captured()
    }
    pub(in crate::native_service::pair) fn settle_service<'a>(
        &'a mut self,
        writer: &'a mut FreshWriter,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let closed = self.phase == IngressPhase::Closed;
        if !closed { self.phase = IngressPhase::Closing; }
        async move {
            if closed { return Ok(()); }
            // Actual original current-reader retirement precedes frozen89 close,
            // which eventually closes the original86 native source. Neither a
            // failure nor a dropped waiter may turn this workspace into a grant.
            self.workspace.settle_service_close(writer).await?;
            self.capture.close().await?;
            self.phase = IngressPhase::Closed;
            Ok(())
        }
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn acquisition_evidence(&self) -> (motion_catalog_backend::RetainedCaptureInputCustody, bool) {
        (self.capture.retained_input_custody().unwrap(), self.workspace.reusable())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn native_capture_custody(&self) -> motion_catalog_backend::RetainedNativeCaptureCustody {
        self.capture.retained_native_custody().unwrap()
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn native_capture_evidence(&self) -> (bool, bool, bool) {
        (self.capture.retained_native_captured(), self.workspace.reusable(), self.native_captured())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn paired_capture_custody(&self) -> motion_catalog_backend::RetainedPairedCaptureCustody {
        self.capture.retained_paired_custody().unwrap()
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn paired_capture_evidence(&self) -> (bool, bool, bool) {
        (self.capture.retained_paired_captured(), self.workspace.reusable(), self.paired_captured())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) async fn refuse_native_capture_onward_for_fixture(&mut self) {
        assert!(self.capture.writer_in().await.is_err());
        assert!(self.capture.verify(&AtomicBool::new(false)).await.is_err());
        assert!(self.capture.capture(&AtomicBool::new(false)).await.is_err());
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn closed_for_fixture(&self) -> bool {
        self.phase == IngressPhase::Closed
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_evidence(
        &self,
        stopped: &crate::native_service::StoppedService,
    ) -> anyhow::Result<(Instant, usize, usize, bool, bool, bool, bool)> {
        self.workspace.verify_service(stopped)?;
        Ok((self.deadline(), self as *const Self as usize,
            &self.workspace as *const AuthorizationWorkspace as usize,
            self.original_phase == Phase::CaptureInputReady, self.reconcile_used,
            self.phase == IngressPhase::Closed, self.capture.retained_input_prepared()))
    }
}
