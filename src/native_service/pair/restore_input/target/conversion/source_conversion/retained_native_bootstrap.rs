//! Private connected96 bootstrap continuation. The exact79→80 owner and admitted
//! auth workspace stay inline through every await. This is not service adoption,
//! paired95, installation, restart admission, or permission for subsequent94.
//! Each command/execute/ACK purpose creates fresh real current and target tokens.
//! Before opening a current reader, a future service driver must check/claim the
//! eligible retained phase. These private entries accept an already-open reader;
//! they do not pre-admit arbitrary repeated reader acquisition for rejected calls.
//! Initial async readonly-reader/auth failures retain the original writers, but
//! are not claimed to be fully failure-owned or hard-preemptible SQLx operations.
use super::{
    current_authorization_error, retained_native_apply::ConnectedPending96,
    retained_native_intent::AuthorizationWorkspace,
};
use crate::v2::auth::{
    Caller,
    retained::{CurrentSourceAuthentication, TargetAuthentication},
};
use anyhow::ensure;
use catabolic_store::catalog_commands::ReceiptPolicy;
use motion_catalog_backend::{
    NativeBootstrapOperation, ReadyNativeSource96, SourceRelocationSession,
    motion_snapshot::RetainedAuthorizationReader,
};
use playscale_core::access::Permission;
use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Inputs,
    Interrupted,
    CommandSaved,
    MeasuredComplete,
    ReadySource,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Command,
    Execute,
    Acknowledge,
}
impl Effect {
    fn before(self) -> Phase {
        match self {
            Self::Command => Phase::Inputs,
            Self::Execute => Phase::CommandSaved,
            Self::Acknowledge => Phase::MeasuredComplete,
        }
    }
    fn after(self) -> Phase {
        match self {
            Self::Command => Phase::CommandSaved,
            Self::Execute => Phase::MeasuredComplete,
            Self::Acknowledge => Phase::ReadySource,
        }
    }
}

pub(in crate::native_service::pair) struct RetainedNativeBootstrap {
    operation: NativeBootstrapOperation,
    workspace: AuthorizationWorkspace,
    phase: Phase,
    #[cfg(test)]
    observation: Observation,
}

/// Whole genuine source session plus the same settled accounting workspace.
/// The parent stable conversion aggregate must retain both, not extract copied
/// proof fields or drop ancestry to construct a legacy task-local actor.
pub(in crate::native_service::pair) struct ConnectedSource96 {
    session: SourceRelocationSession,
    workspace: AuthorizationWorkspace,
}

pub(in crate::native_service::pair) struct ConnectedSourceClose {
    operation: motion_catalog_backend::SourceSessionCloseOperation,
    workspace: AuthorizationWorkspace,
}
impl ConnectedSource96 {
    /// Admit only the original service cohort; a requester cannot substitute a
    /// workspace, stopped service, deadline, pool, or native session.
    pub(super) fn admit_relocation94(
        &mut self,
        stopped: &crate::native_service::StoppedService,
        request: motion_catalog_backend::SourceRelocation94Request,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.workspace.reusable(),
            "source94 requires real source-reader retirement ACK"
        );
        self.workspace.verify_service(stopped)?;
        self.session.admit_owned94(
            request,
            deadline,
            super::retained_source_relocation::RetainedSourceRelocation94::storage_reservation(),
            cancel,
        )
    }
    /// Moves-only handoff into an already admitted stable service destination.
    /// Refusal returns the same session and original source-close proof whole.
    pub(super) fn begin_relocation94(
        self,
    ) -> Result<
        (
            motion_catalog_backend::OwnedSourceRelocation94,
            AuthorizationWorkspace,
        ),
        Self,
    > {
        if !self.workspace.reusable() || !self.workspace.has_service_provenance() {
            return Err(self);
        }
        match self.session.begin_owned94() {
            Ok(operation) => Ok((operation, self.workspace)),
            Err(session) => Err(Self {
                session,
                workspace: self.workspace,
            }),
        }
    }
    pub(in crate::native_service::pair) fn into_service_close(
        self,
    ) -> Result<ConnectedSourceClose, Self> {
        if !self.workspace.reusable() || !self.workspace.has_service_provenance() {
            return Err(self);
        }
        match self.session.begin_retained_close() {
            Ok(operation) => Ok(ConnectedSourceClose {
                operation,
                workspace: self.workspace,
            }),
            Err(session) => Err(Self {
                session,
                workspace: self.workspace,
            }),
        }
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_identity(
        &self,
    ) -> &playscale_core::catalog_backend::Identity {
        self.session.identity()
    }
}
impl ConnectedSourceClose {
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        self.workspace.settle_service_close(writer).await?;
        self.operation.close()
    }
    pub(in crate::native_service::pair) fn closed(&self) -> bool {
        self.operation.phase() == motion_catalog_backend::SourceSessionClosePhase::Closed
    }
}

#[derive(Debug)]
struct TargetAuthorizationError(crate::v2::Problem);
impl std::fmt::Display for TargetAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "retained native bootstrap target authorization: {:?}",
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
        "retained native bootstrap authorization cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "retained native bootstrap inherited deadline expired"
    );
    Ok(())
}
fn final_fence(
    target: &TargetAuthentication<'_>,
    current: &CurrentSourceAuthentication<'_>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    check(deadline, cancel)?;
    target.revalidate_temporal().map_err(target_error)?;
    current
        .revalidate_temporal()
        .map_err(current_authorization_error)?;
    Ok(())
}

impl RetainedNativeBootstrap {
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_panic_after_effect(&mut self, yes: bool) {
        self.observation.panic_after_effect = yes;
    }
    #[cfg(test)]
    pub(in crate::native_service::pair) fn service_states(
        &self,
    ) -> (
        motion_catalog_backend::NativeBootstrapPhase,
        motion_catalog_backend::NativeBootstrapCommitState,
        motion_catalog_backend::NativeBootstrapCommitState,
    ) {
        (
            self.operation.phase(),
            self.operation.command_commit_state(),
            self.operation.ack_commit_state(),
        )
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

    /// New simultaneous fixed layouts only. The actual typed79 workspace carries
    /// its previously admitted64MiB cohort and source-close share; neither H nor
    /// native forwarded high water is reduced. Backend target-close slots remain
    /// separately additive, including completed prior-phase boxes.
    pub(super) const fn storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<ConnectedSourceClose>()
            + std::mem::size_of::<Result<ConnectedSourceClose, ConnectedSource96>>()
            + std::mem::size_of::<ConnectedPending96>()
            + std::mem::size_of::<(NativeBootstrapOperation, AuthorizationWorkspace)>()
            + std::mem::size_of::<
                Result<(NativeBootstrapOperation, AuthorizationWorkspace), ConnectedPending96>,
            >()
            + std::mem::size_of::<Result<Self, ConnectedPending96>>()
            + std::mem::size_of::<ReadyNativeSource96>()
            + std::mem::size_of::<Result<ReadyNativeSource96, NativeBootstrapOperation>>()
            + std::mem::size_of::<ConnectedSource96>()
            + std::mem::size_of::<Result<ConnectedSource96, Self>>()
            + std::mem::size_of::<TargetAuthentication<'static>>()
            + std::mem::size_of::<Result<TargetAuthentication<'static>, crate::v2::Problem>>()
            + std::mem::size_of::<CurrentSourceAuthentication<'static>>()
            + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>(
            )
            + std::mem::size_of::<RetainedAuthorizationReader<'static>>()
            + std::mem::size_of::<Result<(), crate::v2::Problem>>()
            + std::mem::size_of::<Result<(), anyhow::Error>>()) as u64
    }
    pub(in crate::native_service::pair) fn admit(
        ready: &mut ConnectedPending96,
        operation_id: &str,
        policy: ReceiptPolicy,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ready.admit_bootstrap(operation_id, policy, caller_deadline, cancel)
    }
    pub(in crate::native_service::pair) fn from_admitted(
        ready: ConnectedPending96,
    ) -> Result<Self, ConnectedPending96> {
        ready.begin_bootstrap().map(|(operation, workspace)| Self {
            operation,
            workspace,
            phase: Phase::Inputs,
            #[cfg(test)]
            observation: Observation::default(),
        })
    }

    pub(super) fn authorize_command<'a, 'source: 'a>(
        &'a mut self,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        self.authorize(Effect::Command, caller, source_reader, cancel)
    }
    pub(super) fn authorize_execute<'a, 'source: 'a>(
        &'a mut self,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        self.authorize(Effect::Execute, caller, source_reader, cancel)
    }
    pub(super) fn authorize_ack<'a, 'source: 'a>(
        &'a mut self,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        self.authorize(Effect::Acknowledge, caller, source_reader, cancel)
    }

    /// Fixed private dispatch, not a validator/effect callback. Each public entry
    /// binds one actual backend reader purpose. Terminal/in-flight status and the
    /// workspace claim are installed before even an unpolled future is returned.
    fn claim(&mut self, effect: Effect) -> anyhow::Result<()> {
        let eligible = self.phase == effect.before();
        self.phase = Phase::Interrupted;
        self.workspace.begin()?;
        ensure!(
            eligible,
            "retained native bootstrap cannot retry or skip phases"
        );
        Ok(())
    }
    fn authorize<'a, 'source: 'a>(
        &'a mut self,
        effect: Effect,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let started = self.claim(effect);
        async move {
            started?;
            self.authorize_claimed(effect, caller, source_reader, cancel)
                .await
        }
    }
    fn authorize_service<'a>(
        &'a mut self,
        effect: Effect,
        caller: &'a Caller,
        writer: &'a mut motion_catalog_backend::motion_snapshot::FreshWriter,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let eligible = self.phase == effect.before();
        self.phase = Phase::Interrupted;
        let started = (|| {
            ensure!(eligible, "retained native bootstrap cannot retry or skip phases");
            ensure!(
                self.workspace.has_service_provenance(),
                "actual service workspace required"
            );
            self.workspace.begin()
        })();
        // Every accepted purpose owns its source claim before an unpolled
        // waiter can be dropped. Rejected gates never claim a fresh reader.
        let source = started.map(|()| writer.authorization_reader());
        async move {
            let reader = source?.await?;
            self.authorize_claimed(effect, caller, reader, cancel).await
        }
    }
    pub(in crate::native_service::pair) fn authorize_service_command<'a>(
        &'a mut self,
        caller: &'a Caller,
        writer: &'a mut motion_catalog_backend::motion_snapshot::FreshWriter,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        self.authorize_service(Effect::Command, caller, writer, cancel)
    }
    pub(in crate::native_service::pair) fn authorize_service_execute<'a>(
        &'a mut self,
        caller: &'a Caller,
        writer: &'a mut motion_catalog_backend::motion_snapshot::FreshWriter,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        self.authorize_service(Effect::Execute, caller, writer, cancel)
    }
    pub(in crate::native_service::pair) fn authorize_service_ack<'a>(
        &'a mut self,
        caller: &'a Caller,
        writer: &'a mut motion_catalog_backend::motion_snapshot::FreshWriter,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        self.authorize_service(Effect::Acknowledge, caller, writer, cancel)
    }
    async fn authorize_claimed(
        &mut self,
        effect: Effect,
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
        target
            .close_retained_native_bootstrap_reader()
            .await
            .map_err(target_error)?;
        // Last await before these effects. Every full mutable-reference,
        // identity, journal and allocation scan precedes the pure fences.
        target
            .preflight_retained_native_bootstrap(cancel)
            .map_err(target_error)?;
        #[cfg(test)]
        self.observation.before_effect(cancel);
        final_fence(&target, &current, deadline, cancel)?;
        match effect {
            Effect::Command => {
                target
                    .insert_retained_native_bootstrap_command(cancel)
                    .map_err(target_error)?;
                #[cfg(test)]
                self.observation.after_insertion(cancel);
                // INSERT's retained resample is not a COMMIT grant.
                final_fence(&target, &current, deadline, cancel)?;
                target
                    .commit_retained_native_bootstrap_command(cancel)
                    .map_err(target_error)?;
            }
            Effect::Execute => {
                // Consumes authority for receipt-first reconciliation and,
                // only if absent, one native execution invocation. Native
                // checkpoints are not per-effect credential revalidation or
                // hard preemption inside SQLite/copy/receipt operations.
                target
                    .execute_retained_native_bootstrap(cancel)
                    .map_err(target_error)?;
            }
            Effect::Acknowledge => {
                target
                    .insert_retained_native_bootstrap_ack(cancel)
                    .map_err(target_error)?;
                #[cfg(test)]
                self.observation.after_insertion(cancel);
                final_fence(&target, &current, deadline, cancel)?;
                target
                    .commit_retained_native_bootstrap_ack(cancel)
                    .map_err(target_error)?;
            }
        }
        #[cfg(test)]
        self.observation.after_effect();
        drop(target);
        // A separate settlement await after the effects. If abandoned, the
        // backend retains its actual committed/measured state, while this
        // app owner remains Interrupted and cannot hand off/retry/reuse H.
        self.workspace
            .retire_current(current, deadline, cancel)
            .await?;
        self.phase = effect.after();
        Ok(())
    }
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        self.phase = Phase::Interrupted;
        self.operation.settle_authorization_reader().await?;
        self.operation.settle_motion()?;
        self.workspace.settle_service_close(writer).await
    }
    pub(in crate::native_service::pair) fn into_source(self) -> Result<ConnectedSource96, Self> {
        if self.phase != Phase::ReadySource || !self.workspace.reusable() {
            return Err(self);
        }
        match self.operation.into_ready() {
            Ok(ready) => Ok(ConnectedSource96 {
                session: ready.into_source_session(),
                workspace: self.workspace,
            }),
            Err(operation) => Err(Self {
                operation,
                workspace: self.workspace,
                phase: Phase::Interrupted,
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
    insertion_expiry: Option<i64>,
    cancel_before_effect: bool,
    cancel_after_insert: bool,
    panic_before_effect: bool,
    panic_after_insert: bool,
    panic_after_effect: bool,
}
#[cfg(test)]
impl Observation {
    async fn after_authorization(&self) {
        if let Some(entered) = &self.authorization_entered {
            entered.store(true, Ordering::Release);
        }
        if let Some(pause) = &self.after_authorization {
            pause.notified().await;
        }
    }
    fn expire(expiry: Option<i64>) {
        if let Some(expiry) = expiry {
            while crate::now() < expiry {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    fn before_effect(&self, cancel: &AtomicBool) {
        Self::expire(self.temporal_expiry);
        if self.cancel_before_effect {
            cancel.store(true, Ordering::Release);
        }
        assert!(
            !self.panic_before_effect,
            "injected bootstrap pre-effect unwind"
        );
    }
    fn after_insertion(&self, cancel: &AtomicBool) {
        Self::expire(self.insertion_expiry);
        if self.cancel_after_insert {
            cancel.store(true, Ordering::Release);
        }
        assert!(
            !self.panic_after_insert,
            "injected bootstrap PendingCommit unwind"
        );
    }
    fn after_effect(&self) {
        assert!(
            !self.panic_after_effect,
            "injected bootstrap post-effect unwind"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        retained_native_apply::RetainedNativeApply,
        retained_native_intent::{
            RetainedApplyFixtureCase as Case, RetainedBootstrapFixture as Fixture,
            RetainedNativeIntent, fixture_for_retained_bootstrap,
        },
    };
    use super::*;
    use motion_catalog_backend::{
        NativeBootstrapCommitState as Commit, NativeBootstrapExecutionState as Execution,
        NativeBootstrapPhase as BackendPhase,
    };
    use sqlx::Connection;
    use std::{fs::File, os::fd::AsRawFd, path::PathBuf, sync::Arc, task::Poll, time::Duration};

    fn operation(f: &mut Fixture, tightened: Option<Instant>) -> RetainedNativeBootstrap {
        let ready = f.ready.as_mut().unwrap();
        RetainedNativeBootstrap::admit(
            ready,
            "app-retained81",
            ReceiptPolicy {
                retention: Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 100,
                max_logical_bytes: 16 * 1024 * 1024,
            },
            tightened.unwrap_or(f.deadline),
            &AtomicBool::new(false),
        )
        .unwrap();
        let operation = match RetainedNativeBootstrap::from_admitted(f.ready.take().unwrap()) {
            Ok(operation) => operation,
            Err(_) => panic!("actual connected81 admission refused"),
        };
        assert_eq!(
            operation.operation.deadline(),
            tightened.map_or(f.deadline, |d| d.min(f.deadline))
        );
        operation
    }
    async fn run(
        operation: &mut RetainedNativeBootstrap,
        f: &mut Fixture,
        effect: Effect,
        preparer: bool,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let caller = if preparer { &f.preparer } else { &f.caller };
        let reader = f.source.authorization_reader().await?;
        match effect {
            Effect::Command => operation.authorize_command(caller, reader, cancel).await,
            Effect::Execute => operation.authorize_execute(caller, reader, cancel).await,
            Effect::Acknowledge => operation.authorize_ack(caller, reader, cancel).await,
        }
    }
    async fn before(operation: &mut RetainedNativeBootstrap, f: &mut Fixture, effect: Effect) {
        let cancel = AtomicBool::new(false);
        if effect != Effect::Command {
            run(operation, f, Effect::Command, true, &cancel)
                .await
                .unwrap();
        }
        if effect == Effect::Acknowledge {
            run(operation, f, Effect::Execute, true, &cancel)
                .await
                .unwrap();
        }
        assert_eq!(operation.phase, effect.before());
        assert!(operation.workspace.reusable());
        assert_eq!(operation.operation.deadline(), f.deadline);
    }
    fn backend_before(effect: Effect) -> BackendPhase {
        match effect {
            Effect::Command => BackendPhase::Inputs,
            Effect::Execute => BackendPhase::CommandSaved,
            Effect::Acknowledge => BackendPhase::MeasuredComplete,
        }
    }
    fn preflighted(effect: Effect) -> BackendPhase {
        match effect {
            Effect::Command => BackendPhase::CommandPreflighted,
            Effect::Execute => BackendPhase::ExecutePreflighted,
            Effect::Acknowledge => BackendPhase::AckPreflighted,
        }
    }
    fn counts_before(effect: Effect) -> (i64, i64, i64) {
        match effect {
            Effect::Command => (0, 0, 0),
            Effect::Execute => (1, 0, 0),
            Effect::Acknowledge => (1, 0, 1),
        }
    }
    fn counts_after(effect: Effect) -> (i64, i64, i64) {
        match effect {
            Effect::Command => (1, 0, 0),
            Effect::Execute => (1, 0, 1),
            Effect::Acknowledge => (1, 1, 1),
        }
    }
    async fn effects(f: &Fixture, expected: (i64, i64, i64), writer_retained: bool) {
        let mut db = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&f.motion)
                .create_if_missing(false)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        for (table, count) in [
            ("catabolic_native_bootstrap_intents", 1),
            ("catabolic_native_bootstrap_commands", expected.0),
            ("catabolic_native_bootstrap_acks", expected.1),
        ] {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
                    .fetch_one(&mut db)
                    .await
                    .unwrap(),
                count
            );
        }
        let acquired = sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut db)
            .await
            .is_ok();
        assert_eq!(
            acquired, !writer_retained,
            "unexpected exact target writer settlement"
        );
        if acquired {
            sqlx::query("ROLLBACK").execute(&mut db).await.unwrap();
        }
        db.close().await.unwrap();
        let mut native = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&f.native)
                .create_if_missing(false)
                .read_only(true)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA user_version")
                .fetch_one(&mut native)
                .await
                .unwrap(),
            41
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM catalog_command_receipts WHERE operation_id='app-retained81'"
            )
            .fetch_one(&mut native)
            .await
            .unwrap(),
            expected.2
        );
        native.close().await.unwrap();
        let mut lock = f.native.as_os_str().to_owned();
        lock.push(".lock");
        let lock = File::options()
            .read(true)
            .write(true)
            .open(PathBuf::from(lock))
            .unwrap();
        assert_ne!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "actual native lease lost"
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                f.app.app.db.begin_with("BEGIN IMMEDIATE")
            )
            .await
            .is_err(),
            "original current writer lost"
        );
    }
    async fn cleanup(mut f: Fixture, mut operation: RetainedNativeBootstrap) {
        operation.settle_authorization_reader().await.unwrap();
        operation
            .workspace
            .settle_source_close(&mut f.source)
            .await
            .unwrap();
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }
    fn no_handoff(operation: RetainedNativeBootstrap) -> RetainedNativeBootstrap {
        match operation.into_source() {
            Err(operation) => operation,
            Ok(_) => panic!("interrupted phase minted source handoff"),
        }
    }
    async fn no_retry(operation: &mut RetainedNativeBootstrap, f: &mut Fixture, _effect: Effect) {
        // The original source claim is itself single-use until real retirement,
        // so a repeated app call cannot even obtain another current reader.
        assert!(f.source.authorization_reader().await.is_err());
        assert_eq!(operation.phase, Phase::Interrupted);
        assert!(!operation.workspace.reusable());
    }
    async fn poll_to_signal<F: Future<Output = anyhow::Result<()>>>(
        waiter: std::pin::Pin<&mut F>,
        entered: &AtomicBool,
    ) {
        let mut waiter = waiter;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_connected_success_reuses_one_admitted_auth_cohort() {
        let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
        // These are the exact additional-host values passed by the connected
        // constructors, not observations of private backend H or an RSS claim.
        assert!(
            RetainedNativeIntent::connected_storage_reservation()
                >= super::super::AUTHORIZATION_RESERVE
        );
        assert!(
            RetainedNativeApply::connected_storage_reservation()
                < super::super::AUTHORIZATION_RESERVE
        );
        assert!(
            RetainedNativeBootstrap::storage_reservation() < super::super::AUTHORIZATION_RESERVE
        );
        let mut operation = operation(&mut f, None);
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            run(
                &mut operation,
                &mut f,
                effect,
                false,
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
            assert_eq!(operation.phase, effect.after());
            assert!(operation.workspace.reusable());
            assert_eq!(operation.operation.deadline(), f.deadline);
            effects(&f, counts_after(effect), effect != Effect::Acknowledge).await;
        }
        assert_eq!(
            operation.operation.execution_state(),
            Execution::CompleteMeasured
        );
        assert_eq!(
            operation.operation.command_commit_state(),
            Commit::WriterReacquired
        );
        assert_eq!(
            operation.operation.ack_commit_state(),
            Commit::WriterReacquired
        );
        let source = match operation.into_source() {
            Ok(source) => source,
            Err(_) => panic!("genuine completed source handoff refused"),
        };
        assert!(source.workspace.reusable());
        assert_eq!(source.session.identity().schema_version, 41);
        effects(&f, (1, 1, 1), false).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(source);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_missing_admission_returns_whole_connected_owner() {
        let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
        let ready = match RetainedNativeBootstrap::from_admitted(f.ready.take().unwrap()) {
            Err(ready) => ready,
            Ok(_) => panic!("unadmitted connected owner accepted"),
        };
        assert!(ready.workspace_reusable());
        assert_eq!(ready.deadline(), f.deadline);
        effects(&f, (0, 0, 0), true).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(ready);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_rejected_service_gates_never_claim_source_reader() {
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            assert!(!operation.workspace.has_service_provenance());
            let cancel = AtomicBool::new(false);
            drop(operation.authorize_service(effect, &f.caller, &mut f.source, &cancel));
            assert_eq!(operation.phase, Phase::Interrupted);
            assert!(operation.workspace.reusable());
            assert!(!f.source.authorization_claimed());
            assert!(!f.source.retains_authorization_reader());
            assert!(!f.source.retains_authorization_acquisition());
            let error = operation.authorize_service(effect, &f.caller, &mut f.source, &cancel)
                .await.unwrap_err();
            assert_eq!(error.to_string(), "retained native bootstrap cannot retry or skip phases");
            assert!(operation.workspace.reusable());
            assert!(!f.source.authorization_claimed());
            assert!(!f.source.retains_authorization_reader());
            assert!(!f.source.retains_authorization_acquisition());
            assert_eq!(operation.operation.phase(), backend_before(effect));
            effects(&f, counts_before(effect), true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_unpolled_future_terminal_for_each_fresh_purpose() {
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            drop(operation.authorize(
                effect,
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &AtomicBool::new(false),
            ));
            assert_eq!(operation.phase, Phase::Interrupted);
            assert!(!operation.workspace.reusable());
            assert_eq!(operation.operation.phase(), backend_before(effect));
            no_retry(&mut operation, &mut f, effect).await;
            effects(&f, counts_before(effect), true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_dropped_auth_waiter_retains_exact_backend_and_original_writer()
    {
        let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
        let mut operation = operation(&mut f, None);
        let entered = Arc::new(AtomicBool::new(false));
        operation.observation.authorization_entered = Some(entered.clone());
        operation.observation.after_authorization = Some(Arc::new(tokio::sync::Notify::new()));
        let cancel = AtomicBool::new(false);
        {
            let mut waiter =
                std::pin::pin!(run(&mut operation, &mut f, Effect::Command, false, &cancel));
            poll_to_signal(waiter.as_mut(), &entered).await;
        }
        assert_eq!(operation.operation.phase(), BackendPhase::Inputs);
        no_retry(&mut operation, &mut f, Effect::Command).await;
        effects(&f, (0, 0, 0), true).await;
        cleanup(f, no_handoff(operation)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_real_target_revocation_denies_each_new_purpose() {
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::RevokedTarget).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            let error = run(
                &mut operation,
                &mut f,
                effect,
                false,
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
            let denied = error
                .downcast_ref::<TargetAuthorizationError>()
                .expect("actual target revocation required");
            assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
            assert_eq!(
                denied.0.code,
                playscale_core::access::AccessError::CredentialRevoked.code()
            );
            assert_eq!(operation.operation.phase(), backend_before(effect));
            effects(&f, counts_before(effect), true).await;
            // The failed genuine prepare left this purpose's actual reader in
            // the backend. Cleanup is reachable from the production app owner
            // and can be abandoned/resumed without authorizing another purpose.
            drop(operation.settle_authorization_reader());
            assert_eq!(operation.phase, Phase::Interrupted);
            assert_eq!(operation.operation.phase(), BackendPhase::Failed);
            assert!(f.source.retains_authorization_reader());
            assert!(!operation.workspace.reusable());
            operation.settle_authorization_reader().await.unwrap();
            operation.settle_authorization_reader().await.unwrap();
            assert!(operation.operation.authorization_reader().await.is_err());
            assert!(!operation.workspace.reusable());
            effects(&f, counts_before(effect), true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_real_current_revocation_denies_each_new_purpose() {
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::RevokedCurrent).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            let error = run(
                &mut operation,
                &mut f,
                effect,
                false,
                &AtomicBool::new(false),
            )
            .await
            .unwrap_err();
            let denied = error
                .downcast_ref::<super::super::CurrentConversionAuthorizationError>()
                .expect("actual original current revocation required");
            assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
            assert_eq!(
                denied.0.code,
                playscale_core::access::AccessError::CredentialRevoked.code()
            );
            assert_eq!(operation.operation.phase(), backend_before(effect));
            effects(&f, counts_before(effect), true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    async fn expired_at_boundary(effect: Effect, after_insert: bool) {
        let mut f = fixture_for_retained_bootstrap(Case::Expiring).await;
        let mut operation = operation(&mut f, None);
        before(&mut operation, &mut f, effect).await;
        let remaining = u64::try_from(f.expiry.unwrap() - crate::now()).unwrap();
        assert!(
            f.deadline > Instant::now() + Duration::from_secs(remaining + 5),
            "credential must expire before inherited operation deadline"
        );
        if after_insert {
            operation.observation.insertion_expiry = f.expiry;
        } else {
            operation.observation.temporal_expiry = f.expiry;
        }
        let error = run(
            &mut operation,
            &mut f,
            effect,
            false,
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        let denied = error
            .downcast_ref::<TargetAuthorizationError>()
            .expect("actual credential expiry, not generic deadline failure");
        assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.0.code,
            playscale_core::access::AccessError::CredentialExpired.code()
        );
        assert!(Instant::now() < f.deadline);
        assert_eq!(
            operation.operation.phase(),
            if after_insert {
                BackendPhase::PendingAckCommit
            } else {
                preflighted(effect)
            }
        );
        effects(&f, counts_before(effect), true).await;
        cleanup(f, no_handoff(operation)).await;
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_actual_expiry_after_preflight_prevents_command_insert() {
        expired_at_boundary(Effect::Command, false).await;
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_actual_expiry_pending_ack_commit_preserves_measured_complete() {
        expired_at_boundary(Effect::Acknowledge, true).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_cancel_after_preflight_prevents_each_effect() {
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            operation.observation.cancel_before_effect = true;
            assert!(
                run(
                    &mut operation,
                    &mut f,
                    effect,
                    false,
                    &AtomicBool::new(false)
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("cancelled")
            );
            assert_eq!(operation.operation.phase(), preflighted(effect));
            effects(&f, counts_before(effect), true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_cancel_pending_commit_does_not_claim_durable_command_or_ack() {
        for effect in [Effect::Command, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            operation.observation.cancel_after_insert = true;
            assert!(
                run(
                    &mut operation,
                    &mut f,
                    effect,
                    false,
                    &AtomicBool::new(false)
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("cancelled")
            );
            assert_eq!(
                operation.operation.phase(),
                if effect == Effect::Command {
                    BackendPhase::PendingCommandCommit
                } else {
                    BackendPhase::PendingAckCommit
                }
            );
            assert_eq!(
                if effect == Effect::Command {
                    operation.operation.command_commit_state()
                } else {
                    operation.operation.ack_commit_state()
                },
                Commit::NotAttempted
            );
            effects(&f, counts_before(effect), true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_unwind_before_effect_after_insert_and_after_effect_retains_truth()
     {
        for (effect, boundary) in [
            (Effect::Command, 0),
            (Effect::Execute, 0),
            (Effect::Acknowledge, 0),
            (Effect::Command, 1),
            (Effect::Acknowledge, 1),
            (Effect::Command, 2),
            (Effect::Execute, 2),
            (Effect::Acknowledge, 2),
        ] {
            let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            operation.observation.panic_before_effect = boundary == 0;
            operation.observation.panic_after_insert = boundary == 1;
            operation.observation.panic_after_effect = boundary == 2;
            let cancel = AtomicBool::new(false);
            {
                let mut waiter =
                    std::pin::pin!(run(&mut operation, &mut f, effect, false, &cancel));
                std::future::poll_fn(|cx| {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        waiter.as_mut().poll(cx)
                    })) {
                        Err(_) => Poll::Ready(()),
                        Ok(Poll::Pending) => Poll::Pending,
                        Ok(Poll::Ready(result)) => {
                            panic!("expected injected unwind, got {result:?}")
                        }
                    }
                })
                .await;
            }
            assert_eq!(operation.phase, Phase::Interrupted);
            assert!(!operation.workspace.reusable());
            let expected_phase = match (effect, boundary) {
                (_, 0) => preflighted(effect),
                (Effect::Command, 1) => BackendPhase::PendingCommandCommit,
                (Effect::Acknowledge, 1) => BackendPhase::PendingAckCommit,
                (Effect::Command, 2) => BackendPhase::CommandSaved,
                (Effect::Execute, 2) => BackendPhase::MeasuredComplete,
                (Effect::Acknowledge, 2) => BackendPhase::ReadySource,
                _ => unreachable!(),
            };
            assert_eq!(operation.operation.phase(), expected_phase);
            effects(
                &f,
                if boundary == 2 {
                    counts_after(effect)
                } else {
                    counts_before(effect)
                },
                !(boundary == 2 && effect == Effect::Acknowledge),
            )
            .await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_post_effect_cancel_keeps_actual_unpolled_current_close_box() {
        for effect in [Effect::Command, Effect::Execute, Effect::Acknowledge] {
            let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
            let mut operation = operation(&mut f, None);
            before(&mut operation, &mut f, effect).await;
            let entered = Arc::new(AtomicBool::new(false));
            operation.workspace.retirement_entered = Some(entered.clone());
            operation.workspace.retirement_gate = Some(Arc::new(tokio::sync::Notify::new()));
            let cancel = AtomicBool::new(false);
            {
                let mut waiter =
                    std::pin::pin!(run(&mut operation, &mut f, effect, false, &cancel));
                poll_to_signal(waiter.as_mut(), &entered).await;
                cancel.store(true, Ordering::Release);
            }
            assert_eq!(operation.phase, Phase::Interrupted);
            assert!(!operation.workspace.reusable());
            // Presence queries the actual retained Box, not merely phase. Exact
            // stable Box-pointer identity is inspected by backend slot tests.
            assert!(operation.workspace.retains_unjoined_close());
            assert_eq!(
                operation.operation.phase(),
                match effect {
                    Effect::Command => BackendPhase::CommandSaved,
                    Effect::Execute => BackendPhase::MeasuredComplete,
                    Effect::Acknowledge => BackendPhase::ReadySource,
                }
            );
            no_retry(&mut operation, &mut f, effect).await;
            assert!(operation.workspace.retains_unjoined_close());
            effects(&f, counts_after(effect), effect != Effect::Acknowledge).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_tightened_queued_deadline_does_not_renew_original_ceiling() {
        let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(deadline < f.deadline);
        let mut operation = operation(&mut f, Some(deadline));
        tokio::time::sleep_until(deadline.into()).await;
        let error = run(
            &mut operation,
            &mut f,
            Effect::Command,
            false,
            &AtomicBool::new(false),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("deadline expired"));
        assert_eq!(operation.operation.phase(), BackendPhase::Inputs);
        no_retry(&mut operation, &mut f, Effect::Command).await;
        effects(&f, (0, 0, 0), true).await;
        cleanup(f, no_handoff(operation)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained_bootstrap_app_wrong_order_consumes_cohort_and_never_skips_command() {
        let mut f = fixture_for_retained_bootstrap(Case::Valid).await;
        let mut operation = operation(&mut f, None);
        assert!(
            run(
                &mut operation,
                &mut f,
                Effect::Acknowledge,
                false,
                &AtomicBool::new(false)
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("skip phases")
        );
        assert_eq!(operation.operation.phase(), BackendPhase::Inputs);
        no_retry(&mut operation, &mut f, Effect::Command).await;
        effects(&f, (0, 0, 0), true).await;
        cleanup(f, no_handoff(operation)).await;
    }
}
