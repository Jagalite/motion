//! The actual service continuation owns94 before any relocation future exists.
use super::*;
use crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_source_relocation::RetainedSourceRelocation94;
use motion_catalog_backend::{SourceRelocation94Purpose as Purpose, SourceRelocation94Request};

pub(super) const fn storage_reservation() -> u64 {
    // Original task and authorization cohorts stay admitted. This adds only the
    // new simultaneous destination/handoff layouts, not another64MiB cohort.
    2 * std::mem::size_of::<RetainedSourceRelocation94>() as u64
        + std::mem::size_of::<Result<RetainedSourceRelocation94, crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_native_bootstrap::ConnectedSource96>>() as u64
}
impl ServiceTarget {
    /// Recheck the genuine original service/selection/record/publication
    /// identities, never descriptive paths or Ready94 alone.
    fn verify_source94_capture_binding(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        alive(self.deadline, &shared.cancel)?;
        ensure!(selection.target_deadline(&shared.cancel)? == self.deadline,
            "source94 capture input cannot extend the original deadline");
        ensure!(self.retirement_failure.is_none(), "source94 target retirement failed");
        pair._stopped.verify_lock()?;
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        self.verify_control_record(selected)?;
        let stage = self.staging.as_mut().context("source94 original stage missing")?;
        for slot in &stage.slots[..2] {
            slot.outcome.as_ref().context("source94 original publication missing")?
                .verify_mutable_stage_identity()?;
        }
        Ok(())
    }
    pub(in crate::native_service) fn claim_source94_capture_input(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<Instant> {
        ensure!(self.phase == TargetPhase::Source94Ready,
            "source94 positive target close requires actual Ready94");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        let held = self.staging.as_mut().unwrap().control.as_mut()
            .context("source94 original control missing")?.continuation.as_mut()
            .context("source94 original continuation missing")?;
        ensure!(held.source.is_none(), "source94 cannot replace retained source96 custody");
        let operation = held.relocation.as_mut().context("source94 whole owner missing")?;
        ensure!(operation.deadline() == self.deadline, "source94 original owner deadline changed");
        operation.claim_capture_input(&pair._stopped, &shared.cancel)?;
        self.phase = TargetPhase::Source94ClosingTarget;
        Ok(self.deadline)
    }
    async fn run_source94_capture_input(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(self.phase == TargetPhase::Source94ClosingTarget,
            "source94 target close must be synchronously claimed");
        self.verify_source94_capture_binding(pair, selection, shared)?;
        {
            let held = self.staging.as_mut().unwrap().control.as_mut().unwrap()
                .continuation.as_mut().context("source94 original continuation missing")?;
            let operation = held.relocation.as_mut().context("source94 whole owner missing")?;
            #[cfg(test)]
            super::super::super::super::source94::source94_boundary(shared, 18);
            #[cfg(test)]
            if shared.test.source94_unpolled.load(Ordering::Acquire) == 4 {
                let work = operation.prepare_capture_input(&pair._stopped, &shared.cancel, shared);
                drop(work);
                anyhow::bail!("injected unpolled source94 target-close waiter");
            }
            operation.prepare_capture_input(
                &pair._stopped,
                &shared.cancel,
                #[cfg(test)]
                shared,
            ).await?;
            ensure!(operation.capture_input_ready(),
                "source94 target close lacks actual ended transaction and positive close ACK");
        }
        // The original owner stays in the same slot through these fallible
        // postchecks. A positive target close alone is not a service endpoint.
        self.verify_source94_capture_binding(pair, selection, shared)?;
        self.phase = TargetPhase::Source94CaptureInputReady;
        Ok(())
    }
    pub(in crate::native_service) fn claim_source94(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        request: Option<SourceRelocation94Request>,
        goal: Source94Goal,
        shared: &Shared,
    ) -> anyhow::Result<Instant> {
        alive(self.deadline, &shared.cancel)?;
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "source94 original selection deadline differs"
        );
        pair._stopped.verify_lock()?;
        let initial = request.is_some();
        ensure!(
            self.phase
                == if initial {
                    TargetPhase::SourceReady
                } else {
                    TargetPhase::ReceiptPending
                },
            "source94 requires exact service predecessor"
        );
        ensure!(
            self.retirement_failure.is_none(),
            "target retirement failure blocks source94"
        );
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        self.verify_control_record(selected)?;
        let stage = self.staging.as_mut().context("source94 stage missing")?;
        stage.slots[0]
            .outcome
            .as_ref()
            .context("source94 Motion publication missing")?
            .verify_mutable_stage_identity()?;
        stage.slots[1]
            .outcome
            .as_ref()
            .context("source94 native publication missing")?
            .verify_mutable_stage_identity()?;
        let held = stage
            .control
            .as_mut()
            .context("source94 control missing")?
            .continuation
            .as_mut()
            .context("source94 original service continuation missing")?;
        self.phase = TargetPhase::Source94;
        if let Some(request) = request {
            held.source94_goal = goal;
            ensure!(
                held.relocation.is_none(),
                "source94 destination cannot be replaced"
            );
            RetainedSourceRelocation94::admit(
                held.source
                    .as_mut()
                    .context("source94 exact source session missing")?,
                &pair._stopped,
                request,
                self.deadline,
                &shared.cancel,
            )?;
            match RetainedSourceRelocation94::from_admitted(held.source.take().unwrap()) {
                Ok(operation) => held.relocation = Some(operation),
                Err(source) => {
                    held.source = Some(source);
                    anyhow::bail!("source94 admission retained source whole");
                }
            }
        } else {
            ensure!(
                goal == Source94Goal::Complete && held.source94_goal == Source94Goal::ReceiptOnly,
                "source94 reconciliation requires deliberate receipt checkpoint"
            );
            ensure!(held.source.is_none(), "source94 cannot revive source96");
            held.relocation
                .as_mut()
                .context("source94 same-owner receipt checkpoint missing")?
                .claim_reconcile(&shared.cancel)?;
        }
        // The genuine whole owner is already in its stable destination before
        // this final check or construction/spawn of the worker and timer.
        ensure!(
            held.relocation.as_ref().unwrap().deadline() == self.deadline,
            "source94 backend changed original deadline"
        );
        Ok(self.deadline)
    }
    pub(in crate::native_service) async fn run_source94(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        if self.phase == TargetPhase::Source94PairedCapturing {
            return self.run_source94_paired_capture(pair, selection, shared).await;
        }
        if self.phase == TargetPhase::Source94NativeCapturing {
            return self.run_source94_native_capture(pair, selection, shared).await;
        }
        if self.phase == TargetPhase::Source94CaptureAcquiring {
            return self.run_source94_capture_acquisition(pair, selection, shared).await;
        }
        if self.phase == TargetPhase::Source94ClosingTarget {
            return self.run_source94_capture_input(pair, selection, shared).await;
        }
        ensure!(
            self.phase == TargetPhase::Source94,
            "source94 must be synchronously claimed"
        );
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "source94 selection deadline changed"
        );
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        self.verify_control_record(selected)?;
        let stage = self.staging.as_mut().context("source94 stage missing")?;
        let held = stage
            .control
            .as_mut()
            .context("source94 control missing")?
            .continuation
            .as_mut()
            .context("source94 continuation missing")?;
        let operation = held
            .relocation
            .as_mut()
            .context("source94 stable owner missing")?;
        #[cfg(test)]
        super::super::super::super::source94::source94_boundary(shared, 1);
        if operation.reconcile_claimed() {
            operation
                .authorize_service(
                    Purpose::Reconcile,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    #[cfg(test)]
                    shared,
                )
                .await?;
        } else {
            #[cfg(test)]
            if shared.test.source94_unpolled.load(Ordering::Acquire) == 1 {
                let work = operation.authorize_service(
                    Purpose::Intent,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    shared,
                );
                drop(work);
                anyhow::bail!("injected unpolled source94 intent waiter");
            }
            operation
                .authorize_service(
                    Purpose::Intent,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    #[cfg(test)]
                    shared,
                )
                .await?;
            #[cfg(test)]
            super::super::super::super::source94::source94_boundary(shared, 2);
            #[cfg(test)]
            if shared.test.source94_unpolled.load(Ordering::Acquire) == 2 {
                let work = operation.authorize_service(
                    Purpose::Dispatch,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    shared,
                );
                drop(work);
                anyhow::bail!("injected unpolled source94 dispatch waiter");
            }
            operation
                .authorize_service(
                    Purpose::Dispatch,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    #[cfg(test)]
                    shared,
                )
                .await?;
            ensure!(
                operation.receipt_pending(),
                "source94 receipt checkpoint needs rollback and normal reader ACK"
            );
            #[cfg(test)]
            super::super::super::super::source94::source94_boundary(shared, 3);
            if held.source94_goal == Source94Goal::ReceiptOnly {
                stage.slots[0]
                    .outcome
                    .as_ref()
                    .unwrap()
                    .verify_mutable_stage_identity()?;
                stage.slots[1]
                    .outcome
                    .as_ref()
                    .unwrap()
                    .verify_mutable_stage_identity()?;
                pair._stopped.verify_lock()?;
                alive(self.deadline, &shared.cancel)?;
                self.phase = TargetPhase::ReceiptPending;
                return Ok(());
            }
            #[cfg(test)]
            if shared.test.source94_unpolled.load(Ordering::Acquire) == 3 {
                let work = operation.authorize_service(
                    Purpose::Ack,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    shared,
                );
                drop(work);
                anyhow::bail!("injected unpolled source94 ACK waiter");
            }
            operation
                .authorize_service(
                    Purpose::Ack,
                    &self.caller,
                    &mut pair._writer,
                    &shared.cancel,
                    #[cfg(test)]
                    shared,
                )
                .await?;
        }
        #[cfg(test)]
        super::super::super::super::source94::source94_boundary(shared, 4);
        ensure!(
            operation.ready(),
            "source94 ACK lacks actual source-reader retirement"
        );
        stage.slots[0]
            .outcome
            .as_ref()
            .unwrap()
            .verify_mutable_stage_identity()?;
        stage.slots[1]
            .outcome
            .as_ref()
            .unwrap()
            .verify_mutable_stage_identity()?;
        pair._stopped.verify_lock()?;
        alive(self.deadline, &shared.cancel)?;
        self.phase = TargetPhase::Source94Ready;
        Ok(())
    }
    pub(in crate::native_service) async fn settle_source94(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        if let Some(held) = self.staging.as_mut()
            .and_then(|s| s.control.as_mut()).and_then(|c| c.continuation.as_mut())
        {
            if let Some(ingress) = held.capture_ingress.as_mut() {
                // Keep both original workspace and whole94 in the same service
                // slot throughout cleanup and after any returned failure.
                ingress.settle_service(writer).await?;
            }
            if let Some(operation) = held.relocation.as_mut() {
                operation.settle_service(writer).await?;
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source94_capture_input_evidence(&self) -> Option<(bool, bool)> {
        self.staging.as_ref()?.control.as_ref()?.continuation.as_ref()?.relocation.as_ref()
            .map(RetainedSourceRelocation94::capture_input_evidence)
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source94_custody_evidence(&self) -> Option<(usize, usize)> {
        self.staging.as_ref()?.control.as_ref()?.continuation.as_ref()?.relocation.as_ref()
            .map(RetainedSourceRelocation94::custody_evidence)
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source94_evidence(
        &self,
    ) -> Option<(
        motion_catalog_backend::SourceRelocation94Phase,
        bool,
        bool,
        bool,
    )> {
        self.staging
            .as_ref()?
            .control
            .as_ref()?
            .continuation
            .as_ref()?
            .relocation
            .as_ref()
            .map(RetainedSourceRelocation94::service_evidence)
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source94_retained_evidence(
        &self,
    ) -> Option<(
        Option<bool>,
        bool,
        bool,
        motion_catalog_backend::SourceRelocation94CommitState,
        motion_catalog_backend::SourceRelocation94CommitState,
    )> {
        self.staging
            .as_ref()?
            .control
            .as_ref()?
            .continuation
            .as_ref()?
            .relocation
            .as_ref()
            .map(RetainedSourceRelocation94::retained_evidence)
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source94_successor(
        &self,
    ) -> Option<&motion_catalog_backend::source_lifecycle::LifecycleProof> {
        self.staging
            .as_ref()?
            .control
            .as_ref()?
            .continuation
            .as_ref()?
            .relocation
            .as_ref()?
            .successor()
    }
}

#[cfg(test)]
#[path = "source94/capture_ingress_fixture.rs"]
mod capture_ingress_fixture;

#[path = "source94/service_capture_ingress.rs"]
mod service_capture_ingress;
