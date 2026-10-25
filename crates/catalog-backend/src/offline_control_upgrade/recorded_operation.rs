//! Private fixed recorded93→97 owner, before any native96/bootstrap handoff.
//! The host must retain genuine source/native/service ownership independently.
//! Borrowed work never takes that custody or the exact staged files out of here.
use super::original::Original93Preservation;
use super::record::{RecordAnchor, RecordDirectory};
use super::recovery::RetainedApplyProgress;
use super::*;
use crate::motion_snapshot::source_reader::RestrictedReaderOwner;
use crate::native_bootstrap::TargetAuthorizationReader;
use catabolic_store::migration::offline_bootstrap::OfflineControlWorkspaceBacking;
#[path = "recorded_workspace.rs"]
mod workspace;
#[path = "recorded_source_workspace.rs"]
mod source_workspace;
pub(crate) use source_workspace::SourceWorkspaceQualification;
use std::future::Future;
use std::io::{Seek, SeekFrom};
use std::pin::Pin;
use std::time::{Duration, Instant};

/// Descriptive only. In-flight phases after a dropped future/unwind are terminal
/// for execution; no retry, replacement record or fresh timestamp is permitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordedControlPhase {
    Inputs,
    Preparing,
    Prepared93,
    AuthorizationAdmitting,
    AuthorizationClosing,
    AuthorizationJoined93,
    Preflighting97,
    Preflighted93,
    Applying97,
    PendingCommit,
    Committing97,
    Committed97,
    Reacquiring97,
    Ready97,
    Failed,
    Settling,
    Settled,
}
/// A failed COMMIT is not interpreted as either a commit or a rollback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordedCommitState {
    #[default]
    NotAttempted,
    CommitAttempted,
    Committed97,
    WriterReacquired,
}

type ControlAuthorizationCloseFuture =
    Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send + 'static>>;
// A distinct mandatory share of the unchanged 256MiB cohort, not optional app
// authorization slack or native96's later close share. It admits SQLx's opaque
// box plus simultaneous construction/move/setup frames. SQLx exposes the actual
// frame only after allocation, so measuring it can refuse while retaining the
// box, not prove a hard allocation peak or count its worker/channel internals.
const CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES: u64 = 64 * 1024;
/// Dedicated additive application slot, never inferred from generic H.
struct ControlAuthorizationAdmission {
    bytes: u64,
    ready: bool,
}
/// Owned by the actual recorded97 operation before any poll, never by the token
/// or waiter. SQLx may internally drop its connection/receiver on WorkerCrashed;
/// retaining that opaque error cannot reconstruct them or certify a joined close.
struct ControlAuthorizationClose {
    connection: Option<sqlx::SqliteConnection>,
    future: Option<ControlAuthorizationCloseFuture>,
    outcome: Option<Result<(), sqlx::Error>>,
    frame_bytes: usize,
    acknowledged: bool,
    resource_acknowledged: bool,
    polling: bool,
    #[cfg(test)]
    poll_fault: Option<u8>,
}

/// An inline operation, never a task result or global recovery sink. `begin`
/// only moves incoming descriptors. `prepare` admits all newly owned path and
/// linkage copies before creating them. No Drop implementation performs work.
/// Actual service-owner destruction and downstream legacy96 consumption are
/// outside this unactivated slice, not a claim of completed settlement.
pub struct RecordedControlOperation {
    file: Option<File>,
    record_parent: Option<File>,
    witness: Option<StageWitness>,
    db: Option<Connection>,
    reference: Option<UpgradeReference>,
    original: Option<Original93Preservation>,
    directory: Option<RecordDirectory>,
    anchor: Option<RecordAnchor>,
    before: Option<RetainedRows>,
    prepared: Option<PreparedRecordedControlUpgrade>,
    progress: RetainedApplyProgress,
    authorization_reader: RestrictedReaderOwner,
    authorization_close: Option<ControlAuthorizationClose>,
    terminal_authorization_cleanup: bool,
    authorization_admission: Option<ControlAuthorizationAdmission>,
    phase: RecordedControlPhase,
    deadline: Option<Instant>,
    retained: u64,
    // Born with the original named64 cohort, never inferred from external H.
    // Remains in this actual owner through errors and the opaque source tail.
    control_workspace: Option<OfflineControlWorkspaceBacking>,
    // Private single-use proof for this owner's immutable Ready97 cohort.
    workspace_qualification: Option<workspace::ControlWorkspaceQualification>,
    #[cfg(test)]
    cancel_before_ready: bool,
    #[cfg(test)]
    pause_after_open: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
}
/// Ready97 is a pure move of the whole owner, including original record-parent
/// FD/reference/preservation. Its optional native admission is a fixed retained
/// preparation bridge, never legacy consumption, raw SQL or paired95 authority.
pub struct RecordedAdmittedControl {
    operation: RecordedControlOperation,
    native_admission_attempted: bool,
    native_admission: Option<crate::native_bootstrap::prepare_operation::NativeIntentAdmission>,
}

impl RecordedControlOperation {
    pub fn begin(file: File, record_parent: File) -> Self {
        Self {
            file: Some(file),
            record_parent: Some(record_parent),
            witness: None,
            db: None,
            reference: None,
            original: None,
            directory: None,
            anchor: None,
            before: None,
            prepared: None,
            progress: RetainedApplyProgress::default(),
            authorization_reader: RestrictedReaderOwner::default(),
            authorization_close: None,
            terminal_authorization_cleanup: false,
            authorization_admission: None,
            phase: RecordedControlPhase::Inputs,
            deadline: None,
            retained: 0,
            control_workspace: None,
            workspace_qualification: None,
            #[cfg(test)]
            cancel_before_ready: false,
            #[cfg(test)]
            pause_after_open: None,
        }
    }
    pub fn phase(&self) -> RecordedControlPhase {
        self.phase
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
    /// Admission for a newly retained application authorization workspace, not
    /// reuse of the generic external cohort already supplied to preparation.
    /// The caller computes its fixed layout/current-reader-close reservation.
    /// This named slot is charged before constructing or moving that workspace.
    /// A marked attempt cannot use legacy apply, even if admission fails.
    pub fn admit_retained_authorization(
        &mut self,
        additional_host_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::Prepared93
                && self.authorization_close.is_none()
                && self.authorization_admission.is_none()
                && !self.authorization_reader.is_claimed(),
            "control application authorization admission cannot retry"
        );
        self.phase = RecordedControlPhase::AuthorizationAdmitting;
        self.authorization_admission = Some(ControlAuthorizationAdmission {
            bytes: additional_host_bytes,
            ready: false,
        });
        let result = (|| {
            self.check_before_work(cancel)?;
            let prepared = &mut self.prepared.as_mut().unwrap().prepared;
            let retained = prepared
                .retained_pair_bytes
                .checked_add(additional_host_bytes)
                .context("control application authorization storage overflow")?;
            let original_shares = prepared
                .witness
                .initial_size
                .checked_mul(3)
                .context("control application authorization original shares overflow")?;
            admit_memory(Some(&prepared.db), retained, original_shares)?;
            prepared.retained_pair_bytes = retained;
            self.retained = self.retained.max(retained);
            self.check_before_work(cancel)?;
            self.authorization_admission.as_mut().unwrap().ready = true;
            Ok(())
        })();
        self.phase = if result.is_ok() {
            RecordedControlPhase::Prepared93
        } else {
            RecordedControlPhase::Failed
        };
        result
    }
    /// Pure move precondition referencing only the separately admitted slot.
    /// A large unrelated H, failed/absent admission or advanced phase is not a
    /// workspace grant. This is storage provenance, not application authority.
    pub fn retained_authorization_storage_covers(&self, required: u64) -> bool {
        self.phase == RecordedControlPhase::Prepared93
            && !self.authorization_reader.is_claimed()
            && self
                .authorization_admission
                .as_ref()
                .is_some_and(|slot| slot.ready && slot.bytes >= required)
    }
    pub fn commit_state(&self) -> RecordedCommitState {
        self.progress.commit
    }
    /// Descriptive evidence progress, never proof of database completion.
    pub fn publication_phase(&self) -> Option<&'static str> {
        self.progress
            .publication
            .as_ref()
            .map(|publication| publication.phase())
    }

    /// Fixed owner, final wrapper/result and preparation move slots. A genuine
    /// host may include this named amount before allocating its service slot.
    pub const fn inline_storage_reservation() -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<RecordedAdmittedControl>()
            + std::mem::size_of::<Result<RecordedAdmittedControl, Self>>()
            + std::mem::size_of::<PreparedRecordedControlUpgrade>()
            + std::mem::size_of::<PreparedControlUpgrade>()
            + std::mem::size_of::<StageWitness>()
            + std::mem::size_of::<RecordAnchor>()
            + std::mem::size_of::<ControlAuthorizationClose>()
            + std::mem::size_of::<ControlAuthorizationAdmission>()
            + std::mem::size_of::<ControlAuthorizationCloseFuture>()
            + std::mem::size_of::<Result<(), anyhow::Error>>()
            + std::mem::size_of::<Result<(), sqlx::Error>>()
            + std::mem::size_of::<crate::native_bootstrap::TargetAuthorizationReader<'static>>()
            + std::mem::size_of::<
                Result<crate::native_bootstrap::TargetAuthorizationReader<'static>, anyhow::Error>,
            >()
            + 3 * std::mem::size_of::<std::fs::Metadata>()) as u64
    }

    /// No filesystem call or owned normalization occurs in this preflight.
    /// The original five path-sized terms cover witness, authorization and
    /// verification scratch, including simultaneous canonical writer/main-reader
    /// paths. Four additive terms independently cover the resource path, owned
    /// connect-options filename, SQLx EstablishParams worker CString (+NUL),
    /// and the temporary String/CString construction overlap.
    /// Those persistent/acquisition copies never reuse verification scratch.
    /// The two anchor copies are retained linkage and the temporary Recording
    /// used by the existing closed authorization reader. Publication is admitted
    /// separately from its actual canonical size immediately before its clones.
    fn storage(path: &Path, operation_id: &str, pair: &str) -> anyhow::Result<u64> {
        ensure!(
            path.is_absolute(),
            "recorded control requires retained absolute path"
        );
        ensure!(
            !path
                .as_os_str()
                .as_encoded_bytes()
                .split(|b| *b == b'/')
                .any(|part| part == b"." || part == b".."),
            "recorded control refuses lexical path aliases"
        );
        ensure!(
            !operation_id.is_empty()
                && operation_id.len() <= 256
                && !operation_id.chars().any(char::is_control)
                && pair.len() == 64
                && pair
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "recorded control linkage exceeds exact contract"
        );
        let path_bytes = u64::try_from(path.as_os_str().len())?;
        // Anchor has operation ID and four fixed SHA256 strings. Witness and
        // prepared binding each retain one additional fixed digest.
        Self::inline_storage_reservation()
            .checked_add(CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES)
            .and_then(|n| n.checked_add(RestrictedReaderOwner::opening_frame_reservation()))
            .and_then(|n| n.checked_add(path_bytes.checked_add(1)?.checked_mul(9)?))
            .and_then(|n| n.checked_add((operation_id.len() as u64 + 4 * 64) * 2))
            .and_then(|n| n.checked_add(2 * 64))
            .context("recorded control storage accounting overflow")
    }

    /// In-flight is installed synchronously, even if the returned future is
    /// never polled. Incoming staged/parent files stay here on every error,
    /// panic or future drop. The selected original FD remains caller-borrowed.
    /// `external` must include the caller's retained source/service owners and
    /// any separate authorization/query cohort. This operation does not admit
    /// an already allocated external service slot retroactively. The caller's
    /// absolute deadline is captured now, capped by the existing 60-second limit,
    /// and is never renewed on first poll or a repeated preparation attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare<'a>(
        &'a mut self,
        path: &'a Path,
        original: &'a mut File,
        inspected: &'a crate::motion_snapshot::image_read::InspectedImage,
        operation_id: &'a str,
        pair: &'a str,
        facts: &'a OfflineControlShutdownFacts,
        external: u64,
        caller_deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + 'a {
        let start = self.phase == RecordedControlPhase::Inputs;
        if start {
            self.phase = RecordedControlPhase::Preparing;
            // Capture the caller's existing absolute limit before returning a
            // future. Queueing/unpolled time consumes it; retry cannot refresh it.
            self.deadline = Some(caller_deadline.min(Instant::now() + Duration::from_secs(60)));
        }
        async move {
            ensure!(start, "recorded control preparation cannot retry");
            let result = self
                .prepare_inner(
                    path,
                    original,
                    inspected,
                    operation_id,
                    pair,
                    facts,
                    external,
                    cancel,
                )
                .await;
            self.phase = if result.is_ok() {
                RecordedControlPhase::Prepared93
            } else {
                RecordedControlPhase::Failed
            };
            result
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_inner(
        &mut self,
        path: &Path,
        original: &mut File,
        inspected: &crate::motion_snapshot::image_read::InspectedImage,
        operation_id: &str,
        pair: &str,
        facts: &OfflineControlShutdownFacts,
        external: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        let deadline = self.deadline.context("recorded control deadline missing")?;
        check(cancel)?;
        check_deadline(deadline)?;
        ensure!(
            facts.service_admission_stopped
                && facts.native_workers_joined
                && facts.application_owners_drained
                && facts.exclusive_operational_ownership_retained,
            "recorded control requires genuine retained shutdown custody"
        );
        let storage = Self::storage(path, operation_id, pair)?;
        let retained = external
            .checked_add(storage)
            .context("recorded control external overflow")?;
        let charge = inspected
            .bytes()
            .checked_mul(3)
            .context("recorded original image overflow")?;
        ensure!(
            inspected.bytes() > 0
                && inspected.bytes() <= 56 * 1024 * 1024
                && inspected.schema_version() == 93,
            "recorded control requires bounded original93"
        );
        check(cancel)?;
        check_deadline(deadline)?;
        self.control_workspace = Some(OfflineControlWorkspaceBacking::new());
        admit_memory(None, retained, charge)?;
        self.retained = retained;
        // The synchronously captured deadline covers preparation, authorization
        // and apply. Existing inner scanner deadlines cannot extend this grant.
        // No canonicalization. Physical aliases are unsupported: each borrowed
        // prefix must itself be nonsymlink. Supported host locks exclude rename;
        // this is not protection against hostile same-UID namespace substitution.
        for prefix in path.ancestors() {
            check(cancel)?;
            check_deadline(deadline)?;
            let metadata = std::fs::symlink_metadata(prefix)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "recorded control refuses physical path aliases"
            );
        }
        check(cancel)?;
        check_deadline(deadline)?;
        let file = self
            .file
            .as_ref()
            .context("recorded stage descriptor missing")?;
        let held = file.metadata()?;
        let named = std::fs::symlink_metadata(path)?;
        ensure!(
            held.is_file()
                && named.is_file()
                && held.nlink() == 1
                && named.nlink() == 1
                && held.len() == inspected.bytes()
                && named.len() == held.len()
                && (held.dev(), held.ino()) == (named.dev(), named.ino()),
            "recorded control stage differs from original descriptor"
        );
        let parent_identity = RecordDirectory::validate_retained(
            self.record_parent
                .as_ref()
                .context("recorded parent descriptor missing")?,
        )?;
        // All owned inputs are allocated only after the borrowed admission.
        let path = path.to_owned();
        let initial_sha = inspected.sha256().to_owned();
        let file = self
            .file
            .take()
            .expect("validated retained stage descriptor");
        self.witness = Some(StageWitness {
            file,
            path,
            device: held.dev(),
            inode: held.ino(),
            initial_size: held.len(),
            initial_sha,
            record: None,
        });
        let parent = self
            .record_parent
            .take()
            .expect("validated retained parent descriptor");
        self.directory = Some(RecordDirectory::from_validated(parent, parent_identity));
        self.reference = Some(UpgradeReference::build().await?);
        check(cancel)?;
        check_deadline(deadline)?;
        original.seek(SeekFrom::Start(0))?;
        self.original = Some(super::original::capture(
            self.reference.as_ref().unwrap(),
            original,
            inspected,
            retained,
            cancel,
        )?);
        let original = self.original.as_ref().unwrap();
        self.anchor = Some(RecordAnchor {
            operation_id: operation_id.to_owned(),
            selected_pair_sha256: pair.to_owned(),
            original93_image_sha256: original.image_sha256().to_owned(),
            original93_inventory_sha256: original.inventory_sha256().to_owned(),
            stage_device: held.dev(),
            stage_inode: held.ino(),
            frozen_binding_sha256: original.frozen_binding_sha256().to_owned(),
        });
        let witness = self.witness.as_ref().unwrap();
        witness.verify(true)?;
        check(cancel)?;
        check_deadline(deadline)?;
        // The opened connection is installed before pragmas, BEGIN or any other
        // fallible step. Cancellation of a later await retains this exact owner.
        self.db = Some(Connection::open_with_flags(
            &witness.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?);
        let db = self.db.as_ref().unwrap();
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "cache_size", -2048)?;
        admit_memory(Some(db), retained, witness.initial_size)?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "recursive_triggers", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.progress_handler(10000, Some(move || Instant::now() >= deadline))?;
        witness.verify(true)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        #[cfg(test)]
        if let Some(entered) = &self.pause_after_open {
            entered.store(
                unsafe { self.db.as_ref().unwrap().handle() } as usize,
                Ordering::Release,
            );
            std::future::pending::<()>().await;
        }

        let reference = self.reference.as_ref().unwrap();
        ensure!(
            reference.0.verify_in(db)? == ControlVersion::Retained93,
            "recorded control preparation requires exact93"
        );
        crate::source_freeze::verify_sync(db)?;
        verify_binding(db, original.frozen_binding_sha256())?;
        integrity(db)?;
        let recorded = recovery::Recording {
            original,
            directory: self.directory.as_ref().unwrap(),
            anchor: self.anchor.as_ref().unwrap().clone(),
        };
        recorded.verify_original(witness, db, cancel)?;
        self.before = Some(RetainedRows::capture(db, cancel)?);
        check(cancel)?;
        check_deadline(deadline)?;
        db.progress_handler(0, None::<fn() -> bool>)?;
        let binding = original.frozen_binding_sha256().to_owned();
        drop(recorded);
        // Checked, moves-only assembly: no queries, callbacks or await after
        // taking any owner. Even an invariant failure restores every slot.
        match (
            self.reference.take(),
            self.original.take(),
            self.directory.take(),
            self.anchor.take(),
            self.db.take(),
            self.witness.take(),
            self.before.take(),
        ) {
            (
                Some(reference),
                Some(original),
                Some(directory),
                Some(anchor),
                Some(db),
                Some(witness),
                Some(before),
            ) => {
                self.prepared = Some(PreparedRecordedControlUpgrade {
                    reference,
                    original,
                    directory,
                    anchor,
                    prepared: PreparedControlUpgrade {
                        db,
                        witness,
                        before,
                        binding,
                        retained_pair_bytes: retained,
                        deadline,
                    },
                });
            }
            (reference, original, directory, anchor, db, witness, before) => {
                self.reference = reference;
                self.original = original;
                self.directory = directory;
                self.anchor = anchor;
                self.db = db;
                self.witness = witness;
                self.before = before;
                anyhow::bail!("recorded control preparation slots incomplete");
            }
        }
        check(cancel)?;
        check_deadline(deadline)?;
        Ok(())
    }

    /// Terminal resource cleanup only, including abandoned open/prepare/close
    /// borrowers. Failed is installed even for an unpolled waiter; durable/native
    /// facts are unchanged. Repetition observes only the same retained attempt,
    /// never a new connection, setup, authentication token or effect permission.
    /// Success includes known-never-opened attempts; it is not an effect grant.
    pub fn settle_authorization_reader(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        self.terminal_authorization_cleanup = true;
        self.phase = RecordedControlPhase::Failed;
        async move {
            if let Some(close) = self.authorization_close.as_mut() {
                if !close.resource_acknowledged {
                    TargetAuthorizationReader::settle_close(
                        &mut close.connection,
                        &mut close.future,
                        &mut close.outcome,
                        &mut close.frame_bytes,
                        &mut close.polling,
                        CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES,
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
    ) -> impl Future<Output = anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>>> + '_
    {
        crate::native_bootstrap::TargetAuthorizationReader::open_retained_control(self)
    }
    pub(crate) fn claim_authorization_reader(&mut self) -> anyhow::Result<()> {
        self.authorization_close_ready()?;
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
        self.authorization_close_ready()?;
        ensure!(
            self.authorization_reader.is_claimed(),
            "control reader was not claimed"
        );
        self.prepared
            .as_ref()
            .context("recorded control owner missing")?
            .verify_current()
    }
    pub(crate) fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        self.prepared
            .as_ref()
            .context("recorded control owner missing")?
            .authorization_path()
    }
    pub(crate) fn authorization_close_ready(&self) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::Prepared93 && self.authorization_close.is_none(),
            "recorded control authorization cannot retry or reopen"
        );
        Ok(())
    }
    /// Called only by the exclusive RetainedControl token, which checks the
    /// available phase/empty slot before moving its connection. Install custody
    /// before the first fallible step and before constructing the async waiter.
    pub(crate) fn begin_authorization_close(&mut self) -> anyhow::Result<()> {
        self.authorization_close_ready()?;
        self.phase = RecordedControlPhase::AuthorizationClosing;
        let connection = self.authorization_reader.take_for_retained_close()?;
        self.authorization_close = Some(ControlAuthorizationClose {
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
        self.check_before_work(&AtomicBool::new(false))?;
        let close = self.authorization_close.as_mut().unwrap();
        let connection = close.connection.take().unwrap();
        // SQLx returns an owning box. Store it before size inspection, including
        // when its actual frame exceeds the separately admitted 64KiB share.
        close.future = Some(sqlx::Connection::close(connection));
        close.frame_bytes =
            std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref());
        let simultaneous_frames = (close.frame_bytes as u64)
            .checked_mul(2)
            .context("control authorization close frame overflow")?;
        ensure!(
            simultaneous_frames <= CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES,
            "control authorization close frame exceeds admitted share"
        );
        Ok(())
    }
    pub(crate) async fn await_authorization_close(&mut self) -> anyhow::Result<()> {
        ensure!(
            !self.terminal_authorization_cleanup,
            "terminal reader cleanup grants no authorization continuation"
        );
        ensure!(
            self.phase == RecordedControlPhase::AuthorizationClosing,
            "control authorization close phase differs"
        );
        let close = self
            .authorization_close
            .as_mut()
            .context("control close missing")?;
        ensure!(
            close.outcome.is_none(),
            "control authorization close cannot retry"
        );
        TargetAuthorizationReader::settle_close(
            &mut close.connection,
            &mut close.future,
            &mut close.outcome,
            &mut close.frame_bytes,
            &mut close.polling,
            CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES,
            #[cfg(test)]
            &mut close.poll_fault,
        )
        .await?;
        self.authorization_reader.mark_transferred_close_joined()?;
        close.resource_acknowledged = true;
        self.check_before_work(&AtomicBool::new(false))?;
        self.authorization_close.as_mut().unwrap().acknowledged = true;
        self.phase = RecordedControlPhase::AuthorizationJoined93;
        Ok(())
    }
    fn check_before_work(&self, cancel: &AtomicBool) -> anyhow::Result<()> {
        check(cancel)?;
        check_deadline(self.deadline.context("recorded control deadline missing")?)?;
        check_deadline(
            self.prepared
                .as_ref()
                .context("recorded control owner missing")?
                .prepared
                .deadline,
        )
    }
    /// Last full scan after the actual reader joins, before the application's
    /// pure current/target temporal fence. This is operator mechanics, not an
    /// application grant. Entering a failed/unwound preflight is terminal.
    pub(crate) fn preflight_for_apply(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::AuthorizationJoined93,
            "control preflight requires its own joined reader"
        );
        self.phase = RecordedControlPhase::Preflighting97;
        let result = (|| {
            self.check_before_work(cancel)?;
            self.prepared
                .as_ref()
                .unwrap()
                .verify_current_cancelled(cancel)
        })();
        self.phase = if result.is_ok() {
            RecordedControlPhase::Preflighted93
        } else {
            RecordedControlPhase::Failed
        };
        result
    }
    /// No full preflight here: only bounded local/DBSTATUS checks precede DDL.
    /// Fixed migrations, full postscans and durable publication end at pending
    /// COMMIT, allowing a second fresh pure app temporal fence before COMMIT.
    pub(crate) fn apply_pending(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::Preflighted93,
            "control effects require completed joined preflight"
        );
        self.phase = RecordedControlPhase::Applying97;
        let result = (|| {
            self.check_before_work(cancel)?;
            self.prepared
                .as_mut()
                .unwrap()
                .apply_effects_retained(cancel, &mut self.progress)
        })();
        self.phase = if result.is_ok() {
            RecordedControlPhase::PendingCommit
        } else {
            RecordedControlPhase::Failed
        };
        result
    }
    /// Scan-free continuation. The caller's fresh temporal fence precedes this
    /// method, not the migration scans/publication above. COMMIT may block and a
    /// failed attempt retains an honest CommitAttempted state, never Ready97.
    pub(crate) fn commit_pending(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::PendingCommit,
            "control commit requires pending effects"
        );
        self.phase = RecordedControlPhase::Committing97;
        let result = (|| {
            self.check_before_work(cancel)?;
            self.prepared
                .as_mut()
                .unwrap()
                .commit_retained(&mut self.progress)
        })();
        self.phase = if result.is_ok() {
            RecordedControlPhase::Committed97
        } else {
            RecordedControlPhase::Failed
        };
        result
    }
    /// COMMIT and reacquisition are deliberately separate. Failure here keeps
    /// committed97 evidence, the original writer and all publication owners.
    pub(crate) fn finish_committed(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::Committed97,
            "control finish requires committed97"
        );
        self.phase = RecordedControlPhase::Reacquiring97;
        let result = (|| {
            self.check_before_work(cancel)?;
            self.prepared
                .as_mut()
                .unwrap()
                .reacquire_and_verify_retained(cancel, &mut self.progress)?;
            #[cfg(test)]
            if self.cancel_before_ready {
                cancel.store(true, Ordering::Release);
            }
            self.check_before_work(cancel)
        })();
        self.phase = if result.is_ok() {
            RecordedControlPhase::Ready97
        } else {
            RecordedControlPhase::Failed
        };
        result
    }
    /// Legacy operator composite, never the application's retained continuation.
    /// Closing/joined/in-flight/failed states cannot enter or revive this path.
    pub fn apply(&mut self, cancel: &AtomicBool) -> anyhow::Result<()> {
        ensure!(
            self.phase == RecordedControlPhase::Prepared93
                && self.authorization_admission.is_none()
                && !self.authorization_reader.is_claimed(),
            "recorded control apply cannot retry or bypass application admission"
        );
        self.phase = RecordedControlPhase::Applying97;
        let result = (|| {
            self.prepared
                .as_mut()
                .context("recorded control owner missing")?
                .apply_retained(cancel, &mut self.progress)?;
            #[cfg(test)]
            if self.cancel_before_ready {
                cancel.store(true, Ordering::Release);
            }
            // Post-COMMIT verification can consume the remaining deadline.
            // Refusal preserves committed progress but never produces Ready97.
            check(cancel)?;
            check_deadline(self.deadline.context("recorded control deadline missing")?)
        })();
        self.phase = if result.is_ok() {
            RecordedControlPhase::Ready97
        } else {
            RecordedControlPhase::Failed
        };
        result
    }
    pub fn into_admitted(self) -> Result<RecordedAdmittedControl, Self> {
        if self.phase == RecordedControlPhase::Ready97 {
            Ok(RecordedAdmittedControl {
                operation: self,
                native_admission_attempted: false,
                native_admission: None,
            })
        } else {
            Err(self)
        }
    }

    /// Explicit transaction settlement only, not deletion, service close or
    /// owner extraction. Every file/reference and the exact Connection remain
    /// reachable afterward; durable names are never removed. Failed settlement
    /// stays Settling and cannot yield Ready97. No consuming Connection::close.
    pub fn settle(&mut self) -> anyhow::Result<()> {
        self.workspace_qualification = None;
        if self.phase == RecordedControlPhase::Settled {
            return Ok(());
        }
        self.phase = RecordedControlPhase::Settling;
        let db = self
            .prepared
            .as_ref()
            .map(|p| &p.prepared.db)
            .or(self.db.as_ref());
        if let Some(db) = db {
            db.progress_handler(0, None::<fn() -> bool>)?;
            set_ledger(db, false)?;
            if !db.is_autocommit() {
                db.execute_batch("ROLLBACK")?;
            }
            ensure!(
                db.is_autocommit()
                    && unsafe { ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) } == 0,
                "recorded control transaction settlement incomplete"
            );
        }
        self.phase = RecordedControlPhase::Settled;
        Ok(())
    }
}
impl RecordedAdmittedControl {
    /// Exact two record verification buffers for the later admitted close.
    /// No I/O, authority, allocation or observation of a different owner.
    pub(crate) fn source_close_verification_bytes(&self) -> anyhow::Result<u64> {
        self.operation
            .prepared
            .as_ref()
            .context("close destination requires retained prepared control")?
            .prepared
            .witness
            .close_verification_bytes()
    }
    /// Borrowed pre-admission, terminal before work. The original97 shares are
    /// preserved explicitly; its `retained` scalar alone excludes those shares.
    /// Current Motion SQLite costs are counted once by native host accounting.
    pub fn admit_native_preparation(
        &mut self,
        native: &Path,
        q: catabolic_store::migration::NativeUpgradeQuiescence,
        id: &str,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.admit_native_preparation_reserved(native, q, id, 0, caller_deadline, cancel)
    }
    /// Additional bytes are newly retained application slots/cohorts only,
    /// never a fresh aggregate budget or a replacement for inherited97 H.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_native_preparation_reserved(
        &mut self,
        native: &Path,
        q: catabolic_store::migration::NativeUpgradeQuiescence,
        id: &str,
        additional_host_bytes: u64,
        caller_deadline: Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            !self.native_admission_attempted,
            "native intent admission cannot retry"
        );
        self.native_admission_attempted = true;
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native intent requires genuine Ready97"
        );
        let deadline = self
            .operation
            .deadline
            .context("Ready97 deadline missing")?
            .min(caller_deadline);
        check(cancel)?;
        check_deadline(deadline)?;
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        // apply_retained raises this sticky total before publication clones;
        // operation.retained is only the earlier preparation high-water. The
        // whole publication owner remains alive in operation.progress here.
        ensure!(
            held.prepared.retained_pair_bytes >= self.operation.retained,
            "Ready97 retained publication high-water decreased"
        );
        let retained97_without_workspace = held
            .prepared
            .retained_pair_bytes
            .checked_add(
                held.prepared
                    .witness
                    .initial_size
                    .checked_mul(3)
                    .context("Ready97 original shares overflow")?,
            )
            .and_then(|n| n.checked_add(additional_host_bytes))
            .and_then(|n| {
                n.checked_add((held.prepared.witness.path.as_os_str().len() as u64).checked_mul(4)?)
            })
            .context("Ready97 continuing retention overflow")?;
        self.native_admission = Some(
            crate::native_bootstrap::prepare_operation::NativeIntentAdmission::admit(
                &held.prepared.db,
                retained97_without_workspace,
                self.operation.control_workspace.as_mut()
                    .context("original control workspace backing missing")?,
                deadline,
                native,
                q,
                id,
                additional_host_bytes,
                cancel,
            )?,
        );
        Ok(())
    }
    /// Moves only. Without the opaque borrowed admission, returns this exact
    /// Ready97 unchanged, including every original descriptor and reference.
    pub fn begin_native_preparation(self) -> Result<NativeIntentPreparationOperation, Self> {
        self.begin_native_preparation_reserved(0)
    }
    /// Moves-only enforcement of the application's already admitted slots.
    pub fn begin_native_preparation_reserved(
        mut self,
        required_host_bytes: u64,
    ) -> Result<NativeIntentPreparationOperation, Self> {
        if self.operation.phase != RecordedControlPhase::Ready97
            || !self
                .native_admission
                .as_ref()
                .is_some_and(|admitted| admitted.covers_host_storage(required_host_bytes))
        {
            return Err(self);
        }
        match self.native_admission.take() {
            Some(admission) => Ok(NativeIntentPreparationOperation::from_admitted(
                self, admission,
            )),
            None => Err(self),
        }
    }
    #[cfg(test)]
    pub(crate) fn control_workspace_qualified_for_test(&self) -> Option<bool> {
        self.operation.workspace_qualification.as_ref().map(|q| q.eligible())
    }
    #[cfg(test)]
    pub(crate) fn enlarge_control_capacity_for_test(&mut self) {
        assert!(self.operation.workspace_qualification.is_none());
        self.operation.prepared.as_mut().unwrap().prepared.witness.initial_sha
            .reserve(OfflineControlWorkspaceBacking::RETAINED_LIMIT as usize);
    }
    pub(crate) fn native_intent_authorization_path(&self) -> anyhow::Result<PathBuf> {
        self.operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?
            .prepared
            .authorization_path()
    }
    pub(crate) fn verify_native_intent(
        &self,
        admitted: &crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native intent lost Ready97"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        held.prepared.witness.verify(false)?;
        ensure!(
            held.reference.0.verify_in(&held.prepared.db)? == ControlVersion::Complete97,
            "native intent requires exact ordered97"
        );
        admitted.verify_in(&held.prepared.db)
    }
    pub(crate) fn commit_native_intent(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native intent lost Ready97"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.commit_intent_in(&held.prepared.db, cancel)
    }
    pub(crate) fn verify_native_apply(
        &self,
        admitted: &crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
    ) -> anyhow::Result<()> {
        self.verify_native_intent(admitted)?;
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.verify_apply_in(&held.prepared.db)
    }
    pub(crate) fn admit_native_apply(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        storage_bytes: u64,
        backup_parent: Option<&Path>,
        deadline: std::time::Instant,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        self.verify_native_apply(admitted)?;
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.admit_apply_in(
            &held.prepared.db,
            storage_bytes,
            backup_parent,
            deadline,
            cancel,
        )
    }
    pub(crate) fn preflight_native_apply(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        // A failed/re-entered preflight cannot retain an earlier qualification.
        self.operation.workspace_qualification = None;
        admitted.check_before_work(cancel)?;
        self.verify_native_apply(admitted)?;
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.preflight_apply_in(&held.prepared.db, cancel)?;
        // Traverse actual retained capacities BEFORE the app's final genuine
        // current+target temporal checks. These private cohort owners cannot
        // mutate in Ready97: later settlement changes phase and source-tail
        // extraction follows bootstrap. No detached public scalar escapes.
        let qualification = workspace::ControlWorkspaceQualification::measure(&self.operation)?;
        admitted.check_before_work(cancel)?;
        self.operation.workspace_qualification = Some(qualification);
        Ok(())
    }
    pub(crate) fn apply_native_intent(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        apply: &mut crate::native_bootstrap::prepare_operation::apply_operation::NativeApplyAdmission,
        progress: &mut crate::native_bootstrap::prepare_operation::apply_operation::NativeIntentApplyProgress,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native apply lost original97 custody"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        // Consume the same owner's private preflight proof, with no traversal
        // after the app's genuine temporal fence. This exclusive view holds all
        // actual cohort owners through the fixed call, including error/unwind.
        let _ = held;
        match workspace::QualifiedControlWorkspace::borrow(&mut self.operation)? {
            workspace::ControlWorkspaceUse::Qualified(workspace) => {
                workspace.apply(admitted, apply, progress, cancel)
            }
            workspace::ControlWorkspaceUse::FullCharge(operation) => {
                let held = operation.prepared.as_ref().context("Ready97 owner missing")?;
                admitted.apply_in(&held.prepared.db, apply, progress, cancel, None)
            }
        }
    }
    pub(crate) fn insert_native_intent(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native intent lost Ready97"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.write_intent_in(&held.prepared.db, cancel)
    }
    pub(crate) fn preflight_native_intent(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        admitted.check_before_work(cancel)?;
        self.verify_native_intent(admitted)?;
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.preflight_intent_in(&held.prepared.db, cancel)
    }
    /// Fixed internal bridge, never a general callback or raw SQL accessor.
    pub(crate) fn preflight_native_preparation(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        admitted.check_before_work(cancel)?;
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native preparation lost Ready97 custody"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        held.prepared.witness.verify(false)?;
        ensure!(
            held.reference.0.verify_in(&held.prepared.db)? == ControlVersion::Complete97,
            "native preparation requires exact ordered97"
        );
        admitted.preflight_prepare_in(&held.prepared.db, cancel)
    }
    pub(crate) fn acquire_native_source_locks(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        admitted.check_before_work(cancel)?;
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native locks lost Ready97"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.acquire_source_locks_in(&held.prepared.db, cancel)
    }
    pub(crate) fn prepare_locked_native_intent(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        source: &std::fs::File,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        admitted.check_before_work(cancel)?;
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "locked native preparation lost Ready97"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.prepare_locked_in(&held.prepared.db, source, cancel)
    }
    pub(crate) fn prepare_native_intent(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        admitted.check_before_work(cancel)?;
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "native preparation lost Ready97 custody"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("Ready97 owner missing")?;
        admitted.prepare_in(&held.prepared.db, cancel)
    }

    #[cfg(test)]
    pub(crate) fn native_retention_shares_for_test(&self) -> (u64, u64, u64, u64) {
        let held = self.operation.prepared.as_ref().unwrap();
        (
            self.operation.retained,
            held.prepared.retained_pair_bytes,
            held.original.image_bytes(),
            held.prepared.witness.path.as_os_str().len() as u64,
        )
    }
    #[cfg(test)]
    pub(crate) fn zero_native_intent_busy_timeout_for_test(&self) {
        self.operation
            .prepared
            .as_ref()
            .unwrap()
            .prepared
            .db
            .busy_timeout(Duration::ZERO)
            .unwrap();
    }
    #[cfg(test)]
    pub(crate) fn native_custody_for_test(&self) -> (usize, i32, i32, i32, Instant, u64) {
        use std::os::fd::AsRawFd;
        let held = self.operation.prepared.as_ref().unwrap();
        (
            unsafe { held.prepared.db.handle() } as usize,
            held.prepared.witness.file.as_raw_fd(),
            held.directory.original_parent_fd(),
            held.prepared
                .witness
                .record
                .as_ref()
                .unwrap()
                .original_record_fd(),
            self.operation.deadline.unwrap(),
            self.operation.retained,
        )
    }
    pub fn phase(&self) -> RecordedControlPhase {
        self.operation.phase()
    }
    pub fn commit_state(&self) -> RecordedCommitState {
        self.operation.commit_state()
    }
    pub fn publication_phase(&self) -> Option<&'static str> {
        self.operation.publication_phase()
    }
    pub fn settle(&mut self) -> anyhow::Result<()> {
        self.operation.settle()
    }
}

#[cfg(test)]
#[path = "recorded_operation_tests.rs"]
mod tests;

// Fixed81 continuations. They expose neither Connection nor a caller-selected
// callback; every route retains the same original97 owner and actual writer.
impl RecordedAdmittedControl {
    pub(crate) fn admit_native_bootstrap(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "bootstrap lost original97 custody"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("bootstrap original owner missing")?;
        held.prepared.witness.verify(false)?;
        admitted.admit_bootstrap_in(&held.prepared.db, bytes, cancel)
    }
    pub(crate) fn verify_native_bootstrap(
        &self,
        admitted: &crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        work: &crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::BootstrapWork,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "bootstrap lost original97 custody"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("bootstrap original owner missing")?;
        held.prepared.witness.verify(false)?;
        ensure!(
            held.reference.0.verify_in(&held.prepared.db)? == ControlVersion::Complete97,
            "bootstrap requires exact retained97 reference"
        );
        admitted.verify_bootstrap_in(&held.prepared.db, work)
    }
    pub(crate) fn invalidate_bootstrap_workspace(&mut self) {
        self.operation.workspace_qualification = None;
    }
    pub(crate) fn native_bootstrap_step(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        work: &mut crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::BootstrapWork,
        step: crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::BootstrapStep,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "bootstrap lost original97 custody"
        );
        use crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::BootstrapStep;
        let purpose = match &step {
            BootstrapStep::Preflight(purpose) => {
                // Revoke stale proof before any fallible preflight. The actual
                // retained-capacity walk precedes all app temporal fences.
                self.operation.workspace_qualification = None;
                self.verify_native_bootstrap(admitted, work)?;
                let purpose = workspace::WorkspacePurpose::BootstrapPreflight(*purpose);
                let qualification = workspace::ControlWorkspaceQualification::measure_for(
                    &self.operation, purpose,
                )?;
                admitted.check_before_work(cancel)?;
                self.operation.workspace_qualification = Some(qualification);
                purpose
            }
            BootstrapStep::Execute => workspace::WorkspacePurpose::BootstrapExecute,
            _ => {
                self.operation.workspace_qualification = None;
                let held = self.operation.prepared.as_ref()
                    .context("bootstrap original owner missing")?;
                return admitted.bootstrap_step_in(&held.prepared.db, work, step, cancel, None);
            }
        };
        workspace::QualifiedControlWorkspace::borrow_for(&mut self.operation, purpose)?
            .bootstrap_step(admitted, work, step, cancel)
    }
    pub(crate) fn preflight_bootstrap_source(
        &mut self,
        admitted: &mut crate::native_bootstrap::prepare_operation::NativeIntentAdmission,
        work: &crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::BootstrapWork,
        cancel: &AtomicBool,
    ) -> anyhow::Result<crate::native_bootstrap::RetainedProofAdmission> {
        self.operation.workspace_qualification = None;
        ensure!(
            self.operation.phase == RecordedControlPhase::Ready97,
            "bootstrap source lost original97 custody"
        );
        // Every destination used by the final move is empty before extracting
        // the prepared owner. These checks precede native finish, not the move.
        ensure!(
            self.operation.db.is_none()
                && self.operation.original.is_none()
                && self.operation.directory.is_none()
                && self.operation.anchor.is_none()
                && self.operation.before.is_none(),
            "source tail destinations already occupied"
        );
        let held = self
            .operation
            .prepared
            .as_ref()
            .context("bootstrap source original missing")?;
        held.prepared.witness.verify(false)?;
        ensure!(
            held.reference.0.verify_in(&held.prepared.db)? == ControlVersion::Complete97,
            "source transfer reference differs"
        );
        let admission = admitted.preflight_bootstrap_source_in(&held.prepared.db, work, cancel)?;
        held.prepared.witness.verify(false)?;
        admitted.check_before_work(cancel)?;
        self.operation.phase = RecordedControlPhase::Settled;
        Ok(admission)
    }
    /// Infallible moves only after the private ReadySource preflight. The exact
    /// existing reference/witness continue, while all original/preservation/FD
    /// tails and the settled DB stay in this opaque owner, never used again.
    pub(crate) fn take_settled_source_parts(&mut self) -> (StageWitness, UpgradeReference, String) {
        self.operation.workspace_qualification = None;
        let PreparedRecordedControlUpgrade {
            reference,
            prepared,
            original,
            directory,
            anchor,
        } = self
            .operation
            .prepared
            .take()
            .expect("ReadySource prepared owner");
        let PreparedControlUpgrade {
            db,
            witness,
            before,
            binding,
            retained_pair_bytes,
            ..
        } = prepared;
        self.operation.db = Some(db);
        self.operation.before = Some(before);
        self.operation.original = Some(original);
        self.operation.directory = Some(directory);
        self.operation.anchor = Some(anchor);
        self.operation.retained = self.operation.retained.max(retained_pair_bytes);
        (witness, reference, binding)
    }
}
