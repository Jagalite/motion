//! Finite service-owned recorded93→97. Never consumes the legacy stage helper,
//! transfers a sole owner to a waiter, constructs a second auth workspace, or
//! produces native96/paired95/install authority. Service destruction and legacy
//! terminal-close failure remain explicit activation blockers.
use super::*;
use crate::v2::auth::retained::TargetAuthentication;
use motion_catalog_backend::{RecordedControlOperation, RecordedControlPhase};
use crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_native_intent::{ConnectedRecorded97, ConnectedIntent96, RetainedNativeIntent};
mod native96;
mod source94;
mod source96;
use source96::ServiceContinuation;

// Existing service admission owns the64MiB current+target query cohort and
// current close slot. These additive fixed shares cover only this bridge's
// paths, pending descriptor slots, backend owner/result layouts and auth frames.
const PATH_STORAGE: u64 = 4 * (libc::PATH_MAX as u64 + 256);
const AUTH_LAYOUT: u64 = 2
    * (std::mem::size_of::<CurrentSourceAuthentication<'static>>()
        + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>()
        + std::mem::size_of::<TargetAuthentication<'static>>()
        + std::mem::size_of::<Result<TargetAuthentication<'static>, crate::v2::Problem>>()
        + std::mem::size_of::<
            motion_catalog_backend::motion_snapshot::RetainedAuthorizationReader<'static>,
        >()
        + std::mem::size_of::<
            Result<
                motion_catalog_backend::motion_snapshot::RetainedAuthorizationReader<'static>,
                anyhow::Error,
            >,
        >()
        + std::mem::size_of::<motion_catalog_backend::TargetAuthorizationReader<'static>>()
        + std::mem::size_of::<
            Result<motion_catalog_backend::TargetAuthorizationReader<'static>, anyhow::Error>,
        >()
        + std::mem::size_of::<Result<(), crate::v2::Problem>>()
        + std::mem::size_of::<Result<(), anyhow::Error>>()) as u64;
pub(super) const fn storage_reservation() -> u64 {
    2 * std::mem::size_of::<ServiceControl>() as u64
        + RecordedControlOperation::inline_storage_reservation()
        + std::mem::size_of::<
            Result<motion_catalog_backend::RecordedAdmittedControl, RecordedControlOperation>,
        >() as u64
        + std::mem::size_of::<super::super::super::ServiceRecorded97>() as u64
        + PATH_STORAGE
        + TASK_STORAGE
        + source96::storage_reservation()
}

pub(super) struct ServiceControl {
    path: PathBuf,
    // Clones are installed individually before any subsequent fallible call.
    // On successful assembly only infallible moves replace these by operation.
    stage: Option<File>,
    parent: Option<File>,
    operation: Option<RecordedControlOperation>,
    ready97: Option<ConnectedRecorded97>,
    intent: Option<RetainedNativeIntent>,
    ready96: Option<ConnectedIntent96>,
    native_path: PathBuf,
    native_fd: Option<File>,
    continuation: Option<Box<ServiceContinuation>>,
}
impl ServiceControl {
    pub(super) fn has_operation(&self) -> bool {
        self.operation.is_some()
            || self.ready97.is_some()
            || self.intent.is_some()
            || self.ready96.is_some()
            || self.continuation.is_some()
    }
    fn new(parent: &Path, name: &str) -> anyhow::Result<Self> {
        ensure!(
            name.len() <= 200,
            "control stage leaf exceeds admitted share"
        );
        let capacity = parent
            .as_os_str()
            .len()
            .checked_add(1 + name.len())
            .context("control stage path overflow")?;
        ensure!(
            capacity as u64 <= PATH_STORAGE / 4,
            "control path share exceeded"
        );
        let mut path = PathBuf::with_capacity(capacity);
        path.push(parent);
        path.push(name);
        ensure!(
            path.capacity() as u64 <= PATH_STORAGE / 4,
            "control path capacity exceeded"
        );
        Ok(Self {
            path,
            stage: None,
            parent: None,
            operation: None,
            ready97: None,
            intent: None,
            ready96: None,
            native_path: PathBuf::new(),
            native_fd: None,
            continuation: None,
        })
    }
}

impl ServiceTarget {
    /// Claim before any returned reader future, path allocation or task exists.
    /// The same caller, selected deadline and service workspace remain retained.
    pub(in crate::native_service) fn claim_control97(
        &mut self,
        workspace: &AuthorizationWorkspace,
        stopped: &StoppedService,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Instant> {
        alive(self.deadline, cancel)?;
        ensure!(
            self.phase == TargetPhase::Staged,
            "control97 requires exact Staged target"
        );
        workspace.verify_service(stopped)?;
        ensure!(
            workspace.reusable() && self.retirement_failure.is_none(),
            "staging authorization is not actually retired"
        );
        let stage = self
            .staging
            .as_mut()
            .context("original staging owner missing")?;
        ensure!(stage.control.is_none(), "control97 cannot retry");
        let record = stage
            .record
            .as_ref()
            .context("actual conversion record missing")?;
        self.phase = TargetPhase::Control97;
        stage.control = Some(Box::new(ServiceControl::new(
            &self.path,
            &record.motion_stage.name,
        )?));
        Ok(self.deadline)
    }

    pub(in crate::native_service) async fn run_control97(
        &mut self,
        pair: &mut PreparedPair,
        selection: &mut ServiceSelection,
        workspace: &mut AuthorizationWorkspace,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == TargetPhase::Control97,
            "control97 must be claimed synchronously"
        );
        ensure!(
            selection.target_deadline(&shared.cancel)? == self.deadline,
            "control97 cannot refresh original deadline"
        );
        workspace.verify_service(&pair._stopped)?;
        let selected = selection.target_input(&shared.cancel)?;
        selected.target_binding(pair, &shared.cancel)?;
        let facts = pair
            ._stopped
            .supervised
            .as_ref()
            .context("control97 requires actual stopped service proof")?
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
        #[cfg(test)]
        boundary(shared, 1);
        {
            let stage = self.staging.as_mut().unwrap();
            let control = stage
                .control
                .as_mut()
                .context("claimed control slot missing")?;
            let record = stage.record.as_ref().unwrap();
            stage.slots[0]
                .outcome
                .as_mut()
                .unwrap()
                .pin_service_control(
                    &record.motion_stage,
                    &mut control.stage,
                    &mut control.parent,
                )?;
            // Bind the publisher's parent clone to the original retained target
            // before allowing the backend to use it for durable record effects.
            ensure!(
                identity(control.parent.as_ref().unwrap())?
                    == identity(self.parent.as_ref().unwrap())?,
                "control record parent differs from reserved target"
            );
            #[cfg(test)]
            boundary(shared, 2);
            // Immutable publisher authority was permanently revoked above.
            // Backend prepare independently authenticates exact original93
            // against retained selected bytes/reference under its own writer;
            // identity alone cannot grant preparation or subsequent preflight.
            // Any failure before that is terminal and never mints Ready97.
            control.operation = Some(RecordedControlOperation::begin(
                control.stage.take().unwrap(),
                control.parent.take().unwrap(),
            ));
        }
        // Existing counters, original current writer and native export are
        // measured before backend preparation. The existing auth charge is in
        // retained_native_allocations via StoppedService::retained_restore_storage.
        let external = pair
            ._writer
            .retained_allocations()
            .await?
            .checked_add(pair.retained_native_allocations()?)
            .and_then(|n| n.checked_add(OVERHEAD))
            .context("service control external cohort overflow")?;
        ensure!(
            external <= 256 * 1024 * 1024,
            "service control cohort exceeded"
        );
        {
            let stage = self.staging.as_mut().unwrap();
            let control = stage.control.as_mut().unwrap();
            let record = stage.record.as_ref().unwrap();
            let (original, inspected) = selected.service_control_input();
            control
                .operation
                .as_mut()
                .unwrap()
                .prepare(
                    &control.path,
                    original,
                    inspected,
                    &record.intended_transition_id,
                    &record.pair_sha256,
                    &facts,
                    external,
                    self.deadline,
                    &shared.cancel,
                )
                .await?;
            // The actual preexisting service workspace is separately verified;
            // this small named incremental admission prevents legacy apply and
            // covers two simultaneous copies of the explicit token/result layouts
            // above. Async task frames were admitted before spawn. This is NOT
            // another64MiB cohort, nor a repeated acquisition allowance.
            control
                .operation
                .as_mut()
                .unwrap()
                .admit_retained_authorization(AUTH_LAYOUT, &shared.cancel)?;
        }
        // Complete selected/current/native-stage hash, Motion-stage identity, ancestry, sidecar,
        // reservation and record observations BEFORE fresh app authentication.
        // The Motion publisher's initial hash grant was revoked before prepare.
        // Backend original preservation owns all later mutable-state verification.
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
        #[cfg(test)]
        boundary(shared, 3);
        workspace.begin()?; // synchronous, before even an unpolled reader future
        let reader = pair._writer.authorization_reader().await?;
        let current =
            CurrentSourceAuthentication::prepare(reader, &self.caller, Permission::SystemAdmin)
                .await
                .map_err(auth_error)?;
        let operation = self
            .staging
            .as_mut()
            .unwrap()
            .control
            .as_mut()
            .unwrap()
            .operation
            .as_mut()
            .unwrap();
        let deadline = operation
            .deadline()
            .context("retained control deadline missing")?;
        ensure!(
            deadline <= self.deadline,
            "backend extended original deadline"
        );
        // Catch while the exact real current token remains available to retire.
        // A failed target prepare/open leaves its own acquisition in operation.
        let result = catch_async(async {
            let reader = operation.authorization_reader().await?;
            let mut target =
                TargetAuthentication::prepare(reader, &self.caller, Permission::SystemAdmin)
                    .await
                    .map_err(target_error)?;
            target
                .close_retained_control_reader()
                .await
                .map_err(target_error)?;
            target
                .preflight_retained_control(&shared.cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            boundary(shared, 4);
            temporal_fence(&current, &target, deadline, &shared.cancel)?;
            target
                .apply_retained_control(&shared.cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            boundary(shared, 5);
            // Migration, preservation scans and durable record publication may
            // consume authority. No scan/callback/await follows this second fence.
            temporal_fence(&current, &target, deadline, &shared.cancel)?;
            target
                .commit_retained_control(&shared.cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            boundary(shared, 7);
            target
                .finish_retained_control(&shared.cancel)
                .map_err(target_error)?;
            drop(target);
            ensure!(
                operation.phase() == RecordedControlPhase::Ready97,
                "backend commit/reacquisition is not Ready97"
            );
            Ok::<(), anyhow::Error>(())
        })
        .await;
        if matches!(&result, Ok(Ok(()))) {
            self.phase = TargetPhase::Ready97Unretired;
        }
        // Retire the independently retained original writer reader on every
        // caught target result; terminal close handles prepare failure/panic too.
        #[cfg(test)]
        let retirement_boundary =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| boundary(shared, 6)));
        if let Err(error) = workspace
            .retire_current(current, deadline, &shared.cancel)
            .await
        {
            self.retirement_failure = Some(error);
        }
        #[cfg(test)]
        if let Err(panic) = retirement_boundary {
            std::panic::resume_unwind(panic);
        }
        match result {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(Err(error)) => Err(error),
            Ok(Ok(())) => {
                ensure!(
                    self.retirement_failure.is_none() && workspace.reusable(),
                    "control97 source authorization retirement incomplete"
                );
                // Initial Motion bytes have legitimately changed. Check only
                // the held/named identity, never its immutable93 artifact hash.
                self.staging.as_ref().unwrap().slots[0]
                    .outcome
                    .as_ref()
                    .unwrap()
                    .verify_mutable_stage_identity()?;
                pair._stopped.verify_lock()?;
                alive(deadline, &shared.cancel)?;
                self.phase = TargetPhase::Ready97;
                Ok(())
            }
        }
    }

    fn verify_control_record(&self, selected: &SelectedPairInput) -> anyhow::Result<()> {
        let stage = self.staging.as_ref().context("staging missing")?;
        let record = stage.record.as_ref().context("conversion record missing")?;
        let reservation = self.record.as_ref().context("reservation missing")?;
        let manifest = selected.service_manifest();
        ensure!(
            record.contract == 1
                && record.install_id == reservation.install_id
                && record.pair_sha256 == selected.target_archive().sha256()
                && record.reservation_sha256
                    == self
                        .reservation
                        .as_ref()
                        .unwrap()
                        .evidence()
                        .artifact
                        .sha256
                && record.source_motion_schema_version == 93
                && record.source_motion_image == manifest.motion_database
                && record.source_motion_inventory_sha256 == manifest.motion_control_journals_sha256
                && record.source_motion_binding_sha256 == manifest.motion_binding_sha256
                && record.source_native_identity == manifest.native_identity
                && record.source_native_image == manifest.native_database
                && record.source_native_inventory_sha256 == manifest.native_control_journals_sha256
                && record.motion_stage == *stage.slots[0].outcome.as_ref().unwrap().evidence()
                && record.native_stage == *stage.slots[1].outcome.as_ref().unwrap().evidence(),
            "service control requires original reservation-bound ConversionRecord"
        );
        ensure!(
            record
                .intended_transition_id
                .strip_prefix("paired-upgrade-")
                == Some(record.install_id.as_str()),
            "conversion transition linkage changed"
        );
        Ok(())
    }

    /// Terminal resource settlement is borrowed. No Retry/Ready/recovery grant,
    /// no unlink, no extraction, and no replacement of uncertain commit facts.
    pub(in crate::native_service) async fn settle_control97(&mut self) -> anyhow::Result<()> {
        if let Some(control) = self.staging.as_mut().and_then(|s| s.control.as_mut()) {
            if let Some(operation) = control.operation.as_mut() {
                operation.settle_authorization_reader().await?;
                operation.settle()?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::native_service) fn control97_initial_verification(&mut self) -> [bool; 3] {
        self.staging.as_mut().unwrap().slots.each_mut().map(|slot| {
            slot.outcome
                .as_mut()
                .unwrap()
                .read_verified(|_| Ok(()))
                .is_ok()
        })
    }
    #[cfg(test)]
    pub(in crate::native_service) fn control97_layout() -> (usize, u64, u64) {
        (
            std::mem::size_of::<ServiceControl>(),
            storage_reservation(),
            AUTH_LAYOUT,
        )
    }
    #[cfg(test)]
    pub(in crate::native_service) fn control97_evidence(
        &self,
    ) -> Option<(
        Option<i32>,
        Option<i32>,
        Option<RecordedControlPhase>,
        Option<motion_catalog_backend::RecordedCommitState>,
        Option<&'static str>,
    )> {
        self.staging.as_ref()?.control.as_ref().map(|control| {
            (
                control.stage.as_ref().map(AsRawFd::as_raw_fd),
                control.parent.as_ref().map(AsRawFd::as_raw_fd),
                control
                    .operation
                    .as_ref()
                    .map(RecordedControlOperation::phase),
                control
                    .operation
                    .as_ref()
                    .map(RecordedControlOperation::commit_state),
                control
                    .operation
                    .as_ref()
                    .and_then(RecordedControlOperation::publication_phase),
            )
        })
    }
}

fn temporal_fence(
    current: &CurrentSourceAuthentication<'_>,
    target: &TargetAuthentication<'_>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    current.revalidate_temporal().map_err(auth_error)?;
    target.revalidate_temporal().map_err(target_error)?;
    alive(deadline, cancel)
}
#[cfg(test)]
fn boundary(shared: &Shared, stage: u8) {
    super::super::super::control97::control97_boundary(shared, stage)
}

#[derive(Debug)]
struct TargetControlAuthorizationError(crate::v2::Problem);
impl std::fmt::Display for TargetControlAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "service control target authorization: {:?}", self.0)
    }
}
impl std::error::Error for TargetControlAuthorizationError {}
fn target_error(error: crate::v2::Problem) -> anyhow::Error {
    TargetControlAuthorizationError(error).into()
}
