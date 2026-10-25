//! Disconnected application-authenticated93→97 prelude to connected79→80→81.
//! Actual recorded writer/original/reference/publication ownership stays inline
//! across genuine current and target authentication. No service adoption,
//! ordinary Runtime admission, staged-target custody or paired95 claim is made.
//! The original source writer remains independently retained and borrowed by its
//! real reader. Reader acquisition and its returned connection remain in those
//! original owners when auth fails or is abandoned. SQLx internal pre-return
//! failure, WorkerCrashed, hard preemption and owner destruction remain limits.
use super::super::{AUTHORIZATION_RESERVE, current_authorization_error};
use super::{AuthorizationWorkspace, WorkspacePhase};
use crate::v2::auth::{
    Caller,
    retained::{CurrentSourceAuthentication, TargetAuthentication},
};
use anyhow::{Context, ensure};
use motion_catalog_backend::{
    RecordedAdmittedControl, RecordedControlOperation,
    motion_snapshot::{RetainedAuthorizationCloseSlot, RetainedAuthorizationReader},
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
    Ready97,
}

/// No backend/workspace accessor, retry, replacement or consuming fallible work.
/// The service owner must retain this whole slot; destroying it remains outside
/// this disconnected slice. There is no Drop sink that claims to repair custody.
pub(in super::super) struct RetainedControl {
    operation: RecordedControlOperation,
    workspace: AuthorizationWorkspace,
    phase: Phase,
    #[cfg(test)]
    observation: Observation,
}

/// Only the enclosing native-intent module can inspect this pair. A naked backend
/// Ready97 cannot manufacture it, and the pair is never exposed as two values.
pub(in crate::native_service::pair) struct ConnectedRecorded97 {
    pub(super) ready: RecordedAdmittedControl,
    pub(super) workspace: AuthorizationWorkspace,
}

impl ConnectedRecorded97 {
    pub(in crate::native_service::pair) fn from_service(
        owner: super::super::super::source_image::supervisor::ServiceRecorded97,
    ) -> Self {
        let (ready, workspace) = owner.into_parts();
        Self { ready, workspace }
    }
    pub(in crate::native_service::pair) async fn settle_service(
        &mut self,
        writer: &mut motion_catalog_backend::motion_snapshot::FreshWriter,
    ) -> anyhow::Result<()> {
        self.ready.settle()?;
        self.workspace.settle_service_close(writer).await
    }
}
#[derive(Debug)]
struct TargetAuthorizationError(crate::v2::Problem);
impl std::fmt::Display for TargetAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retained control target authorization: {:?}", self.0)
    }
}
impl std::error::Error for TargetAuthorizationError {}
fn target_error(error: crate::v2::Problem) -> anyhow::Error {
    TargetAuthorizationError(error).into()
}
fn check(deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "retained control authorization cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "retained control inherited deadline expired"
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

impl RetainedControl {
    /// Terminal target-reader cleanup only. Revoke this app continuation before
    /// returning even an unpolled waiter, then borrow the exact backend slots.
    /// This does not acknowledge effects, create Ready, or settle/reuse the
    /// independent current-source workspace. Failure retains those same slots.
    pub(in super::super) fn settle_authorization_reader(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.phase = Phase::Interrupted;
        self.operation.settle_authorization_reader()
    }

    /// Fixed simultaneous owner/result/token layouts, one64MiB auth cohort and
    /// one independent current-close slot. The backend separately admits its own
    /// target-close box, which survives this workspace's later reuse in79/80/81.
    /// This adds to inherited H; it neither subtracts old owners nor promises RSS.
    pub(in super::super) const fn storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<Result<Self, RecordedControlOperation>>()
            + std::mem::size_of::<ConnectedRecorded97>()
            + std::mem::size_of::<Result<ConnectedRecorded97, Self>>()
            + std::mem::size_of::<TargetAuthentication<'static>>()
            + std::mem::size_of::<Result<TargetAuthentication<'static>, crate::v2::Problem>>()
            + std::mem::size_of::<CurrentSourceAuthentication<'static>>()
            + std::mem::size_of::<Result<CurrentSourceAuthentication<'static>, crate::v2::Problem>>(
            )
            + std::mem::size_of::<RetainedAuthorizationReader<'static>>()
            + std::mem::size_of::<Result<RetainedAuthorizationReader<'static>, anyhow::Error>>()
            + std::mem::size_of::<Result<(), crate::v2::Problem>>()
            + std::mem::size_of::<Result<(), anyhow::Error>>()) as u64
            + AUTHORIZATION_RESERVE
            + motion_catalog_backend::motion_snapshot::FreshWriter::authorization_storage_reservation()
            + RetainedAuthorizationCloseSlot::storage_reservation()
    }

    /// Dedicated additive admission while the whole prepared backend remains in
    /// its caller's slot. An arbitrary external H cannot satisfy this named grant.
    pub(in super::super) fn admit(
        operation: &mut RecordedControlOperation,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        operation.admit_retained_authorization(Self::storage_reservation(), cancel)
    }

    /// Only moves after dedicated backend admission. The new workspace is tied
    /// to this exact prepared owner; no caller scalar or detached grant is used.
    pub(in super::super) fn from_admitted(
        operation: RecordedControlOperation,
    ) -> Result<Self, RecordedControlOperation> {
        if !operation.retained_authorization_storage_covers(Self::storage_reservation()) {
            return Err(operation);
        }
        Ok(Self {
            operation,
            workspace: AuthorizationWorkspace {
                service: None,
                source_close: RetainedAuthorizationCloseSlot::default(),
                phase: WorkspacePhase::Reusable,
                #[cfg(test)]
                retirement_entered: None,
                #[cfg(test)]
                retirement_gate: None,
            },
            phase: Phase::Inputs,
            #[cfg(test)]
            observation: Observation::default(),
        })
    }

    /// Claim the one attempt and workspace synchronously, including an unpolled
    /// future. Every backend owner remains in self, not in the returned waiter.
    /// A future service driver must claim eligibility before opening its current
    /// reader; this private seam does not fund arbitrary repeated acquisitions.
    pub(in super::super) fn authorize_and_commit<'a, 'source: 'a>(
        &'a mut self,
        caller: &'a Caller,
        source_reader: RetainedAuthorizationReader<'source>,
        cancel: &'a AtomicBool,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        let eligible = self.phase == Phase::Inputs;
        self.phase = Phase::Interrupted;
        let started = self.workspace.begin().and_then(|()| {
            ensure!(eligible, "retained control authorization cannot retry");
            Ok(())
        });
        async move {
            started?;
            let deadline = self
                .operation
                .deadline()
                .context("retained control deadline missing")?;
            check(deadline, cancel)?;
            let mut current = CurrentSourceAuthentication::prepare(
                source_reader,
                caller,
                Permission::SystemAdmin,
            )
            .await
            .map_err(current_authorization_error)?;
            check(deadline, cancel)?;
            let reader = self.operation.authorization_reader().await?;
            let mut target = TargetAuthentication::prepare(reader, caller, Permission::SystemAdmin)
                .await
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.after_authorization().await?;
            target.revalidate().await.map_err(target_error)?;
            current
                .revalidate()
                .await
                .map_err(current_authorization_error)?;
            target
                .close_retained_control_reader()
                .await
                .map_err(target_error)?;
            // All IO/full scans occur before these fresh pure checks. No await,
            // general callback or declaration witness can cross the effect edge.
            target
                .preflight_retained_control(cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.before_effect(cancel);
            final_fence(&target, &current, deadline, cancel)?;
            target
                .apply_retained_control(cancel)
                .map_err(target_error)?;
            #[cfg(test)]
            self.observation.after_effect(cancel);
            // Migration/post-effect scans/publication may consume authority.
            // Recheck both real tokens immediately before scan-free COMMIT.
            final_fence(&target, &current, deadline, cancel)?;
            target
                .commit_retained_control(cancel)
                .map_err(target_error)?;
            // Reacquisition is separate from COMMIT and may fail after durable97.
            target
                .finish_retained_control(cancel)
                .map_err(target_error)?;
            drop(target);
            self.workspace
                .retire_current(current, deadline, cancel)
                .await?;
            self.phase = Phase::Ready97;
            Ok(())
        }
    }

    /// Successful current close acknowledgment and ended token are mandatory
    /// before this opaque pair can move to native intent's borrowed admission.
    pub(in super::super) fn into_connected_ready(self) -> Result<ConnectedRecorded97, Self> {
        if self.phase != Phase::Ready97 || !self.workspace.reusable() {
            return Err(self);
        }
        match self.operation.into_admitted() {
            Ok(ready) => Ok(ConnectedRecorded97 {
                ready,
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
    authorization_gate: Option<std::sync::Arc<tokio::sync::Notify>>,
    error_after_authorization: bool,
    effect_expiry: Option<i64>,
    commit_expiry: Option<i64>,
    cancel_before_effect: bool,
    cancel_after_effect: bool,
    panic_before_effect: bool,
    panic_after_effect: bool,
}
#[cfg(test)]
impl Observation {
    async fn after_authorization(&self) -> anyhow::Result<()> {
        if let Some(entered) = &self.authorization_entered {
            entered.store(true, Ordering::Release);
        }
        if let Some(gate) = &self.authorization_gate {
            gate.notified().await;
        }
        ensure!(
            !self.error_after_authorization,
            "actual retained control failure after successful prepare"
        );
        Ok(())
    }
    fn expire(expiry: Option<i64>) {
        if let Some(expiry) = expiry {
            assert!(
                crate::now() < expiry,
                "real credential expired before test fence"
            );
            while crate::now() < expiry {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    fn before_effect(&self, cancel: &AtomicBool) {
        Self::expire(self.effect_expiry);
        if self.cancel_before_effect {
            cancel.store(true, Ordering::Release);
        }
        assert!(!self.panic_before_effect, "retained control pre-DDL unwind");
    }
    fn after_effect(&self, cancel: &AtomicBool) {
        Self::expire(self.commit_expiry);
        if self.cancel_after_effect {
            cancel.store(true, Ordering::Release);
        }
        assert!(
            !self.panic_after_effect,
            "retained control PendingCommit unwind"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        retained_native_apply::RetainedNativeApply,
        retained_native_bootstrap::RetainedNativeBootstrap,
    };
    use super::super::{
        RetainedNativeIntent,
        application_fixture::{Fixture as ApplicationFixture, OPERATOR},
    };
    use super::*;
    use motion_catalog_backend::{
        OfflineControlShutdownFacts, RecordedCommitState, RecordedControlPhase,
        motion_snapshot::FreshWriter,
    };
    use std::{
        fs::File,
        io::Cursor,
        path::{Path, PathBuf},
        sync::Arc,
        task::Poll,
        time::Duration,
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
        prepared: Option<RecordedControlOperation>,
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

    async fn fixture_with_limit(
        expiring: bool,
        revocation: Revocation,
        limit: Duration,
        extra_host: u64,
    ) -> Fixture {
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
                "app-retained97-record",
                &catabolic_core::sha256(b"actual-selected-pair"),
                &OfflineControlShutdownFacts {
                    service_admission_stopped: true,
                    native_workers_joined: true,
                    application_owners_drained: true,
                    exclusive_operational_ownership_retained: true,
                },
                source_bytes + extra_host,
                Instant::now() + limit,
                &cancel,
            )
            .await
            .unwrap();
        let _ = preparer; // Reserve admin makes public revocation legal.
        Fixture {
            app,
            _stage: stage,
            _original: original,
            native,
            motion,
            source,
            caller,
            expiry,
            prepared: Some(operation),
        }
    }
    async fn fixture(expiring: bool, revocation: Revocation) -> Fixture {
        fixture_with_limit(expiring, revocation, Duration::from_secs(180), 0).await
    }
    fn operation(f: &mut Fixture) -> RetainedControl {
        RetainedControl::admit(f.prepared.as_mut().unwrap(), &AtomicBool::new(false)).unwrap();
        match RetainedControl::from_admitted(f.prepared.take().unwrap()) {
            Ok(operation) => operation,
            Err(_) => panic!("dedicated app97 admission rejected"),
        }
    }
    fn no_handoff(operation: RetainedControl) -> RetainedControl {
        match operation.into_connected_ready() {
            Err(operation) => operation,
            Ok(_) => panic!("interrupted retained97 yielded connected handoff"),
        }
    }
    async fn run(
        operation: &mut RetainedControl,
        f: &mut Fixture,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        operation
            .authorize_and_commit(&f.caller, f.source.authorization_reader().await?, cancel)
            .await
    }
    async fn durable_version_and_locked(path: &Path, version: i64, locked: bool) {
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
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            version
        );
        assert_eq!(
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut db)
                .await
                .is_err(),
            locked,
            "retained target writer custody changed"
        );
        db.close().await.unwrap();
    }
    async fn pending_effects_are_not_committed(operation: &mut RetainedControl, path: &Path) {
        use sqlx::Connection;
        // Rollback-journal DDL may legitimately hold an EXCLUSIVE lock after
        // cache spill. Do not require an independent schema read before rollback.
        let mut contender = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        assert!(
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut contender)
                .await
                .is_err()
        );
        contender.close().await.unwrap();
        operation.operation.settle().unwrap();
        durable_version_and_locked(path, 93, false).await;
    }
    async fn cleanup(mut f: Fixture, mut operation: RetainedControl) {
        operation.settle_authorization_reader().await.unwrap();
        operation
            .workspace
            .settle_source_close(&mut f.source)
            .await
            .unwrap();
        operation.operation.settle().unwrap();
        f.source.close().await.unwrap();
        drop(f.source);
        drop(operation);
        f.app.app.db.close().await;
    }
    async fn poll_to_signal<F: Future>(mut future: std::pin::Pin<&mut F>, entered: &AtomicBool) {
        tokio::time::timeout(
            Duration::from_secs(30),
            std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
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
    async fn retained97_app_real_current_and_target_authentication_commits_with_original_deadline()
    {
        let mut f = fixture(false, Revocation::None).await;
        let deadline = f.prepared.as_ref().unwrap().deadline().unwrap();
        let mut operation = operation(&mut f);
        run(&mut operation, &mut f, &AtomicBool::new(false))
            .await
            .unwrap();
        assert_eq!(operation.phase, Phase::Ready97);
        assert_eq!(operation.operation.phase(), RecordedControlPhase::Ready97);
        assert_eq!(
            operation.operation.commit_state(),
            RecordedCommitState::WriterReacquired
        );
        assert_eq!(operation.operation.deadline(), Some(deadline));
        assert!(operation.workspace.reusable());
        durable_version_and_locked(&f.motion, 97, true).await;
        let mut ready = match operation.into_connected_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("genuine97 did not yield its connected owner"),
        };
        assert!(ready.workspace.reusable());
        ready.ready.settle().unwrap();
        f.source.close().await.unwrap();
        drop(f.source);
        drop(ready);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_real_target_and_current_revocation_refuse_before_ddl() {
        for revocation in [Revocation::Target, Revocation::Current] {
            let mut f = fixture(false, revocation).await;
            let mut operation = operation(&mut f);
            // Independently prove the other catalog's grant. It must not rescue
            // the revoked one, and neither is a fabricated authentication token.
            if revocation == Revocation::Current {
                let mut target = TargetAuthentication::prepare(
                    operation.operation.authorization_reader().await.unwrap(),
                    &f.caller,
                    Permission::SystemAdmin,
                )
                .await
                .unwrap();
                target.revalidate().await.unwrap();
            }
            let error = run(&mut operation, &mut f, &AtomicBool::new(false))
                .await
                .unwrap_err();
            let denied = if revocation == Revocation::Target {
                &error
                    .downcast_ref::<TargetAuthorizationError>()
                    .expect("actual target denial")
                    .0
            } else {
                &error
                    .downcast_ref::<super::super::super::CurrentConversionAuthorizationError>()
                    .expect("actual current denial")
                    .0
            };
            assert_eq!(denied.status, axum::http::StatusCode::UNAUTHORIZED);
            assert!(f.source.retains_authorization_reader());
            assert!(f.source.authorization_claimed());
            assert_eq!(
                denied.code,
                playscale_core::access::AccessError::CredentialRevoked.code()
            );
            assert_eq!(operation.phase, Phase::Interrupted);
            assert_eq!(
                operation.operation.phase(),
                RecordedControlPhase::Prepared93
            );
            assert_eq!(
                operation.operation.commit_state(),
                RecordedCommitState::NotAttempted
            );
            durable_version_and_locked(&f.motion, 93, true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_real_expiry_before_ddl_and_after_scans_prevents_commit() {
        for commit_edge in [false, true] {
            let mut f = fixture(true, Revocation::None).await;
            let mut operation = operation(&mut f);
            let deadline = operation.operation.deadline().unwrap();
            let remaining = u64::try_from(f.expiry.unwrap() - crate::now()).unwrap();
            assert!(deadline > Instant::now() + Duration::from_secs(remaining + 5));
            if commit_edge {
                operation.observation.commit_expiry = f.expiry;
            } else {
                operation.observation.effect_expiry = f.expiry;
            }
            let error = run(&mut operation, &mut f, &AtomicBool::new(false))
                .await
                .unwrap_err();
            let denied = error
                .downcast_ref::<TargetAuthorizationError>()
                .expect("real temporal target denial");
            assert_eq!(denied.0.status, axum::http::StatusCode::UNAUTHORIZED);
            assert_eq!(
                denied.0.code,
                playscale_core::access::AccessError::CredentialExpired.code()
            );
            assert!(Instant::now() < deadline);
            assert_eq!(
                operation.operation.phase(),
                if commit_edge {
                    RecordedControlPhase::PendingCommit
                } else {
                    RecordedControlPhase::Preflighted93
                }
            );
            assert_eq!(
                operation.operation.commit_state(),
                RecordedCommitState::NotAttempted
            );
            assert_eq!(operation.phase, Phase::Interrupted);
            if commit_edge {
                pending_effects_are_not_committed(&mut operation, &f.motion).await;
            } else {
                durable_version_and_locked(&f.motion, 93, true).await;
            }
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_cancel_and_unwind_at_both_effect_edges_retain_owners() {
        for commit_edge in [false, true] {
            for unwind in [false, true] {
                let mut f = fixture(false, Revocation::None).await;
                let mut operation = operation(&mut f);
                match (commit_edge, unwind) {
                    (false, false) => operation.observation.cancel_before_effect = true,
                    (true, false) => operation.observation.cancel_after_effect = true,
                    (false, true) => operation.observation.panic_before_effect = true,
                    (true, true) => operation.observation.panic_after_effect = true,
                }
                let cancel = AtomicBool::new(false);
                if unwind {
                    let mut waiter = std::pin::pin!(run(&mut operation, &mut f, &cancel));
                    std::future::poll_fn(|cx| {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            waiter.as_mut().poll(cx)
                        })) {
                            Ok(Poll::Pending) => Poll::Pending,
                            Ok(Poll::Ready(_)) => panic!("effect-edge test did not unwind"),
                            Err(_) => Poll::Ready(()),
                        }
                    })
                    .await;
                } else {
                    assert!(run(&mut operation, &mut f, &cancel).await.is_err());
                }
                assert_eq!(operation.phase, Phase::Interrupted);
                assert!(!operation.workspace.reusable());
                assert_eq!(
                    operation.operation.phase(),
                    if commit_edge {
                        RecordedControlPhase::PendingCommit
                    } else {
                        RecordedControlPhase::Preflighted93
                    }
                );
                assert_eq!(
                    operation.operation.commit_state(),
                    RecordedCommitState::NotAttempted
                );
                if commit_edge {
                    pending_effects_are_not_committed(&mut operation, &f.motion).await;
                } else {
                    durable_version_and_locked(&f.motion, 93, true).await;
                }
                assert!(operation.operation.apply(&AtomicBool::new(false)).is_err());
                cleanup(f, no_handoff(operation)).await;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_unpolled_and_dropped_auth_waiters_are_terminal() {
        for polled in [false, true] {
            let mut f = fixture(false, Revocation::None).await;
            let mut operation = operation(&mut f);
            let entered = Arc::new(AtomicBool::new(false));
            operation.observation.authorization_entered = Some(entered.clone());
            operation.observation.authorization_gate = Some(Arc::new(tokio::sync::Notify::new()));
            let cancel = AtomicBool::new(false);
            {
                // Call the production non-async entry, not the async test helper,
                // so an unpolled waiter really tests synchronous attempt claiming.
                let reader = f.source.authorization_reader().await.unwrap();
                let mut waiter =
                    std::pin::pin!(operation.authorize_and_commit(&f.caller, reader, &cancel));
                if polled {
                    poll_to_signal(waiter.as_mut(), &entered).await;
                }
            }
            assert_eq!(operation.phase, Phase::Interrupted);
            assert!(!operation.workspace.reusable());
            assert!(f.source.retains_authorization_reader());
            assert!(f.source.authorization_claimed());
            assert_eq!(
                operation.operation.phase(),
                RecordedControlPhase::Prepared93
            );
            assert!(operation.operation.apply(&cancel).is_err());
            assert!(run(&mut operation, &mut f, &cancel).await.is_err());
            durable_version_and_locked(&f.motion, 93, true).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    async fn original_source_remains_reserved(f: &Fixture) {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(30),
                f.app.app.db.begin_with("BEGIN IMMEDIATE"),
            )
            .await
            .is_err(),
            "the independently retained original writer was released"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_current_prepare_unpolled_error_cancel_and_panic_keep_actual_reader() {
        use crate::v2::auth::retained::PreparationObservation;
        // Each case enters a genuine reader against the original WRITE owner.
        // A canceled/panicked decoder loses only its borrow, not the connection.
        for interruption in 0..4 {
            let mut f = fixture(false, Revocation::None).await;
            let mut operation = operation(&mut f);
            operation.workspace.begin().unwrap();
            let mut observation = PreparationObservation::default();
            observation.fail = interruption == 1;
            observation.panic = interruption == 3;
            if interruption == 2 {
                observation.gate = Some(Arc::new(tokio::sync::Notify::new()));
            }
            {
                let reader = f.source.authorization_reader().await.unwrap();
                let prepare = CurrentSourceAuthentication::prepare_observed(
                    reader,
                    &f.caller,
                    Permission::SystemAdmin,
                    &observation,
                );
                if interruption == 0 {
                    drop(prepare);
                } else if interruption == 1 {
                    assert!(prepare.await.is_err());
                } else {
                    let mut prepare = std::pin::pin!(prepare);
                    if interruption == 2 {
                        poll_to_signal(prepare.as_mut(), &observation.entered).await;
                    } else {
                        std::future::poll_fn(|cx| {
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                prepare.as_mut().poll(cx)
                            })) {
                                Ok(Poll::Pending) => Poll::Pending,
                                Ok(Poll::Ready(_)) => panic!("real current prepare did not unwind"),
                                Err(_) => Poll::Ready(()),
                            }
                        })
                        .await;
                    }
                }
            }
            assert!(f.source.retains_authorization_reader());
            assert!(f.source.retains_authorization_acquisition());
            assert!(f.source.authorization_claimed());
            assert!(f.source.authorization_reader().await.is_err());
            assert!(!operation.workspace.reusable());
            assert_eq!(
                operation.operation.phase(),
                RecordedControlPhase::Prepared93
            );
            assert!(operation.operation.apply(&AtomicBool::new(false)).is_err());
            original_source_remains_reserved(&f).await;
            durable_version_and_locked(&f.motion, 93, true).await;
            // Terminal cleanup joins the actual retained reader but never makes
            // this interrupted authentication cohort reusable.
            operation
                .workspace
                .settle_source_close(&mut f.source)
                .await
                .unwrap();
            assert_eq!(operation.workspace.phase, WorkspacePhase::Settled);
            assert!(operation.workspace.source_close.is_joined());
            assert!(!operation.workspace.reusable());
            assert!(f.source.authorization_reader().await.is_err());
            original_source_remains_reserved(&f).await;
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_target_prepare_unpolled_error_cancel_and_panic_cannot_reopen_or_effect() {
        use crate::v2::auth::retained::PreparationObservation;
        for interruption in 0..4 {
            let mut f = fixture(false, Revocation::None).await;
            let mut operation = operation(&mut f);
            operation.workspace.begin().unwrap();
            // Retain the actual current reader too, as the real app operation
            // does before preparing this distinct historical target policy.
            let current = CurrentSourceAuthentication::prepare(
                f.source.authorization_reader().await.unwrap(),
                &f.caller,
                Permission::SystemAdmin,
            )
            .await
            .unwrap();
            let mut observation = PreparationObservation::default();
            observation.fail = interruption == 1;
            observation.panic = interruption == 3;
            if interruption == 2 {
                observation.gate = Some(Arc::new(tokio::sync::Notify::new()));
            }
            {
                let reader = operation.operation.authorization_reader().await.unwrap();
                let prepare = TargetAuthentication::prepare_observed(
                    reader,
                    &f.caller,
                    Permission::SystemAdmin,
                    &observation,
                );
                if interruption == 0 {
                    drop(prepare);
                } else if interruption == 1 {
                    assert!(prepare.await.is_err());
                } else {
                    let mut prepare = std::pin::pin!(prepare);
                    if interruption == 2 {
                        poll_to_signal(prepare.as_mut(), &observation.entered).await;
                    } else {
                        std::future::poll_fn(|cx| {
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                prepare.as_mut().poll(cx)
                            })) {
                                Ok(Poll::Pending) => Poll::Pending,
                                Ok(Poll::Ready(_)) => panic!("real target prepare did not unwind"),
                                Err(_) => Poll::Ready(()),
                            }
                        })
                        .await;
                    }
                }
            }
            drop(current);
            assert!(f.source.retains_authorization_reader());
            assert!(operation.operation.authorization_reader().await.is_err());
            assert!(operation.operation.apply(&AtomicBool::new(false)).is_err());
            assert!(!operation.workspace.reusable());
            assert_eq!(
                operation.operation.commit_state(),
                RecordedCommitState::NotAttempted
            );
            original_source_remains_reserved(&f).await;
            durable_version_and_locked(&f.motion, 93, true).await;
            // Backend companion tests inspect the exact retained target slot,
            // FD, connection and connect/close boxes. No phase-only assertion is
            // represented here as a physical reader-presence observation.
            cleanup(f, no_handoff(operation)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_later_error_after_successful_prepare_keeps_both_claims_and_no_effects() {
        let mut f = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut f);
        operation.observation.error_after_authorization = true;
        let error = run(&mut operation, &mut f, &AtomicBool::new(false))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "actual retained control failure after successful prepare"
        );
        assert!(f.source.retains_authorization_reader());
        assert!(f.source.authorization_reader().await.is_err());
        assert!(operation.operation.authorization_reader().await.is_err());
        assert!(!operation.workspace.reusable());
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            RecordedControlPhase::Prepared93
        );
        assert_eq!(
            operation.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        assert!(operation.operation.apply(&AtomicBool::new(false)).is_err());
        original_source_remains_reserved(&f).await;
        durable_version_and_locked(&f.motion, 93, true).await;
        // Reach the real retained reader through the production app cleanup
        // surface, not a test-only backend field. An unpolled cleanup already
        // prohibits all forward continuations; another waiter joins that slot.
        drop(operation.settle_authorization_reader());
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(operation.operation.phase(), RecordedControlPhase::Failed);
        assert!(f.source.retains_authorization_reader());
        assert!(!operation.workspace.reusable());
        operation.settle_authorization_reader().await.unwrap();
        operation.settle_authorization_reader().await.unwrap();
        assert!(operation.operation.authorization_reader().await.is_err());
        assert!(!operation.workspace.reusable());
        assert_eq!(
            operation.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        original_source_remains_reserved(&f).await;
        durable_version_and_locked(&f.motion, 93, true).await;
        cleanup(f, no_handoff(operation)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_current_auth_cannot_enter_or_acknowledge_retained_retirement() {
        let mut f = fixture(false, Revocation::None).await;
        let operation = operation(&mut f);
        // The new operation's public input is RetainedAuthorizationReader, so
        // this distinct backend type cannot be passed to authorize_and_commit.
        // Its shared auth wrapper must also refuse every retained-close path.
        for retirement in 0..3 {
            let mut current = CurrentSourceAuthentication::prepare_legacy(
                f.source.legacy_authorization_reader().await.unwrap(),
                &f.caller,
                Permission::SystemAdmin,
            )
            .await
            .unwrap();
            current.revalidate().await.unwrap();
            let mut slot = RetainedAuthorizationCloseSlot::default();
            match retirement {
                0 => {
                    assert!(current.begin_retained_close(&mut slot).is_err());
                    drop(current);
                }
                1 => assert!(current.finish_retained_close(&mut slot).is_err()),
                _ => assert!(current.retire_inline().await.is_err()),
            }
            assert!(!slot.retains_reader_owner());
            assert!(!slot.is_joined());
            assert!(!f.source.authorization_claimed());
        }
        original_source_remains_reserved(&f).await;
        durable_version_and_locked(&f.motion, 93, true).await;
        cleanup(f, no_handoff(operation)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_unpolled_target_close_stays_in_backend_and_cannot_restart() {
        let mut f = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut f);
        let mut target = TargetAuthentication::prepare(
            operation.operation.authorization_reader().await.unwrap(),
            &f.caller,
            Permission::SystemAdmin,
        )
        .await
        .unwrap();
        target.revalidate().await.unwrap();
        drop(target.close_retained_control_reader());
        assert!(target.revalidate().await.is_err());
        drop(target);
        assert_eq!(
            operation.operation.phase(),
            RecordedControlPhase::AuthorizationClosing
        );
        assert!(operation.operation.authorization_reader().await.is_err());
        assert!(operation.operation.apply(&AtomicBool::new(false)).is_err());
        // Backend companion tests inspect the actual private box, not just phase.
        durable_version_and_locked(&f.motion, 93, true).await;
        cleanup(f, no_handoff(operation)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_current_close_drop_retains_actual_box_without_workspace_reuse() {
        let mut f = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut f);
        let entered = Arc::new(AtomicBool::new(false));
        operation.workspace.retirement_entered = Some(entered.clone());
        operation.workspace.retirement_gate = Some(Arc::new(tokio::sync::Notify::new()));
        let cancel = AtomicBool::new(false);
        {
            let mut waiter = std::pin::pin!(run(&mut operation, &mut f, &cancel));
            poll_to_signal(waiter.as_mut(), &entered).await;
        }
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(operation.operation.phase(), RecordedControlPhase::Ready97);
        assert_eq!(
            operation.operation.commit_state(),
            RecordedCommitState::WriterReacquired
        );
        assert!(operation.workspace.retains_unjoined_close());
        assert!(!operation.workspace.reusable());
        durable_version_and_locked(&f.motion, 97, true).await;
        assert!(run(&mut operation, &mut f, &cancel).await.is_err());
        assert!(operation.workspace.retains_unjoined_close());
        cleanup(f, no_handoff(operation)).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_external_host_scalar_cannot_manufacture_workspace_admission() {
        let mut f = fixture_with_limit(
            false,
            Revocation::None,
            Duration::from_secs(180),
            RetainedControl::storage_reservation(),
        )
        .await;
        let mut original = match RetainedControl::from_admitted(f.prepared.take().unwrap()) {
            Err(original) => original,
            Ok(_) => panic!("external host scalar manufactured app authorization workspace"),
        };
        assert_eq!(original.phase(), RecordedControlPhase::Prepared93);
        durable_version_and_locked(&f.motion, 93, true).await;
        original.settle().unwrap();
        f.source.close().await.unwrap();
        drop(f.source);
        drop(original);
        f.app.app.db.close().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_queued_time_never_renews_original_deadline() {
        let mut f = fixture_with_limit(false, Revocation::None, Duration::from_secs(3), 0).await;
        let mut operation = operation(&mut f);
        let deadline = operation.operation.deadline().unwrap();
        let cancel = AtomicBool::new(false);
        let reader = f.source.authorization_reader().await.unwrap();
        let waiter = operation.authorize_and_commit(&f.caller, reader, &cancel);
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        assert!(waiter.await.is_err());
        assert_eq!(operation.operation.deadline(), Some(deadline));
        assert_eq!(operation.phase, Phase::Interrupted);
        assert_eq!(
            operation.operation.phase(),
            RecordedControlPhase::Prepared93
        );
        durable_version_and_locked(&f.motion, 93, true).await;
        cleanup(f, no_handoff(operation)).await;
    }

    fn quiescence() -> catabolic_store::migration::NativeUpgradeQuiescence {
        catabolic_store::migration::NativeUpgradeQuiescence {
            admission_stopped: true,
            readers_and_workers_stopped: true,
            exclusive_operational_ownership: true,
        }
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn retained97_app_connected_handoff_keeps_one_workspace_through79_80_81() {
        let mut f = fixture(false, Revocation::None).await;
        let mut operation = operation(&mut f);
        let deadline = operation.operation.deadline().unwrap();
        let retirement = Arc::new(AtomicBool::new(false));
        operation.workspace.retirement_entered = Some(retirement.clone());
        let cancel = AtomicBool::new(false);
        run(&mut operation, &mut f, &cancel).await.unwrap();
        assert!(retirement.load(Ordering::Acquire));
        let ready = match operation.into_connected_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("real connected97 refused"),
        };
        let mut ready = match RetainedNativeIntent::from_connected_recorded(ready) {
            Err(ready) => ready,
            Ok(_) => panic!("native intent accepted without borrowed admission"),
        };
        assert!(Arc::ptr_eq(
            ready.workspace.retirement_entered.as_ref().unwrap(),
            &retirement
        ));
        assert!(ready.workspace.reusable());
        assert!(RetainedControl::storage_reservation() >= AUTHORIZATION_RESERVE);
        assert!(
            RetainedNativeIntent::connected_recorded_storage_reservation() < AUTHORIZATION_RESERVE
        );
        assert!(RetainedNativeApply::connected_storage_reservation() < AUTHORIZATION_RESERVE);
        assert!(RetainedNativeBootstrap::storage_reservation() < AUTHORIZATION_RESERVE);
        RetainedNativeIntent::admit_connected_recorded(
            &mut ready,
            &f.native,
            quiescence(),
            "app-connected97-native96",
            deadline,
            &cancel,
        )
        .unwrap();
        let mut intent = match RetainedNativeIntent::from_connected_recorded(ready) {
            Ok(intent) => intent,
            Err(_) => panic!("real connected97 borrowed admission refused"),
        };
        assert_eq!(intent.operation.deadline(), deadline);
        assert!(Arc::ptr_eq(
            intent
                .workspace
                .as_ref()
                .unwrap()
                .retirement_entered
                .as_ref()
                .unwrap(),
            &retirement
        ));
        retirement.store(false, Ordering::Release);
        intent
            .authorize_and_commit(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(retirement.load(Ordering::Acquire));
        let mut ready = match intent.into_connected_ready() {
            Ok(ready) => ready,
            Err(_) => panic!("real connected79 refused"),
        };
        RetainedNativeApply::admit_connected(&mut ready, None, deadline, &cancel).unwrap();
        let mut apply = match RetainedNativeApply::from_connected(ready) {
            Ok(apply) => apply,
            Err(_) => panic!("real connected80 admission refused"),
        };
        retirement.store(false, Ordering::Release);
        apply
            .authorize_and_apply(
                &f.caller,
                f.source.authorization_reader().await.unwrap(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(
            retirement.load(Ordering::Acquire),
            "same97 workspace did not retire80 current reader"
        );
        let mut ready = match apply.into_connected_pending() {
            Ok(ready) => ready,
            Err(_) => panic!("real connected80 handoff refused"),
        };
        assert_eq!(ready.deadline(), deadline);
        RetainedNativeBootstrap::admit(
            &mut ready,
            "app-connected97-bootstrap",
            catabolic_store::catalog_commands::ReceiptPolicy {
                retention: Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 100,
                max_logical_bytes: 16 * 1024 * 1024,
            },
            deadline,
            &cancel,
        )
        .unwrap();
        let mut bootstrap = match RetainedNativeBootstrap::from_admitted(ready) {
            Ok(bootstrap) => bootstrap,
            Err(_) => panic!("real connected81 admission refused"),
        };
        for effect in 0..3 {
            retirement.store(false, Ordering::Release);
            let reader = f.source.authorization_reader().await.unwrap();
            match effect {
                0 => {
                    bootstrap
                        .authorize_command(&f.caller, reader, &cancel)
                        .await
                }
                1 => {
                    bootstrap
                        .authorize_execute(&f.caller, reader, &cancel)
                        .await
                }
                _ => bootstrap.authorize_ack(&f.caller, reader, &cancel).await,
            }
            .unwrap();
            assert!(
                retirement.load(Ordering::Acquire),
                "same97 workspace did not retire81 current reader"
            );
        }
        let source = match bootstrap.into_source() {
            Ok(source) => source,
            Err(_) => panic!("real connected81 source handoff refused"),
        };
        durable_version_and_locked(&f.motion, 97, false).await;
        f.source.close().await.unwrap();
        drop(f.source);
        drop(source);
        f.app.app.db.close().await;
    }
}
