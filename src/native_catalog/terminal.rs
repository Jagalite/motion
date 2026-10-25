//! Terminal native admission closure. Not a service-drain proof on its own.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
struct CancelOnDrop {
    flag: Arc<AtomicBool>,
    armed: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.flag.store(true, Ordering::Release);
        }
    }
}

/// Non-Clone/non-serializable capability bound to one retained Runtime owner.
/// Drop never reopens ordinary admission. Export failure leaves it closed.
pub(crate) struct TerminalOwner {
    owner: Arc<Mutex<RuntimeAdmission>>,
}
impl Runtime {
    pub(crate) async fn close_admission_for_stopped_service(
        &self,
    ) -> anyhow::Result<TerminalOwner> {
        self.begin_close_admission_for_stopped_service()
            .await
            .context("native terminal closure worker failed")?
    }
    /// Restore supervisor retains this task across cancellation of its waiter.
    pub(crate) fn begin_close_admission_for_stopped_service(
        &self,
    ) -> tokio::task::JoinHandle<anyhow::Result<TerminalOwner>> {
        self.begin_drain();
        let owner = self.admission.clone();
        tokio::task::spawn_blocking(move || {
            {
                let mut held = owner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
                anyhow::ensure!(!held.terminal, "native admission already terminally closed");
                // Refusal is terminal even if subsequent ownership validation fails.
                held.terminal = true;
                let host = held
                    .server
                    .admission
                    .admitted_host()
                    .map_err(|e| anyhow::anyhow!("native terminal admission: {e:?}"))?;
                let inventory = host
                    .worker_inventory()
                    .map_err(|e| anyhow::anyhow!("native terminal inventory: {e:?}"))?;
                anyhow::ensure!(
                    inventory.native && inventory.admits_new_work(),
                    "native ownership remains unresolved"
                );
            }
            Ok(TerminalOwner { owner })
        })
    }
}
/// The service retains this entire completion, including the original fresh
/// writer on both ordinary failure and unwind. The worker never returns custody
/// to a request-owned status channel.
pub(crate) struct PairExportCompletion<T> {
    pub(crate) writer: motion_catalog_backend::motion_snapshot::FreshWriter,
    pub(crate) outcome: std::thread::Result<anyhow::Result<T>>,
}
impl TerminalOwner {
    /// The callback receives only a verified anonymous export, never HostSession
    /// or mutable store authority. Native mutex remains held through callback,
    /// including the future pair publisher's copied-DB checks and durable fsync.
    pub(crate) async fn with_export<T: Send + 'static>(
        &mut self,
        parent: PathBuf,
        cancelled: Arc<AtomicBool>,
        publish: impl FnOnce(
            catabolic_store::catalog_backup::VerifiedExport,
            Arc<AtomicBool>,
        ) -> anyhow::Result<T>
        + Send
        + 'static,
    ) -> anyhow::Result<T> {
        let owner = self.owner.clone();
        let mut cancellation = CancelOnDrop {
            flag: cancelled.clone(),
            armed: true,
        };
        let result = tokio::task::spawn_blocking(move || {
            let mut held = owner
                .lock()
                .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
            anyhow::ensure!(held.terminal, "native terminal capability mismatch");
            let host = held
                .server
                .admission
                .admitted_host()
                .map_err(|e| anyhow::anyhow!("native terminal export: {e:?}"))?;
            let generation = host
                .catalog_generation()
                .map_err(|e| anyhow::anyhow!("native terminal generation: {e:?}"))?;
            let export = host
                .export_snapshot(generation, &parent, &cancelled)
                .map_err(|e| anyhow::anyhow!("native terminal export: {e:?}"))?;
            anyhow::ensure!(
                !cancelled.load(Ordering::Acquire),
                "terminal export cancelled before pair composition"
            );
            publish(export, cancelled)
        })
        .await
        .context("native terminal export worker failed")?;
        cancellation.armed = false;
        result
    }
    /// Pair-only composition: the actual fresh writer moves into the blocking
    /// native owner. Cancellation signals it but cannot release either custody
    /// while synchronous copying or verification is still running.
    pub(crate) async fn with_pair_export<T: Send + 'static>(
        &mut self,
        parent: PathBuf,
        cancelled: Arc<AtomicBool>,
        mut writer: motion_catalog_backend::motion_snapshot::FreshWriter,
        compose: impl FnOnce(
            catabolic_store::catalog_backup::VerifiedExport,
            motion_catalog_backend::motion_snapshot::FreshWriter,
            String,
            Arc<AtomicBool>,
        ) -> anyhow::Result<T>
        + Send
        + 'static,
    ) -> anyhow::Result<T> {
        // Covers bounded stage buffers, manifest strings and receipt overhead.
        // Neither database image is counted here; each owner charges its actual
        // image copies and live connection counters against the shared 256 MiB.
        const PAIR_OVERHEAD: u64 = 1024 * 1024;
        let owner = self.owner.clone();
        let mut cancellation = CancelOnDrop {
            flag: cancelled.clone(),
            armed: true,
        };
        let result = tokio::task::spawn_blocking(move || {
            let mut held = owner
                .lock()
                .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
            anyhow::ensure!(held.terminal, "native terminal capability mismatch");
            let host = held
                .server
                .admission
                .admitted_host()
                .map_err(|e| anyhow::anyhow!("native terminal pair export: {e:?}"))?;
            let generation = host
                .catalog_generation()
                .map_err(|e| anyhow::anyhow!("native terminal generation: {e:?}"))?;
            let identity = host
                .identity()
                .map_err(|e| anyhow::anyhow!("native terminal identity: {e:?}"))?;
            let profile = identity["profile"]
                .as_str()
                .context("native terminal profile missing")?
                .to_owned();
            writer.refuse_destination_overlap_blocking(&parent)?;
            let retained = writer
                .retained_allocations_blocking()?
                .checked_add(PAIR_OVERHEAD)
                .context("pair allocation reservation overflow")?;
            let export = host
                .export_snapshot_reserved(generation, &parent, &cancelled, retained)
                .map_err(|e| anyhow::anyhow!("native terminal pair export: {e:?}"))?;
            anyhow::ensure!(!cancelled.load(Ordering::Acquire), "pair export cancelled");
            // No further host/SQL operation until composition finishes, so the
            // receipt's measured retained native cache remains the phase reserve.
            compose(export, writer, profile, cancelled)
        })
        .await
        .context("native terminal pair worker failed")?;
        cancellation.armed = false;
        result
    }
    /// New restore-only producer. The actual service stores the returned handle
    /// before awaiting it. Its original terminal owner remains service-held.
    /// `service_storage` is the checked, fixed future slot layout, not a new cap.
    pub(crate) fn begin_pair_export<T: Send + 'static>(
        &mut self,
        parent: PathBuf,
        cancelled: Arc<AtomicBool>,
        mut writer: motion_catalog_backend::motion_snapshot::FreshWriter,
        service_storage: u64,
        compose: impl FnOnce(
            catabolic_store::catalog_backup::VerifiedExport,
            &mut motion_catalog_backend::motion_snapshot::FreshWriter,
            String,
            Arc<AtomicBool>,
            &std::path::Path,
        ) -> anyhow::Result<T>
        + Send
        + 'static,
    ) -> tokio::task::JoinHandle<PairExportCompletion<T>> {
        let owner = self.owner.clone();
        tokio::task::spawn_blocking(move || {
            // Install the borrowed writer boundary before any fallible native
            // work. A composition panic cannot drop the original writer.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut held = owner
                    .lock()
                    .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
                anyhow::ensure!(held.terminal, "native terminal capability mismatch");
                let host = held
                    .server
                    .admission
                    .admitted_host()
                    .map_err(|e| anyhow::anyhow!("native terminal pair export: {e:?}"))?;
                let generation = host
                    .catalog_generation()
                    .map_err(|e| anyhow::anyhow!("native terminal generation: {e:?}"))?;
                let identity = host
                    .identity()
                    .map_err(|e| anyhow::anyhow!("native terminal identity: {e:?}"))?;
                let profile = identity["profile"]
                    .as_str()
                    .context("native terminal profile missing")?
                    .to_owned();
                writer.refuse_destination_overlap_blocking(&parent)?;
                let retained = writer
                    .retained_allocations_blocking()?
                    .checked_add(1024 * 1024)
                    .and_then(|n| n.checked_add(service_storage))
                    .context("supervised pair allocation reservation overflow")?;
                let export = host
                    .export_snapshot_reserved(generation, &parent, &cancelled, retained)
                    .map_err(|e| anyhow::anyhow!("native terminal pair export: {e:?}"))?;
                anyhow::ensure!(!cancelled.load(Ordering::Acquire), "pair export cancelled");
                compose(export, &mut writer, profile, cancelled, &parent)
            }));
            PairExportCompletion { writer, outcome }
        })
    }
    /// The actual service can retain this handle before any close waiter polls.
    /// Failure still has the existing consuming terminal-close limitation; this
    /// method does not manufacture a returned owner or a settled status.
    pub(crate) fn begin_close(self) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        tokio::task::spawn_blocking(move || close_runtime(self.owner, Duration::from_secs(5), true))
    }
    pub(crate) async fn close(self) -> anyhow::Result<()> {
        self.begin_close()
            .await
            .context("native terminal close worker failed")?
    }
}

/// Only the terminal Motion server owner can revoke this shared native gate.
/// Closing an arbitrary HostSession must not revoke other hosts sharing it.
fn close_runtime(
    owner: Arc<Mutex<RuntimeAdmission>>,
    wait: Duration,
    require_terminal: bool,
) -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let revocation = {
        let mut held = owner
            .lock()
            .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
        anyhow::ensure!(
            !require_terminal || held.terminal,
            "native terminal capability mismatch"
        );
        held.terminal = true;
        let host = held
            .server
            .admission
            .admitted_host()
            .map_err(|e| anyhow::anyhow!("native shutdown: {e:?}"))?;
        let revocation = host
            .authorization()
            .map_err(|e| anyhow::anyhow!("native terminal authority: {e:?}"))?
            .begin_revocation()
            .map_err(|e| anyhow::anyhow!("native terminal revocation: {e:?}"))?;
        host.begin_drain();
        // poll_scan is cleanup under the already-admitted supervisor, not a
        // fresh authorization. It reaps its actual child, releases the scan
        // lease and records cleanup before this owner closes the backend.
        // Never wait for that lease before polling its supervisor.
        // A repeated close can retain a completed worker handle, but its store
        // is already closed; do not try to record its cleanup a second time.
        if host.lifecycle() != catabolic_runtime::host::Lifecycle::Closed
            && let Some(handle) = host.active_scan()
        {
            anyhow::ensure!(
                host.poll_scan(&handle, wait.saturating_sub(started.elapsed()))
                    .map_err(|e| anyhow::anyhow!("native terminal scan drain: {e:?}"))?
                    .is_some(),
                "native workers did not drain"
            );
        }
        revocation
    };
    // Do not hold RuntimeAdmission while consumers unwind. A queued operation
    // may need that mutex to observe terminal refusal and drop its actual lease.
    // No application SQLite writer is acquired or retained by this wait.
    revocation
        .wait_drained(wait.saturating_sub(started.elapsed()))
        .map_err(|e| anyhow::anyhow!("native authorization did not drain: {e:?}"))?;
    {
        let mut held = owner
            .lock()
            .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
        anyhow::ensure!(held.terminal, "native terminal capability mismatch");
        held.server
            .admission
            .admitted_host()
            .map_err(|e| anyhow::anyhow!("native terminal close: {e:?}"))?
            .close()
            .map_err(|e| anyhow::anyhow!("native terminal close: {e:?}"))?;
    }
    Ok(())
}

/// Ordinary admission is already closed. This actual blocking owner survives a
/// dropped waiter; timeout keeps the shared gate closed and custody retryable.
pub(super) async fn shutdown_runtime(owner: Arc<Mutex<RuntimeAdmission>>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || close_runtime(owner, Duration::from_secs(5), false))
        .await
        .context("native shutdown worker failed")?
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
