use super::*;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::{Arc, atomic::AtomicUsize};

fn facts() -> OfflineControlShutdownFacts {
    OfflineControlShutdownFacts {
        service_admission_stopped: true,
        native_workers_joined: true,
        application_owners_drained: true,
        exclusive_operational_ownership_retained: true,
    }
}
struct Trial {
    _directory: tempfile::TempDir,
    path: PathBuf,
    original: File,
    original_fd: RawFd,
    target_fd: RawFd,
    parent_fd: RawFd,
    operation: RecordedControlOperation,
}
fn trial(root: &Path, bytes: &[u8]) -> Trial {
    let directory = tempfile::tempdir_in(root).unwrap();
    let path = directory.path().join("recorded-stage.sqlite3");
    std::fs::write(&path, bytes).unwrap();
    let source = directory.path().join("selected-original93.sqlite3");
    std::fs::write(&source, bytes).unwrap();
    let original = File::open(source).unwrap();
    let file = File::open(&path).unwrap();
    let parent = File::open(directory.path()).unwrap();
    Trial {
        path,
        original_fd: original.as_raw_fd(),
        original,
        target_fd: file.as_raw_fd(),
        parent_fd: parent.as_raw_fd(),
        operation: RecordedControlOperation::begin(file, parent),
        _directory: directory,
    }
}
fn connection(operation: &RecordedControlOperation) -> &Connection {
    operation
        .prepared
        .as_ref()
        .map(|p| &p.prepared.db)
        .or(operation.db.as_ref())
        .unwrap()
}
fn pointer(operation: &RecordedControlOperation) -> usize {
    (unsafe { connection(operation).handle() }) as usize
}
fn target_fd(operation: &RecordedControlOperation) -> RawFd {
    operation
        .prepared
        .as_ref()
        .map(|p| p.prepared.witness.file.as_raw_fd())
        .or_else(|| operation.witness.as_ref().map(|w| w.file.as_raw_fd()))
        .or_else(|| operation.file.as_ref().map(AsRawFd::as_raw_fd))
        .unwrap()
}
fn parent_fd(operation: &RecordedControlOperation) -> RawFd {
    operation
        .prepared
        .as_ref()
        .map(|p| p.directory.original_parent_fd())
        .or_else(|| {
            operation
                .directory
                .as_ref()
                .map(RecordDirectory::original_parent_fd)
        })
        .or_else(|| operation.record_parent.as_ref().map(AsRawFd::as_raw_fd))
        .unwrap()
}
fn record_fd(operation: &RecordedControlOperation) -> RawFd {
    operation
        .prepared
        .as_ref()
        .and_then(|p| p.prepared.witness.record.as_ref())
        .map(|r| r.original_record_fd())
        .or_else(|| {
            operation
                .progress
                .publication
                .as_ref()
                .and_then(|p| p.original_record_fd())
        })
        .unwrap()
}
fn version(operation: &RecordedControlOperation) -> i64 {
    connection(operation)
        .query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| {
            r.get(0)
        })
        .unwrap()
}
fn no_restore_effects(operation: &RecordedControlOperation) {
    for table in [
        "catabolic_paired_restore_intents",
        "catabolic_paired_restore_acks",
        "catabolic_native_bootstrap_intents",
    ] {
        let count: i64 = connection(operation)
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
async fn prepare(
    trial: &mut Trial,
    inspected: &crate::motion_snapshot::image_read::InspectedImage,
) {
    trial
        .operation
        .prepare(
            &trial.path,
            &mut trial.original,
            inspected,
            "retained97-owner",
            &catabolic_core::sha256(b"selected-pair-attestation"),
            &facts(),
            0,
            Instant::now() + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
}

#[tokio::test]
async fn retained_recorded97_unpolled_and_refused_prepare_keep_incoming_files() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let pair = catabolic_core::sha256(b"selected-pair-attestation");
    for mode in 0..4 {
        let mut trial = trial(root.path(), &bytes);
        let cancel = AtomicBool::new(mode == 1);
        let facts = facts();
        let path = if mode == 3 {
            Path::new("relative-stage.sqlite3")
        } else {
            &trial.path
        };
        let future = trial.operation.prepare(
            path,
            &mut trial.original,
            &inspected,
            "retained97-owner",
            &pair,
            &facts,
            if mode == 2 { 256 * 1024 * 1024 } else { 0 },
            Instant::now() + Duration::from_secs(60),
            &cancel,
        );
        if mode == 0 {
            drop(future);
        } else {
            assert!(future.await.is_err());
        }
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
        assert!(trial.operation.db.is_none() && trial.operation.witness.is_none());
        assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
        assert!(!trial._directory.path().join(record::FILE_NAME).exists());
        assert_eq!(std::fs::read(&trial.path).unwrap(), bytes);
        trial.operation.settle().unwrap();
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
    }
}

#[tokio::test]
async fn retained_recorded97_inherited_deadline_survives_queue_unpolled_and_retry() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let pair = catabolic_core::sha256(b"selected-pair-attestation");
    let facts = facts();
    let cancel = AtomicBool::new(false);
    // Expired on entry, expires while unpolled in a queue, unpolled and dropped,
    // then a caller limit beyond the unchanged local 60-second ceiling.
    for mode in 0..4 {
        let mut trial = trial(root.path(), &bytes);
        let before = Instant::now();
        let caller_deadline = match mode {
            0 => before - Duration::from_secs(1),
            1 => before + Duration::from_millis(20),
            2 => before + Duration::from_secs(30),
            _ => before + Duration::from_secs(3600),
        };
        let future = trial.operation.prepare(
            &trial.path,
            &mut trial.original,
            &inspected,
            "retained97-owner",
            &pair,
            &facts,
            0,
            caller_deadline,
            &cancel,
        );
        if mode < 2 {
            if mode == 1 {
                // The operation future is not polled while its original limit
                // expires. No reference construction or target opening begins.
                tokio::time::sleep_until(tokio::time::Instant::from_std(caller_deadline)).await;
            }
            assert!(future.await.is_err());
            assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        } else {
            drop(future);
            assert_eq!(trial.operation.phase(), RecordedControlPhase::Preparing);
        }
        let captured = trial.operation.deadline.unwrap();
        if mode < 3 {
            assert_eq!(captured, caller_deadline);
        } else {
            assert!(captured >= before + Duration::from_secs(60));
            assert!(captured <= Instant::now() + Duration::from_secs(60));
            assert!(captured < caller_deadline);
        }
        let prior_phase = trial.operation.phase();
        // A fresh long caller limit cannot replace an attempted/unpolled limit.
        assert!(
            trial
                .operation
                .prepare(
                    &trial.path,
                    &mut trial.original,
                    &inspected,
                    "retained97-owner",
                    &pair,
                    &facts,
                    0,
                    Instant::now() + Duration::from_secs(3600),
                    &cancel,
                )
                .await
                .is_err()
        );
        assert_eq!(trial.operation.deadline, Some(captured));
        assert_eq!(trial.operation.phase(), prior_phase);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
        assert!(trial.operation.db.is_none() && trial.operation.witness.is_none());
        assert!(trial.operation.reference.is_none() && trial.operation.original.is_none());
        assert_eq!(trial.operation.retained, 0);
        assert_eq!(trial.operation.publication_phase(), None);
        assert_eq!(
            trial.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        assert!(!trial._directory.path().join(record::FILE_NAME).exists());
        assert_eq!(std::fs::read(&trial.path).unwrap(), bytes);
        trial.operation.settle().unwrap();
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert_eq!(trial.operation.deadline, Some(captured));
    }
}

#[tokio::test]
async fn retained_recorded97_abandoned_prepare_keeps_exact_opened_connection() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    let entered = Arc::new(AtomicUsize::new(0));
    trial.operation.pause_after_open = Some(entered.clone());
    let pair = catabolic_core::sha256(b"selected-pair-attestation");
    let facts = facts();
    let cancel = AtomicBool::new(false);
    {
        let future = trial.operation.prepare(
            &trial.path,
            &mut trial.original,
            &inspected,
            "retained97-owner",
            &pair,
            &facts,
            0,
            Instant::now() + Duration::from_secs(60),
            &cancel,
        );
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => panic!("prepare completed before retained-writer pause: {result:?}"),
            _ = async { while entered.load(Ordering::Acquire) == 0 { tokio::task::yield_now().await; } } => (),
        }
    }
    assert_eq!(trial.operation.phase(), RecordedControlPhase::Preparing);
    assert_eq!(pointer(&trial.operation), entered.load(Ordering::Acquire));
    assert!(!connection(&trial.operation).is_autocommit());
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
    assert!(trial.operation.apply(&cancel).is_err());
    trial.operation.settle().unwrap();
    assert_eq!(pointer(&trial.operation), entered.load(Ordering::Acquire));
    assert!(connection(&trial.operation).is_autocommit());
    assert_eq!(version(&trial.operation), 93);
}

#[tokio::test]
async fn retained_recorded97_ready_handoff_retains_writer_original_and_record_fds() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let original_pointer = pointer(&trial.operation);
    {
        let mut reader = trial.operation.authorization_reader().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
            .fetch_one(reader.authorize_connection().await.unwrap())
            .await
            .unwrap();
        assert!(count > 0);
        let cancel = AtomicBool::new(false);
        reader.close_retained_control_reader().await.unwrap();
        reader.preflight_retained_control(&cancel).unwrap();
        reader.apply_retained_control(&cancel).unwrap();
        reader.commit_retained_control(&cancel).unwrap();
        reader.finish_retained_control(&cancel).unwrap();
    }
    assert_eq!(pointer(&trial.operation), original_pointer);
    no_restore_effects(&trial.operation);
    assert_eq!(
        trial.operation.commit_state(),
        RecordedCommitState::WriterReacquired
    );
    let published_fd = record_fd(&trial.operation);
    assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
    assert_eq!(record_fd(&trial.operation), published_fd);
    let mut ready = match trial.operation.into_admitted() {
        Ok(ready) => ready,
        Err(_) => panic!("Ready97 handoff refused"),
    };
    assert_eq!(ready.phase(), RecordedControlPhase::Ready97);
    assert_eq!(pointer(&ready.operation), original_pointer);
    assert_eq!(target_fd(&ready.operation), trial.target_fd);
    assert_eq!(parent_fd(&ready.operation), trial.parent_fd);
    assert_eq!(record_fd(&ready.operation), published_fd);
    assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
    ready.settle().unwrap();
    assert_eq!(ready.phase(), RecordedControlPhase::Settled);
    assert_eq!(pointer(&ready.operation), original_pointer);
    assert_eq!(record_fd(&ready.operation), published_fd);
    assert_eq!(version(&ready.operation), 97);
    assert!(trial._directory.path().join(record::FILE_NAME).exists());
}

#[tokio::test]
async fn retained_recorded97_cancellation_and_deadline_refuse_before_effects() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for expired in [false, true] {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        if expired {
            trial.operation.prepared.as_mut().unwrap().prepared.deadline =
                Instant::now() - Duration::from_secs(1);
        }
        assert!(trial.operation.apply(&AtomicBool::new(!expired)).is_err());
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert_eq!(
            trial.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        assert!(!trial._directory.path().join(record::FILE_NAME).exists());
        trial.operation.settle().unwrap();
        assert_eq!(version(&trial.operation), 93);
        assert_eq!(std::fs::read(&trial.path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn retained_recorded97_partial_publication_and_unwind_keep_all_owners() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for (checkpoint, panic) in [(1, false), (3, true)] {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        trial.operation.progress.record_fault = Some((checkpoint, panic));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            trial.operation.apply(&AtomicBool::new(false))
        }));
        if panic {
            assert!(outcome.is_err());
        } else {
            assert!(outcome.unwrap().is_err());
        }
        assert_eq!(
            trial.operation.phase(),
            if panic {
                RecordedControlPhase::Applying97
            } else {
                RecordedControlPhase::Failed
            }
        );
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
        assert_eq!(
            trial.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        let published_fd = record_fd(&trial.operation);
        let named = trial._directory.path().join(record::FILE_NAME);
        let identity = std::fs::metadata(&named).unwrap();
        assert!(identity.len() > 0);
        let saved = std::fs::read(&named).unwrap();
        assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
        assert_eq!(record_fd(&trial.operation), published_fd);
        trial.operation.settle().unwrap();
        assert_eq!(version(&trial.operation), 93);
        assert_eq!(record_fd(&trial.operation), published_fd);
        assert_eq!(std::fs::read(&named).unwrap(), saved);
        let after = std::fs::metadata(&named).unwrap();
        assert_eq!((identity.dev(), identity.ino()), (after.dev(), after.ino()));
    }
}

#[tokio::test]
async fn retained_recorded97_commit_boundary_states_are_not_completion() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for (checkpoint, expected, after_settle) in [
        (102, RecordedCommitState::CommitAttempted, 93),
        (101, RecordedCommitState::Committed97, 97),
        (103, RecordedCommitState::WriterReacquired, 97),
        (104, RecordedCommitState::WriterReacquired, 97),
    ] {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        trial.operation.cancel_before_ready = checkpoint == 104;
        FAULT.with(|fault| fault.set(Some((checkpoint, false))));
        let result = trial.operation.apply(&AtomicBool::new(false));
        FAULT.with(|fault| fault.set(None));
        assert!(result.is_err());
        assert_eq!(trial.operation.commit_state(), expected);
        assert_eq!(pointer(&trial.operation), before);
        no_restore_effects(&trial.operation);
        let fd = record_fd(&trial.operation);
        assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
        trial.operation.settle().unwrap();
        assert_eq!(version(&trial.operation), after_settle);
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(record_fd(&trial.operation), fd);
        assert!(trial._directory.path().join(record::FILE_NAME).exists());
        assert!(trial.operation.into_admitted().is_err());
    }
}

#[tokio::test]
async fn retained_recorded97_real_busy_commit_retains_uncertain_writer() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let before = pointer(&trial.operation);
    // A genuine second read transaction prevents the real rollback-journal
    // COMMIT. Zero busy timeout changes only test waiting, never admission.
    let reader = Connection::open(&trial.path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let observed_version: i64 = reader
        .query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(observed_version, 93);
    connection(&trial.operation)
        .busy_timeout(Duration::ZERO)
        .unwrap();
    assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
    assert_eq!(
        trial.operation.commit_state(),
        RecordedCommitState::CommitAttempted
    );
    assert_eq!(pointer(&trial.operation), before);
    assert!(!connection(&trial.operation).is_autocommit());
    let published_fd = record_fd(&trial.operation);
    reader.execute_batch("ROLLBACK").unwrap();
    trial.operation.settle().unwrap();
    assert_eq!(version(&trial.operation), 93);
    assert_eq!(record_fd(&trial.operation), published_fd);
    assert_eq!(pointer(&trial.operation), before);
}

#[tokio::test]
async fn retained_control_unpolled_close_retains_actual_box_and_cannot_bypass_join() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let before = pointer(&trial.operation);
    let deadline = trial.operation.deadline;
    let pair = catabolic_core::sha256(b"selected-pair-attestation");
    assert_eq!(
        trial.operation.retained,
        RecordedControlOperation::inline_storage_reservation()
            + CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES
            + RestrictedReaderOwner::opening_frame_reservation()
            + (trial.path.as_os_str().len() as u64 + 1) * 9
            + ("retained97-owner".len() as u64 + 4 * 64) * 2
            + 2 * 64,
        "zero-external entry pre-admits its own fixed close share"
    );
    assert_eq!(
        RecordedControlOperation::storage(&trial.path, "retained97-owner", &pair).unwrap(),
        trial.operation.retained
    );
    let cancel = AtomicBool::new(false);
    let mut reader = trial.operation.authorization_reader().await.unwrap();
    assert!(reader.preflight_retained_control(&cancel).is_err());
    assert!(reader.close_retained_native_intent_reader().await.is_err());
    assert!(reader.close_retained_native_apply_reader().await.is_err());
    assert!(
        reader
            .close_retained_native_bootstrap_reader()
            .await
            .is_err()
    );
    reader.verify().await.unwrap();
    let waiter = reader.close_retained_control_reader();
    drop(waiter);
    assert!(reader.verify().await.is_err());
    assert!(reader.close_retained_control_reader().await.is_err());
    assert!(reader.preflight_retained_control(&cancel).is_err());
    assert!(reader.apply_retained_control(&cancel).is_err());
    drop(reader);
    assert_eq!(
        trial.operation.phase(),
        RecordedControlPhase::AuthorizationClosing
    );
    let close = trial.operation.authorization_close.as_ref().unwrap();
    assert!(close.connection.is_none() && close.future.is_some());
    assert!(close.outcome.is_none() && !close.acknowledged);
    assert_eq!(
        close.frame_bytes,
        std::mem::size_of_val(close.future.as_ref().unwrap().as_ref().get_ref())
    );
    assert!(close.frame_bytes > 0);
    assert!(close.frame_bytes as u64 * 2 <= CONTROL_AUTHORIZATION_CLOSE_STORAGE_BYTES);
    assert!(trial.operation.authorization_reader().await.is_err());
    assert!(trial.operation.apply(&cancel).is_err());
    assert!(trial.operation.apply_pending(&cancel).is_err());
    assert_eq!(pointer(&trial.operation), before);
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    assert_eq!(trial.operation.deadline, deadline);
    assert_eq!(version(&trial.operation), 93);
    assert!(!trial._directory.path().join(record::FILE_NAME).exists());
    // Explicit writer settlement is not reader-close certification and does
    // not dispose of the actual unpolled SQLx future or revive Ready97.
    trial.operation.settle().unwrap();
    assert!(
        trial
            .operation
            .authorization_close
            .as_ref()
            .unwrap()
            .future
            .is_some()
    );
    assert!(trial.operation.into_admitted().is_err());
}

#[tokio::test]
async fn retained_control_pending_close_survives_waiter_and_token_drop() {
    use std::sync::{Condvar, Mutex};
    use std::task::Poll;
    struct ReleaseWorker(Arc<(Mutex<bool>, Condvar)>);
    impl Drop for ReleaseWorker {
        fn drop(&mut self) {
            *self.0.0.lock().unwrap() = true;
            self.0.1.notify_all();
        }
    }
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let before = pointer(&trial.operation);
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = ReleaseWorker(gate.clone());
    let mut reader = trial.operation.authorization_reader().await.unwrap();
    {
        let connection = reader.authorize_connection().await.unwrap();
        let worker_entered = entered.clone();
        let worker_gate = gate.clone();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_progress_handler(1, move || {
                worker_entered.store(true, Ordering::Release);
                let mut released = worker_gate.0.lock().unwrap();
                while !*released {
                    released = worker_gate.1.wait(released).unwrap();
                }
                true
            });
        // Test-only worker gate blocks a genuine SQLx SELECT, not a stand-in
        // close future. Dropping this borrower leaves the worker running.
        let query =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM profiles").fetch_one(connection);
        tokio::pin!(query);
        tokio::select! {
            result = &mut query => panic!("gated reader query completed: {result:?}"),
            _ = async {
                while !entered.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            } => (),
        }
    }
    {
        let waiter = reader.close_retained_control_reader();
        tokio::pin!(waiter);
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    drop(reader);
    let close = trial.operation.authorization_close.as_ref().unwrap();
    assert!(close.future.is_some() && close.outcome.is_none() && !close.acknowledged);
    assert_eq!(
        trial.operation.phase(),
        RecordedControlPhase::AuthorizationClosing
    );
    assert_eq!(pointer(&trial.operation), before);
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
    drop(release);
    // A private test resume observes the same stored SQLx shutdown receiver.
    // No public retry/token recreation is granted by this internal completion.
    trial.operation.await_authorization_close().await.unwrap();
    let close = trial.operation.authorization_close.as_ref().unwrap();
    assert!(close.future.is_some() && close.acknowledged);
    assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
    assert_eq!(
        trial.operation.phase(),
        RecordedControlPhase::AuthorizationJoined93
    );
    assert!(trial.operation.authorization_reader().await.is_err());
    assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
    trial.operation.settle().unwrap();
    assert_eq!(version(&trial.operation), 93);
}

#[tokio::test]
async fn retained_control_joined_stages_keep_real_owners_through_ready97() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let before = pointer(&trial.operation);
    let deadline = trial.operation.deadline;
    connection(&trial.operation)
        .busy_timeout(Duration::ZERO)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let mut reader = trial.operation.authorization_reader().await.unwrap();
    reader.close_retained_control_reader().await.unwrap();
    assert!(reader.authorize_connection().await.is_err());
    assert!(reader.apply_retained_control(&cancel).is_err());
    reader.preflight_retained_control(&cancel).unwrap();
    assert!(reader.preflight_retained_native_intent(&cancel).is_err());
    assert!(reader.prepare_retained_native_intent(&cancel).is_err());
    assert!(reader.apply_retained_native(&cancel).is_err());
    assert!(
        reader
            .insert_retained_native_bootstrap_command(&cancel)
            .is_err()
    );
    // Application temporal grants are deliberately not invented by this test.
    reader.apply_retained_control(&cancel).unwrap();
    assert!(reader.apply_retained_control(&cancel).is_err());
    assert!(reader.preflight_retained_control(&cancel).is_err());
    assert!(reader.finish_retained_control(&cancel).is_err());
    reader.commit_retained_control(&cancel).unwrap();
    assert!(reader.commit_retained_control(&cancel).is_err());
    reader.finish_retained_control(&cancel).unwrap();
    assert!(reader.finish_retained_control(&cancel).is_err());
    drop(reader);
    assert_eq!(trial.operation.phase(), RecordedControlPhase::Ready97);
    assert_eq!(
        trial.operation.commit_state(),
        RecordedCommitState::WriterReacquired
    );
    assert_eq!(pointer(&trial.operation), before);
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    assert_eq!(trial.original.as_raw_fd(), trial.original_fd);
    assert_eq!(trial.operation.deadline, deadline);
    no_restore_effects(&trial.operation);
    let fd = record_fd(&trial.operation);
    let mut ready = match trial.operation.into_admitted() {
        Ok(ready) => ready,
        Err(_) => panic!("joined control did not become Ready97"),
    };
    let close = ready.operation.authorization_close.as_ref().unwrap();
    assert!(close.future.is_some() && close.acknowledged);
    assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
    ready.settle().unwrap();
    assert_eq!(pointer(&ready.operation), before);
    assert_eq!(record_fd(&ready.operation), fd);
    assert_eq!(version(&ready.operation), 97);
}

#[tokio::test]
async fn retained_control_expiry_or_cancel_refuses_effects_and_pending_commit() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for pending_commit in [false, true] {
        for expired in [false, true] {
            let mut trial = trial(root.path(), &bytes);
            prepare(&mut trial, &inspected).await;
            let before = pointer(&trial.operation);
            let cancel = AtomicBool::new(false);
            let mut reader = trial.operation.authorization_reader().await.unwrap();
            reader.close_retained_control_reader().await.unwrap();
            reader.preflight_retained_control(&cancel).unwrap();
            if pending_commit {
                reader.apply_retained_control(&cancel).unwrap();
            }
            drop(reader);
            if expired {
                trial.operation.prepared.as_mut().unwrap().prepared.deadline =
                    Instant::now() - Duration::from_secs(1);
            } else {
                cancel.store(true, Ordering::Release);
            }
            // Private fixed continuation exercises the actual retained97 owner
            // after deterministic deadline injection, without minting app auth.
            let result = if pending_commit {
                trial.operation.commit_pending(&cancel)
            } else {
                trial.operation.apply_pending(&cancel)
            };
            assert!(result.is_err());
            assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
            assert_eq!(
                trial.operation.commit_state(),
                RecordedCommitState::NotAttempted
            );
            assert_eq!(pointer(&trial.operation), before);
            assert_eq!(target_fd(&trial.operation), trial.target_fd);
            assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
            assert_eq!(
                trial._directory.path().join(record::FILE_NAME).exists(),
                pending_commit
            );
            assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
            assert!(trial.operation.authorization_reader().await.is_err());
            trial.operation.settle().unwrap();
            assert_eq!(version(&trial.operation), 93);
            assert_eq!(std::fs::read(&trial.path).unwrap(), bytes);
        }
    }
}

#[tokio::test]
async fn retained_control_publication_fault_and_unwind_retain_joined_owners() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for (checkpoint, panic) in [(1, false), (3, true)] {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        trial.operation.progress.record_fault = Some((checkpoint, panic));
        let cancel = AtomicBool::new(false);
        let mut reader = trial.operation.authorization_reader().await.unwrap();
        reader.close_retained_control_reader().await.unwrap();
        reader.preflight_retained_control(&cancel).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.apply_retained_control(&cancel)
        }));
        if panic {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert!(reader.commit_retained_control(&cancel).is_err());
        drop(reader);
        assert_eq!(
            trial.operation.phase(),
            if panic {
                RecordedControlPhase::Applying97
            } else {
                RecordedControlPhase::Failed
            }
        );
        assert_eq!(
            trial.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        let fd = record_fd(&trial.operation);
        let published = std::fs::read(trial._directory.path().join(record::FILE_NAME)).unwrap();
        assert!(
            trial
                .operation
                .authorization_close
                .as_ref()
                .unwrap()
                .acknowledged
        );
        trial.operation.settle().unwrap();
        assert_eq!(version(&trial.operation), 93);
        assert_eq!(record_fd(&trial.operation), fd);
        assert_eq!(
            std::fs::read(trial._directory.path().join(record::FILE_NAME)).unwrap(),
            published
        );
    }
}

#[tokio::test]
async fn retained_control_commit_faults_and_real_busy_commit_remain_honest() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for (checkpoint, expected, settled_version) in [
        (100, RecordedCommitState::NotAttempted, 93),
        (102, RecordedCommitState::CommitAttempted, 93),
        (101, RecordedCommitState::Committed97, 97),
        (103, RecordedCommitState::WriterReacquired, 97),
        (0, RecordedCommitState::CommitAttempted, 93),
    ] {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        connection(&trial.operation)
            .busy_timeout(Duration::ZERO)
            .unwrap();
        let blocker = if checkpoint == 0 {
            let db = Connection::open(&trial.path).unwrap();
            db.execute_batch("BEGIN").unwrap();
            let observed: i64 = db
                .query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(observed, 93);
            Some(db)
        } else {
            None
        };
        let cancel = AtomicBool::new(false);
        let mut reader = trial.operation.authorization_reader().await.unwrap();
        reader.close_retained_control_reader().await.unwrap();
        reader.preflight_retained_control(&cancel).unwrap();
        reader.apply_retained_control(&cancel).unwrap();
        FAULT.with(|fault| fault.set(Some((checkpoint, false))));
        let result = reader
            .commit_retained_control(&cancel)
            .and_then(|()| reader.finish_retained_control(&cancel));
        FAULT.with(|fault| fault.set(None));
        assert!(result.is_err());
        assert!(reader.commit_retained_control(&cancel).is_err());
        drop(reader);
        assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        assert_eq!(trial.operation.commit_state(), expected);
        assert_eq!(pointer(&trial.operation), before);
        let fd = record_fd(&trial.operation);
        if let Some(blocker) = blocker {
            blocker.execute_batch("ROLLBACK").unwrap();
        }
        trial.operation.settle().unwrap();
        assert_eq!(version(&trial.operation), settled_version);
        assert_eq!(record_fd(&trial.operation), fd);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert!(trial.operation.into_admitted().is_err());
    }
}

#[tokio::test]
async fn retained_control_real_reacquire_failure_keeps_committed97_not_ready() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let before = pointer(&trial.operation);
    connection(&trial.operation)
        .busy_timeout(Duration::ZERO)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let mut reader = trial.operation.authorization_reader().await.unwrap();
    reader.close_retained_control_reader().await.unwrap();
    reader.preflight_retained_control(&cancel).unwrap();
    reader.apply_retained_control(&cancel).unwrap();
    reader.commit_retained_control(&cancel).unwrap();
    let blocker = Connection::open(&trial.path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(reader.finish_retained_control(&cancel).is_err());
    drop(reader);
    assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
    assert_eq!(
        trial.operation.commit_state(),
        RecordedCommitState::Committed97
    );
    assert!(connection(&trial.operation).is_autocommit());
    assert_eq!(pointer(&trial.operation), before);
    let fd = record_fd(&trial.operation);
    blocker.execute_batch("ROLLBACK").unwrap();
    assert!(trial.operation.finish_committed(&cancel).is_err());
    trial.operation.settle().unwrap();
    assert_eq!(version(&trial.operation), 97);
    assert_eq!(record_fd(&trial.operation), fd);
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    assert!(trial.operation.into_admitted().is_err());
}

#[tokio::test]
async fn retained_control_close_refuses_legacy_control_owner_before_moving_reader() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let mut legacy = crate::native_bootstrap::TargetAuthorizationReader::open_control(
        trial.operation.prepared.as_mut().unwrap(),
    )
    .await
    .unwrap();
    assert!(legacy.close_retained_control_reader().await.is_err());
    assert!(
        legacy
            .preflight_retained_control(&AtomicBool::new(false))
            .is_err()
    );
    assert!(
        legacy
            .commit_retained_control(&AtomicBool::new(false))
            .is_err()
    );
    legacy.verify().await.unwrap();
    drop(legacy);
    assert!(trial.operation.authorization_close.is_none());
    assert_eq!(trial.operation.phase(), RecordedControlPhase::Prepared93);
}

#[tokio::test]
async fn retained_control_named_app_admission_is_additive_not_generic_host_inference() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let pair = catabolic_core::sha256(b"selected-pair-attestation");
    for (external, additional) in [(0, 0), (0, 64 * 1024 * 1024), (64 * 1024 * 1024, 4096)] {
        let mut trial = trial(root.path(), &bytes);
        let cancel = AtomicBool::new(false);
        trial
            .operation
            .prepare(
                &trial.path,
                &mut trial.original,
                &inspected,
                "retained97-owner",
                &pair,
                &facts(),
                external,
                Instant::now() + Duration::from_secs(60),
                &cancel,
            )
            .await
            .unwrap();
        let before = pointer(&trial.operation);
        let deadline = trial.operation.deadline();
        let initial = trial
            .operation
            .prepared
            .as_ref()
            .unwrap()
            .prepared
            .retained_pair_bytes;
        assert_eq!(
            initial,
            external
                + RecordedControlOperation::storage(&trial.path, "retained97-owner", &pair)
                    .unwrap()
        );
        assert!(!trial.operation.retained_authorization_storage_covers(0));
        assert!(
            !trial
                .operation
                .retained_authorization_storage_covers(additional)
        );
        trial
            .operation
            .admit_retained_authorization(additional, &cancel)
            .unwrap();
        assert!(
            trial
                .operation
                .retained_authorization_storage_covers(additional)
        );
        assert!(
            !trial
                .operation
                .retained_authorization_storage_covers(additional + 1)
        );
        assert_eq!(
            trial
                .operation
                .prepared
                .as_ref()
                .unwrap()
                .prepared
                .retained_pair_bytes,
            initial + additional,
            "named app slot is charged on top of all existing external custody"
        );
        assert_eq!(trial.operation.retained, initial + additional);
        assert!(trial.operation.apply(&cancel).is_err());
        assert!(
            trial
                .operation
                .admit_retained_authorization(additional, &cancel)
                .is_err()
        );
        assert_eq!(trial.operation.deadline(), deadline);
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(version(&trial.operation), 93);
        assert!(!trial._directory.path().join(record::FILE_NAME).exists());
        let mut reader = trial.operation.authorization_reader().await.unwrap();
        reader.close_retained_control_reader().await.unwrap();
        drop(reader);
        assert!(
            !trial
                .operation
                .retained_authorization_storage_covers(additional)
        );
        assert!(trial.operation.apply(&cancel).is_err());
        trial.operation.settle().unwrap();
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    }
}

#[tokio::test]
async fn retained_control_failed_named_admission_keeps_custody_and_blocks_legacy() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for mode in 0..3 {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        let deadline = trial.operation.deadline();
        if mode == 2 {
            trial.operation.prepared.as_mut().unwrap().prepared.deadline =
                Instant::now() - Duration::from_secs(1);
        }
        let amount = if mode == 0 { 256 * 1024 * 1024 } else { 4096 };
        assert!(
            trial
                .operation
                .admit_retained_authorization(amount, &AtomicBool::new(mode == 1))
                .is_err()
        );
        assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        let admission = trial.operation.authorization_admission.as_ref().unwrap();
        assert_eq!(admission.bytes, amount);
        assert!(!admission.ready);
        assert!(!trial.operation.retained_authorization_storage_covers(0));
        assert!(
            trial
                .operation
                .admit_retained_authorization(0, &AtomicBool::new(false))
                .is_err()
        );
        assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
        assert!(trial.operation.authorization_reader().await.is_err());
        assert_eq!(trial.operation.deadline(), deadline);
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
        assert_eq!(
            trial.operation.commit_state(),
            RecordedCommitState::NotAttempted
        );
        assert!(!trial._directory.path().join(record::FILE_NAME).exists());
        trial.operation.settle().unwrap();
        assert_eq!(version(&trial.operation), 93);
        assert_eq!(std::fs::read(&trial.path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn retained_control_unpolled_authorization_open_claims_without_legacy_escape() {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let opening = trial.operation.authorization_reader();
    drop(opening);
    assert!(trial.operation.authorization_reader.is_claimed());
    assert!(!trial.operation.authorization_reader.has_connection());
    assert!(trial.operation.authorization_reader().await.is_err());
    drop(trial.operation.settle_authorization_reader());
    assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
    trial.operation.settle_authorization_reader().await.unwrap();
    assert!(trial.operation.authorization_reader.is_settled());
    assert!(!trial.operation.authorization_reader.close_joined());
    assert!(!trial.operation.authorization_reader.retains_close());
    assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
    assert_eq!(version(&trial.operation), 93);
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
}

#[tokio::test]
async fn retained_control_abandoned_prepare_keeps_exact_reader_until_real_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for after_read in [false, true] {
        for mode in 0..3 {
            let mut trial = trial(root.path(), &bytes);
            prepare(&mut trial, &inspected).await;
            let before = pointer(&trial.operation);
            TargetReader::abandon_prepare(trial.operation.authorization_reader(), after_read, mode)
                .await;
            let reader =
                TargetReader::actual_reader_custody(&mut trial.operation.authorization_reader)
                    .await;
            assert!(trial.operation.authorization_reader().await.is_err());
            assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
            assert!(
                trial
                    .operation
                    .preflight_for_apply(&AtomicBool::new(false))
                    .is_err()
            );
            assert_eq!(
                TargetReader::actual_reader_custody(&mut trial.operation.authorization_reader)
                    .await,
                reader
            );
            assert_eq!(pointer(&trial.operation), before);
            assert_eq!(version(&trial.operation), 93);
            assert_eq!(target_fd(&trial.operation), trial.target_fd);
            assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
            drop(trial.operation.settle_authorization_reader());
            assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
            trial.operation.settle_authorization_reader().await.unwrap();
            assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
            assert!(trial.operation.authorization_reader.close_joined());
            assert!(trial.operation.authorization_reader.retains_close());
            assert!(trial.operation.authorization_close.is_none());
            trial.operation.settle_authorization_reader().await.unwrap();
            assert!(trial.operation.await_authorization_close().await.is_err());
            assert!(!trial.operation.authorization_reader.has_connection());
            assert_eq!(
                (
                    trial.operation.authorization_reader.file_fd(),
                    trial.operation.authorization_reader.connect_address()
                ),
                (reader.1, reader.2)
            );
            assert!(trial.operation.authorization_reader().await.is_err());
        }
    }
}

#[tokio::test]
async fn retained_control_setup_interruption_keeps_acquired_reader_and_real_ack() {
    use crate::motion_snapshot::source_reader::SetupFault;
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for mode in 0..3 {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        let entered = trial
            .operation
            .authorization_reader
            .set_setup_fault(match mode {
                0 => SetupFault::Pending,
                1 => SetupFault::Error,
                _ => SetupFault::Panic,
            });
        TargetReader::abandon_opening(trial.operation.authorization_reader(), entered, mode).await;
        assert!(trial.operation.authorization_reader.has_connection());
        assert!(trial.operation.authorization_reader.reader_mut().is_err());
        let held = (
            trial.operation.authorization_reader.file_fd(),
            trial.operation.authorization_reader.connect_address(),
        );
        assert!(trial.operation.authorization_reader().await.is_err());
        assert!(trial.operation.apply(&AtomicBool::new(false)).is_err());
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(version(&trial.operation), 93);
        drop(trial.operation.settle_authorization_reader());
        assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        trial.operation.settle_authorization_reader().await.unwrap();
        assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        assert!(trial.operation.authorization_reader.close_joined());
        assert!(trial.operation.authorization_reader.retains_close());
        assert!(trial.operation.authorization_close.is_none());
        trial.operation.settle_authorization_reader().await.unwrap();
        assert!(trial.operation.await_authorization_close().await.is_err());
        assert!(!trial.operation.authorization_reader.has_connection());
        assert_eq!(
            (
                trial.operation.authorization_reader.file_fd(),
                trial.operation.authorization_reader.connect_address()
            ),
            held
        );
        assert!(unsafe { libc::fcntl(held.0, libc::F_GETFD) } >= 0);
        assert!(trial.operation.authorization_reader().await.is_err());
    }
}

#[tokio::test]
async fn retained_control_terminal_cleanup_rejoins_pending_but_never_repolls_panicked_close() {
    use crate::native_bootstrap::TargetAuthorizationReader as TargetReader;
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    for panic in [false, true] {
        let mut trial = trial(root.path(), &bytes);
        prepare(&mut trial, &inspected).await;
        let before = pointer(&trial.operation);
        let mut reader = trial.operation.authorization_reader().await.unwrap();
        drop(reader.close_retained_control_reader());
        drop(reader);
        let close = trial.operation.authorization_close.as_mut().unwrap();
        let address =
            std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
        close.poll_fault = Some(u8::from(panic));
        TargetReader::abandon_close_waiter(trial.operation.await_authorization_close(), panic)
            .await;
        assert_eq!(
            trial
                .operation
                .authorization_close
                .as_ref()
                .unwrap()
                .polling,
            panic
        );
        drop(trial.operation.settle_authorization_reader());
        assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        if panic {
            assert!(trial.operation.settle_authorization_reader().await.is_err());
            assert!(trial.operation.settle_authorization_reader().await.is_err());
            let close = trial.operation.authorization_close.as_ref().unwrap();
            assert!(close.polling && close.outcome.is_none() && !close.resource_acknowledged);
            assert!(!trial.operation.authorization_reader.close_joined());
        } else {
            // Terminal Pending is independently resumable, using exactly the same box.
            trial
                .operation
                .authorization_close
                .as_mut()
                .unwrap()
                .poll_fault = Some(0);
            TargetReader::abandon_close_waiter(
                trial.operation.settle_authorization_reader(),
                false,
            )
            .await;
            assert!(
                !trial
                    .operation
                    .authorization_close
                    .as_ref()
                    .unwrap()
                    .polling
            );
            trial.operation.settle_authorization_reader().await.unwrap();
            trial.operation.settle_authorization_reader().await.unwrap();
            let close = trial.operation.authorization_close.as_ref().unwrap();
            assert!(close.resource_acknowledged && !close.acknowledged);
            assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
            assert!(trial.operation.authorization_reader.close_joined());
        }
        let close = trial.operation.authorization_close.as_ref().unwrap();
        assert_eq!(
            std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
            address
        );
        assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
        assert!(trial.operation.authorization_reader().await.is_err());
        assert!(trial.operation.await_authorization_close().await.is_err());
        assert_eq!(pointer(&trial.operation), before);
        assert_eq!(version(&trial.operation), 93);
        assert_eq!(target_fd(&trial.operation), trial.target_fd);
        assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
    }
}

#[tokio::test]
async fn retained_control_terminal_cleanup_observes_real_ready_close_without_reviving_expired_authority()
 {
    let (root, bytes, inspected) = super::super::original::tests::original().await;
    let mut trial = trial(root.path(), &bytes);
    prepare(&mut trial, &inspected).await;
    let before = pointer(&trial.operation);
    let facts = trial.operation.commit_state();
    let mut reader = trial.operation.authorization_reader().await.unwrap();
    drop(reader.close_retained_control_reader());
    drop(reader);
    trial.operation.deadline = Some(Instant::now());
    // SQLx genuinely joins, then the normal authority deadline refuses continuation.
    assert!(trial.operation.await_authorization_close().await.is_err());
    let close = trial.operation.authorization_close.as_ref().unwrap();
    assert!(close.resource_acknowledged && !close.acknowledged && !close.polling);
    assert!(matches!(close.outcome.as_ref(), Some(Ok(()))));
    assert!(trial.operation.authorization_reader.close_joined());
    let address =
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>();
    trial.operation.settle_authorization_reader().await.unwrap();
    trial.operation.settle_authorization_reader().await.unwrap();
    assert_eq!(trial.operation.phase(), RecordedControlPhase::Failed);
    assert!(trial.operation.authorization_reader().await.is_err());
    assert!(trial.operation.await_authorization_close().await.is_err());
    let close = trial.operation.authorization_close.as_ref().unwrap();
    assert_eq!(
        std::ptr::from_ref(close.future.as_ref().unwrap().as_ref().get_ref()).cast::<()>(),
        address
    );
    assert!(close.resource_acknowledged && !close.acknowledged);
    assert_eq!(trial.operation.commit_state(), facts);
    assert_eq!(pointer(&trial.operation), before);
    assert_eq!(version(&trial.operation), 93);
    assert_eq!(target_fd(&trial.operation), trial.target_fd);
    assert_eq!(parent_fd(&trial.operation), trial.parent_fd);
}
