//! Authored, not executed. These require the genuine physical97/96/94 fixture;
//! root's current96 prerequisite is not qualified. No fresh deadline at handoff.
use super::*;
use motion_catalog_backend::RetainedCaptureInputCustody;

async fn ingress(f: &mut Fixture, root: &Path, expected: &LifecycleProof, candidate: &Path, op: &str) -> Custody94 {
    acknowledge94(f, root, expected, candidate, op).await;
    f.service.settle_selected_relocation94_for_capture_input().await.unwrap();
    f.service.begin_selected_source94_capture_input().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    let before = custody94(f, root);
    f.service.settle_selected_capture_input_for_ingress().await.unwrap();
    f.service.retain_selected_source94_capture_ingress().unwrap();
    before
}
fn retained(f: &Fixture, before: &Custody94) -> (RetainedCaptureInputCustody, bool, (usize, usize)) {
    let phase = f.shared.phase.lock().unwrap();
    let pair = phase.pair.as_ref().unwrap();
    let target = phase.target.as_ref().unwrap();
    let evidence = target.source94_service_ingress_evidence(&pair._stopped).unwrap().unwrap();
    assert_eq!(evidence.0, before.deadline);
    assert!(evidence.3);
    assert!(phase.image.is_none() && phase.workspace.is_none());
    assert!(target.source94_evidence().is_none());
    assert_eq!(pair.motion.as_raw_fd(), f.pair_fd);
    pair._stopped.verify_lock().unwrap();
    let (selected, bound) = phase.selection.as_ref().unwrap().evidence_for_fixture();
    let (selected, hash) = selected.unwrap();
    assert!(bound);
    assert_eq!(selected.as_raw_fd(), before.selected_fd);
    assert_eq!(file_identity(selected.as_raw_fd()), before.selected_identity);
    assert_eq!(hash, f.archive_hash);
    assert_eq!(f.shared.authorization_storage.load(Ordering::Acquire), before.authorization_storage);
    assert_eq!(f.shared.selection_storage.load(Ordering::Acquire), before.selection_storage);
    let lock = File::open(&before.target_lock_path).unwrap();
    assert_eq!(file_identity(lock.as_raw_fd()), before.target_lock_identity);
    assert!(fs2::FileExt::try_lock_exclusive(&lock).is_err());
    let (physical, reusable) = target.source94_acquisition_evidence();
    (physical, reusable, (evidence.1, evidence.2))
}

#[tokio::test]
async fn actual_service91_acquisition_keeps_original_custody_and_requires_normal_current_ack() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let op = "service91-actual-acquire";
    let before = ingress(f, root.path(), &expected, &candidate, op).await;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let stable = retained(f, &before).2;
    f.shared.test.source94_pause.store(22, Ordering::Release);
    f.service.begin_selected_capture_acquisition().unwrap();
    paused94(f, 22).await;
    // The service worker owns the operation; the request only waits for status.
    drop(f.service.wait_selected_pair_restore());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    release94(f);
    f.service.wait_selected_pair_restore().await.unwrap();
    let (physical, reusable, after) = retained(f, &before);
    assert_eq!(stable, after);
    assert!(physical.target_connect && physical.target_close && physical.target_close_ack);
    assert!(!physical.target_reader);
    assert!(physical.acquire_frame && physical.acquire_started && !physical.acquire_failed);
    assert!(physical.begin_frame && physical.begin_started && !physical.begin_failed);
    assert!(physical.writer && physical.prepared && reusable);
    {
        let phase = f.shared.phase.lock().unwrap();
        assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94CapturePrepared);
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
    }
    assert!(f.service.begin_selected_capture_acquisition().is_err());
    drop(f.service.settle_selected_capture_acquisition());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    f.service.settle_selected_capture_acquisition().await.unwrap();
    assert!(f.service.restore.worker.is_none() && f.service.restore.selection_timer.is_none());
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes, "no native capture occurred");
    one_effect94(&motion, &native, op).await;
    f.close().await;
    one_effect94(&motion, &native, op).await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service91_unpolled_claim_prepare_and_target_close_leave_actual_resources() {
    for injection in [5, 7, 6] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = ingress(f, root.path(), &expected, &candidate, "service91-unpolled").await;
        let stable = retained(f, &before).2;
        f.shared.test.source94_unpolled.store(injection, Ordering::Release);
        f.service.begin_selected_capture_acquisition().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let (physical, reusable, after) = retained(f, &before);
        assert_eq!(stable, after);
        assert!(!reusable && !physical.prepared && !physical.writer);
        assert!(!physical.begin_started);
        assert_eq!(physical.target_connect, injection != 5);
        assert_eq!(physical.target_reader, injection == 7);
        assert_eq!(physical.target_close, injection == 6);
        assert!(!physical.target_close_ack, "unpolled close never manufactures an ACK");
        assert_eq!(physical.acquire_started, injection == 6);
        assert_eq!(physical.begin_frame, injection == 6);
        let phase = f.shared.phase.lock().unwrap();
        assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert_eq!(phase.pair.as_ref().unwrap()._writer.retains_authorization_acquisition(), injection != 5);
        drop(phase);
        assert!(f.service.begin_selected_capture_acquisition().is_err());
        no_later_effects(root.path());
        // In6 the exact unpolled BEGIN owns its connection and cannot be polled
        // merely for cleanup. This case intentionally retains terminal custody.
    }
}

#[tokio::test]
async fn actual_service91_cancel_and_wrapper_unwind_at_each_fixed_poll_keep_frames() {
    for stage in [22, 23] {
        for panic_at_boundary in [false, true] {
            let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
            let f = &mut fixture;
            let root = tempfile::tempdir().unwrap();
            let before = ingress(f, root.path(), &expected, &candidate, "service91-fenced").await;
            let stable = retained(f, &before).2;
            f.shared.test.source94_pause.store(stage, Ordering::Release);
            f.service.begin_selected_capture_acquisition().unwrap();
            paused94(f, stage).await;
            if panic_at_boundary { f.shared.test.source94_panic.store(true, Ordering::Release); }
            else { f.shared.cancel.store(true, Ordering::Release); }
            release94(f);
            assert!(f.service.wait_selected_pair_restore().await.is_err());
            let (physical, reusable, after) = retained(f, &before);
            assert_eq!(stable, after);
            assert!(physical.target_connect && physical.acquire_frame);
            assert_eq!(physical.acquire_started, stage == 23);
            assert_eq!(physical.target_close_ack, stage == 23);
            assert_eq!(physical.begin_frame, stage == 23);
            assert!(!physical.begin_started && !physical.writer && !physical.prepared && !reusable);
            assert!(f.service.begin_selected_capture_acquisition().is_err());
            no_later_effects(root.path());
            // This is a wrapper-boundary unwind, NOT a SQLx-origin panic.
        }
    }
}

#[tokio::test]
async fn actual_service91_backend_prepared_does_not_replace_current_reader_ack() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let before = ingress(f, root.path(), &expected, &candidate, "service91-current-ack").await;
    f.shared.test.source94_pause.store(24, Ordering::Release);
    f.service.begin_selected_capture_acquisition().unwrap();
    paused94(f, 24).await;
    f.shared.cancel.store(true, Ordering::Release);
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    let (physical, reusable, _) = retained(f, &before);
    assert!(physical.writer && physical.prepared && physical.target_close_ack);
    assert!(!reusable, "backend success cannot release failed normal current retirement");
    let phase = f.shared.phase.lock().unwrap();
    assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94CaptureAcquiring);
    assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
    assert!(phase.failure.is_some());
    drop(phase);
    assert!(f.service.begin_selected_capture_acquisition().is_err());
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service91_fresh_target_policy_denial_is_not_current_source_permission() {
    use sqlx::Connection;
    let Physical94 { mut fixture, expected, candidate, requester, reserve_admin, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let before = ingress(f, root.path(), &expected, &candidate, "service91-target-denied").await;
    let (motion, _) = names(f, root.path());
    let mut target = sqlx::SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&motion).foreign_keys(true)).await.unwrap();
    motion_catalog_backend::projection::configure_connection(&mut target).await.unwrap();
    sqlx::query("BEGIN IMMEDIATE").execute(&mut target).await.unwrap();
    // This existing policy-owner helper removes the real grant, including
    // SystemAdmin, while a distinct real admin satisfies admin-survival policy.
    crate::v2::auth::retained::deny_target_source_management_in(&mut target, &reserve_admin, &requester).await.unwrap();
    sqlx::query("COMMIT").execute(&mut target).await.unwrap();
    target.close().await.unwrap();
    f.service.begin_selected_capture_acquisition().unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    let (physical, reusable, _) = retained(f, &before);
    assert!(physical.target_connect && physical.target_reader);
    assert!(!physical.acquire_frame && !physical.begin_frame && !physical.writer && !reusable);
    let phase = f.shared.phase.lock().unwrap();
    let Some(Failure::Error(error)) = &phase.failure else { panic!("actual target authorization error required") };
    assert!(format!("{error:#}").contains("403") || format!("{error:#}").contains("forbidden"), "{error:#}");
    assert!(phase.pair.as_ref().unwrap()._writer.retains_authorization_reader());
    drop(phase);
    assert!(f.service.begin_selected_capture_acquisition().is_err());
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service91_busy_begin_keeps_actual_failed_begin_and_no_writer() {
    use sqlx::Connection;
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let before = ingress(f, root.path(), &expected, &candidate, "service91-busy-begin").await;
    let (motion, _) = names(f, root.path());
    f.shared.test.source94_pause.store(23, Ordering::Release);
    f.service.begin_selected_capture_acquisition().unwrap();
    paused94(f, 23).await;
    // Deliberate external contention, no policy writes or fabricated outcome.
    let mut competing = sqlx::SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&motion).busy_timeout(Duration::ZERO)).await.unwrap();
    sqlx::query("BEGIN IMMEDIATE").execute(&mut competing).await.unwrap();
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    let (physical, reusable, _) = retained(f, &before);
    assert!(physical.target_close_ack && physical.acquire_frame && physical.acquire_started);
    assert!(physical.begin_frame && physical.begin_started && physical.begin_failed);
    assert!(!physical.writer && !physical.prepared && !reusable);
    sqlx::query("ROLLBACK").execute(&mut competing).await.unwrap();
    competing.close().await.unwrap();
    assert!(f.service.begin_selected_capture_acquisition().is_err());
    no_later_effects(root.path());
    // SQLx returns no original connection on BEGIN Err. Its retained real error
    // and frame are uncertainty, not source-close ACK or permission to retry.
}

#[path = "service_native_capture_tests.rs"]
mod service_native_capture_tests;
