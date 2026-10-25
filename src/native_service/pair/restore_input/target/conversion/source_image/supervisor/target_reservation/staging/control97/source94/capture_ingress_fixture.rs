//! Narrow test-only bridge through otherwise private service fields. A genuine
//! target-close proof and original service binding are prerequisites. No phase
//! is changed, and this is deliberately NOT production capture adoption.
use super::*;
use crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_source_relocation::CaptureIngressFixture94;
use std::sync::Arc;

impl ServiceTarget {
    pub(in crate::native_service::pair) fn source94_ingress_fixture_caller_address(&self) -> usize {
        &self.caller as *const Caller as usize
    }
    pub(in crate::native_service::pair) fn admit_source94_ingress_for_fixture(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
        cancel: &Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94CaptureInputReady,
            "fixture admission requires actual service target closure");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().context("fixture original continuation absent")?
            .relocation.as_mut().context("fixture original whole94 absent")?
            .admit_capture_ingress_for_fixture(&pair._stopped, cancel)
    }
    pub(in crate::native_service::pair) fn begin_source94_ingress_for_fixture(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<CaptureIngressFixture94> {
        ensure!(self.phase == TargetPhase::Source94CaptureInputReady,
            "fixture handoff requires actual service target closure");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let slot = &mut self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().context("fixture original continuation absent")?.relocation;
        let owner = slot.take().context("fixture original whole94 absent")?;
        match owner.begin_capture_ingress_for_fixture() {
            Ok(capture) => Ok(capture),
            Err(owner) => {
                // Only the returned original whole owner is reinserted. There
                // is no helper accepting an arbitrary source or replacement.
                *slot = Some(owner);
                anyhow::bail!("fixture backend capture ingress admission missing")
            }
        }
    }
}
