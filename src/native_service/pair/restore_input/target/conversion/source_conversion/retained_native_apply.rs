//! Private, disconnected ReadyIntent96 -> retained native applied/pending bootstrap.
//! The whole backend operation remains inline in the caller's slot through every
//! await, cancellation and unwind. This does not authorize bootstrap commands,
//! ACK, completion, paired95, ordinary Runtime admission or service activation.
//! Fresh current and target tokens are created for this phase; no prior intent
//! token or copied authorization fact is accepted as its authority.
//! Before opening a current reader, a future service driver must check/claim the
//! eligible retained phase. These private entries accept an already-open reader;
//! they do not pre-admit arbitrary repeated reader acquisition for rejected calls.
//! Acquisition/reader custody remains in the actual owners after auth failure.
//! SQLx internal pre-return failure, WorkerCrashed, hard preemption and service
//! owner destruction remain explicit limits, not repaired by a global sink.
use super::retained_native_intent::{AuthorizationWorkspace, ConnectedIntent96};
use super::{AUTHORIZATION_RESERVE, current_authorization_error};
use crate::v2::auth::{
    Caller,
    retained::{CurrentSourceAuthentication, TargetAuthentication},
};
use anyhow::ensure;
use motion_catalog_backend::{
    NativeIntentApplyOperation, ReadyNativeIntent96, ReadyNativePendingBootstrap96,
    motion_snapshot::RetainedAuthorizationReader,
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
    Interrupted,
    AppliedPendingBootstrap,
}
/// Stable external custody, not a task-owned operation or a global recovery sink.
/// No retry, raw owner accessor, Clone, or reusable extraction on failure exists.
pub(in crate::native_service::pair) struct RetainedNativeApply {
    operation: NativeIntentApplyOperation,
    phase: Phase,
    workspace: Option<AuthorizationWorkspace>,
    #[cfg(test)]
    observation: Observation,
}

pub(in crate::native_service::pair) struct ConnectedPending96 {
    ready: ReadyNativePendingBootstrap96,
    workspace: AuthorizationWorkspace,
}
impl ConnectedPending96 {
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        self.ready.settle_motion()?;
        self.workspace.settle_service_close(writer).await
    }

    pub(in crate::native_service::pair) fn deadline(&self) -> Instant {
        self.ready.deadline()
    }
    pub(in crate::native_service::pair) fn workspace_reusable(&self) -> bool {
        self.workspace.reusable()
    }
    pub(super) fn admit_bootstrap(
        &mut self,
        operation_id: &str,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.workspace.reusable(),
            "previous auth cohort is still live"
        );
        self.ready.admit_bootstrap_reserved(
            operation_id,
            policy,
            super::retained_native_bootstrap::RetainedNativeBootstrap::storage_reservation(),
            deadline,
            cancel,
        )
    }
    pub(super) fn begin_bootstrap(
        self,
    ) -> Result<
        (
            motion_catalog_backend::NativeBootstrapOperation,
            AuthorizationWorkspace,
        ),
        Self,
    > {
        if !self.workspace.reusable() {
            return Err(self);
        }
        match self.ready.begin_bootstrap_reserved(
            super::retained_native_bootstrap::RetainedNativeBootstrap::storage_reservation(),
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
            "retained native apply target authorization: {:?}",
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
        "retained native apply authorization cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "retained native apply inherited deadline expired"
    );
    Ok(())
}
impl RetainedNativeApply {
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_panic_after_effect(&mut self, yes: bool) {
        self.observation.panic_after_apply = yes;
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_progress(
        &self,
    ) -> motion_catalog_backend::NativeIntentApplyProgress {
        self.operation.progress()
    }

    /// Terminal target-reader cleanup only. Revoke this app continuation before
    /// returning even an unpolled waiter, then borrow the exact backend slots.
    /// This does not acknowledge effects, create Ready, or settle/reuse the
    /// independent current-source workspace. Failure retains those same slots.
    pub(super) fn settle_authorization_reader(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.phase = Phase::Interrupted;
        self.operation.settle_authorization_reader()
    }

    /// Additional admitted host retention, not a new budget or replacement H.
    /// The existing conservative auth cohort may double-count prior retention
    /// and refuse work; fixed wrapper/result/auth layouts are explicit. Backend
    /// admission separately covers simultaneous prior/new close-future custody.
    const fn fixed_storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<Result<Self, ReadyNativeIntent96>>()
            + std::mem::size_of::<ReadyNativePendingBootstrap96>()
            + std::mem::size_of::<Result<ReadyNativePendingBootstrap96, Self>>()
            + std::mem::size_of::<TargetAuthentication<'static>>()
            + std::mem::size_of::<Result<TargetAuthentication<'static>, crate::v2::Problem>>()
            + std::mem::size_of::<CurrentSourceAuthentication<'static>>()
            + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>(
            )
            + std::mem::size_of::<RetainedAuthorizationReader<'static>>()
            + std::mem::size_of::<Result<(), crate::v2::Problem>>()
            + std::mem::size_of::<Result<(), anyhow::Error>>()) as u64
    }
    pub(super) const fn storage_reservation() -> u64 {
        Self::fixed_storage_reservation()
            + AUTHORIZATION_RESERVE
            + motion_catalog_backend::motion_snapshot::FreshWriter::authorization_storage_reservation()
    }
    pub(super) fn admit(
        ready: &mut ReadyNativeIntent96,
        backup_parent: Option<&Path>,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ready.admit_native_apply_reserved(
            backup_parent,
            Self::storage_reservation(),
            caller_deadline,
            cancel,
        )
    }
    pub(super) fn from_admitted(ready: ReadyNativeIntent96) -> Result<Self, ReadyNativeIntent96> {
        ready
            .begin_native_apply_reserved(Self::storage_reservation())
            .map(|operation| Self {
                operation,
                phase: Phase::Inputs,
                workspace: None,
                #[cfg(test)]
                observation: Observation::default(),
            })
    }

    /// Additional layouts only. The exact private workspace is inherited from
    /// connected79 after its actual readers/tokens/close waiter ended. Its H is
    /// never lowered or re-priced, and retained target close boxes are separate.
    pub(super) const fn connected_storage_reservation() -> u64 {
        Self::fixed_storage_reservation()
            + (std::mem::size_of::<ConnectedIntent96>()
                + std::mem::size_of::<(NativeIntentApplyOperation, AuthorizationWorkspace)>()
                + std::mem::size_of::<
                    Result<(NativeIntentApplyOperation, AuthorizationWorkspace), ConnectedIntent96>,
                >()
                + std::mem::size_of::<Result<Self, ConnectedIntent96>>()
                + std::mem::size_of::<ConnectedPending96>()
                + std::mem::size_of::<Result<ConnectedPending96, Self>>()) as u64
    }

    pub(in crate::native_service::pair) fn admit_connected(
        ready: &mut ConnectedIntent96,
        backup_parent: Option<&Path>,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ready.admit_apply(backup_parent, caller_deadline, cancel)
    }

    pub(in crate::native_service::pair) fn from_connected(
        ready: ConnectedIntent96,
    ) -> Result<Self, ConnectedIntent96> {
        ready.begin_apply().map(|(operation, workspace)| Self {
            operation,
            phase: Phase::Inputs,
            workspace: Some(workspace),
            #[cfg(test)]
            observation: Observation::default(),
        })
    }
    /// Intentionally non-async: even an unpolled future consumes this attempt.
    /// The source reader only borrows the actual externally retained FreshWriter.
    /// The whole Ready96/backend/native operation never enters the waiter.
    pub(super) fn authorize_and_apply<'a, 'source: 'a>(
        &'a mut self,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let eligible = self.phase == Phase::Inputs;
        self.phase = Phase::Interrupted;
        let started = self
            .workspace
            .as_mut()
            .map(AuthorizationWorkspace::begin)
            .transpose();
        async move {
            ensure!(eligible, "retained native apply cannot retry");
            started?;
            self.authorize_claimed(caller, source_reader, cancel).await
        }
    }
    /// Claim both app phase and real workspace before creating the acquisition future.
    pub(in crate::native_service::pair) fn authorize_service<'a>(
        &'a mut self,
        caller: &'a Caller,
        writer: &'a mut motion_catalog_backend::motion_snapshot::FreshWriter,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let eligible = self.phase == Phase::Inputs;
        self.phase = Phase::Interrupted;
        let started = (|| {
            ensure!(eligible, "retained native apply cannot retry");
            ensure!(
                self.workspace
                    .as_ref()
                    .is_some_and(AuthorizationWorkspace::has_service_provenance),
                "actual service workspace required"
            );
            self.workspace.as_mut().unwrap().begin()
        })();
        // Pair the claimed workspace with the actual retained source owner
        // before returning a waiter. No connection/opening frame is polled here.
        let source = started.map(|()| writer.authorization_reader());
        async move {
            let reader = source?.await?;
            self.authorize_claimed(caller, reader, cancel).await
        }
    }
    async fn authorize_claimed(
        &mut self,
        caller: &Caller,
        source_reader: RetainedAuthorizationReader<'_>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let deadline = self.operation.deadline();
        check(deadline, cancel)?;
        let mut current =
            CurrentSourceAuthentication::prepare(source_reader, caller, Permission::SystemAdmin)
                .await
                .map_err(current_authorization_error)?;
        check(deadline, cancel)?;
        let reader = self.operation.authorization_reader().await?;
        check(deadline, cancel)?;
        let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
            .await
            .map_err(target_error)?;
        #[cfg(test)]
        self.observation.after_authorization().await;
        target.revalidate().await.map_err(target_error)?;
        current
            .revalidate()
            .await
            .map_err(current_authorization_error)?;
        // Distinct apply-purpose close. Its actual owning future is already
        // in the backend slot before this borrowed waiter can be dropped.
        target
            .close_retained_native_apply_reader()
            .await
            .map_err(target_error)?;
        // Last await above; fresh full scans precede the actual final fences.
        target
            .preflight_retained_native_apply(cancel)
            .map_err(target_error)?;
        #[cfg(test)]
        self.observation.before_apply(cancel);
        check(deadline, cancel)?;
        target.revalidate_temporal().map_err(target_error)?;
        current
            .revalidate_temporal()
            .map_err(current_authorization_error)?;
        // Authority is consumed here, immediately before the narrow retained
        // apply continuation. Native code has its own cancellation/deadline
        // checkpoints, not per-effect credential checks or hard preemption
        // inside SQL, preservation/copying or backup verification calls.
        target.apply_retained_native(cancel).map_err(target_error)?;
        #[cfg(test)]
        self.observation.after_apply();
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
        self.phase = Phase::AppliedPendingBootstrap;
        Ok(())
    }
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        self.phase = Phase::Interrupted;
        self.operation.settle_authorization_reader().await?;
        self.operation.settle_motion()?;
        if let Some(workspace) = self.workspace.as_mut() {
            workspace.settle_service_close(writer).await?;
        }
        Ok(())
    }
    pub(super) fn into_pending(self) -> Result<ReadyNativePendingBootstrap96, Self> {
        if self.phase != Phase::AppliedPendingBootstrap || self.workspace.is_some() {
            return Err(self);
        }
        match self.operation.into_pending() {
            Ok(pending) => Ok(pending),
            Err(operation) => Err(Self {
                operation,
                phase: Phase::Interrupted,
                workspace: self.workspace,
                #[cfg(test)]
                observation: self.observation,
            }),
        }
    }

    pub(in crate::native_service::pair) fn into_connected_pending(
        self,
    ) -> Result<ConnectedPending96, Self> {
        if self.phase != Phase::AppliedPendingBootstrap
            || !self
                .workspace
                .as_ref()
                .is_some_and(AuthorizationWorkspace::reusable)
        {
            return Err(self);
        }
        match self.operation.into_pending() {
            Ok(ready) => Ok(ConnectedPending96 {
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
    authorization_entered: Option<std::sync::Arc<AtomicBool>>,
    after_authorization: Option<std::sync::Arc<tokio::sync::Notify>>,
    temporal_expiry: Option<i64>,
    cancel_before_apply: bool,
    panic_before_apply: bool,
    panic_after_apply: bool,
}
#[cfg(test)]
impl Observation {
    async fn after_authorization(&self) {
        if let Some(entered) = &self.authorization_entered {
            entered.store(true, Ordering::Release);
        }
        if let Some(gate) = &self.after_authorization {
            gate.notified().await;
        }
    }
    fn before_apply(&self, cancel: &AtomicBool) {
        assert!(
            !self.panic_before_apply,
            "actual retained apply preflight unwind"
        );
        if self.cancel_before_apply {
            cancel.store(true, Ordering::Release);
        }
        if let Some(expiry) = self.temporal_expiry {
            let now = crate::now();
            assert!(
                now < expiry,
                "actual credential expired before apply preflight"
            );
            // Test-only real wall-clock expiry after full preflight. No synthetic
            // clock/token or production await is inserted before native apply.
            std::thread::sleep(std::time::Duration::from_secs(
                u64::try_from(expiry - now).unwrap(),
            ));
        }
    }
    fn after_apply(&self) {
        assert!(
            !self.panic_after_apply,
            "actual retained applied-owner unwind"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::retained_native_intent::{
        ConnectedApplyFixture, RetainedApplyFixtureCase as Case, connected_apply_deadline,
        fixture_for_connected_apply, fixture_for_retained_apply,
    };
    use super::*;
    use motion_catalog_backend::{
        NativeIntentApplyPhase, NativeIntentApplyProgress, motion_snapshot::FreshWriter,
    };
    use std::{fs::File, os::fd::AsRawFd, path::PathBuf, sync::Arc, task::Poll, time::Duration};

    fn operation(ready: ConnectedIntent96) -> RetainedNativeApply {
        let deadline = connected_apply_deadline(&ready);
        let operation = operation_with_deadline(ready, Instant::now() + Duration::from_secs(180));
        assert_eq!(
            operation.operation.deadline(),
            deadline,
            "deadline was renewed"
        );
        operation
    }
    fn operation_with_deadline(
        mut ready: ConnectedIntent96,
        deadline: Instant,
    ) -> RetainedNativeApply {
        RetainedNativeApply::admit_connected(&mut ready, None, deadline, &AtomicBool::new(false))
            .unwrap();
        match RetainedNativeApply::from_connected(ready) {
            Ok(operation) => operation,
            Err(_) => panic!("pre-admitted real Ready96 move refused"),
        }
    }
    fn native_writer_available(path: &Path) -> bool {
        let mut name = path.as_os_str().to_owned();
        name.push(".lock");
        let file = File::options()
            .read(true)
            .write(true)
            .open(PathBuf::from(name))
            .unwrap();
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }
    async fn verify_effects(motion: &Path, native: &Path, native_version: u32) {
        use sqlx::Connection;
        let mut db = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(motion)
                .create_if_missing(false)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        for (table, expected) in [
            ("catabolic_native_bootstrap_intents", 1),
            ("catabolic_native_bootstrap_commands", 0),
            ("catabolic_native_bootstrap_acks", 0),
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                    .fetch_one(&mut db)
                    .await
                    .unwrap(),
                expected
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
        let mut db = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(native)
                .create_if_missing(false)
                .read_only(true)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            i64::from(native_version)
        );
        db.close().await.unwrap();
        assert!(
            !native_writer_available(native),
            "actual native writer lease was released"
        );
    }
    fn not_applied(operation: &RetainedNativeApply, phase: NativeIntentApplyPhase) {
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(operation.operation.phase(), phase);
        assert_eq!(
            operation.operation.progress(),
            NativeIntentApplyProgress::NotAttempted
        );
    }
    async fn cannot_retry(
        operation: &mut RetainedNativeApply,
        _caller: &Caller,
        source: &mut FreshWriter,
    ) {
        assert!(source.retains_authorization_reader());
        assert!(source.authorization_reader().await.is_err());
        assert_eq!(operation.phase, Phase::Interrupted);
        assert!(!operation.workspace.as_ref().unwrap().reusable());
    }
    fn retained_after_failed_handoff(operation: RetainedNativeApply) -> RetainedNativeApply {
        let operation = match operation.into_pending() {
            Err(operation) => operation,
            Ok(_) => panic!("interrupted attempt discarded its workspace"),
        };
        match operation.into_connected_pending() {
            Err(operation) => operation,
            Ok(_) => panic!("interrupted attempt minted pending bootstrap handoff"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_success_requires_fresh_authorities_and_stops_pending_bootstrap() {
        let mut f: ConnectedApplyFixture = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        operation
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        assert_eq!(operation.phase, Phase::AppliedPendingBootstrap);
        assert_eq!(
            operation.operation.phase(),
            NativeIntentApplyPhase::AppliedPendingBootstrap
        );
        assert_eq!(
            operation.operation.progress(),
            NativeIntentApplyProgress::AppliedPendingBootstrap
        );
        verify_effects(&f.motion, &f.native, 41).await;
        let operation = match operation.into_pending() {
            Err(operation) => operation,
            Ok(_) => panic!("successful connected apply discarded its workspace"),
        };
        let pending = match operation.into_connected_pending() {
            Ok(pending) => pending,
            Err(_) => panic!("successful move-only handoff refused"),
        };
        assert!(pending.workspace_reusable());
        verify_effects(&f.motion, &f.native, 41).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(pending);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_rejected_service_gates_never_claim_source_reader() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        assert!(!operation.workspace.as_ref().unwrap().has_service_provenance());
        let cancel = AtomicBool::new(false);
        drop(operation.authorize_service(&f.caller, &mut f.source, &cancel));
        assert!(operation.workspace.as_ref().unwrap().reusable());
        assert!(!f.source.authorization_claimed());
        assert!(!f.source.retains_authorization_reader());
        assert!(!f.source.retains_authorization_acquisition());
        let error = operation.authorize_service(&f.caller, &mut f.source, &cancel)
            .await.unwrap_err();
        assert_eq!(error.to_string(), "retained native apply cannot retry");
        assert!(operation.workspace.as_ref().unwrap().reusable());
        assert!(!f.source.authorization_claimed());
        assert!(!f.source.retains_authorization_reader());
        assert!(!f.source.retains_authorization_acquisition());
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        verify_effects(&f.motion, &f.native, 40).await;
        let mut operation = retained_after_failed_handoff(operation);
        operation.settle_authorization_reader().await.unwrap();
        operation.operation.settle_motion().unwrap();
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_unpolled_future_consumes_attempt_without_native_apply() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        drop(operation.authorize_and_apply(
            &f.caller,
            f.source.authorization_reader().await.unwrap(),
            &AtomicBool::new(false),
        ));
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        cannot_retry(&mut operation, &f.caller, &mut f.source).await;
        verify_effects(&f.motion, &f.native, 40).await;
        let operation = retained_after_failed_handoff(operation);
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_cancelled_attempt_never_applies_or_retries() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        let error = operation
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &AtomicBool::new(true),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "retained native apply authorization cancelled"
        );
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        cannot_retry(&mut operation, &f.caller, &mut f.source).await;
        verify_effects(&f.motion, &f.native, 40).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_dropped_auth_waiter_keeps_whole_ready96_and_native_owners() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        let entered = Arc::new(AtomicBool::new(false));
        operation.observation.authorization_entered = Some(entered.clone());
        operation.observation.after_authorization = Some(Arc::new(tokio::sync::Notify::new()));
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(operation.authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &cancel
            ));
            tokio::time::timeout(
                Duration::from_secs(30),
                std::future::poll_fn(|cx| {
                    assert!(waiter.as_mut().poll(cx).is_pending());
                    if entered.load(Ordering::Acquire) {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                }),
            )
            .await
            .unwrap();
        }
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        cannot_retry(&mut operation, &f.caller, &mut f.source).await;
        verify_effects(&f.motion, &f.native, 40).await;
        let operation = retained_after_failed_handoff(operation);
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_real_target_revocation_cannot_inherit_prior_intent_authority() {
        let mut f = fixture_for_connected_apply(Case::RevokedTarget).await;
        let mut operation = operation(f.ready);
        let error = operation
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<TargetAuthorizationError>()
            .expect("fresh target policy must deny, not an unrelated fixture failure");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialRevoked.code()
        );
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        verify_effects(&f.motion, &f.native, 40).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_real_current_revocation_denies_existing_target_grant() {
        let mut f = fixture_for_connected_apply(Case::RevokedCurrent).await;
        let mut operation = operation(f.ready);
        let mut target = TargetAuthentication::prepare(
            operation.operation.authorization_reader().await.unwrap(),
            &f.caller,
            Permission::SystemAdmin,
        )
        .await
        .unwrap();
        target.revalidate().await.unwrap();
        drop(target);
        let error = operation
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<super::super::CurrentConversionAuthorizationError>()
            .expect("actual current source must deny the requested caller");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialRevoked.code()
        );
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        verify_effects(&f.motion, &f.native, 40).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_real_expiry_after_preflight_prevents_native_apply() {
        let mut f = fixture_for_connected_apply(Case::Expiring).await;
        let mut operation = operation(f.ready);
        let deadline = operation.operation.deadline();
        let remaining = u64::try_from(f.expiry.unwrap() - crate::now()).unwrap();
        assert!(
            deadline > Instant::now() + Duration::from_secs(remaining + 5),
            "credential expiry must precede original inherited deadline"
        );
        operation.observation.temporal_expiry = f.expiry;
        let error = operation
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
        let denied = error
            .downcast_ref::<TargetAuthorizationError>()
            .expect("actual credential expiry, not deadline exhaustion, must deny apply");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialExpired.code()
        );
        assert!(Instant::now() < deadline);
        not_applied(&operation, NativeIntentApplyPhase::Preflighted);
        verify_effects(&f.motion, &f.native, 40).await;
        let operation = retained_after_failed_handoff(operation);
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_cancellation_after_preflight_preserves_unapplied_custody() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        operation.observation.cancel_before_apply = true;
        let cancel = AtomicBool::new(false);
        let error = operation
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            error.to_string(),
            "retained native apply authorization cancelled"
        );
        not_applied(&operation, NativeIntentApplyPhase::Preflighted);
        cannot_retry(&mut operation, &f.caller, &mut f.source).await;
        verify_effects(&f.motion, &f.native, 40).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_queued_future_uses_original_tightened_deadline() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut operation = operation_with_deadline(f.ready, deadline);
        assert_eq!(operation.operation.deadline(), deadline);
        let cancel = AtomicBool::new(false);
        let waiter = operation.authorize_and_apply(
            &f.caller,
            f.source.authorization_reader().await.unwrap(),
            &cancel,
        );
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        let error = waiter.await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "retained native apply inherited deadline expired"
        );
        not_applied(&operation, NativeIntentApplyPhase::Inputs);
        verify_effects(&f.motion, &f.native, 40).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    async fn unwind(after_apply: bool) {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        operation.observation.panic_before_apply = !after_apply;
        operation.observation.panic_after_apply = after_apply;
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(operation.authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &cancel
            ));
            std::future::poll_fn(|cx| {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    waiter.as_mut().poll(cx)
                })) {
                    Ok(Poll::Pending) => Poll::Pending,
                    Ok(Poll::Ready(_)) => panic!("actual apply checkpoint did not unwind"),
                    Err(_) => Poll::Ready(()),
                }
            })
            .await;
        }
        assert_eq!(operation.phase, Phase::Interrupted);
        if after_apply {
            assert_eq!(
                operation.operation.phase(),
                NativeIntentApplyPhase::AppliedPendingBootstrap
            );
            assert_eq!(
                operation.operation.progress(),
                NativeIntentApplyProgress::AppliedPendingBootstrap
            );
        } else {
            not_applied(&operation, NativeIntentApplyPhase::Preflighted);
        }
        // Post-apply failure is not represented as a rolled-back native40.
        verify_effects(&f.motion, &f.native, if after_apply { 41 } else { 40 }).await;
        cannot_retry(&mut operation, &f.caller, &mut f.source).await;
        let operation = retained_after_failed_handoff(operation);
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_unwind_before_apply_keeps_whole_unapplied_owner() {
        unwind(false).await;
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_unwind_after_apply_keeps_durable_native41_without_handoff() {
        unwind(true).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_unpolled_close_terminalizes_backend_without_losing_custody() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let mut operation = operation(f.ready);
        let mut target = TargetAuthentication::prepare(
            operation.operation.authorization_reader().await.unwrap(),
            &f.caller,
            Permission::SystemAdmin,
        )
        .await
        .unwrap();
        target.revalidate().await.unwrap();
        drop(target.close_retained_native_apply_reader());
        assert!(target.revalidate().await.is_err());
        drop(target);
        assert_eq!(operation.operation.phase(), NativeIntentApplyPhase::Failed);
        assert!(operation.operation.authorization_reader().await.is_err());
        assert_eq!(
            operation.operation.progress(),
            NativeIntentApplyProgress::NotAttempted
        );
        // Actual pending-close object retention is separately inspected by the
        // backend's private-slot test. Production app cleanup reaches that same
        // pending close without exposing or consuming the backend operation.
        drop(operation.settle_authorization_reader());
        assert_eq!(operation.phase, Phase::Interrupted);
        operation.settle_authorization_reader().await.unwrap();
        operation.settle_authorization_reader().await.unwrap();
        assert!(operation.operation.authorization_reader().await.is_err());
        assert_eq!(
            operation.operation.progress(),
            NativeIntentApplyProgress::NotAttempted
        );
        verify_effects(&f.motion, &f.native, 40).await;
        let operation = retained_after_failed_handoff(operation);
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_missing_admission_returns_genuine_ready96_unchanged() {
        let mut f = fixture_for_connected_apply(Case::Valid).await;
        let deadline = connected_apply_deadline(&f.ready);
        let ready = match RetainedNativeApply::from_connected(f.ready) {
            Err(ready) => ready,
            Ok(_) => panic!("unadmitted Ready96 entered app apply slot"),
        };
        assert_eq!(connected_apply_deadline(&ready), deadline);
        verify_effects(&f.motion, &f.native, 40).await;
        let operation = operation(ready);
        assert_eq!(operation.operation.deadline(), deadline);
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_apply_app_standalone_admission_keeps_conservative_auth_reservation() {
        // A bare Ready96 carries no proof that the prior app reader cohort has
        // actually retired. Keep its conservative second cohort; only the
        // genuine connected workspace may reuse the already admitted one.
        let mut f = fixture_for_retained_apply(Case::Valid).await;
        let deadline = f.ready.deadline();
        let error =
            RetainedNativeApply::admit(&mut f.ready, None, deadline, &AtomicBool::new(false))
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "offline aggregate allocation budget exhausted"
        );
        assert_eq!(f.ready.deadline(), deadline);
        let ready = match RetainedNativeApply::from_admitted(f.ready) {
            Err(ready) => ready,
            Ok(_) => panic!("refused standalone admission entered app apply slot"),
        };
        assert_eq!(ready.deadline(), deadline);
        verify_effects(&f.motion, &f.native, 40).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(ready);
        f.app.app.db.close().await;
    }
}
