//! Genuine original93 -> recorded97 -> intent96 -> applied41 -> bootstrap81.
//! No fabricated command/receipt/proof, legacy consuming completion or cap change.
use super::*;
use std::{
    fs::File,
    os::fd::AsRawFd,
    panic::{AssertUnwindSafe, catch_unwind},
    time::Duration,
};

async fn pending() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    File,
    PathBuf,
    PathBuf,
    ReadyNativePendingBootstrap96,
) {
    let (root, stage, original, native, motion, mut ready) =
        super::super::super::tests::ready_native_intent_fixture().await;
    ready
        .admit_native_apply(None, ready.deadline(), &AtomicBool::new(false))
        .unwrap();
    let mut apply = match ready.begin_native_apply() {
        Ok(op) => op,
        Err(_) => panic!("actual80 admission"),
    };
    let mut reader = apply.authorization_reader().await.unwrap();
    reader.close_retained_native_apply_reader().await.unwrap();
    reader
        .preflight_retained_native_apply(&AtomicBool::new(false))
        .unwrap();
    reader
        .apply_retained_native(&AtomicBool::new(false))
        .unwrap();
    drop(reader);
    let pending = match apply.into_pending() {
        Ok(pending) => pending,
        Err(_) => panic!("actual80 apply"),
    };
    (root, stage, original, native, motion, pending)
}
fn policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(3600),
        max_receipts: 100,
        max_logical_bytes: 4 * 1024 * 1024,
        max_hold_records: 100,
    }
}
fn admit(mut ready: ReadyNativePendingBootstrap96) -> NativeBootstrapOperation {
    ready
        .admit_bootstrap_reserved(
            "retained81-bootstrap",
            policy(),
            0,
            ready.deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let empty = ready
        .bootstrap_admission
        .as_ref()
        .unwrap()
        .tail
        .as_ref()
        .unwrap();
    let address = empty.as_ref() as *const RetainedBootstrapTail as usize;
    assert!(empty.owner.is_none());
    let op = match ready.begin_bootstrap_reserved(0) {
        Ok(op) => op,
        Err(_) => panic!("actual81 admission"),
    };
    assert_eq!(tail_address(&op), address);
    assert!(op.owner().ready.bootstrap_admission.is_none());
    assert!(op.owner().work.admission.tail.is_none());
    op
}
async fn command(
    operation: &mut NativeBootstrapOperation,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut reader = operation.authorization_reader().await?;
    reader.close_retained_native_bootstrap_reader().await?;
    reader.preflight_retained_native_bootstrap(cancel)?;
    reader.insert_retained_native_bootstrap_command(cancel)?;
    reader.commit_retained_native_bootstrap_command(cancel)
}
async fn execute(
    operation: &mut NativeBootstrapOperation,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let mut reader = operation.authorization_reader().await?;
    reader.close_retained_native_bootstrap_reader().await?;
    reader.preflight_retained_native_bootstrap(cancel)?;
    reader.execute_retained_native_bootstrap(cancel)
}
async fn ack(operation: &mut NativeBootstrapOperation, cancel: &AtomicBool) -> anyhow::Result<()> {
    let mut reader = operation.authorization_reader().await?;
    reader.close_retained_native_bootstrap_reader().await?;
    reader.preflight_retained_native_bootstrap(cancel)?;
    reader.insert_retained_native_bootstrap_ack(cancel)?;
    reader.commit_retained_native_bootstrap_ack(cancel)
}
fn tail_address(operation: &NativeBootstrapOperation) -> usize {
    operation.tail.as_ref() as *const RetainedBootstrapTail as usize
}
fn pending_custody(ready: &ReadyNativePendingBootstrap96) -> (usize, i32, i32, i32, Instant, u64) {
    ready
        .operation
        .ready
        .operation
        .control
        .native_custody_for_test()
}
fn custody(operation: &NativeBootstrapOperation) -> (usize, i32, i32, i32, Instant, u64) {
    operation
        .owner()
        .ready
        .operation
        .ready
        .operation
        .control
        .native_custody_for_test()
}
fn live_fds(custody: (usize, i32, i32, i32, Instant, u64)) {
    for fd in [custody.1, custody.2, custody.3] {
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }
}
fn native_locked(native: &Path) {
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
fn rows(motion: &Path, table: &str) -> i64 {
    Connection::open(motion)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn retained81_actual_borrowed_admission_preserves_deadline_and_h_once() {
    let (_root, _stage, _original, _native, _motion, ready) = pending().await;
    let original_custody = pending_custody(&ready);
    let mut ready = match ready.begin_bootstrap_reserved(0) {
        Err(ready) => ready,
        Ok(_) => panic!("unadmitted begin"),
    };
    assert_eq!(pending_custody(&ready), original_custody);
    assert!(ready.bootstrap_admission.is_none());
    let old = ready
        .operation
        .ready
        .operation
        .admitted
        .allocation
        .external();
    let deadline = ready.deadline();
    let before = ready
        .operation
        .ready
        .operation
        .control
        .native_custody_for_test();
    let close_verification_bytes = ready
        .operation
        .ready
        .operation
        .control
        .source_close_verification_bytes()
        .unwrap();
    ready
        .admit_bootstrap_reserved(
            "retained81-bootstrap",
            policy(),
            4096,
            deadline + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .unwrap();
    let marker = ready.bootstrap_admission.as_ref().unwrap();
    let empty = marker.tail.as_ref().unwrap();
    let address = empty.as_ref() as *const RetainedBootstrapTail as usize;
    assert!(empty.owner.is_none());
    assert_eq!(marker.deadline, deadline);
    assert_eq!(
        ready
            .operation
            .ready
            .operation
            .admitted
            .allocation
            .external(),
        old + marker.storage_bytes
    );
    let intent = ready
        .operation
        .ready
        .operation
        .admitted
        .intent
        .as_ref()
        .unwrap();
    let key_bytes = "retained81-bootstrap".len()
        + intent.from.catalog_id.len()
        + intent.from.incarnation.len()
        + crate::HOST_NAMESPACE.len();
    assert_eq!(
        marker.storage_bytes,
        NativeBootstrapOperation::inline_storage_reservation()
            + close_verification_bytes
            + AUTHORIZATION_CLOSE_STORAGE_BYTES
            + 3 * RestrictedReaderOwner::opening_frame_reservation()
            + 12 * (ready
                .operation
                .ready
                .operation
                .admitted
                .authorization_path_bytes
                + 1)
            + 2 * 64
            + 4096
            + 16 * key_bytes as u64
    );
    let ready = match ready.begin_bootstrap_reserved(4097) {
        Err(ready) => ready,
        Ok(_) => panic!("uncovered host wrapper"),
    };
    assert_eq!(pending_custody(&ready), before);
    let empty = ready
        .bootstrap_admission
        .as_ref()
        .unwrap()
        .tail
        .as_ref()
        .unwrap();
    assert_eq!(
        empty.as_ref() as *const RetainedBootstrapTail as usize,
        address
    );
    assert!(empty.owner.is_none());
    let op = match ready.begin_bootstrap_reserved(4096) {
        Ok(op) => op,
        Err(_) => panic!("covered move"),
    };
    assert_eq!(tail_address(&op), address);
    assert!(op.owner().ready.bootstrap_admission.is_none());
    assert!(op.owner().work.admission.tail.is_none());
    assert_eq!(custody(&op), before);
    assert_eq!(op.deadline(), deadline);
}

#[tokio::test]
async fn retained81_failed_borrowed_admission_keeps_unboxed_original_owner() {
    // Both paths install the terminal marker but refuse before allocating the
    // empty tail. Returning Err from begin must preserve the genuine producer.
    for over_budget in [false, true] {
        let (_root, _stage, _original, native, _motion, mut ready) = pending().await;
        let before = pending_custody(&ready);
        let deadline = ready.deadline();
        let cancel = AtomicBool::new(!over_budget);
        assert!(
            ready
                .admit_bootstrap_reserved(
                    "retained81-bootstrap",
                    policy(),
                    if over_budget { 256 * 1024 * 1024 } else { 0 },
                    deadline,
                    &cancel,
                )
                .is_err()
        );
        let admission = ready.bootstrap_admission.as_ref().unwrap();
        assert!(!admission.ready);
        assert!(admission.tail.is_none());
        let ready = match ready.begin_bootstrap_reserved(0) {
            Err(ready) => ready,
            Ok(_) => panic!("failed admission moved original owner"),
        };
        assert_eq!(pending_custody(&ready), before);
        assert_eq!(ready.deadline(), deadline);
        assert!(matches!(
            &ready.operation.ready.operation.admitted.native,
            NativeOwner::Applied(_)
        ));
        live_fds(before);
        native_locked(&native);
    }
}

#[tokio::test]
async fn retained81_failed_or_repeated_admission_never_reopens_older_bound() {
    let (_root, _stage, _original, _native, _motion, mut ready) = pending().await;
    ready
        .admit_bootstrap_reserved(
            "retained81-bootstrap",
            policy(),
            0,
            ready.deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let before = pending_custody(&ready);
    let empty = ready
        .bootstrap_admission
        .as_ref()
        .unwrap()
        .tail
        .as_ref()
        .unwrap();
    let address = empty.as_ref() as *const RetainedBootstrapTail as usize;
    assert!(empty.owner.is_none());
    let tightened = Instant::now() - Duration::from_secs(1);
    assert!(
        ready
            .admit_bootstrap_reserved(
                "retained81-bootstrap",
                policy(),
                0,
                tightened,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(ready.deadline(), tightened);
    assert_eq!(
        ready.bootstrap_admission.as_ref().unwrap().deadline,
        tightened
    );
    let ready = match ready.begin_bootstrap_reserved(0) {
        Err(ready) => ready,
        Ok(_) => panic!("repeated admission is terminal"),
    };
    let empty = ready
        .bootstrap_admission
        .as_ref()
        .unwrap()
        .tail
        .as_ref()
        .unwrap();
    assert_eq!(
        empty.as_ref() as *const RetainedBootstrapTail as usize,
        address
    );
    assert!(empty.owner.is_none());
    assert_eq!(pending_custody(&ready), before);
    live_fds(before);
}

#[tokio::test]
async fn retained81_unpolled_close_keeps_three_actual_boxes_after_reader_drop() {
    let (_root, _stage, _original, native, _motion, ready) = pending().await;
    let mut op = admit(ready);
    let before = custody(&op);
    let mut reader = op.authorization_reader().await.unwrap();
    let waiter = reader.close_retained_native_bootstrap_reader();
    drop(waiter);
    drop(reader);
    let close = op.owner().authorization_close.as_ref().unwrap();
    assert!(close.future.is_some() && close.outcome.is_none() && !close.acknowledged);
    let actual = std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref());
    assert_eq!(actual, close.frame_bytes);
    assert!(actual > 0 && actual as u64 * 2 <= AUTHORIZATION_CLOSE_STORAGE_BYTES);
    assert!(
        op.owner()
            .ready
            .operation
            .authorization_close
            .as_ref()
            .unwrap()
            .future
            .is_some()
    );
    assert!(
        op.owner()
            .ready
            .operation
            .ready
            .operation
            .authorization_close
            .as_ref()
            .unwrap()
            .future
            .is_some()
    );
    assert_eq!(op.phase(), NativeBootstrapPhase::Failed);
    assert!(op.authorization_reader().await.is_err());
    assert_eq!(custody(&op), before);
    live_fds(before);
    native_locked(&native);
    assert!(op.into_ready().is_err());
}

#[tokio::test]
async fn retained81_joined_token_drop_refuses_same_purpose_reopen() {
    let (_root, _stage, _original, _native, _motion, ready) = pending().await;
    let mut op = admit(ready);
    let mut reader = op.authorization_reader().await.unwrap();
    reader
        .close_retained_native_bootstrap_reader()
        .await
        .unwrap();
    drop(reader);
    assert!(
        op.owner()
            .authorization_close
            .as_ref()
            .unwrap()
            .acknowledged
    );
    assert!(op.authorization_reader().await.is_err());
    assert!(
        op.owner()
            .authorization_close
            .as_ref()
            .unwrap()
            .future
            .is_some()
    );
}

#[tokio::test]
async fn retained81_real_receipt_complete_ack_and_move_only_source_keep_original_fds() {
    let (_root, _stage, _original, native, motion, ready) = pending().await;
    let mut op = admit(ready);
    let address = tail_address(&op);
    let deadline = op.deadline();
    let cancel = AtomicBool::new(false);
    let before = custody(&op);
    command(&mut op, &cancel).await.unwrap();
    assert_eq!(tail_address(&op), address);
    assert_eq!(op.deadline(), deadline);
    assert_eq!(
        op.command_commit_state(),
        NativeBootstrapCommitState::WriterReacquired
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_commands"), 1);
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
    execute(&mut op, &cancel).await.unwrap();
    assert_eq!(tail_address(&op), address);
    assert_eq!(op.deadline(), deadline);
    assert!(matches!(
        op.owner().work.reconciliation.as_ref().and_then(BootstrapProducer::value),
        Some(None)
    ));
    assert!(
        op.owner()
            .work
            .execution
            .as_ref()
            .unwrap()
            .value()
            .unwrap()
            .is_committed()
    );
    let NativeOwner::Completing(native_owner) =
        &op.owner().ready.operation.ready.operation.admitted.native
    else {
        panic!("retained completion operation")
    };
    let facts = native_owner.measured_facts().unwrap();
    assert_eq!(facts.command(), op.owner().work.command.as_ref().unwrap());
    assert!(hex(facts.state_digest()));
    assert_eq!(
        op.execution_state(),
        NativeBootstrapExecutionState::CompleteMeasured
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
    ack(&mut op, &cancel).await.unwrap();
    assert_eq!(tail_address(&op), address);
    assert_eq!(op.deadline(), deadline);
    assert_eq!(
        op.ack_commit_state(),
        NativeBootstrapCommitState::WriterReacquired
    );
    assert!(matches!(
        &op.owner().ready.operation.ready.operation.admitted.native,
        NativeOwner::Completed(_)
    ));
    // This direct-prepare fixture opens Inputs[0] and Prepared[1], not the
    // service corridor's SourceLocked[2]. Prove that unused purpose stayed vacant
    // rather than accepting it as a successful reader-close acknowledgment.
    let preparation_readers = &op
        .owner()
        .ready
        .operation
        .ready
        .operation
        .authorization_readers;
    let unused_source_locked_reader = &preparation_readers[2];
    assert!(
        !unused_source_locked_reader.is_claimed()
            && !unused_source_locked_reader.has_file()
            && !unused_source_locked_reader.has_connection()
            && !unused_source_locked_reader.retains_acquisition()
            && !unused_source_locked_reader.retains_close()
            && !unused_source_locked_reader.close_joined()
            && !unused_source_locked_reader.is_settled()
    );
    let target_reader_fds: Vec<_> = preparation_readers[..2]
        .iter()
        .chain(std::iter::once(
            &op.owner().ready.operation.authorization_reader,
        ))
        .chain(op.owner().authorization_readers.iter())
        .map(|slot| {
            assert!(slot.is_claimed() && !slot.has_connection() && slot.close_joined());
            slot.verify_identity().unwrap();
            (slot.file_fd(), slot.connect_address())
        })
        .collect();
    assert_eq!(target_reader_fds.len(), 6);
    for (index, (_, opening)) in target_reader_fds.iter().enumerate() {
        assert!(
            target_reader_fds[..index]
                .iter()
                .all(|(_, earlier)| earlier != opening)
        );
    }
    let ready = match op.into_ready() {
        Ok(ready) => ready,
        Err(_) => panic!("source ready"),
    };
    assert_eq!(tail_address(&ready.operation), address);
    // Cancellation is now set without reopening admission. The final handoff
    // takes no token/callback and moves the already admitted box into the proof
    // without allocating it.
    cancel.store(true, Ordering::Release);
    let session = ready.into_source_session();
    assert_eq!(
        session.inner.retained_bootstrap_slot_for_test(),
        (Some(address), std::mem::size_of::<usize>())
    );
    assert_eq!(session.identity().schema_version, 41);
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 1);
    assert_eq!(rows(&motion, "catabolic_paired_restore_intents"), 0);
    let db = Connection::open(&motion).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; ROLLBACK").unwrap();
    live_fds(before);
    native_locked(&native);
    for (fd, _) in target_reader_fds {
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "each purpose's exact retained target FD must survive the source-tail move"
        );
    }
    drop(session);
}

#[tokio::test]
async fn retained81_foreign_joined_purpose_cannot_ack_or_execute() {
    let (_root, _stage, _original, _native, _motion, ready) = pending().await;
    let mut op = admit(ready);
    let cancel = AtomicBool::new(false);
    let mut reader = op.authorization_reader().await.unwrap();
    reader
        .close_retained_native_bootstrap_reader()
        .await
        .unwrap();
    assert!(reader.execute_retained_native_bootstrap(&cancel).is_err());
    assert!(
        reader
            .insert_retained_native_bootstrap_ack(&cancel)
            .is_err()
    );
    reader.preflight_retained_native_bootstrap(&cancel).unwrap();
    reader
        .insert_retained_native_bootstrap_command(&cancel)
        .unwrap();
    reader
        .commit_retained_native_bootstrap_command(&cancel)
        .unwrap();
    assert!(reader.preflight_retained_native_bootstrap(&cancel).is_err());
    assert!(reader.execute_retained_native_bootstrap(&cancel).is_err());
}

#[tokio::test]
async fn retained81_cancel_after_complete_retains_native_measurement_and_uncommitted_ack() {
    let (_root, _stage, _original, native, motion, ready) = pending().await;
    let mut op = admit(ready);
    let cancel = AtomicBool::new(false);
    command(&mut op, &cancel).await.unwrap();
    execute(&mut op, &cancel).await.unwrap();
    let before = custody(&op);
    let mut reader = op.authorization_reader().await.unwrap();
    reader
        .close_retained_native_bootstrap_reader()
        .await
        .unwrap();
    reader.preflight_retained_native_bootstrap(&cancel).unwrap();
    reader
        .insert_retained_native_bootstrap_ack(&cancel)
        .unwrap();
    cancel.store(true, Ordering::Release);
    assert!(
        reader
            .commit_retained_native_bootstrap_ack(&cancel)
            .is_err()
    );
    drop(reader);
    assert_eq!(
        op.ack_commit_state(),
        NativeBootstrapCommitState::NotAttempted
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
    assert!(matches!(
        &op.owner().ready.operation.ready.operation.admitted.native,
        NativeOwner::Completing(_)
    ));
    assert_eq!(custody(&op), before);
    live_fds(before);
    native_locked(&native);
    assert!(op.into_ready().is_err());
}

#[tokio::test]
async fn retained81_native_and_motion_boundaries_retain_actual_owner_on_error_or_unwind() {
    for point in [3u8, 5, 6, 8, 9, 10, 11, 13, 14, 15, 16] {
        for mode in [0u8, 1] {
            let (_root, _stage, _original, native, motion, ready) = pending().await;
            let mut op = admit(ready);
            let address = tail_address(&op);
            let cancel = AtomicBool::new(false);
            if point >= 8 {
                command(&mut op, &cancel).await.unwrap();
            }
            if point >= 11 {
                execute(&mut op, &cancel).await.unwrap();
            }
            op.owner_mut().work.fault = Some((point, mode));
            let before = custody(&op);
            let mut reader = op.authorization_reader().await.unwrap();
            reader
                .close_retained_native_bootstrap_reader()
                .await
                .unwrap();
            reader.preflight_retained_native_bootstrap(&cancel).unwrap();
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                if point < 8 {
                    reader.insert_retained_native_bootstrap_command(&cancel)?;
                    reader.commit_retained_native_bootstrap_command(&cancel)
                } else if point < 11 {
                    reader.execute_retained_native_bootstrap(&cancel)
                } else {
                    reader.insert_retained_native_bootstrap_ack(&cancel)?;
                    reader.commit_retained_native_bootstrap_ack(&cancel)
                }
            }));
            drop(reader);
            if mode == 1 {
                assert!(outcome.is_err());
            } else {
                assert!(outcome.unwrap().is_err());
            }
            assert_eq!(tail_address(&op), address);
            assert_eq!(op.phase(), NativeBootstrapPhase::Failed);
            assert_eq!(custody(&op), before);
            live_fds(before);
            native_locked(&native);
            assert!(op.authorization_reader().await.is_err());
            if point >= 9 {
                assert!(matches!(
                    &op.owner().ready.operation.ready.operation.admitted.native,
                    NativeOwner::Completing(_)
                ));
            }
            if point >= 14 {
                assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 1);
            }
            let op = match op.into_ready() {
                Err(op) => op,
                Ok(_) => panic!("failed operation lost terminal custody"),
            };
            assert_eq!(tail_address(&op), address);
            assert_eq!(custody(&op), before);
        }
    }
}

#[tokio::test]
async fn retained81_receipt_first_uses_genuine_existing_native_receipt_without_second_execute() {
    let (_root, _stage, _original, _native, motion, ready) = pending().await;
    let mut op = admit(ready);
    let cancel = AtomicBool::new(false);
    command(&mut op, &cancel).await.unwrap();
    // Test setup uses the actual native borrowed producer and the exact durably
    // recorded envelope to establish a real prior receipt, never a fabricated
    // row/owner or an application authorization shortcut. This is not a claim
    // that the terminal failed81 operation itself may retry.
    let NativeOwner::Applied(native) = &op.owner().ready.operation.ready.operation.admitted.native
    else {
        panic!("actual bootstrap producer")
    };
    let prior = native
        .execute(
            op.owner().work.command.as_ref().unwrap(),
            crate::HOST_NAMESPACE,
            &[],
            policy(),
        )
        .unwrap();
    assert!(prior.is_committed());
    execute(&mut op, &cancel).await.unwrap();
    assert!(matches!(
        op.owner().work.reconciliation.as_ref().and_then(BootstrapProducer::value),
        Some(Some(_))
    ));
    assert!(op.owner().work.execution.is_none());
    assert_eq!(
        op.execution_state(),
        NativeBootstrapExecutionState::CompleteMeasured
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
}

/// Fixed cross-module test bridge: the SAME genuine97→79→80→81 producer and
/// public source handoff, never a proof factory or a deadline reset. The raw
/// SQLite handle is used only here for read-only DBSTATUS observation, not SQL.
pub(crate) async fn retained81_source_capture_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    File,
    PathBuf,
    crate::SourceRelocationSession,
    usize,
    u64,
    u64,
    (u64, u64),
) {
    let (root, stage, original, _native, motion, ready) = pending().await;
    let original_deadline = ready.deadline();
    let mut op = admit(ready);
    let old_handle = custody(&op).0 as *mut rusqlite::ffi::sqlite3;
    let cancel = AtomicBool::new(false);
    command(&mut op, &cancel).await.unwrap();
    execute(&mut op, &cancel).await.unwrap();
    ack(&mut op, &cancel).await.unwrap();
    assert_eq!(op.deadline(), original_deadline);
    let allocation = &op
        .owner()
        .ready
        .operation
        .ready
        .operation
        .admitted
        .allocation;
    let accepted = allocation.source_pair_reservation();
    let frozen = allocation.source_pair_target_highwater().unwrap();
    assert_eq!(allocation.source_pair_reservation(), accepted);
    let mut observed = 0u64;
    for status in [
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED,
    ] {
        let (mut now, mut high) = (0, 0);
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_db_status(old_handle, status, &mut now, &mut high, 0) },
            rusqlite::ffi::SQLITE_OK
        );
        assert!(now >= 0);
        observed += now as u64;
    }
    assert!(observed > 0 && frozen >= observed);
    assert!(accepted.0 + frozen <= accepted.1);
    let ready = match op.into_ready() {
        Ok(ready) => ready,
        Err(_) => panic!("genuine81 source readiness"),
    };
    assert_eq!(ready.deadline(), original_deadline);
    let session = ready.into_source_session();
    (
        root,
        stage,
        original,
        motion,
        session,
        old_handle as usize,
        frozen,
        observed,
        accepted,
    )
}

#[tokio::test]
async fn retained81_unpolled_authorization_open_claims_each_exact_purpose() {
    for purpose in [
        BootstrapPurpose::Command,
        BootstrapPurpose::Execute,
        BootstrapPurpose::Ack,
    ] {
        let (_root, _stage, _original, native, _motion, ready) = pending().await;
        let mut operation = admit(ready);
        let cancel = AtomicBool::new(false);
        if purpose != BootstrapPurpose::Command {
            command(&mut operation, &cancel).await.unwrap();
        }
        if purpose == BootstrapPurpose::Ack {
            execute(&mut operation, &cancel).await.unwrap();
        }
        let before = custody(&operation);
        let index = operation.authorization_reader_index().unwrap();
        let opening = operation.authorization_reader();
        drop(opening);
        assert!(operation.owner().authorization_readers[index].is_claimed());
        assert!(!operation.owner().authorization_readers[index].has_connection());
        assert!(operation.authorization_reader().await.is_err());
        drop(operation.settle_authorization_reader());
        assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
        operation.settle_authorization_reader().await.unwrap();
        assert!(operation.owner().authorization_readers[index].is_settled());
        assert!(!operation.owner().authorization_readers[index].close_joined());
        assert!(!operation.owner().authorization_readers[index].retains_close());
        assert!(operation.preflight(purpose, &cancel).is_err());
        assert_eq!(custody(&operation), before);
        live_fds(before);
        native_locked(&native);
    }
}

#[tokio::test]
async fn retained81_abandoned_prepare_keeps_each_purpose_reader_until_real_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for purpose in [
        BootstrapPurpose::Command,
        BootstrapPurpose::Execute,
        BootstrapPurpose::Ack,
    ] {
        for after_read in [false, true] {
            for mode in 0..3 {
                let (_root, _stage, _original, native, motion, ready) = pending().await;
                let mut operation = admit(ready);
                let cancel = AtomicBool::new(false);
                if purpose != BootstrapPurpose::Command {
                    command(&mut operation, &cancel).await.unwrap();
                }
                if purpose == BootstrapPurpose::Ack {
                    execute(&mut operation, &cancel).await.unwrap();
                }
                let before = custody(&operation);
                let index = operation.authorization_reader_index().unwrap();
                let commands = rows(&motion, "catabolic_native_bootstrap_commands");
                TargetReader::abandon_prepare(operation.authorization_reader(), after_read, mode)
                    .await;
                let reader = TargetReader::actual_reader_custody(
                    &mut operation.owner_mut().authorization_readers[index],
                )
                .await;
                assert!(operation.authorization_reader().await.is_err());
                assert!(operation.preflight(purpose, &cancel).is_err());
                assert_eq!(
                    TargetReader::actual_reader_custody(
                        &mut operation.owner_mut().authorization_readers[index]
                    )
                    .await,
                    reader
                );
                assert_eq!(custody(&operation), before);
                assert_eq!(
                    rows(&motion, "catabolic_native_bootstrap_commands"),
                    commands
                );
                assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
                live_fds(before);
                native_locked(&native);
                drop(operation.settle_authorization_reader());
                assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
                operation.settle_authorization_reader().await.unwrap();
                assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
                assert!(operation.owner().authorization_readers[index].close_joined());
                assert!(operation.owner().authorization_readers[index].retains_close());
                assert!(operation.owner().authorization_close.is_none());
                operation.settle_authorization_reader().await.unwrap();
                assert!(operation.await_authorization_close().await.is_err());
                let slot = &operation.owner().authorization_readers[index];
                assert!(!slot.has_connection());
                assert_eq!(
                    (slot.file_fd(), slot.connect_address()),
                    (reader.1, reader.2)
                );
                slot.verify_identity().unwrap();
                assert!(operation.authorization_reader().await.is_err());
            }
        }
    }
}

#[tokio::test]
async fn retained81_setup_interruption_keeps_each_purpose_reader_and_real_ack() {
    use crate::motion_snapshot::source_reader::SetupFault;
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for purpose in [
        BootstrapPurpose::Command,
        BootstrapPurpose::Execute,
        BootstrapPurpose::Ack,
    ] {
        for mode in 0..3 {
            let (_root, _stage, _original, native, motion, ready) = pending().await;
            let mut operation = admit(ready);
            let cancel = AtomicBool::new(false);
            if purpose != BootstrapPurpose::Command {
                command(&mut operation, &cancel).await.unwrap();
            }
            if purpose == BootstrapPurpose::Ack {
                execute(&mut operation, &cancel).await.unwrap();
            }
            let before = custody(&operation);
            let index = operation.authorization_reader_index().unwrap();
            let entered =
                operation.owner_mut().authorization_readers[index].set_setup_fault(match mode {
                    0 => SetupFault::Pending,
                    1 => SetupFault::Error,
                    _ => SetupFault::Panic,
                });
            TargetReader::abandon_opening(operation.authorization_reader(), entered, mode).await;
            let slot = &mut operation.owner_mut().authorization_readers[index];
            assert!(slot.has_connection());
            assert!(slot.reader_mut().is_err());
            let held = (slot.file_fd(), slot.connect_address());
            assert!(operation.authorization_reader().await.is_err());
            assert!(operation.preflight(purpose, &cancel).is_err());
            assert_eq!(custody(&operation), before);
            assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
            native_locked(&native);
            drop(operation.settle_authorization_reader());
            assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
            operation.settle_authorization_reader().await.unwrap();
            assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
            assert!(operation.owner().authorization_readers[index].close_joined());
            assert!(operation.owner().authorization_readers[index].retains_close());
            assert!(operation.owner().authorization_close.is_none());
            operation.settle_authorization_reader().await.unwrap();
            assert!(operation.await_authorization_close().await.is_err());
            let slot = &operation.owner().authorization_readers[index];
            assert!(!slot.has_connection());
            assert_eq!((slot.file_fd(), slot.connect_address()), held);
            assert!(unsafe { libc::fcntl(held.0, libc::F_GETFD) } >= 0);
            assert!(operation.authorization_reader().await.is_err());
        }
    }
}

#[tokio::test]
async fn retained81_terminal_cleanup_rejoins_pending_but_never_repolls_panicked_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    for purpose in [
        BootstrapPurpose::Command,
        BootstrapPurpose::Execute,
        BootstrapPurpose::Ack,
    ] {
        for panic in [false, true] {
            let (_root, _stage, _original, native, motion, ready) = pending().await;
            let mut operation = admit(ready);
            let cancel = AtomicBool::new(false);
            if purpose != BootstrapPurpose::Command {
                command(&mut operation, &cancel).await.unwrap();
            }
            if purpose == BootstrapPurpose::Ack {
                execute(&mut operation, &cancel).await.unwrap();
            }
            let index = operation.authorization_reader_index().unwrap();
            let before = custody(&operation);
            let commands = rows(&motion, "catabolic_native_bootstrap_commands");
            let mut reader = operation.authorization_reader().await.unwrap();
            drop(reader.close_retained_native_bootstrap_reader());
            drop(reader);
            let close = operation.owner_mut().authorization_close.as_mut().unwrap();
            let address =
                std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
            close.poll_fault = Some(u8::from(panic));
            TargetReader::abandon_close_waiter(operation.await_authorization_close(), panic).await;
            assert_eq!(
                operation
                    .owner()
                    .authorization_close
                    .as_ref()
                    .unwrap()
                    .polling,
                panic
            );
            drop(operation.settle_authorization_reader());
            assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
            if panic {
                assert!(operation.settle_authorization_reader().await.is_err());
                assert!(operation.settle_authorization_reader().await.is_err());
                let close = operation.owner().authorization_close.as_ref().unwrap();
                assert!(close.polling && close.outcome.is_none() && !close.resource_acknowledged);
                assert!(!operation.owner().authorization_readers[index].close_joined());
            } else {
                // Terminal Pending is independently resumable, using exactly the same box.
                operation
                    .owner_mut()
                    .authorization_close
                    .as_mut()
                    .unwrap()
                    .poll_fault = Some(0);
                TargetReader::abandon_close_waiter(operation.settle_authorization_reader(), false)
                    .await;
                assert!(
                    !operation
                        .owner()
                        .authorization_close
                        .as_ref()
                        .unwrap()
                        .polling
                );
                operation.settle_authorization_reader().await.unwrap();
                operation.settle_authorization_reader().await.unwrap();
                let close = operation.owner().authorization_close.as_ref().unwrap();
                assert!(close.resource_acknowledged && !close.acknowledged);
                assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
                assert!(operation.owner().authorization_readers[index].close_joined());
            }
            let close = operation.owner().authorization_close.as_ref().unwrap();
            assert_eq!(
                std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
                address
            );
            assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
            assert!(operation.authorization_reader().await.is_err());
            assert!(operation.await_authorization_close().await.is_err());
            assert_eq!(custody(&operation), before);
            assert_eq!(
                rows(&motion, "catabolic_native_bootstrap_commands"),
                commands
            );
            assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
            live_fds(before);
            native_locked(&native);
        }
    }
}

#[tokio::test]
async fn retained81_terminal_cleanup_observes_real_ready_close_without_reviving_expired_authority()
{
    let (_root, _stage, _original, native, motion, ready) = pending().await;
    let mut operation = admit(ready);
    let cancel = AtomicBool::new(false);
    command(&mut operation, &cancel).await.unwrap();
    execute(&mut operation, &cancel).await.unwrap();
    let before = custody(&operation);
    let facts = (
        operation.command_commit_state(),
        operation.ack_commit_state(),
        operation.execution_state(),
    );
    let mut reader = operation.authorization_reader().await.unwrap();
    drop(reader.close_retained_native_bootstrap_reader());
    drop(reader);
    operation.owner_mut().work.admission.deadline = Instant::now();
    // SQLx genuinely joins, then the normal authority deadline refuses continuation.
    assert!(operation.await_authorization_close().await.is_err());
    let close = operation.owner().authorization_close.as_ref().unwrap();
    assert!(close.resource_acknowledged && !close.acknowledged && !close.polling);
    assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
    assert!(operation.owner().authorization_readers[2].close_joined());
    let address =
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
    operation.settle_authorization_reader().await.unwrap();
    operation.settle_authorization_reader().await.unwrap();
    assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
    assert!(operation.authorization_reader().await.is_err());
    assert!(operation.await_authorization_close().await.is_err());
    let close = operation.owner().authorization_close.as_ref().unwrap();
    assert_eq!(
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
        address
    );
    assert!(close.resource_acknowledged && !close.acknowledged);
    assert_eq!(
        (
            operation.command_commit_state(),
            operation.ack_commit_state(),
            operation.execution_state()
        ),
        facts
    );
    assert_eq!(custody(&operation), before);
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_commands"), 1);
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 0);
    live_fds(before);
    native_locked(&native);
}

async fn retained_close_source() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    File,
    PathBuf,
    PathBuf,
    crate::SourceRelocationSession,
) {
    let (root, stage, original, native, motion, ready) = pending().await;
    let mut operation = admit(ready);
    let cancel = AtomicBool::new(false);
    command(&mut operation, &cancel).await.unwrap();
    execute(&mut operation, &cancel).await.unwrap();
    ack(&mut operation, &cancel).await.unwrap();
    let source = match operation.into_ready() {
        Ok(ready) => ready.into_source_session(),
        Err(_) => panic!("actual retained source"),
    };
    (root, stage, original, native, motion, source)
}

fn native_released(native: &std::path::Path) {
    for suffix in [".lock", ".native-admission.lock"] {
        let mut name = native.as_os_str().to_owned();
        name.push(suffix);
        let lock = File::options()
            .read(true)
            .write(true)
            .open(PathBuf::from(name))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
    }
}

#[tokio::test]
async fn retained_source_close_preserves_preadmitted_tail_until_verified_release() {
    let (_root, _stage, _original, native, _motion, source) = retained_close_source().await;
    let tail = source.inner.retained_bootstrap_slot_for_test().0;
    assert!(tail.is_some());
    native_locked(&native);
    let mut close = match source.begin_retained_close() {
        Ok(close) => close,
        Err(_) => panic!("actual close admission"),
    };
    let before = close.custody_for_test();
    assert_eq!(before.4, tail);
    assert!(before.0 && !before.1 && !before.2);
    close.close().unwrap();
    assert_eq!(close.phase(), crate::SourceSessionClosePhase::Closed);
    let after = close.custody_for_test();
    assert_eq!((after.3, after.4), (before.3, before.4));
    assert!(!after.0 && !after.1 && after.2);
    assert!(close.close().is_err());
    assert!(close.finish().is_ok());
    native_released(&native);
}

#[tokio::test]
async fn retained_source_close_real_witness_failure_keeps_native_and_exact_tail() {
    let (_root, _stage, _original, native, motion, source) = retained_close_source().await;
    let mut close = match source.begin_retained_close() {
        Ok(close) => close,
        Err(_) => panic!("actual close admission"),
    };
    let before = close.custody_for_test();
    let hidden = motion.with_extension("close-hidden");
    std::fs::rename(&motion, &hidden).unwrap();
    assert!(close.close().is_err());
    assert_eq!(
        close.phase(),
        crate::SourceSessionClosePhase::FailedBeforeRelease
    );
    assert_eq!(close.custody_for_test(), before);
    native_locked(&native);
    std::fs::rename(&hidden, &motion).unwrap();
    assert!(close.close().is_err());
    let close = match close.finish() {
        Err(close) => close,
        Ok(()) => panic!("failed close escaped"),
    };
    assert_eq!(close.custody_for_test(), before);
    native_locked(&native);
}

#[tokio::test]
async fn retained_source_close_synthetic_boundaries_keep_honest_release_state() {
    for fault in 1..=4 {
        let (_root, _stage, _original, native, _motion, source) = retained_close_source().await;
        let mut close = match source.begin_retained_close() {
            Ok(close) => close,
            Err(_) => panic!("actual close admission"),
        };
        let before = close.custody_for_test();
        assert!(catch_unwind(AssertUnwindSafe(|| close.close_with_fault_for_test(fault))).is_err());
        let expected = match fault {
            1 | 2 => crate::SourceSessionClosePhase::FailedBeforeRelease,
            3 => crate::SourceSessionClosePhase::ReleaseAttempted,
            4 => crate::SourceSessionClosePhase::CloseFailedAfterRelease,
            _ => unreachable!(),
        };
        assert_eq!(close.phase(), expected);
        let after = close.custody_for_test();
        assert_eq!((before.3, before.4), (after.3, after.4));
        if fault < 4 {
            native_locked(&native);
        } else {
            assert!(!after.0 && !after.1 && after.2);
            native_released(&native);
        }
        assert!(close.close().is_err());
        assert!(close.finish().is_err());
    }
}

#[tokio::test]
async fn retained_source_close_refuses_actual_unpolled_capture_marker_without_losing_source() {
    let (_root, _stage, _original, native, motion, mut source) = retained_close_source().await;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&motion))
        .await
        .unwrap();
    let mut writer = pool.begin().await.unwrap();
    let tail = source.inner.retained_bootstrap_slot_for_test();
    let cancel = AtomicBool::new(false);
    drop(source.inner.admit_current_image_operation(
        &mut writer,
        Instant::now() + Duration::from_secs(60),
        &cancel,
    ));
    let source = match source.begin_retained_close() {
        Err(source) => source,
        Ok(_) => panic!("capture marker bypassed"),
    };
    assert_eq!(source.inner.retained_bootstrap_slot_for_test(), tail);
    native_locked(&native);
    writer.rollback().await.unwrap();
    pool.close().await;
    drop(source);
}


fn bootstrap_qualification(operation: &NativeBootstrapOperation) -> Option<bool> {
    operation.owner().ready.operation.ready.operation.control.control_workspace_qualified_for_test()
}
fn assert_all_seven_guarded_results(operation: &NativeBootstrapOperation) {
    let work = &operation.owner().work;
    assert!(matches!(&work.command_transition, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&work.payload, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&work.generation, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&work.reconciliation, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&work.execution, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&work.execution_transition, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&work.completion, Some(BootstrapProducer::Workspace(_))));
}
async fn execute_preflight(operation: &mut NativeBootstrapOperation, cancel: &AtomicBool) {
    let mut reader = operation.authorization_reader().await.unwrap();
    reader.close_retained_native_bootstrap_reader().await.unwrap();
    reader.preflight_retained_native_bootstrap(cancel).unwrap();
}

#[tokio::test]
async fn retained_bootstrap_workspace_keeps_all_seven_results_through_source_close() {
    let (_root, _stage, _original, native, motion, ready) = pending().await;
    let mut operation = admit(ready);
    let cancel = AtomicBool::new(false);
    assert_eq!(bootstrap_qualification(&operation), None);
    command(&mut operation, &cancel).await.unwrap();
    assert_eq!(bootstrap_qualification(&operation), None);
    assert!(matches!(&operation.owner().work.command_transition, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&operation.owner().work.payload, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&operation.owner().work.generation, Some(BootstrapProducer::Workspace(_))));
    execute_preflight(&mut operation, &cancel).await;
    assert_eq!(bootstrap_qualification(&operation), Some(true));
    operation.execute(&cancel).unwrap();
    assert_eq!(bootstrap_qualification(&operation), None);
    assert!(matches!(&operation.owner().work.reconciliation, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&operation.owner().work.execution, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&operation.owner().work.execution_transition, Some(BootstrapProducer::Workspace(_))));
    assert!(matches!(&operation.owner().work.completion, Some(BootstrapProducer::Workspace(_))));
    assert!(operation.execute(&cancel).is_err());
    ack(&mut operation, &cancel).await.unwrap();
    assert_eq!(bootstrap_qualification(&operation), None);
    assert_all_seven_guarded_results(&operation);
    let address = tail_address(&operation);
    let source = match operation.into_ready() {
        Ok(ready) => ready.into_source_session(),
        Err(_) => panic!("genuine completed workspace owner required"),
    };
    assert_eq!(source.inner.retained_bootstrap_slot_for_test().0, Some(address));
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks"), 1);
    let mut close = match source.begin_retained_close() {
        Ok(close) => close,
        Err(_) => panic!("genuine source close admission required"),
    };
    close.close().unwrap();
    // Native has closed first, while the same tail still owns every result and
    // independent guard. Guard destruction must not access a closed SQLite DB.
    assert_eq!(close.custody_for_test().4, Some(address));
    native_released(&native);
    assert!(close.finish().is_ok());
}

#[tokio::test]
async fn retained_bootstrap_failed_execute_preflight_cannot_publish_workspace_proof() {
    for mode in [0, 1, 2] {
        let (_root, _stage, _original, native, _motion, ready) = pending().await;
        let mut operation = admit(ready);
        let cancel = AtomicBool::new(false);
        command(&mut operation, &cancel).await.unwrap();
        operation.owner_mut().work.fault = Some((2, mode));
        let result = catch_unwind(AssertUnwindSafe(|| {
            operation.preflight(BootstrapPurpose::Execute, &cancel)
        }));
        if mode == 1 { assert!(result.is_err()); } else { assert!(result.unwrap().is_err()); }
        assert_eq!(bootstrap_qualification(&operation), None);
        assert_eq!(operation.phase(), NativeBootstrapPhase::Failed);
        assert!(matches!(&operation.owner().work.reconciliation, Some(BootstrapProducer::Workspace(_))));
        assert!(operation.owner().work.execution.is_none());
        assert!(operation.execute(&AtomicBool::new(false)).is_err());
        native_locked(&native);
    }
}

#[tokio::test]
async fn retained_bootstrap_wrong_purpose_and_unpolled_cleanup_revoke_workspace_proof() {
    for cleanup in [false, true] {
        let (_root, _stage, _original, native, _motion, ready) = pending().await;
        let mut operation = admit(ready);
        let cancel = AtomicBool::new(false);
        command(&mut operation, &cancel).await.unwrap();
        execute_preflight(&mut operation, &cancel).await;
        assert_eq!(bootstrap_qualification(&operation), Some(true));
        if cleanup {
            drop(operation.settle_authorization_reader());
        } else {
            // This reaches the backend preflight and revokes its accounting
            // proof. The public reader's earlier purpose rejection is unchanged
            // and is not claimed to terminalize an otherwise valid authority.
            assert!(operation.preflight(BootstrapPurpose::Ack, &cancel).is_err());
        }
        assert_eq!(bootstrap_qualification(&operation), None);
        assert!(operation.execute(&cancel).is_err());
        assert!(operation.owner().work.execution.is_none());
        native_locked(&native);
    }
}

#[tokio::test]
async fn oversized_actual_control_workspace_preserves_full_bootstrap_charge_path() {
    let (_root, _stage, _original, _native, _motion, ready) = pending().await;
    let mut operation = admit(ready);
    operation.owner_mut().ready.operation.ready.operation.control.enlarge_control_capacity_for_test();
    let cancel = AtomicBool::new(false);
    command(&mut operation, &cancel).await.unwrap();
    assert!(operation.owner().work.command_transition.is_none());
    assert!(operation.owner().work.generation.is_none());
    assert!(matches!(&operation.owner().work.payload, Some(BootstrapProducer::Ordinary(_))));
    execute_preflight(&mut operation, &cancel).await;
    assert_eq!(bootstrap_qualification(&operation), Some(false));
    operation.execute(&cancel).unwrap();
    assert_eq!(bootstrap_qualification(&operation), None);
    assert!(operation.owner().work.execution_transition.is_none());
    assert!(operation.owner().work.completion.is_none());
    assert!(matches!(&operation.owner().work.execution, Some(BootstrapProducer::Ordinary(_))));
    ack(&mut operation, &cancel).await.unwrap();
    assert!(operation.into_ready().is_ok());
}


#[tokio::test]
async fn retained_source94_fresh_workspace_outputs_survive_native_close() {
    use crate::source_relocations::owned_operation::producer::RelocationProducer;
    let (_root, _stage, _original, native, _motion, mut source) = retained_close_source().await;
    let address = source.inner.retained_bootstrap_slot_for_test().0;
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), None);
    source.inner.preflight_execute94().unwrap();
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), Some(true));
    let provenance = source.inner.source_workspace_provenance94_for_test().unwrap();
    assert_eq!(provenance.1, provenance.2);
    source.inner.refresh_source_workspace94_for_test().unwrap();
    assert_eq!(source.inner.source_workspace_provenance94_for_test().unwrap(), provenance);
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), Some(true));
    let retained = source.inner.prepare94("absent-profile", "absent-source",
        std::path::Path::new("/absent-source94-candidate"), &AtomicBool::new(false));
    // This is genuine preparation refusal from the retained completed owner,
    // not a fabricated success or replacement source94 authority.
    assert!(matches!(&retained, RelocationProducer::Workspace(Ok(_))));
    assert!(retained.result().is_err());
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), None);
    let mut close = match source.begin_retained_close() {
        Ok(close) => close, Err(_) => panic!("genuine close admission"),
    };
    close.close().unwrap();
    assert_eq!(close.custody_for_test().4, address);
    native_released(&native);
    // The error allocation and accounting context still belong to the wrapper
    // after the SQLite owner has closed. Borrowing never extracts either one.
    assert!(retained.result().is_err());
    assert!(close.finish().is_ok());
    assert!(retained.result().is_err());
    drop(retained);
}

#[tokio::test]
async fn oversized_actual_source94_tail_preserves_ordinary_full_charge() {
    use crate::source_relocations::owned_operation::producer::RelocationProducer;
    let (_root, _stage, _original, _native, _motion, mut source) = retained_close_source().await;
    source.inner.enlarge_source_workspace94_for_test();
    source.inner.preflight_execute94().unwrap();
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), Some(false));
    let retained = source.inner.prepare94("absent-profile", "absent-source",
        std::path::Path::new("/absent-source94-candidate"), &AtomicBool::new(false));
    assert!(matches!(&retained, RelocationProducer::Ordinary(_)));
    assert!(retained.result().is_err());
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), None);
}

#[tokio::test]
async fn source94_execute_consumes_only_its_fresh_tail_certificate() {
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
    use crate::source_relocations::owned_operation::producer::RelocationProducer;
    let (_root, _stage, _original, native, _motion, mut source) = retained_close_source().await;
    let command = CommandEnvelope::new(1,
        CommandKey::new("foreign-catalog", crate::HOST_NAMESPACE, "foreign-source94").unwrap(),
        "foreign-incarnation", 0, r#"{"operation":"source_renewal"}"#).unwrap();
    let cancel = AtomicBool::new(false);
    let missing = source.inner.execute94(&command, &cancel);
    assert!(matches!(&missing, RelocationProducer::Admission(_)));
    source.inner.preflight_execute94().unwrap();
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), Some(true));
    let actual = source.inner.execute94(&command, &cancel);
    assert!(matches!(&actual, RelocationProducer::Workspace(Ok(_))));
    assert!(actual.result().is_err());
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), None);
    let repeated = source.inner.execute94(&command, &cancel);
    assert!(matches!(&repeated, RelocationProducer::Admission(_)));
    native_locked(&native);
    source.inner.preflight_execute94().unwrap();
    source.inner.invalidate_execute94();
    assert_eq!(source.inner.source_workspace_qualification94_for_test(), None);
    assert!(matches!(source.inner.execute94(&command, &cancel), RelocationProducer::Admission(_)));
}
