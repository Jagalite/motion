//! Fixed94 access to the actual completed native owner. No reconstructed grant.
use super::*;
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::{catalog_commands::{CommandOutcome, CommandStoreError, ReceiptPolicy}, migration::offline_bootstrap::SourceRenewalReceipt};
use std::time::Instant;

use crate::source_relocations::owned_operation::{RelocationAccounting, producer::RelocationProducer};
const LIMIT: u64 = 256 * 1024 * 1024;
fn sum(parts: &[u64]) -> anyhow::Result<u64> {
    parts.iter().try_fold(0u64, |n, p| n.checked_add(*p).filter(|n| *n <= LIMIT).context("owned94 allocation cohort exceeded"))
}
impl StageSourceSession {
    pub(crate) fn source_close_admitted94(&self) -> bool {
        self.capture_admission.is_none() && self.proof.has_retained_close_admission()
    }
    pub(crate) fn path94(&self) -> &PathBuf { &self.witness.path }
    fn metadata94(&self) -> anyhow::Result<u64> {
        sum(&[self.proof.retained_allocations()?, self.reference.0.retained_allocations()?,
            self.witness.path.capacity() as u64, self.witness.initial_sha.capacity() as u64,
            self.witness.record.as_ref().map(|r| r.retained_and_verification_bytes()).transpose()?.unwrap_or(0)])
    }
    pub(crate) fn admit94(&mut self, storage: u64, deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<RelocationAccounting> {
        ensure!(self.source_close_admitted94(), "owned94 requires genuine retained96 close admission");
        check(cancel)?;
        ensure!(Instant::now() < deadline, "owned94 original deadline expired");
        let previous = self.native.external_retained_reservation()?;
        let host = sum(&[self.proof.capture_external_baseline(previous)?, storage])?;
        // H remains a distinct host claim. K is only the successful native forward.
        self.proof.retain_external_claim(host)?;
        let forwarded = previous.max(sum(&[host, self.metadata94()?])?);
        sum(&[self.native.retained_allocations()?, forwarded])?;
        self.proof.refresh_source_workspace94(&mut self.native, &self.reference, &self.witness, forwarded)?;
        self.proof.record_allocation_forward(previous, forwarded)?;
        Ok(RelocationAccounting { host })
    }
    pub(crate) async fn refresh94(&mut self, accounting: &RelocationAccounting, db: &mut SqliteConnection, extra: u64, deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<u64> {
        check(cancel)?;
        ensure!(Instant::now() < deadline, "owned94 original deadline expired");
        let target = {
            let mut lease = db.lock_handle().await?;
            let actual = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }?;
            crate::motion_snapshot::connection_allocations(&[&actual])?
        };
        let previous = self.native.external_retained_reservation()?;
        // proof.metadata includes the retained OLD96 SQLite tail exactly once;
        // target is the different, actual NEW94 SQLx connection.
        let host = accounting.host.max(self.proof.capture_external_baseline(previous)?);
        let base=sum(&[host,self.metadata94()?,target,extra])?;
        let native=self.native.retained_allocations()?;
        let remaining=LIMIT.checked_sub(sum(&[native,base])?).context("owned94 scanner has no remainder")?;
        let transitive=crate::source_relocations::owned_operation::allocation::preflight_in(db,remaining,deadline,cancel).await?.total()?;
        let forwarded = previous.max(sum(&[base,transitive])?);
        sum(&[self.native.retained_allocations()?, forwarded])?;
        self.proof.refresh_source_workspace94(&mut self.native, &self.reference, &self.witness, forwarded)?;
        self.proof.record_allocation_forward(previous, forwarded)?;
        check(cancel)?;
        ensure!(Instant::now() < deadline, "owned94 original deadline expired");
        Ok(target)
    }
    // Callers must install each actual Result in their retained owner immediately.
    // These have no post-effect await, allocation refresh or pair postcheck.
    pub(crate) fn prepare94(&mut self, profile: &str, source: &str, path: &Path, cancel: &AtomicBool) -> RelocationProducer<SourceRenewalPreparation, catabolic_store::Error> {
        self.proof.prepare_source_workspace94(&mut self.native, &self.reference, &self.witness, profile, source, path, cancel)
    }
    pub(crate) fn invalidate_execute94(&mut self) {
        self.proof.invalidate_source_workspace94();
    }
    /// Full P traversal precedes the app's final current+target temporal fence.
    pub(crate) fn preflight_execute94(&mut self) -> anyhow::Result<()> {
        self.proof.preflight_source_execute_workspace94(&self.reference, &self.witness)
    }
    pub(crate) fn execute94(&mut self, command: &CommandEnvelope, cancel: &AtomicBool) -> RelocationProducer<CommandOutcome, CommandStoreError> {
        self.proof.execute_source_workspace94(&mut self.native, command, source_authority(), ReceiptPolicy {
            retention: std::time::Duration::from_secs(3600), max_receipts: 100,
            max_hold_records: 100, max_logical_bytes: 16 * 1024 * 1024,
        }, cancel)
    }
    pub(crate) fn receipt94(&mut self, command: &CommandEnvelope) -> RelocationProducer<Option<SourceRenewalReceipt>, catabolic_store::Error> {
        self.proof.receipt_source_workspace94(&self.native, &self.reference, &self.witness, command, source_authority())
    }
    #[cfg(test)]
    pub(crate) fn source_workspace_qualification94_for_test(&self) -> Option<bool> {
        self.proof.source_workspace_qualification94_for_test()
    }
    #[cfg(test)]
    pub(crate) fn source_workspace_provenance94_for_test(&self) -> anyhow::Result<(u64, u64, u64)> {
        let (host, forwarded) = self.proof.allocation_provenance();
        Ok((host, forwarded, self.native.external_retained_reservation()?))
    }
    #[cfg(test)]
    pub(crate) fn refresh_source_workspace94_for_test(&mut self) -> anyhow::Result<()> {
        let forwarded = self.native.external_retained_reservation()?;
        self.proof.refresh_source_workspace94(&mut self.native, &self.reference, &self.witness, forwarded)?;
        self.proof.record_allocation_forward(forwarded, forwarded)
    }
    #[cfg(test)]
    pub(crate) fn enlarge_source_workspace94_for_test(&mut self) {
        self.proof.enlarge_source_workspace94_for_test();
    }
}

// Capture ingress retains the whole predecessor. No source extractor, native
// capture admission or arbitrary path/pool is exposed at this finite boundary.
impl StageSourceSession {
    pub(crate) fn capture_ingress_path94(&self) -> anyhow::Result<&PathBuf> {
        ensure!(self.source_close_admitted94(), "capture ingress close marker missing");
        self.witness.verify(false)?;
        Ok(&self.witness.path)
    }
    pub(crate) fn capture_ingress_host94(&self) -> anyhow::Result<u64> {
        self.proof.capture_external_baseline(self.native.external_retained_reservation()?)
    }
    pub(crate) fn retain_capture_ingress94(&mut self, host: u64, deadline: Instant, cancel: &AtomicBool) -> anyhow::Result<()> {
        self.capture_ingress_path94()?;
        check(cancel)?;
        ensure!(Instant::now() < deadline, "capture ingress original deadline expired");
        let previous = self.native.external_retained_reservation()?;
        let observed = self.proof.capture_external_baseline(previous)?;
        ensure!(host >= observed, "capture ingress host provenance changed");
        self.proof.retain_external_claim(host)?;
        let forwarded = previous.max(sum(&[host, self.metadata94()?])?);
        sum(&[self.native.retained_allocations()?, forwarded])?;
        // The whole94 handoff still owns the original settled control backing.
        // Freshly qualify its current split P, then borrow it only for this
        // synchronous refresh. H/K and every independent retained charge stay
        // unchanged; absent tail or oversized P uses the ordinary full charge.
        self.proof.refresh_source_workspace94(&mut self.native, &self.reference, &self.witness, forwarded)?;
        self.proof.record_allocation_forward(previous, forwarded)?;
        check(cancel)?;
        ensure!(Instant::now() < deadline, "capture ingress original deadline expired");
        Ok(())
    }
    /// Fresh same-file check before denial-only projection setup or BEGIN.
    /// Opening itself uses fixed connection-local PRAGMAs only. The positive88
    /// predecessor and original host cooperative locks remain required.
    pub(crate) async fn verify_capture_connection94(&self, db: &mut SqliteConnection) -> anyhow::Result<usize> {
        self.witness.verify(false)?;
        let mut lease = db.lock_handle().await?;
        let handle = lease.as_raw_handle().as_ptr();
        let current = unsafe { Connection::from_handle(handle) }?;
        ensure!(current.is_autocommit()
            && unsafe { ffi::sqlite3_txn_state(handle, c"main".as_ptr()) } == 0,
            "capture ingress connection is not initially idle");
        let filename: String = current.query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'", [], |row| row.get(0))?;
        ensure!(!filename.is_empty()
            && std::fs::canonicalize(&filename)? == std::fs::canonicalize(&self.witness.path)?,
            "capture ingress target filename differs");
        let named = std::fs::symlink_metadata(&filename)?;
        ensure!(named.is_file() && named.nlink() == 1
            && (named.dev(), named.ino()) == (self.witness.device, self.witness.inode),
            "capture ingress target identity differs");
        // ClosedReference requires an actual transaction. Do not weaken it or
        // call it on this idle connection. Full exact97 schema/pair verification
        // repeats through the original verifier after typed BEGIN is retained.
        self.witness.verify(false)?;
        Ok(handle as usize)
    }
}
