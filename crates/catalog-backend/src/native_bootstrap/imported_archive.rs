//! Fixed host-publication witness. Native permanent lineage is verified
//! independently; this owner never manufactures a native import/result grant.
//! Optional phase custody is retained here; derived library consumers remain
//! separate and require their own current-pair admission and native proof.
pub(crate) use crate::imported_sources::archive::{
    ImportedLibraryBridge, RetainedImportedArchive, TransferredImportedArchive,
};

/// Conversion-only cancellation/deadline custody. It ends at the fixed
/// completed-source transfer; subsequent reads have their own actual operation
/// bounds and cannot mint evidence merely by retaining this descriptor.
pub(super) struct RetainedArchiveCapture {
    archive: RetainedImportedArchive,
    deadline: std::time::Instant,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl RetainedArchiveCapture {
    pub(super) fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            std::time::Instant::now() < self.deadline
                && !self.cancel.load(std::sync::atomic::Ordering::Acquire),
            "archive conversion interrupted"
        );
        Ok(())
    }
    pub(super) fn capture_in(
        db: &rusqlite::Connection,
        original: &playscale_core::catalog_backend::Identity,
        remaining: u64,
        deadline: std::time::Instant,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<Self> {
        let archive =
            RetainedImportedArchive::capture_in(db, original, remaining, deadline, &cancel)?;
        Ok(Self {
            archive,
            deadline,
            cancel,
        })
    }
    pub(super) fn retained_allocations(&self) -> anyhow::Result<u64> {
        self.archive.retained_allocations()
    }
    pub(super) fn into_transferred(
        self,
        db: &rusqlite::Connection,
        original: &playscale_core::catalog_backend::Identity,
        remaining: u64,
    ) -> anyhow::Result<TransferredImportedArchive> {
        self.archive
            .into_transferred(db, original, remaining, self.deadline, &self.cancel)
    }
}
