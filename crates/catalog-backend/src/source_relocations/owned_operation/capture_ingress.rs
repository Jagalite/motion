//! Real fixed-stage SQLx producer inside the existing capture operation. The
//! fixed native continuation retains the whole94 history and original SQLx graph
//! through opaque NativeCaptured; no source/image extraction or95 grant.
use super::*;
mod authorization;
mod native_capture;
mod paired_capture;
use crate::motion_snapshot::source_reader::RestrictedReaderOwner;
use sqlx::{pool::PoolConnection, sqlite::{SqliteConnectOptions, SqlitePoolOptions}, Sqlite, SqlitePool, Transaction};
use std::{any::Any, future::{Future, poll_fn}, panic::{AssertUnwindSafe, catch_unwind}, pin::Pin, sync::Arc, time::Duration};

type SqlResult<T> = Result<T, sqlx::Error>;
type Connection = PoolConnection<Sqlite>;
type Writer = Transaction<'static, Sqlite>;
type Panic = Box<dyn Any + Send>;
enum BorrowedOutcome<T> { Returned(T), Panicked(Panic) }

pub(super) struct Admission {
    cancel: Arc<AtomicBool>,
    host: u64,
    storage: u64,
    graphs: u64,
    ready: bool,
}

/// The exact frame remains installed after completion or panic. A panic is
/// terminal uncertainty, never a reason to repoll a possibly poisoned future.
struct Attempt<T> {
    frame: Pin<Box<dyn Future<Output = T> + Send>>,
    output: Option<T>,
    panic: Option<Panic>,
    started: bool,
    returned: bool,
}
impl<T: 'static> Attempt<T> {
    fn new(frame: impl Future<Output = T> + Send + 'static) -> Self {
        Self::boxed(Box::pin(frame))
    }
    fn boxed(frame: Pin<Box<dyn Future<Output = T> + Send>>) -> Self {
        Self { frame, output: None, panic: None, started: false, returned: false }
    }
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        if self.panic.is_some() { return Poll::Ready(Err(anyhow::anyhow!("capture ingress frame panicked; original custody is uncertain"))); }
        if self.returned { return Poll::Ready(Ok(())); }
        self.started = true;
        match catch_unwind(AssertUnwindSafe(|| self.frame.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => {
                // The actual typed output/owner is installed before inspection.
                self.output = Some(output);
                self.returned = true;
                Poll::Ready(Ok(()))
            }
            Err(payload) => {
                self.panic = Some(payload);
                Poll::Ready(Err(anyhow::anyhow!("capture ingress frame panicked; original custody is uncertain")))
            }
        }
    }
    async fn wait(&mut self) -> anyhow::Result<()> { poll_fn(|cx| self.poll(cx)).await }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { Ready, Authorizing, AcquisitionPreflight, AcquisitionReady, Acquired, PreparingBegin, BeginReady, Beginning, Begun, Verifying, Acquiring, Prepared, NativeClaimed, NativePreflight, NativeReady, NativeAuthorizing, NativeReaderClosing, NativeReaderJoined, NativeAttempted, NativeCaptured, PairedClaimed, PairedPreflight, PairedReady, PairedAuthorizing, PairedReaderClosing, PairedReaderJoined, PairedCaptured, Failed, Closing, Closed }
/// A state of SourceImageCaptureOperation, not a separate capture algorithm or
/// reusable source wrapper. No method extracts its source, pool or new writer.
pub(crate) struct RetainedCaptureIngress94 {
    owner: OwnedSourceRelocation94,
    authorization: RestrictedReaderOwner,
    native_authorization: RestrictedReaderOwner,
    paired_authorization: RestrictedReaderOwner,
    paired_completed: bool,
    native_capture: Option<crate::offline_control_upgrade::StageImageCaptureOperation>,
    phase: Phase,
    pool: Option<SqlitePool>,
    acquire: Option<Attempt<SqlResult<Connection>>>,
    acquire_result: Option<SqlResult<()>>,
    connection: Option<Connection>,
    configure: Option<Attempt<(Connection, BorrowedOutcome<SqlResult<()>>)>>,
    configure_result: Option<BorrowedOutcome<SqlResult<()>>>,
    begin: Option<Attempt<SqlResult<Writer>>>,
    begin_result: Option<SqlResult<()>>,
    writer: Option<Writer>,
    identity: Option<usize>,
    settlement: Option<Attempt<(Writer, BorrowedOutcome<anyhow::Result<()>>)>>,
    settlement_result: Option<BorrowedOutcome<anyhow::Result<()>>>,
    handback_started: bool,
    handback_panic: Option<Panic>,
    close: Option<Attempt<SqlResult<()>>>,
    close_result: Option<SqlResult<()>>,
    pool_close: Option<Attempt<()>>,
}

// Source-derived LOGICAL graph-node categories, using the existing94 visitor's
//1024-byte per-node convention. These are not exact private SQLx heap sizes or
// a bound on arbitrary library error/panic payloads. The supported success
// cohort has no user hooks/extensions/parent pool, one connection and waiter.
const NODE: u64 = 1024;
// PoolInner Arc, options Arc, one idle queue allocation/cell, semaphore state,
// close-event registration, one acquire waiter, one return-to-pool task state.
const POOL_NODES: u64 = 8;
// SqliteConnectOptions::new installs exactly16 fixed PRAGMA entries. Include
// their map/backing index, thread-name callback Arc, establishment parameters,
// and two worker-name copies (u64 decimal suffix is at most20 bytes).
const OPTIONS_NODES: u64 = 16 + 2 + 1 + 1;
const WORKER_NAME_BYTES: u64 = 2 * (19 + 20);
// Worker shared-state Arc/mutex, establish oneshot, command channel header/cell,
// row channel header/cell, one rendezvous BEGIN/rollback, shutdown oneshot,
// handle-lock waiter and return-ping oneshot. DBSTATUS is separate, measured
// later from the actual new connection by the existing capture preflight.
const WORKER_NODES: u64 = 10;
// Transaction::begin erases its already-allocated future. Preadmit its visible
// captured ownership/result/await layouts and alignment state, then measure
// the actual erased frame immediately AFTER installing it and BEFORE polling.
// That constructor-allocation window is a documented library limitation, not
// permission to cast a raw connection or assert an exact total heap bound.
const BEGIN_FRAME_STORAGE: u64 = (std::mem::size_of::<Connection>()
    + std::mem::size_of::<Option<std::borrow::Cow<'static, str>>>()
    + std::mem::size_of::<SqlResult<Writer>>()
    + std::mem::size_of::<Pin<Box<dyn Future<Output = SqlResult<()>> + Send>>>()
    + 8 * std::mem::size_of::<usize>()) as u64;

fn acquire_frame(pool: &SqlitePool) -> impl Future<Output = SqlResult<Connection>> + Send + 'static { pool.acquire() }
async fn configure_frame(mut connection: Connection) -> (Connection, BorrowedOutcome<SqlResult<()>>) {
    let outcome = {
        // This future only borrows the separately owned connection. Catch its
        // poll panic here, so the original owner survives the unwind.
        let mut borrowed = std::pin::pin!(crate::projection::configure_connection(&mut connection));
        poll_fn(|cx| match catch_unwind(AssertUnwindSafe(|| borrowed.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(result)) => Poll::Ready(BorrowedOutcome::Returned(result)),
            Err(payload) => Poll::Ready(BorrowedOutcome::Panicked(payload)),
        }).await
    };
    (connection, outcome)
}
async fn settle_frame(mut writer: Writer) -> (Writer, BorrowedOutcome<anyhow::Result<()>>) {
    let outcome = {
        let mut borrowed = std::pin::pin!(crate::motion_snapshot::close_retained_writer(&mut writer));
        poll_fn(|cx| match catch_unwind(AssertUnwindSafe(|| borrowed.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(result)) => Poll::Ready(BorrowedOutcome::Returned(result)),
            Err(payload) => Poll::Ready(BorrowedOutcome::Panicked(payload)),
        }).await
    };
    (writer, outcome)
}
async fn close_frame(connection: Connection) -> SqlResult<()> { connection.close().await }
async fn pool_close_frame(pool: SqlitePool) { pool.close().await }
fn frame_size<A, F, R>(_: F) -> u64 where F: FnOnce(A) -> R, R: Future { std::mem::size_of::<R>() as u64 }
fn borrowed_ingress_size<F, R>(_: F) -> u64
where F: FnOnce(&'static mut RetainedCaptureIngress94) -> R, R: Future {
    std::mem::size_of::<R>() as u64
}
fn borrowed_facade_size<F, R>(_: F) -> u64
where F: FnOnce(&'static mut crate::SourceImageCaptureOperation) -> R, R: Future {
    std::mem::size_of::<R>() as u64
}
fn storage(path_capacity: usize) -> anyhow::Result<u64> {
    let layouts = [
        std::mem::size_of::<RetainedCaptureIngress94>(),
        std::mem::size_of::<crate::SourceImageCaptureOperation>(),
        std::mem::size_of::<Result<crate::SourceImageCaptureOperation, OwnedSourceRelocation94>>(),
        std::mem::size_of::<SqlitePoolOptions>(),
        std::mem::size_of::<SqliteConnectOptions>(),
        std::mem::size_of::<SqlitePool>(),
        std::mem::size_of::<SqlResult<Connection>>(),
        std::mem::size_of::<SqlResult<Writer>>(),
        std::mem::size_of::<(Connection, BorrowedOutcome<SqlResult<()>>)>(),
        std::mem::size_of::<(Writer, BorrowedOutcome<anyhow::Result<()>>)>(),
        std::mem::size_of::<Option<Connection>>(),
        std::mem::size_of::<Option<Writer>>(),
        std::mem::size_of::<Pin<Box<dyn Future<Output = SqlResult<Writer>> + Send>>>(),
    ].into_iter().try_fold(0u64, |n, size| add(&[n, size as u64]))?;
    // New options filename, establish String/CString, and two canonical-name
    // verification temporaries. Original witness/request/94 paths are in H or
    // metadata already and are not included a second time as dynamic graphs.
    let paths = (path_capacity as u64).checked_mul(5).and_then(|n| n.checked_add(1))
        .context("capture ingress path storage overflow")?;
    add(&[layouts, paths,
        frame_size(acquire_frame), frame_size(configure_frame),
        frame_size(settle_frame), frame_size(close_frame), frame_size(pool_close_frame),
        borrowed_ingress_size(RetainedCaptureIngress94::prepare),
        borrowed_ingress_size(RetainedCaptureIngress94::close),
        borrowed_facade_size(crate::SourceImageCaptureOperation::prepare_retained_input),
        borrowed_facade_size(crate::SourceImageCaptureOperation::close),
        NODE * POOL_NODES, NODE * OPTIONS_NODES, WORKER_NAME_BYTES,
        NODE * WORKER_NODES, BEGIN_FRAME_STORAGE,
        authorization::storage(path_capacity)?,
        native_capture::storage(path_capacity)?, paired_capture::storage(path_capacity)?])
}
fn next_host(original_host: u64, new_storage: u64, previously_extra_graphs: u64) -> anyhow::Result<u64> {
    add(&[original_host, new_storage, previously_extra_graphs])
}

impl OwnedSourceRelocation94 {
    fn capture_ingress_dynamic_graphs(&self) -> anyhow::Result<u64> {
        // Existing visitor deliberately includes fixed inline proof/mapping
        // headers and logical Value root nodes. Those actual inline headers
        // already live in old H. Remove only these identified exact layouts,
        // never an unknown K delta or a speculative fraction of a graph.
        let mut roots = usize::from(self.command.is_some());
        roots += 2 * usize::from(matches!(self.preparation.as_ref().map(RelocationProducer::result), Some(Ok(_))));
        roots += usize::from(self.prepared_intent.is_some());
        roots += usize::from(matches!(self.native_outcome.as_ref().map(RelocationProducer::result),
            Some(Ok(CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. }))));
        roots += 2 * self.receipts.iter().filter(|r| matches!(r.result(), Ok(Some(_)))).count();
        let inline_ack = if self.prepared_ack.is_some() {
            roots += 2; // old mapping binding and policy
            2 * std::mem::size_of::<LifecycleProof>() + std::mem::size_of::<SourceMapping>()
        } else { 0 };
        self.retained_graph_bytes()?.checked_sub((inline_ack
            + roots * std::mem::size_of::<serde_json::Value>()) as u64)
            .context("capture ingress graph/header ledger differs")
    }
    /// Borrowed numeric admission only. The actual original cancel owner must be
    /// supplied, not a fresh flag. The host keeps its original service workspace,
    /// caller, selected records and stopped-service locks alongside this owner.
    pub fn admit_capture_ingress(&mut self, cancel: &Arc<AtomicBool>) -> anyhow::Result<()> {
        ensure!(self.capture_input_ready(), "capture ingress requires positive original94 closure");
        ensure!(Arc::as_ptr(cancel) as usize == self.admission.original_cancel,
            "capture ingress requires original cancellation context");
        alive(self.deadline(), cancel)?;
        // Unsupported historical opaque errors are never silently counted as0.
        ensure!(self.preparation.as_ref().is_none_or(RelocationProducer::is_ok)
            && self.native_outcome.as_ref().is_none_or(RelocationProducer::is_ok)
            && self.receipts.iter().all(RelocationProducer::is_ok)
            && self.insert_result.iter().flatten().all(Result::is_ok)
            && self.target.as_ref().is_some_and(|target| target.capture_ingress_history_clean()),
            "capture ingress cannot classify retained94 error or panic graphs");
        let graphs = self.capture_ingress_dynamic_graphs()?;
        let source = self.source.as_ref().context("capture ingress original source absent")?;
        let new_storage = storage(source.inner.capture_ingress_path94()?.capacity())?;
        if self.admission.capture_ingress.is_none() {
            // H already covers request capacities, owned94 layouts and all94
            // retained frames. Only dynamic graphs that lived in extra/K and
            // genuinely NEW simultaneous destinations enter this claim once.
            let host = next_host(source.inner.capture_ingress_host94()?, new_storage, graphs)?;
            self.admission.capture_ingress = Some(Admission {
                cancel: Arc::clone(cancel), host, storage: new_storage, graphs, ready: false,
            });
        }
        let admitted = self.admission.capture_ingress.as_mut().unwrap();
        admitted.ready = false;
        ensure!(admitted.storage == new_storage && admitted.graphs == graphs
            && Arc::ptr_eq(&admitted.cancel, cancel), "capture ingress frozen admission changed");
        let host = admitted.host;
        let deadline = self.deadline();
        self.source.as_mut().unwrap().inner.retain_capture_ingress94(host, deadline, cancel)?;
        self.admission.capture_ingress.as_mut().unwrap().ready = true;
        Ok(())
    }
    /// Moves only into the existing capture owner. Missing admission returns
    /// this entire unchanged owner, not its source or a reconstructed summary.
    pub fn begin_capture_ingress(self) -> Result<crate::SourceImageCaptureOperation, Self> {
        if !self.capture_input_ready()
            || !self.admission.capture_ingress.as_ref().is_some_and(|a| a.ready) {
            return Err(self);
        }
        Ok(crate::SourceImageCaptureOperation::from_retained94(RetainedCaptureIngress94 {
            owner: self, authorization: RestrictedReaderOwner::default(),
            native_authorization: RestrictedReaderOwner::default(),
            paired_authorization: RestrictedReaderOwner::default(), paired_completed: false, native_capture: None,
            phase: Phase::Ready, pool: None, acquire: None, acquire_result: None,
            connection: None, configure: None, configure_result: None, begin: None,
            begin_result: None, writer: None, identity: None, settlement: None,
            settlement_result: None, handback_started: false, handback_panic: None,
            close: None, close_result: None, pool_close: None,
        }))
    }
}
impl RetainedCaptureIngress94 {
    pub(crate) fn deadline(&self) -> Instant { self.owner.deadline() }
    pub(crate) fn prepared(&self) -> bool { self.phase == Phase::Prepared }
    fn cancel(&self) -> Arc<AtomicBool> { Arc::clone(&self.owner.admission.capture_ingress.as_ref().unwrap().cancel) }
    fn start_acquire(&mut self) -> anyhow::Result<()> {
        alive(self.deadline(), &self.cancel())?;
        let source = self.owner.source.as_ref().context("capture ingress source absent")?;
        let path = source.inner.capture_ingress_path94()?;
        // No user callback or maintenance task can run at construction. The
        // exact source path is borrowed; options perform the admitted clone.
        let options = SqliteConnectOptions::new().filename(path).create_if_missing(false)
            .statement_cache_capacity(0).command_buffer_size(1).row_buffer_size(1)
            .foreign_keys(true).busy_timeout(Duration::from_secs(5));
        self.pool = Some(SqlitePoolOptions::new().max_connections(1).min_connections(0)
            .max_lifetime(None).idle_timeout(None).test_before_acquire(false)
            .acquire_timeout(self.deadline().saturating_duration_since(Instant::now()))
            .connect_lazy_with(options));
        // This is SQLx's ACTUAL owned acquire future, installed before any await
        // or first poll, not a marker that will create the future later.
        self.acquire = Some(Attempt::new(acquire_frame(self.pool.as_ref().unwrap())));
        Ok(())
    }
    pub(crate) fn prepare(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let allowed = self.phase == Phase::Ready;
        let claim = if allowed {
            self.phase = Phase::Acquiring;
            self.start_acquire()
        } else { Err(anyhow::anyhow!("capture ingress preparation is one-shot")) };
        async move {
            claim?;
            let result = self.prepare_inner().await;
            self.phase = if result.is_ok() { Phase::Prepared } else { Phase::Failed };
            result
        }
    }
    async fn finish_acquire(&mut self) -> anyhow::Result<()> {
        let attempt = self.acquire.as_mut().context("capture ingress acquire frame missing")?;
        attempt.wait().await?;
        if let Some(result) = attempt.output.take() {
            match result {
                Ok(connection) => { self.connection = Some(connection); self.acquire_result = Some(Ok(())); }
                Err(error) => self.acquire_result = Some(Err(error)),
            }
        }
        ensure!(matches!(self.acquire_result, Some(Ok(()))), "capture ingress acquisition failed; source retained");
        Ok(())
    }
    async fn finish_configure(&mut self) -> anyhow::Result<()> {
        let attempt = self.configure.as_mut().unwrap();
        attempt.wait().await?;
        if let Some((connection, result)) = attempt.output.take() {
            self.connection = Some(connection);
            self.configure_result = Some(result);
        }
        ensure!(matches!(self.configure_result, Some(BorrowedOutcome::Returned(Ok(())))), "capture ingress configuration failed; source retained");
        Ok(())
    }
    async fn finish_begin(&mut self) -> anyhow::Result<()> {
        let attempt = self.begin.as_mut().unwrap();
        attempt.wait().await?;
        if let Some(result) = attempt.output.take() {
            match result {
                Ok(writer) => { self.writer = Some(writer); self.begin_result = Some(Ok(())); }
                Err(error) => self.begin_result = Some(Err(error)),
            }
        }
        // SQLx consumes PoolConnection on failure and provides no recovery
        // output. Do not infer successful close/rollback from an Err or size0.
        ensure!(matches!(self.begin_result, Some(Ok(()))), "capture ingress BEGIN failed; original custody remains opaque");
        Ok(())
    }
    async fn prepare_inner(&mut self) -> anyhow::Result<()> {
        let cancel = self.cancel();
        self.finish_acquire().await?;
        alive(self.deadline(), &cancel)?;
        self.identity = Some(self.owner.source.as_ref().unwrap().inner
            .verify_capture_connection94(self.connection.as_mut().unwrap()).await?);
        alive(self.deadline(), &cancel)?;
        self.configure = Some(Attempt::new(configure_frame(self.connection.take().unwrap())));
        self.finish_configure().await?;
        alive(self.deadline(), &cancel)?;
        // Actual SQLx boxed BEGIN frame takes the SAME pool connection now,
        // before first poll. No raw94 handle is cast/adopted as a Transaction.
        self.begin = Some(Attempt::boxed(Transaction::begin(
            self.connection.take().unwrap(), Some(std::borrow::Cow::Borrowed("BEGIN IMMEDIATE")))));
        ensure!(std::mem::size_of_val(self.begin.as_ref().unwrap().frame.as_ref().get_ref()) as u64
            <= BEGIN_FRAME_STORAGE, "capture ingress erased BEGIN frame exceeds admitted layout; unpolled frame retained");
        self.finish_begin().await?;
        alive(self.deadline(), &cancel)?;
        let deadline = self.deadline();
        self.owner.source.as_mut().unwrap().inner.admit_capture_ingress_writer94(
            self.writer.as_mut().unwrap(), deadline, &cancel).await?;
        alive(deadline, &cancel)
    }
    /// Terminal-only. Never starts an unpolled acquisition or BEGIN merely for
    /// cleanup, never creates a replacement connection, never exposes a writer.
    pub(crate) fn close(&mut self) -> impl Future<Output = anyhow::Result<()>> + '_ {
        let closed = self.phase == Phase::Closed;
        if !closed { self.phase = Phase::Closing; }
        // Claim terminal custody before returning even an unpolled borrower.
        // A dropped close waiter cannot restore Ready or permit preparation.
        async move {
            if closed { Ok(()) } else { self.close_inner().await }
        }
    }
    async fn close_inner(&mut self) -> anyhow::Result<()> {
        // The real auth-only reader must settle before writable/native cleanup.
        // Failed/unknown connect or close cannot masquerade as a positive ACK.
        if self.authorization.is_claimed() {
            self.authorization.settle_inline().await?;
        }
        if self.native_authorization.is_claimed() {
            self.native_authorization.settle_inline().await?;
        }
        if self.paired_authorization.is_claimed() {
            self.paired_authorization.settle_inline().await?;
        }
        // Move only the exact writer into the ORIGINAL settlement graph. The
        // Stage owner keeps Native and every proof/reference/guard in custody.
        if let Some(capture) = &mut self.native_capture {
            if let Some(writer) = capture.terminal_writer() {
                self.writer = Some(writer);
            }
        }
        ensure!(self.handback_panic.is_none(), "capture ingress handback panicked; source retained");
        if let Some(acquire) = &self.acquire {
            if acquire.started { self.finish_acquire().await?; }
        }
        if self.configure.is_some() {
            let result = self.finish_configure().await;
            // A normal configuration Err still returns the SAME concrete
            // connection. Terminal close may settle it; panic does not.
            if self.configure.as_ref().unwrap().panic.is_some()
                || matches!(self.configure_result, Some(BorrowedOutcome::Panicked(_)))
                || self.connection.is_none() {
                result?;
            }
        }
        if let Some(begin) = &self.begin {
            ensure!(begin.started, "unpolled BEGIN retains its exact connection; cleanup cannot start it");
            self.finish_begin().await?;
        }
        if self.writer.is_some() && self.settlement.is_none() {
            self.settlement = Some(Attempt::new(settle_frame(self.writer.take().unwrap())));
        }
        if let Some(settlement) = &mut self.settlement {
            settlement.wait().await?;
            if let Some((writer, result)) = settlement.output.take() {
                self.writer = Some(writer);
                self.settlement_result = Some(result);
            }
            ensure!(matches!(self.settlement_result, Some(BorrowedOutcome::Returned(Ok(())))), "capture ingress writer settlement failed; source retained");
        }
        if !self.handback_started && self.writer.is_some() {
            // Record the one-shot handback before Drop. SQLx's return-to-pool
            // task may fail its ping and hard-close without an observable ACK.
            self.handback_started = true;
            let writer = self.writer.take().unwrap();
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(writer))) {
                self.handback_panic = Some(payload);
                anyhow::bail!("capture ingress handback panicked; source retained");
            }
        }
        if self.handback_started && self.connection.is_none() && self.close.is_none() {
            loop {
                let pool = self.pool.as_ref().unwrap();
                if let Some(connection) = pool.try_acquire() {
                    self.connection = Some(connection); // Retain before checks/await.
                    break;
                }
                ensure!(pool.size() != 0 && !pool.is_closed(),
                    "capture ingress original connection was not returned; close outcome uncertain");
                // No timeout/deadline reset and no replacement acquire. Dropping
                // this waiter retains the original private pool and all tails.
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
        if self.close.is_none() {
            if let Some(connection) = &mut self.connection {
                let mut lease = connection.lock_handle().await?;
                let handle = lease.as_raw_handle().as_ptr();
                if self.handback_started {
                    ensure!(Some(handle as usize) == self.identity,
                        "capture ingress returned connection identity differs");
                }
                ensure!(unsafe { rusqlite::ffi::sqlite3_get_autocommit(handle) } != 0
                    && unsafe { rusqlite::ffi::sqlite3_txn_state(handle, c"main".as_ptr()) } == 0,
                    "capture ingress close transaction remains active");
                drop(lease);
                self.close = Some(Attempt::new(close_frame(self.connection.take().unwrap())));
            }
        }
        if let Some(close) = &mut self.close {
            close.wait().await?;
            if let Some(result) = close.output.take() { self.close_result = Some(result); }
            ensure!(matches!(self.close_result, Some(Ok(()))), "capture ingress exact connection close failed; source retained");
        }
        if let Some(pool) = &self.pool {
            // Pool.close() swallows per-connection errors. It is never our ACK.
            // Before constructing it require either no acquisition poll EVER,
            // or an explicit same-connection Returned(Ok) close, plus size0.
            ensure!(self.acquire.as_ref().is_some_and(|attempt| !attempt.started)
                || matches!(self.close_result, Some(Ok(()))),
                "capture ingress has no positive exact close evidence");
            ensure!(pool.size() == 0 && self.connection.is_none() && self.writer.is_none(),
                "capture ingress pool still owns a connection");
            if self.pool_close.is_none() {
                self.pool_close = Some(Attempt::new(pool_close_frame(pool.clone())));
            }
            self.pool_close.as_mut().unwrap().wait().await?;
        }
        self.owner.settle_target().await?;
        if let Some(capture) = &mut self.native_capture {
            ensure!(matches!(self.close_result, Some(Ok(()))),
                "retained Native release requires exact same-connection close ACK");
            capture.release_after_exact_close()?;
            self.owner.phase = SourceRelocation94Phase::Closed;
        } else {
            self.owner.close_source()?;
        }
        self.phase = Phase::Closed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_claim_adds_only_new_storage_and_previously_extra_graphs() {
        assert_eq!(next_host(100, 20, 30).unwrap(), 150);
        // Refresh reuses this complete frozen150 claim, not150+20+30 or K+50.
        let frozen = next_host(100, 20, 30).unwrap();
        assert_eq!(frozen.max(100), 150);
        assert!(next_host(LIMIT, 1, 0).is_err());
    }
    #[tokio::test]
    async fn returned_frame_keeps_output_and_is_not_repolled() {
        let mut frame = Attempt::new(async { String::from("exact typed output") });
        frame.wait().await.unwrap();
        assert_eq!(frame.output.as_deref(), Some("exact typed output"));
        let output = frame.output.take().unwrap();
        frame.wait().await.unwrap();
        assert!(frame.output.is_none());
        assert_eq!(output, "exact typed output");
    }
    #[tokio::test]
    async fn panicked_frame_is_retained_and_never_repolled() {
        let mut frame = Attempt::new(async { panic!("capture ingress test panic") });
        assert!(frame.wait().await.is_err());
        assert!(frame.panic.is_some());
        assert!(frame.wait().await.is_err());
        assert!(!frame.returned);
    }
}

#[cfg(test)]
#[path = "capture_ingress/primitive_tests.rs"]
mod primitive_tests;
