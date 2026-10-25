//! Retained native41 bootstrap command, receipt-first execution and exact96 ACK.
//! Operator mechanics only. Each purpose needs a fresh real application token.
//! Imported evidence is deliberately absent in this fixed initial-source slice.
use super::*;
use crate::native_bootstrap::source_session::{RetainedProofAdmission, SourcePairProof};
use catabolic_store::catalog_commands::{CommandOutcome, ReceiptPolicy};
use catabolic_store::migration::offline_bootstrap::{
    OfflineControlWorkspaceBacking, OfflineOwnedTransition, OfflineRetainedResult,
    OfflineWorkspaceAdmissionRefused,
};

#[path = "bootstrap_source_workspace.rs"]
mod source_workspace;

/// Own the actual producer result, not a copied error or an uncharged extracted
/// Value. Workspace results retain their own allocation guard even if Native
/// closes before this old Motion tail. Inspection never transfers owned data.
enum BootstrapProducer<T, E> {
    Ordinary(Result<T, E>),
    Workspace(OfflineRetainedResult<T, E>),
    Refused(OfflineWorkspaceAdmissionRefused),
}
impl<T, E> BootstrapProducer<T, E> {
    fn workspace(result: Result<OfflineRetainedResult<T, E>, OfflineWorkspaceAdmissionRefused>) -> Self {
        match result {
            Ok(result) => Self::Workspace(result),
            Err(refused) => Self::Refused(refused),
        }
    }
    fn value(&self) -> Option<&T> {
        match self {
            Self::Ordinary(result) => result.as_ref().ok(),
            Self::Workspace(result) => result.result().as_ref().ok(),
            Self::Refused(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeBootstrapPhase {
    Inputs,
    Failed,
    CommandPreflighted,
    PendingCommandCommit,
    CommandSaved,
    ExecutePreflighted,
    MeasuredComplete,
    AckPreflighted,
    PendingAckCommit,
    ReadySource,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeBootstrapCommitState {
    #[default]
    NotAttempted,
    CommitAttempted,
    Durable,
    WriterReacquired,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeBootstrapExecutionState {
    #[default]
    NotAttempted,
    ReceiptObserved,
    ExecuteAttempted,
    CompleteMeasured,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootstrapPurpose {
    Command,
    Execute,
    Ack,
}
impl BootstrapPurpose {
    fn phase(self) -> NativeBootstrapPhase {
        match self {
            Self::Command => NativeBootstrapPhase::Inputs,
            Self::Execute => NativeBootstrapPhase::CommandSaved,
            Self::Ack => NativeBootstrapPhase::MeasuredComplete,
        }
    }
}
/// Unforgeable within the crate: minted only after the actual native owner
/// accepts the complete close-destination reservation below.
pub(crate) struct SourceCloseAdmission {
    _private: (),
}
pub(crate) struct BootstrapAdmission {
    close_admission: Option<SourceCloseAdmission>,
    deadline: Instant,
    additional_host_bytes: u64,
    storage_bytes: u64,
    operation_id: String,
    policy: ReceiptPolicy,
    // Allocated only after borrowed admission accepts the actual slot layouts.
    // An unfilled slot never takes custody of the original ready owner.
    tail: Option<Box<RetainedBootstrapTail>>,
    ready: bool,
}
pub(crate) struct BootstrapWork {
    admission: BootstrapAdmission,
    base_external: u64,
    owned_bytes: u64,
    outcome_bytes: u64,
    transition_digest: Option<String>,
    command_error: Option<anyhow::Error>,
    payload: Option<BootstrapProducer<Value, catabolic_store::Error>>,
    command_transition: Option<BootstrapProducer<OfflineOwnedTransition, catabolic_store::Error>>,
    execution_transition: Option<BootstrapProducer<OfflineOwnedTransition, catabolic_store::Error>>,
    generation: Option<BootstrapProducer<i64, catabolic_store::Error>>,
    completion: Option<BootstrapProducer<(), catabolic_store::Error>>,
    payload_raw: Option<String>,
    command: Option<CommandEnvelope>,
    reconciliation: Option<BootstrapProducer<Option<CommandOutcome>, catabolic_store::catalog_commands::CommandStoreError>>,
    execution: Option<BootstrapProducer<CommandOutcome, catabolic_store::catalog_commands::CommandStoreError>>,
    acknowledgment: Option<Acknowledgment>,
    ack: Option<String>,
    command_commit: NativeBootstrapCommitState,
    ack_commit: NativeBootstrapCommitState,
    execution_state: NativeBootstrapExecutionState,
    #[cfg(test)]
    fault: Option<(u8, u8)>,
}
struct BootstrapAuthorizationClose {
    connection: Option<sqlx::SqliteConnection>,
    future: Option<AuthorizationCloseFuture>,
    outcome: Option<Result<(), sqlx::Error>>,
    frame_bytes: usize,
    purpose: BootstrapPurpose,
    acknowledged: bool,
    resource_acknowledged: bool,
    polling: bool,
    #[cfg(test)]
    poll_fault: Option<u8>,
}
pub struct NativeBootstrapOperation {
    tail: Box<RetainedBootstrapTail>,
    phase: NativeBootstrapPhase,
    terminal_authorization_cleanup: bool,
    proof_admission: Option<RetainedProofAdmission>,
}
pub struct ReadyNativeSource96 {
    operation: NativeBootstrapOperation,
}
/// At source transfer there is no proof/native/witness/reference recursion.
/// Everything else from the same producer remains alive, including the settled
/// Motion connection and all old close boxes. Preservation/publication and
/// admitted frames stay in inherited H. The settled SQLite category is cached
/// separately by SourcePairProof and included once in fresh owned arithmetic.
/// The transferred tail exposes no execution API.
pub(crate) struct RetainedBootstrapTail {
    // Empty only while borrowed admission retains the original owner outside
    // this box. Begin fills it without allocation; final transfer keeps the
    // same box and never rebuilds this aggregate on the consuming stack.
    owner: Option<BootstrapOwned>,
    // Fresh post-transfer proof, never the consumed Ready97 qualification.
    source_execute_qualification: Option<crate::offline_control_upgrade::SourceWorkspaceQualification>,
    source_capture_qualification: Option<crate::offline_control_upgrade::SourceWorkspaceQualification>,
}
struct BootstrapOwned {
    ready: ReadyNativePendingBootstrap96,
    work: BootstrapWork,
    authorization_readers: [RestrictedReaderOwner; 3],
    authorization_close: Option<BootstrapAuthorizationClose>,
    binding: String,
}
pub(crate) enum BootstrapStep {
    Preflight(BootstrapPurpose),
    InsertCommand,
    CommitCommand,
    Execute,
    InsertAck,
    CommitAck,
}
impl ReadyNativePendingBootstrap96 {
    pub fn admit_bootstrap_reserved(
        &mut self,
        operation_id: &str,
        policy: ReceiptPolicy,
        additional_host_bytes: u64,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == NativeIntentApplyPhase::AppliedPendingBootstrap,
            "bootstrap requires actual applied owner"
        );
        let deadline = self.deadline().min(caller_deadline);
        self.operation.admission.deadline = deadline;
        self.operation.ready.operation.admitted.deadline = deadline;
        if let Some(admission) = self.bootstrap_admission.as_mut() {
            admission.deadline = admission.deadline.min(deadline);
            admission.ready = false;
            anyhow::bail!("bootstrap admission cannot retry; original owner retained terminally");
        }
        ensure!(
            !operation_id.is_empty()
                && operation_id.len() <= 256
                && !operation_id.chars().any(char::is_control),
            "bootstrap operation id differs"
        );
        let intent = self
            .operation
            .ready
            .operation
            .admitted
            .intent
            .as_ref()
            .context("bootstrap admission intent missing")?;
        let key_bytes = [
            operation_id,
            intent.from.catalog_id.as_str(),
            intent.from.incarnation.as_str(),
            crate::HOST_NAMESPACE,
        ]
        .into_iter()
        .try_fold(0u64, |n, text| {
            n.checked_add(text.len() as u64)
                .context("bootstrap key storage overflow")
        })?;
        let close_verification_bytes = self
            .operation
            .ready
            .operation
            .control
            .source_close_verification_bytes()?;
        let storage_bytes = NativeBootstrapOperation::inline_storage_reservation()
            .checked_add(close_verification_bytes)
            .and_then(|n| n.checked_add(AUTHORIZATION_CLOSE_STORAGE_BYTES))
            .and_then(|n| n.checked_add(3 * RestrictedReaderOwner::opening_frame_reservation()))
            .and_then(|n| {
                n.checked_add(
                    self.operation
                        .ready
                        .operation
                        .admitted
                        .authorization_path_bytes
                        .checked_add(1)?
                        .checked_mul(12)?,
                )
            })
            .and_then(|n| n.checked_add(2 * 64))
            .and_then(|n| n.checked_add(additional_host_bytes))
            .and_then(|n| n.checked_add(key_bytes.checked_mul(16)?))
            .context("bootstrap storage overflow")?;
        self.bootstrap_admission = Some(BootstrapAdmission {
            close_admission: None,
            deadline,
            additional_host_bytes,
            storage_bytes,
            operation_id: String::new(),
            policy,
            tail: None,
            ready: false,
        });
        let base = &mut self.operation.ready.operation;
        base.control
            .admit_native_bootstrap(&mut base.admitted, storage_bytes, cancel)?;
        let admission = self.bootstrap_admission.as_mut().unwrap();
        admission.close_admission = Some(SourceCloseAdmission { _private: () });
        // Only the EMPTY pointee is allocated here, after native acceptance,
        // with the original Ready owner still held by this borrowed receiver.
        // Store the box before any subsequent fallible check or allocation.
        admission.tail = Some(Box::new(RetainedBootstrapTail { owner: None, source_execute_qualification: None, source_capture_qualification: None }));
        admission.operation_id = operation_id.to_owned();
        check(deadline, cancel)?;
        admission.ready = true;
        Ok(())
    }
    pub fn begin_bootstrap_reserved(
        mut self,
        required_host_bytes: u64,
    ) -> Result<NativeBootstrapOperation, Self> {
        if !self.bootstrap_admission.as_ref().is_some_and(|a| {
            a.ready
                && a.additional_host_bytes >= required_host_bytes
                && a.tail.as_ref().is_some_and(|tail| tail.owner.is_none())
        }) {
            return Err(self);
        }
        let mut admission = self.bootstrap_admission.take().unwrap();
        let mut tail = admission.tail.take().unwrap();
        let base_external = self
            .operation
            .ready
            .operation
            .admitted
            .allocation
            .external();
        // Move retained custody with only fixed empty/scalar initialization:
        // no allocation, SQL, poll or user callback. Admission no longer owns
        // this box, so filling the whole aggregate cannot form recursion.
        tail.owner = Some(BootstrapOwned {
            ready: self,
            work: BootstrapWork {
                admission,
                base_external,
                owned_bytes: 0,
                outcome_bytes: 0,
                transition_digest: None,
                command_error: None,
                payload: None,
                command_transition: None,
                execution_transition: None,
                generation: None,
                completion: None,
                payload_raw: None,
                command: None,
                reconciliation: None,
                execution: None,
                acknowledgment: None,
                ack: None,
                command_commit: NativeBootstrapCommitState::NotAttempted,
                ack_commit: NativeBootstrapCommitState::NotAttempted,
                execution_state: NativeBootstrapExecutionState::NotAttempted,
                #[cfg(test)]
                fault: None,
            },
            // Fixed derived defaults contain no resource, allocation or user
            // callback. All acquisition remains behind the existing claim gate.
            authorization_readers: [
                RestrictedReaderOwner::default(),
                RestrictedReaderOwner::default(),
                RestrictedReaderOwner::default(),
            ],
            authorization_close: None,
            binding: String::new(),
        });
        Ok(NativeBootstrapOperation {
            tail,
            phase: NativeBootstrapPhase::Inputs,
            terminal_authorization_cleanup: false,
            proof_admission: None,
        })
    }
}
impl NativeBootstrapOperation {
    /// Typed inline/move/Result storage plus the one preallocated tail pointee.
    /// The public reservation API is unchanged; admission charges this whole
    /// destination set before an empty ownership slot can be allocated.
    pub const fn inline_storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<Result<Self, ReadyNativePendingBootstrap96>>()
            + std::mem::size_of::<ReadyNativeSource96>()
            + std::mem::size_of::<Result<ReadyNativeSource96, Self>>()
            + std::mem::size_of::<anyhow::Result<()>>()
            + std::mem::size_of::<anyhow::Result<TargetAuthorizationReader<'static>>>()
            + std::mem::size_of::<SourcePairProof>()
            // Name the pre-finish admission local/Result in addition to its
            // stable operation slot; the cached category's metadata is not free.
            + std::mem::size_of::<RetainedProofAdmission>()
            + std::mem::size_of::<anyhow::Result<RetainedProofAdmission>>()
            // Actual allocated pointee, its owning/optional pointer slots and
            // the whole-owner move destination are admitted BEFORE allocation.
            // These are typed storage, never another copy of inherited H or K.
            + std::mem::size_of::<RetainedBootstrapTail>()
            + std::mem::size_of::<Box<RetainedBootstrapTail>>()
            + std::mem::size_of::<Option<Box<RetainedBootstrapTail>>>()
            + std::mem::size_of::<BootstrapOwned>()
            + std::mem::size_of::<Option<BootstrapOwned>>()
            + std::mem::size_of::<BootstrapAdmission>()
            + std::mem::size_of::<BootstrapWork>()
            + crate::offline_control_upgrade::source_close_storage()
            + std::mem::size_of::<crate::SourceRelocationSession>()
            + 2 * std::mem::size_of::<TargetAuthorizationReader<'static>>()) as u64
    }
    fn owner(&self) -> &BootstrapOwned {
        self.tail.owner.as_ref().unwrap()
    }
    fn owner_mut(&mut self) -> &mut BootstrapOwned {
        self.tail.owner.as_mut().unwrap()
    }
    pub fn phase(&self) -> NativeBootstrapPhase {
        self.phase
    }
    pub fn deadline(&self) -> Instant {
        self.owner().work.admission.deadline
    }
    pub fn command_commit_state(&self) -> NativeBootstrapCommitState {
        self.owner().work.command_commit
    }
    pub fn ack_commit_state(&self) -> NativeBootstrapCommitState {
        self.owner().work.ack_commit
    }
    pub fn execution_state(&self) -> NativeBootstrapExecutionState {
        self.owner().work.execution_state
    }
    pub(crate) fn authorization_purpose(&self) -> anyhow::Result<BootstrapPurpose> {
        match self.phase {
            NativeBootstrapPhase::Inputs => Ok(BootstrapPurpose::Command),
            NativeBootstrapPhase::CommandSaved => Ok(BootstrapPurpose::Execute),
            NativeBootstrapPhase::MeasuredComplete => Ok(BootstrapPurpose::Ack),
            _ => anyhow::bail!("bootstrap authorization phase unavailable"),
        }
    }
    /// Terminal resource cleanup only, including abandoned open/prepare/close
    /// borrowers. Failed is installed even for an unpolled waiter; durable/native
    /// facts are unchanged. Repetition observes only the same retained attempt,
    /// never a new connection, setup, authentication token or effect permission.
    /// Success includes known-never-opened attempts; it is not an effect grant.
    pub fn settle_authorization_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.terminal_authorization_cleanup = true;
        self.phase = NativeBootstrapPhase::Failed;
        self.owner_mut().ready.operation.ready.operation.control.invalidate_bootstrap_workspace();
        async move {
            let owner = self.owner_mut();
            if let Some(close) = owner.authorization_close.as_mut() {
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
                    owner.authorization_readers[match close.purpose {
                        BootstrapPurpose::Command => 0,
                        BootstrapPurpose::Execute => 1,
                        BootstrapPurpose::Ack => 2,
                    }]
                    .mark_transferred_close_joined()?;
                    close.resource_acknowledged = true;
                }
            }
            ensure!(
                owner
                    .authorization_readers
                    .iter()
                    .filter(|reader| reader.is_claimed() && !reader.is_settled())
                    .count()
                    <= 1,
                "multiple unresolved authorization purposes cannot share a close reservation"
            );
            if owner
                .authorization_readers
                .iter()
                .any(|reader| reader.is_claimed() && !reader.is_settled())
            {
                // Only an exact observed ACK permits retiring the previous
                // purpose's box. Its admitted close share is then available to
                // this slot; earlier phase owners retain their separate boxes.
                ensure!(
                    owner
                        .authorization_close
                        .as_ref()
                        .is_none_or(|close| close.resource_acknowledged),
                    "unresolved external authorization close remains retained"
                );
                owner.authorization_close = None;
            }
            for reader in &mut owner.authorization_readers {
                if reader.is_claimed() && !reader.is_settled() {
                    reader.settle_inline().await?;
                }
            }
            Ok(())
        }
    }

    pub fn authorization_reader(
        &mut self,
    ) -> impl Future<Output = anyhow::Result<TargetAuthorizationReader<'_>>> + '_ {
        TargetAuthorizationReader::open_retained_bootstrap(self)
    }
    fn authorization_reader_index(&self) -> anyhow::Result<usize> {
        Ok(match self.authorization_purpose()? {
            BootstrapPurpose::Command => 0,
            BootstrapPurpose::Execute => 1,
            BootstrapPurpose::Ack => 2,
        })
    }
    pub(crate) fn claim_authorization_reader(&mut self) -> anyhow::Result<()> {
        let index = self.authorization_reader_index()?;
        self.owner_mut().authorization_readers[index].claim()?;
        self.verify_authorization_owner()
    }
    pub(crate) fn authorization_reader_slot(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        Ok(&self.owner().authorization_readers[self.authorization_reader_index()?])
    }
    pub(crate) fn authorization_reader_slot_mut(
        &mut self,
    ) -> anyhow::Result<&mut RestrictedReaderOwner> {
        let index = self.authorization_reader_index()?;
        Ok(&mut self.owner_mut().authorization_readers[index])
    }
    pub(crate) fn verify_authorization_owner(&self) -> anyhow::Result<()> {
        let purpose = self.authorization_purpose()?;
        ensure!(
            self.authorization_reader_slot()?.is_claimed(),
            "bootstrap reader was not claimed"
        );
        ensure!(
            self.owner()
                .authorization_close
                .as_ref()
                .is_none_or(|c| c.acknowledged && c.purpose != purpose),
            "bootstrap reader purpose already consumed"
        );
        check(self.deadline(), &AtomicBool::new(false))?;
        let base = &self.owner().ready.operation.ready.operation;
        base.control
            .verify_native_bootstrap(&base.admitted, &self.owner().work)
    }
    pub(crate) fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        self.verify_authorization_owner()?;
        self.owner()
            .ready
            .operation
            .ready
            .operation
            .control
            .native_intent_authorization_path()
    }
    pub(crate) fn begin_authorization_close(&mut self) -> anyhow::Result<BootstrapPurpose> {
        let purpose = self.authorization_purpose()?;
        let index = self.authorization_reader_index()?;
        ensure!(
            self.owner()
                .authorization_close
                .as_ref()
                .is_none_or(|close| close.acknowledged && close.purpose != purpose),
            "bootstrap authorization close cannot retry"
        );
        self.phase = NativeBootstrapPhase::Failed;
        let connection = self.owner_mut().authorization_readers[index].take_for_retained_close()?;
        // Replacing only a successfully acknowledged DIFFERENT purpose keeps one
        //81 box at a time;79/80 boxes retain their separate mandatory shares.
        self.owner_mut().authorization_close = Some(BootstrapAuthorizationClose {
            connection: Some(connection),
            future: None,
            outcome: None,
            frame_bytes: 0,
            purpose,
            acknowledged: false,
            resource_acknowledged: false,
            polling: false,
            #[cfg(test)]
            poll_fault: None,
        });
        check(self.deadline(), &AtomicBool::new(false))?;
        let close = self.owner_mut().authorization_close.as_mut().unwrap();
        close.future = Some(sqlx::Connection::close(close.connection.take().unwrap()));
        close.frame_bytes =
            std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref());
        ensure!(
            (close.frame_bytes as u64)
                .checked_mul(2)
                .is_some_and(|n| n <= AUTHORIZATION_CLOSE_STORAGE_BYTES),
            "bootstrap opaque close frame exceeds admitted share"
        );
        Ok(purpose)
    }
    pub(crate) async fn await_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            !self.terminal_authorization_cleanup,
            "terminal reader cleanup grants no authorization continuation"
        );
        ensure!(
            self.phase == NativeBootstrapPhase::Failed,
            "bootstrap close phase differs"
        );
        let owner = self.tail.owner.as_mut().unwrap();
        let close = owner
            .authorization_close
            .as_mut()
            .context("bootstrap close missing")?;
        ensure!(close.outcome.is_none(), "bootstrap close cannot retry");
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
        owner.authorization_readers[match close.purpose {
            BootstrapPurpose::Command => 0,
            BootstrapPurpose::Execute => 1,
            BootstrapPurpose::Ack => 2,
        }]
        .mark_transferred_close_joined()?;
        close.resource_acknowledged = true;
        check(owner.work.admission.deadline, &AtomicBool::new(false))?;
        close.acknowledged = true;
        self.phase = close.purpose.phase();
        Ok(())
    }
    pub(crate) fn preflight(
        &mut self,
        purpose: BootstrapPurpose,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.owner_mut().ready.operation.ready.operation.control.invalidate_bootstrap_workspace();
        ensure!(
            self.phase == purpose.phase(),
            "bootstrap purpose cannot be reused"
        );
        ensure!(
            !self.authorization_reader_slot()?.is_claimed()
                || self
                    .owner()
                    .authorization_close
                    .as_ref()
                    .is_some_and(|close| close.acknowledged && close.purpose == purpose),
            "bootstrap claimed reader must actually join before effects"
        );
        self.step(BootstrapStep::Preflight(purpose), cancel)?;
        self.phase = match purpose {
            BootstrapPurpose::Command => NativeBootstrapPhase::CommandPreflighted,
            BootstrapPurpose::Execute => NativeBootstrapPhase::ExecutePreflighted,
            BootstrapPurpose::Ack => NativeBootstrapPhase::AckPreflighted,
        };
        Ok(())
    }
    fn step(&mut self, step: BootstrapStep, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.phase = NativeBootstrapPhase::Failed;
        let owner = self.owner_mut();
        let base = &mut owner.ready.operation.ready.operation;
        base.control
            .native_bootstrap_step(&mut base.admitted, &mut owner.work, step, cancel)
    }
    pub(crate) fn insert_command(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeBootstrapPhase::CommandPreflighted,
            "bootstrap command insert cannot retry"
        );
        self.step(BootstrapStep::InsertCommand, cancel)?;
        self.phase = NativeBootstrapPhase::PendingCommandCommit;
        Ok(())
    }
    pub(crate) fn commit_command(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeBootstrapPhase::PendingCommandCommit,
            "bootstrap command commit cannot retry"
        );
        self.step(BootstrapStep::CommitCommand, cancel)?;
        self.phase = NativeBootstrapPhase::CommandSaved;
        Ok(())
    }
    pub(crate) fn execute(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        if self.phase != NativeBootstrapPhase::ExecutePreflighted {
            self.owner_mut().ready.operation.ready.operation.control.invalidate_bootstrap_workspace();
        }
        ensure!(
            self.phase == NativeBootstrapPhase::ExecutePreflighted,
            "bootstrap execution cannot retry"
        );
        self.step(BootstrapStep::Execute, cancel)?;
        self.phase = NativeBootstrapPhase::MeasuredComplete;
        Ok(())
    }
    pub(crate) fn insert_ack(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeBootstrapPhase::AckPreflighted,
            "bootstrap ACK insert cannot retry"
        );
        self.step(BootstrapStep::InsertAck, cancel)?;
        self.phase = NativeBootstrapPhase::PendingAckCommit;
        Ok(())
    }
    pub(crate) fn commit_ack(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeBootstrapPhase::PendingAckCommit,
            "bootstrap ACK commit cannot retry"
        );
        self.step(BootstrapStep::CommitAck, cancel)?;
        let owner = self.tail.owner.as_mut().unwrap();
        let base = &mut owner.ready.operation.ready.operation;
        // Every fallible writer/reference/proof/accounting/settlement check is
        // before this native moves-only finish. No consuming legacy completion.
        self.proof_admission = Some(base.control.preflight_bootstrap_source(
            &mut base.admitted,
            &owner.work,
            cancel,
        )?);
        owner.work.checkpoint(16, cancel)?;
        match std::mem::replace(&mut base.admitted.native, NativeOwner::Empty) {
            NativeOwner::Completing(native) => {
                base.admitted.native = match native.finish() {
                    Ok(native) => NativeOwner::Completed(native),
                    Err(failure) => NativeOwner::CompletionFailed(failure),
                }
            }
            other => {
                base.admitted.native = other;
                anyhow::bail!("bootstrap completion custody missing");
            }
        }
        if matches!(&base.admitted.native, NativeOwner::Completed(_)) {
            self.phase = NativeBootstrapPhase::ReadySource;
            Ok(())
        } else {
            anyhow::bail!("bootstrap completion finish retained failure")
        }
    }
    pub fn settle_motion(&mut self) -> anyhow::Result<()> {
        self.phase = NativeBootstrapPhase::Failed;
        self.owner_mut()
            .ready
            .operation
            .ready
            .operation
            .settle_motion()
    }
    pub fn into_ready(self) -> Result<ReadyNativeSource96, Self> {
        if self.phase == NativeBootstrapPhase::ReadySource {
            Ok(ReadyNativeSource96 { operation: self })
        } else {
            Err(self)
        }
    }
}
impl ReadyNativeSource96 {
    pub fn deadline(&self) -> Instant {
        self.operation.deadline()
    }
    /// Pure move after all verification and writer settlement. No fresh reference
    /// rebuild, reader acquisition, I/O or fallback consumes the original owner.
    pub fn into_source_session(self) -> crate::SourceRelocationSession {
        let NativeBootstrapOperation {
            mut tail,
            proof_admission,
            ..
        } = self.operation;
        let owner = tail.owner.as_mut().unwrap();
        let base = &mut owner.ready.operation.ready.operation;
        let native = match std::mem::replace(&mut base.admitted.native, NativeOwner::Empty) {
            NativeOwner::Completed(native) => native,
            _ => unreachable!("Ready native completed owner"),
        };
        let intent = base.admitted.intent.take().unwrap();
        let raw = base.admitted.raw.take().unwrap();
        let command = owner.work.command.take().unwrap();
        let ack = owner.work.ack.take().unwrap();
        let (witness, reference, binding) = base.control.take_settled_source_parts();
        let close_admission = owner.work.admission.close_admission.take().unwrap();
        owner.binding = binding;
        // The original admission box moves intact. No allocation, callback,
        // owner reconstruction or old-SQLite query occurs at this handoff.
        let proof =
            proof_admission
                .unwrap()
                .finish(intent, raw, command, ack, tail, close_admission);
        crate::SourceRelocationSession::from_stage(
            crate::offline_control_upgrade::StageSourceSession::from_retained_parts(
                native, proof, witness, reference,
            ),
        )
    }
}

impl BootstrapWork {
    fn checkpoint(&self, point: u8, cancel: &AtomicBool) -> anyhow::Result<()> {
        #[cfg(test)]
        if let Some((wanted, mode)) = self.fault {
            if wanted == point {
                match mode {
                    1 => panic!("retained bootstrap checkpoint {point}"),
                    2 => cancel.store(true, Ordering::Release),
                    _ => anyhow::bail!("retained bootstrap checkpoint {point}"),
                }
            }
        }
        let _ = point;
        check(self.admission.deadline, cancel)
    }
    fn command(&self) -> anyhow::Result<&CommandEnvelope> {
        self.command
            .as_ref()
            .context("retained bootstrap command missing")
    }
}
impl NativeIntentAdmission {
    fn refresh_bootstrap(
        &mut self,
        reservation: catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation,
    ) -> anyhow::Result<()> {
        match &mut self.native {
            NativeOwner::Applied(native) => native.refresh_external_reservation(reservation)?,
            NativeOwner::Completing(native) => native.refresh_external_reservation(reservation)?,
            _ => anyhow::bail!("retained bootstrap accounting owner unavailable"),
        }
        Ok(())
    }
    fn full_bootstrap_reservation(
        &mut self,
        db: &Connection,
        work: &BootstrapWork,
    ) -> anyhow::Result<()> {
        let reservation = self.allocation.reservation(
            db,
            Some(
                &self
                    .intent
                    .as_ref()
                    .context("bootstrap intent missing")?
                    .plan,
            ),
            work.command.as_ref(),
        )?;
        self.refresh_bootstrap(reservation)
    }
    fn narrow_bootstrap_reservation(&mut self, db: &Connection) -> anyhow::Result<()> {
        let external = self.allocation.external();
        let reservation = self.allocation.advance(db, external)?;
        self.refresh_bootstrap(reservation)
    }
    fn admit_bootstrap_owned(
        &mut self,
        db: &Connection,
        work: &mut BootstrapWork,
        bytes: u64,
    ) -> anyhow::Result<()> {
        work.owned_bytes = work
            .owned_bytes
            .checked_add(bytes)
            .context("bootstrap owned storage overflow")?;
        let external = work
            .base_external
            .checked_add(work.owned_bytes)
            .context("bootstrap external overflow")?;
        let reservation = self.allocation.advance(db, external)?;
        self.refresh_bootstrap(reservation)
    }
    pub(crate) fn admit_bootstrap_in(
        &mut self,
        db: &Connection,
        bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline, cancel)?;
        let external = self
            .allocation
            .external()
            .checked_add(bytes)
            .context("bootstrap admission overflow")?;
        let reservation = self.allocation.advance(db, external)?;
        self.refresh_bootstrap(reservation)?;
        check(self.deadline, cancel)
    }
    pub(crate) fn verify_bootstrap_in(
        &self,
        db: &Connection,
        work: &BootstrapWork,
    ) -> anyhow::Result<()> {
        check(work.admission.deadline, &AtomicBool::new(false))?;
        let intent = self.intent.as_ref().context("bootstrap intent missing")?;
        let raw = self.raw.as_ref().context("bootstrap raw missing")?;
        verify_intent_writer(db, intent, raw)?;
        if work.command_commit == NativeBootstrapCommitState::WriterReacquired {
            verify_saved_writer(db, intent, raw, work.command()?)?;
        } else {
            ensure!(
                journal::record(db, "command", &self.id)?.is_none(),
                "bootstrap command already exists"
            );
        }
        ensure!(
            journal::record(db, "ack", &self.id)?.is_none(),
            "bootstrap ACK already exists"
        );
        ensure!(
            matches!(
                &self.native,
                NativeOwner::Applied(_) | NativeOwner::Completing(_)
            ),
            "bootstrap actual native owner differs"
        );
        Ok(())
    }
    pub(crate) fn bootstrap_step_in(
        &mut self,
        db: &Connection,
        work: &mut BootstrapWork,
        step: BootstrapStep,
        cancel: &AtomicBool,
        mut control: Option<&mut OfflineControlWorkspaceBacking>,
    ) -> anyhow::Result<()> {
        work.checkpoint(1, cancel)?;
        match step {
            BootstrapStep::Preflight(purpose) => {
                self.full_bootstrap_reservation(db, work)?;
                self.verify_bootstrap_in(db, work)?;
                match purpose {
                    BootstrapPurpose::Command => self.prepare_bootstrap_command(db, work, control.as_deref_mut())?,
                    BootstrapPurpose::Execute => {
                        let NativeOwner::Applied(native) = &self.native else {
                            anyhow::bail!("bootstrap receipt owner missing");
                        };
                        work.reconciliation = Some(match control.as_deref_mut() {
                            Some(backing) => BootstrapProducer::workspace(
                                native.reconcile_with_control_workspace(work.command()?, crate::HOST_NAMESPACE, backing),
                            ),
                            None => BootstrapProducer::Ordinary(native.reconcile(work.command()?, crate::HOST_NAMESPACE)),
                        });
                        ensure!(
                            work.reconciliation.as_ref().and_then(BootstrapProducer::value).is_some(),
                            "bootstrap receipt lookup failed; actual result retained"
                        );
                        if matches!(work.reconciliation.as_ref().and_then(BootstrapProducer::value), Some(Some(_))) {
                            work.execution_state = NativeBootstrapExecutionState::ReceiptObserved;
                        }
                        self.admit_bootstrap_outcomes(db, work)?;
                    }
                    BootstrapPurpose::Ack => self.prepare_bootstrap_ack(db, work)?,
                }
                self.full_bootstrap_reservation(db, work)?;
                work.checkpoint(2, cancel)
            }
            BootstrapStep::InsertCommand => {
                self.narrow_bootstrap_reservation(db)?;
                journal::insert(db, "command", &self.id, work.command()?.canonical_payload())?;
                work.checkpoint(3, cancel)?;
                self.full_bootstrap_reservation(db, work)?;
                work.checkpoint(4, cancel)
            }
            BootstrapStep::CommitCommand => {
                self.narrow_bootstrap_reservation(db)?;
                work.command_commit = NativeBootstrapCommitState::CommitAttempted;
                work.checkpoint(5, cancel)?;
                db.execute_batch("COMMIT")?;
                work.command_commit = NativeBootstrapCommitState::Durable;
                work.checkpoint(6, cancel)?;
                db.execute_batch("BEGIN IMMEDIATE")?;
                work.command_commit = NativeBootstrapCommitState::WriterReacquired;
                verify_saved_writer(
                    db,
                    self.intent.as_ref().unwrap(),
                    self.raw.as_ref().unwrap(),
                    work.command()?,
                )?;
                self.narrow_bootstrap_reservation(db)?;
                work.checkpoint(7, cancel)
            }
            BootstrapStep::Execute => self.execute_and_measure_bootstrap(db, work, cancel, control),
            BootstrapStep::InsertAck => {
                self.narrow_bootstrap_reservation(db)?;
                journal::insert(
                    db,
                    "ack",
                    &self.id,
                    work.ack.as_ref().context("bootstrap ACK missing")?,
                )?;
                work.checkpoint(11, cancel)?;
                self.full_bootstrap_reservation(db, work)?;
                work.checkpoint(12, cancel)
            }
            BootstrapStep::CommitAck => {
                self.narrow_bootstrap_reservation(db)?;
                work.ack_commit = NativeBootstrapCommitState::CommitAttempted;
                work.checkpoint(13, cancel)?;
                db.execute_batch("COMMIT")?;
                work.ack_commit = NativeBootstrapCommitState::Durable;
                work.checkpoint(14, cancel)?;
                db.execute_batch("BEGIN IMMEDIATE")?;
                work.ack_commit = NativeBootstrapCommitState::WriterReacquired;
                SourcePairProof::verify_retained_parts(
                    db,
                    self.intent.as_ref().unwrap(),
                    self.raw.as_ref().unwrap(),
                    work.command()?,
                    work.ack.as_ref().unwrap(),
                )?;
                self.narrow_bootstrap_reservation(db)?;
                work.checkpoint(15, cancel)
            }
        }
    }
    fn prepare_bootstrap_command(
        &mut self,
        db: &Connection,
        work: &mut BootstrapWork,
        mut control: Option<&mut OfflineControlWorkspaceBacking>,
    ) -> anyhow::Result<()> {
        let NativeOwner::Applied(native) = &self.native else {
            anyhow::bail!("bootstrap payload owner missing");
        };
        let intent = self.intent.as_ref().unwrap();
        if let Some(backing) = control.as_deref_mut() {
            work.command_transition = Some(BootstrapProducer::workspace(
                native.transition_with_control_workspace(backing),
            ));
            let transition = work.command_transition.as_ref().and_then(BootstrapProducer::value)
                .context("bootstrap transition failed; actual result retained")?;
            verify_bootstrap_transition(intent, transition.approved_plan_sha256(),
                transition.source_generation(), transition.catalog_id(), transition.incarnation(), transition.digest())?;
            // Existing admission separately prepays this SHA-256 host copy.
            work.transition_digest = Some(transition.digest().to_owned());
        } else {
            let transition = native.transition()?;
            verify_bootstrap_transition(intent, transition.approved_plan_sha256(),
                transition.source_generation(), transition.catalog_id(), transition.incarnation(), transition.digest())?;
            work.transition_digest = Some(transition.digest().to_owned());
        }
        // Actual native result installed before any encoding/accounting. Native
        // provisional producer buffers and opaque library constructors remain an
        // explicit limitation, not a hard per-allocation peak guarantee.
        work.payload = Some(match control.as_deref_mut() {
            Some(backing) => BootstrapProducer::workspace(native.prepare_empty_with_control_workspace(crate::HOST_NAMESPACE, backing)),
            None => BootstrapProducer::Ordinary(native.prepare(crate::HOST_NAMESPACE, &[])),
        });
        let payload = work
            .payload
            .as_ref()
            .and_then(BootstrapProducer::value)
            .context("native payload failed; actual result retained")?;
        let mut encoded = EncodedBudget { bytes: 0 };
        serde_json::to_writer(&mut encoded, payload)?;
        let owned = accounting::value_bytes(payload)?
            .checked_mul(4)
            .and_then(|n| n.checked_add((encoded.bytes as u64).checked_mul(8)?))
            .context("bootstrap payload/command copies overflow")?;
        // Named conservative simultaneous original payload, encoded payload,
        // canonical command/parser and exact INSERT/reread cohorts, not K-as-H.
        self.admit_bootstrap_owned(db, work, owned)?;
        let mut bytes = Vec::with_capacity(encoded.bytes);
        serde_json::to_writer(&mut bytes, work.payload.as_ref().unwrap().value().unwrap())?;
        work.payload_raw = Some(String::from_utf8(bytes)?);
        let NativeOwner::Applied(native) = &mut self.native else {
            unreachable!()
        };
        let intent = self.intent.as_ref().unwrap();
        // Preserve the original key-admission-before-generation query order.
        let key = CommandKey::new(
            &intent.from.catalog_id,
            crate::HOST_NAMESPACE,
            &work.admission.operation_id,
        )?;
        let generation = if let Some(backing) = control.as_deref_mut() {
            work.generation = Some(BootstrapProducer::workspace(native.generation_with_control_workspace(backing)));
            *work.generation.as_ref().and_then(BootstrapProducer::value)
                .context("bootstrap generation failed; actual result retained")?
        } else {
            native.generation()?
        };
        let result = CommandEnvelope::new(
            1,
            key,
            &intent.from.incarnation,
            generation,
            work.payload_raw.as_ref().unwrap(),
        );
        match result {
            Ok(command) => work.command = Some(command),
            Err(error) => {
                work.command_error = Some(error.into());
                anyhow::bail!("bootstrap envelope failed; actual result retained");
            }
        }
        // Pre-admission clamps the completion operation to the same inherited
        // deadline. Bootstrap prepare/reconcile/execute retain their producer's
        // original bound; a tighter caller bound is checked around these existing
        // synchronous calls, not injected into library-internal work. No move yet.
        native
            .admit_completion_operation(work.command.as_ref().unwrap(), work.admission.deadline)?;
        Ok(())
    }
    fn admit_bootstrap_outcomes(
        &mut self,
        db: &Connection,
        work: &mut BootstrapWork,
    ) -> anyhow::Result<()> {
        let mut bytes = 0u64;
        for outcome in [
            work.reconciliation
                .as_ref()
                .and_then(BootstrapProducer::value)
                .and_then(|v| v.as_ref()),
            work.execution.as_ref().and_then(BootstrapProducer::value),
        ]
        .into_iter()
        .flatten()
        {
            if let CommandOutcome::Committed { result, .. }
            | CommandOutcome::Replay { result, .. } = outcome
            {
                bytes = bytes
                    .checked_add(accounting::value_bytes(result)?)
                    .context("bootstrap outcome overflow")?;
            }
        }
        let delta = bytes.saturating_sub(work.outcome_bytes);
        work.outcome_bytes = work.outcome_bytes.max(bytes);
        self.admit_bootstrap_owned(db, work, delta)
    }
    fn execute_and_measure_bootstrap(
        &mut self,
        db: &Connection,
        work: &mut BootstrapWork,
        cancel: &AtomicBool,
        mut control: Option<&mut OfflineControlWorkspaceBacking>,
    ) -> anyhow::Result<()> {
        self.narrow_bootstrap_reservation(db)?;
        match work.reconciliation.as_ref().and_then(BootstrapProducer::value) {
            Some(None) => {
                work.execution_state = NativeBootstrapExecutionState::ExecuteAttempted;
                let NativeOwner::Applied(native) = &self.native else {
                    anyhow::bail!("native execution owner missing");
                };
                work.execution = Some(match control.as_deref_mut() {
                    Some(backing) => BootstrapProducer::workspace(native.execute_empty_with_control_workspace(
                        work.command()?, crate::HOST_NAMESPACE, work.admission.policy, backing,
                    )),
                    None => BootstrapProducer::Ordinary(native.execute(
                        work.command()?, crate::HOST_NAMESPACE, &[], work.admission.policy,
                    )),
                });
                ensure!(
                    work.execution.as_ref().and_then(BootstrapProducer::value).is_some(),
                    "native execution failed; actual result and uncertain receipt retained"
                );
            }
            Some(Some(_)) => {}
            _ => anyhow::bail!("receipt-first lookup missing"),
        }
        work.checkpoint(8, cancel)?;
        self.admit_bootstrap_outcomes(db, work)?;
        self.full_bootstrap_reservation(db, work)?;
        let NativeOwner::Applied(native) = &self.native else {
            anyhow::bail!("completion transition owner missing");
        };
        if let Some(backing) = control.as_deref_mut() {
            work.execution_transition = Some(BootstrapProducer::workspace(native.transition_with_control_workspace(backing)));
            let transition = work.execution_transition.as_ref().and_then(BootstrapProducer::value)
                .context("completion transition failed; actual result retained")?;
            ensure!(Some(transition.digest()) == work.transition_digest.as_deref(),
                "completion permanent transition differs");
        } else {
            let transition = native.transition()?;
            ensure!(Some(transition.digest()) == work.transition_digest.as_deref(),
                "completion permanent transition differs");
        }
        // Actual borrowed completion operation installs before measure or any
        // injected failure. Legacy consuming complete/into_completed is forbidden.
        match std::mem::replace(&mut self.native, NativeOwner::Empty) {
            NativeOwner::Applied(native) => {
                self.native = match native.begin_completion() {
                    Ok(operation) => NativeOwner::Completing(operation),
                    Err(failure) => NativeOwner::Applied(failure.into_bootstrap()),
                }
            }
            other => {
                self.native = other;
                anyhow::bail!("native completion begin source differs");
            }
        }
        work.checkpoint(9, cancel)?;
        let NativeOwner::Completing(native) = &mut self.native else {
            anyhow::bail!("completion admission missing");
        };
        if let Some(backing) = control.as_deref_mut() {
            work.completion = Some(BootstrapProducer::workspace(native.complete_with_control_workspace(cancel, backing)));
            ensure!(work.completion.as_ref().and_then(BootstrapProducer::value).is_some(),
                "native completion failed; actual result retained");
        } else {
            native.complete(cancel)?;
        }
        work.execution_state = NativeBootstrapExecutionState::CompleteMeasured;
        work.checkpoint(10, cancel)?;
        self.full_bootstrap_reservation(db, work)
    }
    fn prepare_bootstrap_ack(
        &mut self,
        db: &Connection,
        work: &mut BootstrapWork,
    ) -> anyhow::Result<()> {
        let NativeOwner::Completing(native) = &self.native else {
            anyhow::bail!("ACK completion operation missing");
        };
        let complete = native
            .measured_facts()
            .context("ACK requires independent actual Complete")?;
        let intent = self.intent.as_ref().unwrap();
        let command = work.command()?;
        ensure!(
            complete.catalog_id() == intent.to.catalog_id
                && complete.incarnation() == intent.to.incarnation
                && complete.command_key() == command.key()
                && complete.payload_digest() == command.digest().as_str()
                && Some(complete.transition_digest()) == work.transition_digest.as_deref(),
            "measured Complete differs from exact command"
        );
        let text_bytes = [
            intent.transition_id.as_str(),
            complete.transition_digest(),
            complete.inventory_digest(),
            complete.state_digest(),
            complete.payload_digest(),
            command.key().catalog_id(),
            command.key().host_namespace(),
            command.key().operation_id(),
        ]
        .iter()
        .try_fold(64u64, |n, s| {
            n.checked_add(s.len() as u64).context("ACK text overflow")
        })?;
        // String clones and encoded/INSERT/reread cohorts are admitted before
        // construction. The getter is allocation-free and does not rerun Complete.
        self.admit_bootstrap_owned(
            db,
            work,
            text_bytes
                .checked_mul(16)
                .and_then(|n| n.checked_add(2048))
                .context("ACK storage overflow")?,
        )?;
        let NativeOwner::Completing(native) = &self.native else {
            unreachable!()
        };
        let complete = native
            .measured_facts()
            .context("ACK Complete lost after accounting")?;
        let intent = self.intent.as_ref().unwrap();
        work.acknowledgment = Some(Acknowledgment {
            contract_version: 1,
            transition_id: intent.transition_id.clone(),
            intent_sha256: hash(self.raw.as_ref().unwrap()),
            native_transition_sha256: complete.transition_digest().to_owned(),
            command_key: complete.command_key().clone(),
            payload_digest: complete.payload_digest().to_owned(),
            inventory_digest: complete.inventory_digest().to_owned(),
            committed_generation: complete.committed_generation(),
            state_digest: complete.state_digest().to_owned(),
            source_count: complete.source_count(),
        });
        validate_ack(
            intent,
            self.raw.as_ref().unwrap(),
            work.command()?,
            work.acknowledgment.as_ref().unwrap(),
        )?;
        let mut encoded = EncodedBudget { bytes: 0 };
        serde_json::to_writer(&mut encoded, work.acknowledgment.as_ref().unwrap())?;
        let mut bytes = Vec::with_capacity(encoded.bytes);
        serde_json::to_writer(&mut bytes, work.acknowledgment.as_ref().unwrap())?;
        work.ack = Some(String::from_utf8(bytes)?);
        Ok(())
    }
    pub(crate) fn preflight_bootstrap_source_in(
        &mut self,
        db: &Connection,
        work: &BootstrapWork,
        cancel: &AtomicBool,
    ) -> anyhow::Result<RetainedProofAdmission> {
        ensure!(
            work.ack_commit == NativeBootstrapCommitState::WriterReacquired,
            "source requires durable exact ACK and reacquired writer"
        );
        SourcePairProof::verify_retained_parts(
            db,
            self.intent.as_ref().unwrap(),
            self.raw.as_ref().unwrap(),
            work.command()?,
            work.ack.as_ref().unwrap(),
        )?;
        self.full_bootstrap_reservation(db, work)?;
        let NativeOwner::Completing(native) = &self.native else {
            anyhow::bail!("source completion custody missing");
        };
        ensure!(
            native.measured_facts().is_some(),
            "source completion is not measured"
        );
        check(work.admission.deadline, cancel)?;
        db.progress_handler(0, None::<fn() -> bool>)?;
        db.execute_batch("ROLLBACK")?;
        ensure!(
            db.is_autocommit()
                && db.transaction_state(Some("main"))? == rusqlite::TransactionState::None,
            "source original writer settlement incomplete"
        );
        self.narrow_bootstrap_reservation(db)?;
        check(work.admission.deadline, cancel)?;
        let (host, forwarded) = self.allocation.source_pair_reservation();
        // All old-DB SQL, statement cleanup and settlement precede the narrow
        // sample above. No subsequent production path queries/mutates this DB:
        // witness/deadline checks, native finish and opaque transfer are next.
        // Preserve only its known category, not the whole K or a new H claim.
        let settled_target = self.allocation.source_pair_target_highwater()?;
        RetainedProofAdmission::new(host, forwarded, settled_target)
    }
}

#[cfg(test)]
#[path = "bootstrap_operation_tests.rs"]
pub(crate) mod tests;

/// Shared pure comparison preserves the ordinary and fixed-workspace paths.
fn verify_bootstrap_transition(
    intent: &Intent,
    plan_sha256: &str,
    source_generation: i64,
    catalog_id: &str,
    incarnation: &str,
    digest: &str,
) -> anyhow::Result<()> {
    ensure!(
        plan_sha256 == intent.plan["plan_fingerprint"].as_str()
            .context("bootstrap plan fingerprint missing")?
            && source_generation == intent.source_generation
            && catalog_id == intent.from.catalog_id
            && incarnation == intent.from.incarnation
            && hex(digest),
        "bootstrap permanent migration differs from intent"
    );
    Ok(())
}
