//! Fixed service-owned continuation. Native algorithms remain in retained wrappers.
use super::*;
use crate::native_service::pair::restore_input::target::conversion::source_conversion::{
    retained_native_apply::{ConnectedPending96, RetainedNativeApply},
    retained_native_bootstrap::{ConnectedSource96, ConnectedSourceClose, RetainedNativeBootstrap},
};
const COMMAND_STORAGE: u64 = 512;
pub(super) const fn storage_reservation() -> u64 {
    2 * std::mem::size_of::<ServiceContinuation>() as u64
        + std::mem::size_of::<Result<RetainedNativeApply, ConnectedIntent96>>() as u64
        + std::mem::size_of::<Result<ConnectedPending96, RetainedNativeApply>>() as u64
        + std::mem::size_of::<Result<RetainedNativeBootstrap, ConnectedPending96>>() as u64
        + std::mem::size_of::<Result<ConnectedSource96, RetainedNativeBootstrap>>() as u64
        + std::mem::size_of::<Result<ConnectedSourceClose, ConnectedSource96>>() as u64
        + COMMAND_STORAGE
        + super::source94::storage_reservation()
}
/// Preadmitted empty destinations. No request future becomes their sole owner.
pub(super) struct ServiceContinuation {
    operation_id: String,
    apply: Option<RetainedNativeApply>,
    pending: Option<ConnectedPending96>,
    bootstrap: Option<RetainedNativeBootstrap>,
    pub(super) source: Option<ConnectedSource96>,
    pub(super) source94_goal: Source94Goal,
    pub(super) relocation: Option<crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_source_relocation::RetainedSourceRelocation94>,
    pub(super) capture_ingress: Option<crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_source_relocation::RetainedServiceCaptureIngress94>,
    closing: Option<ConnectedSourceClose>,
}
impl ServiceContinuation {
    fn empty() -> Self {
        Self {
            operation_id: String::new(),
            apply: None,
            pending: None,
            bootstrap: None,
            source: None,
            relocation: None,
            capture_ingress: None,
            source94_goal: Source94Goal::Complete,
            closing: None,
        }
    }
    fn prepare_operation_id(&mut self, install: &str) -> anyhow::Result<()> {
        ensure!(
            self.operation_id.is_empty() && install.len() == 36,
            "source96 original install identity differs"
        );
        let length = "paired-bootstrap-"
            .len()
            .checked_add(install.len())
            .context("bootstrap ID overflow")?;
        ensure!(
            length as u64 <= COMMAND_STORAGE,
            "bootstrap ID exceeds admitted share"
        );
        self.operation_id = String::with_capacity(length);
        self.operation_id.push_str("paired-bootstrap-");
        self.operation_id.push_str(install);
        ensure!(
            self.operation_id.capacity() as u64 <= COMMAND_STORAGE,
            "bootstrap ID capacity exceeds admitted share"
        );
        Ok(())
    }
}
impl ServiceTarget {
    pub(in crate::native_service) fn claim_source96(
        &mut self,
        stopped: &StoppedService,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Instant> {
        alive(self.deadline, cancel)?;
        stopped.verify_lock()?;
        ensure!(
            self.phase == TargetPhase::ReadyIntent96 && self.retirement_failure.is_none(),
            "source96 requires exact joined intent"
        );
        let control = self
            .staging
            .as_mut()
            .context("staging missing")?
            .control
            .as_mut()
            .context("control missing")?;
        ensure!(
            control.ready96.is_some() && control.continuation.is_none(),
            "source96 cannot retry"
        );
        self.phase = TargetPhase::Source96;
        control.continuation = Some(Box::new(ServiceContinuation::empty()));
        Ok(self.deadline)
    }
    pub(in crate::native_service) async fn run_source96(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == TargetPhase::Source96,
            "source96 must be claimed before worker"
        );
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "source96 deadline changed"
        );
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        self.verify_control_record(selected)?;
        // Both stage images are legitimately mutable. Never restore old hashes as authority.
        let stage = self.staging.as_mut().context("staging missing")?;
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
        let control = stage.control.as_mut().unwrap();
        let held = control
            .continuation
            .as_mut()
            .context("source96 slot missing")?;
        held.prepare_operation_id(&stage.record.as_ref().unwrap().install_id)?;
        RetainedNativeApply::admit_connected(
            control.ready96.as_mut().unwrap(),
            None,
            self.deadline,
            &shared.cancel,
        )?;
        match RetainedNativeApply::from_connected(control.ready96.take().unwrap()) {
            Ok(operation) => held.apply = Some(operation),
            Err(ready) => {
                control.ready96 = Some(ready);
                anyhow::bail!("native apply handoff retained exact intent");
            }
        }
        #[cfg(test)]
        super::super::super::super::source96::source96_boundary(shared, 1);
        #[cfg(test)]
        held.apply.as_mut().unwrap().service_panic_after_effect(
            shared.test.source96_inner_panic.load(Ordering::Acquire) == 2,
        );
        #[cfg(test)]
        if shared.test.source96_unpolled.load(Ordering::Acquire) == 1 {
            let work = held.apply.as_mut().unwrap().authorize_service(
                &self.caller,
                &mut pair._writer,
                &shared.cancel,
            );
            drop(work);
            anyhow::bail!("injected unpolled source96 authorization waiter");
        }
        held.apply
            .as_mut()
            .unwrap()
            .authorize_service(&self.caller, &mut pair._writer, &shared.cancel)
            .await?;
        #[cfg(test)]
        super::super::super::super::source96::source96_boundary(shared, 2);
        match held.apply.take().unwrap().into_connected_pending() {
            Ok(pending) => held.pending = Some(pending),
            Err(operation) => {
                held.apply = Some(operation);
                anyhow::bail!("native apply result retained");
            }
        }
        RetainedNativeBootstrap::admit(
            held.pending.as_mut().unwrap(),
            &held.operation_id,
            catabolic_store::catalog_commands::ReceiptPolicy {
                retention: std::time::Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 100,
                max_logical_bytes: 16 * 1024 * 1024,
            },
            self.deadline,
            &shared.cancel,
        )?;
        match RetainedNativeBootstrap::from_admitted(held.pending.take().unwrap()) {
            Ok(operation) => held.bootstrap = Some(operation),
            Err(pending) => {
                held.pending = Some(pending);
                anyhow::bail!("bootstrap admission retained applied owner");
            }
        }
        #[cfg(test)]
        held.bootstrap.as_mut().unwrap().service_panic_after_effect(
            shared.test.source96_inner_panic.load(Ordering::Acquire) == 3,
        );
        #[cfg(test)]
        if shared.test.source96_unpolled.load(Ordering::Acquire) == 2 {
            let work = held.bootstrap.as_mut().unwrap().authorize_service_command(
                &self.caller,
                &mut pair._writer,
                &shared.cancel,
            );
            drop(work);
            anyhow::bail!("injected unpolled source96 authorization waiter");
        }
        held.bootstrap
            .as_mut()
            .unwrap()
            .authorize_service_command(&self.caller, &mut pair._writer, &shared.cancel)
            .await?;
        #[cfg(test)]
        super::super::super::super::source96::source96_boundary(shared, 3);
        #[cfg(test)]
        held.bootstrap.as_mut().unwrap().service_panic_after_effect(
            shared.test.source96_inner_panic.load(Ordering::Acquire) == 4,
        );
        #[cfg(test)]
        if shared.test.source96_unpolled.load(Ordering::Acquire) == 3 {
            let work = held.bootstrap.as_mut().unwrap().authorize_service_execute(
                &self.caller,
                &mut pair._writer,
                &shared.cancel,
            );
            drop(work);
            anyhow::bail!("injected unpolled source96 authorization waiter");
        }
        held.bootstrap
            .as_mut()
            .unwrap()
            .authorize_service_execute(&self.caller, &mut pair._writer, &shared.cancel)
            .await?;
        #[cfg(test)]
        super::super::super::super::source96::source96_boundary(shared, 4);
        #[cfg(test)]
        held.bootstrap.as_mut().unwrap().service_panic_after_effect(
            shared.test.source96_inner_panic.load(Ordering::Acquire) == 5,
        );
        #[cfg(test)]
        if shared.test.source96_unpolled.load(Ordering::Acquire) == 4 {
            let work = held.bootstrap.as_mut().unwrap().authorize_service_ack(
                &self.caller,
                &mut pair._writer,
                &shared.cancel,
            );
            drop(work);
            anyhow::bail!("injected unpolled source96 authorization waiter");
        }
        held.bootstrap
            .as_mut()
            .unwrap()
            .authorize_service_ack(&self.caller, &mut pair._writer, &shared.cancel)
            .await?;
        #[cfg(test)]
        super::super::super::super::source96::source96_boundary(shared, 5);
        match held.bootstrap.take().unwrap().into_source() {
            Ok(source) => held.source = Some(source),
            Err(operation) => {
                held.bootstrap = Some(operation);
                anyhow::bail!("source handoff retained bootstrap");
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
        self.phase = TargetPhase::SourceReady;
        Ok(())
    }
    pub(in crate::native_service) async fn settle_source96(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        let Some(held) = self
            .staging
            .as_mut()
            .and_then(|s| s.control.as_mut())
            .and_then(|c| c.continuation.as_mut())
        else {
            return Ok(());
        };
        if let Some(operation) = held.apply.as_mut() {
            operation.settle_service(writer).await?;
        }
        if let Some(pending) = held.pending.as_mut() {
            pending.settle_service(writer).await?;
        }
        if let Some(operation) = held.bootstrap.as_mut() {
            operation.settle_service(writer).await?;
        }
        if let Some(source) = held.source.take() {
            match source.into_service_close() {
                Ok(close) => held.closing = Some(close),
                Err(source) => {
                    held.source = Some(source);
                    anyhow::bail!("retained source close admission missing");
                }
            }
        }
        if let Some(close) = held.closing.as_mut() {
            if !close.closed() {
                close.settle_service(writer).await?;
            }
            ensure!(close.closed(), "source close not acknowledged");
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source96_states(
        &self,
    ) -> (
        bool,
        bool,
        bool,
        bool,
        Option<(
            motion_catalog_backend::NativeBootstrapPhase,
            motion_catalog_backend::NativeBootstrapCommitState,
            motion_catalog_backend::NativeBootstrapCommitState,
        )>,
    ) {
        let c = self
            .staging
            .as_ref()
            .unwrap()
            .control
            .as_ref()
            .unwrap()
            .continuation
            .as_ref()
            .unwrap();
        (
            c.apply.is_some(),
            c.pending.is_some(),
            c.bootstrap.is_some(),
            c.source.is_some(),
            c.bootstrap
                .as_ref()
                .map(RetainedNativeBootstrap::service_states),
        )
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn source96_identity(
        &self,
    ) -> Option<&playscale_core::catalog_backend::Identity> {
        self.staging
            .as_ref()?
            .control
            .as_ref()?
            .continuation
            .as_ref()?
            .source
            .as_ref()
            .map(ConnectedSource96::service_identity)
    }
}
