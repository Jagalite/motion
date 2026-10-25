//! Actual service-owned Ready97 → native locks → exact durable intent96.
//! Same cooperative host-lock contract, not hostile-namespace FD adoption.
use super::*;

impl ServiceTarget {
    pub(in crate::native_service) fn claim_native96(
        &mut self,
        workspace: &mut Option<AuthorizationWorkspace>,
        stopped: &StoppedService,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Instant> {
        alive(self.deadline, cancel)?;
        ensure!(
            self.phase == TargetPhase::Ready97 && self.retirement_failure.is_none(),
            "native96 requires exact Ready97"
        );
        let held_workspace = workspace
            .as_ref()
            .context("original service workspace missing")?;
        held_workspace.verify_service(stopped)?;
        ensure!(
            held_workspace.reusable(),
            "Ready97 current reader not acknowledged closed"
        );
        let control = self
            .staging
            .as_mut()
            .context("staging missing")?
            .control
            .as_mut()
            .context("control missing")?;
        ensure!(
            control.ready97.is_none()
                && control.intent.is_none()
                && control.ready96.is_none()
                && control
                    .operation
                    .as_ref()
                    .is_some_and(|op| op.phase() == RecordedControlPhase::Ready97),
            "native96 owner cannot retry"
        );
        self.phase = TargetPhase::Native96;
        // Moves only, with both possible results stored before fallible work.
        match control.operation.take().unwrap().into_admitted() {
            Ok(ready) => {
                control.ready97 = Some(ConnectedRecorded97::from_service(ServiceRecorded97 {
                    ready,
                    workspace: workspace.take().expect("verified same workspace"),
                }));
            }
            Err(operation) => {
                control.operation = Some(operation);
                anyhow::bail!("recorded97 handoff refused with original owner retained");
            }
        }
        Ok(self.deadline)
    }

    pub(in crate::native_service) async fn run_native96(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == TargetPhase::Native96,
            "native96 must be claimed before worker"
        );
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "native96 deadline changed"
        );
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        let facts = pair
            ._stopped
            .supervised
            .as_ref()
            .context("stopped service proof missing")?
            .control_shutdown_facts()?;
        self.observe_staging(
            selected,
            &mut pair.motion,
            &mut pair.native,
            &pair.parent,
            pair.parent_identity,
            &pair.manifest,
            &pair._stopped,
            shared,
        )?;
        self.verify_control_record(selected)?;
        let stage = self.staging.as_mut().unwrap();
        let control = stage.control.as_mut().unwrap();
        let record = stage.record.as_ref().unwrap();
        // Two path buffers coexist inside the original four-path share.
        let capacity = self
            .path
            .as_os_str()
            .len()
            .checked_add(1 + record.native_stage.name.len())
            .context("native stage path overflow")?;
        ensure!(
            capacity as u64 <= PATH_STORAGE / 4,
            "native stage path exceeds admitted share"
        );
        control.native_path = PathBuf::with_capacity(capacity);
        control.native_path.push(&self.path);
        control.native_path.push(&record.native_stage.name);
        ensure!(
            control.native_path.capacity() as u64 <= PATH_STORAGE / 4,
            "native stage path capacity exceeded"
        );
        RetainedNativeIntent::admit_connected_recorded(
            control.ready97.as_mut().context("whole Ready97 missing")?,
            &control.native_path,
            catabolic_store::migration::NativeUpgradeQuiescence {
                admission_stopped: facts.service_admission_stopped,
                readers_and_workers_stopped: facts.native_workers_joined
                    && facts.application_owners_drained,
                exclusive_operational_ownership: facts.exclusive_operational_ownership_retained,
            },
            &record.intended_transition_id,
            self.deadline,
            &shared.cancel,
        )?;
        match RetainedNativeIntent::from_connected_recorded(control.ready97.take().unwrap()) {
            Ok(intent) => control.intent = Some(intent),
            Err(ready) => {
                control.ready97 = Some(ready);
                anyhow::bail!("native96 admission handoff refused");
            }
        }
        // This fixed method claims app phase/workspace BEFORE even constructing
        // the original FreshWriter's acquisition future. The actual reader/open/
        // close slots remain on their independently retained owners on drop.
        control
            .intent
            .as_mut()
            .unwrap()
            .authorize_service_locked(
                &self.caller,
                &mut pair._writer,
                stage.slots[1].outcome.as_mut().unwrap(),
                &record.native_stage,
                &mut control.native_fd,
                #[cfg(test)]
                shared,
                &shared.cancel,
            )
            .await?;
        // Auth workspace is genuinely retired before this moves-only result.
        match control.intent.take().unwrap().into_connected_ready() {
            Ok(ready) => control.ready96 = Some(ready),
            Err(intent) => {
                control.intent = Some(intent);
                anyhow::bail!("native96 ready handoff refused");
            }
        }
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
        self.phase = TargetPhase::ReadyIntent96;
        Ok(())
    }

    /// Terminal cleanup finds the same workspace even after its typed handoff.
    /// No effect, retry, source authority or new native owner is produced.
    pub(in crate::native_service) async fn settle_native96(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        if let Some(control) = self.staging.as_mut().and_then(|s| s.control.as_mut()) {
            if let Some(ready) = control.ready97.as_mut() {
                ready.settle_service(writer).await?;
            }
            if let Some(intent) = control.intent.as_mut() {
                intent.settle_service(writer).await?;
            }
            if let Some(ready) = control.ready96.as_mut() {
                ready.settle_service(writer).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl ServiceTarget {
    pub(in crate::native_service::pair) fn native96_evidence(
        &self,
    ) -> (
        bool,
        bool,
        Option<(
            motion_catalog_backend::NativeIntentPreparationPhase,
            motion_catalog_backend::NativeIntentCommitState,
            bool,
        )>,
        Option<(bool, motion_catalog_backend::NativeIntentCommitState)>,
        Option<i32>,
    ) {
        let control = self.staging.as_ref().unwrap().control.as_ref().unwrap();
        (
            control.operation.is_some(),
            control.ready97.is_some(),
            control
                .intent
                .as_ref()
                .map(RetainedNativeIntent::service_evidence),
            control
                .ready96
                .as_ref()
                .map(ConnectedIntent96::service_evidence),
            control.native_fd.as_ref().map(AsRawFd::as_raw_fd),
        )
    }
}
