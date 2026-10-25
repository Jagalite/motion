//! Real prior97/intent96 producer, actual SQLx close futures and native apply.
//! Backend mechanics are not a substitute for the companion app-auth tests.
use super::*;
use std::{
    fs::File,
    os::fd::AsRawFd,
    panic::{AssertUnwindSafe, catch_unwind},
    time::Duration,
};

async fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    File,
    PathBuf,
    PathBuf,
    ReadyNativeIntent96,
) {
    super::super::tests::ready_native_intent_fixture().await
}
fn admit(mut ready: ReadyNativeIntent96) -> NativeIntentApplyOperation {
    ready
        .admit_native_apply(None, ready.deadline(), &AtomicBool::new(false))
        .unwrap();
    match ready.begin_native_apply() {
        Ok(operation) => operation,
        Err(_) => panic!("genuine native apply admission refused"),
    }
}
fn live(operation: &NativeIntentApplyOperation, native: &Path) {
    let (_, target, parent, record, _, _) =
        operation.ready.operation.control.native_custody_for_test();
    for fd in [target, parent, record] {
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }
    for suffix in [".lock", ".native-admission.lock"] {
        let mut name = native.as_os_str().to_owned();
        name.push(suffix);
        let lock = File::options()
            .read(true)
            .write(true)
            .open(PathBuf::from(name))
            .unwrap();
        assert_ne!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
    }
}
fn schema(native: &Path) -> i64 {
    Connection::open(native)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}
fn intent_only(motion: &Path) {
    let db = Connection::open(motion).unwrap();
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
async fn retained80_requires_actual_borrowed_admission_and_whole_move() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let before = ready.operation.control.native_custody_for_test();
    let mut ready = match ready.begin_native_apply() {
        Err(ready) => ready,
        Ok(_) => panic!("unadmitted apply began"),
    };
    let deadline = ready.deadline();
    let prior = ready.operation.admitted.allocation.external();
    ready
        .admit_native_apply_reserved(
            None,
            4096,
            deadline + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .unwrap();
    let marker = ready.apply_admission.as_ref().unwrap();
    assert_eq!(marker.base_external, prior);
    assert_eq!(
        marker.storage_bytes,
        NativeIntentApplyOperation::inline_storage_reservation()
            + AUTHORIZATION_CLOSE_STORAGE_BYTES
            + RestrictedReaderOwner::opening_frame_reservation()
            + 4 * (ready.operation.admitted.authorization_path_bytes + 1)
            + 4096
    );
    assert_eq!(
        ready.operation.admitted.allocation.external(),
        prior + marker.storage_bytes
    );
    let charged = ready.operation.admitted.allocation.external();
    ready
        .admit_native_apply_reserved(
            None,
            4096,
            deadline + Duration::from_secs(120),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        ready.operation.admitted.allocation.external(),
        charged,
        "successful repeat must not re-add prior or new shares"
    );
    let ready = match ready.begin_native_apply_reserved(4097) {
        Err(ready) => ready,
        Ok(_) => panic!("host storage underadmitted"),
    };
    assert_eq!(ready.operation.control.native_custody_for_test(), before);
    let operation = match ready.begin_native_apply_reserved(4096) {
        Ok(operation) => operation,
        Err(_) => panic!("covered move refused"),
    };
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Inputs);
    assert_eq!(
        operation.progress(),
        NativeIntentApplyProgress::NotAttempted
    );
    assert_eq!(operation.deadline(), deadline);
    assert!(matches!(
        &operation.ready.operation.admitted.native,
        NativeOwner::Prepared(_)
    ));
    assert_eq!(
        operation.ready.operation.control.native_custody_for_test(),
        before
    );
    assert!(operation.ready.apply_admission.is_none());
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 40);
}

#[tokio::test]
async fn retained80_failed_admission_preserves_owner_and_cannot_double_add_or_retry() {
    let (_root, _stage, _original, native, motion, mut ready) = fixture().await;
    let before = ready.operation.control.native_custody_for_test();
    let deadline = ready.deadline();
    assert!(
        ready
            .admit_native_apply_reserved(None, 256 * 1024 * 1024, deadline, &AtomicBool::new(false))
            .is_err()
    );
    let raised = ready.operation.admitted.allocation.external();
    assert!(!ready.apply_admission.as_ref().unwrap().ready);
    assert!(
        ready
            .admit_native_apply(
                None,
                deadline + Duration::from_secs(60),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(ready.operation.admitted.allocation.external(), raised);
    assert_eq!(ready.operation.control.native_custody_for_test(), before);
    assert!(matches!(
        &ready.operation.admitted.native,
        NativeOwner::Prepared(_)
    ));
    assert_eq!(schema(&native), 40);
    intent_only(&motion);
    assert!(ready.begin_native_apply().is_err());
}

#[tokio::test]
async fn retained80_successful_admission_tightens_real_deadline_without_revival() {
    let (_root, _stage, _original, native, motion, mut ready) = fixture().await;
    let original = ready.deadline();
    ready
        .admit_native_apply(None, original, &AtomicBool::new(false))
        .unwrap();
    let expired = Instant::now();
    assert!(
        ready
            .admit_native_apply(None, expired, &AtomicBool::new(false))
            .is_err()
    );
    assert!(
        ready
            .admit_native_apply(
                None,
                original + Duration::from_secs(60),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(ready.deadline(), expired);
    let mut operation = match ready.begin_native_apply() {
        Ok(operation) => operation,
        Err(_) => panic!("moves-only begin must not poll deadline"),
    };
    assert_eq!(operation.deadline(), expired);
    assert!(
        operation
            .preflight_for_apply(&AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
    assert!(
        operation
            .preflight_for_apply(&AtomicBool::new(false))
            .is_err()
    );
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 40);
}

#[tokio::test]
async fn retained80_unpolled_close_retains_both_actual_boxed_futures_after_token_drop() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    let before = operation.ready.operation.control.native_custody_for_test();
    let old = operation
        .ready
        .operation
        .authorization_close
        .as_ref()
        .unwrap();
    assert!(old.acknowledged && matches!(old.outcome.as_ref(), Some(Ok(()))));
    let old_ptr = old.future.as_ref().unwrap().as_ref().get_ref() as *const _ as *const () as usize;
    let mut reader = operation.authorization_reader().await.unwrap();
    assert!(reader.close_retained_native_intent_reader().await.is_err());
    let waiter = reader.close_retained_native_apply_reader();
    drop(waiter);
    assert!(reader.verify().await.is_err());
    assert!(reader.close_retained_native_apply_reader().await.is_err());
    drop(reader);
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
    let close = operation.authorization_close.as_ref().unwrap();
    assert!(close.connection.is_none() && close.future.is_some() && close.outcome.is_none());
    assert!(!close.acknowledged);
    assert_eq!(
        close.frame_bytes,
        std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref())
    );
    assert!(close.frame_bytes as u64 * 2 <= AUTHORIZATION_CLOSE_STORAGE_BYTES);
    let new_ptr =
        close.future.as_ref().unwrap().as_ref().get_ref() as *const _ as *const () as usize;
    assert_ne!(
        old_ptr, new_ptr,
        "old acknowledged and new pending boxes simultaneously live"
    );
    assert!(
        operation
            .ready
            .operation
            .authorization_close
            .as_ref()
            .unwrap()
            .future
            .is_some()
    );
    assert!(operation.authorization_reader().await.is_err());
    assert!(
        operation
            .preflight_for_apply(&AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        operation.ready.operation.control.native_custody_for_test(),
        before
    );
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 40);
}

#[tokio::test]
async fn retained80_joined_fresh_reader_applies_to_pending_bootstrap_only() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    let before = operation.ready.operation.control.native_custody_for_test();
    let deadline = operation.deadline();
    let cancel = AtomicBool::new(false);
    let mut reader = operation.authorization_reader().await.unwrap();
    assert!(reader.preflight_retained_native_apply(&cancel).is_err());
    reader.close_retained_native_apply_reader().await.unwrap();
    assert!(reader.authorize_connection().await.is_err());
    assert!(reader.preflight_retained_native_intent(&cancel).is_err());
    reader.preflight_retained_native_apply(&cancel).unwrap();
    reader.apply_retained_native(&cancel).unwrap();
    assert!(reader.apply_retained_native(&cancel).is_err());
    assert!(reader.preflight_retained_native_apply(&cancel).is_err());
    drop(reader);
    assert_eq!(
        operation.phase(),
        NativeIntentApplyPhase::AppliedPendingBootstrap
    );
    assert_eq!(
        operation.progress(),
        NativeIntentApplyProgress::AppliedPendingBootstrap
    );
    assert_eq!(
        operation.intent_commit_state(),
        NativeIntentCommitState::WriterReacquired
    );
    assert_eq!(
        operation.ready.operation.control.native_custody_for_test(),
        before
    );
    let NativeOwner::Applied(bootstrap) = &operation.ready.operation.admitted.native else {
        panic!("actual pending Bootstrap missing")
    };
    assert!(bootstrap.verified_backup_path().unwrap().exists());
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 41);
    let pending = match operation.into_pending() {
        Ok(pending) => pending,
        Err(_) => panic!("actual applied handoff refused"),
    };
    assert_eq!(pending.deadline(), deadline);
    assert_eq!(
        pending.progress(),
        NativeIntentApplyProgress::AppliedPendingBootstrap
    );
    live(&pending.operation, &native);
}

#[tokio::test]
async fn retained80_cancel_before_apply_and_unpolled_handoff_keep_prepared_owner() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let operation = admit(ready);
    let before = operation.ready.operation.control.native_custody_for_test();
    let mut operation = match operation.into_pending() {
        Err(operation) => operation,
        Ok(_) => panic!("unattempted apply became pending"),
    };
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Inputs);
    operation
        .preflight_for_apply(&AtomicBool::new(false))
        .unwrap();
    assert!(operation.apply(&AtomicBool::new(true)).is_err());
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
    assert_eq!(
        operation.progress(),
        NativeIntentApplyProgress::NotAttempted
    );
    assert!(matches!(
        &operation.ready.operation.admitted.native,
        NativeOwner::Prepared(_)
    ));
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    assert_eq!(
        operation.ready.operation.control.native_custody_for_test(),
        before
    );
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 40);
}

#[tokio::test]
async fn retained80_error_panic_cancel_after_native_move_or_finish_keep_actual_owner() {
    for point in [2, 3, 4] {
        for mode in [0, 1, 2] {
            let (_root, _stage, _original, native, motion, ready) = fixture().await;
            let mut operation = admit(ready);
            let before = operation.ready.operation.control.native_custody_for_test();
            let cancel = AtomicBool::new(false);
            operation.preflight_for_apply(&cancel).unwrap();
            operation.admission.fault = Some((point, mode));
            let result = catch_unwind(AssertUnwindSafe(|| operation.apply(&cancel)));
            if mode == 1 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
            if point == 2 {
                assert!(matches!(
                    &operation.ready.operation.admitted.native,
                    NativeOwner::Applying(_)
                ));
                assert_eq!(
                    operation.progress(),
                    NativeIntentApplyProgress::NotAttempted
                );
                assert_eq!(schema(&native), 40);
            } else {
                assert!(matches!(
                    &operation.ready.operation.admitted.native,
                    NativeOwner::Applied(_)
                ));
                assert_eq!(
                    operation.progress(),
                    NativeIntentApplyProgress::AppliedPendingBootstrap
                );
                assert_eq!(schema(&native), 41);
            }
            assert!(operation.apply(&AtomicBool::new(false)).is_err());
            assert!(
                operation
                    .preflight_for_apply(&AtomicBool::new(false))
                    .is_err()
            );
            assert_eq!(
                operation.ready.operation.control.native_custody_for_test(),
                before
            );
            live(&operation, &native);
            intent_only(&motion);
            let mut operation = match operation.into_pending() {
                Err(operation) => operation,
                Ok(_) => panic!("failed attempt minted pending readiness"),
            };
            operation.settle_motion().unwrap();
            live(&operation, &native);
            intent_only(&motion);
        }
    }
}

#[tokio::test]
async fn retained80_real_backup_path_failure_keeps_native_attempt_and_original_writer() {
    let (_root, _stage, _original, native, motion, mut ready) = fixture().await;
    // An actual database file cannot be a backup parent directory.
    ready
        .admit_native_apply(Some(&native), ready.deadline(), &AtomicBool::new(false))
        .unwrap();
    let mut operation = match ready.begin_native_apply() {
        Ok(operation) => operation,
        Err(_) => panic!("real borrowed admission refused"),
    };
    let before = operation.ready.operation.control.native_custody_for_test();
    operation
        .preflight_for_apply(&AtomicBool::new(false))
        .unwrap();
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
    assert_eq!(
        operation.progress(),
        NativeIntentApplyProgress::ApplyAttempted
    );
    assert!(matches!(
        &operation.ready.operation.admitted.native,
        NativeOwner::Applying(_)
    ));
    assert_eq!(
        operation.ready.operation.control.native_custody_for_test(),
        before
    );
    assert!(operation.apply(&AtomicBool::new(false)).is_err());
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 40);
}

#[tokio::test]
async fn retained80_dropped_joined_token_cannot_open_a_second_reader_or_close_owner() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    let mut reader = operation.authorization_reader().await.unwrap();
    reader.close_retained_native_apply_reader().await.unwrap();
    drop(reader);
    let close = operation.authorization_close.as_ref().unwrap();
    assert!(close.acknowledged && matches!(close.outcome.as_ref(), Some(Ok(()))));
    let before =
        close.future.as_ref().unwrap().as_ref().get_ref() as *const _ as *const () as usize;
    assert!(operation.authorization_reader().await.is_err());
    let after = operation
        .authorization_close
        .as_ref()
        .unwrap()
        .future
        .as_ref()
        .unwrap()
        .as_ref()
        .get_ref() as *const _ as *const () as usize;
    assert_eq!(before, after);
    assert_eq!(
        operation.progress(),
        NativeIntentApplyProgress::NotAttempted
    );
    assert!(matches!(
        &operation.ready.operation.admitted.native,
        NativeOwner::Prepared(_)
    ));
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 40);
}

#[tokio::test]
async fn retained80_unpolled_authorization_open_claims_without_reopening() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    let opening = operation.authorization_reader();
    drop(opening);
    assert!(operation.authorization_reader.is_claimed());
    assert!(!operation.authorization_reader.has_connection());
    assert!(operation.authorization_reader().await.is_err());
    drop(operation.settle_authorization_reader());
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
    operation.settle_authorization_reader().await.unwrap();
    assert!(operation.authorization_reader.is_settled());
    assert!(!operation.authorization_reader.close_joined());
    assert!(!operation.authorization_reader.retains_close());
    assert!(
        operation
            .preflight_for_apply(&AtomicBool::new(false))
            .is_err()
    );
    live(&operation, &native);
    intent_only(&motion);
}

#[tokio::test]
async fn retained80_abandoned_prepare_keeps_exact_target_reader_until_real_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for after_read in [false, true] {
        for mode in 0..3 {
            let (_root, _stage, _original, native, motion, ready) = fixture().await;
            let mut operation = admit(ready);
            let before = operation.ready.operation.control.native_custody_for_test();
            TargetReader::abandon_prepare(operation.authorization_reader(), after_read, mode).await;
            let reader =
                TargetReader::actual_reader_custody(&mut operation.authorization_reader).await;
            assert!(operation.authorization_reader().await.is_err());
            assert!(
                operation
                    .preflight_for_apply(&AtomicBool::new(false))
                    .is_err()
            );
            assert_eq!(
                TargetReader::actual_reader_custody(&mut operation.authorization_reader).await,
                reader
            );
            live(&operation, &native);
            intent_only(&motion);
            drop(operation.settle_authorization_reader());
            assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
            operation.settle_authorization_reader().await.unwrap();
            assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
            assert!(operation.authorization_reader.close_joined());
            assert!(operation.authorization_reader.retains_close());
            assert!(operation.authorization_close.is_none());
            operation.settle_authorization_reader().await.unwrap();
            assert!(operation.await_authorization_close().await.is_err());
            assert!(!operation.authorization_reader.has_connection());
            assert_eq!(
                (
                    operation.authorization_reader.file_fd(),
                    operation.authorization_reader.connect_address()
                ),
                (reader.1, reader.2)
            );
            assert_eq!(
                operation.ready.operation.control.native_custody_for_test(),
                before
            );
            assert!(operation.authorization_reader().await.is_err());
        }
    }
}

#[tokio::test]
async fn retained80_setup_interruption_retains_acquired_reader_and_later_real_ack() {
    use crate::motion_snapshot::source_reader::SetupFault;
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for mode in 0..3 {
        let (_root, _stage, _original, native, motion, ready) = fixture().await;
        let mut operation = admit(ready);
        let entered = operation.authorization_reader.set_setup_fault(match mode {
            0 => SetupFault::Pending,
            1 => SetupFault::Error,
            _ => SetupFault::Panic,
        });
        TargetReader::abandon_opening(operation.authorization_reader(), entered, mode).await;
        assert!(operation.authorization_reader.has_connection());
        assert!(operation.authorization_reader.reader_mut().is_err());
        let held = (
            operation.authorization_reader.file_fd(),
            operation.authorization_reader.connect_address(),
        );
        assert!(operation.authorization_reader().await.is_err());
        assert!(
            operation
                .preflight_for_apply(&AtomicBool::new(false))
                .is_err()
        );
        live(&operation, &native);
        intent_only(&motion);
        drop(operation.settle_authorization_reader());
        assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
        operation.settle_authorization_reader().await.unwrap();
        assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
        assert!(operation.authorization_reader.close_joined());
        assert!(operation.authorization_reader.retains_close());
        assert!(operation.authorization_close.is_none());
        operation.settle_authorization_reader().await.unwrap();
        assert!(operation.await_authorization_close().await.is_err());
        assert!(!operation.authorization_reader.has_connection());
        assert_eq!(
            (
                operation.authorization_reader.file_fd(),
                operation.authorization_reader.connect_address()
            ),
            held
        );
        assert!(unsafe { libc::fcntl(held.0, libc::F_GETFD) } >= 0);
        assert!(operation.authorization_reader().await.is_err());
    }
}

#[tokio::test]
async fn retained80_terminal_cleanup_rejoins_pending_but_never_repolls_panicked_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for panic in [false, true] {
        let (_root, _stage, _original, native, motion, ready) = fixture().await;
        let mut operation = admit(ready);
        let before = operation.ready.operation.control.native_custody_for_test();
        let mut reader = operation.authorization_reader().await.unwrap();
        drop(reader.close_retained_native_apply_reader());
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
        assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
        if panic {
            assert!(operation.settle_authorization_reader().await.is_err());
            assert!(operation.settle_authorization_reader().await.is_err());
            let close = operation.authorization_close.as_ref().unwrap();
            assert!(close.polling && close.outcome.is_none() && !close.resource_acknowledged);
            assert!(!operation.authorization_reader.close_joined());
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
            assert!(operation.authorization_reader.close_joined());
        }
        let close = operation.authorization_close.as_ref().unwrap();
        assert_eq!(
            std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
            address
        );
        assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
        assert!(operation.authorization_reader().await.is_err());
        assert!(operation.await_authorization_close().await.is_err());
        assert_eq!(
            operation.ready.operation.control.native_custody_for_test(),
            before
        );
        live(&operation, &native);
        intent_only(&motion);
    }
}

#[tokio::test]
async fn retained80_terminal_cleanup_observes_real_ready_close_without_reviving_expired_authority()
{
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    let before = operation.ready.operation.control.native_custody_for_test();
    let facts = operation.progress();
    let mut reader = operation.authorization_reader().await.unwrap();
    drop(reader.close_retained_native_apply_reader());
    drop(reader);
    operation.admission.deadline = Instant::now();
    // SQLx genuinely joins, then the normal authority deadline refuses continuation.
    assert!(operation.await_authorization_close().await.is_err());
    let close = operation.authorization_close.as_ref().unwrap();
    assert!(close.resource_acknowledged && !close.acknowledged && !close.polling);
    assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
    assert!(operation.authorization_reader.close_joined());
    let address =
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
    operation.settle_authorization_reader().await.unwrap();
    operation.settle_authorization_reader().await.unwrap();
    assert_eq!(operation.phase(), NativeIntentApplyPhase::Failed);
    assert!(operation.authorization_reader().await.is_err());
    assert!(operation.await_authorization_close().await.is_err());
    let close = operation.authorization_close.as_ref().unwrap();
    assert_eq!(
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
        address
    );
    assert!(close.resource_acknowledged && !close.acknowledged);
    assert_eq!(operation.progress(), facts);
    assert_eq!(
        operation.ready.operation.control.native_custody_for_test(),
        before
    );
    live(&operation, &native);
    intent_only(&motion);
}


#[tokio::test]
async fn retained_control_workspace_is_qualified_before_effect_and_consumed_once() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    assert_eq!(operation.ready.operation.control.control_workspace_qualified_for_test(), None);
    let cancel = AtomicBool::new(false);
    operation.preflight_for_apply(&cancel).unwrap();
    assert_eq!(operation.ready.operation.control.control_workspace_qualified_for_test(), Some(true));
    let full = operation.ready.operation.admitted.allocation.source_pair_reservation();
    operation.apply(&cancel).unwrap();
    assert_eq!(operation.ready.operation.control.control_workspace_qualified_for_test(), None);
    let after = operation.ready.operation.admitted.allocation.source_pair_reservation();
    assert_eq!(after.0, full.0);
    assert!(after.1 >= full.1);
    assert!(operation.apply(&cancel).is_err());
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 41);
}

#[tokio::test]
async fn oversized_actual_control_capacity_keeps_full_native_workspace_charge_path() {
    let (_root, _stage, _original, native, motion, ready) = fixture().await;
    let mut operation = admit(ready);
    operation.ready.operation.control.enlarge_control_capacity_for_test();
    let cancel = AtomicBool::new(false);
    operation.preflight_for_apply(&cancel).unwrap();
    assert_eq!(operation.ready.operation.control.control_workspace_qualified_for_test(), Some(false));
    operation.apply(&cancel).unwrap();
    assert_eq!(operation.ready.operation.control.control_workspace_qualified_for_test(), None);
    live(&operation, &native);
    intent_only(&motion);
    assert_eq!(schema(&native), 41);
}
