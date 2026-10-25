use super::*;
use std::future::Future;
use std::{
    io::Write,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    time::Duration,
};

async fn fixture_ready() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    StageRestoreOperation,
    RestoreSealSelection,
) {
    let (root, path, native, upgraded, selected) =
        crate::offline_control_upgrade::source_session::tests::upgraded_fixture_with_record(true)
            .await;
    let prepared = upgraded
        .prepare_native_reserved(
            &native,
            catabolic_store::migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
            "reservation-native-transition",
            1024 * 1024,
        )
        .unwrap();
    let session = prepared
        .apply(None)
        .unwrap()
        .save_command("reservation-bootstrap", &[])
        .unwrap()
        .execute_unacknowledged(
            &[],
            catabolic_store::catalog_commands::ReceiptPolicy {
                retention: Duration::from_secs(3600),
                max_receipts: 100,
                max_hold_records: 100,
                max_logical_bytes: 16 * 1024 * 1024,
            },
        )
        .unwrap()
        .acknowledge()
        .unwrap()
        .into_source_session()
        .await
        .unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let image = match session
        .into_current_image(tx, &AtomicBool::new(false))
        .await
    {
        Ok(image) => image,
        Err(failure) => panic!("genuine reservation image: {}", failure.error()),
    };
    let mut image = match image.into_restore_operation() {
        Err(image) => image,
        Ok(_) => panic!("unadmitted image handed off"),
    };
    let selected = selected.unwrap();
    let cancel = AtomicBool::new(false);
    let selected_storage = selected.retained_allocations().unwrap();
    drop(image.admit_restore_operation(0, selected_storage, &cancel));
    let original_deadline = image.restore_admission.as_ref().unwrap().deadline;
    let mut image = match image.into_restore_operation() {
        Err(image) => image,
        Ok(_) => panic!("unpolled admission permitted owned handoff"),
    };
    image
        .admit_restore_operation(0, selected_storage, &cancel)
        .await
        .unwrap();
    assert_eq!(
        image.restore_admission.as_ref().unwrap().deadline,
        original_deadline
    );
    let operation = match image.into_restore_operation() {
        Ok(operation) => operation,
        Err(_) => panic!("actual admitted handoff refused"),
    };
    (root, pool, operation, selected)
}
async fn fixture() -> (tempfile::TempDir, sqlx::SqlitePool, StageRestoreOperation) {
    let (root, pool, mut operation, selected) = fixture_ready().await;
    operation
        .prepare(selected, &AtomicBool::new(false))
        .await
        .unwrap();
    (root, pool, operation)
}
fn publish(root: &std::path::Path, bytes: &[u8]) -> (File, File) {
    let path = root.join(durable::RESERVATION_NAME.to_str().unwrap());
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    output.write_all(bytes).unwrap();
    // Actual fixed create-only publication; backend establishes file+dir fsync.
    (File::open(root).unwrap(), File::open(path).unwrap())
}
fn identity(file: &File) -> (u64, u64) {
    let meta = file.metadata().unwrap();
    (meta.dev(), meta.ino())
}
fn descriptors(owner: &StageRestoreOperation) -> (i32, i32, (u64, u64), (u64, u64)) {
    let (parent, record) = match owner.files.as_ref().expect("retained descriptors") {
        Files::Pending(files) => files.descriptors_for_test(),
        Files::Verified(files) => files.descriptors_for_test(),
    };
    (
        parent.as_raw_fd(),
        record.as_raw_fd(),
        identity(parent),
        identity(record),
    )
}
fn stage(
    root: &std::path::Path,
    owner: &mut StageRestoreOperation,
) -> (i32, i32, (u64, u64), (u64, u64)) {
    let (parent, record) = publish(root, owner.canonical_record().unwrap());
    let expected = (
        parent.as_raw_fd(),
        record.as_raw_fd(),
        identity(&parent),
        identity(&record),
    );
    if let Err(failure) = owner.stage_reservation(parent, record) {
        panic!("stage descriptors: {}", failure.error);
    }
    expected
}
async fn close(mut owner: StageRestoreOperation, pool: sqlx::SqlitePool) {
    // Test-only inspection never reconstructs a public failed-phase grant.
    let image = owner.image.as_mut().expect("original native owner");
    assert!(image.native.retained_allocations().unwrap() > 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_paired_restore_intents")
        .fetch_one(&mut *image.writer)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    owner.close().await.unwrap();
    assert_eq!(owner.phase, Phase::Closed);
    assert!(owner.image.is_none());
    owner.close().await.unwrap();
    drop(owner);
    pool.close().await;
}
#[tokio::test]
async fn genuine_reservation_preserves_seal_deadline_and_original_writer() {
    let (root, pool, mut owner) = fixture().await;
    let deadline = owner.deadline();
    let digest = owner.sha256().unwrap().to_owned();
    let expected = stage(root.path(), &mut owner);
    owner.reserve(&AtomicBool::new(false)).await.unwrap();
    assert_eq!(owner.phase, Phase::Reserved);
    assert_eq!(owner.deadline(), deadline);
    assert_eq!(owner.sha256().unwrap(), digest);
    assert_eq!(descriptors(&owner), expected);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_paired_restore_acks")
        .fetch_one(owner.writer_in().await.unwrap())
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(
        root.path()
            .join(durable::RESERVATION_NAME.to_str().unwrap())
            .exists()
    );
    close(owner, pool).await;
}
#[tokio::test]
async fn genuine_reservation_returns_descriptors_after_post_fsync_cancel() {
    let (root, pool, mut owner) = fixture().await;
    let deadline = owner.deadline();
    let expected = stage(root.path(), &mut owner);
    let cancel = AtomicBool::new(false);
    let error = owner.reserve_inner(&cancel, 1).await.unwrap_err();
    assert!(cancel.load(Ordering::Acquire));
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(owner.phase, Phase::Failed);
    assert_eq!(owner.deadline(), deadline);
    assert_eq!(descriptors(&owner), expected);
    assert!(matches!(owner.files, Some(Files::Verified(_))));
    assert!(owner.reserve(&AtomicBool::new(false)).await.is_err());
    close(owner, pool).await;
}
#[tokio::test]
async fn genuine_reservation_refuses_changed_sealed_inventory_with_custody() {
    let (root, pool, mut owner) = fixture().await;
    let expected = stage(root.path(), &mut owner);
    assert_eq!(
        sqlx::query("UPDATE invitation_attempts SET attempts=attempts+1 WHERE singleton=1")
            .execute(owner.writer_in().await.unwrap())
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    let error = owner.reserve(&AtomicBool::new(false)).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sealed Motion inventory changed")
    );
    assert_eq!(descriptors(&owner), expected);
    assert!(owner.writer_in().await.is_err());
    close(owner, pool).await;
}
#[tokio::test]
async fn genuine_reservation_deadline_refusal_keeps_exact_descriptors() {
    let (root, pool, mut owner) = fixture().await;
    let expected = stage(root.path(), &mut owner);
    let expired = Instant::now() - Duration::from_secs(1);
    owner.admission.deadline = expired;
    owner.input.as_mut().unwrap().deadline = expired;
    let error = owner.reserve(&AtomicBool::new(false)).await.unwrap_err();
    assert!(error.to_string().contains("deadline"));
    assert_eq!(descriptors(&owner), expected);
    close(owner, pool).await;
}
// Test-only safe polling harness: the owner remains outside the panic boundary.
// No task/JoinHandle becomes sole custody and no unsafe Send/Sync is asserted.
struct Catch<F: std::future::Future>(std::pin::Pin<Box<F>>);
impl<F: std::future::Future> std::future::Future for Catch<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send>>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.get_mut().0.as_mut().poll(cx)
        })) {
            Ok(std::task::Poll::Ready(output)) => std::task::Poll::Ready(Ok(output)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(error) => std::task::Poll::Ready(Err(error)),
        }
    }
}
#[tokio::test]
async fn genuine_post_fsync_unwind_retains_exact_seal_native_owner_and_fds() {
    let (root, pool, mut owner) = fixture().await;
    let deadline = owner.deadline();
    let expected = stage(root.path(), &mut owner);
    let cancel = AtomicBool::new(false);
    assert!(
        Catch(Box::pin(owner.reserve_inner(&cancel, 2)))
            .await
            .is_err()
    );
    assert_eq!(owner.phase, Phase::Reserving);
    assert_eq!(owner.deadline(), deadline);
    assert_eq!(descriptors(&owner), expected);
    assert!(matches!(owner.files, Some(Files::Verified(_))));
    assert!(owner.verify(&cancel).await.is_err());
    assert!(owner.reserve(&cancel).await.is_err());
    close(owner, pool).await;
}
#[tokio::test]
async fn genuine_unpolled_handoffs_preserve_exact_reservation_and_image_custody() {
    let (root, pool, mut owner) = fixture().await;
    let expected = stage(root.path(), &mut owner);
    let cancel = AtomicBool::new(false);
    drop(owner.reserve(&cancel));
    assert_eq!(owner.phase, Phase::Reserving);
    assert_eq!(descriptors(&owner), expected);
    assert!(owner.reserve(&cancel).await.is_err());
    close(owner, pool).await;
    let (_root, pool, mut owner, selected) = fixture_ready().await;
    drop(owner.prepare(selected, &cancel));
    assert_eq!(owner.phase, Phase::Preparing);
    assert!(owner.image.is_some());
    assert!(owner.input.is_some());
    assert!(owner.canonical_record().is_err());
    close(owner, pool).await;
}
#[tokio::test]
async fn genuine_mid_await_drop_keeps_caller_slot_custody_and_refuses_retry() {
    let (root, pool, mut owner) = fixture().await;
    let expected = stage(root.path(), &mut owner);
    let cancel = AtomicBool::new(false);
    let mut future = Box::pin(owner.reserve_inner(&cancel, 3));
    let first = std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await;
    assert!(first.is_pending());
    drop(future);
    assert_eq!(owner.phase, Phase::Reserving);
    assert_eq!(descriptors(&owner), expected);
    assert!(owner.reserve(&cancel).await.is_err());
    close(owner, pool).await;
}
