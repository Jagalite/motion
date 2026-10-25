use super::*;
use std::{cell::Cell, future::Future, task::Poll};
thread_local! { static AFTER_NATIVE: Cell<bool> = const { Cell::new(false) }; }
pub(super) async fn after_native(pause: bool, panic: bool) {
    AFTER_NATIVE.set(true);
    if panic {
        panic!("fixed post-native capture panic");
    }
    if pause {
        std::future::pending::<()>().await;
    }
}
async fn catch<F: Future>(future: F) -> Result<F::Output, ()> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(_) => Poll::Ready(Err(())),
        }
    })
    .await
}
async fn fixture() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    StageSourceSession,
    Transaction<'static, Sqlite>,
) {
    let (root, path, native, upgraded, _) =
        crate::offline_control_upgrade::source_session::tests::upgraded_fixture_with_record(true)
            .await;
    let session = upgraded
        .prepare_native_reserved(
            &native,
            catabolic_store::migration::NativeUpgradeQuiescence {
                admission_stopped: true,
                readers_and_workers_stopped: true,
                exclusive_operational_ownership: true,
            },
            "capture-native-transition",
            1024 * 1024,
        )
        .unwrap()
        .apply(None)
        .unwrap()
        .save_command("capture-bootstrap", &[])
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
                .filename(path)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    (root, pool, session, writer)
}
async fn operation() -> (
    tempfile::TempDir,
    sqlx::SqlitePool,
    StageImageCaptureOperation,
) {
    let (root, pool, mut source, mut writer) = fixture().await;
    source
        .admit_current_image_operation(
            &mut writer,
            Instant::now() + Duration::from_secs(60),
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
    let operation = match source.begin_current_image_capture(writer) {
        Ok(operation) => operation,
        Err(_) => panic!("admitted capture handoff refused"),
    };
    (root, pool, operation)
}
#[tokio::test]
async fn genuine_capture_requires_admission_and_final_same_writer_verification() {
    let (root, pool, source, writer) = fixture().await;
    let (mut source, mut writer) = match source.begin_current_image_capture(writer) {
        Err(parts) => parts,
        Ok(_) => panic!("unadmitted source moved"),
    };
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    drop(source.admit_current_image_operation(&mut writer, deadline, &cancel));
    let (mut source, mut writer) = match source.begin_current_image_capture(writer) {
        Err(parts) => parts,
        Ok(_) => panic!("unpolled admission moved source"),
    };
    let original = source.capture_admission.as_ref().unwrap().deadline;
    source
        .admit_current_image_operation(&mut writer, deadline + Duration::from_secs(60), &cancel)
        .await
        .unwrap();
    assert_eq!(
        source.capture_admission.as_ref().unwrap().deadline,
        original
    );
    assert!(source.capture_admission.as_ref().unwrap().storage > 0);
    let first = source.proof.allocation_provenance();
    source
        .admit_current_image_operation(&mut writer, deadline + Duration::from_secs(60), &cancel)
        .await
        .unwrap();
    assert_eq!(
        source.capture_admission.as_ref().unwrap().deadline,
        original
    );
    assert_eq!(source.proof.allocation_provenance().0, first.0);
    assert!(source.proof.allocation_provenance().1 >= first.1);
    let provenance = source.proof.allocation_provenance();
    let mut operation = match source.begin_current_image_capture(writer) {
        Ok(operation) => operation,
        Err(_) => panic!("admitted source refused"),
    };
    operation.capture(&cancel).await.unwrap();
    assert!(matches!(operation.parts_for_test().native, Some(Native::Image(_))));
    let db = operation.writer_in().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(db)
            .await
            .unwrap(),
        1
    );
    let mut operation = match operation.finish() {
        Err(operation) => operation,
        Ok(_) => panic!("post-auth final verification skipped"),
    };
    operation.verify(&cancel).await.unwrap();
    assert_eq!(operation.parts_for_test().proof.allocation_provenance().0, provenance.0);
    assert!(operation.parts_for_test().proof.allocation_provenance().1 >= provenance.1);
    let mut image = match operation.finish() {
        Ok(image) => image,
        Err(_) => panic!("capture not complete"),
    };
    image.verify_current(&cancel).await.unwrap();
    let (source, writer) = image.discard_copy();
    writer.rollback().await.unwrap();
    source.close().unwrap();
    pool.close().await;
    drop(root);
}
#[tokio::test]
async fn genuine_unpolled_and_cancelled_capture_keep_terminal_custody() {
    for cancel_now in [false, true] {
        let (root, pool, mut operation) = operation().await;
        let cancel = AtomicBool::new(cancel_now);
        if cancel_now {
            assert!(operation.capture(&cancel).await.is_err());
        } else {
            drop(operation.capture(&cancel));
        }
        assert!(matches!(operation.parts_for_test().native, Some(Native::Session(_))));
        assert!(operation.capture(&AtomicBool::new(false)).await.is_err());
        let mut operation = match operation.finish() {
            Err(operation) => operation,
            Ok(_) => panic!("failed capture escaped"),
        };
        drop(operation.close());
        assert!(operation.parts_for_test().native.is_some());
        assert_eq!(
            writer_identity(operation.parts_for_test().writer.as_mut().unwrap()).await.unwrap(),
            operation.parts_for_test().admission.writer.unwrap()
        );
        operation.close().await.unwrap();
        assert!(operation.parts_for_test().native.is_none());
        drop(operation);
        pool.close().await;
        drop(root);
    }
}
#[tokio::test]
async fn genuine_capture_rejects_replaced_writer_and_expired_deadline() {
    let (root, pool, mut source, mut original) = fixture().await;
    let cancel = AtomicBool::new(false);
    source
        .admit_current_image_operation(
            &mut original,
            Instant::now() + Duration::from_secs(60),
            &cancel,
        )
        .await
        .unwrap();
    let foreign = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let writer = foreign.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let mut operation = match source.begin_current_image_capture(writer) {
        Ok(op) => op,
        Err(_) => panic!("admitted slot missing"),
    };
    assert!(
        operation
            .capture(&cancel)
            .await
            .unwrap_err()
            .to_string()
            .contains("writer changed")
    );
    assert!(matches!(operation.parts_for_test().native, Some(Native::Session(_))));
    assert!(
        operation
            .close()
            .await
            .unwrap_err()
            .to_string()
            .contains("writer changed")
    );
    assert!(operation.parts_for_test().native.is_some());
    assert!(operation.parts_for_test().phase != Phase::Closed);
    // Deliberately unsupported mismatched input cannot claim settled original
    // custody. The fixture's external owner explicitly ends its original
    // writer; dropping the opaque failed slot is teardown, not a close grant.
    original.rollback().await.unwrap();
    drop(operation);
    foreign.close().await;
    pool.close().await;
    drop(root);
    let (root, pool, mut source, mut writer) = fixture().await;
    assert!(
        source
            .admit_current_image_operation(&mut writer, Instant::now(), &cancel)
            .await
            .is_err()
    );
    let (source, writer) = match source.begin_current_image_capture(writer) {
        Err(parts) => parts,
        Ok(_) => panic!("expired admission moved"),
    };
    writer.rollback().await.unwrap();
    source.close().unwrap();
    pool.close().await;
    drop(root);
}
#[tokio::test]
async fn genuine_post_native_copy_await_drop_and_panic_keep_original_owner() {
    for panic in [false, true] {
        let (root, pool, mut operation) = operation().await;
        operation.parts_for_test().test_pause = !panic;
        operation.parts_for_test().test_panic = panic;
        AFTER_NATIVE.set(false);
        let cancel = AtomicBool::new(false);
        if panic {
            assert!(catch(operation.capture(&cancel)).await.is_err());
        } else {
            let future = operation.capture(&cancel);
            let mut future = std::pin::pin!(future);
            std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                if AFTER_NATIVE.get() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        }
        assert!(AFTER_NATIVE.get());
        assert!(matches!(operation.parts_for_test().native, Some(Native::Image(_))));
        assert!(operation.capture(&cancel).await.is_err());
        let mut operation = match operation.finish() {
            Err(op) => op,
            Ok(_) => panic!("incomplete capture escaped"),
        };
        assert_eq!(
            writer_identity(operation.parts_for_test().writer.as_mut().unwrap()).await.unwrap(),
            operation.parts_for_test().admission.writer.unwrap()
        );
        operation.parts_for_test().witness.verify(false).unwrap();
        operation.close().await.unwrap();
        drop(operation);
        pool.close().await;
        drop(root);
    }
}

#[tokio::test]
async fn retained81_settled_sqlite_and_new_writer_count_once_through_capture() {
    let (_root, _stage, _original, motion, session, old_handle, frozen, actual_old, accepted) =
        crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::tests::retained81_source_capture_fixture().await;
    let mut source = session.inner;
    assert_eq!(source.proof.allocation_provenance(), accepted);
    assert_eq!(
        source.native.external_retained_reservation().unwrap(),
        accepted.1
    );
    let (tail, metadata) = source.proof.retained_tail_accounting_for_test().unwrap();
    assert_eq!(tail, frozen);
    assert!(tail >= actual_old && actual_old > 0);
    assert_eq!(
        source.proof.retained_allocations().unwrap(),
        metadata + tail
    );

    // This is an actual different SQLite connection, simultaneously alive with
    // the permanently inert old rusqlite owner inside this same proof's tail.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(motion)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let new_target = {
        let mut lease = writer.lock_handle().await.unwrap();
        let db = unsafe { Connection::from_handle(lease.as_raw_handle().as_ptr()) }.unwrap();
        crate::motion_snapshot::connection_allocations(&[&db]).unwrap()
    };
    assert!(new_target > 0);
    // Test-only DBSTATUS read after the new connection is acquired: the old
    // handle is still genuinely alive in this proof, not a stale sampled owner.
    // No SQL or mutation is performed on the permanently inert old connection.
    let mut old_simultaneous = 0u64;
    for status in [
        rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_SCHEMA_USED,
        rusqlite::ffi::SQLITE_DBSTATUS_STMT_USED,
    ] {
        let (mut current, mut high) = (0, 0);
        assert_eq!(
            unsafe {
                rusqlite::ffi::sqlite3_db_status(
                    old_handle as *mut rusqlite::ffi::sqlite3,
                    status,
                    &mut current,
                    &mut high,
                    0,
                )
            },
            rusqlite::ffi::SQLITE_OK
        );
        assert!(current >= 0);
        old_simultaneous += current as u64;
    }
    assert!(old_simultaneous > 0 && old_simultaneous <= tail);
    let simultaneous = accepted.0 + metadata + tail + new_target;
    assert!(simultaneous >= accepted.0 + metadata + old_simultaneous + new_target);
    assert!(simultaneous > accepted.0 + metadata + new_target);
    // A separate later capture has its existing operation deadline. Nothing in
    // the fixture renewed the original97→96/81 producer deadline to get here.
    let deadline = Instant::now() + Duration::from_secs(60);
    let cancel = AtomicBool::new(false);
    for _ in 0..2 {
        source
            .admit_current_image_operation(&mut writer, deadline, &cancel)
            .await
            .unwrap();
        assert_eq!(source.proof.allocation_provenance().0, accepted.0);
        assert_eq!(
            source.proof.retained_tail_accounting_for_test().unwrap(),
            (tail, metadata)
        );
        assert_eq!(
            source.proof.retained_allocations().unwrap(),
            metadata + tail
        );
        assert!(source.native.external_retained_reservation().unwrap() >= simultaneous);
    }
    let mut capture = match source.begin_current_image_capture(writer) {
        Ok(capture) => capture,
        Err(_) => panic!("genuine source capture admission"),
    };
    assert_eq!(
        capture.parts_for_test().proof.retained_tail_accounting_for_test().unwrap(),
        (tail, metadata)
    );
    capture.capture(&cancel).await.unwrap();
    capture.verify(&cancel).await.unwrap();
    assert_eq!(capture.parts_for_test().proof.allocation_provenance().0, accepted.0);
    assert_eq!(
        capture.parts_for_test().proof.retained_allocations().unwrap(),
        metadata + tail
    );
    let mut image = match capture.finish() {
        Ok(image) => image,
        Err(_) => panic!("genuine current-image handoff"),
    };
    image.verify_current(&cancel).await.unwrap();
    assert_eq!(image.proof.allocation_provenance().0, accepted.0);
    assert_eq!(
        image.proof.retained_tail_accounting_for_test().unwrap(),
        (tail, metadata)
    );
    assert_eq!(image.proof.retained_allocations().unwrap(), metadata + tail);
    let (source, writer) = image.discard_copy();
    assert_eq!(source.proof.allocation_provenance().0, accepted.0);
    assert_eq!(
        source.proof.retained_tail_accounting_for_test().unwrap(),
        (tail, metadata)
    );
    assert_eq!(
        source.proof.retained_allocations().unwrap(),
        metadata + tail
    );
    writer.rollback().await.unwrap();
    source.close().unwrap();
    pool.close().await;
}

#[tokio::test]
async fn retained81_legacy_source_without_tail_keeps_existing_owned_count() {
    // Existing genuine consuming producer drops its old Motion DB. It must not
    // acquire a phantom tail charge when this additive retained path is present.
    let (_root, pool, source, writer) = fixture().await;
    let (tail, metadata) = source.proof.retained_tail_accounting_for_test().unwrap();
    assert_eq!(tail, 0);
    assert_eq!(source.proof.retained_allocations().unwrap(), metadata);
    let before = source.proof.allocation_provenance();
    assert_eq!(source.proof.retained_allocations().unwrap(), metadata);
    assert_eq!(source.proof.allocation_provenance(), before);
    writer.rollback().await.unwrap();
    source.close().unwrap();
    pool.close().await;
}

// These exercise only the numeric source adapters with an actual retained81
// owner. The app's whole94/service90/service91 fixtures separately establish
// receipt history, target close, fresh authorization and ingress custody.
fn retain_ingress_constructor_pressure(source: &mut StageSourceSession) -> u64 {
    use catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation;
    assert!(source.source_close_admitted94());
    assert!(source.retained_bootstrap_slot_for_test().0.is_some());
    assert_eq!(source.source_workspace_qualification94_for_test(), None);
    let (host, previous) = source.proof.allocation_provenance();
    assert_eq!(source.native.external_retained_reservation().unwrap(), previous);
    let forwarded = previous.max(225 * 1024 * 1024);
    let native = source.native.retained_allocations().unwrap();
    assert!(native.checked_add(forwarded).unwrap() <= 256 * 1024 * 1024,
        "the actual retained aggregate must fit, not merely the scalar");
    assert!(OfflineAllocationReservation::new(forwarded).is_err());
    // Raise only a conservative previous K through the existing exact-tail
    // refresh. No H is discounted or converted into workspace authority.
    // Success itself requires a fresh eligible P measurement of this owner.
    source.proof.refresh_source_workspace94(&mut source.native, &source.reference,
        &source.witness, forwarded).unwrap();
    source.proof.record_allocation_forward(previous, forwarded).unwrap();
    assert_eq!(source.source_workspace_provenance94_for_test().unwrap(),
        (host, forwarded, forwarded));
    assert_eq!(source.source_workspace_qualification94_for_test(), None);
    forwarded
}

async fn ingress_writer(path: &std::path::Path) -> (sqlx::SqlitePool, Transaction<'static, Sqlite>) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(path).foreign_keys(true))
        .await.unwrap();
    let writer = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    (pool, writer)
}

#[tokio::test]
async fn retained_ingress_refreshes_initial_repeat_and_writer_without_capture_authority() {
    use std::os::fd::AsRawFd;
    // Fixed before constructing the predecessor, never renewed by a retry.
    let deadline = Instant::now() + Duration::from_secs(60);
    let (_root, _stage, _original, motion, session, _old, _frozen, _actual, _accepted) =
        crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::tests::retained81_source_capture_fixture().await;
    let mut source = session.inner;
    let tail = source.retained_bootstrap_slot_for_test();
    let witness = source.witness.file.as_raw_fd();
    let pressure = retain_ingress_constructor_pressure(&mut source);
    let host = source.capture_ingress_host94().unwrap();
    let cancel = AtomicBool::new(false);
    for _ in 0..2 {
        source.retain_capture_ingress94(host, deadline, &cancel).unwrap();
        let (h, k, actual) = source.source_workspace_provenance94_for_test().unwrap();
        assert_eq!(h, host);
        assert_eq!(k, actual);
        assert!(k >= pressure);
        assert_eq!(source.retained_bootstrap_slot_for_test(), tail);
        assert_eq!(source.witness.file.as_raw_fd(), witness);
        assert_eq!(source.source_workspace_qualification94_for_test(), None);
        assert!(source.source_close_admitted94() && source.capture_admission.is_none());
    }
    let (pool, mut writer) = ingress_writer(&motion).await;
    let writer_id = writer_identity(&mut writer).await.unwrap();
    let before = source.source_workspace_provenance94_for_test().unwrap();
    source.admit_capture_ingress_writer94(&mut writer, deadline, &cancel).await.unwrap();
    let after = source.source_workspace_provenance94_for_test().unwrap();
    assert_eq!(after.0, host);
    assert!(after.1 >= before.1 && after.1 == after.2);
    assert_eq!(source.retained_bootstrap_slot_for_test(), tail);
    assert_eq!(source.witness.file.as_raw_fd(), witness);
    assert_eq!(source.source_workspace_qualification94_for_test(), None);
    assert_eq!(writer_identity(&mut writer).await.unwrap(), writer_id);
    assert!(source.source_close_admitted94() && source.capture_admission.is_none());
    let (source, writer) = match source.begin_current_image_capture(writer) {
        Err(parts) => parts,
        Ok(_) => panic!("ingress accounting granted native capture"),
    };
    writer.rollback().await.unwrap();
    pool.close().await;
    let mut close = match source.begin_retained_close() {
        Ok(close) => close,
        Err(_) => panic!("original retained close admission was lost"),
    };
    close.close().unwrap();
    assert_eq!(close.custody_for_test().3, witness);
    assert_eq!(close.custody_for_test().4, tail.0);
}

#[tokio::test]
async fn oversized_ingress_workspace_refuses_both_refreshes_and_keeps_claims_and_custody() {
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + Duration::from_secs(60);
    let (_root, _stage, _original, motion, session, _old, _frozen, _actual, _accepted) =
        crate::native_bootstrap::prepare_operation::apply_operation::bootstrap_operation::tests::retained81_source_capture_fixture().await;
    let mut source = session.inner;
    let tail = source.retained_bootstrap_slot_for_test();
    let witness = source.witness.file.as_raw_fd();
    retain_ingress_constructor_pressure(&mut source);
    let before = source.source_workspace_provenance94_for_test().unwrap();
    source.enlarge_source_workspace94_for_test();
    let cancel = AtomicBool::new(false);
    // Fresh P, not the earlier eligible refresh, must choose full charge now.
    let error = source.retain_capture_ingress94(before.0, deadline, &cancel).unwrap_err();
    assert_eq!(error.to_string(), "offline aggregate allocation budget exhausted");
    assert_eq!(source.source_workspace_provenance94_for_test().unwrap(), before);
    let (pool, mut writer) = ingress_writer(&motion).await;
    let writer_id = writer_identity(&mut writer).await.unwrap();
    let error = source.admit_capture_ingress_writer94(&mut writer, deadline, &cancel)
        .await.unwrap_err();
    assert_eq!(error.to_string(), "offline aggregate allocation budget exhausted");
    assert_eq!(source.source_workspace_provenance94_for_test().unwrap(), before);
    // A new retained H claim is not rolled back on refusal, but unsuccessful
    // forwarding must not be recorded as a successful Native reservation.
    let larger_host = before.0.checked_add(1).unwrap();
    assert!(source.retain_capture_ingress94(larger_host, deadline, &cancel).is_err());
    assert_eq!(source.source_workspace_provenance94_for_test().unwrap(),
        (larger_host, before.1, before.2));
    assert_eq!(source.retained_bootstrap_slot_for_test(), tail);
    assert_eq!(source.witness.file.as_raw_fd(), witness);
    assert_eq!(source.source_workspace_qualification94_for_test(), None);
    assert_eq!(writer_identity(&mut writer).await.unwrap(), writer_id);
    assert!(source.source_close_admitted94() && source.capture_admission.is_none());
    source.verify_pair_in(&mut writer).await.unwrap();
    writer.rollback().await.unwrap();
    pool.close().await;
    let mut close = match source.begin_retained_close() {
        Ok(close) => close,
        Err(_) => panic!("failed refresh discarded the original close admission"),
    };
    close.close().unwrap();
    assert_eq!(close.custody_for_test().3, witness);
    assert_eq!(close.custody_for_test().4, tail.0);
}

#[tokio::test]
async fn legacy_no_tail_ingress_preflight_keeps_ordinary_refresh_and_no_capture_grant() {
    use catabolic_store::migration::offline_bootstrap::OfflineAllocationReservation;
    let deadline = Instant::now() + Duration::from_secs(60);
    let (_root, pool, mut source, mut writer) = fixture().await;
    assert!(source.retained_bootstrap_slot_for_test().0.is_none());
    assert!(!source.source_close_admitted94());
    let before = source.source_workspace_provenance94_for_test().unwrap();
    let proposed = before.2.max(225 * 1024 * 1024);
    assert!(source.native.retained_allocations().unwrap().checked_add(proposed).unwrap()
        <= 256 * 1024 * 1024);
    assert!(OfflineAllocationReservation::new(proposed).is_err());
    let error = source.proof.refresh_source_workspace94(&mut source.native, &source.reference,
        &source.witness, proposed).unwrap_err();
    assert_eq!(error.to_string(), "offline aggregate allocation budget exhausted");
    assert_eq!(source.source_workspace_provenance94_for_test().unwrap(), before);
    // This numeric adapter is not a whole94 ingress grant. With ordinary
    // headroom it uses the original no-tail owner and leaves capture unadmitted.
    let writer_id = writer_identity(&mut writer).await.unwrap();
    source.admit_capture_ingress_writer94(&mut writer, deadline, &AtomicBool::new(false))
        .await.unwrap();
    let after = source.source_workspace_provenance94_for_test().unwrap();
    assert_eq!(after.0, before.0);
    assert!(after.1 >= before.1 && after.1 == after.2);
    assert!(source.retained_bootstrap_slot_for_test().0.is_none());
    assert!(!source.source_close_admitted94() && source.capture_admission.is_none());
    assert_eq!(writer_identity(&mut writer).await.unwrap(), writer_id);
    let (source, writer) = match source.begin_current_image_capture(writer) {
        Err(parts) => parts,
        Ok(_) => panic!("no-tail numeric preflight granted native capture"),
    };
    writer.rollback().await.unwrap();
    source.close().unwrap();
    pool.close().await;
}

#[tokio::test]
async fn genuine_paired_capture_refuses_ordinary_owner_without_claim_or_effect() {
    use sqlx::TransactionManager;
    let (_root, pool, mut operation) = operation().await;
    let phase = operation.parts_for_test().phase;
    let deadline = operation.parts_for_test().admission.deadline;
    let writer = writer_identity(operation.parts_for_test().writer.as_mut().unwrap()).await.unwrap();
    drop(operation.prepare_retained_pair(std::sync::Arc::new(AtomicBool::new(false))));
    assert!(operation.parts_for_test().phase == phase);
    assert!(operation.prepare_retained_pair(std::sync::Arc::new(AtomicBool::new(false))).await.is_err());
    assert!(operation.complete_retained_pair(&AtomicBool::new(false)).is_err());
    assert!(!operation.paired_captured());
    assert!(!operation.paired_closure_retained());
    assert!(operation.parts_for_test().phase == phase);
    assert_eq!(operation.parts_for_test().admission.deadline, deadline);
    assert_eq!(writer_identity(operation.parts_for_test().writer.as_mut().unwrap()).await.unwrap(), writer);
    assert!(matches!(operation.parts_for_test().native, Some(Native::Session(_))));
    operation.close().await.unwrap();
    assert!(operation.parts_for_test().phase == Phase::Closed);
    assert!(operation.parts_for_test().native.is_none());
    assert_eq!(
        sqlx::sqlite::SqliteTransactionManager::get_transaction_depth(
            operation.parts_for_test().writer.as_ref().unwrap(),
        ),
        0
    );
    // close() settles the writer but retains its checked-out pool connection.
    drop(operation);
    pool.close().await;
}
