//! Fixed recorded control evolution. A record is pending intent, never proof
//! of COMMIT. Recovery independently rechecks original93 rows and closed97.
use super::original::Original93Preservation;
use super::record::{DurableRecord, PreparedRecord, RecordAnchor, RecordDirectory};
use super::*;

pub(super) struct Recording<'a> {
    pub(super) original: &'a Original93Preservation,
    pub(super) directory: &'a RecordDirectory,
    pub(super) anchor: RecordAnchor,
}
impl Recording<'_> {
    pub(super) fn verify_original(
        &self,
        stage: &StageWitness,
        db: &Connection,
        cancel: &AtomicBool,
    ) -> anyhow::Result<()> {
        verify_anchor(&self.anchor, self.original, stage)?;
        self.directory.require_absent()?;
        ensure!(
            stage.initial_sha == self.original.image_sha256(),
            "recorded upgrade stage is not the selected original93 image"
        );
        self.original
            .verify_rows(db, cancel, &ControlAdditions::default())
    }
    pub(super) fn persist(
        &self,
        reference: &UpgradeReference,
        db: &Connection,
        additions: &ControlAdditions,
    ) -> anyhow::Result<DurableRecord> {
        let guards = crate::source_freeze::preview97_guard_definitions_in(db)?;
        let prepared = PreparedRecord::new(self.anchor.clone(), additions, &reference.0, &guards)?;
        self.directory.create_only(&prepared)
    }
}
fn verify_anchor(
    anchor: &RecordAnchor,
    original: &Original93Preservation,
    stage: &StageWitness,
) -> anyhow::Result<()> {
    ensure!(
        anchor.original93_image_sha256 == original.image_sha256()
            && anchor.original93_inventory_sha256 == original.inventory_sha256()
            && anchor.frozen_binding_sha256 == original.frozen_binding_sha256()
            && anchor.stage_device == stage.device
            && anchor.stage_inode == stage.inode,
        "control operation differs from selected original93 or retained stage"
    );
    Ok(())
}
/// Owned exact93 phase for authorization before the first control DDL. The
/// original, record directory, anchor and compiled reference are retained by
/// value; temporary Recording views never become self-references.
pub(crate) struct PreparedRecordedControlUpgrade {
    pub(super) reference: UpgradeReference,
    pub(super) prepared: PreparedControlUpgrade,
    pub(super) original: Original93Preservation,
    pub(super) directory: RecordDirectory,
    pub(super) anchor: RecordAnchor,
}

#[derive(Default)]
pub(super) struct RetainedApplyProgress {
    pub(super) additions: ControlAdditions,
    pub(super) publication: Option<super::record::RetainedRecordPublication>,
    pub(super) commit: RecordedCommitState,
    #[cfg(test)]
    pub(super) record_fault: Option<(u8, bool)>,
}
impl PreparedRecordedControlUpgrade {
    pub(crate) fn verify_current(&self) -> anyhow::Result<()> {
        self.verify_current_cancelled(&AtomicBool::new(false))
    }
    pub(super) fn verify_current_cancelled(&self, cancel: &AtomicBool) -> anyhow::Result<()> {
        let recorded = Recording {
            original: &self.original,
            directory: &self.directory,
            anchor: self.anchor.clone(),
        };
        self.prepared
            .verify_current(&self.reference, cancel, Some(&recorded))
    }
    /// Only the closed authorization reader consumes this checked pathname;
    /// neither the retained database nor a migration callback is exposed.
    pub(crate) fn authorization_path(&self) -> anyhow::Result<PathBuf> {
        self.prepared.authorization_path()
    }
    pub(super) fn apply_retained(
        &mut self,
        cancel: &AtomicBool,
        progress: &mut RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        let recorded = Recording {
            original: &self.original,
            directory: &self.directory,
            anchor: self.anchor.clone(),
        };
        self.prepared
            .apply_retained(&self.reference, cancel, Some(&recorded), progress)
    }
    /// Fixed pending97 effects, after the caller's full joined-reader preflight.
    /// Original evidence, directory and writer remain inline on error or unwind.
    pub(super) fn apply_effects_retained(
        &mut self,
        cancel: &AtomicBool,
        progress: &mut RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        let recorded = Recording {
            original: &self.original,
            directory: &self.directory,
            anchor: self.anchor.clone(),
        };
        self.prepared
            .apply_effects_retained(&self.reference, cancel, Some(&recorded), progress)
    }
    pub(super) fn commit_retained(
        &mut self,
        progress: &mut RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        self.prepared.commit_retained(progress)
    }
    pub(super) fn reacquire_and_verify_retained(
        &mut self,
        cancel: &AtomicBool,
        progress: &mut RetainedApplyProgress,
    ) -> anyhow::Result<()> {
        self.prepared
            .reacquire_and_verify_retained(&self.reference, cancel, progress)
    }
    pub(crate) fn apply(self, cancel: &AtomicBool) -> anyhow::Result<AdmittedControlStore> {
        let Self {
            reference,
            prepared,
            original,
            directory,
            anchor,
        } = self;
        let recorded = Recording {
            original: &original,
            directory: &directory,
            anchor,
        };
        prepared.apply(&reference, cancel, Some(&recorded))
    }
}

impl UpgradeReference {
    /// Preparation holds the same exact93 WRITE owner but performs no94–97 DDL
    /// and creates no record. Authorization may borrow this phase exclusively;
    /// expiry/cancellation before apply leaves only the original93 stage.
    pub(super) fn prepare_recorded(
        self,
        file: File,
        path: &Path,
        original: Original93Preservation,
        directory: RecordDirectory,
        anchor: RecordAnchor,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<PreparedRecordedControlUpgrade> {
        let prepared = {
            let recorded = Recording {
                original: &original,
                directory: &directory,
                anchor: anchor.clone(),
            };
            self.prepare_inner(
                file,
                path,
                original.image_sha256(),
                original.frozen_binding_sha256(),
                retained_pair_bytes,
                cancel,
                Some(&recorded),
            )?
        };
        Ok(PreparedRecordedControlUpgrade {
            reference: self,
            prepared,
            original,
            directory,
            anchor,
        })
    }
    /// The selected-pair/reservation owner supplies the independently captured
    /// original and durable directory. No public pathname or schema override.
    pub(super) fn upgrade_recorded(
        &self,
        file: File,
        path: &Path,
        original: &Original93Preservation,
        directory: &RecordDirectory,
        anchor: RecordAnchor,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<AdmittedControlStore> {
        self.upgrade_inner(
            file,
            path,
            original.image_sha256(),
            original.frozen_binding_sha256(),
            retained_pair_bytes,
            cancel,
            Some(Recording {
                original,
                directory,
                anchor,
            }),
        )
    }
    /// Only the exact same recorded stage may resume. Original row evidence
    /// comes from the selected immutable93 image, never the current97 database.
    pub(super) fn recover_completed97(
        &self,
        file: File,
        path: &Path,
        current_image_sha256: &str,
        original: &Original93Preservation,
        directory: &RecordDirectory,
        anchor: &RecordAnchor,
        retained_pair_bytes: u64,
        cancel: &AtomicBool,
    ) -> anyhow::Result<AdmittedControlStore> {
        let mut witness = StageWitness::new(file, path, current_image_sha256)?;
        verify_anchor(anchor, original, &witness)?;
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
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "recursive_triggers", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        set_ledger(&db, false)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        db.progress_handler(10000, Some(move || std::time::Instant::now() >= deadline))?;
        witness.verify(true)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        ensure!(
            self.0.verify_in(&db)? == ControlVersion::Complete97,
            "recorded control stage is not complete97; pending93 requires explicit reconciliation"
        );
        crate::source_freeze::verify_sync(&db)?;
        verify_binding(&db, original.frozen_binding_sha256())?;
        let guards = crate::source_freeze::preview97_guard_definitions_in(&db)?;
        let record = directory
            .load_exact(anchor, &self.0, &guards)?
            .context("durable control operation record missing")?;
        record.verify()?;
        original.verify_rows(&db, cancel, record.additions())?;
        new_owners_empty(&db)?;
        integrity(&db)?;
        admit_memory(Some(&db), retained_pair_bytes, witness.initial_size)?;
        witness.verify(false)?;
        check(cancel)?;
        record.verify()?;
        witness.record = Some(record);
        db.progress_handler(0, None::<fn() -> bool>)?;
        Ok(AdmittedControlStore {
            db,
            witness,
            binding: original.frozen_binding_sha256().to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
