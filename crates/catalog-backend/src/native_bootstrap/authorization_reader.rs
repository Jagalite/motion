//! Restricted target authorization while the genuine fixed control/96 owner remains
//! exclusively borrowed. This is not the original40 FreshWriter binding proof.
//! Retained variants keep acquisition resources on the actual phase owner.
//! SQLx may internally discard a connection before connect returns an error;
//! retaining its connect box cannot reconstruct that connection or certify its
//! shutdown. Likewise WorkerCrashed is never interpreted as a joined close.
use super::prepare_operation::apply_operation::bootstrap_operation::{
    BootstrapPurpose, NativeBootstrapOperation,
};
use super::prepare_operation::apply_operation::{
    NativeIntentApplyOperation, NativeIntentApplyPhase,
};
use super::prepare_operation::{
    AuthorizationCloseResume, NativeIntentPreparationOperation, NativeIntentPreparationPhase,
};
use super::{ExecutedTransition, PendingTransition, PreparedTransition, retain_writer};
use crate::motion_snapshot::source_reader::{
    RestrictedReaderOwner, open_restricted_reader, verify_reader_main, verify_rusqlite_writer_main,
};
use anyhow::{Context, ensure};
use sqlx::SqliteConnection;
use std::{
    fs::File,
    future::Future,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::atomic::AtomicBool,
};

/// The trusted host uses existing Caller reads only. No writer, native owner or
/// phase extraction is exposed, and the borrow excludes migration, bootstrap and ACK.
/// As with the source reader, replacing raw hooks is outside the supported API.
pub struct TargetAuthorizationReader<'phase> {
    // Retained variants own no SQLx connection, FD, path or connect future here.
    // Legacy variants remain explicitly await-local: their consuming transitions
    // must not accidentally inherit a live readonly transaction.
    legacy: Option<LegacyTargetReader>,
    reader_phase: ReaderPhase,
    owner: TargetOwner<'phase>,
}
struct LegacyTargetReader {
    reader: SqliteConnection,
    file: File,
    path: PathBuf,
    identity: (u64, u64),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReaderPhase {
    Open,
    Closing,
    Joined(AuthorizationCloseResume),
    ControlClosing,
    ControlJoined,
    ApplyClosing,
    ApplyJoined,
    BootstrapClosing,
    BootstrapJoined(BootstrapPurpose),
    CaptureClosing,
    CaptureJoined,
    NativeCaptureClosing,
    NativeCaptureJoined,
    PairedCaptureClosing,
    PairedCaptureJoined,
}
// Closed owner variants, never supplied by the caller or constructed from SQL.
enum TargetOwner<'a> {
    Control(&'a mut crate::offline_control_upgrade::PreparedRecordedControlUpgrade),
    RetainedControl(&'a mut crate::offline_control_upgrade::RecordedControlOperation),
    RetainedPreparation(&'a mut super::prepare_operation::NativeIntentPreparationOperation),
    RetainedApply(&'a mut NativeIntentApplyOperation),
    RetainedBootstrap(&'a mut NativeBootstrapOperation),
    RetainedCapture(&'a mut crate::source_relocations::owned_operation::RetainedCaptureIngress94),
    RetainedNativeCapture(&'a mut crate::source_relocations::owned_operation::RetainedCaptureIngress94),
    RetainedPairedCapture(&'a mut crate::source_relocations::owned_operation::RetainedCaptureIngress94),
    Prepared(&'a mut PreparedTransition),
    Pending(&'a mut PendingTransition),
    Executed(&'a mut ExecutedTransition),
}
impl TargetOwner<'_> {
    fn claim(&mut self) -> anyhow::Result<()> {
        match self {
            Self::RetainedControl(owner) => owner.claim_authorization_reader(),
            Self::RetainedPreparation(owner) => owner.claim_authorization_reader(),
            Self::RetainedApply(owner) => owner.claim_authorization_reader(),
            Self::RetainedBootstrap(owner) => owner.claim_authorization_reader(),
            Self::RetainedCapture(owner) => owner.claim_authorization_reader(),
            Self::RetainedNativeCapture(owner) => owner.claim_native_authorization_reader(),
            Self::RetainedPairedCapture(owner) => owner.claim_paired_authorization_reader(),
            _ => self.verify(),
        }
    }
    fn retained_reader(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        match self {
            Self::RetainedControl(owner) => owner.authorization_reader_slot(),
            Self::RetainedPreparation(owner) => owner.authorization_reader_slot(),
            Self::RetainedApply(owner) => owner.authorization_reader_slot(),
            Self::RetainedBootstrap(owner) => owner.authorization_reader_slot(),
            Self::RetainedCapture(owner) => owner.authorization_reader_slot(),
            Self::RetainedNativeCapture(owner) => owner.native_authorization_reader_slot(),
            Self::RetainedPairedCapture(owner) => owner.paired_authorization_reader_slot(),
            _ => anyhow::bail!("legacy target has no retained reader slot"),
        }
    }
    fn retained_reader_mut(&mut self) -> anyhow::Result<&mut RestrictedReaderOwner> {
        match self {
            Self::RetainedControl(owner) => owner.authorization_reader_slot_mut(),
            Self::RetainedPreparation(owner) => owner.authorization_reader_slot_mut(),
            Self::RetainedApply(owner) => owner.authorization_reader_slot_mut(),
            Self::RetainedBootstrap(owner) => owner.authorization_reader_slot_mut(),
            Self::RetainedCapture(owner) => owner.authorization_reader_slot_mut(),
            Self::RetainedNativeCapture(owner) => owner.native_authorization_reader_slot_mut(),
            Self::RetainedPairedCapture(owner) => owner.paired_authorization_reader_slot_mut(),
            _ => anyhow::bail!("legacy target has no retained reader slot"),
        }
    }
    fn is_retained(&self) -> bool {
        matches!(
            self,
            Self::RetainedControl(_)
                | Self::RetainedPreparation(_)
                | Self::RetainedApply(_)
                | Self::RetainedBootstrap(_)
                | Self::RetainedCapture(_)
                | Self::RetainedNativeCapture(_)
                | Self::RetainedPairedCapture(_)
        )
    }
    fn verify(&self) -> anyhow::Result<()> {
        match self {
            Self::Control(owner) => owner.verify_current(),
            Self::RetainedControl(owner) => owner.verify_authorization_owner(),
            Self::RetainedPreparation(owner) => owner.verify_authorization_owner(),
            Self::RetainedApply(owner) => owner.verify_authorization_owner(),
            Self::RetainedBootstrap(owner) => owner.verify_authorization_owner(),
            Self::RetainedCapture(owner) => owner.verify_authorization_owner(),
            Self::RetainedNativeCapture(owner) => owner.verify_native_authorization_owner(),
            Self::RetainedPairedCapture(owner) => owner.verify_paired_authorization_owner(),
            Self::Prepared(owner) => owner.verify_prepared_writer(),
            Self::Pending(owner) => owner.verify_pending_writer(),
            Self::Executed(owner) => owner.verify_current(),
        }
    }
    fn path(&self) -> anyhow::Result<PathBuf> {
        match self {
            Self::Control(owner) => owner.authorization_path(),
            Self::RetainedControl(owner) => owner.authorization_path(),
            Self::RetainedPreparation(owner) => owner.authorization_path(),
            Self::RetainedApply(owner) => owner.authorization_path(),
            Self::RetainedBootstrap(owner) => owner.authorization_path(),
            Self::RetainedCapture(owner) => owner.authorization_path(),
            Self::RetainedNativeCapture(owner) => owner.native_authorization_path(),
            Self::RetainedPairedCapture(owner) => owner.paired_authorization_path(),
            Self::Prepared(owner) => verify_rusqlite_writer_main(&owner.db),
            Self::Pending(owner) => verify_rusqlite_writer_main(&owner.db),
            Self::Executed(owner) => verify_rusqlite_writer_main(&owner.db),
        }
    }
}
impl PreparedTransition {
    pub(crate) async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<TargetAuthorizationReader<'_>> {
        retain_writer(&self.db)?;
        TargetAuthorizationReader::open(TargetOwner::Prepared(self)).await
    }
}
impl PendingTransition {
    pub(crate) async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<TargetAuthorizationReader<'_>> {
        retain_writer(&self.db)?;
        TargetAuthorizationReader::open(TargetOwner::Pending(self)).await
    }
}
impl ExecutedTransition {
    pub(crate) async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<TargetAuthorizationReader<'_>> {
        TargetAuthorizationReader::open(TargetOwner::Executed(self)).await
    }
}
impl TargetAuthorizationReader<'_> {
    /// Fixed resource-only poll seam for the four existing external close boxes.
    /// The owning operation chooses purpose and phase; this never restores them.
    /// Polling survives unwind as terminal, whereas Pending is resumable. The
    /// actual returned SQLx result is installed before any fallible inspection.
    pub(crate) async fn settle_close(
        connection: &mut Option<SqliteConnection>,
        future: &mut Option<
            std::pin::Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send>>,
        >,
        outcome: &mut Option<Result<(), sqlx::Error>>,
        frame_bytes: &mut usize,
        polling: &mut bool,
        storage: u64,
        #[cfg(test)] poll_fault: &mut Option<u8>,
    ) -> anyhow::Result<()> {
        ensure!(
            !*polling,
            "authorization close panicked; original frame cannot be repolled"
        );
        if outcome.is_none() {
            if future.is_none() {
                let reader = connection
                    .take()
                    .context("authorization close connection missing")?;
                *future = Some(sqlx::Connection::close(reader));
                *frame_bytes = std::mem::size_of_val(future.as_ref().unwrap().as_ref().get_ref());
            }
            ensure!(
                (*frame_bytes as u64)
                    .checked_mul(2)
                    .is_some_and(|n| n <= storage),
                "authorization close exceeds its admitted frame share"
            );
            std::future::poll_fn(|context| {
                *polling = true;
                // Fixed test-only gates retain the genuine SQLx owning box.
                // They do not replace its future or manufacture a close result.
                #[cfg(test)]
                match poll_fault.take() {
                    Some(0) => {
                        *polling = false;
                        context.waker().wake_by_ref();
                        return std::task::Poll::Pending;
                    }
                    Some(_) => panic!("injected authorization close poll panic"),
                    None => {}
                }
                match future.as_mut().unwrap().as_mut().poll(context) {
                    std::task::Poll::Pending => {
                        *polling = false;
                        std::task::Poll::Pending
                    }
                    std::task::Poll::Ready(result) => {
                        *outcome = Some(result);
                        *polling = false;
                        std::task::Poll::Ready(())
                    }
                }
            })
            .await;
        }
        ensure!(
            matches!(outcome.as_ref(), Some(Ok(()))),
            "authorization reader close failed; closure is unverified"
        );
        Ok(())
    }
}

impl<'owner> TargetAuthorizationReader<'owner> {
    pub(crate) fn open_retained_native_capture(
        owner: &'owner mut crate::source_relocations::owned_operation::RetainedCaptureIngress94,
    ) -> impl Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedNativeCapture(owner))
    }

    pub(crate) fn open_retained_paired_capture(
        owner: &'owner mut crate::source_relocations::owned_operation::RetainedCaptureIngress94,
    ) -> impl Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedPairedCapture(owner))
    }

    pub(crate) fn open_retained_capture(
        owner: &'owner mut crate::source_relocations::owned_operation::RetainedCaptureIngress94,
    ) -> impl Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedCapture(owner))
    }

    pub(crate) fn open_retained_control(
        owner: &'owner mut crate::offline_control_upgrade::RecordedControlOperation,
    ) -> impl std::future::Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedControl(owner))
    }
    pub(crate) fn open_retained_bootstrap(
        owner: &'owner mut NativeBootstrapOperation,
    ) -> impl std::future::Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedBootstrap(owner))
    }
    pub(crate) fn open_retained_apply(
        owner: &'owner mut NativeIntentApplyOperation,
    ) -> impl std::future::Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedApply(owner))
    }
    pub(crate) fn open_retained_preparation(
        owner: &'owner mut NativeIntentPreparationOperation,
    ) -> impl std::future::Future<Output = anyhow::Result<Self>> + 'owner {
        Self::open_retained(TargetOwner::RetainedPreparation(owner))
    }
    // Claim is deliberately outside the async body. An unpolled open consumes
    // this exact phase/purpose attempt without acquiring or releasing resources.
    fn open_retained(
        mut owner: TargetOwner<'owner>,
    ) -> impl std::future::Future<Output = anyhow::Result<Self>> + 'owner {
        let started = owner.claim();
        async move {
            started?;
            owner.verify()?;
            let path = owner.path()?;
            owner.retained_reader_mut()?.install_path(path)?;
            if matches!(&owner, TargetOwner::RetainedCapture(_) | TargetOwner::RetainedNativeCapture(_) | TargetOwner::RetainedPairedCapture(_)) {
                owner.retained_reader_mut()?.acquire_uncached().await?;
            } else {
                owner.retained_reader_mut()?.acquire().await?;
            }
            let mut proof = Self {
                legacy: None,
                reader_phase: ReaderPhase::Open,
                owner,
            };
            proof.verify().await?;
            Ok(proof)
        }
    }
    /// Exact fixed control93 owner only. Its writer/deadline/evidence are
    /// retained across this read-only borrow and the subsequent control apply.
    pub(crate) async fn open_control(
        owner: &'owner mut crate::offline_control_upgrade::PreparedRecordedControlUpgrade,
    ) -> anyhow::Result<Self> {
        Self::open(TargetOwner::Control(owner)).await
    }
    async fn open(owner: TargetOwner<'owner>) -> anyhow::Result<Self> {
        ensure!(
            !owner.is_retained(),
            "retained target refuses legacy reader acquisition"
        );
        owner.verify()?;
        let path = owner.path()?;
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "native41 authorization target is not single-link regular file"
        );
        let identity = (metadata.dev(), metadata.ino());
        let reader = open_restricted_reader(&path).await?;
        let mut proof = Self {
            legacy: Some(LegacyTargetReader {
                reader,
                file,
                path,
                identity,
            }),
            reader_phase: ReaderPhase::Open,
            owner,
        };
        proof.verify().await?;
        Ok(proof)
    }
}
impl TargetAuthorizationReader<'_> {
    /// Descriptive owner provenance only, not a current-source authentication
    /// grant or permission to execute any retained continuation.
    pub fn is_retained_operation(&self) -> bool {
        self.owner.is_retained()
    }
    pub async fn authorize_connection(&mut self) -> anyhow::Result<&mut SqliteConnection> {
        self.verify().await?;
        if let Some(legacy) = &mut self.legacy {
            Ok(&mut legacy.reader)
        } else {
            self.owner.retained_reader_mut()?.reader_mut()
        }
    }
    pub async fn verify(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.reader_phase == ReaderPhase::Open,
            "native authorization reader is closing or closed"
        );
        self.owner.verify()?;
        let owner_path = self.owner.path()?;
        if self.owner.is_retained() {
            let reader_path =
                verify_reader_main(self.owner.retained_reader_mut()?.reader_mut()?).await?;
            let resources = self.owner.retained_reader()?;
            ensure!(
                owner_path == resources.path()? && reader_path == resources.path()?,
                "native41 authorization reader differs from retained target"
            );
            resources.verify_identity()?;
        } else {
            let legacy = self
                .legacy
                .as_mut()
                .context("legacy target reader missing")?;
            let reader_path = verify_reader_main(&mut legacy.reader).await?;
            ensure!(
                owner_path == legacy.path && reader_path == legacy.path,
                "native41 authorization reader differs from retained target"
            );
            let held = legacy.file.metadata()?;
            let named = std::fs::symlink_metadata(&legacy.path)?;
            ensure!(
                held.is_file()
                    && named.is_file()
                    && held.nlink() == 1
                    && named.nlink() == 1
                    && (held.dev(), held.ino()) == legacy.identity
                    && (named.dev(), named.ino()) == legacy.identity,
                "native41 authorization target identity changed"
            );
        }
        // Exact phase and native evidence belong to the retained owner. Existing
        // control/96 variants retain their WRITE. Capture94 instead retains the
        // original supported cooperative stage locks through its service owner;
        // its readonly token never impersonates a retained WRITE or auth grant.
        // The application still freshly checks policy and temporal authority.
        Ok(())
    }
    /// The actual SQLx close box is installed synchronously on recorded97,
    /// before this borrowed waiter can be polled or dropped. Legacy Control and
    /// every native96/bootstrap purpose are deliberately refused before taking
    /// the reader connection, so none can manufacture joined-control authority.
    pub fn close_retained_control_reader(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let started = self.begin_retained_control_close();
        async move {
            started?;
            let TargetOwner::RetainedControl(owner) = &mut self.owner else {
                unreachable!("fixed retained control close")
            };
            owner.await_authorization_close().await?;
            self.reader_phase = ReaderPhase::ControlJoined;
            Ok(())
        }
    }
    fn begin_retained_control_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.reader_phase == ReaderPhase::Open,
            "control authorization reader close cannot retry"
        );
        let TargetOwner::RetainedControl(owner) = &mut self.owner else {
            anyhow::bail!("control close requires actual retained97 owner");
        };
        owner.authorization_close_ready()?;
        self.reader_phase = ReaderPhase::ControlClosing;
        owner.begin_authorization_close()
    }
    fn joined_control_owner(
        &mut self,
    ) -> anyhow::Result<&mut crate::offline_control_upgrade::RecordedControlOperation> {
        ensure!(
            self.reader_phase == ReaderPhase::ControlJoined,
            "control continuation requires its own joined reader"
        );
        match &mut self.owner {
            TargetOwner::RetainedControl(owner) => Ok(owner),
            _ => anyhow::bail!("control continuation requires actual retained97 owner"),
        }
    }
    /// Full fixed owner scans occur here, after all async work and before the
    /// app's final pure current/target authorization-time checks.
    pub fn preflight_retained_control(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_control_owner()?.preflight_for_apply(cancel)
    }
    /// First fixed DDL through durable publication, but never COMMIT. Backend
    /// mechanics do not establish current-reader or application authorization.
    pub fn apply_retained_control(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_control_owner()?.apply_pending(cancel)
    }
    /// The caller must freshly check both temporal grants after publication.
    /// This continuation has no SQL/scan before COMMIT, which itself can block.
    pub fn commit_retained_control(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_control_owner()?.commit_pending(cancel)
    }
    /// Separate writer reacquisition and full terminal verification, retaining
    /// the same joined token/operation through both committed failure and Ready97.
    pub fn finish_retained_control(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_control_owner()?.finish_committed(cancel)
    }

    /// Close the explicit read transaction before an effect, but keep this
    /// genuine token's exclusive owner borrow and application snapshot. The
    /// resource FD/path and completed connect box remain on the phase owner.
    /// The actual owning SQLx close future lives on the independently retained
    /// backend operation, never in this await-local token or returned waiter.
    /// Construction is synchronous and terminal even if the waiter is unpolled.
    pub fn close_retained_native_intent_reader(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let started = self.begin_retained_native_intent_close();
        async move {
            let purpose = started?;
            let TargetOwner::RetainedPreparation(owner) = &mut self.owner else {
                unreachable!("fixed retained preparation close")
            };
            owner.await_authorization_close().await?;
            self.reader_phase = ReaderPhase::Joined(purpose);
            Ok(())
        }
    }
    fn begin_retained_native_intent_close(&mut self) -> anyhow::Result<AuthorizationCloseResume> {
        ensure!(
            self.reader_phase == ReaderPhase::Open,
            "native authorization reader close cannot retry"
        );
        let TargetOwner::RetainedPreparation(owner) = &mut self.owner else {
            anyhow::bail!("native intent close requires retained preparation owner");
        };
        ensure!(
            matches!(
                owner.phase(),
                NativeIntentPreparationPhase::Inputs
                    | NativeIntentPreparationPhase::SourceLocked
                    | NativeIntentPreparationPhase::PreparedAwaitingAuthorization
            ),
            "native authorization close phase unavailable"
        );
        self.reader_phase = ReaderPhase::Closing;
        owner.begin_authorization_close()
    }
    fn joined_native_intent_owner(
        &mut self,
        purpose: AuthorizationCloseResume,
    ) -> anyhow::Result<&mut NativeIntentPreparationOperation> {
        ensure!(
            self.reader_phase == ReaderPhase::Joined(purpose),
            "native intent continuation requires matching joined reader"
        );
        match &mut self.owner {
            TargetOwner::RetainedPreparation(owner) => Ok(owner),
            _ => anyhow::bail!("native intent continuation requires retained preparation owner"),
        }
    }
    /// The fixed application token calls this after its last asynchronous work
    /// and before final no-IO temporal checks. No generic SQL/callback is exposed.
    pub fn preflight_retained_native_intent(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let ReaderPhase::Joined(purpose) = self.reader_phase else {
            anyhow::bail!("native intent preflight requires joined reader");
        };
        let owner = self.joined_native_intent_owner(purpose)?;
        let expected = match purpose {
            AuthorizationCloseResume::Inputs => NativeIntentPreparationPhase::Inputs,
            AuthorizationCloseResume::SourceLocked => NativeIntentPreparationPhase::SourceLocked,
            AuthorizationCloseResume::Prepared => {
                NativeIntentPreparationPhase::PreparedAwaitingAuthorization
            }
        };
        ensure!(
            owner.phase() == expected,
            "native intent authorization token cannot be reused"
        );
        owner.preflight_for_next_effect(cancel)
    }
    pub fn lock_retained_native_source(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_intent_owner(AuthorizationCloseResume::Inputs)?
            .acquire_source_locks(cancel)
    }
    pub fn prepare_locked_native_intent(
        &mut self,
        source: &std::fs::File,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.joined_native_intent_owner(AuthorizationCloseResume::SourceLocked)?
            .prepare_locked(source, cancel)
    }
    pub fn prepare_retained_native_intent(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_intent_owner(AuthorizationCloseResume::Inputs)?
            .prepare(cancel)
    }
    pub fn insert_retained_native_intent(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_intent_owner(AuthorizationCloseResume::Prepared)?
            .insert_intent(cancel)
    }
    pub fn commit_retained_native_intent(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_intent_owner(AuthorizationCloseResume::Prepared)?
            .commit_intent(cancel)
    }
    /// New phase, new genuine token. Neither an old intent reader nor its joined
    /// purpose can enter these fixed continuations. The actual close future is
    /// installed on the independently retained apply operation before return.
    pub fn close_retained_native_apply_reader(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let started = self.begin_retained_native_apply_close();
        async move {
            started?;
            let TargetOwner::RetainedApply(owner) = &mut self.owner else {
                unreachable!("fixed retained apply close")
            };
            owner.await_authorization_close().await?;
            self.reader_phase = ReaderPhase::ApplyJoined;
            Ok(())
        }
    }
    fn begin_retained_native_apply_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.reader_phase == ReaderPhase::Open,
            "native apply reader close cannot retry"
        );
        let TargetOwner::RetainedApply(owner) = &mut self.owner else {
            anyhow::bail!("native apply close requires retained apply owner");
        };
        ensure!(
            owner.phase() == NativeIntentApplyPhase::Inputs,
            "native apply close phase unavailable"
        );
        self.reader_phase = ReaderPhase::ApplyClosing;
        owner.begin_authorization_close()
    }
    fn joined_native_apply_owner(&mut self) -> anyhow::Result<&mut NativeIntentApplyOperation> {
        ensure!(
            self.reader_phase == ReaderPhase::ApplyJoined,
            "native apply requires its own joined reader"
        );
        match &mut self.owner {
            TargetOwner::RetainedApply(owner) => Ok(owner),
            _ => anyhow::bail!("native apply requires retained apply owner"),
        }
    }
    pub fn preflight_retained_native_apply(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_apply_owner()?
            .preflight_for_apply(cancel)
    }
    pub fn apply_retained_native(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_apply_owner()?.apply(cancel)
    }
}

impl TargetAuthorizationReader<'_> {
    /// Actual close future is retained on81 before this borrowed waiter exists.
    /// A joined token remains purpose-bound through only that phase's effects.
    pub fn close_retained_native_bootstrap_reader(
        &mut self,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + '_ {
        let started = self.begin_retained_native_bootstrap_close();
        async move {
            let purpose = started?;
            let TargetOwner::RetainedBootstrap(owner) = &mut self.owner else {
                unreachable!("fixed bootstrap close")
            };
            owner.await_authorization_close().await?;
            self.reader_phase = ReaderPhase::BootstrapJoined(purpose);
            Ok(())
        }
    }
    fn begin_retained_native_bootstrap_close(&mut self) -> anyhow::Result<BootstrapPurpose> {
        ensure!(
            self.reader_phase == ReaderPhase::Open,
            "bootstrap reader close cannot retry"
        );
        let TargetOwner::RetainedBootstrap(owner) = &mut self.owner else {
            anyhow::bail!("bootstrap close requires retained81 owner");
        };
        owner.authorization_purpose()?;
        self.reader_phase = ReaderPhase::BootstrapClosing;
        owner.begin_authorization_close()
    }
    fn joined_native_bootstrap_owner(
        &mut self,
        purpose: BootstrapPurpose,
    ) -> anyhow::Result<&mut NativeBootstrapOperation> {
        ensure!(
            self.reader_phase == ReaderPhase::BootstrapJoined(purpose),
            "bootstrap continuation requires matching joined purpose"
        );
        match &mut self.owner {
            TargetOwner::RetainedBootstrap(owner) => Ok(owner),
            _ => anyhow::bail!("bootstrap continuation requires actual retained81 owner"),
        }
    }
    pub fn preflight_retained_native_bootstrap(
        &mut self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let ReaderPhase::BootstrapJoined(purpose) = self.reader_phase else {
            anyhow::bail!("bootstrap preflight requires joined purpose");
        };
        self.joined_native_bootstrap_owner(purpose)?
            .preflight(purpose, cancel)
    }
    pub fn insert_retained_native_bootstrap_command(
        &mut self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.joined_native_bootstrap_owner(BootstrapPurpose::Command)?
            .insert_command(cancel)
    }
    pub fn commit_retained_native_bootstrap_command(
        &mut self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.joined_native_bootstrap_owner(BootstrapPurpose::Command)?
            .commit_command(cancel)
    }
    pub fn execute_retained_native_bootstrap(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.joined_native_bootstrap_owner(BootstrapPurpose::Execute)?
            .execute(cancel)
    }
    pub fn insert_retained_native_bootstrap_ack(
        &mut self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.joined_native_bootstrap_owner(BootstrapPurpose::Ack)?
            .insert_ack(cancel)
    }
    pub fn commit_retained_native_bootstrap_ack(
        &mut self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.joined_native_bootstrap_owner(BootstrapPurpose::Ack)?
            .commit_ack(cancel)
    }
}

#[cfg(test)]
pub(crate) mod custody_tests {
    use super::*;
    use std::{
        future::Future,
        panic::{AssertUnwindSafe, catch_unwind},
        task::Poll,
    };

    impl TargetAuthorizationReader<'_> {
        pub(crate) async fn abandon_close_waiter(
            waiter: impl Future<Output = anyhow::Result<()>>,
            panic: bool,
        ) {
            let mut waiter = Box::pin(waiter);
            let observed = std::future::poll_fn(|cx| {
                Poll::Ready(catch_unwind(AssertUnwindSafe(|| waiter.as_mut().poll(cx))))
            })
            .await;
            if panic {
                assert!(observed.is_err());
            } else {
                assert!(matches!(observed, Ok(Poll::Pending)));
            }
            drop(waiter);
        }

        pub(crate) async fn abandon_opening<'a>(
            opening: impl Future<Output = anyhow::Result<TargetAuthorizationReader<'a>>>,
            entered: std::sync::Arc<AtomicBool>,
            mode: u8,
        ) {
            let mut attempt = Box::pin(opening);
            if mode == 0 {
                tokio::select! {
                    _ = &mut attempt => panic!("gated target setup unexpectedly finished"),
                    _ = async {
                        while !entered.load(std::sync::atomic::Ordering::Acquire) {
                            tokio::task::yield_now().await;
                        }
                    } => (),
                }
            } else {
                let result = std::future::poll_fn(|cx| {
                    match catch_unwind(AssertUnwindSafe(|| attempt.as_mut().poll(cx))) {
                        Ok(Poll::Pending) => Poll::Pending,
                        Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
                        Err(panic) => Poll::Ready(Err(panic)),
                    }
                })
                .await;
                assert!(entered.load(std::sync::atomic::Ordering::Acquire));
                if mode == 1 {
                    assert!(matches!(result, Ok(Err(_))));
                } else {
                    assert!(result.is_err());
                }
            }
            drop(attempt);
        }

        pub(crate) async fn actual_reader_custody(
            slot: &mut RestrictedReaderOwner,
        ) -> (usize, i32, *const ()) {
            slot.verify_identity().unwrap();
            let fd = slot.file_fd();
            let opening = slot.connect_address();
            assert!(slot.has_connection());
            verify_reader_main(slot.reader_mut().unwrap())
                .await
                .unwrap();
            let handle = slot
                .reader_mut()
                .unwrap()
                .lock_handle()
                .await
                .unwrap()
                .as_raw_handle()
                .as_ptr() as usize;
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
            (handle, fd, opening)
        }

        // A real borrowed token and actual policy-shaped SELECT, never a Caller,
        // authentication grant, fabricated connection, or stand-in close receipt.
        pub(crate) async fn abandon_prepare<'a>(
            opening: impl Future<Output = anyhow::Result<TargetAuthorizationReader<'a>>>,
            after_read: bool,
            mode: u8,
        ) {
            let entered = std::sync::Arc::new(AtomicBool::new(false));
            let observed = entered.clone();
            let mut attempt = Box::pin(async move {
                let mut reader = opening.await?;
                if after_read {
                    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
                        .fetch_one(reader.authorize_connection().await?)
                        .await?;
                    ensure!(count >= 0, "actual authorization read failed");
                }
                observed.store(true, std::sync::atomic::Ordering::Release);
                match mode {
                    0 => std::future::pending::<anyhow::Result<()>>().await,
                    1 => anyhow::bail!("injected target prepare error"),
                    _ => panic!("injected target prepare panic"),
                }
            });
            if mode == 0 {
                tokio::select! {
                    result = &mut attempt => panic!("target preparation unexpectedly finished: {result:?}"),
                    _ = async {
                        while !entered.load(std::sync::atomic::Ordering::Acquire) {
                            tokio::task::yield_now().await;
                        }
                    } => (),
                }
                // Dropping only this borrowed prepare future must not drop the
                // exact actual reader already held by the independent phase owner.
                drop(attempt);
            } else {
                let result = std::future::poll_fn(|cx| {
                    match catch_unwind(AssertUnwindSafe(|| attempt.as_mut().poll(cx))) {
                        Ok(Poll::Pending) => Poll::Pending,
                        Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
                        Err(panic) => Poll::Ready(Err(panic)),
                    }
                })
                .await;
                assert!(entered.load(std::sync::atomic::Ordering::Acquire));
                if mode == 1 {
                    assert!(
                        result
                            .unwrap()
                            .unwrap_err()
                            .to_string()
                            .contains("injected target prepare error")
                    );
                } else {
                    assert!(result.is_err());
                }
                drop(attempt);
            }
        }
    }
}

// Fixed capture-input continuations. No target token can choose arbitrary SQL,
// extract a writer, supply a callback, or invoke native capture through these.
impl TargetAuthorizationReader<'_> {
    pub async fn capture_authorization_allocations(&mut self) -> anyhow::Result<u64> {
        ensure!(matches!(&self.owner, TargetOwner::RetainedCapture(_) | TargetOwner::RetainedNativeCapture(_) | TargetOwner::RetainedPairedCapture(_)), "capture accounting requires exact retained94 token");
        self.verify().await?;
        let reader = self.owner.retained_reader_mut()?.reader_mut()?;
        let mut lease = reader.lock_handle().await?;
        // Nonowning wrapper is lexical under the exclusive SQLx lease.
        let borrowed = unsafe { rusqlite::Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
        let bytes = crate::motion_snapshot::connection_allocations(&[&borrowed])?;
        ensure!(bytes <= 16 * 1024 * 1024, "capture authorization reader exceeded original bounded cohort");
        Ok(bytes)
    }
    pub fn preflight_capture_acquire(&mut self) -> anyhow::Result<()> {
        ensure!(self.reader_phase == ReaderPhase::Open, "capture acquisition needs its open reader");
        let TargetOwner::RetainedCapture(owner) = &mut self.owner else {
            anyhow::bail!("capture acquisition needs exact retained94 owner");
        };
        owner.preflight_authorized_acquire()
    }
    pub fn poll_capture_acquire(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<anyhow::Result<()>> {
        if self.reader_phase != ReaderPhase::Open {
            return std::task::Poll::Ready(Err(anyhow::anyhow!("capture acquire reader closed")));
        }
        match &mut self.owner {
            TargetOwner::RetainedCapture(owner) => owner.poll_authorized_acquire(cx),
            _ => std::task::Poll::Ready(Err(anyhow::anyhow!("capture acquire requires retained94 owner"))),
        }
    }
    pub fn prepare_capture_begin(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let work = match &mut self.owner {
            TargetOwner::RetainedCapture(owner) if self.reader_phase == ReaderPhase::Open => Some(owner.prepare_authorized_begin()),
            _ => None,
        };
        async move {
            match work { Some(work) => work.await, None => anyhow::bail!("capture BEGIN preflight needs its live target reader") }
        }
    }
    pub fn close_capture_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let started = (|| {
            ensure!(self.reader_phase == ReaderPhase::Open, "capture reader close cannot retry");
            let TargetOwner::RetainedCapture(owner) = &mut self.owner else {
                anyhow::bail!("capture close requires retained94 owner");
            };
            self.reader_phase = ReaderPhase::CaptureClosing;
            owner.begin_authorization_close()
        })();
        async move {
            started?;
            let TargetOwner::RetainedCapture(owner) = &mut self.owner else { unreachable!() };
            owner.await_authorization_close().await?;
            self.reader_phase = ReaderPhase::CaptureJoined;
            Ok(())
        }
    }
    pub fn poll_capture_begin(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<anyhow::Result<()>> {
        if self.reader_phase != ReaderPhase::CaptureJoined {
            return std::task::Poll::Ready(Err(anyhow::anyhow!("capture BEGIN requires this target reader's exact ACK")));
        }
        match &mut self.owner {
            TargetOwner::RetainedCapture(owner) => owner.poll_authorized_begin(cx),
            _ => std::task::Poll::Ready(Err(anyhow::anyhow!("capture BEGIN requires retained94 owner"))),
        }
    }
    pub fn finish_capture_input(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let work = match &mut self.owner {
            TargetOwner::RetainedCapture(owner) if self.reader_phase == ReaderPhase::CaptureJoined => Some(owner.finish_authorized_input()),
            _ => None,
        };
        async move {
            match work { Some(work) => work.await, None => anyhow::bail!("capture finish requires its joined target token") }
        }
    }
}

impl TargetAuthorizationReader<'_> {
    /// Separate fixed-purpose reader and ACK. The acquisition reader's old
    /// CaptureJoined proof cannot enter this branch or authorize Native work.
    pub fn close_native_capture_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let started = (|| {
            ensure!(self.reader_phase == ReaderPhase::Open, "native capture reader close cannot retry");
            let TargetOwner::RetainedNativeCapture(owner) = &mut self.owner else {
                anyhow::bail!("native capture close requires its distinct purpose owner");
            };
            self.reader_phase = ReaderPhase::NativeCaptureClosing;
            owner.begin_native_authorization_close()
        })();
        async move {
            started?;
            let TargetOwner::RetainedNativeCapture(owner) = &mut self.owner else { unreachable!() };
            owner.await_native_authorization_close().await?;
            self.reader_phase = ReaderPhase::NativeCaptureJoined;
            Ok(())
        }
    }
    pub fn capture_native(&mut self) -> anyhow::Result<()> {
        ensure!(self.reader_phase == ReaderPhase::NativeCaptureJoined,
            "native capture requires this target purpose's exact close ACK");
        let TargetOwner::RetainedNativeCapture(owner) = &mut self.owner else {
            anyhow::bail!("native capture requires its distinct purpose owner");
        };
        owner.capture_native()
    }
}

impl TargetAuthorizationReader<'_> {
    pub fn close_paired_capture_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let started = (|| {
            ensure!(self.reader_phase == ReaderPhase::Open, "paired capture reader close cannot retry");
            let TargetOwner::RetainedPairedCapture(owner) = &mut self.owner else {
                anyhow::bail!("paired capture close requires its distinct purpose owner");
            };
            self.reader_phase = ReaderPhase::PairedCaptureClosing;
            owner.begin_paired_authorization_close()
        })();
        async move {
            started?;
            let TargetOwner::RetainedPairedCapture(owner) = &mut self.owner else { unreachable!() };
            owner.await_paired_authorization_close().await?;
            self.reader_phase = ReaderPhase::PairedCaptureJoined;
            Ok(())
        }
    }
    pub fn complete_paired_capture(&mut self) -> anyhow::Result<()> {
        ensure!(self.reader_phase == ReaderPhase::PairedCaptureJoined,
            "paired capture requires exact target purpose close ACK");
        let TargetOwner::RetainedPairedCapture(owner) = &mut self.owner else {
            anyhow::bail!("paired capture requires distinct retained purpose");
        };
        owner.complete_paired_capture()
    }
}
