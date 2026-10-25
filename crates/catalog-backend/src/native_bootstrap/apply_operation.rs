//! Retained durable intent96 -> native applied/pending-bootstrap owner.
//! No bootstrap command, ACK, completion, paired95 or service activation.
//! App authority must be fresh for this phase; backend calls are operator mechanics.
use super::*;
#[path = "bootstrap_operation.rs"]
pub(crate) mod bootstrap_operation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIntentApplyPhase {
    Inputs,
    Failed,
    Preflighted,
    AppliedPendingBootstrap,
}
/// An attempted failure may already contain durable native41. This is not a
/// rollback certificate, and only the final variant asserts an actual Bootstrap owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeIntentApplyProgress {
    NotAttempted,
    ApplyAttempted,
    AppliedPendingBootstrap,
}
pub(crate) struct NativeApplyAdmission {
    deadline: Instant,
    additional_host_bytes: u64,
    base_external: u64,
    storage_bytes: u64,
    backup_parent: Option<PathBuf>,
    ready: bool,
    #[cfg(test)]
    fault: Option<(u8, u8)>,
}
/// Separate from79's acknowledged close slot, which remains alive inside Ready.
/// Each box therefore has its own mandatory admitted share; no reuse is assumed.
struct ApplyAuthorizationClose {
    connection: Option<sqlx::SqliteConnection>,
    future: Option<AuthorizationCloseFuture>,
    outcome: Option<Result<(), sqlx::Error>>,
    frame_bytes: usize,
    acknowledged: bool,
    resource_acknowledged: bool,
    polling: bool,
    #[cfg(test)]
    poll_fault: Option<u8>,
}
pub struct NativeIntentApplyOperation {
    ready: ReadyNativeIntent96,
    admission: NativeApplyAdmission,
    phase: NativeIntentApplyPhase,
    progress: NativeIntentApplyProgress,
    authorization_reader: RestrictedReaderOwner,
    authorization_close: Option<ApplyAuthorizationClose>,
    terminal_authorization_cleanup: bool,
}
/// Actual retained OfflineBootstrap, not a completed receipt or normal-open grant.
/// Original97 writer/reference/witness/record FDs and durable96 linkage remain inline.
pub struct ReadyNativePendingBootstrap96 {
    operation: NativeIntentApplyOperation,
    bootstrap_admission: Option<bootstrap_operation::BootstrapAdmission>,
}
impl ReadyNativeIntent96 {
    pub fn admit_native_apply(
        &mut self,
        backup_parent: Option<&Path>,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.admit_native_apply_reserved(backup_parent, 0, caller_deadline, cancel)
    }
    /// Admission borrows the whole Ready owner. Failed started admission retains
    /// its raised accounting/native admission and cannot retry or start effects.
    /// Successful repetition may only tighten time and reuse covered host storage.
    pub fn admit_native_apply_reserved(
        &mut self,
        backup_parent: Option<&Path>,
        additional_host_bytes: u64,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == NativeIntentPreparationPhase::ReadyIntent96
                && self.operation.admitted.commit == NativeIntentCommitState::WriterReacquired,
            "native apply requires retained durable intent96"
        );
        let deadline = self.operation.deadline().min(caller_deadline);
        self.operation.admitted.deadline = deadline;
        if let Some(admission) = self.apply_admission.as_mut() {
            admission.deadline = admission.deadline.min(deadline);
            ensure!(
                admission.ready,
                "failed native apply admission cannot retry"
            );
            let NativeOwner::Prepared(native) = &mut self.operation.admitted.native else {
                anyhow::bail!("native apply Prepared owner missing");
            };
            // Tighten the actual native deadline even if the caller's changed
            // directory/storage request is subsequently refused.
            native.admit_apply_operation(admission.backup_parent.as_deref(), admission.deadline)?;
            ensure!(
                admission.backup_parent.as_deref() == backup_parent
                    && admission.additional_host_bytes >= additional_host_bytes,
                "native apply admitted directory or host storage differs"
            );
            return check(admission.deadline, cancel);
        }
        check(deadline, cancel)?;
        let path_bytes = backup_parent.map_or(0, |path| path.as_os_str().len()) as u64;
        let storage_bytes = NativeIntentApplyOperation::inline_storage_reservation()
            .checked_add(AUTHORIZATION_CLOSE_STORAGE_BYTES)
            .and_then(|n| n.checked_add(RestrictedReaderOwner::opening_frame_reservation()))
            .and_then(|n| {
                n.checked_add(
                    self.operation
                        .admitted
                        .authorization_path_bytes
                        .checked_add(1)?
                        .checked_mul(4)?,
                )
            })
            .and_then(|n| n.checked_add(additional_host_bytes))
            .and_then(|n| n.checked_add(path_bytes.checked_mul(2)?))
            .context("native apply host storage overflow")?;
        // Inline marker before any fallible admission work. A partial raise is
        // retained exactly once, never re-added by retrying a failed admission.
        self.apply_admission = Some(NativeApplyAdmission {
            deadline,
            additional_host_bytes,
            base_external: self.operation.admitted.allocation.external(),
            storage_bytes,
            backup_parent: None,
            ready: false,
            #[cfg(test)]
            fault: None,
        });
        self.operation.control.admit_native_apply(
            &mut self.operation.admitted,
            storage_bytes,
            backup_parent,
            deadline,
            cancel,
        )?;
        let admission = self.apply_admission.as_mut().unwrap();
        admission.backup_parent = backup_parent.map(Path::to_path_buf);
        check(deadline, cancel)?;
        admission.ready = true;
        Ok(())
    }
    pub fn begin_native_apply(self) -> Result<NativeIntentApplyOperation, Self> {
        self.begin_native_apply_reserved(0)
    }
    /// Moves only. Missing/inadequate admission returns this exact whole owner;
    /// actual Prepared custody is not consumed into the native apply op yet.
    pub fn begin_native_apply_reserved(
        mut self,
        required_host_bytes: u64,
    ) -> Result<NativeIntentApplyOperation, Self> {
        if !self
            .apply_admission
            .as_ref()
            .is_some_and(|a| a.ready && a.additional_host_bytes >= required_host_bytes)
        {
            return Err(self);
        }
        let admission = self.apply_admission.take().unwrap();
        Ok(NativeIntentApplyOperation {
            ready: self,
            admission,
            phase: NativeIntentApplyPhase::Inputs,
            progress: NativeIntentApplyProgress::NotAttempted,
            authorization_reader: RestrictedReaderOwner::default(),
            authorization_close: None,
            terminal_authorization_cleanup: false,
        })
    }
}
impl NativeIntentApplyOperation {
    /// Additional typed host destination/Result slots, not another copy of H79.
    /// Native's actual apply operation/finish slots are admitted by its own API;
    /// no separate native operation-size scalar is added here. Whole typed host
    /// move destinations are conservatively charged while all prior H survives.
    pub const fn inline_storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<ReadyNativePendingBootstrap96>()
            + std::mem::size_of::<Result<Self, ReadyNativeIntent96>>()
            + std::mem::size_of::<Result<ReadyNativePendingBootstrap96, Self>>()
            + std::mem::size_of::<Result<(), anyhow::Error>>()
            + std::mem::size_of::<Result<(), sqlx::Error>>()
            + std::mem::size_of::<AuthorizationCloseFuture>()
            + std::mem::size_of::<TargetAuthorizationReader<'static>>()
            + std::mem::size_of::<Result<TargetAuthorizationReader<'static>, anyhow::Error>>())
            as u64
    }
    pub fn phase(&self) -> NativeIntentApplyPhase {
        self.phase
    }
    pub fn progress(&self) -> NativeIntentApplyProgress {
        self.progress
    }
    pub fn deadline(&self) -> Instant {
        self.admission.deadline
    }
    pub fn intent_commit_state(&self) -> NativeIntentCommitState {
        self.ready.commit_state()
    }
    /// Terminal resource cleanup only, including abandoned open/prepare/close
    /// borrowers. Failed is installed even for an unpolled waiter; durable/native
    /// facts are unchanged. Repetition observes only the same retained attempt,
    /// never a new connection, setup, authentication token or effect permission.
    /// Success includes known-never-opened attempts; it is not an effect grant.
    pub fn settle_authorization_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.terminal_authorization_cleanup = true;
        self.phase = NativeIntentApplyPhase::Failed;
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
                    self.authorization_reader.mark_transferred_close_joined()?;
                    close.resource_acknowledged = true;
                }
            }
            if std::iter::once(&self.authorization_reader)
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
            for reader in std::slice::from_mut(&mut self.authorization_reader) {
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
        TargetAuthorizationReader::open_retained_apply(self)
    }
    pub(crate) fn claim_authorization_reader(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentApplyPhase::Inputs && self.authorization_close.is_none(),
            "native apply authorization cannot reopen"
        );
        self.authorization_reader.claim()?;
        self.verify_authorization_owner()
    }
    pub(crate) fn authorization_reader_slot(&self) -> anyhow::Result<&RestrictedReaderOwner> {
        Ok(&self.authorization_reader)
    }
    pub(crate) fn authorization_reader_slot_mut(
        &mut self,
    ) -> anyhow::Result<&mut RestrictedReaderOwner> {
        Ok(&mut self.authorization_reader)
    }
    pub(crate) fn verify_authorization_owner(&self) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentApplyPhase::Inputs
                && self.authorization_close.is_none()
                && self.authorization_reader.is_claimed(),
            "native apply authorization unavailable or its one close already started"
        );
        check(self.deadline(), &AtomicBool::new(false))?;
        self.ready
            .operation
            .control
            .verify_native_apply(&self.ready.operation.admitted)
    }
    pub(crate) fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        self.verify_authorization_owner()?;
        self.ready
            .operation
            .control
            .native_intent_authorization_path()
    }
    pub fn preflight_for_apply(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentApplyPhase::Inputs,
            "native apply preflight cannot retry"
        );
        ensure!(
            !self.authorization_reader.is_claimed()
                || self
                    .authorization_close
                    .as_ref()
                    .is_some_and(|close| close.acknowledged),
            "native apply claimed reader must actually join before effects"
        );
        self.phase = NativeIntentApplyPhase::Failed;
        self.ready
            .operation
            .control
            .preflight_native_apply(&mut self.ready.operation.admitted, cancel)?;
        self.phase = NativeIntentApplyPhase::Preflighted;
        Ok(())
    }
    /// Synchronous fixed continuation after the app's final pure temporal checks.
    /// Native SQL/copy/preservation calls only honor their existing checkpoints;
    /// there is no per-effect credential check or per-charge deadline guarantee.
    pub fn apply(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentApplyPhase::Preflighted,
            "native apply cannot retry"
        );
        self.phase = NativeIntentApplyPhase::Failed;
        self.ready.operation.control.apply_native_intent(
            &mut self.ready.operation.admitted,
            &mut self.admission,
            &mut self.progress,
            cancel,
        )?;
        self.phase = NativeIntentApplyPhase::AppliedPendingBootstrap;
        Ok(())
    }
    pub fn into_pending(self) -> Result<ReadyNativePendingBootstrap96, Self> {
        if self.phase == NativeIntentApplyPhase::AppliedPendingBootstrap {
            Ok(ReadyNativePendingBootstrap96 {
                operation: self,
                bootstrap_admission: None,
            })
        } else {
            Err(self)
        }
    }
    pub fn settle_motion(&mut self) -> anyhow::Result<()> {
        self.phase = NativeIntentApplyPhase::Failed;
        self.ready.operation.settle_motion()
    }
    pub(crate) fn begin_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.phase == NativeIntentApplyPhase::Inputs,
            "native apply close cannot retry"
        );
        self.phase = NativeIntentApplyPhase::Failed;
        ensure!(
            self.authorization_close.is_none(),
            "native apply close already owns custody"
        );
        let connection = self.authorization_reader.take_for_retained_close()?;
        self.authorization_close = Some(ApplyAuthorizationClose {
            connection: Some(connection),
            future: None,
            outcome: None,
            frame_bytes: 0,
            acknowledged: false,
            resource_acknowledged: false,
            polling: false,
            #[cfg(test)]
            poll_fault: None,
        });
        check(self.deadline(), &AtomicBool::new(false))?;
        let close = self.authorization_close.as_mut().unwrap();
        let connection = close.connection.take().unwrap();
        close.future = Some(sqlx::Connection::close(connection));
        close.frame_bytes =
            std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref());
        ensure!(
            (close.frame_bytes as u64)
                .checked_mul(2)
                .is_some_and(|n| n <= AUTHORIZATION_CLOSE_STORAGE_BYTES),
            "native apply close frame exceeds its separately admitted share"
        );
        Ok(())
    }
    pub(crate) async fn await_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            !self.terminal_authorization_cleanup,
            "terminal reader cleanup grants no authorization continuation"
        );
        ensure!(
            self.phase == NativeIntentApplyPhase::Failed,
            "native apply close phase differs"
        );
        let close = self
            .authorization_close
            .as_mut()
            .context("native apply close missing")?;
        ensure!(close.outcome.is_none(), "native apply close cannot retry");
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
        self.authorization_reader.mark_transferred_close_joined()?;
        close.resource_acknowledged = true;
        check(self.deadline(), &AtomicBool::new(false))?;
        self.authorization_close.as_mut().unwrap().acknowledged = true;
        self.phase = NativeIntentApplyPhase::Inputs;
        Ok(())
    }
}
impl ReadyNativePendingBootstrap96 {
    /// Terminal settlement borrows the exact retained Motion owner. No native
    /// handoff, retry, new reader or source authority is produced.
    pub fn settle_motion(&mut self) -> anyhow::Result<()> {
        self.operation.settle_motion()
    }
    pub fn deadline(&self) -> Instant {
        self.operation.deadline()
    }
    pub fn progress(&self) -> NativeIntentApplyProgress {
        self.operation.progress()
    }
    pub fn intent_commit_state(&self) -> NativeIntentCommitState {
        self.operation.intent_commit_state()
    }
}
impl NativeApplyAdmission {
    fn checkpoint(&self, point: u8, cancel: &AtomicBool) -> anyhow::Result<()> {
        #[cfg(test)]
        if let Some((wanted, mode)) = self.fault {
            if wanted == point {
                match mode {
                    1 => panic!("retained native apply checkpoint {point}"),
                    2 => cancel.store(true, Ordering::Release),
                    _ => anyhow::bail!("retained native apply checkpoint {point}"),
                }
            }
        }
        let _ = point;
        check(self.deadline, cancel)
    }
}
impl NativeIntentAdmission {
    pub(crate) fn verify_apply_in(&self, db: &Connection) -> anyhow::Result<()> {
        let intent = self
            .intent
            .as_ref()
            .context("native apply intent missing")?;
        verify_intent_writer(
            db,
            intent,
            self.raw.as_ref().context("native apply raw missing")?,
        )?;
        ensure!(
            matches!(&self.native, NativeOwner::Prepared(_)),
            "native apply Prepared custody differs"
        );
        ensure!(
            journal::record(db, "command", &self.id)?.is_none()
                && journal::record(db, "ack", &self.id)?.is_none(),
            "native apply cannot follow bootstrap command or ACK"
        );
        check(self.deadline, &AtomicBool::new(false))
    }
    pub(crate) fn admit_apply_in(
        &mut self,
        db: &Connection,
        storage_bytes: u64,
        backup_parent: Option<&Path>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(deadline, cancel)?;
        let external = self
            .allocation
            .external()
            .checked_add(storage_bytes)
            .context("native apply external overflow")?;
        let reservation = self.allocation.advance(db, external)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            anyhow::bail!("native apply Prepared owner missing");
        };
        native.refresh_external_reservation(reservation)?;
        native.admit_apply_operation(backup_parent, deadline)?;
        check(deadline, cancel)
    }
    pub(crate) fn preflight_apply_in(
        &mut self,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        check(self.deadline, cancel)?;
        let intent = self
            .intent
            .as_ref()
            .context("native apply intent missing")?;
        let reservation = self.allocation.reservation(db, Some(&intent.plan), None)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            anyhow::bail!("native apply Prepared owner missing");
        };
        native.refresh_external_reservation(reservation)?;
        check(self.deadline, cancel)
    }
    pub(crate) fn apply_in(
        &mut self,
        db: &Connection,
        admission: &mut NativeApplyAdmission,
        progress: &mut NativeIntentApplyProgress,
        cancel: &AtomicBool,
        workspace: Option<&mut catabolic_store::migration::offline_bootstrap::OfflineControlWorkspaceBacking>,
    ) -> anyhow::Result<()> {
        admission.checkpoint(1, cancel)?;
        // No full scan after the host's final temporal fence. Refresh the SAME
        // Prepared/context before its move: OfflineApplyOperation has no refresh API.
        let external = self.allocation.external();
        let reservation = self.allocation.advance(db, external)?;
        let NativeOwner::Prepared(native) = &mut self.native else {
            anyhow::bail!("native apply Prepared owner missing");
        };
        native.refresh_external_reservation(reservation)?;
        match std::mem::replace(&mut self.native, NativeOwner::Empty) {
            NativeOwner::Prepared(native) => {
                self.native = match native.begin_apply() {
                    Ok(operation) => NativeOwner::Applying(operation),
                    Err(failure) => NativeOwner::Prepared(failure.into_prepared()),
                };
            }
            other => {
                self.native = other;
                anyhow::bail!("native apply move source differs");
            }
        }
        ensure!(
            matches!(&self.native, NativeOwner::Applying(_)),
            "native apply storage was not admitted"
        );
        admission.checkpoint(2, cancel)?;
        *progress = NativeIntentApplyProgress::ApplyAttempted;
        let NativeOwner::Applying(native) = &mut self.native else {
            unreachable!("installed native apply owner")
        };
        if let Some(workspace) = workspace {
            // The private Motion qualifier retains exclusive actual control
            // ownership. Native validates counted backing and exact context.
            native.apply_with_control_workspace(cancel, workspace)?;
        } else {
            // Large/unqualified cohorts retain the original full32 charge.
            native.apply(cancel)?;
        }
        // Actual successful producer result installs before any subsequent
        // checkpoint/validation. Both finish branches retain the actual owner.
        match std::mem::replace(&mut self.native, NativeOwner::Empty) {
            NativeOwner::Applying(native) => {
                self.native = match native.finish() {
                    Ok(native) => NativeOwner::Applied(native),
                    Err(failure) => NativeOwner::ApplyFailed(failure),
                };
            }
            other => {
                self.native = other;
                anyhow::bail!("native apply finish source differs");
            }
        }
        let NativeOwner::Applied(native) = &mut self.native else {
            anyhow::bail!("native apply retained opaque finish failure");
        };
        *progress = NativeIntentApplyProgress::AppliedPendingBootstrap;
        admission.checkpoint(3, cancel)?;
        let intent = self.intent.as_ref().unwrap();
        ensure!(
            native.approved_plan() == &intent.plan
                && native.catalog_id() == intent.from.catalog_id
                && native.incarnation() == intent.from.incarnation,
            "retained applied native owner differs from durable intent"
        );
        native.refresh_external_reservation(self.allocation.reservation(
            db,
            Some(&intent.plan),
            None,
        )?)?;
        admission.checkpoint(4, cancel)
    }
}

#[cfg(test)]
#[path = "apply_operation_tests.rs"]
mod tests;
