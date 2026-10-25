//! Genuine activated native40 + sealed original93 -> recorded Ready97 producer.
//! No copied owner, synthetic receipt, schema edit or disabled qualification gate.
use super::*;
use crate::offline_control_upgrade::{OfflineControlShutdownFacts, RecordedControlOperation};
use std::{
    borrow::Cow,
    fs::File,
    io::Cursor,
    os::fd::AsRawFd,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::Duration,
};

struct Fixture {
    _root: tempfile::TempDir,
    _stage: tempfile::TempDir,
    _original: File,
    native: PathBuf,
    motion: PathBuf,
    ready: RecordedAdmittedControl,
}
fn quiescence() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("native.sqlite3");
    let original_path = root.path().join("motion.sqlite3");
    crate::initialize(native.clone()).await.unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&original_path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("../../migrations");
    migrations.migrations = Cow::Owned(
        migrations
            .iter()
            .filter(|m| m.version <= 93)
            .cloned()
            .collect(),
    );
    migrations.run(&pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Title','video'); INSERT INTO item_origins VALUES('work','Title');")
        .execute(&pool).await.unwrap();
    let backend = crate::NativeCatalog::bind(
        pool.clone(),
        native.clone(),
        crate::inspect(native.clone()).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend.freeze_motion_source("retained96", 0).await.unwrap();
    backend.capture_motion_import("retained96").await.unwrap();
    backend.stage_import("retained96").await.unwrap();
    backend.prepare_motion_cutover("retained96").await.unwrap();
    backend
        .activate_motion_cutover(Duration::from_secs(30))
        .await
        .unwrap();
    drop(backend);
    let cancel = Arc::new(AtomicBool::new(false));
    let (writer, image) = crate::motion_snapshot::capture(&pool, cancel.clone())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    image.copy_verified_to(&mut bytes, &cancel).unwrap();
    writer.rollback().await.unwrap();
    pool.close().await;
    let inspected = crate::motion_snapshot::image_read::inspect(
        Cursor::new(&bytes),
        bytes.len() as u64,
        0,
        &cancel,
    )
    .unwrap();
    let stage = tempfile::tempdir_in(root.path()).unwrap();
    let motion = stage.path().join("stage.sqlite3");
    std::fs::write(&motion, &bytes).unwrap();
    let source = stage.path().join("original93.sqlite3");
    std::fs::write(&source, &bytes).unwrap();
    let mut original = File::open(source).unwrap();
    let mut operation = RecordedControlOperation::begin(
        File::open(&motion).unwrap(),
        File::open(stage.path()).unwrap(),
    );
    operation
        .prepare(
            &motion,
            &mut original,
            &inspected,
            "retained96-record",
            &catabolic_core::sha256(b"selected-pair"),
            &OfflineControlShutdownFacts {
                service_admission_stopped: true,
                native_workers_joined: true,
                application_owners_drained: true,
                exclusive_operational_ownership_retained: true,
            },
            0,
            Instant::now() + Duration::from_secs(60),
            &cancel,
        )
        .await
        .unwrap();
    operation.apply(&cancel).unwrap();
    let ready = match operation.into_admitted() {
        Ok(ready) => ready,
        Err(_) => panic!("genuine recorded97 did not finish"),
    };
    Fixture {
        _root: root,
        _stage: stage,
        _original: original,
        native,
        motion,
        ready,
    }
}
fn operation(
    mut fixture: Fixture,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    File,
    PathBuf,
    PathBuf,
    NativeIntentPreparationOperation,
) {
    fixture
        .ready
        .admit_native_preparation(
            &fixture.native,
            quiescence(),
            "native96",
            Instant::now() + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .unwrap();
    let operation = match fixture.ready.begin_native_preparation() {
        Ok(operation) => operation,
        Err(_) => panic!("admitted move refused"),
    };
    (
        fixture._root,
        fixture._stage,
        fixture._original,
        fixture.native,
        fixture.motion,
        operation,
    )
}
// Operator-mechanics helpers deliberately keep preflight and effects separate.
// The application additionally brackets each effect with real temporal checks.
fn prepare(
    operation: &mut NativeIntentPreparationOperation,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    ensure!(
        operation.phase() == NativeIntentPreparationPhase::Inputs,
        "test prepare cannot retry"
    );
    operation.preflight_for_next_effect(cancel)?;
    operation.prepare(cancel)
}
fn commit(
    operation: &mut NativeIntentPreparationOperation,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    ensure!(
        operation.phase() == NativeIntentPreparationPhase::PreparedAwaitingAuthorization,
        "test commit cannot retry"
    );
    operation.preflight_for_next_effect(cancel)?;
    operation.insert_intent(cancel)?;
    operation.commit_intent(cancel)
}
fn writer_available(path: &Path) -> bool {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let file = File::options()
        .read(true)
        .write(true)
        .open(PathBuf::from(lock))
        .unwrap();
    // Independent file description tests the actual retained native flock.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}
fn no_intent(path: &Path) {
    let db = Connection::open(path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM catabolic_native_bootstrap_intents",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
fn live_custody(control: &RecordedAdmittedControl) {
    let (_, target, parent, record, _, _) = control.native_custody_for_test();
    for fd in [target, parent, record] {
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }
}

#[tokio::test]
async fn retained96_requires_borrowed_admission_and_preserves_original_deadline() {
    let fixture = fixture().await;
    let before = fixture.ready.native_custody_for_test();
    let mut ready = match fixture.ready.begin_native_preparation() {
        Err(ready) => ready,
        Ok(_) => panic!("unadmitted owner moved"),
    };
    assert_eq!(ready.native_custody_for_test(), before);
    let tighter = Instant::now() + Duration::from_secs(15);
    ready
        .admit_native_preparation(
            &fixture.native,
            quiescence(),
            "native96",
            tighter,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(
        writer_available(&fixture.native),
        "admission acquired native resources"
    );
    let mut operation = match ready.begin_native_preparation() {
        Ok(operation) => operation,
        Err(_) => panic!("admitted move refused"),
    };
    assert_eq!(operation.deadline(), before.4.min(tighter));
    assert_eq!(operation.control.native_custody_for_test(), before);
    prepare(&mut operation, &AtomicBool::new(true)).unwrap_err();
    assert!(prepare(&mut operation, &AtomicBool::new(false)).is_err());
    assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
    assert_eq!(operation.control.native_custody_for_test(), before);
    live_custody(&operation.control);
    no_intent(&fixture.motion);
}

#[tokio::test]
async fn retained96_cancel_and_unwind_retain_actual_native_and_motion_owners() {
    for point in [1, 2, 3, 4, 5] {
        for mode in [1, 2] {
            let (_root, _stage, original, native, motion, mut operation) =
                operation(fixture().await);
            let before = operation.control.native_custody_for_test();
            let original_fd = original.as_raw_fd();
            operation.admitted.fault = Some((point, mode));
            let cancel = AtomicBool::new(false);
            let result = catch_unwind(AssertUnwindSafe(|| prepare(&mut operation, &cancel)));
            if mode == 1 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
            assert_eq!(operation.control.native_custody_for_test(), before);
            assert_eq!(original.as_raw_fd(), original_fd);
            assert!(prepare(&mut operation, &AtomicBool::new(false)).is_err());
            assert_eq!(
                operation.commit_state(),
                NativeIntentCommitState::NotAttempted
            );
            live_custody(&operation.control);
            assert_eq!(writer_available(&native), point < 3);
            no_intent(&motion);
            operation.settle_motion().unwrap();
            live_custody(&operation.control);
            assert_eq!(
                writer_available(&native),
                point < 3,
                "settlement must not drop native custody"
            );
        }
    }
}

#[tokio::test]
async fn retained96_expired_queue_and_refused_admission_are_terminal_without_native_acquisition() {
    let mut trial = fixture().await;
    let before = trial.ready.native_custody_for_test();
    assert!(
        trial
            .ready
            .admit_native_preparation(
                &trial.native,
                quiescence(),
                "native96",
                Instant::now() - Duration::from_secs(1),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(
        trial
            .ready
            .admit_native_preparation(
                &trial.native,
                quiescence(),
                "native96",
                Instant::now() + Duration::from_secs(60),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(trial.ready.native_custody_for_test(), before);
    assert!(writer_available(&trial.native));
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    // Test-only tightening, never a fresh deadline or wider grant.
    operation.admitted.deadline = Instant::now() - Duration::from_secs(1);
    assert!(prepare(&mut operation, &AtomicBool::new(false)).is_err());
    assert!(writer_available(&native));
    no_intent(&motion);
}

#[tokio::test]
async fn retained96_success_stops_before_intent_and_holds_prepared_native_owner() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    let before = operation.control.native_custody_for_test();
    prepare(&mut operation, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        operation.phase(),
        NativeIntentPreparationPhase::PreparedAwaitingAuthorization
    );
    assert!(matches!(
        operation.admitted.native,
        NativeOwner::Prepared(_)
    ));
    assert!(!writer_available(&native));
    assert_eq!(operation.control.native_custody_for_test(), before);
    assert!(prepare(&mut operation, &AtomicBool::new(false)).is_err());
    no_intent(&motion);
    assert_eq!(
        operation.commit_state(),
        NativeIntentCommitState::NotAttempted
    );
    assert!(
        operation.into_ready().is_err(),
        "preparation alone must not mint intent authority"
    );
}

#[tokio::test]
async fn retained96_application_slot_admission_is_checked_before_consuming_owner() {
    let fixture = fixture().await;
    let before = fixture.ready.native_custody_for_test();
    let mut ready = fixture.ready;
    ready
        .admit_native_preparation(
            &fixture.native,
            quiescence(),
            "native96",
            Instant::now() + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .unwrap();
    let ready = match ready.begin_native_preparation_reserved(1) {
        Err(ready) => ready,
        Ok(_) => panic!("unadmitted application slots accepted"),
    };
    assert_eq!(ready.native_custody_for_test(), before);
    assert!(writer_available(&fixture.native));
    // Refusing a larger consumer must not consume the existing smaller marker.
    assert!(ready.begin_native_preparation().is_ok());
}

#[tokio::test]
async fn retained96_reader_borrows_real_writer_before_and_after_native_preparation() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    let before = operation.control.native_custody_for_test();
    for prepared in [false, true] {
        if prepared {
            prepare(&mut operation, &AtomicBool::new(false)).unwrap();
        }
        let mut reader = operation.authorization_reader().await.unwrap();
        reader.verify().await.unwrap();
        let db = reader.authorize_connection().await.unwrap();
        let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&mut *db)
            .await
            .unwrap();
        assert_eq!(version, 97);
        assert!(
            sqlx::query("DELETE FROM catabolic_native_bootstrap_intents")
                .execute(&mut *db)
                .await
                .is_err()
        );
        reader.close_retained_native_intent_reader().await.unwrap();
        drop(reader);
        assert_eq!(operation.control.native_custody_for_test(), before);
        assert_eq!(writer_available(&native), !prepared);
        no_intent(&motion);
    }
    // Reader opening is not itself an application authentication proof.
    assert_eq!(
        operation.phase(),
        NativeIntentPreparationPhase::PreparedAwaitingAuthorization
    );
}

#[tokio::test]
async fn retained96_operator_commit_boundaries_preserve_custody_and_honest_states() {
    // Operator-mechanics tests only. App authority is separately checked by the
    // fixed application coordinator; no backend reader is called a user grant.
    for (point, expected, durable) in [
        (5, NativeIntentCommitState::NotAttempted, false),
        (6, NativeIntentCommitState::NotAttempted, false),
        (11, NativeIntentCommitState::NotAttempted, false),
        (12, NativeIntentCommitState::NotAttempted, false),
        (13, NativeIntentCommitState::NotAttempted, false),
        (7, NativeIntentCommitState::CommitAttempted, false),
        (8, NativeIntentCommitState::DurableIntent96, true),
        (9, NativeIntentCommitState::WriterReacquired, true),
        (10, NativeIntentCommitState::WriterReacquired, true),
        (14, NativeIntentCommitState::WriterReacquired, true),
    ] {
        for mode in [1, 2] {
            let (_root, _stage, _original, native, motion, mut operation) =
                operation(fixture().await);
            let before = operation.control.native_custody_for_test();
            prepare(&mut operation, &AtomicBool::new(false)).unwrap();
            operation.admitted.fault = Some((point, mode));
            let cancel = AtomicBool::new(false);
            let result = catch_unwind(AssertUnwindSafe(|| commit(&mut operation, &cancel)));
            if mode == 1 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
            assert_eq!(operation.commit_state(), expected);
            assert!(commit(&mut operation, &AtomicBool::new(false)).is_err());
            assert_eq!(operation.control.native_custody_for_test(), before);
            assert!(!writer_available(&native));
            live_custody(&operation.control);
            operation.settle_motion().unwrap();
            let db = Connection::open(&motion).unwrap();
            let count: i64 = db
                .query_row(
                    "SELECT count(*) FROM catabolic_native_bootstrap_intents",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, i64::from(durable));
            assert_eq!(
                operation.commit_state(),
                expected,
                "settlement must not rewrite observed commit history"
            );
            assert_eq!(operation.control.native_custody_for_test(), before);
            assert!(!writer_available(&native));
            assert!(operation.into_ready().is_err());
        }
    }
}

#[tokio::test]
async fn retained96_real_busy_commit_is_uncertain_and_retains_both_writers() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    operation.control.zero_native_intent_busy_timeout_for_test();
    prepare(&mut operation, &AtomicBool::new(false)).unwrap();
    let before = operation.control.native_custody_for_test();
    let reader = Connection::open(&motion).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let observed: i64 = reader
        .query_row(
            "SELECT count(*) FROM catabolic_native_bootstrap_intents",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(observed, 0);
    assert!(commit(&mut operation, &AtomicBool::new(false)).is_err());
    assert_eq!(
        operation.commit_state(),
        NativeIntentCommitState::CommitAttempted
    );
    assert_eq!(operation.control.native_custody_for_test(), before);
    assert!(!writer_available(&native));
    reader.execute_batch("ROLLBACK").unwrap();
    operation.settle_motion().unwrap();
    no_intent(&motion);
    assert_eq!(
        operation.commit_state(),
        NativeIntentCommitState::CommitAttempted
    );
    live_custody(&operation.control);
}

#[tokio::test]
async fn retained96_success_is_move_only_with_native_prepared_and_original97_owners() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    let before = operation.control.native_custody_for_test();
    let deadline = operation.deadline();
    prepare(&mut operation, &AtomicBool::new(false)).unwrap();
    commit(&mut operation, &AtomicBool::new(false)).unwrap();
    let ready = match operation.into_ready() {
        Ok(ready) => ready,
        Err(_) => panic!("real durable intent did not hand off"),
    };
    assert_eq!(
        ready.commit_state(),
        NativeIntentCommitState::WriterReacquired
    );
    assert_eq!(ready.deadline(), deadline);
    assert_eq!(ready.operation.control.native_custody_for_test(), before);
    assert!(matches!(
        &ready.operation.admitted.native,
        NativeOwner::Prepared(_)
    ));
    live_custody(&ready.operation.control);
    assert!(!writer_available(&native));
    let db = Connection::open(&motion).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM catabolic_native_bootstrap_intents",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    for table in [
        "catabolic_native_bootstrap_commands",
        "catabolic_native_bootstrap_acks",
        "catabolic_paired_restore_intents",
        "catabolic_paired_restore_acks",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn retained96_real_native_lock_failure_retains_partial_acquisition() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    let before = operation.control.native_custody_for_test();
    let mut writer_name = native.as_os_str().to_owned();
    writer_name.push(".lock");
    let writer = File::options()
        .read(true)
        .write(true)
        .open(PathBuf::from(writer_name))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(writer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(prepare(&mut operation, &AtomicBool::new(false)).is_err());
    assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
    let NativeOwner::Preparing(retained) = &operation.admitted.native else {
        panic!("native partial operation was discarded")
    };
    assert_eq!(
        retained.phase(),
        catabolic_store::migration::offline_bootstrap::OfflinePreparationPhase::Failed
    );
    assert_eq!(operation.control.native_custody_for_test(), before);
    drop(writer);
    let mut admission_name = native.as_os_str().to_owned();
    admission_name.push(".native-admission.lock");
    let admission = File::options()
        .read(true)
        .write(true)
        .open(PathBuf::from(admission_name))
        .unwrap();
    assert_ne!(
        unsafe { libc::flock(admission.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "failed native writer acquisition must retain previously acquired lifecycle custody"
    );
    assert!(prepare(&mut operation, &AtomicBool::new(false)).is_err());
    operation.settle_motion().unwrap();
    assert_ne!(
        unsafe { libc::flock(admission.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    live_custody(&operation.control);
    no_intent(&motion);
}

#[tokio::test]
async fn retained96_carries_final_publication_highwater_once_before_native_admission() {
    let mut fixture = fixture().await;
    let (early, final_retained, original_bytes, motion_path_bytes) =
        fixture.ready.native_retention_shares_for_test();
    assert!(
        final_retained > early,
        "genuine recorded97 publication must raise its sticky admission"
    );
    let extra_host = 4096;
    fixture
        .ready
        .admit_native_preparation_reserved(
            &fixture.native,
            quiescence(),
            "native96",
            extra_host,
            Instant::now() + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .unwrap();
    let operation = match fixture.ready.begin_native_preparation_reserved(extra_host) {
        Ok(operation) => operation,
        Err(_) => panic!("genuine admitted handoff refused"),
    };
    let expected =
        final_retained + original_bytes * 3 + 64 * 1024 * 1024 + extra_host + motion_path_bytes * 4;
    assert_eq!(
        operation.admitted.retained97, expected,
        "carry final publication total, not early total and not early plus final"
    );
    assert!(
        writer_available(&fixture.native),
        "publication charge must precede native acquisition"
    );
    assert_eq!(
        operation.control.native_retention_shares_for_test(),
        (early, final_retained, original_bytes, motion_path_bytes)
    );
    live_custody(&operation.control);
}

#[tokio::test]
async fn retained96_unpolled_close_keeps_actual_future_after_reader_and_waiter_drop() {
    for prepared in [false, true] {
        let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
        assert_eq!(operation.admitted.additional_host_bytes, 0);
        assert_eq!(
            operation.admitted.allocation.external(),
            operation.admitted.retained97
                + NativeIntentPreparationOperation::inline_storage_reservation()
                + AUTHORIZATION_CLOSE_STORAGE_BYTES
                + 3 * RestrictedReaderOwner::opening_frame_reservation()
                + 12 * (operation.admitted.authorization_path_bytes + 1)
                + operation.admitted.id.len() as u64 * 4
                + native.as_os_str().len() as u64 * 4,
            "zero-host entry must pre-admit its own close share before any acquisition"
        );
        if prepared {
            prepare(&mut operation, &AtomicBool::new(false)).unwrap();
        }
        let before = operation.control.native_custody_for_test();
        let mut reader = operation.authorization_reader().await.unwrap();
        reader.verify().await.unwrap();
        let waiter = reader.close_retained_native_intent_reader();
        // Construction already moved SQLx's actual owning close future into the
        // independently retained operation. No poll or synthetic close receipt.
        drop(waiter);
        assert!(reader.verify().await.is_err());
        assert!(reader.close_retained_native_intent_reader().await.is_err());
        assert!(
            reader
                .preflight_retained_native_intent(&AtomicBool::new(false))
                .is_err()
        );
        drop(reader);
        assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
        let close = operation
            .authorization_close
            .as_ref()
            .expect("actual retained close slot");
        assert!(
            close.connection.is_none(),
            "connection moved into actual SQLx future"
        );
        assert!(
            close.future.is_some(),
            "dropping token must not abandon actual future"
        );
        assert_eq!(
            close.frame_bytes,
            std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref()),
            "record the actual library Future frame, not the Box pointer size"
        );
        assert!(close.frame_bytes > 0);
        assert!(close.frame_bytes as u64 * 2 <= AUTHORIZATION_CLOSE_STORAGE_BYTES);
        eprintln!(
            "retained96 actual SQLx close frame: {} bytes (prepared={prepared}, admitted share={} bytes)",
            close.frame_bytes, AUTHORIZATION_CLOSE_STORAGE_BYTES
        );
        assert!(close.outcome.is_none());
        assert!(!close.acknowledged);
        assert!(operation.authorization_reader().await.is_err());
        assert!(
            operation
                .preflight_for_next_effect(&AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(operation.control.native_custody_for_test(), before);
        live_custody(&operation.control);
        assert_eq!(writer_available(&native), !prepared);
        no_intent(&motion);
    }
}

#[tokio::test]
async fn retained96_joined_reader_tokens_are_purpose_bound_and_do_not_self_busy_commit() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    operation.control.zero_native_intent_busy_timeout_for_test();
    let before = operation.control.native_custody_for_test();
    let cancel = AtomicBool::new(false);
    let mut reader = operation.authorization_reader().await.unwrap();
    assert!(reader.preflight_retained_native_intent(&cancel).is_err());
    reader.close_retained_native_intent_reader().await.unwrap();
    assert!(reader.authorize_connection().await.is_err());
    reader.preflight_retained_native_intent(&cancel).unwrap();
    reader.prepare_retained_native_intent(&cancel).unwrap();
    // First joined token cannot authorize the intent phase, even though the
    // underlying operation has advanced to PreparedAwaitingAuthorization.
    assert!(reader.preflight_retained_native_intent(&cancel).is_err());
    assert!(reader.insert_retained_native_intent(&cancel).is_err());
    drop(reader);
    let joined = operation.authorization_close.as_ref().unwrap();
    assert!(joined.acknowledged && matches!(joined.outcome.as_ref(), Some(Ok(()))));
    assert!(joined.future.is_some());
    no_intent(&motion);
    let mut reader = operation.authorization_reader().await.unwrap();
    reader.close_retained_native_intent_reader().await.unwrap();
    reader.preflight_retained_native_intent(&cancel).unwrap();
    assert!(reader.prepare_retained_native_intent(&cancel).is_err());
    reader.insert_retained_native_intent(&cancel).unwrap();
    // No async step here. App tests supply the real second temporal fence.
    reader.commit_retained_native_intent(&cancel).unwrap();
    assert!(reader.commit_retained_native_intent(&cancel).is_err());
    assert!(reader.preflight_retained_native_intent(&cancel).is_err());
    drop(reader);
    assert_eq!(
        operation.phase(),
        NativeIntentPreparationPhase::ReadyIntent96
    );
    assert_eq!(
        operation.commit_state(),
        NativeIntentCommitState::WriterReacquired
    );
    assert_eq!(operation.control.native_custody_for_test(), before);
    assert!(!writer_available(&native));
    assert!(operation.into_ready().is_ok());
}

#[tokio::test]
async fn retained96_pending_commit_keeps_uncommitted_intent_and_refuses_cancelled_commit() {
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    let before = operation.control.native_custody_for_test();
    let cancel = AtomicBool::new(false);
    assert!(
        operation.prepare(&cancel).is_err(),
        "full preflight is required"
    );
    prepare(&mut operation, &cancel).unwrap();
    assert!(
        operation.insert_intent(&cancel).is_err(),
        "intent preflight is required"
    );
    operation.preflight_for_next_effect(&cancel).unwrap();
    assert_eq!(
        operation.phase(),
        NativeIntentPreparationPhase::IntentPreflighted
    );
    operation.insert_intent(&cancel).unwrap();
    assert_eq!(
        operation.phase(),
        NativeIntentPreparationPhase::PendingCommit
    );
    assert_eq!(
        operation.commit_state(),
        NativeIntentCommitState::NotAttempted
    );
    no_intent(&motion);
    let mut operation = match operation.into_ready() {
        Err(operation) => operation,
        Ok(_) => panic!("uncommitted intent became Ready"),
    };
    assert!(operation.commit_intent(&AtomicBool::new(true)).is_err());
    assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
    assert_eq!(
        operation.commit_state(),
        NativeIntentCommitState::NotAttempted
    );
    assert_eq!(operation.control.native_custody_for_test(), before);
    assert!(!writer_available(&native));
    operation.settle_motion().unwrap();
    no_intent(&motion);
    live_custody(&operation.control);
}

// Fixed cfg(test)-only producer for the separate80 backend slice. Both actual
// target readers are joined; no owner or receipt is fabricated for apply tests.
pub(super) async fn ready_native_intent_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    File,
    PathBuf,
    PathBuf,
    ReadyNativeIntent96,
) {
    let (root, stage, original, native, motion, mut operation) = operation(fixture().await);
    let cancel = AtomicBool::new(false);
    let mut reader = operation.authorization_reader().await.unwrap();
    assert!(reader.close_retained_native_apply_reader().await.is_err());
    reader.close_retained_native_intent_reader().await.unwrap();
    assert!(reader.preflight_retained_native_apply(&cancel).is_err());
    reader.preflight_retained_native_intent(&cancel).unwrap();
    reader.prepare_retained_native_intent(&cancel).unwrap();
    drop(reader);
    let mut reader = operation.authorization_reader().await.unwrap();
    reader.close_retained_native_intent_reader().await.unwrap();
    reader.preflight_retained_native_intent(&cancel).unwrap();
    reader.insert_retained_native_intent(&cancel).unwrap();
    reader.commit_retained_native_intent(&cancel).unwrap();
    assert!(reader.apply_retained_native(&cancel).is_err());
    drop(reader);
    let ready = match operation.into_ready() {
        Ok(ready) => ready,
        Err(_) => panic!("genuine intent producer did not finish"),
    };
    (root, stage, original, native, motion, ready)
}

#[tokio::test]
async fn retained96_unpolled_authorization_open_claims_each_exact_purpose() {
    for prepared in [false, true] {
        let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
        if prepared {
            prepare(&mut operation, &AtomicBool::new(false)).unwrap();
        }
        let before = operation.control.native_custody_for_test();
        let opening = operation.authorization_reader();
        drop(opening);
        let slot = &operation.authorization_readers[usize::from(prepared)];
        assert!(slot.is_claimed() && !slot.has_connection());
        assert!(operation.authorization_reader().await.is_err());
        drop(operation.settle_authorization_reader());
        assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
        operation.settle_authorization_reader().await.unwrap();
        assert!(operation.authorization_readers[usize::from(prepared)].is_settled());
        assert!(!operation.authorization_readers[usize::from(prepared)].close_joined());
        assert!(!operation.authorization_readers[usize::from(prepared)].retains_close());
        assert!(
            operation
                .preflight_for_next_effect(&AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(operation.control.native_custody_for_test(), before);
        assert_eq!(writer_available(&native), !prepared);
        no_intent(&motion);
    }
}

#[tokio::test]
async fn retained96_abandoned_prepare_keeps_exact_target_reader_until_real_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for prepared in [false, true] {
        for after_read in [false, true] {
            for mode in 0..3 {
                let (_root, _stage, _original, native, motion, mut operation) =
                    operation(fixture().await);
                if prepared {
                    prepare(&mut operation, &AtomicBool::new(false)).unwrap();
                }
                let before = operation.control.native_custody_for_test();
                let index = usize::from(prepared);
                TargetReader::abandon_prepare(operation.authorization_reader(), after_read, mode)
                    .await;
                let reader = TargetReader::actual_reader_custody(
                    &mut operation.authorization_readers[index],
                )
                .await;
                assert!(operation.authorization_reader().await.is_err());
                assert!(
                    operation
                        .preflight_for_next_effect(&AtomicBool::new(false))
                        .is_err()
                );
                assert_eq!(
                    TargetReader::actual_reader_custody(
                        &mut operation.authorization_readers[index]
                    )
                    .await,
                    reader
                );
                assert_eq!(operation.control.native_custody_for_test(), before);
                assert_eq!(writer_available(&native), !prepared);
                no_intent(&motion);
                // The production owner cleanup joins the exact retained reader
                // without recreating the abandoned application token.
                drop(operation.settle_authorization_reader());
                assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
                operation.settle_authorization_reader().await.unwrap();
                assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
                assert!(operation.authorization_readers[index].close_joined());
                assert!(operation.authorization_readers[index].retains_close());
                assert!(operation.authorization_close.is_none());
                operation.settle_authorization_reader().await.unwrap();
                assert!(operation.await_authorization_close().await.is_err());
                let slot = &operation.authorization_readers[index];
                assert!(!slot.has_connection());
                assert_eq!(
                    (slot.file_fd(), slot.connect_address()),
                    (reader.1, reader.2)
                );
                slot.verify_identity().unwrap();
                assert!(operation.authorization_reader().await.is_err());
                assert_eq!(operation.control.native_custody_for_test(), before);
            }
        }
    }
}

#[tokio::test]
async fn retained96_setup_interruption_retains_acquired_reader_and_later_real_ack() {
    use crate::motion_snapshot::source_reader::SetupFault;
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for prepared in [false, true] {
        for mode in 0..3 {
            let (_root, _stage, _original, native, motion, mut operation) =
                operation(fixture().await);
            if prepared {
                prepare(&mut operation, &AtomicBool::new(false)).unwrap();
            }
            let before = operation.control.native_custody_for_test();
            let index = usize::from(prepared);
            let entered = operation.authorization_readers[index].set_setup_fault(match mode {
                0 => SetupFault::Pending,
                1 => SetupFault::Error,
                _ => SetupFault::Panic,
            });
            TargetReader::abandon_opening(operation.authorization_reader(), entered, mode).await;
            let slot = &mut operation.authorization_readers[index];
            assert!(slot.has_connection());
            assert!(slot.reader_mut().is_err());
            slot.verify_identity().unwrap();
            let held = (slot.file_fd(), slot.connect_address());
            assert!(operation.authorization_reader().await.is_err());
            assert!(
                operation
                    .preflight_for_next_effect(&AtomicBool::new(false))
                    .is_err()
            );
            assert_eq!(operation.control.native_custody_for_test(), before);
            assert_eq!(writer_available(&native), !prepared);
            no_intent(&motion);
            drop(operation.settle_authorization_reader());
            assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
            operation.settle_authorization_reader().await.unwrap();
            assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
            assert!(operation.authorization_readers[index].close_joined());
            assert!(operation.authorization_readers[index].retains_close());
            assert!(operation.authorization_close.is_none());
            operation.settle_authorization_reader().await.unwrap();
            assert!(operation.await_authorization_close().await.is_err());
            let slot = &operation.authorization_readers[index];
            assert!(!slot.has_connection());
            assert_eq!((slot.file_fd(), slot.connect_address()), held);
            assert!(unsafe { libc::fcntl(held.0, libc::F_GETFD) } >= 0);
            assert!(operation.authorization_reader().await.is_err());
        }
    }
}

#[tokio::test]
async fn retained96_terminal_cleanup_rejoins_pending_but_never_repolls_panicked_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for prepared in [false, true] {
        for panic in [false, true] {
            let (_root, _stage, _original, native, motion, mut operation) =
                operation(fixture().await);
            if prepared {
                prepare(&mut operation, &AtomicBool::new(false)).unwrap();
            }
            let index = usize::from(prepared);
            let before = operation.control.native_custody_for_test();
            let mut reader = operation.authorization_reader().await.unwrap();
            drop(reader.close_retained_native_intent_reader());
            drop(reader);
            let close = operation.authorization_close.as_mut().unwrap();
            let address =
                std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
            close.poll_fault = Some(u8::from(panic));
            TargetReader::abandon_close_waiter(operation.await_authorization_close(), panic).await;
            assert_eq!(
                operation.authorization_close.as_ref().unwrap().polling,
                panic
            );
            drop(operation.settle_authorization_reader());
            assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
            if panic {
                assert!(operation.settle_authorization_reader().await.is_err());
                assert!(operation.settle_authorization_reader().await.is_err());
                let close = operation.authorization_close.as_ref().unwrap();
                assert!(close.polling && close.outcome.is_none() && !close.resource_acknowledged);
                assert!(!operation.authorization_readers[index].close_joined());
            } else {
                // Terminal Pending is independently resumable, using exactly the same box.
                operation.authorization_close.as_mut().unwrap().poll_fault = Some(0);
                TargetReader::abandon_close_waiter(operation.settle_authorization_reader(), false)
                    .await;
                assert!(!operation.authorization_close.as_ref().unwrap().polling);
                operation.settle_authorization_reader().await.unwrap();
                operation.settle_authorization_reader().await.unwrap();
                let close = operation.authorization_close.as_ref().unwrap();
                assert!(close.resource_acknowledged && !close.acknowledged);
                assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
                assert!(operation.authorization_readers[index].close_joined());
            }
            let close = operation.authorization_close.as_ref().unwrap();
            assert_eq!(
                std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
                address
            );
            assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
            assert!(operation.authorization_reader().await.is_err());
            assert!(operation.await_authorization_close().await.is_err());
            assert_eq!(operation.control.native_custody_for_test(), before);
            assert_eq!(writer_available(&native), !prepared);
            no_intent(&motion);
        }
    }
}

#[tokio::test]
async fn retained96_terminal_cleanup_observes_real_ready_close_without_reviving_expired_authority()
{
    let (_root, _stage, _original, native, motion, mut operation) = operation(fixture().await);
    let before = operation.control.native_custody_for_test();
    let facts = operation.commit_state();
    let mut reader = operation.authorization_reader().await.unwrap();
    drop(reader.close_retained_native_intent_reader());
    drop(reader);
    operation.admitted.deadline = Instant::now();
    // SQLx genuinely joins, then the normal authority deadline refuses continuation.
    assert!(operation.await_authorization_close().await.is_err());
    let close = operation.authorization_close.as_ref().unwrap();
    assert!(close.resource_acknowledged && !close.acknowledged && !close.polling);
    assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
    assert!(operation.authorization_readers[0].close_joined());
    let address =
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
    operation.settle_authorization_reader().await.unwrap();
    operation.settle_authorization_reader().await.unwrap();
    assert_eq!(operation.phase(), NativeIntentPreparationPhase::Failed);
    assert!(operation.authorization_reader().await.is_err());
    assert!(operation.await_authorization_close().await.is_err());
    let close = operation.authorization_close.as_ref().unwrap();
    assert_eq!(
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
        address
    );
    assert!(close.resource_acknowledged && !close.acknowledged);
    assert_eq!(operation.commit_state(), facts);
    assert_eq!(operation.control.native_custody_for_test(), before);
    assert!(writer_available(&native));
    no_intent(&motion);
}
