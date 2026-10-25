// SOURCE-ONLY PROPOSAL. Not compiled, formatted by rustfmt, or executed.
// Integration: load this file as a private cfg(test), explicit-path child
// module of capture_ingress.rs. No production hooks are required for the tests
// below. This file deliberately does NOT fabricate an
// OwnedSourceRelocation94, source session, receipt, ACK, or service continuation.
//
// Scope: actual SQLx 0.8.6 acquire/BEGIN/configure/settle/close ownership primitives
// and the actual Attempt implementation. These are NOT whole94 integration
// tests and do NOT prove the production close() ordering or source86 closure.
// The final comment identifies the genuine existing fixture and missing seams.
use super::*;
use sqlx::{SqliteConnection, TransactionManager};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Wake, Waker},
};

const WAIT: Duration = Duration::from_secs(10);

// This is only a real temporary SQLite database and private max1 SQLx pool.
// It is not an approximation of a native/source/service authorization graph.
struct SqliteFixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    pool: SqlitePool,
}

impl SqliteFixture {
    fn new(busy_timeout: Duration) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture-ingress.sqlite3");
        let database = rusqlite::Connection::open(&path).unwrap();
        database
            .execute_batch("CREATE TABLE ingress_probe (value INTEGER NOT NULL)")
            .unwrap();
        database.close().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false)
            .statement_cache_capacity(0)
            .command_buffer_size(1)
            .row_buffer_size(1)
            .foreign_keys(true)
            .busy_timeout(busy_timeout);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .max_lifetime(None)
            .idle_timeout(None)
            .test_before_acquire(false)
            .acquire_timeout(WAIT)
            .connect_lazy_with(options);
        Self { _directory: directory, path, pool }
    }

    fn normal() -> Self {
        Self::new(Duration::from_secs(5))
    }
}

struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    future.poll(&mut context)
}

fn frame_address<T>(attempt: &Attempt<T>) -> usize {
    // Observation only: never cast/adopt a database handle or transfer owner.
    attempt.frame.as_ref().get_ref() as *const _ as *const () as usize
}

fn assert_returned_ok<E: std::fmt::Display>(outcome: &BorrowedOutcome<Result<(), E>>) {
    // Borrow the actual outcome, preserving its error/panic payload and the
    // separately returned Connection/Writer. An outer Attempt Returned bit
    // alone does not imply its borrowed operation returned successfully.
    match outcome {
        BorrowedOutcome::Returned(Ok(())) => {}
        BorrowedOutcome::Returned(Err(error)) => panic!("borrowed operation returned error: {error}"),
        BorrowedOutcome::Panicked(_) => panic!("borrowed operation panicked; owner was retained"),
    }
}

async fn wait_ok<T: 'static>(attempt: &mut Attempt<T>) {
    tokio::time::timeout(WAIT, attempt.wait())
        .await
        .expect("retained test attempt did not finish")
        .expect("retained test attempt panicked");
}

async fn connection_state(connection: &mut SqliteConnection) -> (usize, bool, i32) {
    let mut lease = connection.lock_handle().await.unwrap();
    let handle = lease.as_raw_handle().as_ptr();
    // SAFETY: both read-only SQLite observations occur under this exclusive
    // SQLx handle lease. Only the numeric identity leaves the lease.
    let autocommit = unsafe { rusqlite::ffi::sqlite3_get_autocommit(handle) } != 0;
    let transaction = unsafe { rusqlite::ffi::sqlite3_txn_state(handle, c"main".as_ptr()) };
    (handle as usize, autocommit, transaction)
}

fn immediate(connection: Connection) -> Attempt<SqlResult<Writer>> {
    // Install SQLx's returned boxed future directly. It has already taken
    // the real PoolConnection, even while started is still false.
    Attempt::boxed(Transaction::begin(
        connection,
        Some(Cow::Borrowed("BEGIN IMMEDIATE")),
    ))
}

fn competing_writer(path: &Path) -> rusqlite::Connection {
    let database = rusqlite::Connection::open(path).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    database
}

async fn returned_only(pool: &SqlitePool) -> Connection {
    // Test waiter, not a production replacement acquisition. It intentionally
    // has NO pool.acquire(), connection creation, retry hook or min pool size.
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(connection) = pool.try_acquire() {
                return connection;
            }
            assert_eq!(pool.size(), 1, "the original connection was lost");
            assert!(!pool.is_closed());
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original connection was not handed back")
}

async fn close_exact(connection: Connection, pool: &SqlitePool) {
    let mut close = Attempt::new(close_frame(connection));
    wait_ok(&mut close).await;
    assert!(matches!(close.output.as_ref(), Some(Ok(()))));
    let close_result = close.output.take();
    assert_eq!(pool.size(), 0);
    let mut pool_close = Attempt::new(pool_close_frame(pool.clone()));
    wait_ok(&mut pool_close).await;
    assert!(pool.is_closed());
    // The unit-valued pool close result does not replace the typed close ACK.
    assert!(matches!(close_result, Some(Ok(()))));
    assert!(close.returned);
}

async fn settle_and_close(writer: Writer, pool: &SqlitePool, identity: usize) {
    let mut settlement = Attempt::new(settle_frame(writer));
    wait_ok(&mut settlement).await;
    let (mut writer, result) = settlement.output.take().unwrap();
    assert_returned_ok(&result);
    assert_eq!(connection_state(&mut writer).await, (identity, true, 0));
    assert_eq!(sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(&writer), 0);
    drop(writer);
    let mut connection = returned_only(pool).await;
    assert_eq!(connection_state(&mut connection).await, (identity, true, 0));
    close_exact(connection, pool).await;
}

#[tokio::test]
async fn unpolled_actual_acquire_and_dropped_waiter_create_no_connection() {
    let fixture = SqliteFixture::normal();
    let mut acquire = Attempt::new(acquire_frame(&fixture.pool));
    let address = frame_address(&acquire);
    drop(acquire.wait());
    tokio::task::yield_now().await;
    assert_eq!(frame_address(&acquire), address);
    assert!(!acquire.started);
    assert!(!acquire.returned);
    assert!(acquire.output.is_none());
    assert!(acquire.panic.is_none());
    assert_eq!(fixture.pool.size(), 0);
    assert!(fixture.pool.try_acquire().is_none());

    // The no-first-poll case can close an empty pool while keeping the exact
    // acquisition frame. It does not execute the acquisition for cleanup.
    let mut pool_close = Attempt::new(pool_close_frame(fixture.pool.clone()));
    wait_ok(&mut pool_close).await;
    assert!(fixture.pool.is_closed());
    assert_eq!(frame_address(&acquire), address);
    assert!(!acquire.started);
}

#[tokio::test]
async fn pending_actual_acquire_survives_waiter_drop_and_returns_original_connection() {
    let fixture = SqliteFixture::normal();
    let mut held = fixture.pool.acquire().await.unwrap();
    let identity = connection_state(&mut held).await.0;
    let mut acquire = Attempt::new(acquire_frame(&fixture.pool));
    let address = frame_address(&acquire);
    {
        let mut waiter = Box::pin(acquire.wait());
        assert!(poll_once(waiter.as_mut()).is_pending());
    }
    assert!(acquire.started);
    assert!(!acquire.returned);
    assert_eq!(frame_address(&acquire), address);
    assert!(acquire.output.is_none());
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());
    drop(held);

    wait_ok(&mut acquire).await;
    assert_eq!(frame_address(&acquire), address);
    assert!(acquire.returned);
    let connection = acquire.output.as_mut().unwrap().as_mut().unwrap();
    assert_eq!(connection_state(connection).await.0, identity);
    assert!(fixture.pool.try_acquire().is_none(), "output owns the connection");
    // Re-polling the completed actual async acquire frame would panic. These
    // repeated waits must return through Attempt's terminal state instead.
    wait_ok(&mut acquire).await;
    let connection = acquire.output.as_mut().unwrap().as_mut().unwrap();
    assert_eq!(connection_state(connection).await.0, identity);
    let connection = acquire.output.take().unwrap().unwrap();
    wait_ok(&mut acquire).await;
    assert!(acquire.output.is_none(), "terminal wait must not reacquire");
    close_exact(connection, &fixture.pool).await;
}

#[tokio::test]
async fn actual_configuration_retains_connection_before_poll_and_after_return() {
    let fixture = SqliteFixture::normal();
    let mut connection = fixture.pool.acquire().await.unwrap();
    let identity = connection_state(&mut connection).await.0;
    let mut configure = Attempt::new(configure_frame(connection));
    let address = frame_address(&configure);
    drop(configure.wait());
    assert!(!configure.started);
    assert!(fixture.pool.try_acquire().is_none());
    assert_eq!(fixture.pool.size(), 1);
    wait_ok(&mut configure).await;
    assert_eq!(frame_address(&configure), address);
    assert!(configure.returned);
    assert!(configure.panic.is_none(), "outer Attempt panic differs from borrowed outcome");
    assert_returned_ok(&configure.output.as_ref().unwrap().1);
    let connection = &mut configure.output.as_mut().unwrap().0;
    assert_eq!(connection_state(connection).await.0, identity);
    let denied: i64 = sqlx::query_scalar("SELECT motion_native_projection('items')")
        .fetch_one(&mut **connection)
        .await
        .unwrap();
    assert_eq!(denied, 0, "configuration installed denial, not authority");
    wait_ok(&mut configure).await;
    let (connection, result) = configure.output.take().unwrap();
    assert_returned_ok(&result);
    close_exact(connection, &fixture.pool).await;
}

#[tokio::test]
async fn actual_begin_owns_connection_before_first_poll_and_retains_typed_writer() {
    let fixture = SqliteFixture::normal();
    let mut connection = fixture.pool.acquire().await.unwrap();
    let identity = connection_state(&mut connection).await.0;
    let mut begin = immediate(connection);
    let address = frame_address(&begin);
    drop(begin.wait());
    assert!(!begin.started);
    assert!(!begin.returned);
    assert!(begin.output.is_none());
    assert_eq!(frame_address(&begin), address);
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());

    // This explicitly resumes the TEST'S attempted BEGIN. It is not terminal
    // cleanup and must not be cited as permission for close() to start BEGIN.
    wait_ok(&mut begin).await;
    let writer = begin.output.as_mut().unwrap().as_mut().unwrap();
    assert_eq!(connection_state(writer).await.0, identity);
    assert_eq!(sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(writer), 1);
    assert!(!connection_state(writer).await.1);
    assert!(fixture.pool.try_acquire().is_none());
    wait_ok(&mut begin).await;
    assert_eq!(frame_address(&begin), address);
    let writer = begin.output.take().unwrap().unwrap();
    wait_ok(&mut begin).await;
    assert!(begin.output.is_none(), "completed frame must not mint a second writer");
    settle_and_close(writer, &fixture.pool, identity).await;
}

#[tokio::test]
async fn actual_pending_begin_survives_waiter_drop_without_recreating_writer() {
    let fixture = SqliteFixture::normal();
    let mut connection = fixture.pool.acquire().await.unwrap();
    let identity = connection_state(&mut connection).await.0;
    let blocker = competing_writer(&fixture.path);
    let mut begin = immediate(connection);
    let address = frame_address(&begin);
    {
        let mut waiter = Box::pin(begin.wait());
        assert!(poll_once(waiter.as_mut()).is_pending());
    }
    assert!(begin.started);
    assert!(!begin.returned);
    assert!(begin.output.is_none());
    assert_eq!(frame_address(&begin), address);
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());
    blocker.execute_batch("ROLLBACK").unwrap();
    wait_ok(&mut begin).await;
    let mut writer = begin.output.take().unwrap().unwrap();
    assert_eq!(connection_state(&mut writer).await.0, identity);
    wait_ok(&mut begin).await;
    assert!(begin.output.is_none());
    settle_and_close(writer, &fixture.pool, identity).await;
}

#[tokio::test]
async fn actual_begin_busy_error_is_retained_and_never_repolled() {
    let fixture = SqliteFixture::new(Duration::ZERO);
    let connection = fixture.pool.acquire().await.unwrap();
    let blocker = competing_writer(&fixture.path);
    let mut begin = immediate(connection);
    let address = frame_address(&begin);
    wait_ok(&mut begin).await;
    assert!(begin.returned, "Attempt returned even though SQLx returned Err");
    assert!(begin.panic.is_none());
    let error_address = match begin.output.as_ref().unwrap() {
        Err(sqlx::Error::Database(error)) => {
            assert_eq!(error.code().as_deref(), Some("5"), "actual SQLITE_BUSY required");
            error.as_ref() as *const _ as *const () as usize
        }
        other => panic!("expected actual BEGIN IMMEDIATE SQLITE_BUSY, got {other:?}"),
    };
    blocker.execute_batch("ROLLBACK").unwrap();
    wait_ok(&mut begin).await;
    assert_eq!(frame_address(&begin), address);
    match begin.output.as_ref().unwrap() {
        Err(sqlx::Error::Database(error)) => {
            assert_eq!(error.as_ref() as *const _ as *const () as usize, error_address);
        }
        other => panic!("terminal error was replaced: {other:?}"),
    }
    // SQLx consumed the connection on Err. No close ACK is invented here.
    // Pool.close is test teardown only, NOT positive source/connection proof.
    tokio::time::timeout(WAIT, fixture.pool.close()).await.unwrap();
    assert!(matches!(begin.output.as_ref(), Some(Err(_))));
}

struct PanicTag(&'static str);
struct PanicAfterPending<T> {
    actual: Pin<Box<dyn Future<Output = T> + Send>>,
    polls: Arc<AtomicUsize>,
    observed_pending: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
}

impl<T> Future for PanicAfterPending<T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        this.polls.fetch_add(1, Ordering::SeqCst);
        assert!(this.actual.as_mut().poll(cx).is_pending(), "test must reach actual Pending");
        this.observed_pending.store(true, Ordering::SeqCst);
        // Deliberate wrapper panic AFTER polling the real SQLx frame. This
        // does not claim SQLx itself naturally emitted the injected panic.
        std::panic::panic_any(PanicTag("exact panic payload"));
    }
}

impl<T> Drop for PanicAfterPending<T> {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn panic_after_pending<T: 'static>(
    actual: Pin<Box<dyn Future<Output = T> + Send>>,
) -> (Attempt<T>, Arc<AtomicUsize>, Arc<AtomicBool>, Arc<AtomicBool>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let observed_pending = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let frame = PanicAfterPending {
        actual,
        polls: Arc::clone(&polls),
        observed_pending: Arc::clone(&observed_pending),
        dropped: Arc::clone(&dropped),
    };
    (Attempt::new(frame), polls, observed_pending, dropped)
}

async fn assert_panicked_frame_retained<T: 'static>(
    attempt: &mut Attempt<T>,
    polls: &AtomicUsize,
    observed_pending: &AtomicBool,
    dropped: &AtomicBool,
) {
    let address = frame_address(attempt);
    assert!(attempt.wait().await.is_err());
    assert!(observed_pending.load(Ordering::SeqCst));
    let panic_address = attempt.panic.as_ref().unwrap().as_ref() as *const _ as *const () as usize;
    assert_eq!(attempt.panic.as_ref().unwrap().downcast_ref::<PanicTag>().unwrap().0,
        "exact panic payload");
    assert!(attempt.started);
    assert!(!attempt.returned);
    assert!(attempt.output.is_none());
    assert!(!dropped.load(Ordering::SeqCst));
    assert!(attempt.wait().await.is_err());
    assert!(attempt.wait().await.is_err());
    assert_eq!(polls.load(Ordering::SeqCst), 1, "poisoned actual frame must never be re-polled");
    assert_eq!(frame_address(attempt), address);
    assert_eq!(attempt.panic.as_ref().unwrap().as_ref() as *const _ as *const () as usize, panic_address);
    assert!(!dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn panic_after_real_pending_acquire_retains_frame_and_never_repolls() {
    let fixture = SqliteFixture::normal();
    let held = fixture.pool.acquire().await.unwrap();
    let (mut attempt, polls, pending, dropped) =
        panic_after_pending(Box::pin(acquire_frame(&fixture.pool)));
    assert_panicked_frame_retained(&mut attempt, &polls, &pending, &dropped).await;
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());
    drop(attempt);
    assert!(dropped.load(Ordering::SeqCst));
    close_exact(held, &fixture.pool).await;
}

#[tokio::test]
async fn panic_after_real_pending_begin_retains_exact_owner_and_never_repolls() {
    let fixture = SqliteFixture::normal();
    let connection = fixture.pool.acquire().await.unwrap();
    let blocker = competing_writer(&fixture.path);
    let actual = Transaction::begin(connection, Some(Cow::Borrowed("BEGIN IMMEDIATE")));
    let (mut attempt, polls, pending, dropped) = panic_after_pending(actual);
    assert_panicked_frame_retained(&mut attempt, &polls, &pending, &dropped).await;
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(attempt.wait().await.is_err());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(!dropped.load(Ordering::SeqCst));
    // End of ownership assertions. Deliberate test teardown discards the
    // poisoned wrapper. Production must NOT treat this Drop/pool.close as
    // successful BEGIN settlement or an explicit same-connection close ACK.
    drop(attempt);
    assert!(dropped.load(Ordering::SeqCst));
    tokio::time::timeout(WAIT, fixture.pool.close()).await.unwrap();
}

#[tokio::test]
async fn exact_settlement_rolls_back_and_try_acquire_handback_preserves_pointer() {
    let fixture = SqliteFixture::normal();
    let mut connection = fixture.pool.acquire().await.unwrap();
    let identity = connection_state(&mut connection).await.0;
    let mut begin = immediate(connection);
    wait_ok(&mut begin).await;
    let mut writer = begin.output.take().unwrap().unwrap();
    sqlx::query("INSERT INTO ingress_probe (value) VALUES (7)")
        .execute(&mut *writer).await.unwrap();
    let mut settlement = Attempt::new(settle_frame(writer));
    let address = frame_address(&settlement);
    drop(settlement.wait());
    assert!(!settlement.started);
    assert!(fixture.pool.try_acquire().is_none());
    wait_ok(&mut settlement).await;
    assert!(settlement.returned);
    assert!(settlement.panic.is_none(), "outer Attempt panic differs from borrowed outcome");
    assert_returned_ok(&settlement.output.as_ref().unwrap().1);
    let writer = &mut settlement.output.as_mut().unwrap().0;
    assert_eq!(connection_state(writer).await, (identity, true, 0));
    assert_eq!(sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(writer), 0);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ingress_probe")
        .fetch_one(&mut **writer).await.unwrap();
    assert_eq!(count, 0, "the actual write was rolled back, not just a flag changed");
    wait_ok(&mut settlement).await;
    assert_eq!(frame_address(&settlement), address);
    let (writer, settlement_result) = settlement.output.take().unwrap();
    assert_returned_ok(&settlement_result);
    drop(writer);
    let mut returned = returned_only(&fixture.pool).await;
    assert_eq!(connection_state(&mut returned).await, (identity, true, 0));
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());
    close_exact(returned, &fixture.pool).await;
    assert_returned_ok(&settlement_result); // Exact borrowed outcome remains retained.
    assert_eq!(frame_address(&settlement), address);
}

#[tokio::test]
async fn actual_explicit_close_owns_connection_unpolled_and_ack_survives_pool_close() {
    let fixture = SqliteFixture::normal();
    let connection = fixture.pool.acquire().await.unwrap();
    let mut close = Attempt::new(close_frame(connection));
    let address = frame_address(&close);
    drop(close.wait());
    assert!(!close.started);
    assert!(close.output.is_none());
    assert_eq!(fixture.pool.size(), 1);
    assert!(fixture.pool.try_acquire().is_none());
    assert!(!fixture.pool.is_closed());
    wait_ok(&mut close).await;
    assert!(close.returned);
    assert!(close.panic.is_none());
    assert!(matches!(close.output.as_ref(), Some(Ok(()))));
    assert_eq!(fixture.pool.size(), 0);
    assert!(!fixture.pool.is_closed(), "exact close ACK precedes pool close here");
    wait_ok(&mut close).await;
    assert_eq!(frame_address(&close), address);
    let close_result = close.output.take();
    wait_ok(&mut close).await;
    assert!(close.output.is_none(), "terminal wait must not rerun close");
    let mut pool_close = Attempt::new(pool_close_frame(fixture.pool.clone()));
    wait_ok(&mut pool_close).await;
    assert!(fixture.pool.is_closed());
    assert!(matches!(close_result, Some(Ok(()))));
    assert_eq!(frame_address(&close), address);
}

/*
INTEGRATION GAP AND MINIMAL HOOK PROPOSAL (not implemented here)

Genuine whole94 source available in the source88 composition:
  motion/src/native_service/pair/restore_input/target/conversion/source_image/
    supervisor/control97_tests/native96_tests/source94_tests.rs
    - Physical94 and physical_fixture(false) produce a real stopped service,
      original cancellation context, source observation, selected pair and94.
  .../source94_tests/positive_close_tests.rs
    - acknowledge94(), custody94(), same_custody94() already establish real
      receipt/ACK history, exact predecessor ownership and target-close success.
Reuse/refactor those fixture helpers in their existing app test parent. Do not
copy their setup into a backend synthetic-owner helper or expose a source/writer
just to make a backend test constructible. The current ingress API is backend
only; the service owner must keep the genuine whole94 graph in its actual slot.

These primitive tests do not cover real whole94 admission rejection/retention,
unpolled prepare()->close(), unpolled BEGIN close refusal, actual close failure,
handback loss/pointer mismatch fail-closed, or source86 ordering. They also do not
prove a panic from SQLx itself; panic tests explicitly inject a wrapper panic
after a real SQLx Pending poll and verify that its actual frame remains retained.
The new BorrowedOutcome::Panicked branch of configure_frame/settle_frame is NOT
exercised here: doing that faithfully needs an inner borrowed-future poll seam,
not an outer wrapper panic or a synthetic outcome presented as actual SQLx.
No frame/flag-only result should be reported as a genuine service94 pass.

Suggested bounded cfg(test) seams, using current source89 names as anchors
(line numbers are approximate because production is being edited concurrently):
1. In capture_ingress.rs near RetainedCaptureIngress94 fields (lines63-83), add
   one read-only numeric snapshot, available through the existing service fixture
   path only: phase; original cancel address; unchanged deadline; acquire/begin/
   settlement/close frame addresses and started/returned/panic bits; typed-result
   discriminants; pool size/closed; handback_started; recorded identity; original94
   retained custody observations. Do not return pool, Transaction, Connection,
   source session, cloneable owner or mutation access. Do not add a capture grant.
2. At start_acquire immediately after installing self.acquire (about line277),
   no async hook is needed for prepare()'s synchronous claim: simply construct
   then drop prepare() and inspect the actual retained owner through that snapshot.
3. Immediately after installing self.begin and before finish_begin (about341-345),
   add a bounded per-instance, cfg(test) pause. Its blocked waiter may be dropped
   while the real BEGIN remains unpolled; close must refuse without first poll,
   source close or replacement acquisition. Reuse existing fixture pause-release
   guard style so assertion panic cannot strand an unrelated service worker.
4. For dropped begun waiters, pause AFTER the actual Attempt has returned Pending,
   not before creating its frame or via a marker future. A single optional
   per-instance test pause in Attempt::poll's Pending arm can cover acquire,
   BEGIN, settlement and close; its counters must distinguish actual SQLx polls
   from wrapper waits. Do not use global mutable hooks across parallel tests.
5. Before pool_close_frame construction (about440), and before owner.close_source
   (about445), record bounded monotonic test ordinals plus actual result tags.
   Full fixture assertions require exact close Returned(Ok), same pointer and
   autocommit/txn-state proof, then pool close completion, then real source86
   close. Pool size0, dropped transaction, closed pool and phase flags are not ACKs.
6. Exercise genuine error paths only with controlled actual library/source86
   failure seams that retain their real owners and typed outputs. Replacing a
   close future with async { Err(...) } does not test PoolConnection.close; a
   fabricated source owner cannot test86. Any such fault-injection coverage must
   be named and qualified separately from unmodified SQLx behavior.

Future real-fixture matrix after the bounded service-to-ingress path exists:
- Fresh/saved/reconciled genuine94 history, original cancel/deadline and whole
  predecessor graph retained; no image/native95/control95 effect or writer export.
- Wrong cancel and missing admission return/retain the entire original94 owner.
- Drop unpolled prepare, drop actually-pending acquire, and drop immediately
  after BEGIN installation; assert one-shot claims and exact frame identities.
- After successful BEGIN, dropped waiters resume only those exact frames for
  terminal settlement; handback uses try_acquire only, with no second creation.
- Lost handback, mismatch, actual BEGIN error/panic and actual close error/panic
  remain source-retaining; unchanged/no-repoll evidence persists after retry.
- Exact close success precedes pool close and genuine86 source close, while
  source-close failure leaves retained evidence available for supported retry.
*/
