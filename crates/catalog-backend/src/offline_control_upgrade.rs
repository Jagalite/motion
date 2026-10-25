//! Fixed offline93→97 control evolution. No ordinary startup or native41 grant.
mod facade;
pub use crate::native_bootstrap::prepare_operation::{
    NativeIntentCommitState, NativeIntentPreparationOperation, NativeIntentPreparationPhase,
    ReadyNativeIntent96,
};
pub use facade::{
    OfflineControlShutdownAcknowledgment, OfflineControlShutdownFacts, OfflineControlUpgrade,
    PreparedRecordedControlOperation, UpgradedControlStore, verify_ordered97_in,
};
mod recorded_operation;
pub(crate) use recorded_operation::SourceWorkspaceQualification;
pub use recorded_operation::{
    RecordedAdmittedControl, RecordedCommitState, RecordedControlOperation, RecordedControlPhase,
};
mod original;
mod record;
mod recovery;
pub(crate) use recovery::PreparedRecordedControlUpgrade;
mod reference;
mod source_session;
use crate::motion_snapshot::row_preservation::{ControlAdditions, RetainedRows};
use anyhow::{Context, ensure};
use reference::{ClosedReference, ControlVersion};
use rusqlite::{Connection, ffi, types::Value};
pub(crate) use source_session::AdmittedSourceArchiveRead;
pub use source_session::{RestoreSealSelection, SourceSessionClosePhase};
pub(crate) use source_session::{
    StageCurrentImage, StageCurrentImageFailure, StageImageCaptureOperation,
    StageRestoreDescriptorFailure, StageRestoreOperation, StageSourceSession,
};
pub(crate) use source_session::{StageSourceCloseOperation, source_close_storage};
use std::{
    ffi::CStr,
    fs::File,
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Independently compiled reference, never caller SQL or a schema override.
pub(crate) struct UpgradeReference(ClosedReference);
impl UpgradeReference {
    pub(crate) async fn build() -> anyhow::Result<Self> {
        Ok(Self(ClosedReference::build().await?))
    }
    /// The paired target owner holds the server lock, reservation and directory.
    /// This excludes supported concurrent writers; it does not promise security
    /// against hostile same-UID ABA substitution during SQLite pathname opening.
    pub(crate) fn upgrade(
        &self,
        file: File,
        path: &Path,
        expected_image: &str,
        expected_frozen_binding_sha256: &str,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<AdmittedControlStore> {
        self.upgrade_inner(
            file,
            path,
            expected_image,
            expected_frozen_binding_sha256,
            retained_pair_bytes,
            cancel,
            None,
        )
    }
    fn upgrade_inner(
        &self,
        file: File,
        path: &Path,
        expected_image: &str,
        expected_frozen_binding_sha256: &str,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
        recorded: Option<recovery::Recording<'_>>,
    ) -> anyhow::Result<AdmittedControlStore> {
        self.prepare_inner(
            file,
            path,
            expected_image,
            expected_frozen_binding_sha256,
            retained_pair_bytes,
            cancel,
            recorded.as_ref(),
        )?
        .apply(self, cancel, recorded.as_ref())
    }
    fn prepare_inner(
        &self,
        file: File,
        path: &Path,
        expected_image: &str,
        expected_frozen_binding_sha256: &str,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
        recorded: Option<&recovery::Recording<'_>>,
    ) -> anyhow::Result<PreparedControlUpgrade> {
        let witness = StageWitness::new(file, path, expected_image)?;
        admit_memory(None, retained_pair_bytes, witness.initial_size)?;
        witness.verify(true)?;
        check(cancel)?;
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.pragma_update(None, "cache_size", -2048)?;
        admit_memory(Some(&db), retained_pair_bytes, witness.initial_size)?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "recursive_triggers", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        db.progress_handler(10000, Some(move || std::time::Instant::now() >= deadline))?;
        witness.verify(true)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        let version = self.0.verify_in(&db)?;
        ensure!(
            version == ControlVersion::Retained93,
            "completed97 recovery requires original93 preservation evidence; current stage hash is not an upgrade proof"
        );
        crate::source_freeze::verify_sync(&db)?;
        verify_binding(&db, expected_frozen_binding_sha256)?;
        integrity(&db)?;
        if let Some(recorded) = &recorded {
            recorded.verify_original(&witness, &db, cancel)?;
        }
        let before = RetainedRows::capture(&db, cancel)?;
        check(cancel)?;
        check_deadline(deadline)?;
        db.progress_handler(0, None::<fn() -> bool>)?;
        Ok(PreparedControlUpgrade {
            db,
            witness,
            before,
            binding: expected_frozen_binding_sha256.to_owned(),
            retained_pair_bytes,
            deadline,
        })
    }
}

/// Validated exact93 before any94–97 effects. The same WRITE connection and
/// preservation evidence survive the authorization interval; no database escape.
struct PreparedControlUpgrade {
    db: Connection,
    witness: StageWitness,
    before: RetainedRows,
    binding: String,
    retained_pair_bytes: u64,
    deadline: std::time::Instant,
}
impl PreparedControlUpgrade {
    fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        check_deadline(self.deadline)?;
        self.witness.verify(false)?;
        let path = crate::motion_snapshot::source_reader::verify_rusqlite_writer_main(&self.db)?;
        ensure!(
            path == std::fs::canonicalize(&self.witness.path)?,
            "prepared control writer differs from retained stage"
        );
        Ok(path)
    }
    fn verify_current(
        &self,
        reference: &UpgradeReference,
        cancel: &AtomicBool,
        recorded: Option<&recovery::Recording<'_>>,
    ) -> anyhow::Result<()> {
        check(cancel)?;
        check_deadline(self.deadline)?;
        let deadline = self.deadline;
        self.db
            .progress_handler(10000, Some(move || std::time::Instant::now() >= deadline))?;
        let verified = (|| {
            self.authorization_path()?;
            admit_memory(
                Some(&self.db),
                self.retained_pair_bytes,
                self.witness.initial_size,
            )?;
            self.witness.verify(true)?;
            ensure!(
                reference.0.verify_in(&self.db)? == ControlVersion::Retained93,
                "prepared control is no longer exact93"
            );
            crate::source_freeze::verify_sync(&self.db)?;
            verify_binding(&self.db, &self.binding)?;
            self.before
                .verify(&self.db, cancel, &ControlAdditions::default())?;
            if let Some(recorded) = recorded {
                recorded.verify_original(&self.witness, &self.db, cancel)?;
            }
            integrity(&self.db)?;
            check(cancel)?;
            check_deadline(deadline)
        })();
        // Idle authorization never resets the absolute deadline. Remove only
        // the SQL callback, reinstalling the same deadline for the next check.
        let cleared = self.db.progress_handler(0, None::<fn() -> bool>);
        verified?;
        cleared?;
        Ok(())
    }
    fn apply(
        mut self,
        reference: &UpgradeReference,
        cancel: &AtomicBool,
        recorded: Option<&recovery::Recording<'_>>,
    ) -> anyhow::Result<AdmittedControlStore> {
        let mut progress = recovery::RetainedApplyProgress::default();
        self.apply_retained(reference, cancel, recorded, &mut progress)?;
        Ok(AdmittedControlStore {
            db: self.db,
            witness: self.witness,
            binding: self.binding,
        })
    }
    // Both the legacy consumer and the new operation use these exact effects.
    // Only the new operation retains this owner/progress across error or unwind.
    fn apply_retained(
        &mut self,
        reference: &UpgradeReference,
        cancel: &AtomicBool,
        recorded: Option<&recovery::Recording<'_>>,
        progress: &mut recovery::RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        // Preserve the legacy ordering, including every full scan and fault
        // checkpoint. The joined-reader path calls these same fixed stages
        // separately so its application temporal fences do not precede scans.
        self.verify_current(reference, cancel, recorded)?;
        self.apply_effects_retained(reference, cancel, recorded, progress)?;
        self.commit_retained(progress)?;
        self.reacquire_and_verify_retained(reference, cancel, progress)
    }
    /// First fixed effects after the joined full preflight and the application's
    /// no-IO temporal fence. Before the first DDL there are only bounded local
    /// checks, progress-handler setup and SQLite DBSTATUS accounting, not SQL
    /// verification, filesystem reads or a fresh preservation/reference scan.
    fn apply_effects_retained(
        &mut self,
        reference: &UpgradeReference,
        cancel: &AtomicBool,
        recorded: Option<&recovery::Recording<'_>>,
        progress: &mut recovery::RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        let db = &self.db;
        let witness = &mut self.witness;
        let before = &self.before;
        let binding = &self.binding;
        let mut retained_pair_bytes = self.retained_pair_bytes;
        let deadline = self.deadline;
        check_deadline(deadline)?;
        db.progress_handler(10000, Some(move || std::time::Instant::now() >= deadline))?;
        let additions = &mut progress.additions;
        for migration in reference.0.suffix() {
            check(cancel)?;
            check_deadline(deadline)?;
            admit_memory(Some(&db), retained_pair_bytes, witness.initial_size)?;
            db.execute_batch(&migration.sql)?;
            let installed_on: String =
                db.query_row("SELECT CURRENT_TIMESTAMP", [], |r| r.get(0))?;
            set_ledger(&db, true)?;
            let inserted=db.execute("INSERT INTO _sqlx_migrations(version,description,installed_on,success,checksum,execution_time) VALUES(?,?,?,1,?,0)",rusqlite::params![migration.version,migration.description.as_ref(),installed_on,migration.checksum.as_ref()]);
            let revoked = set_ledger(&db, false);
            inserted?;
            revoked?;
            // Expected additions derive from the exact insertion inputs,
            // not an unconstrained row observed after migration effects.
            additions.ledger.push(vec![
                Value::Integer(db.last_insert_rowid()),
                Value::Integer(migration.version),
                Value::Text(migration.description.to_string()),
                Value::Text(installed_on),
                Value::Integer(1),
                Value::Blob(migration.checksum.to_vec()),
                Value::Integer(0),
            ]);
            checkpoint(migration.version)?;
        }
        check(cancel)?;
        check_deadline(deadline)?;
        additions.guards = crate::source_freeze::append_preview97_guards_in(&db)?;
        ensure!(
            reference.0.verify_in(&db)? == ControlVersion::Complete97,
            "control upgrade did not reach closed97"
        );
        crate::source_freeze::verify_sync(&db)?;
        verify_binding(&db, &binding)?;
        before.verify(&db, cancel, &additions)?;
        admit_memory(Some(&db), retained_pair_bytes, witness.initial_size)?;
        new_owners_empty(&db)?;
        integrity(&db)?;
        witness.verify(false)?;
        check(cancel)?;
        check_deadline(deadline)?;
        if let Some(recorded) = &recorded {
            let guards = crate::source_freeze::preview97_guard_definitions_in(db)?;
            let publication_bytes = record::PreparedRecord::storage_reservation(
                &recorded.anchor,
                additions,
                &reference.0,
                &guards,
            )?;
            retained_pair_bytes = retained_pair_bytes
                .checked_add(publication_bytes)
                .context("retained control publication accounting overflow")?;
            admit_memory(Some(db), retained_pair_bytes, witness.initial_size)?;
            // Admission precedes every publication clone. This sticky charge is
            // retained on every failure, even if the canonical record is partial.
            self.retained_pair_bytes = retained_pair_bytes;
            let prepared = record::PreparedRecord::new(
                recorded.anchor.clone(),
                additions,
                &reference.0,
                &guards,
            )?;
            progress.publication = Some(record::RetainedRecordPublication::new(prepared));
            let publication = progress
                .publication
                .as_mut()
                .expect("just stored publication");
            #[cfg(test)]
            if let Some((wanted, panic)) = progress.record_fault {
                publication.publish_at_checkpoint(
                    recorded.directory,
                    cancel,
                    deadline,
                    |phase, _| {
                        if phase == wanted {
                            if panic {
                                panic!("retained control publication unwind");
                            }
                            anyhow::bail!("retained control publication fault");
                        }
                        Ok(())
                    },
                )?;
            } else {
                publication.publish(recorded.directory, cancel, deadline)?;
            }
            #[cfg(not(test))]
            publication.publish(recorded.directory, cancel, deadline)?;
            witness.record = publication.take_durable();
            ensure!(
                witness.record.is_some(),
                "verified control record owner missing"
            );
            witness.verify(false)?;
            check(cancel)?;
        }
        Ok(())
    }
    /// The application performs a fresh pure temporal fence immediately before
    /// this fixed continuation. No SQL, scans, pathname checks or publication
    /// occur before COMMIT. COMMIT can block; its outcome is uncertain on error,
    /// and the inherited deadline is not a hard preemption guarantee.
    fn commit_retained(
        &mut self,
        progress: &mut recovery::RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        check_deadline(self.deadline)?;
        checkpoint(100)?;
        progress.commit = RecordedCommitState::CommitAttempted;
        checkpoint(102)?;
        self.db.execute_batch("COMMIT")?;
        progress.commit = RecordedCommitState::Committed97;
        checkpoint(101)?;
        Ok(())
    }
    /// Separate from the commit continuation. A committed result alone does not
    /// authorize Ready97: the same connection must reacquire its writer and pass
    /// every original terminal reference/binding/preservation check.
    fn reacquire_and_verify_retained(
        &mut self,
        reference: &UpgradeReference,
        cancel: &AtomicBool,
        progress: &mut recovery::RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        let db = &self.db;
        let witness = &self.witness;
        let before = &self.before;
        let binding = &self.binding;
        let additions = &progress.additions;
        let retained_pair_bytes = self.retained_pair_bytes;
        // Retain a fresh writer for the fixed96 transfer, revalidating the exact
        // terminal schema and binding after the explicit commit boundary.
        db.execute_batch("BEGIN IMMEDIATE")
            .context("control97 committed; retained custody requires recovery")?;
        progress.commit = RecordedCommitState::WriterReacquired;
        checkpoint(103)?;
        ensure!(
            reference.0.verify_in(&db)? == ControlVersion::Complete97,
            "control97 changed after commit"
        );
        crate::source_freeze::verify_sync(&db)?;
        verify_binding(&db, &binding)?;
        before.verify(&db, cancel, &additions)?;
        new_owners_empty(&db)?;
        admit_memory(Some(&db), retained_pair_bytes, witness.initial_size)?;
        witness.verify(false)?;
        db.progress_handler(0, None::<fn() -> bool>)?;
        Ok(())
    }
}
pub(crate) struct AdmittedControlStore {
    db: Connection,
    witness: StageWitness,
    binding: String,
}
impl AdmittedControlStore {
    /// Retain the actual original archive before native acquisition. The
    /// witness is descriptive until a separate native/host composite validates
    /// it; bootstrap evidence is not synthesized by this handoff.
    pub(crate) fn prepare_native_with_archive_reserved(
        self,
        native: &Path,
        q: catabolic_store::migration::NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
        deadline: std::time::Instant,
        cancel: std::sync::Arc<AtomicBool>,
    ) -> anyhow::Result<StagePreparedTransition> {
        self.witness.verify(false)?;
        verify_binding(&self.db, &self.binding)?;
        let prepared = crate::native_bootstrap::PreparedTransition::prepare_retained_control_with_archive_reserved(
            self.db, native, q, id, external_retained_bytes, deadline, cancel,
        )?;
        self.witness.verify(false)?;
        Ok(StagePreparedTransition {
            prepared,
            witness: self.witness,
        })
    }
    /// Fixed owner handoff, not a raw connection or generic callback API.
    pub(crate) fn prepare_native(
        self,
        native: &Path,
        q: catabolic_store::migration::NativeUpgradeQuiescence,
        id: &str,
    ) -> anyhow::Result<StagePreparedTransition> {
        self.prepare_native_reserved(native, q, id, 0)
    }
    pub(crate) fn prepare_native_reserved(
        self,
        native: &Path,
        q: catabolic_store::migration::NativeUpgradeQuiescence,
        id: &str,
        external_retained_bytes: u64,
    ) -> anyhow::Result<StagePreparedTransition> {
        self.witness.verify(false)?;
        verify_binding(&self.db, &self.binding)?;
        // The same connection remains owned; no reopen/reacquisition of Motion.
        let prepared =
            crate::native_bootstrap::PreparedTransition::prepare_retained_control_reserved(
                self.db,
                native,
                q,
                id,
                external_retained_bytes,
            )?;
        Ok(StagePreparedTransition {
            prepared,
            witness: self.witness,
        })
    }
}
pub(crate) struct StagePreparedTransition {
    prepared: crate::native_bootstrap::PreparedTransition,
    witness: StageWitness,
}

impl StagePreparedTransition {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.witness.verify(false)?;
        self.prepared
            .refresh_external_reservation(external_retained_bytes)
    }
    pub(crate) async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        self.witness.verify(false)?;
        self.prepared.authorization_reader().await
    }
    pub(crate) fn apply(self, backup: Option<&Path>) -> anyhow::Result<StagePendingTransition> {
        self.witness.verify(false)?;
        let pending = self.prepared.apply(backup)?;
        self.witness.verify(false)?;
        Ok(StagePendingTransition {
            pending,
            witness: self.witness,
        })
    }
}
pub(crate) struct StagePendingTransition {
    pending: crate::native_bootstrap::PendingTransition,
    witness: StageWitness,
}
impl StagePendingTransition {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.witness.verify(false)?;
        self.pending
            .refresh_external_reservation(external_retained_bytes)
    }
    pub(crate) async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        self.witness.verify(false)?;
        self.pending.authorization_reader().await
    }
    pub(crate) fn verified_backup_path(&self) -> anyhow::Result<&Path> {
        self.pending.verified_backup_path()
    }
    pub(crate) fn save_command(
        self,
        id: &str,
        evidence: &[catabolic_store::migration::offline_bootstrap::HostSourceEvidence<'_>],
    ) -> anyhow::Result<Self> {
        self.witness.verify(false)?;
        let pending = self.pending.save_command(id, evidence)?;
        self.witness.verify(false)?;
        Ok(Self {
            pending,
            witness: self.witness,
        })
    }
    pub(crate) fn execute_unacknowledged(
        self,
        evidence: &[catabolic_store::migration::offline_bootstrap::HostSourceEvidence<'_>],
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
    ) -> anyhow::Result<StageExecutedTransition> {
        self.witness.verify(false)?;
        let executed = self.pending.execute_saved(evidence, policy)?;
        self.witness.verify(false)?;
        Ok(StageExecutedTransition {
            executed,
            witness: self.witness,
        })
    }
}
pub(crate) struct StageExecutedTransition {
    executed: crate::native_bootstrap::ExecutedTransition,
    witness: StageWitness,
}
impl StageExecutedTransition {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.witness.verify(false)?;
        self.executed
            .refresh_external_reservation(external_retained_bytes)
    }
    pub(crate) async fn authorization_reader(
        &mut self,
    ) -> anyhow::Result<crate::native_bootstrap::TargetAuthorizationReader<'_>> {
        self.witness.verify(false)?;
        self.executed.authorization_reader().await
    }
    pub(crate) fn acknowledge(self) -> anyhow::Result<StageAcknowledged> {
        self.witness.verify(false)?;
        let acknowledged = self.executed.acknowledge()?;
        self.witness.verify(false)?;
        Ok(StageAcknowledged {
            acknowledged,
            witness: self.witness,
        })
    }
}

pub(crate) struct StageAcknowledged {
    acknowledged: crate::native_bootstrap::AcknowledgedNativeBootstrap,
    witness: StageWitness,
}
impl StageAcknowledged {
    pub(crate) fn refresh_external_reservation(
        &mut self,
        external_retained_bytes: u64,
    ) -> anyhow::Result<()> {
        self.witness.verify(false)?;
        self.acknowledged
            .refresh_external_reservation(external_retained_bytes)
    }
    pub(crate) fn transition_id(&self) -> &str {
        self.acknowledged.transition_id()
    }
    pub(crate) fn acknowledgment_digest(&self) -> &str {
        self.acknowledged.acknowledgment_digest()
    }
    pub(crate) fn native_transition_digest(&self) -> &str {
        self.acknowledged.native_transition_digest()
    }
    pub(crate) fn identity(&self) -> &playscale_core::catalog_backend::Identity {
        self.acknowledged.identity()
    }
    pub(crate) fn into_completed_image(
        self,
        cancel: &AtomicBool,
    ) -> anyhow::Result<StageCompletedImage> {
        self.witness.verify(false)?;
        let image = self.acknowledged.into_completed_image(cancel)?;
        self.witness.verify(false)?;
        Ok(StageCompletedImage {
            image,
            witness: self.witness,
        })
    }
}
pub(crate) struct StageCompletedImage {
    image: crate::native_bootstrap::AcknowledgedCompletedImage,
    witness: StageWitness,
}
impl StageCompletedImage {
    pub(crate) fn into_restore_parts(
        self,
    ) -> anyhow::Result<(
        crate::native_bootstrap::AcknowledgedCompletedImage,
        StageWitness,
    )> {
        self.witness.verify(false)?;
        Ok((self.image, self.witness))
    }
}

pub(crate) struct StageWitness {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    initial_size: u64,
    initial_sha: String,
    record: Option<record::DurableRecord>,
}
impl StageWitness {
    pub(crate) fn close_verification_bytes(&self) -> anyhow::Result<u64> {
        let record = self
            .record
            .as_ref()
            .map_or(Ok(0), |record| record.close_verification_bytes())?;
        // Metadata on the named stage may materialize an OS path buffer. Keep
        // two checked path+NUL shares in addition to the exact record buffers;
        // no assumption that the standard library uses stack-only conversion.
        let path = u64::try_from(self.path.as_os_str().len())?
            .checked_add(1)
            .and_then(|n| n.checked_mul(2))
            .context("source close path storage overflow")?;
        record
            .checked_add(path)
            .context("source close verifier storage overflow")
    }
    fn new(file: File, path: &Path, sha: &str) -> anyhow::Result<Self> {
        ensure!(
            sha.len() == 64
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid initial stage digest"
        );
        let m = file.metadata()?;
        ensure!(
            m.is_file() && m.nlink() == 1 && m.len() > 0 && m.len() <= 56 * 1024 * 1024,
            "unsupported control stage file"
        );
        Ok(Self {
            file,
            path: path.to_owned(),
            device: m.dev(),
            inode: m.ino(),
            initial_size: m.len(),
            initial_sha: sha.into(),
            record: None,
        })
    }
    fn verify(&self, initial: bool) -> anyhow::Result<()> {
        if let Some(record) = &self.record {
            record.verify()?;
        }
        let held = self.file.metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            held.is_file()
                && named.is_file()
                && held.nlink() == 1
                && named.nlink() == 1
                && held.dev() == self.device
                && held.ino() == self.inode
                && named.dev() == self.device
                && named.ino() == self.inode,
            "control stage identity changed"
        );
        if initial {
            ensure!(
                held.len() == self.initial_size && named.len() == self.initial_size,
                "initial control image length changed"
            );
            // The image is bounded before allocation; the same held descriptor,
            // not a second pathname open, supplies the compared exact bytes.
            let mut bytes = vec![0; usize::try_from(self.initial_size)?];
            self.file.read_exact_at(&mut bytes, 0)?;
            ensure!(
                catabolic_core::sha256(&bytes) == self.initial_sha,
                "initial control image changed"
            );
        }
        Ok(())
    }
}
fn check(cancel: &AtomicBool) -> anyhow::Result<()> {
    ensure!(!cancel.load(Ordering::Acquire), "control upgrade cancelled");
    Ok(())
}
fn check_deadline(deadline: std::time::Instant) -> anyhow::Result<()> {
    ensure!(
        std::time::Instant::now() < deadline,
        "control upgrade preparation deadline expired"
    );
    Ok(())
}
fn integrity(db: &Connection) -> anyhow::Result<()> {
    let result: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(result == "ok", "control image integrity failed");
    ensure!(
        !db.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_some(),
        "control foreign key check failed"
    );
    Ok(())
}
fn verify_binding(db: &Connection, expected: &str) -> anyhow::Result<()> {
    let len: i64 = db.query_row(
        "SELECT length(CAST(identity_json AS BLOB)) FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!((1..=65536).contains(&len), "control binding exceeds bound");
    let raw: String = db.query_row(
        "SELECT identity_json FROM catabolic_binding WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        catabolic_core::sha256(raw.as_bytes()) == expected,
        "control frozen binding changed"
    );
    let binding: playscale_core::catalog_backend::Identity = serde_json::from_str(&raw)?;
    binding
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid frozen control identity: {e:?}"))?;
    for (table, column) in [
        ("catabolic_native_upgrade_intent", "intent_json"),
        ("catabolic_native_upgrade_ack", "proof_json"),
    ] {
        let (count, bytes): (i64, i64) = db.query_row(
            &format!(
                "SELECT count(*),coalesce(sum(length(CAST({column} AS BLOB))),0) FROM {table}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            (0..=1).contains(&count) && (0..=2 * 1024 * 1024).contains(&bytes),
            "control historical upgrade evidence exceeds bound"
        );
    }
    let current = crate::native_upgrade::effective_identity_sync(db, &binding)?;
    ensure!(
        current.schema_version == 40,
        "control upgrade requires authenticated native40 lineage"
    );
    Ok(())
}
unsafe extern "C" fn ledger(c: *mut ffi::sqlite3_context, n: i32, v: *mut *mut ffi::sqlite3_value) {
    let allowed = if n == 1 && !v.is_null() {
        let p = unsafe { ffi::sqlite3_value_text(*v) };
        !p.is_null() && unsafe { CStr::from_ptr(p.cast()) }.to_bytes() == b"_sqlx_migrations"
    } else {
        false
    };
    unsafe { ffi::sqlite3_result_int(c, i32::from(allowed)) };
}
unsafe extern "C" fn deny(c: *mut ffi::sqlite3_context, _: i32, _: *mut *mut ffi::sqlite3_value) {
    unsafe { ffi::sqlite3_result_int(c, 0) };
}
fn set_ledger(db: &Connection, allow: bool) -> anyhow::Result<()> {
    let code = unsafe {
        ffi::sqlite3_create_function_v2(
            db.handle(),
            c"motion_native_projection".as_ptr(),
            1,
            ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
            std::ptr::null_mut(),
            Some(if allow { ledger } else { deny }),
            None,
            None,
            None,
        )
    };
    ensure!(
        code == ffi::SQLITE_OK,
        "control ledger capability registration failed"
    );
    Ok(())
}

#[cfg(test)]
#[path = "offline_control_upgrade/tests.rs"]
mod tests;

#[cfg(test)]
thread_local! { static FAULT: std::cell::Cell<Option<(i64,bool)>> = const { std::cell::Cell::new(None) }; }
fn checkpoint(phase: i64) -> anyhow::Result<()> {
    #[cfg(test)]
    if let Some((wanted, crash)) = FAULT.with(|f| f.get()) {
        if wanted == phase {
            if crash {
                std::process::exit(77);
            }
            anyhow::bail!("injected post-effect control rollback");
        }
    }
    let _ = phase;
    Ok(())
}

/// Logical retained-memory admission, not a universal peak RSS guarantee.
/// The fixed allowance covers bounded reference/inventory buffers; the caller
/// supplies actually retained paired-owner bytes, never a user budget override.
fn admit_memory(
    db: Option<&Connection>,
    retained_pair_bytes: u64,
    image_bytes: u64,
) -> anyhow::Result<()> {
    let mut total = retained_pair_bytes
        .checked_add(image_bytes)
        .and_then(|n| n.checked_add(catabolic_store::migration::offline_bootstrap::OfflineControlWorkspaceBacking::BYTES))
        .context("control memory admission overflow")?;
    if let Some(db) = db {
        for op in [
            ffi::SQLITE_DBSTATUS_CACHE_USED,
            ffi::SQLITE_DBSTATUS_SCHEMA_USED,
            ffi::SQLITE_DBSTATUS_STMT_USED,
        ] {
            let mut current = 0;
            let mut high = 0;
            let code =
                unsafe { ffi::sqlite3_db_status(db.handle(), op, &mut current, &mut high, 0) };
            ensure!(
                code == ffi::SQLITE_OK && current >= 0,
                "control retained allocation unavailable"
            );
            total = total
                .checked_add(current as u64)
                .context("control allocation overflow")?;
        }
    }
    ensure!(
        total <= 256 * 1024 * 1024,
        "control and paired retained-memory cohort exceeded"
    );
    Ok(())
}

fn new_owners_empty(db: &Connection) -> anyhow::Result<()> {
    for table in [
        "catabolic_source_relocation_admissions",
        "catabolic_source_relocation_commands",
        "catabolic_source_relocation_acks",
        "catabolic_paired_restore_intents",
        "catabolic_paired_restore_acks",
        "catabolic_native_bootstrap_intents",
        "catabolic_native_bootstrap_commands",
        "catabolic_native_bootstrap_acks",
        "preview_sheet_artifacts",
    ] {
        let count: i64 =
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        ensure!(
            count == 0,
            "control upgrade cannot adopt existing new-owner effects: {table}"
        );
    }
    Ok(())
}

mod phase_facade;
pub use phase_facade::{
    AcknowledgedControlNativeBootstrap, ExecutedControlNativeBootstrap,
    PendingControlNativeBootstrap, PreparedControlNativeUpgrade,
};
