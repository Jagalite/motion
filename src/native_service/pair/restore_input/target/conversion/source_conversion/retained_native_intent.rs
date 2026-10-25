//! Private, disconnected application authorization for retained Ready97 -> intent96.
//! The whole backend operation stays inline in the caller's retained slot through
//! every await, cancellation, caught unwind and unpolled-future drop. This does
//! not install a service slot, authorize native apply, or advance paired95.
//! Current authentication is prepared here for the same caller and permission
//! through a real reader borrowing the independently retained original writer.
//! Before opening a current reader, a future service driver must check/claim the
//! eligible retained phase. These private entries accept an already-open reader;
//! they do not pre-admit arbitrary repeated reader acquisition for rejected calls.
//! The original writers also retain readonly acquisition/reader custody when
//! authentication fails or its waiter is dropped. SQLx internal pre-return
//! failures, WorkerCrashed and service-owner destruction remain explicit limits;
//! retained acquisition is not a claim of hard-preemptible SQLx operations.
// Kept beneath this module so no sibling can split or arbitrarily re-pair the
// retained97 owner and its exact admitted workspace at this new handoff.
#[path = "retained_control.rs"]
mod retained_control;
pub(in crate::native_service::pair) use retained_control::ConnectedRecorded97;
pub(super) use retained_control::RetainedControl;

use super::{AUTHORIZATION_RESERVE, current_authorization_error};
use crate::v2::auth::{
    Caller,
    retained::{CurrentSourceAuthentication, TargetAuthentication},
};
use anyhow::ensure;
use motion_catalog_backend::{
    NativeIntentPreparationOperation, ReadyNativeIntent96, RecordedAdmittedControl,
    motion_snapshot::{FreshWriter, RetainedAuthorizationCloseSlot, RetainedAuthorizationReader},
};
use playscale_core::access::Permission;
use std::{
    future::Future,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Inputs,
    // Also the in-flight state. Dropping a waiter cannot restore Inputs.
    Interrupted,
    ReadyIntent96,
}

/// No Clone, backend accessor, extraction on error, replacement or retry.
/// Destruction of the containing service owner is an explicit activation blocker:
/// retaining this object is the caller's obligation, not a global Drop sink.
pub(in crate::native_service::pair) struct RetainedNativeIntent {
    operation: NativeIntentPreparationOperation,
    phase: Phase,
    workspace: Option<AuthorizationWorkspace>,
    #[cfg(test)]
    observation: Observation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspacePhase {
    Reusable,
    Active,
    Retiring,
    // Terminal service cleanup is not authorization or a reusable cohort.
    Settled,
}

/// Actual admitted accounting custody, not authentication evidence. There is no
/// scalar constructor or Clone. The checked connected79/retained97 constructors
/// and opaque actual-service admission can create this owner. Named target
/// reservation uses genuine current auth; later connected effects still require
/// both genuine tokens and a whole-owner service-provenance handoff.
pub(in crate::native_service::pair) struct AuthorizationWorkspace {
    // Present only for the actual service origin. It is opaque, non-Clone, and
    // minted after that exact Shared/current pair admitted the named cohort.
    service: Option<super::super::source_image::supervisor::ServiceAuthorizationAdmission>,
    source_close: RetainedAuthorizationCloseSlot,
    phase: WorkspacePhase,
    #[cfg(test)]
    pub(super) retirement_entered: Option<std::sync::Arc<AtomicBool>>,
    #[cfg(test)]
    pub(super) retirement_gate: Option<std::sync::Arc<tokio::sync::Notify>>,
}
impl AuthorizationWorkspace {
    pub(in crate::native_service::pair) fn has_service_provenance(&self) -> bool {
        self.service.is_some()
    }

    /// Named one-time service admission, not a generic H scalar. No service to
    /// connected97 handoff exists yet: it must move the entire target/workspace
    /// and verify this original stopped-service provenance, never swap owners.
    pub(in crate::native_service::pair) const fn service_storage_reservation() -> u64 {
        AUTHORIZATION_RESERVE
            + FreshWriter::authorization_storage_reservation()
            + RetainedAuthorizationCloseSlot::storage_reservation()
            + std::mem::size_of::<Self>() as u64
    }
    pub(in crate::native_service::pair) fn for_service(
        service: super::super::source_image::supervisor::ServiceAuthorizationAdmission,
    ) -> Self {
        Self {
            service: Some(service),
            source_close: RetainedAuthorizationCloseSlot::default(),
            phase: WorkspacePhase::Reusable,
            #[cfg(test)]
            retirement_entered: None,
            #[cfg(test)]
            retirement_gate: None,
        }
    }
    pub(in crate::native_service::pair) fn verify_service(
        &self,
        stopped: &crate::native_service::StoppedService,
    ) -> anyhow::Result<()> {
        self.service
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "service authorization requires its own admitted workspace provenance"
                )
            })?
            .verify(stopped)
    }
    /// Terminal service cleanup borrows the exact original writer after the
    /// worker/token lifetime ended. A failed prepare still left its real reader
    /// there; a dropped acquisition left that same opening frame there. Joining
    /// can settle those resources, never retry authentication or grant reuse.
    pub(in crate::native_service::pair) async fn settle_service_close(
        &mut self,
        writer: &mut FreshWriter,
    ) -> anyhow::Result<()> {
        ensure!(
            self.service.is_some(),
            "service workspace provenance missing"
        );
        self.settle_source_close(writer).await
    }

    /// Resource-only terminal cleanup for this exact retained workspace. It
    /// cannot yield a reusable cohort or a current authorization capability.
    pub(super) async fn settle_source_close(
        &mut self,
        writer: &mut FreshWriter,
    ) -> anyhow::Result<()> {
        match self.phase {
            WorkspacePhase::Reusable | WorkspacePhase::Settled => return Ok(()),
            WorkspacePhase::Active => {
                self.phase = WorkspacePhase::Retiring;
                writer.begin_retained_authorization_close(&mut self.source_close)?;
            }
            WorkspacePhase::Retiring => {}
        }
        // Pending/panicked/failed acquisition or close never manufactures ACK.
        // The backend may separately prove a never-polled acquisition launched
        // no worker. That is sufficient only for this terminal cleanup branch.
        self.source_close.join().await?;
        writer.settle_authorization_close(&mut self.source_close)?;
        self.phase = WorkspacePhase::Settled;
        Ok(())
    }
    /// Measurement only against this exact already-admitted active workspace.
    /// No scalar can create provenance, extend H or make a reader reusable.
    pub(in crate::native_service::pair) fn verify_capture_cohort(&self, measured: u64) -> anyhow::Result<()> {
        ensure!(self.service.is_some() && self.phase == WorkspacePhase::Active,
            "capture measurement requires the original active service workspace");
        ensure!(measured <= AUTHORIZATION_RESERVE, "capture actual readers exceed original admitted cohort");
        Ok(())
    }
    pub(in crate::native_service::pair) fn reusable(&self) -> bool {
        self.phase == WorkspacePhase::Reusable
    }

    /// Claim the one admitted auth cohort before a borrower future exists.
    pub(in crate::native_service::pair) fn begin(&mut self) -> anyhow::Result<()> {
        ensure!(self.reusable(), "authorization workspace is not reusable");
        self.phase = WorkspacePhase::Active;
        Ok(())
    }

    /// The actual SQLx close future is installed in this stable workspace
    /// synchronously. Its waiter owns only the temporary auth token and borrows
    /// the workspace; the original current writer remains externally retained.
    /// A dropped waiter, panic, error, expiry or cancellation never makes the
    /// cohort reusable, even when the preceding native/SQL effect succeeded.
    pub(in crate::native_service::pair) fn retire_current<'a, 'source: 'a>(
        &'a mut self,
        mut current: CurrentSourceAuthentication<'source>,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let active = self.phase == WorkspacePhase::Active;
        self.phase = WorkspacePhase::Retiring;
        let started = if active {
            current
                .begin_retained_close(&mut self.source_close)
                .map_err(current_authorization_error)
        } else {
            Err(anyhow::anyhow!(
                "authorization workspace cannot retire twice"
            ))
        };
        #[cfg(test)]
        if started.is_ok() {
            if let Some(entered) = &self.retirement_entered {
                entered.store(true, Ordering::Release);
            }
        }
        async move {
            started?;
            #[cfg(test)]
            if let Some(gate) = &self.retirement_gate {
                gate.notified().await;
            }
            self.source_close.join().await?;
            check(deadline, cancel)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            // Consume both actual tokens and drop their authentication facts
            // before acknowledging this exact source claim. Joined alone is
            // insufficient: the backend checks private writer/slot identity and
            // a real worker ACK, then resets both owners together. An unrelated
            // or old joined slot cannot release this source or its workspace.
            current
                .finish_retained_close(&mut self.source_close)
                .map_err(current_authorization_error)?;
            self.phase = WorkspacePhase::Reusable;
            Ok(())
        }
    }

    #[cfg(test)]
    pub(super) fn retains_unjoined_close(&self) -> bool {
        self.source_close.retains_close() && !self.source_close.is_joined()
    }
}

/// The same admitted cohort moves with the whole successful backend owner.
/// No naked backend Ready can manufacture this private connected handoff.
pub(in crate::native_service::pair) struct ConnectedIntent96 {
    ready: ReadyNativeIntent96,
    workspace: AuthorizationWorkspace,
}
impl ConnectedIntent96 {
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_evidence(
        &self,
    ) -> (bool, motion_catalog_backend::NativeIntentCommitState) {
        (self.workspace.reusable(), self.ready.commit_state())
    }
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut FreshWriter,
    ) -> anyhow::Result<()> {
        self.ready.settle_motion()?;
        self.workspace.settle_service_close(writer).await
    }
    pub(super) fn admit_apply(
        &mut self,
        backup_parent: Option<&Path>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.workspace.reusable(),
            "previous auth cohort is still live"
        );
        self.ready.admit_native_apply_reserved(
            backup_parent,
            super::retained_native_apply::RetainedNativeApply::connected_storage_reservation(),
            deadline,
            cancel,
        )
    }
    pub(super) fn begin_apply(
        self,
    ) -> Result<
        (
            motion_catalog_backend::NativeIntentApplyOperation,
            AuthorizationWorkspace,
        ),
        Self,
    > {
        if !self.workspace.reusable() {
            return Err(self);
        }
        match self.ready.begin_native_apply_reserved(
            super::retained_native_apply::RetainedNativeApply::connected_storage_reservation(),
        ) {
            Ok(operation) => Ok((operation, self.workspace)),
            Err(ready) => Err(Self {
                ready,
                workspace: self.workspace,
            }),
        }
    }
}

#[derive(Debug)]
struct TargetAuthorizationError(crate::v2::Problem);
impl std::fmt::Display for TargetAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "retained native intent target authorization: {:?}",
            self.0
        )
    }
}
impl std::error::Error for TargetAuthorizationError {}
fn target_error(error: crate::v2::Problem) -> anyhow::Error {
    TargetAuthorizationError(error).into()
}
fn check(deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "retained native intent authorization cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "retained native intent inherited deadline expired"
    );
    Ok(())
}

impl RetainedNativeIntent {
    /// Terminal target-reader cleanup only. Revoke this app continuation before
    /// returning even an unpolled waiter, then borrow the exact backend slots.
    /// This does not acknowledge effects, create Ready, or settle/reuse the
    /// independent current-source workspace. Failure retains those same slots.
    pub(in crate::native_service::pair) fn settle_authorization_reader(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.phase = Phase::Interrupted;
        self.operation.settle_authorization_reader()
    }

    /// Additional host retention, not a fresh budget or a replacement for H.
    /// Fixed wrapper/result/authentication layouts are explicit. The existing
    /// conservative auth cohort covers bounded readers, decoded snapshots and
    /// async-frame metadata; it may double-count a producer's prior auth reserve
    /// and therefore refuse otherwise admissible work. Aggregate limits remain.
    const fn fixed_storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<Result<Self, RecordedAdmittedControl>>()
            + std::mem::size_of::<ReadyNativeIntent96>()
            + std::mem::size_of::<Result<ReadyNativeIntent96, Self>>()
            + 2 * std::mem::size_of::<TargetAuthentication<'static>>()
            + 2 * std::mem::size_of::<Result<TargetAuthentication<'static>, crate::v2::Problem>>()
            + std::mem::size_of::<Result<(), crate::v2::Problem>>()
            + std::mem::size_of::<CurrentSourceAuthentication<'static>>()
            + std::mem::size_of::<RetainedAuthorizationReader<'static>>()
            + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>(
            )
            + std::mem::size_of::<Result<(), anyhow::Error>>()) as u64
    }
    pub(super) const fn storage_reservation() -> u64 {
        Self::fixed_storage_reservation()
            + AUTHORIZATION_RESERVE
            + FreshWriter::authorization_storage_reservation()
    }

    /// Pre-admit every new retained host slot before moving the complete97
    /// owner. A refusal leaves that owner in the caller's original slot.
    pub(super) fn admit(
        ready: &mut RecordedAdmittedControl,
        native: &Path,
        quiescence: catabolic_store::migration::NativeUpgradeQuiescence,
        transition: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ready.admit_native_preparation_reserved(
            native,
            quiescence,
            transition,
            Self::storage_reservation(),
            deadline,
            cancel,
        )
    }

    /// Moves only, after the backend verifies the retained host reservation.
    /// No SQL, allocation, new deadline, credential copy or native acquisition.
    pub(super) fn from_admitted(
        ready: RecordedAdmittedControl,
    ) -> Result<Self, RecordedAdmittedControl> {
        ready
            .begin_native_preparation_reserved(Self::storage_reservation())
            .map(|operation| Self {
                operation,
                phase: Phase::Inputs,
                workspace: None,
                #[cfg(test)]
                observation: Observation::default(),
            })
    }

    /// One auth cohort is charged here and retained in H through every connected
    /// successor. Separate source-close storage is mandatory, not a fresh limit.
    pub(super) const fn connected_storage_reservation() -> u64 {
        Self::storage_reservation()
            + RetainedAuthorizationCloseSlot::storage_reservation()
            + (std::mem::size_of::<ConnectedIntent96>()
                + std::mem::size_of::<Result<ConnectedIntent96, Self>>()) as u64
    }

    pub(super) fn admit_connected(
        ready: &mut RecordedAdmittedControl,
        native: &Path,
        quiescence: catabolic_store::migration::NativeUpgradeQuiescence,
        transition: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ready.admit_native_preparation_reserved(
            native,
            quiescence,
            transition,
            Self::connected_storage_reservation(),
            deadline,
            cancel,
        )
    }

    pub(super) fn from_admitted_connected(
        ready: RecordedAdmittedControl,
    ) -> Result<Self, RecordedAdmittedControl> {
        ready
            .begin_native_preparation_reserved(Self::connected_storage_reservation())
            .map(|operation| Self {
                operation,
                phase: Phase::Inputs,
                // Standalone connected79 origin: this exact retained97
                // owner just accepted the fixed connected charge. The new
                // connected97 path instead moves its original workspace.
                workspace: Some(AuthorizationWorkspace {
                    service: None,
                    source_close: RetainedAuthorizationCloseSlot::default(),
                    phase: WorkspacePhase::Reusable,
                    #[cfg(test)]
                    retirement_entered: None,
                    #[cfg(test)]
                    retirement_gate: None,
                }),
                #[cfg(test)]
                observation: Observation::default(),
            })
    }

    /// Only new simultaneous layouts are additive. The opaque ConnectedRecorded97
    /// carries the one64MiB cohort and its joined current-close share from97;
    /// neither is inferred from the backend's external-retention scalar.
    pub(in crate::native_service::pair) const fn connected_recorded_storage_reservation() -> u64 {
        Self::fixed_storage_reservation()
            + (std::mem::size_of::<ConnectedIntent96>()
                + std::mem::size_of::<Result<ConnectedIntent96, Self>>()
                + std::mem::size_of::<Result<Self, ConnectedRecorded97>>()) as u64
    }

    /// Borrowed admission leaves the whole opaque97+workspace owner retained.
    /// Native admission can tighten, but cannot restart, the original97 deadline.
    pub(in crate::native_service::pair) fn admit_connected_recorded(
        ready: &mut ConnectedRecorded97,
        native: &Path,
        quiescence: catabolic_store::migration::NativeUpgradeQuiescence,
        transition: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            ready.workspace.reusable(),
            "previous auth cohort is still live"
        );
        ready.ready.admit_native_preparation_reserved(
            native,
            quiescence,
            transition,
            Self::connected_recorded_storage_reservation(),
            deadline,
            cancel,
        )
    }

    /// Moves only. Missing/failed admission returns this same opaque owner, not
    /// a naked Ready97 or an independently usable workspace that can be re-paired.
    pub(in crate::native_service::pair) fn from_connected_recorded(
        ready: ConnectedRecorded97,
    ) -> Result<Self, ConnectedRecorded97> {
        if !ready.workspace.reusable() {
            return Err(ready);
        }
        match ready
            .ready
            .begin_native_preparation_reserved(Self::connected_recorded_storage_reservation())
        {
            Ok(operation) => Ok(Self {
                operation,
                phase: Phase::Inputs,
                workspace: Some(ready.workspace),
                #[cfg(test)]
                observation: Observation::default(),
            }),
            Err(owner) => Err(ConnectedRecorded97 {
                ready: owner,
                workspace: ready.workspace,
            }),
        }
    }

    /// This is intentionally not `async fn`: terminal state is installed before
    /// the future is returned, even when that future is never polled. The real
    /// source reader enters the future, but only borrows the original FreshWriter;
    /// dropping it cannot drop that writer. All97/native owners remain inline.
    pub(super) fn authorize_and_commit<'a, 'source: 'a>(
        &'a mut self,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let first_attempt = self.phase == Phase::Inputs;
        // Even an invalid second attempt may receive a newly opened source
        // reader. Poison both handoff and cohort before its future can drop.
        self.phase = Phase::Interrupted;
        let workspace_started = self
            .workspace
            .as_mut()
            .map(AuthorizationWorkspace::begin)
            .transpose();
        async move {
            ensure!(first_attempt, "retained native intent cannot retry");
            workspace_started?;
            let deadline = self.operation.deadline();
            check(deadline, cancel)?;
            // Do not accept an independently supplied current token: it could
            // represent a different credential or permission. This fixed call
            // binds the actual source reader to this same caller/SystemAdmin.
            let mut current = CurrentSourceAuthentication::prepare(
                source_reader,
                caller,
                Permission::SystemAdmin,
            )
            .await
            .map_err(current_authorization_error)?;
            check(deadline, cancel)?;
            let reader = self.operation.authorization_reader().await?;
            check(deadline, cancel)?;
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await
                .map_err(target_error)?;
            target.revalidate().await.map_err(target_error)?;
            current
                .revalidate()
                .await
                .map_err(current_authorization_error)?;
            // The actual close future lives in the backend operation, never
            // solely in this await-local authentication token or waiter. A
            // successful join is the final await before the full preflight.
            target
                .close_retained_native_intent_reader()
                .await
                .map_err(target_error)?;
            target
                .preflight_retained_native_intent(cancel)
                .map_err(target_error)?;
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            // Native acquisition can recover a hot journal. Keep both real
            // tokens live; no full scans or await intervene after these checks.
            target
                .prepare_retained_native_intent(cancel)
                .map_err(target_error)?;
            drop(target);

            #[cfg(test)]
            self.observation.after_preparation().await;

            check(deadline, cancel)?;
            let reader = self.operation.authorization_reader().await?;
            check(deadline, cancel)?;
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.before_final_authorization().await;
            target.revalidate().await.map_err(target_error)?;
            current
                .revalidate()
                .await
                .map_err(current_authorization_error)?;
            target
                .close_retained_native_intent_reader()
                .await
                .map_err(target_error)?;
            target
                .preflight_retained_native_intent(cancel)
                .map_err(target_error)?;
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            // INSERT and its full resample stop at PendingCommit. Keep the
            // original target snapshot plus exact owner borrow throughout.
            target
                .insert_retained_native_intent(cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.after_insertion(cancel);
            // Resampling can take time: the earlier temporal checks are not
            // reused as a COMMIT grant. No full scan precedes COMMIT/re-BEGIN;
            // the backend verifies the resulting writer after those effects.
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            target
                .commit_retained_native_intent(cancel)
                .map_err(target_error)?;
            drop(target);
            if let Some(workspace) = self.workspace.as_mut() {
                workspace.retire_current(current, deadline, cancel).await?;
            } else {
                current
                    .retire_inline()
                    .await
                    .map_err(current_authorization_error)?;
                check(deadline, cancel)?;
            }
            self.phase = Phase::ReadyIntent96;
            Ok(())
        }
    }

    /// Fixed actual-service corridor. Claim before constructing the source-reader
    /// future; both reader acquisitions remain on their original retained owners.
    pub(in crate::native_service::pair) fn authorize_service_locked<'a>(
        &'a mut self,
        caller: &'a Caller,
        writer: &'a mut FreshWriter,
        native: &'a mut crate::native_service::publication_file::Outcome,
        expected: &'a playscale_core::paired_backup::publication::Evidence,
        native_fd: &'a mut Option<std::fs::File>,
        #[cfg(test)] shared: &'a super::super::source_image::supervisor::Shared,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let first_attempt = self.phase == Phase::Inputs;
        self.phase = Phase::Interrupted;
        let workspace_started = (|| {
            ensure!(first_attempt, "retained native intent cannot retry");
            ensure!(
                self.workspace.as_ref().is_some_and(|w| w.service.is_some()),
                "service native intent needs actual service workspace"
            );
            self.workspace.as_mut().unwrap().begin()
        })();
        // The real claim stays on FreshWriter even if this outer waiter is
        // never polled; rejected phase/provenance gates acquire nothing.
        let source = workspace_started.map(|()| writer.authorization_reader());
        async move {
            let source_reader = source?.await?;
            let deadline = self.operation.deadline();
            check(deadline, cancel)?;
            // Do not accept an independently supplied current token: it could
            // represent a different credential or permission. This fixed call
            // binds the actual source reader to this same caller/SystemAdmin.
            let mut current = CurrentSourceAuthentication::prepare(
                source_reader,
                caller,
                Permission::SystemAdmin,
            )
            .await
            .map_err(current_authorization_error)?;
            check(deadline, cancel)?;
            let reader = self.operation.authorization_reader().await?;
            check(deadline, cancel)?;
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await
                .map_err(target_error)?;
            target.revalidate().await.map_err(target_error)?;
            current
                .revalidate()
                .await
                .map_err(current_authorization_error)?;
            // The actual close future lives in the backend operation, never
            // solely in this await-local authentication token or waiter. A
            // successful join is the final await before the full preflight.
            target
                .close_retained_native_intent_reader()
                .await
                .map_err(target_error)?;
            target
                .preflight_retained_native_intent(cancel)
                .map_err(target_error)?;
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            // This fixed step acquires only native host locks, not SQLite.
            // Keep both real tokens live through the lock continuation.
            #[cfg(test)]
            super::super::source_image::supervisor::native96_boundary(shared, 1);
            target
                .lock_retained_native_source(cancel)
                .map_err(target_error)?;
            drop(target);
            // Real native leases now exclude supported Store/CLI writers. Verify
            // original published bytes under those leases, retain FD immediately,
            // and permanently revoke immutable publication before SQLite setup.
            native.pin_service_native(expected, native_fd)?;
            #[cfg(test)]
            super::super::source_image::supervisor::native96_boundary(shared, 2);
            check(deadline, cancel)?;
            let reader = self.operation.authorization_reader().await?;
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await
                .map_err(target_error)?;
            target.revalidate().await.map_err(target_error)?;
            current
                .revalidate()
                .await
                .map_err(current_authorization_error)?;
            target
                .close_retained_native_intent_reader()
                .await
                .map_err(target_error)?;
            target
                .preflight_retained_native_intent(cancel)
                .map_err(target_error)?;
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            target
                .prepare_locked_native_intent(
                    native_fd
                        .as_ref()
                        .expect("retained verified native descriptor"),
                    cancel,
                )
                .map_err(target_error)?;
            drop(target);

            #[cfg(test)]
            self.observation.after_preparation().await;
            #[cfg(test)]
            super::super::source_image::supervisor::native96_boundary(shared, 3);

            check(deadline, cancel)?;
            let reader = self.operation.authorization_reader().await?;
            check(deadline, cancel)?;
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.before_final_authorization().await;
            target.revalidate().await.map_err(target_error)?;
            current
                .revalidate()
                .await
                .map_err(current_authorization_error)?;
            target
                .close_retained_native_intent_reader()
                .await
                .map_err(target_error)?;
            target
                .preflight_retained_native_intent(cancel)
                .map_err(target_error)?;
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            // INSERT and its full resample stop at PendingCommit. Keep the
            // original target snapshot plus exact owner borrow throughout.
            target
                .insert_retained_native_intent(cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.after_insertion(cancel);
            #[cfg(test)]
            super::super::source_image::supervisor::native96_boundary(shared, 4);
            // Resampling can take time: the earlier temporal checks are not
            // reused as a COMMIT grant. No full scan precedes COMMIT/re-BEGIN;
            // the backend verifies the resulting writer after those effects.
            check(deadline, cancel)?;
            target.revalidate_temporal().map_err(target_error)?;
            current
                .revalidate_temporal()
                .map_err(current_authorization_error)?;
            target
                .commit_retained_native_intent(cancel)
                .map_err(target_error)?;
            drop(target);
            #[cfg(test)]
            super::super::source_image::supervisor::native96_boundary(shared, 5);
            if let Some(workspace) = self.workspace.as_mut() {
                workspace.retire_current(current, deadline, cancel).await?;
            } else {
                current
                    .retire_inline()
                    .await
                    .map_err(current_authorization_error)?;
                check(deadline, cancel)?;
            }
            self.phase = Phase::ReadyIntent96;
            Ok(())
        }
    }

    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_evidence(
        &self,
    ) -> (
        motion_catalog_backend::NativeIntentPreparationPhase,
        motion_catalog_backend::NativeIntentCommitState,
        bool,
    ) {
        (
            self.operation.phase(),
            self.operation.commit_state(),
            self.workspace
                .as_ref()
                .is_some_and(AuthorizationWorkspace::reusable),
        )
    }
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut FreshWriter,
    ) -> anyhow::Result<()> {
        self.phase = Phase::Interrupted;
        self.operation.settle_authorization_reader().await?;
        self.operation.settle_motion()?;
        if let Some(workspace) = self.workspace.as_mut() {
            workspace.settle_service_close(writer).await?;
        }
        Ok(())
    }
    /// No Ready value can be manufactured from an in-flight or interrupted
    /// attempt. Both branches retain the exact backend operation, moves only.
    pub(super) fn into_ready(self) -> Result<ReadyNativeIntent96, Self> {
        if self.phase != Phase::ReadyIntent96 || self.workspace.is_some() {
            return Err(self);
        }
        match self.operation.into_ready() {
            Ok(ready) => Ok(ready),
            Err(operation) => Err(Self {
                operation,
                phase: Phase::Interrupted,
                workspace: self.workspace,
                #[cfg(test)]
                observation: self.observation,
            }),
        }
    }

    pub(in crate::native_service::pair) fn into_connected_ready(
        self,
    ) -> Result<ConnectedIntent96, Self> {
        if self.phase != Phase::ReadyIntent96
            || !self
                .workspace
                .as_ref()
                .is_some_and(AuthorizationWorkspace::reusable)
        {
            return Err(self);
        }
        match self.operation.into_ready() {
            Ok(ready) => Ok(ConnectedIntent96 {
                ready,
                workspace: self.workspace.expect("checked connected workspace"),
            }),
            Err(operation) => Err(Self {
                operation,
                phase: Phase::Interrupted,
                workspace: self.workspace,
                #[cfg(test)]
                observation: self.observation,
            }),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct Observation {
    after_prepare: Option<std::sync::Arc<tokio::sync::Notify>>,
    final_expiry: Option<i64>,
    panic_after_prepare: bool,
    commit_expiry: Option<i64>,
    cancel_after_insert: bool,
    panic_after_insert: bool,
}
#[cfg(test)]
impl Observation {
    async fn after_preparation(&self) {
        assert!(
            !self.panic_after_prepare,
            "actual retained native preparation interruption"
        );
        if let Some(gate) = &self.after_prepare {
            gate.notified().await;
        }
    }
    fn after_insertion(&self, cancel: &AtomicBool) {
        assert!(
            !self.panic_after_insert,
            "actual retained PendingCommit interruption"
        );
        if self.cancel_after_insert {
            cancel.store(true, Ordering::Release);
        }
        if let Some(expiry) = self.commit_expiry {
            let now = crate::now();
            assert!(now < expiry, "actual credential expired before INSERT");
            // Deliberately synchronous, test-only real wall-clock expiry. No
            // synthetic clock, copied auth token or production await is added.
            std::thread::sleep(std::time::Duration::from_secs(
                u64::try_from(expiry - now).unwrap(),
            ));
        }
    }
    async fn before_final_authorization(&self) {
        if let Some(expiry) = self.final_expiry {
            let now = crate::now();
            assert!(
                now < expiry,
                "actual credential expired before final admission"
            );
            tokio::time::sleep(std::time::Duration::from_secs(
                u64::try_from(expiry - now).unwrap(),
            ))
            .await;
        }
    }
}

#[cfg(test)]
use crate as playscale_app;
#[cfg(test)]
#[path = "../../../../../../../tests/support/native_application_fixture.rs"]
mod application_fixture;

#[cfg(test)]
pub(super) use tests::{
    ConnectedApplyFixture, RetainedApplyFixture, RetainedApplyFixtureCase,
    RetainedBootstrapFixture, connected_apply_deadline, fixture_for_connected_apply,
    fixture_for_retained_apply, fixture_for_retained_bootstrap,
};

#[cfg(test)]
mod tests {
    use super::*;
    use application_fixture::{Fixture as ApplicationFixture, OPERATOR};
    use motion_catalog_backend::{
        NativeIntentCommitState, NativeIntentPreparationPhase, OfflineControlShutdownFacts,
        RecordedControlOperation, motion_snapshot::FreshWriter,
    };
    use std::{
        fs::File, io::Cursor, os::fd::AsRawFd, path::PathBuf, sync::Arc, task::Poll, time::Duration,
    };

    struct Fixture {
        app: ApplicationFixture,
        _stage: tempfile::TempDir,
        _original: File,
        native: PathBuf,
        motion: PathBuf,
        source: FreshWriter,
        caller: Caller,
        expiry: Option<i64>,
        ready: RecordedAdmittedControl,
    }
    // Fixed test-only bridge into the next retained phase. It returns genuine
    // producer output plus the original fixture custody, not an auth factory.
    pub(crate) enum RetainedApplyFixtureCase {
        Valid,
        Expiring,
        RevokedTarget,
        RevokedCurrent,
    }
    pub(crate) struct RetainedApplyFixture {
        pub(crate) app: ApplicationFixture,
        pub(crate) _stage: tempfile::TempDir,
        pub(crate) _original: File,
        pub(crate) native: PathBuf,
        pub(crate) motion: PathBuf,
        pub(crate) source: FreshWriter,
        pub(crate) caller: Caller,
        pub(crate) expiry: Option<i64>,
        pub(crate) ready: ReadyNativeIntent96,
    }
    pub(crate) async fn fixture_for_retained_apply(
        case: RetainedApplyFixtureCase,
    ) -> RetainedApplyFixture {
        let (expiring, revocation) = match case {
            RetainedApplyFixtureCase::Valid => (false, Revocation::None),
            RetainedApplyFixtureCase::Expiring => (true, Revocation::None),
            RetainedApplyFixtureCase::RevokedTarget => (false, Revocation::Target),
            RetainedApplyFixtureCase::RevokedCurrent => (false, Revocation::Current),
        };
        let (
            Fixture {
                app,
                _stage,
                _original,
                native,
                motion,
                mut source,
                caller,
                expiry,
                ready,
            },
            preparer,
        ) = fixture_with_preparer(expiring, revocation).await;
        let mut producer = operation(&mut Some(ready), &native);
        let deadline = producer.operation.deadline();
        // Revocation fixtures retain the genuinely paired reserve admin for
        // this earlier phase. Apply must freshly authenticate the requested
        // (possibly revoked) caller, never inherit this producer's grant.
        producer
            .authorize_and_commit(
                &preparer,
                source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        let ready = match producer.into_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("real app intent producer did not finish"),
        };
        assert_eq!(ready.deadline(), deadline);
        RetainedApplyFixture {
            app,
            _stage,
            _original,
            native,
            motion,
            source,
            caller,
            expiry,
            ready,
        }
    }
    pub(crate) struct ConnectedApplyFixture {
        pub(crate) app: ApplicationFixture,
        pub(crate) _stage: tempfile::TempDir,
        pub(crate) _original: File,
        pub(crate) native: PathBuf,
        pub(crate) motion: PathBuf,
        pub(crate) source: FreshWriter,
        pub(crate) caller: Caller,
        pub(crate) expiry: Option<i64>,
        pub(crate) ready: ConnectedIntent96,
    }
    // Fixed genuine connected79 producer. The reserve administrator prepares the
    // intent; the requested caller remains separate for fresh phase80 authority.
    pub(crate) async fn fixture_for_connected_apply(
        case: RetainedApplyFixtureCase,
    ) -> ConnectedApplyFixture {
        let (expiring, revocation) = match case {
            RetainedApplyFixtureCase::Valid => (false, Revocation::None),
            RetainedApplyFixtureCase::Expiring => (true, Revocation::None),
            RetainedApplyFixtureCase::RevokedTarget => (false, Revocation::Target),
            RetainedApplyFixtureCase::RevokedCurrent => (false, Revocation::Current),
        };
        let (
            Fixture {
                app,
                _stage,
                _original,
                native,
                motion,
                mut source,
                caller,
                expiry,
                mut ready,
            },
            preparer,
        ) = fixture_with_preparer(expiring, revocation).await;
        let cancel = AtomicBool::new(false);
        RetainedNativeIntent::admit_connected(
            &mut ready,
            &native,
            quiescence(),
            "app-native96",
            Instant::now() + Duration::from_secs(180),
            &cancel,
        )
        .unwrap();
        let mut producer = match RetainedNativeIntent::from_admitted_connected(ready) {
            Ok(producer) => producer,
            Err(_) => panic!("actual connected79 admission refused"),
        };
        let deadline = producer.operation.deadline();
        // Revocation fixtures retain the genuinely paired reserve admin for
        // this earlier phase. Apply must freshly authenticate the requested
        // (possibly revoked) caller, never inherit this producer's grant.
        producer
            .authorize_and_commit(
                &preparer,
                source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        // This succeeds only after the actual source close has acknowledged,
        // the original current token has dropped, and the workspace is reusable.
        let ready = match producer.into_connected_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("real connected app intent producer did not finish"),
        };
        assert!(ready.workspace.reusable());
        assert_eq!(ready.ready.deadline(), deadline);
        ConnectedApplyFixture {
            app,
            _stage,
            _original,
            native,
            motion,
            source,
            caller,
            expiry,
            ready,
        }
    }
    pub(crate) fn connected_apply_deadline(ready: &ConnectedIntent96) -> Instant {
        assert!(
            ready.workspace.reusable(),
            "previous auth cohort is still live"
        );
        ready.ready.deadline()
    }

    pub(crate) struct RetainedBootstrapFixture {
        pub(crate) app: ApplicationFixture,
        pub(crate) _stage: tempfile::TempDir,
        pub(crate) _original: File,
        pub(crate) native: PathBuf,
        pub(crate) motion: PathBuf,
        pub(crate) source: FreshWriter,
        pub(crate) caller: Caller,
        pub(crate) preparer: Caller,
        pub(crate) expiry: Option<i64>,
        pub(crate) deadline: Instant,
        pub(crate) ready: Option<super::super::retained_native_apply::ConnectedPending96>,
    }

    /// Fixed genuine connected79→80 fixture, not a workspace or policy factory.
    pub(crate) async fn fixture_for_retained_bootstrap(
        case: RetainedApplyFixtureCase,
    ) -> RetainedBootstrapFixture {
        use super::super::retained_native_apply::RetainedNativeApply;
        let (expiring, revocation) = match case {
            RetainedApplyFixtureCase::Valid => (false, Revocation::None),
            RetainedApplyFixtureCase::Expiring => (true, Revocation::None),
            RetainedApplyFixtureCase::RevokedTarget => (false, Revocation::Target),
            RetainedApplyFixtureCase::RevokedCurrent => (false, Revocation::Current),
        };
        let (
            Fixture {
                app,
                _stage,
                _original,
                native,
                motion,
                mut source,
                caller,
                expiry,
                mut ready,
            },
            preparer,
        ) = fixture_with_preparer(expiring, revocation).await;
        let cancel = AtomicBool::new(false);
        RetainedNativeIntent::admit_connected(
            &mut ready,
            &native,
            quiescence(),
            "app-native96",
            Instant::now() + Duration::from_secs(180),
            &cancel,
        )
        .unwrap();
        let mut producer = match RetainedNativeIntent::from_admitted_connected(ready) {
            Ok(producer) => producer,
            Err(_) => panic!("actual connected79 admission refused"),
        };
        let deadline = producer.operation.deadline();
        producer
            .authorize_and_commit(
                &preparer,
                source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        // A naked handoff must not silently discard the actual workspace.
        let producer = match producer.into_ready() {
            Err(producer) => producer,
            Ok(_) => panic!("connected79 dropped its workspace"),
        };
        let mut ready = match producer.into_connected_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("actual connected79 handoff refused"),
        };
        assert!(ready.workspace.reusable());
        assert_eq!(ready.ready.deadline(), deadline);
        RetainedNativeApply::admit_connected(&mut ready, None, deadline, &cancel).unwrap();
        let mut apply = match RetainedNativeApply::from_connected(ready) {
            Ok(apply) => apply,
            Err(_) => panic!("actual connected80 admission refused"),
        };
        apply
            .authorize_and_apply(
                &preparer,
                source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        let apply = match apply.into_pending() {
            Err(apply) => apply,
            Ok(_) => panic!("connected80 dropped its workspace"),
        };
        let ready = match apply.into_connected_pending() {
            Ok(ready) => ready,
            Err(_) => panic!("actual connected80 handoff refused"),
        };
        assert!(ready.workspace_reusable());
        assert_eq!(ready.deadline(), deadline);
        RetainedBootstrapFixture {
            app,
            _stage,
            _original,
            native,
            motion,
            source,
            caller,
            preparer,
            expiry,
            deadline,
            ready: Some(ready),
        }
    }

    fn quiescence() -> catabolic_store::migration::NativeUpgradeQuiescence {
        catabolic_store::migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Revocation {
        None,
        Target,
        Current,
    }

    async fn revoke(app: &ApplicationFixture, caller: &Caller) {
        let path = format!(
            "/api/v2/devices/{}",
            caller.principal.device_id.as_ref().unwrap()
        );
        let bearer = format!("Bearer {OPERATOR}");
        let descriptor = app
            .call("GET", &path, None, &[("authorization", &bearer)])
            .await;
        assert_eq!(
            descriptor.status,
            axum::http::StatusCode::OK,
            "public device descriptor GET before revocation: {:?}",
            descriptor.body
        );
        let reply = app
            .call(
                "DELETE",
                &path,
                None,
                &[
                    ("authorization", &bearer),
                    ("if-match", descriptor.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(
            reply.status,
            axum::http::StatusCode::NO_CONTENT,
            "{:?}",
            reply.body
        );
    }

    /// Uses public pairing and actual activation/snapshot/recorded97 producers.
    /// This fixture never starts a service; it is not service-drain evidence.
    async fn fixture(expiring: bool, revocation: Revocation) -> Fixture {
        fixture_with_preparer(expiring, revocation).await.0
    }
    async fn fixture_with_preparer(expiring: bool, revocation: Revocation) -> (Fixture, Caller) {
        let mut app = ApplicationFixture::new().await;
        app.cutover().await;
        let (caller, expiry) = if expiring {
            let (caller, expiry) = crate::v2::auth::retained::actual_expiring_admin(&app.app).await;
            (caller, Some(expiry))
        } else {
            (
                crate::v2::auth::retained::actual_paired_admin(&app.app).await,
                None,
            )
        };
        let preparer = if revocation != Revocation::None {
            // The public last-admin guard remains active in the archived copy.
            crate::v2::auth::retained::actual_paired_admin(&app.app).await
        } else {
            caller.clone()
        };
        if expiring {
            // Age the genuinely issued60s credential before creating97. Its
            // wall-clock expiry is then comfortably earlier than the inherited
            // operation ceiling, without changing time, TTLs or auth rows.
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        app.close().await;
        let cancel = AtomicBool::new(false);
        let mut source = FreshWriter::begin(&app.app.db, &cancel).await.unwrap();
        let image = tokio::task::block_in_place(|| source.capture_blocking(&cancel)).unwrap();
        let mut bytes = Vec::new();
        image.copy_verified_to(&mut bytes, &cancel).unwrap();
        drop(image);
        if revocation == Revocation::Current {
            // Preserve the historical target grant, then revoke on the actual
            // original source through its public API before acquiring the final
            // current writer. No copied source token may stand in for it.
            source.close().await.unwrap();
            drop(source);
            // A bound catalog cannot serve public device routes without its
            // real native owner. Re-admit the current pair only for revocation,
            // then shut it down again before retaining the offline writer.
            app.open().await;
            revoke(&app, &caller).await;
            app.close().await;
            source = FreshWriter::begin(&app.app.db, &cancel).await.unwrap();
        }
        let stage = tempfile::tempdir_in(app._dir.path()).unwrap();
        let motion = stage.path().join("target-motion.sqlite3");
        std::fs::write(&motion, &bytes).unwrap();
        let native = stage.path().join("target-native.sqlite3");
        std::fs::copy(app._dir.path().join("native.sqlite3"), &native).unwrap();

        if revocation == Revocation::Target {
            // Change only the historical target through the real API. Current
            // source remains separately WRITE-locked with the genuine grant.
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .after_connect(|db, _| {
                    Box::pin(motion_catalog_backend::projection::configure_connection(db))
                })
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(&motion)
                        .foreign_keys(true),
                )
                .await
                .unwrap();
            let original_pool = std::mem::replace(&mut app.app.db, pool);
            // Admit the copied pair itself, not the original native catalog.
            // Preserve the public descriptor ETag and DELETE authorization.
            app.app.native_catalog = Some(Arc::new(
                crate::native_catalog::Runtime::open(
                    app.app.db.clone(),
                    native.clone(),
                    "default".into(),
                )
                .await
                .unwrap(),
            ));
            revoke(&app, &caller).await;
            app.close().await;
            let mut historical = FreshWriter::begin(&app.app.db, &cancel).await.unwrap();
            let image =
                tokio::task::block_in_place(|| historical.capture_blocking(&cancel)).unwrap();
            bytes.clear();
            image.copy_verified_to(&mut bytes, &cancel).unwrap();
            historical.close().await.unwrap();
            drop(historical);
            let target_pool = std::mem::replace(&mut app.app.db, original_pool);
            target_pool.close().await;
            std::fs::write(&motion, &bytes).unwrap();
        }
        let inspected = motion_catalog_backend::motion_snapshot::image_read::inspect(
            Cursor::new(&bytes),
            bytes.len() as u64,
            0,
            &cancel,
        )
        .unwrap();
        let original_path = stage.path().join("original93.sqlite3");
        std::fs::write(&original_path, &bytes).unwrap();
        drop(bytes);
        let mut original = File::open(&original_path).unwrap();
        let mut operation = RecordedControlOperation::begin(
            File::open(&motion).unwrap(),
            File::open(stage.path()).unwrap(),
        );
        let source_bytes =
            tokio::task::block_in_place(|| source.retained_allocations_blocking()).unwrap();
        operation
            .prepare(
                &motion,
                &mut original,
                &inspected,
                "app-retained96-record",
                &catabolic_core::sha256(b"actual-selected-pair"),
                &OfflineControlShutdownFacts {
                    service_admission_stopped: true,
                    native_workers_joined: true,
                    application_owners_drained: true,
                    exclusive_operational_ownership_retained: true,
                },
                source_bytes,
                Instant::now() + Duration::from_secs(180),
                &cancel,
            )
            .await
            .unwrap();
        operation.apply(&cancel).unwrap();
        let ready = match operation.into_admitted() {
            Ok(ready) => ready,
            Err(_) => panic!("actual recorded97 producer did not finish"),
        };
        (
            Fixture {
                app,
                _stage: stage,
                _original: original,
                native,
                motion,
                source,
                caller,
                expiry,
                ready,
            },
            preparer,
        )
    }

    fn operation(
        ready: &mut Option<RecordedAdmittedControl>,
        native: &Path,
    ) -> RetainedNativeIntent {
        RetainedNativeIntent::admit(
            ready.as_mut().unwrap(),
            native,
            quiescence(),
            "app-native96",
            Instant::now() + Duration::from_secs(180),
            &AtomicBool::new(false),
        )
        .unwrap();
        match RetainedNativeIntent::from_admitted(ready.take().unwrap()) {
            Ok(operation) => operation,
            Err(_) => panic!("pre-admitted app owner move refused"),
        }
    }
    fn native_writer_available(path: &Path) -> bool {
        let mut lock = path.as_os_str().to_owned();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        if !lock.exists() {
            return true;
        }
        let file = File::options().read(true).write(true).open(lock).unwrap();
        // Independent file description, not a copied boolean custody witness.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }
    async fn verify_effects(path: &Path, intents: i64) {
        use sqlx::Connection;
        let mut db = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_native_bootstrap_intents")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            intents
        );
        for table in [
            "catabolic_native_bootstrap_commands",
            "catabolic_native_bootstrap_acks",
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                    .fetch_one(&mut db)
                    .await
                    .unwrap(),
                0
            );
        }
        assert!(
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut db)
                .await
                .is_err(),
            "exact target writer was released"
        );
        db.close().await.unwrap();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_success_requires_both_real_authorities_and_only_commits_intent() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation
            .authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        assert_eq!(operation.phase, Phase::ReadyIntent96);
        assert_eq!(
            operation.operation.commit_state(),
            NativeIntentCommitState::WriterReacquired
        );
        verify_effects(&motion, 1).await;
        assert!(!native_writer_available(&native));
        let ready = match operation.into_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("successful handoff refused"),
        };
        assert_eq!(
            ready.commit_state(),
            NativeIntentCommitState::WriterReacquired
        );
        source.close().await.unwrap();
        drop(source);
        drop(ready);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_unpolled_future_is_terminal_without_native_acquisition() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        let cancel = AtomicBool::new(false);
        drop(operation.authorize_and_commit(
            &caller,
            source.authorization_reader().await.unwrap(),
            &cancel,
        ));
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::Inputs
        );
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation = match operation.into_ready() {
            Err(operation) => operation,
            Ok(_) => panic!("unpolled attempt yielded Ready"),
        };
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_cancelled_attempt_never_acquires_native_or_retries() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        assert!(
            operation
                .authorize_and_commit(
                    &caller,
                    source.authorization_reader().await.unwrap(),
                    &AtomicBool::new(true)
                )
                .await
                .is_err()
        );
        assert_eq!(operation.phase, Phase::Interrupted);
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_dropped_prepared_waiter_retains_whole_native_and_target_owners() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation.observation.after_prepare = Some(Arc::new(tokio::sync::Notify::new()));
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(operation.authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &cancel
            ));
            tokio::time::timeout(
                Duration::from_secs(30),
                std::future::poll_fn(|cx| {
                    assert!(waiter.as_mut().poll(cx).is_pending());
                    if !native_writer_available(&native) {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                }),
            )
            .await
            .unwrap();
        }
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PreparedAwaitingAuthorization
        );
        assert!(!native_writer_available(&native));
        verify_effects(&motion, 0).await;
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        operation.operation.settle_motion().unwrap();
        assert!(
            !native_writer_available(&native),
            "settlement dropped prepared native custody"
        );
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_real_target_revocation_denies_before_native_acquisition() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::Target).await;
        let mut operation = operation(&mut Some(ready), &native);
        let error = operation
            .authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<TargetAuthorizationError>()
            .expect("target policy must refuse, not an unrelated fixture failure");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialRevoked.code()
        );
        assert_eq!(operation.phase, Phase::Interrupted);
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_real_credential_expiry_after_native_prepare_prevents_intent() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            expiry,
        } = fixture(true, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation.observation.final_expiry = expiry;
        let deadline = operation.operation.deadline();
        let remaining = u64::try_from(expiry.unwrap() - crate::now()).unwrap();
        assert!(
            deadline > Instant::now() + Duration::from_secs(remaining + 5),
            "actual credential must expire before the inherited owner deadline"
        );
        let error = operation
            .authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<TargetAuthorizationError>()
            .expect("real credential expiry must refuse, not inherited deadline expiry");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialExpired.code()
        );
        assert!(
            Instant::now() < deadline,
            "expiry case exhausted its original operation deadline"
        );
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PreparedAwaitingAuthorization
        );
        assert!(!native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_real_current_revocation_denies_historical_target_grant() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::Current).await;
        let mut operation = operation(&mut Some(ready), &native);
        // The target grant is real; it cannot rescue the revoked original source.
        let mut target = TargetAuthentication::prepare(
            operation.operation.authorization_reader().await.unwrap(),
            &caller,
            Permission::SystemAdmin,
        )
        .await
        .unwrap();
        target.revalidate().await.unwrap();
        drop(target);
        let error = operation
            .authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<super::super::CurrentConversionAuthorizationError>()
            .expect("actual original source authentication must refuse");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialRevoked.code()
        );
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::Inputs
        );
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_queued_future_consumes_original_tightened_deadline() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            mut ready,
            ..
        } = fixture(false, Revocation::None).await;
        let deadline = Instant::now() + Duration::from_secs(2);
        RetainedNativeIntent::admit(
            &mut ready,
            &native,
            quiescence(),
            "app-native96",
            deadline,
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut operation = match RetainedNativeIntent::from_admitted(ready) {
            Ok(operation) => operation,
            Err(_) => panic!("admitted owner move refused"),
        };
        assert_eq!(operation.operation.deadline(), deadline);
        let cancel = AtomicBool::new(false);
        let waiter = operation.authorize_and_commit(
            &caller,
            source.authorization_reader().await.unwrap(),
            &cancel,
        );
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        assert!(waiter.await.is_err());
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::Inputs
        );
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_unwind_keeps_prepared_native_and_target_in_caller_slot() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation.observation.panic_after_prepare = true;
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(operation.authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &cancel
            ));
            std::future::poll_fn(|cx| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    waiter.as_mut().poll(cx)
                })) {
                    Ok(Poll::Pending) => Poll::Pending,
                    Ok(Poll::Ready(_)) => panic!("actual prepared checkpoint did not unwind"),
                    Err(_) => Poll::Ready(()),
                }
            })
            .await;
        }
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PreparedAwaitingAuthorization
        );
        assert!(!native_writer_available(&native));
        verify_effects(&motion, 0).await;
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        operation.operation.settle_motion().unwrap();
        assert!(!native_writer_available(&native));
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_missing_host_reservation_returns_whole97_without_wrap() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            mut ready,
            ..
        } = fixture(false, Revocation::None).await;
        ready
            .admit_native_preparation(
                &native,
                quiescence(),
                "app-native96",
                Instant::now() + Duration::from_secs(180),
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut ready = match RetainedNativeIntent::from_admitted(ready) {
            Err(ready) => ready,
            Ok(_) => panic!("unreserved app/result/authentication storage was accepted"),
        };
        assert_eq!(
            ready.phase(),
            motion_catalog_backend::RecordedControlPhase::Ready97
        );
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        ready.settle().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(ready);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_cancellation_after_prepare_retains_native_without_intent() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        let gate = Arc::new(tokio::sync::Notify::new());
        operation.observation.after_prepare = Some(gate.clone());
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(operation.authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &cancel
            ));
            tokio::time::timeout(
                Duration::from_secs(30),
                std::future::poll_fn(|cx| {
                    assert!(waiter.as_mut().poll(cx).is_pending());
                    if !native_writer_available(&native) {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                }),
            )
            .await
            .unwrap();
            cancel.store(true, Ordering::Release);
            gate.notify_one();
            assert!(waiter.await.is_err());
        }
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PreparedAwaitingAuthorization
        );
        assert!(!native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_unpolled_reader_close_terminalizes_the_retained_backend() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        let mut target = TargetAuthentication::prepare(
            operation.operation.authorization_reader().await.unwrap(),
            &caller,
            Permission::SystemAdmin,
        )
        .await
        .unwrap();
        target.revalidate().await.unwrap();
        // The real forwarding method must enter the backend close attempt
        // before returning a waiter. Neither waiter nor token owns that close.
        drop(target.close_retained_native_intent_reader());
        assert!(target.revalidate().await.is_err());
        drop(target);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::Failed
        );
        assert!(operation.operation.authorization_reader().await.is_err());
        assert!(
            operation
                .operation
                .prepare(&AtomicBool::new(false))
                .is_err()
        );
        assert!(native_writer_available(&native));
        verify_effects(&motion, 0).await;
        // The backend companion test additionally inspects its actual private
        // pending-close slot after reader/waiter drop. The production app cleanup
        // must reach that same close without extracting the private operation.
        drop(operation.settle_authorization_reader());
        assert_eq!(operation.phase, Phase::Interrupted);
        operation.settle_authorization_reader().await.unwrap();
        operation.settle_authorization_reader().await.unwrap();
        assert!(operation.operation.authorization_reader().await.is_err());
        verify_effects(&motion, 0).await;
        operation = match operation.into_ready() {
            Err(operation) => operation,
            Ok(_) => panic!("unpolled reader close yielded Ready"),
        };
        operation.operation.settle_motion().unwrap();
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_real_expiry_at_pending_commit_prevents_durable_intent() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            expiry,
        } = fixture(true, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation.observation.commit_expiry = expiry;
        let deadline = operation.operation.deadline();
        let remaining = u64::try_from(expiry.unwrap() - crate::now()).unwrap();
        assert!(
            deadline > Instant::now() + Duration::from_secs(remaining + 5),
            "actual credential must expire before the inherited owner deadline"
        );
        let error = operation
            .authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<TargetAuthorizationError>()
            .expect("actual PendingCommit target expiry must deny COMMIT");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialExpired.code()
        );
        assert!(Instant::now() < deadline);
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PendingCommit
        );
        assert_eq!(
            operation.operation.commit_state(),
            NativeIntentCommitState::NotAttempted
        );
        assert!(!native_writer_available(&native));
        verify_effects(&motion, 0).await;
        operation = match operation.into_ready() {
            Err(operation) => operation,
            Ok(_) => panic!("expired PendingCommit yielded Ready"),
        };
        operation.operation.settle_motion().unwrap();
        assert!(!native_writer_available(&native));
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_cancellation_at_pending_commit_preserves_custody() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation.observation.cancel_after_insert = true;
        let cancel = AtomicBool::new(false);
        let error = operation
            .authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            error.to_string(),
            "retained native intent authorization cancelled"
        );
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PendingCommit
        );
        assert_eq!(
            operation.operation.commit_state(),
            NativeIntentCommitState::NotAttempted
        );
        verify_effects(&motion, 0).await;
        assert!(!native_writer_available(&native));
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        operation.operation.settle_motion().unwrap();
        assert!(!native_writer_available(&native));
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained96_app_unwind_at_pending_commit_preserves_custody() {
        let Fixture {
            app,
            _stage,
            _original,
            native,
            motion,
            mut source,
            caller,
            ready,
            ..
        } = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut Some(ready), &native);
        operation.observation.panic_after_insert = true;
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(operation.authorize_and_commit(
                &caller,
                source.authorization_reader().await.unwrap(),
                &cancel,
            ));
            std::future::poll_fn(|cx| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    waiter.as_mut().poll(cx)
                })) {
                    Ok(Poll::Pending) => Poll::Pending,
                    Ok(Poll::Ready(_)) => panic!("actual PendingCommit checkpoint did not unwind"),
                    Err(_) => Poll::Ready(()),
                }
            })
            .await;
        }
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentPreparationPhase::PendingCommit
        );
        assert_eq!(
            operation.operation.commit_state(),
            NativeIntentCommitState::NotAttempted
        );
        verify_effects(&motion, 0).await;
        assert!(!native_writer_available(&native));
        operation = match operation.into_ready() {
            Err(operation) => operation,
            Ok(_) => panic!("unwound PendingCommit yielded Ready"),
        };
        operation.operation.settle_motion().unwrap();
        assert!(!native_writer_available(&native));
        source.close().await.unwrap();
        drop(source);
        drop(operation);
        app.app.db.close().await;
    }
}
