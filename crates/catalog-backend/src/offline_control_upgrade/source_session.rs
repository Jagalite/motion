//! Fixed staged-pair succession custody, never an ordinary Runtime41 fallback.
mod close;
mod relocation;
use super::*;
use catabolic_store::migration::offline_bootstrap::{
    CompletedCatalogSession, CompletedSourceWithdrawal, SourceRenewalPreparation,
};
pub use close::SourceSessionClosePhase;
pub(crate) use close::{StageSourceCloseOperation, source_close_storage};
use sqlx::SqliteConnection;
/// Borrow-scoped admission for one fixed archive reread. Only this module can
/// construct it, after the retained stage, exact97 schema and source-pair proof
/// have been checked. It neither exposes the connection nor grants native use.
pub(crate) struct AdmittedSourceArchiveRead<'a> {
    db: &'a Connection,
    original: &'a playscale_core::catalog_backend::Identity,
}
impl AdmittedSourceArchiveRead<'_> {
    pub(crate) fn validate_in(
        &self,
        db: &Connection,
    ) -> anyhow::Result<&playscale_core::catalog_backend::Identity> {
        ensure!(std::ptr::eq(self.db, db), "archive admitted writer differs");
        crate::motion_snapshot::source_reader::verify_rusqlite_writer_main(db)?;
        Ok(self.original)
    }
}
pub(crate) struct StageSourceSession {
    capture_admission: Option<current_image::CaptureAdmission>,
    native: CompletedCatalogSession,
    proof: crate::native_bootstrap::SourcePairProof,
    witness: StageWitness,
    reference: UpgradeReference,
}
impl StageAcknowledged {
    pub(crate) async fn into_source_session(self) -> anyhow::Result<StageSourceSession> {
        let reference = UpgradeReference::build().await?;
        self.witness.verify(false)?;
        let (native, proof) = self.acknowledged.into_source_parts()?;
        self.witness.verify(false)?;
        Ok(StageSourceSession {
            capture_admission: None,
            native,
            proof,
            witness: self.witness,
            reference,
        })
    }
}
impl StageSourceSession {
    #[cfg(test)]
    pub(crate) fn retained_bootstrap_slot_for_test(&self) -> (Option<usize>, usize) {
        self.proof.retained_bootstrap_slot_for_test()
    }
    /// Fixed moves-only destination after the retained96 producer has verified
    /// its exact ACK, completed native owner, settled writer and stage custody.
    /// The proof retains the original producer tail; this constructs no grant
    /// from descriptions and performs no further fallible work or acquisition.
    pub(crate) fn from_retained_parts(
        native: CompletedCatalogSession,
        proof: crate::native_bootstrap::SourcePairProof,
        witness: StageWitness,
        reference: UpgradeReference,
    ) -> Self {
        Self {
            capture_admission: None,
            native,
            proof,
            witness,
            reference,
        }
    }
    pub(crate) fn identity(&self) -> &playscale_core::catalog_backend::Identity {
        self.proof.identity()
    }
    pub(crate) fn close(self) -> anyhow::Result<()> {
        self.witness.verify(false)?;
        let Self {
            native, witness, ..
        } = self;
        drop(native);
        witness.verify(false)
    }
    pub(crate) async fn verify_pair_in(&self, db: &mut SqliteConnection) -> anyhow::Result<()> {
        verify_pair_parts(db, &self.proof, &self.witness, &self.reference).await
    }
    pub(crate) async fn withdrawal_in(
        &self,
        db: &mut SqliteConnection,
        profile: &str,
        native_source: &str,
        motion_source_id: &str,
    ) -> anyhow::Result<CompletedSourceWithdrawal<'_>> {
        self.verify_pair_in(db).await?;
        let proof = self
            .native
            .source_withdrawal(profile, native_source, motion_source_id)?;
        self.verify_pair_in(db).await?;
        Ok(proof)
    }
    pub(crate) async fn prepare_in(
        &mut self,
        db: &mut SqliteConnection,
        profile: &str,
        source: &str,
        candidate: &Path,
        cancel: &AtomicBool,
    ) -> anyhow::Result<SourceRenewalPreparation> {
        self.verify_pair_in(db).await?;
        check(cancel)?;
        let prepared = self
            .native
            .prepare_source_succession(profile, source, candidate, cancel)?;
        self.verify_pair_in(db).await?;
        Ok(prepared)
    }
    pub(crate) async fn execute_in(
        &mut self,
        mut journal: crate::source_relocations::journal::JournalSourceCommand<'_>,
        policy: catabolic_store::catalog_commands::ReceiptPolicy,
        cancel: &AtomicBool,
    ) -> anyhow::Result<catabolic_store::catalog_commands::CommandOutcome> {
        journal.validate_writer_in().await?;
        self.verify_pair_in(journal.writer_in()).await?;
        check(cancel)?;
        let result = self.native.execute_source_succession(
            journal.command(),
            source_authority(),
            policy,
            cancel,
        )?;
        journal.validate_writer_in().await?;
        self.verify_pair_in(journal.writer_in()).await?;
        Ok(result)
    }
    pub(crate) async fn receipt_in(
        &mut self,
        mut journal: crate::source_relocations::journal::JournalSourceCommand<'_>,
    ) -> anyhow::Result<Option<catabolic_store::migration::offline_bootstrap::SourceRenewalReceipt>>
    {
        journal.validate_writer_in().await?;
        self.verify_pair_in(journal.writer_in()).await?;
        let result = self
            .native
            .source_succession_receipt(journal.command(), source_authority())?;
        journal.validate_writer_in().await?;
        self.verify_pair_in(journal.writer_in()).await?;
        Ok(result)
    }
}
fn source_authority() -> catabolic_store::catalog_commands::TrustedCommandAuthority<'static> {
    catabolic_store::catalog_commands::TrustedCommandAuthority {
        host_namespace: crate::HOST_NAMESPACE,
        receipt_visibility: catabolic_core::catalog_command::ReceiptVisibility::FullResult,
        allow_new_commands: true,
        identity_authority: None,
    }
}

#[cfg(test)]
#[path = "source_session/tests.rs"]
mod tests;

async fn verify_pair_parts(
    db: &mut SqliteConnection,
    proof: &crate::native_bootstrap::SourcePairProof,
    witness: &StageWitness,
    reference: &UpgradeReference,
) -> anyhow::Result<()> {
    witness.verify(false)?;
    let mut lease = db.lock_handle().await?;
    let handle = lease.as_raw_handle().as_ptr();
    ensure!(
        unsafe { ffi::sqlite3_get_autocommit(handle) } == 0
            && unsafe { ffi::sqlite3_txn_state(handle, c"main".as_ptr()) } == 2,
        "source pair requires actual retained Motion WRITE transaction"
    );
    // Non-owning fixed-read wrapper dies before exclusive SQLx handle lease.
    let current = unsafe { Connection::from_handle(handle) }?;
    verify_writer_identity(&current, witness)?;
    ensure!(
        reference.0.verify_in(&current)? == ControlVersion::Complete97,
        "source pair requires exact ordered97 control schema"
    );
    proof.verify_in(&current)?;
    witness.verify(false)?;
    Ok(())
}

// Fixed inexpensive file/transaction checks shared by preflight and full proof.
// These do not authenticate schema, historical journals, or native custody.
fn verify_writer_identity(current: &Connection, witness: &StageWitness) -> anyhow::Result<()> {
    ensure!(
        !current.is_autocommit()
            && unsafe { ffi::sqlite3_txn_state(current.handle(), c"main".as_ptr()) } == 2,
        "source pair requires actual retained Motion WRITE transaction"
    );
    let filename: String = current.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |r| r.get(0),
    )?;
    ensure!(
        !filename.is_empty(),
        "source pair requires file-backed Motion"
    );
    // Supported host namespace/server-lock custody, not hostile same-UID ABA
    // proof or an assertion that SQLite exposes its held OS descriptor here.
    ensure!(
        std::fs::canonicalize(&filename)? == std::fs::canonicalize(&witness.path)?,
        "source pair Motion filename differs"
    );
    let named = std::fs::symlink_metadata(&filename)?;
    ensure!(
        named.is_file()
            && named.nlink() == 1
            && (named.dev(), named.ino()) == (witness.device, witness.inode),
        "source pair Motion identity differs"
    );
    Ok(())
}

#[path = "source_session/current_image.rs"]
mod current_image;
pub use current_image::RestoreSealSelection;
pub(crate) use current_image::{
    StageCurrentImage, StageCurrentImageFailure, StageImageCaptureOperation,
    StageRestoreDescriptorFailure, StageRestoreOperation,
};
