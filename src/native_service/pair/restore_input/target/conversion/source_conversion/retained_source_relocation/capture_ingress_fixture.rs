//! Test-only genuine-owner handoff. No source/session constructor, phase setter,
//! replacement workspace, capture grant or service continuation is provided.
use super::*;
use motion_catalog_backend::SourceImageCaptureOperation;
use std::sync::Arc;

/// Explicitly detached backend-integration custody, NOT a stable service slot.
/// Every original wrapper field moves intact; the original authorization
/// workspace stays beside the whole94 backend owner throughout the test.
pub(in crate::native_service::pair) struct CaptureIngressFixture94 {
    capture: SourceImageCaptureOperation,
    workspace: AuthorizationWorkspace,
    original_phase: Phase,
    reconcile_used: bool,
}
// Exact cfg(test)-only destinations, charged by the original app source94
// reservation BEFORE admission or this fixture handoff. No generic allowance,
// host subtraction, or credit against backend-owned layouts is taken. These
// are separate conservative INLINE move/result slots, including their inline
// backend payloads. The inherited whole94 dynamic graphs remain counted once;
// sizeof here never adds another copy of those graphs or an error/panic heap.
pub(super) const fn storage_reservation() -> u64 {
    // The retained fixture binding and its consuming finish-refusal return.
    2 * std::mem::size_of::<CaptureIngressFixture94>() as u64
        // RetainedSourceRelocation94 -> fixture bridge, including unchanged Err.
        + std::mem::size_of::<Result<CaptureIngressFixture94, RetainedSourceRelocation94>>() as u64
        // ServiceTarget return and the test's detach forwarding return.
        + 2 * std::mem::size_of::<anyhow::Result<CaptureIngressFixture94>>() as u64
        // Public finish() is called only to prove that it returns this capture.
        + std::mem::size_of::<Result<motion_catalog_backend::SourceCurrentImage, SourceImageCaptureOperation>>() as u64
}
impl RetainedSourceRelocation94 {
    pub(in crate::native_service::pair) fn admit_capture_ingress_for_fixture(
        &mut self,
        stopped: &crate::native_service::StoppedService,
        cancel: &Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        ensure!(self.capture_input_ready(), "fixture requires genuine app and backend capture-input proof");
        self.workspace.verify_service(stopped)?;
        self.operation.admit_capture_ingress(cancel)
    }

    /// An unsupported/missing backend admission returns every original field.
    /// The caller restores this returned wrapper to the original service slot;
    /// that extraction/reinsertion is test plumbing, not service custody proof.
    pub(in crate::native_service::pair) fn begin_capture_ingress_for_fixture(
        self,
    ) -> Result<CaptureIngressFixture94, Self> {
        if !self.capture_input_ready() {
            return Err(self);
        }
        let Self { operation, workspace, phase, reconcile_used } = self;
        match operation.begin_capture_ingress() {
            Ok(capture) => Ok(CaptureIngressFixture94 {
                capture, workspace, original_phase: phase, reconcile_used,
            }),
            Err(operation) => Err(Self { operation, workspace, phase, reconcile_used }),
        }
    }
}
impl CaptureIngressFixture94 {
    pub(in crate::native_service::pair) fn capture_mut(&mut self) -> &mut SourceImageCaptureOperation {
        &mut self.capture
    }
    pub(in crate::native_service::pair) fn verify_workspace(
        &self,
        stopped: &crate::native_service::StoppedService,
    ) -> anyhow::Result<()> {
        ensure!(self.original_phase == Phase::CaptureInputReady && self.workspace.reusable(),
            "fixture must preserve the original normally retired workspace");
        self.workspace.verify_service(stopped)
    }
    pub(in crate::native_service::pair) fn workspace_address(&self) -> usize {
        &self.workspace as *const AuthorizationWorkspace as usize
    }
    pub(in crate::native_service::pair) fn assert_finish_refused(self) -> Self {
        let Self { capture, workspace, original_phase, reconcile_used } = self;
        match capture.finish() {
            Err(capture) => Self { capture, workspace, original_phase, reconcile_used },
            Ok(_) => panic!("retained94 ingress cannot manufacture a captured image"),
        }
    }
}
