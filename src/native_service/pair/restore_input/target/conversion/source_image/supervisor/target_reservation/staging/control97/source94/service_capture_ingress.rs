//! Finite production adoption, not detached fixture custody or capture.
use super::*;
impl ServiceTarget {
    pub(in crate::native_service) fn retain_source94_capture_ingress(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94CaptureInputReady,
            "service ingress requires the actual positive target-close checkpoint");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let held = self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().context("service ingress original continuation missing")?;
        ensure!(held.source.is_none() && held.capture_ingress.is_none(),
            "service ingress cannot replace existing custody");
        let original = held.relocation.as_mut().context("service ingress original whole94 missing")?;
        ensure!(original.deadline() == self.deadline, "service ingress deadline differs");
        // From here failures are terminal, with all available original fields
        // retained. Admission occurs while the complete wrapper is still stored.
        self.phase = TargetPhase::Source94CaptureIngress;
        original.admit_service_capture_ingress(&pair._stopped, &shared.cancel)?;
        move_into_service_slot(&mut held.relocation, &mut held.capture_ingress)?;
        // Every later fallible check sees the entire moved owner in its genuine
        // service slot, beside the same caller/pair/selection/stopped locks.
        #[cfg(test)]
        super::super::super::super::super::source94::source94_boundary(shared, 20);
        let ingress = held.capture_ingress.as_ref().unwrap();
        ensure!(ingress.deadline() == self.deadline, "service ingress cannot replace deadline");
        ingress.verify_service(&pair._stopped)?;
        self.verify_source94_capture_binding(pair, selection, shared)?;
        self.phase = TargetPhase::Source94CaptureIngressRetained;
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source94_service_ingress_evidence(
        &self,
        stopped: &StoppedService,
    ) -> anyhow::Result<Option<(Instant, usize, usize, bool, bool, bool, bool)>> {
        let Some(held) = self.staging.as_ref().and_then(|s| s.control.as_ref())
            .and_then(|c| c.continuation.as_ref()) else { return Ok(None); };
        ensure!(held.capture_ingress.is_none() || (held.relocation.is_none() && held.source.is_none()),
            "service capture and original source slots overlap");
        held.capture_ingress.as_ref().map(|owner| owner.service_evidence(stopped)).transpose()
    }
}

fn move_into_service_slot(
    relocation: &mut Option<RetainedSourceRelocation94>,
    capture: &mut Option<crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_source_relocation::RetainedServiceCaptureIngress94>,
) -> anyhow::Result<()> {
    ensure!(capture.is_none(), "service ingress destination already occupied");
    let original = relocation.take().context("service ingress original owner absent")?;
    match original.begin_service_capture_ingress() {
        Ok(ingress) => *capture = Some(ingress),
        Err(original) => {
            *relocation = Some(original);
            anyhow::bail!("service ingress admission refused with original wrapper retained");
        }
    }
    Ok(())
}
#[cfg(test)]
impl ServiceTarget {
    pub(in crate::native_service::pair) fn missing_service_ingress_admission_for_fixture(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94CaptureInputReady, "missing-admission fixture requires genuine predecessor");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let held = self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().unwrap();
        // Use the actual production moves-only helper. No test detach owner,
        // duplicate move/reinsert code, new admission or invented phase.
        move_into_service_slot(&mut held.relocation, &mut held.capture_ingress)
    }
    pub(in crate::native_service::pair) fn source94_service_ingress_closed(&self) -> Option<bool> {
        Some(self.staging.as_ref()?.control.as_ref()?.continuation.as_ref()?
            .capture_ingress.as_ref()?.closed_for_fixture())
    }
}

impl ServiceTarget {
    pub(in crate::native_service) fn claim_source94_capture_acquisition(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<Instant> {
        ensure!(self.phase == TargetPhase::Source94CaptureIngressRetained,
            "capture acquisition requires actual retained service ingress");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let held = self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().context("capture original continuation missing")?;
        ensure!(held.source.is_none() && held.relocation.is_none(), "capture owner slots overlap");
        held.capture_ingress.as_mut().context("capture stable owner missing")?
            .claim_acquisition(&pair._stopped, &shared.cancel)?;
        self.phase = TargetPhase::Source94CaptureAcquiring;
        Ok(self.deadline)
    }
    pub(super) async fn run_source94_capture_acquisition(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94CaptureAcquiring, "capture acquisition was not claimed");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        {
            let ingress = self.staging.as_mut().unwrap().control.as_mut().unwrap()
                .continuation.as_mut().unwrap().capture_ingress.as_mut()
                .context("capture stable owner missing")?;
            #[cfg(test)]
            if shared.test.source94_unpolled.load(Ordering::Acquire) == 5 {
                drop(ingress.authorize_acquisition(&self.caller, &mut pair._writer, &shared.cancel, shared));
                anyhow::bail!("injected unpolled service capture authorization waiter");
            }
            ingress.authorize_acquisition(&self.caller, &mut pair._writer, &shared.cancel,
                #[cfg(test)] shared).await?;
            ensure!(ingress.input_prepared(), "capture input lacks actual normal reader retirement");
        }
        self.verify_source94_capture_binding(pair, selection, shared)?;
        self.phase = TargetPhase::Source94CapturePrepared;
        Ok(())
    }
}

#[cfg(test)]
impl ServiceTarget {
    pub(in crate::native_service::pair) fn source94_acquisition_evidence(&self) -> (motion_catalog_backend::RetainedCaptureInputCustody, bool) {
        self.staging.as_ref().unwrap().control.as_ref().unwrap().continuation.as_ref().unwrap()
            .capture_ingress.as_ref().unwrap().acquisition_evidence()
    }
}

impl ServiceTarget {
    pub(in crate::native_service) fn claim_source94_native_capture(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<Instant> {
        ensure!(self.phase == TargetPhase::Source94CapturePrepared,
            "native capture requires actual prepared service input");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let held = self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().context("native capture original continuation missing")?;
        ensure!(held.source.is_none() && held.relocation.is_none(), "native capture owner slots overlap");
        held.capture_ingress.as_mut().context("native capture stable owner missing")?
            .claim_native_capture(&pair._stopped, &shared.cancel)?;
        self.phase = TargetPhase::Source94NativeCapturing;
        Ok(self.deadline)
    }
    pub(super) async fn run_source94_native_capture(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94NativeCapturing, "native capture was not claimed");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        {
            let ingress = self.staging.as_mut().unwrap().control.as_mut().unwrap()
                .continuation.as_mut().unwrap().capture_ingress.as_mut()
                .context("native capture stable owner missing")?;
            #[cfg(test)]
            if shared.unpolled_native_capture94(8) {
                drop(ingress.authorize_native_capture(&self.caller, &mut pair._writer, &shared.cancel, shared));
                anyhow::bail!("injected unpolled service native capture waiter");
            }
            ingress.authorize_native_capture(&self.caller, &mut pair._writer, &shared.cancel,
                #[cfg(test)] shared).await?;
            ensure!(ingress.native_captured(), "native image lacks actual normal current-reader retirement");
        }
        // The original service locks, selection, durable publications and
        // absolute deadline remain binding after the opaque image is retained.
        self.verify_source94_capture_binding(pair, selection, shared)?;
        self.phase = TargetPhase::Source94NativeCaptured;
        Ok(())
    }
}

#[cfg(test)]
impl ServiceTarget {
    pub(in crate::native_service::pair) fn source94_native_capture_custody(&self) -> motion_catalog_backend::RetainedNativeCaptureCustody {
        self.staging.as_ref().unwrap().control.as_ref().unwrap().continuation.as_ref().unwrap()
            .capture_ingress.as_ref().unwrap().native_capture_custody()
    }
    pub(in crate::native_service::pair) fn source94_native_capture_evidence(&self) -> (bool, bool, bool) {
        self.staging.as_ref().unwrap().control.as_ref().unwrap().continuation.as_ref().unwrap()
            .capture_ingress.as_ref().unwrap().native_capture_evidence()
    }
    pub(in crate::native_service::pair) async fn refuse_source94_native_onward_for_fixture(&mut self) {
        self.staging.as_mut().unwrap().control.as_mut().unwrap().continuation.as_mut().unwrap()
            .capture_ingress.as_mut().unwrap().refuse_native_capture_onward_for_fixture().await;
    }
}

impl ServiceTarget {
    pub(in crate::native_service) fn claim_source94_paired_capture(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<Instant> {
        ensure!(self.phase == TargetPhase::Source94NativeCaptured,
            "paired capture requires actual service NativeCaptured");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let held = self.staging.as_mut().unwrap().control.as_mut().unwrap()
            .continuation.as_mut().context("paired capture original continuation missing")?;
        ensure!(held.source.is_none() && held.relocation.is_none(), "paired capture owner slots overlap");
        held.capture_ingress.as_mut().context("paired capture stable owner missing")?
            .claim_paired_capture(&pair._stopped, &shared.cancel)?;
        self.phase = TargetPhase::Source94PairedCapturing;
        Ok(self.deadline)
    }
    pub(super) async fn run_source94_paired_capture(
        &mut self, pair: &mut PreparedPair, selection: &mut ServiceSelection, shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94PairedCapturing, "paired capture was not claimed");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        {
            let ingress = self.staging.as_mut().unwrap().control.as_mut().unwrap()
                .continuation.as_mut().unwrap().capture_ingress.as_mut()
                .context("paired capture stable owner missing")?;
            #[cfg(test)]
            if shared.unpolled_paired_capture94(13) {
                drop(ingress.authorize_paired_capture(&self.caller, &mut pair._writer, &shared.cancel, shared));
                anyhow::bail!("injected unpolled service paired capture waiter");
            }
            ingress.authorize_paired_capture(&self.caller, &mut pair._writer, &shared.cancel,
                #[cfg(test)] shared).await?;
            ensure!(ingress.paired_captured(), "paired image lacks actual normal current-reader retirement");
        }
        self.verify_source94_capture_binding(pair, selection, shared)?;
        self.phase = TargetPhase::Source94PairedCaptured;
        Ok(())
    }
}

#[cfg(test)]
impl ServiceTarget {
    pub(in crate::native_service::pair) fn source94_paired_capture_custody(&self) -> motion_catalog_backend::RetainedPairedCaptureCustody {
        self.staging.as_ref().unwrap().control.as_ref().unwrap().continuation.as_ref().unwrap()
            .capture_ingress.as_ref().unwrap().paired_capture_custody()
    }
    pub(in crate::native_service::pair) fn source94_paired_capture_evidence(&self) -> (bool, bool, bool) {
        self.staging.as_ref().unwrap().control.as_ref().unwrap().continuation.as_ref().unwrap()
            .capture_ingress.as_ref().unwrap().paired_capture_evidence()
    }
}
