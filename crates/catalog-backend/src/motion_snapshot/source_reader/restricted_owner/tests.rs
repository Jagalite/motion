use super::*;
use crate::motion_snapshot::source_reader::tests::fixture;
use sqlx::Connection as _;
use std::{
    future::poll_fn,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

fn path(root: &tempfile::TempDir) -> PathBuf {
    root.path().join("motion.sqlite3")
}

#[tokio::test]
async fn actual_setup_cancellation_retains_fd_connection_and_opening_until_real_close() {
    let (root, pool, mut writer) = fixture().await;
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.install_path(path(&root)).unwrap();
    let fd = owner.file_fd();
    let entered = owner.set_setup_fault(SetupFault::Pending);
    {
        let mut acquiring = pin!(owner.acquire());
        poll_fn(|cx| {
            assert!(acquiring.as_mut().poll(cx).is_pending());
            if entered.load(Ordering::Acquire) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
    assert!(owner.has_connection());
    assert_eq!(owner.state, ReaderState::SettingUp);
    let original_opening = owner.connect_address();
    assert_eq!(owner.file_fd(), fd);
    // SAFETY: non-owning observation of the same retained descriptor.
    assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    assert!(owner.acquire().await.is_err());
    assert!(owner.claim().is_err());
    owner.begin_close().unwrap();
    let actual_close = owner.close_address();
    let waiter = owner.join_close();
    drop(waiter);
    assert_eq!(owner.connect_address(), original_opening);
    assert_eq!(owner.close_address(), actual_close);
    owner.join_close().await.unwrap();
    assert!(owner.close_joined());
    assert_eq!(owner.close_address(), actual_close);
    assert_eq!(owner.connect_address(), original_opening);
    assert_eq!(owner.file_fd(), fd);
    assert!(owner.claim().is_err());
    drop(owner);
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn actual_setup_error_keeps_returned_connection_closeable_without_reopen() {
    let (root, pool, mut writer) = fixture().await;
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.install_path(path(&root)).unwrap();
    let entered = owner.set_setup_fault(SetupFault::Error);
    assert!(owner.acquire().await.is_err());
    assert!(entered.load(Ordering::Acquire));
    assert!(owner.has_connection());
    assert_eq!(owner.state, ReaderState::SetupFailed);
    let opening = owner.connect_address();
    assert!(owner.acquire().await.is_err());
    owner.settle_inline().await.unwrap();
    assert!(owner.close_joined());
    assert_eq!(owner.connect_address(), opening);
    assert!(owner.reader_mut().is_err());
    drop(owner);
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn actual_setup_panic_retains_connection_and_never_reenters_setup() {
    let (root, pool, mut writer) = fixture().await;
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.install_path(path(&root)).unwrap();
    let entered = owner.set_setup_fault(SetupFault::Panic);
    {
        let mut acquiring = pin!(owner.acquire());
        poll_fn(|cx| {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                acquiring.as_mut().poll(cx)
            })) {
                Err(_) => Poll::Ready(()),
                Ok(Poll::Pending) => Poll::Pending,
                Ok(Poll::Ready(_)) => {
                    panic!("fault must unwind after an actual connection is installed")
                }
            }
        })
        .await;
    }
    assert!(entered.load(Ordering::Acquire));
    assert!(owner.has_connection());
    assert_eq!(owner.state, ReaderState::SettingUp);
    let opening = owner.connect_address();
    assert!(owner.acquire().await.is_err());
    owner.settle_inline().await.unwrap();
    assert!(owner.close_joined());
    assert_eq!(owner.connect_address(), opening);
    drop(owner);
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

#[tokio::test]
async fn real_sqlx_connect_error_stays_unresolved_and_cannot_be_repolled() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("not-sqlite");
    std::fs::write(&path, b"not a sqlite database").unwrap();
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.install_path(path).unwrap();
    assert!(owner.acquire().await.is_err());
    let opening = owner.connect_address();
    // SQLx can fail its own internal PRAGMA after establishing a connection.
    // We retain the future and gate, but must not claim recovery or shutdown.
    assert_eq!(owner.state, ReaderState::ConnectFailed);
    assert!(owner.begin_close().is_err());
    assert!(owner.join_close().await.is_err());
    assert!(owner.acquire().await.is_err());
    assert!(!owner.is_settled());
    assert_eq!(owner.connect_address(), opening);
}

struct PanicConnect(Arc<AtomicUsize>);
impl Future for PanicConnect {
    type Output = Result<SqliteConnection, sqlx::Error>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("connect unwind seam");
    }
}

#[tokio::test]
async fn panicked_connect_box_is_retained_and_never_polled_twice() {
    let polls = Arc::new(AtomicUsize::new(0));
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.opening = Some(Box::pin(PanicConnect(polls.clone())));
    owner.state = ReaderState::Opening;
    let original = owner.connect_address();
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.poll_connect(&mut cx)))
            .is_err()
    );
    assert!(owner.poll_connect(&mut cx).is_ready());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(owner.connect_address(), original);
    assert!(!owner.is_settled());
    assert!(owner.join_close().await.is_err());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pending_opening_cleanup_resumes_the_exact_future_then_observes_real_close() {
    let (root, pool, mut writer) = fixture().await;
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.install_path(path(&root)).unwrap();
    let polls = Arc::new(AtomicUsize::new(0));
    let observed = polls.clone();
    let options = SqliteConnectOptions::new()
        .filename(owner.path().unwrap())
        .read_only(true);
    owner.opening = Some(Box::pin(async move {
        poll_fn(|cx| {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        SqliteConnection::connect_with(&options).await
    }));
    owner.state = ReaderState::Opening;
    let original = owner.connect_address();
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(owner.poll_connect(&mut cx).is_pending());
    owner.begin_close().unwrap();
    drop(owner.join_close());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    owner.join_close().await.unwrap();
    assert!(owner.close_joined());
    assert_eq!(owner.connect_address(), original);
    assert!(polls.load(Ordering::SeqCst) >= 2);
    drop(owner);
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

#[test]
fn reservation_reports_inline_and_both_retained_frame_shares() {
    assert_eq!(
        RestrictedReaderOwner::storage_reservation(),
        std::mem::size_of::<RestrictedReaderOwner>() as u64 + 128 * 1024
    );
    assert_eq!(
        RestrictedReaderOwner::opening_frame_reservation(),
        64 * 1024
    );
    eprintln!(
        "restricted reader inline bytes: {}; admitted connect frame: {}; close frame: {}",
        std::mem::size_of::<RestrictedReaderOwner>(),
        MAX_CONNECT_FRAME,
        MAX_CLOSE_FRAME
    );
}

#[tokio::test]
async fn failed_preconnect_identity_check_is_never_retried_and_never_mints_close_ack() {
    let (root, pool, mut writer) = fixture().await;
    let mut owner = RestrictedReaderOwner::default();
    owner.claim().unwrap();
    owner.install_path(path(&root)).unwrap();
    let moved = root.path().join("moved.sqlite3");
    std::fs::rename(owner.path().unwrap(), &moved).unwrap();
    assert!(owner.acquire().await.is_err());
    assert!(!owner.retains_acquisition());
    assert_eq!(owner.state, ReaderState::PreparingConnect);
    std::fs::rename(&moved, owner.path().unwrap()).unwrap();
    assert!(owner.acquire().await.is_err());
    owner.settle_inline().await.unwrap();
    assert!(owner.is_settled());
    assert!(!owner.close_joined());
    assert!(!owner.retains_close());
    drop(owner);
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}

struct PanicClose(Arc<AtomicUsize>);
impl Future for PanicClose {
    type Output = Result<(), sqlx::Error>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("close unwind seam");
    }
}
#[tokio::test]
async fn panic_and_error_close_keep_terminal_boxes_without_retry_or_ack() {
    let polls = Arc::new(AtomicUsize::new(0));
    let mut owner = RestrictedReaderOwner::default();
    owner.close = Some(Box::pin(PanicClose(polls.clone())));
    owner.state = ReaderState::Closing;
    let original = owner.close_address();
    let mut cx = Context::from_waker(std::task::Waker::noop());
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.poll_close(&mut cx)))
            .is_err()
    );
    assert!(owner.join_close().await.is_err());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(owner.close_address(), original);
    assert!(!owner.is_settled());
    let mut failed = RestrictedReaderOwner::default();
    failed.close = Some(Box::pin(async { Err(sqlx::Error::WorkerCrashed) }));
    failed.state = ReaderState::Closing;
    let original = failed.close_address();
    assert!(failed.join_close().await.is_err());
    assert!(failed.join_close().await.is_err());
    assert_eq!(failed.close_address(), original);
    assert!(!failed.close_joined());
    assert!(!failed.is_settled());
}

// Authored source-only regression for source91's fixed acquisition policy.
#[tokio::test]
async fn actual_capture_reader_disables_statement_cache_without_changing_existing_readers() {
    let (root, pool, mut writer) = fixture().await;
    for uncached in [false, true] {
        let mut owner = RestrictedReaderOwner::default();
        owner.claim().unwrap();
        owner.install_path(path(&root)).unwrap();
        if uncached { owner.acquire_uncached().await.unwrap(); }
        else { owner.acquire().await.unwrap(); }
        let opening = owner.connect_address();
        let fd = owner.file_fd();
        for _ in 0..2 {
            let _: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema")
                .fetch_one(owner.reader_mut().unwrap()).await.unwrap();
        }
        let count = owner.reader_mut().unwrap().cached_statements_size();
        if uncached { assert_eq!(count, 0, "capture retains no Rust statement-cache entries/keys"); }
        else { assert!(count > 0, "existing caller's default cache policy is unchanged"); }
        assert!(sqlx::query("CREATE TABLE forbidden_capture(value)")
            .execute(owner.reader_mut().unwrap()).await.is_err());
        owner.begin_close().unwrap();
        let close = owner.close_address();
        drop(owner.join_close());
        assert!(!owner.close_joined());
        assert_eq!(owner.connect_address(), opening);
        assert_eq!(owner.close_address(), close);
        assert_eq!(owner.file_fd(), fd);
        owner.join_close().await.unwrap();
        assert!(owner.close_joined());
        assert!(owner.acquire_uncached().await.is_err());
    }
    writer.close().await.unwrap();
    drop(writer);
    pool.close().await;
}
