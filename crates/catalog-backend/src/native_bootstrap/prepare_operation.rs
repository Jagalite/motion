//! Retained initial Ready97 -> durable intent96 producer. This is deliberately
//! synchronous effects with an independently retained authorization-reader close
//! future. No abandoned-JoinHandle or service activation claim.
//! Backend methods are operator mechanics, not application authority. A fixed
//! application coordinator must freshly authenticate before native acquisition
//! (which can recover a hot journal), and again before intent writes.
//! This producer ends at ReadyIntent96; a separate retained apply child can
//! continue it. Completion and paired95/service activation remain absent.
use super::*;
use crate::motion_snapshot::source_reader::RestrictedReaderOwner;
use crate::offline_control_upgrade::RecordedAdmittedControl;
use catabolic_store::migration::offline_bootstrap::{
    OfflinePreparationFailure, OfflinePreparationOperation,
};
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
#[path = "apply_operation.rs"]
pub(crate) mod apply_operation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIntentPreparationPhase {
    Inputs,
    Failed,
    PreparedInputs,
    SourceLocked,
    LockedInputs,
    PreparedAwaitingAuthorization,
    IntentPreflighted,
    PendingCommit,
    ReadyIntent96,
}
/// Private restoration ticket, never an application grant. A joined first-phase
/// reader cannot be reused for the second authorization phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthorizationCloseResume {
    Inputs,
    SourceLocked,
    Prepared,
}
type AuthorizationCloseFuture =
    Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send + 'static>>;
// A mandatory, separately named share of the EXISTING 256MiB aggregate, even
// with additional_host_bytes=0. It conservatively admits the opaque SQLx boxed
// close frame, its simultaneous construction/move frame and close setup. It is
// not optional app-auth slack, reused generic framing or a second budget. The
// inline stored outcome and public Result slots are counted separately below.
// SQLx exposes its concrete frame only after allocating: the post-construction
// measurement can refuse while retaining that actual box, not prove a hard peak
// or measure allocator/worker-channel internals or the existing reader's heap.
const AUTHORIZATION_CLOSE_STORAGE_BYTES: u64 = 64 * 1024;
/// This is on the independently retained operation, NOT the await-local reader
/// or authentication token. SQLx's actual future owns its connection before its
/// first poll and its shutdown receiver while pending. SQLx may internally drop
/// these on WorkerCrashed; retaining its opaque result does not reconstruct them
/// or prove joined closure. The original Motion/native owners are separate.
struct AuthorizationClose {
    connection: Option<sqlx::SqliteConnection>,
    future: Option<AuthorizationCloseFuture>,
    outcome: Option<Result<(), sqlx::Error>>,
    frame_bytes: usize,
    resume: AuthorizationCloseResume,
    acknowledged: bool,
    resource_acknowledged: bool,
    polling: bool,
    #[cfg(test)]
    poll_fault: Option<u8>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeIntentCommitState {
    #[default]
    NotAttempted,
    CommitAttempted,
    DurableIntent96,
    WriterReacquired,
}
/// All variants own actual native custody. There is no error conversion that
/// drops a partially acquired native owner or a successful Prepared result.
enum NativeOwner {
    Empty,
    Preparing(OfflinePreparationOperation),
    Prepared(PreparedOfflineBootstrap),
    Failed(OfflinePreparationFailure),
    Applying(catabolic_store::migration::offline_bootstrap::OfflineApplyOperation),
    Applied(OfflineBootstrap<'static>),
    ApplyFailed(catabolic_store::migration::offline_bootstrap::OfflineApplyFailure),
    Completing(catabolic_store::migration::offline_bootstrap::OfflineCompletionOperation<'static>),
    Completed(catabolic_store::migration::offline_bootstrap::CompletedCatalogSession),
    CompletionFailed(
        catabolic_store::migration::offline_bootstrap::OfflineCompletionFailure<'static>,
    ),
}
/// Opaque pre-admission is stored on Ready97 before a consuming move. The native
/// admission itself acquires no files/connections; its actual operation is stored
/// here before prepare is ever called. The original97 shares remain charged.
pub(crate) struct NativeIntentAdmission {
    allocation: accounting::Allocation,
    retained97: u64,
    authorization_path_bytes: u64,
    additional_host_bytes: u64,
    deadline: Instant,
    id: String,
    native: NativeOwner,
    identity: Option<(Identity, Option<String>, String)>,
    intent: Option<Intent>,
    raw: Option<String>,
    commit: NativeIntentCommitState,
    #[cfg(test)]
    fault: Option<(u8, u8)>,
}
pub struct NativeIntentPreparationOperation {
    control: RecordedAdmittedControl,
    admitted: NativeIntentAdmission,
    phase: NativeIntentPreparationPhase,
    // One immutable resource attempt per Inputs/SourceLocked/Prepared purpose. These slots
    // outlive borrowed opens/tokens and remain inline through later handoffs.
    authorization_readers: [RestrictedReaderOwner; 3],
    authorization_close: Option<AuthorizationClose>,
    terminal_authorization_cleanup: bool,
}
/// A genuine Prepared native owner and the original Ready97 writer/reference/
/// witness/record FDs. No extraction, legacy apply, or completion is exposed.
pub struct ReadyNativeIntent96 {
    operation: NativeIntentPreparationOperation,
    apply_admission: Option<apply_operation::NativeApplyAdmission>,
}

fn check(deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "native intent preparation cancelled"
    );
    ensure!(
        Instant::now() < deadline,
        "native intent inherited deadline expired"
    );
    Ok(())
}
impl NativeIntentPreparationOperation {
    /// Includes inline slots, both move destinations and the public Result. The
    /// native operation also pre-admits its internal finish Result independently.
    pub const fn inline_storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<ReadyNativeIntent96>()
            + std::mem::size_of::<Result<ReadyNativeIntent96, Self>>()
            + std::mem::size_of::<Result<Self, RecordedAdmittedControl>>()
            + std::mem::size_of::<Result<NativeIntentAdmission, anyhow::Error>>()
            + std::mem::size_of::<Result<(), anyhow::Error>>()
            + std::mem::size_of::<Result<(), sqlx::Error>>()
            + std::mem::size_of::<Result<AuthorizationCloseResume, anyhow::Error>>()
            + std::mem::size_of::<AuthorizationCloseFuture>()
            + std::mem::size_of::<TargetAuthorizationReader<'static>>()
            + std::mem::size_of::<Result<TargetAuthorizationReader<'static>, anyhow::Error>>())
            as u64
    }
    pub(crate) fn from_admitted(
        control: RecordedAdmittedControl,
        admitted: NativeIntentAdmission,
    ) -> Self {
        Self {
            control,
            admitted,
            phase: NativeIntentPreparationPhase::Inputs,
            authorization_readers: std::array::from_fn(|_| RestrictedReaderOwner::default()),
            authorization_close: None,
            terminal_authorization_cleanup: false,
        }
    }
    pub fn phase(&self) -> NativeIntentPreparationPhase {
        self.phase
    }
    pub fn commit_state(&self) -> NativeIntentCommitState {
        self.admitted.commit
    }
    pub fn deadline(&self) -> Instant {
        self.admitted.deadline
    }
    /// Last full fixed scan after all asynchronous authorization/reader-close
    /// work, but before the application's final no-IO temporal checks. This is
    /// operator mechanics, not a factory for an application authorization grant.
    pub fn preflight_for_next_effect(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let previous = self.phase;
        ensure!(
            matches!(
                previous,
                NativeIntentPreparationPhase::Inputs
                    | NativeIntentPreparationPhase::SourceLocked
                    | NativeIntentPreparationPhase::PreparedAwaitingAuthorization
            ),
            "native intent preflight cannot retry"
        );
        let purpose = match previous {
            NativeIntentPreparationPhase::Inputs => AuthorizationCloseResume::Inputs,
            NativeIntentPreparationPhase::SourceLocked => AuthorizationCloseResume::SourceLocked,
            _ => AuthorizationCloseResume::Prepared,
        };
        ensure!(
            !self.authorization_reader_slot()?.is_claimed()
                || self
                    .authorization_close
                    .as_ref()
                    .is_some_and(|close| close.acknowledged && close.resume == purpose),
            "native intent claimed reader must actually join before effects"
        );
        self.phase = NativeIntentPreparationPhase::Failed;
        if matches!(
            previous,
            NativeIntentPreparationPhase::Inputs | NativeIntentPreparationPhase::SourceLocked
        ) {
            self.control
                .preflight_native_preparation(&mut self.admitted, cancel)?;
            self.phase = if previous == NativeIntentPreparationPhase::SourceLocked {
                NativeIntentPreparationPhase::LockedInputs
            } else {
                NativeIntentPreparationPhase::PreparedInputs
            };
        } else {
            self.control
                .preflight_native_intent(&mut self.admitted, cancel)?;
            self.phase = NativeIntentPreparationPhase::IntentPreflighted;
        }
        Ok(())
    }
    /// Terminal before work. Full scans belong to preflight; only narrow
    /// DBSTATUS admission and native Inputs refresh precede native acquisition.
    pub fn prepare(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentPreparationPhase::PreparedInputs,
            "native intent preparation cannot retry"
        );
        self.phase = NativeIntentPreparationPhase::Failed;
        self.control
            .prepare_native_intent(&mut self.admitted, cancel)?;
        self.phase = NativeIntentPreparationPhase::PreparedAwaitingAuthorization;
        Ok(())
    }
    /// Fixed service corridor: real native leases precede external stage hashing.
    /// This is mechanics only; the app retains its genuine joined token/fence.
    pub fn acquire_source_locks(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentPreparationPhase::PreparedInputs,
            "native locks require first preflight"
        );
        self.phase = NativeIntentPreparationPhase::Failed;
        self.control
            .acquire_native_source_locks(&mut self.admitted, cancel)?;
        self.phase = NativeIntentPreparationPhase::SourceLocked;
        Ok(())
    }
    /// No path-only fallback: continue the same locked owner with the real stage FD.
    pub fn prepare_locked(
        &mut self,
        source: &std::fs::File,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentPreparationPhase::LockedInputs,
            "locked preparation requires fresh preflight"
        );
        self.phase = NativeIntentPreparationPhase::Failed;
        self.control
            .prepare_locked_native_intent(&mut self.admitted, source, cancel)?;
        self.phase = NativeIntentPreparationPhase::PreparedAwaitingAuthorization;
        Ok(())
    }
    pub(super) fn begin_authorization_close(&mut self) -> anyhow::Result<AuthorizationCloseResume> {
        let resume = match self.phase {
            NativeIntentPreparationPhase::Inputs => AuthorizationCloseResume::Inputs,
            NativeIntentPreparationPhase::SourceLocked => AuthorizationCloseResume::SourceLocked,
            NativeIntentPreparationPhase::PreparedAwaitingAuthorization => {
                AuthorizationCloseResume::Prepared
            }
            _ => anyhow::bail!("native authorization close cannot retry"),
        };
        self.phase = NativeIntentPreparationPhase::Failed;
        // A prior slot is replaceable only after its verified successful join.
        // No pending future or failed outcome can be discarded to retry.
        ensure!(
            self.authorization_close
                .as_ref()
                .is_none_or(|close| close.acknowledged),
            "native authorization close custody is still pending"
        );
        let connection = self.authorization_readers[match resume {
            AuthorizationCloseResume::Inputs => 0,
            AuthorizationCloseResume::SourceLocked => 2,
            AuthorizationCloseResume::Prepared => 1,
        }]
        .take_for_retained_close()?;
        self.authorization_close = Some(AuthorizationClose {
            connection: Some(connection),
            future: None,
            outcome: None,
            frame_bytes: 0,
            resume,
            acknowledged: false,
            resource_acknowledged: false,
            polling: false,
            #[cfg(test)]
            poll_fault: None,
        });
        check(self.deadline(), &AtomicBool::new(false))?;
        let close = self.authorization_close.as_mut().unwrap();
        // This mandatory named share was admitted on Ready97 before any move.
        // Install the actual returned box before measurement or fallible work;
        // even an oversize provisional constructor retains its owning future.
        let connection = close.connection.take().unwrap();
        close.future = Some(sqlx::Connection::close(connection));
        close.frame_bytes =
            std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref());
        let simultaneous_frames = (close.frame_bytes as u64)
            .checked_mul(2)
            .context("native authorization close frame overflow")?;
        ensure!(
            simultaneous_frames <= AUTHORIZATION_CLOSE_STORAGE_BYTES,
            "native authorization close frame exceeds admitted share"
        );
        Ok(resume)
    }
    pub(super) async fn await_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            !self.terminal_authorization_cleanup,
            "terminal reader cleanup grants no authorization continuation"
        );
        ensure!(
            self.phase == NativeIntentPreparationPhase::Failed,
            "native authorization close phase differs"
        );
        let close = self
            .authorization_close
            .as_mut()
            .context("native authorization close missing")?;
        ensure!(
            close.outcome.is_none(),
            "native authorization close cannot retry"
        );
        TargetAuthorizationReader::settle_close(
            &mut close.connection,
            &mut close.future,
            &mut close.outcome,
            &mut close.frame_bytes,
            &mut close.polling,
            AUTHORIZATION_CLOSE_STORAGE_BYTES,
            #[cfg(test)]
            &mut close.poll_fault,
        )
        .await?;
        self.authorization_readers[match close.resume {
            AuthorizationCloseResume::Inputs => 0,
            AuthorizationCloseResume::SourceLocked => 2,
            AuthorizationCloseResume::Prepared => 1,
        }]
        .mark_transferred_close_joined()?;
        close.resource_acknowledged = true;
        let resume = close.resume;
        check(self.deadline(), &AtomicBool::new(false))?;
        self.authorization_close.as_mut().unwrap().acknowledged = true;
        self.phase = match resume {
            AuthorizationCloseResume::Inputs => NativeIntentPreparationPhase::Inputs,
            AuthorizationCloseResume::SourceLocked => NativeIntentPreparationPhase::SourceLocked,
            AuthorizationCloseResume::Prepared => {
                NativeIntentPreparationPhase::PreparedAwaitingAuthorization
            }
        };
        Ok(())
    }
    /// Terminal resource cleanup only, including abandoned open/prepare/close
    /// borrowers. Failed is installed even for an unpolled waiter; durable/native
    /// facts are unchanged. Repetition observes only the same retained attempt,
    /// never a new connection, setup, authentication token or effect permission.
    /// Success includes known-never-opened attempts; it is not an effect grant.
    pub fn settle_authorization_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.terminal_authorization_cleanup = true;
        self.phase = NativeIntentPreparationPhase::Failed;
        async move {
            if let Some(close) = self.authorization_close.as_mut() {
                if !close.resource_acknowledged {
                    TargetAuthorizationReader::settle_close(
                        &mut close.connection,
                        &mut close.future,
                        &mut close.outcome,
                        &mut close.frame_bytes,
                        &mut close.polling,
                        AUTHORIZATION_CLOSE_STORAGE_BYTES,
                        #[cfg(test)]
                        &mut close.poll_fault,
                    )
                    .await?;
                    self.authorization_readers[match close.resume {
                        AuthorizationCloseResume::Inputs => 0,
                        AuthorizationCloseResume::SourceLocked => 2,
                        AuthorizationCloseResume::Prepared => 1,
                    }]
                    .mark_transferred_close_joined()?;
                    close.resource_acknowledged = true;
                }
            }
            ensure!(
                self.authorization_readers
                    .iter()
                    .filter(|reader| reader.is_claimed() && !reader.is_settled())
                    .count()
                    <= 1,
                "multiple unresolved authorization purposes cannot share a close reservation"
            );
            if self
                .authorization_readers
                .iter()
                .any(|reader| reader.is_claimed() && !reader.is_settled())
            {
                // Only an exact observed ACK permits retiring the previous
                // purpose's box. Its admitted close share is then available to
                // this slot; earlier phase owners retain their separate boxes.
                ensure!(
                    self.authorization_close
                        .as_ref()
                        .is_none_or(|close| close.resource_acknowledged),
                    "unresolved external authorization close remains retained"
                );
                self.authorization_close = None;
            }
            for reader in &mut self.authorization_readers {
                if reader.is_claimed() && !reader.is_settled() {
                    reader.settle_inline().await?;
                }
            }
            Ok(())
        }
    }

    /// Opens a restricted same-file reader while exclusively borrowing this
    /// exact writer. Opening it is not authentication or permission to mutate.
    pub fn authorization_reader(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<TargetAuthorizationReader<'_>>> + '_ {
        TargetAuthorizationReader::open_retained_preparation(self)
    }
    fn authorization_reader_index(&self) -> anyhow::Result<usize> {
        match self.phase {
            NativeIntentPreparationPhase::Inputs => Ok(0),
            NativeIntentPreparationPhase::SourceLocked => Ok(2),
            NativeIntentPreparationPhase::PreparedAwaitingAuthorization => Ok(1),
            _ => anyhow::bail!("native intent authorization phase unavailable"),
        }
    }
    pub(crate) fn claim_authorization_reader(&mut self) -> anyhow::Result<()> {
        let index = self.authorization_reader_index()?;
        self.authorization_readers[index].claim()?;
        self.verify_authorization_owner()
    }
    pub(crate) fn authorization_reader_slot(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        Ok(&self.authorization_readers[self.authorization_reader_index()?])
    }
    pub(crate) fn authorization_reader_slot_mut(
        &mut self,
    ) -> anyhow::Result<&mut RestrictedReaderOwner> {
        let index = self.authorization_reader_index()?;
        Ok(&mut self.authorization_readers[index])
    }
    pub(crate) fn verify_authorization_owner(&self) -> anyhow::Result<()> {
        ensure!(
            matches!(
                self.phase,
                NativeIntentPreparationPhase::Inputs
                    | NativeIntentPreparationPhase::SourceLocked
                    | NativeIntentPreparationPhase::PreparedAwaitingAuthorization
            ),
            "native intent authorization phase unavailable"
        );
        ensure!(
            self.authorization_reader_slot()?.is_claimed(),
            "native intent authorization reader was not claimed"
        );
        self.control.verify_native_intent(&self.admitted)
    }
    pub(crate) fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        self.verify_authorization_owner()?;
        self.control.native_intent_authorization_path()
    }
    /// INSERT and its full post-write accounting stop at uncommitted custody.
    /// The application must freshly check both retained tokens' temporal facts
    /// again after this call, before the separate COMMIT continuation.
    pub fn insert_intent(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentPreparationPhase::IntentPreflighted,
            "native intent insertion cannot retry"
        );
        self.phase = NativeIntentPreparationPhase::Failed;
        self.control
            .insert_native_intent(&mut self.admitted, cancel)?;
        self.phase = NativeIntentPreparationPhase::PendingCommit;
        Ok(())
    }
    /// No full scan lies between final temporal checks and COMMIT/re-BEGIN.
    /// This fixed continuation is operator mechanics, not an app-user grant.
    pub fn commit_intent(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentPreparationPhase::PendingCommit,
            "native intent commit cannot retry"
        );
        self.phase = NativeIntentPreparationPhase::Failed;
        self.control
            .commit_native_intent(&mut self.admitted, cancel)?;
        self.phase = NativeIntentPreparationPhase::ReadyIntent96;
        Ok(())
    }
    pub fn into_ready(self) -> Result<ReadyNativeIntent96, Self> {
        if self.phase == NativeIntentPreparationPhase::ReadyIntent96 {
            Ok(ReadyNativeIntent96 {
                operation: self,
                apply_admission: None,
            })
        } else {
            Err(self)
        }
    }
    /// Explicit Motion transaction settlement, not native release, rollback
    /// proof, retry permission, or a claim that an attempted COMMIT failed.
    pub fn settle_motion(&mut self) -> anyhow::Result<()> {
        self.phase = NativeIntentPreparationPhase::Failed;
        self.control.settle()
    }
}
impl ReadyNativeIntent96 {
    /// Terminal cleanup retains native custody and settles only this Motion writer.
    pub fn settle_motion(&mut self) -> anyhow::Result<()> {
        self.operation.settle_motion()
    }

    pub fn commit_state(&self) -> NativeIntentCommitState {
        self.operation.commit_state()
    }
    pub fn deadline(&self) -> Instant {
        self.operation.deadline()
    }
}
impl NativeIntentAdmission {
    fn external(
        retained97: u64,
        authorization_path_bytes: u64,
        id: &str,
        path: &Path,
    ) -> anyhow::Result<u64> {
        // Explicit linkage/path copies and mandatory opaque-close share, in
        // addition to original97's proved shares. The close share is charged on
        // every admission and refresh, never borrowed from optional app storage.
        retained97
            .checked_add(NativeIntentPreparationOperation::inline_storage_reservation())
            .and_then(|n| n.checked_add(AUTHORIZATION_CLOSE_STORAGE_BYTES))
            .and_then(|n| n.checked_add(3 * RestrictedReaderOwner::opening_frame_reservation()))
            .and_then(|n| n.checked_add(authorization_path_bytes.checked_add(1)?.checked_mul(12)?))
            .and_then(|n| n.checked_add((id.len() as u64).checked_mul(4)?))
            .and_then(|n| n.checked_add((path.as_os_str().len() as u64).checked_mul(4)?))
            .context("native intent owner storage overflow")
    }
    pub(crate) fn admit(
        db: &Connection,
        retained97_without_workspace: u64,
        workspace: &mut catabolic_store::migration::offline_bootstrap::OfflineControlWorkspaceBacking,
        deadline: Instant,
        path: &Path,
        q: NativeUpgradeQuiescence,
        id: &str,
        additional_host_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Self> {
        check(deadline, cancel)?;
        ensure!(identifier(id), "invalid native intent transition ID");
        // Existing97 path scratch covers this observation and the simultaneous
        // writer/main-reader verification pair. Add four independent (path+1)
        // terms per purpose for slot path, owned options filename, SQLx's
        // worker CString, and transient String/CString construction overlap.
        // These are admitted before handoff/acquisition. The
        // immutable/vfs options are unset, so SQLx does not URI-expand this name.
        let authorization_path_bytes =
            crate::motion_snapshot::source_reader::verify_rusqlite_writer_main(db)?
                .as_os_str()
                .len() as u64;
        // Compose the original named component once, prospectively. H/K remain
        // complete; no generic caller H is reinterpreted as workspace authority.
        let retained97 = retained97_without_workspace
            .checked_add(catabolic_store::migration::offline_bootstrap::OfflineControlWorkspaceBacking::BYTES)
            .context("Ready97 workspace retention overflow")?;
        let external_without_workspace = Self::external(
            retained97_without_workspace, authorization_path_bytes, id, path,
        )?;
        let (allocation, reservation) = accounting::Allocation::with_control_workspace(
            db, external_without_workspace,
        )?;
        check(deadline, cancel)?;
        let native = OfflinePreparationOperation::admit_with_control_workspace(
            path, q, reservation, workspace, deadline,
        )?;
        // This construction is before any native acquisition. No preexisting
        // owner was consumed if admission or linkage allocation failed.
        Ok(Self {
            allocation,
            retained97,
            authorization_path_bytes,
            additional_host_bytes,
            deadline,
            id: id.to_owned(),
            native: NativeOwner::Preparing(native),
            identity: None,
            intent: None,
            raw: None,
            commit: NativeIntentCommitState::NotAttempted,
            #[cfg(test)]
            fault: None,
        })
    }
    pub(crate) fn covers_host_storage(&self, required: u64) -> bool {
        self.additional_host_bytes >= required
    }
    pub(crate) fn check_before_work(&self, cancel: &AtomicBool) -> anyhow::Result<()> {
        check(self.deadline, cancel)
    }
    fn checkpoint(&self, point: u8, cancel: &AtomicBool) -> anyhow::Result<()> {
        #[cfg(test)]
        if let Some((wanted, mode)) = self.fault {
            if wanted == point {
                match mode {
                    1 => panic!("retained native intent checkpoint {point}"),
                    2 => cancel.store(true, Ordering::Release),
                    _ => anyhow::bail!("retained native intent checkpoint {point}"),
                }
            }
        }
        let _ = point;
        check(self.deadline, cancel)
    }
    pub(crate) fn verify_in(&self, db: &Connection) -> anyhow::Result<()> {
        check(self.deadline, &AtomicBool::new(false))?;
        ensure!(
            !db.is_autocommit()
                && db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write,
            "native intent authorization lost writer"
        );
        let plan = self.intent.as_ref().map(|intent| &intent.plan);
        self.allocation.check(db, plan, None)?;
        journal::verify(db)?;
        crate::source_freeze::verify_sync(db)?;
        if let NativeOwner::Prepared(native) = &self.native {
            let intent = self
                .intent
                .as_ref()
                .context("prepared native intent missing")?;
            ensure!(
                native.approved_plan() == &intent.plan
                    && native.catalog_id() == intent.from.catalog_id
                    && native.incarnation() == intent.from.incarnation
                    && native.file_identity() == intent.native_file_identity
                    && native.source_generation() == intent.source_generation,
                "prepared native identity changed"
            );
        } else {
            ensure!(
                matches!(&self.native, NativeOwner::Preparing(_)),
                "native intent owner unavailable"
            );
        }
        check(self.deadline, &AtomicBool::new(false))
    }
    /// Fixed borrowed seam only, invoked by RecordedAdmittedControl with its
    /// actual writer. It never returns or replaces that connection.
    pub(crate) fn preflight_prepare_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.checkpoint(1, cancel)?;
        ensure!(
            !db.is_autocommit()
                && db.transaction_state(Some("main"))? == rusqlite::TransactionState::Write,
            "native intent requires retained Ready97 writer"
        );
        db.progress_handler(
            10000,
            Some({
                let deadline = self.deadline;
                move || Instant::now() >= deadline
            }),
        )?;
        self.allocation.reservation(db, None, None)?;
        journal::verify(db)?;
        crate::source_freeze::verify_sync(db)?;
        let activated: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM catabolic_cutover WHERE phase='activated') OR EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE phase='activated')", [], |r| r.get(0))?;
        ensure!(
            activated,
            "native intent requires completed Motion activation"
        );
        self.identity = Some(initial_identity(db)?);
        ensure!(
            self.identity.as_ref().unwrap().0.schema_version == 40,
            "native intent already advanced"
        );
        check(self.deadline, cancel)
    }
    pub(crate) fn preflight_intent_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.checkpoint(11, cancel)?;
        let intent = self
            .intent
            .as_ref()
            .context("native intent preflight lacks intent")?;
        self.allocation.reservation(db, Some(&intent.plan), None)?;
        self.checkpoint(12, cancel)
    }
    pub(crate) fn acquire_source_locks_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline, cancel)?;
        let reservation = self.allocation.advance(db, self.allocation.external())?;
        let NativeOwner::Preparing(native) = &mut self.native else {
            anyhow::bail!("native preparation unavailable");
        };
        native.refresh_external_reservation(reservation)?;
        native.acquire_source_locks(cancel)?;
        check(self.deadline, cancel)
    }
    pub(crate) fn prepare_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.prepare_source_in(db, None, cancel)
    }
    pub(crate) fn prepare_locked_in(
        &mut self,
        db: &Connection,
        source: &std::fs::File,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.prepare_source_in(db, Some(source), cancel)
    }
    fn prepare_source_in(
        &mut self,
        db: &Connection,
        source: Option<&std::fs::File>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline, cancel)?;
        let reservation = self.allocation.advance(db, self.allocation.external())?;
        let NativeOwner::Preparing(native) = &mut self.native else {
            anyhow::bail!("native preparation unavailable");
        };
        if source.is_some() {
            native.refresh_source_locked_reservation(reservation)?;
        } else {
            native.refresh_external_reservation(reservation)?;
        }
        self.checkpoint(2, cancel)?;
        let NativeOwner::Preparing(native) = &mut self.native else {
            unreachable!("same retained native owner");
        };
        if let Some(source) = source {
            native.prepare_locked(source, cancel)?;
        } else {
            native.prepare(cancel)?;
        }
        self.checkpoint(3, cancel)?;
        // Both branches install custody synchronously. finish is documented
        // moves-only, with no callback, SQL, allocation or deadline renewal.
        match std::mem::replace(&mut self.native, NativeOwner::Empty) {
            NativeOwner::Preparing(native) => {
                self.native = match native.finish() {
                    Ok(prepared) => NativeOwner::Prepared(prepared),
                    Err(failure) => NativeOwner::Failed(failure),
                };
            }
            other => {
                self.native = other;
                anyhow::bail!("native intent preparation slots differ");
            }
        }
        self.checkpoint(4, cancel)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            anyhow::bail!("native preparation failed with retained opaque custody");
        };
        let (from, predecessor, binding) = self
            .identity
            .as_ref()
            .context("native intent identity missing")?;
        ensure!(
            native.catalog_id() == from.catalog_id && native.incarnation() == from.incarnation,
            "native preparation differs from authenticated Motion identity"
        );
        admit_plan(native.approved_plan())?;
        self.allocation.set_external(Self::external(
            self.retained97,
            self.authorization_path_bytes,
            &self.id,
            native.native_path(),
        )?)?;
        native.refresh_external_reservation(self.allocation.reservation(
            db,
            Some(native.approved_plan()),
            None,
        )?)?;
        // Plan, identity, linkage and normalized pathname clones are admitted
        // before construction, rather than retaining only a post-hoc size check.
        self.intent = Some(Intent {
            contract_version: 1,
            transition_id: self.id.clone(),
            predecessor: predecessor.clone(),
            from: from.clone(),
            to: Identity {
                schema_version: 41,
                ..from.clone()
            },
            frozen_binding_sha256: binding.clone(),
            native_path: native.native_path().to_owned(),
            native_file_identity: native.file_identity(),
            source_generation: native.source_generation(),
            plan: native.approved_plan().clone(),
        });
        let intent = self.intent.as_ref().unwrap();
        validate_intent(intent)?;
        let mut encoded = EncodedBudget { bytes: 0 };
        serde_json::to_writer(&mut encoded, intent)?;
        // Four exact-size representations cover output, insertion permission,
        // reread and canonical verification while all original owners remain.
        self.allocation.admit_raw_storage(
            (encoded.bytes as u64)
                .checked_mul(4)
                .context("native intent raw storage overflow")?,
        )?;
        native.refresh_external_reservation(self.allocation.reservation(
            db,
            Some(&intent.plan),
            None,
        )?)?;
        let mut bytes = Vec::with_capacity(encoded.bytes);
        serde_json::to_writer(&mut bytes, intent)?;
        self.raw = Some(String::from_utf8(bytes)?);
        self.checkpoint(5, cancel)?;
        Ok(())
    }
    /// Fixed borrowed operator seam; not a factory for application authority.
    pub(crate) fn write_intent_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline, cancel)?;
        let external = self.allocation.external();
        let reservation = self.allocation.advance(db, external)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            anyhow::bail!("retained Prepared owner missing");
        };
        native.refresh_external_reservation(reservation)?;
        self.checkpoint(5, cancel)?;
        // journal::insert returns success only after revoking its exact permit.
        // Any insertion/revocation error leaves the entire operation Failed.
        journal::insert(db, "intent", &self.id, self.raw.as_ref().unwrap())?;
        self.checkpoint(6, cancel)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            unreachable!("retained Prepared owner")
        };
        native.refresh_external_reservation(self.allocation.reservation(
            db,
            Some(&self.intent.as_ref().unwrap().plan),
            None,
        )?)?;
        self.checkpoint(13, cancel)?;
        Ok(())
    }
    pub(crate) fn commit_intent_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline, cancel)?;
        self.commit = NativeIntentCommitState::CommitAttempted;
        self.checkpoint(7, cancel)?;
        db.execute_batch("COMMIT")?;
        self.commit = NativeIntentCommitState::DurableIntent96;
        self.checkpoint(8, cancel)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        self.commit = NativeIntentCommitState::WriterReacquired;
        self.checkpoint(9, cancel)?;
        let intent = self.intent.as_ref().unwrap();
        verify_intent_writer(db, intent, self.raw.as_ref().unwrap())?;
        ensure!(
            journal::record(db, "command", &self.id)?.is_none()
                && journal::record(db, "ack", &self.id)?.is_none(),
            "native intent unexpectedly advanced"
        );
        self.checkpoint(10, cancel)?;
        // COMMIT/re-BEGIN and verification can increase the actual target
        // cache. Refusal here retains durable/reacquired history, not Ready.
        let external = self.allocation.external();
        let reservation = self.allocation.advance(db, external)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            anyhow::bail!("retained Prepared owner missing");
        };
        native.refresh_external_reservation(reservation)?;
        self.checkpoint(14, cancel)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "prepare_operation_tests.rs"]
mod tests;
