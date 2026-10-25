//! Genuine same-owner NativeCaptured -> opaque PairedCaptured service fixtures.
//! Source-authored only until the root's exact-tree build/runtime qualification.
//! No deadline extension, fabricated readiness, writable auth or95/install grant.
use super::*;
use motion_catalog_backend::RetainedPairedCaptureCustody;

struct PairedCapturePauseRelease(Arc<Shared>);
impl Drop for PairedCapturePauseRelease {
    fn drop(&mut self) {
        if std::thread::panicking() { self.0.cancel.store(true, Ordering::Release); }
        *self.0.test.source94_released.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.0.test.source94_release.notify_one();
        self.0.test.source94_paired_auth_gate.notify_one();
    }
}
async fn native_captured(f: &mut Fixture, root: &Path, expected: &LifecycleProof, candidate: &Path, op: &str) -> Custody94 {
    let before = prepared(f, root, expected, candidate, op).await;
    f.service.begin_selected_native_capture().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    // Actual positive Native output is insufficient until both original service
    // handles are joined. This refusal neither begins nor retries paired work.
    assert!(f.service.begin_selected_paired_capture().is_err());
    f.service.settle_selected_native_capture().await.unwrap();
    before
}
fn paired_evidence(f: &Fixture, before: &Custody94) -> (RetainedPairedCaptureCustody, (bool, bool, bool), (usize, usize)) {
    let (_, _, stable) = retained(f, before);
    let phase = f.shared.phase.lock().unwrap();
    let target = phase.target.as_ref().unwrap();
    let native = target.source94_native_capture_custody();
    assert!(native.parts && native.native_attempted && native.native_captured);
    assert!(native.native_retained && native.histories_retained);
    assert!(native.target_connect && native.target_close && native.target_close_ack);
    assert!(!native.target_reader, "the prior native-purpose reader remains actually retired");
    (target.source94_paired_capture_custody(), target.source94_paired_capture_evidence(), stable)
}
async fn no_paired_effects(motion: &Path, root: &Path) {
    assert_eq!(rows(motion, "catabolic_paired_restore_intents").await, 0);
    assert_eq!(rows(motion, "catabolic_paired_restore_acks").await, 0);
    no_later_effects(root);
}

#[tokio::test]
async fn actual_service116_paired_capture_reuses_original_owner_deadline_and_workspace() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let op = "service116-paired";
    let before = native_captured(f, root.path(), &expected, &candidate, op).await;
    let stable = retained(f, &before).2;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let _release = PairedCapturePauseRelease(Arc::clone(&f.shared));
    f.shared.test.source94_pause.store(31, Ordering::Release);
    f.service.begin_selected_paired_capture().unwrap();
    paused94(f, 31).await;
    drop(f.service.wait_selected_pair_restore());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    release94(f);
    f.service.wait_selected_pair_restore().await.unwrap();
    let (custody, state, after) = paired_evidence(f, &before);
    assert_eq!(stable, after);
    assert_eq!(state, (true, true, true));
    assert!(custody.prepared && custody.paired_captured && custody.native_retained);
    assert!(custody.target_connect && custody.target_close && custody.target_close_ack);
    assert!(!custody.target_reader);
    assert!(custody.writer_held && custody.histories_retained);
    let frame = f.shared.test.source94_async_frame.load(Ordering::Acquire);
    assert!(frame > 0 && frame <= crate::native_service::pair::restore_input::target::conversion::source_image::supervisor::target_reservation::ASYNC_FRAME_LIMIT);
    {
        let mut phase = f.shared.phase.lock().unwrap();
        assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94PairedCaptured);
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        phase.target.as_mut().unwrap().refuse_source94_native_onward_for_fixture().await;
    }
    assert!(f.service.begin_selected_paired_capture().is_err());
    assert!(f.service.begin_selected_native_capture().is_err());
    drop(f.service.settle_selected_paired_capture());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    f.service.settle_selected_paired_capture().await.unwrap();
    assert!(f.service.restore.worker.is_none() && f.service.restore.selection_timer.is_none());
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
    one_effect94(&motion, &native, op).await;
    no_paired_effects(&motion, root.path()).await;
    {
        let mut phase = f.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, .. } = &mut *phase;
        let target = target.as_mut().unwrap();
        target.settle_source94(&mut pair.as_mut().unwrap()._writer).await.unwrap();
        let native = target.source94_native_capture_custody();
        assert!(native.histories_retained && native.writer_handed_back && native.exact_close_ack);
        assert!(native.release_attempted && native.released && !native.writer_held);
        assert!(!target.source94_paired_capture_evidence().2);
    }
    f.close().await;
    one_effect94(&motion, &native, op).await;
    no_paired_effects(&motion, root.path()).await;
}

#[tokio::test]
async fn actual_service116_unpolled_paired_prepare_auth_and_closes_keep_original_custody() {
    for injection in [13, 14, 15, 16, 17] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = native_captured(f, root.path(), &expected, &candidate, "service116-unpolled").await;
        let stable = retained(f, &before).2;
        let (motion, _) = names(f, root.path());
        f.shared.test.source94_unpolled.store(injection, Ordering::Release);
        f.service.begin_selected_paired_capture().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let (custody, state, after) = paired_evidence(f, &before);
        assert_eq!(stable, after);
        assert!(custody.native_retained && custody.writer_held && custody.histories_retained);
        assert_eq!(custody.prepared, injection >= 15);
        assert_eq!(custody.paired_captured, injection == 17);
        assert_eq!(custody.target_connect, injection >= 15);
        assert_eq!(custody.target_reader, injection == 15);
        assert_eq!(custody.target_close, injection >= 16);
        assert_eq!(custody.target_close_ack, injection == 17);
        assert_eq!(state, (injection == 17, false, false));
        {
            let phase = f.shared.phase.lock().unwrap();
            assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
            assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94PairedCapturing);
        }
        assert!(f.service.begin_selected_paired_capture().is_err());
        assert!(f.service.begin_selected_native_capture().is_err());
        no_paired_effects(&motion, root.path()).await;
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service116_cancel_and_postproof_panic_retain_native_closure_and_histories() {
    for stage in [29, 30, 31, 32] {
        for panic_at_boundary in [false, true] {
            let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
            let f = &mut fixture;
            let root = tempfile::tempdir().unwrap();
            let before = native_captured(f, root.path(), &expected, &candidate, "service116-interrupted").await;
            let stable = retained(f, &before).2;
            let (motion, _) = names(f, root.path());
            let _release = PairedCapturePauseRelease(Arc::clone(&f.shared));
            f.shared.test.source94_pause.store(stage, Ordering::Release);
            f.service.begin_selected_paired_capture().unwrap();
            paused94(f, stage).await;
            if panic_at_boundary { f.shared.test.source94_panic.store(true, Ordering::Release); }
            else { f.shared.cancel.store(true, Ordering::Release); }
            release94(f);
            assert!(f.service.wait_selected_pair_restore().await.is_err());
            let (custody, state, after) = paired_evidence(f, &before);
            assert_eq!(stable, after);
            assert!(custody.prepared && custody.native_retained && custody.writer_held && custody.histories_retained);
            assert_eq!(custody.paired_captured, stage == 32);
            assert_eq!(custody.target_close_ack, stage >= 31 || (stage == 30 && !panic_at_boundary));
            assert_eq!(state, (stage == 32, false, false));
            {
                let phase = f.shared.phase.lock().unwrap();
                assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94PairedCapturing);
                assert_eq!(matches!(&phase.failure, Some(Failure::Panic(_))), panic_at_boundary);
            }
            assert!(f.service.begin_selected_paired_capture().is_err());
            no_paired_effects(&motion, root.path()).await;
            f.close().await;
        }
    }
}

#[tokio::test]
async fn actual_service116_cancellation_during_genuine_current_and_target_auth_awaits_keeps_custody() {
    for auth_stage in [1, 2] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = native_captured(f, root.path(), &expected, &candidate, "service116-auth-await").await;
        let stable = retained(f, &before).2;
        let (motion, _) = names(f, root.path());
        let _release = PairedCapturePauseRelease(Arc::clone(&f.shared));
        f.shared.test.source94_paired_auth_pause.store(auth_stage, Ordering::Release);
        f.service.begin_selected_paired_capture().unwrap();
        loop {
            if f.shared.test.source94_paired_auth_entered.load(Ordering::Acquire) { break; }
            assert_eq!(f.shared.status.load(Ordering::Acquire), RUNNING, "paired work failed before genuine auth await");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // The actual auth future is Pending after validating its real reader.
        // Dropping a request wait does not own or abort that retained future.
        drop(f.service.wait_selected_pair_restore());
        assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
        f.shared.cancel.store(true, Ordering::Release);
        f.shared.test.source94_paired_auth_gate.notify_one();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let (custody, state, after) = paired_evidence(f, &before);
        assert_eq!(stable, after);
        assert!(custody.prepared && custody.native_retained && custody.writer_held && custody.histories_retained);
        assert!(!custody.paired_captured && !custody.target_close_ack);
        assert_eq!(custody.target_connect, auth_stage == 2);
        assert_eq!(state, (false, false, false));
        assert!(f.service.begin_selected_paired_capture().is_err());
        no_paired_effects(&motion, root.path()).await;
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service116_real_expiry_blocks_paired_transition_after_proof_and_exact_target_close() {
    use sqlx::Connection;
    for stage in [29, 31] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_native_capture_expiry_fixture().await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = native_captured(f, root.path(), &expected, &candidate, "service116-expiry").await;
        let expiry = f.target_expiry.unwrap();
        let (motion, _) = names(f, root.path());
        let _release = PairedCapturePauseRelease(Arc::clone(&f.shared));
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.service.begin_selected_paired_capture().unwrap();
        paused94(f, stage).await;
        // Real late policy revocation cannot commit under either supported
        // WRITE lock. This verifies contention, not fabricated revocation.
        for path in [&motion, &f.source_motion_path] {
            let mut competing = sqlx::SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path).create_if_missing(false).busy_timeout(Duration::ZERO)).await.unwrap();
            let blocked = sqlx::query("BEGIN IMMEDIATE").execute(&mut competing).await.unwrap_err();
            let code = blocked.as_database_error().and_then(|error| error.code())
                .and_then(|code| code.parse::<i32>().ok());
            assert!(code.is_some_and(|code| matches!(code & 0xff, 5 | 6)),
                "actual retained WRITE must reject with SQLITE_BUSY/LOCKED, not an unrelated failure: {blocked}");
            competing.close().await.unwrap();
        }
        assert!(crate::now() < expiry, "actual credential must reach paired seam before expiry");
        let native_deadline = f.shared.test.source94_native_deadline.lock().unwrap().unwrap();
        let remaining = u64::try_from(expiry - crate::now()).unwrap();
        assert!(Instant::now() + Duration::from_secs(remaining + 1) < native_deadline,
            "credential expiry must precede the unchanged Native deadline");
        assert!(Instant::now() + Duration::from_secs(remaining + 1) < before.deadline,
            "credential expiry must precede the unchanged service deadline");
        while crate::now() < expiry { tokio::time::sleep(Duration::from_millis(100)).await; }
        assert!(Instant::now() < native_deadline && Instant::now() < before.deadline);
        assert!(!f.shared.cancel.load(Ordering::Acquire));
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let (custody, state, _) = paired_evidence(f, &before);
        assert!(custody.prepared && custody.native_retained && custody.writer_held && custody.histories_retained);
        assert!(!custody.paired_captured);
        assert_eq!(custody.target_close_ack, stage == 31);
        assert_eq!(state, (false, false, false));
        {
            let phase = f.shared.phase.lock().unwrap();
            let Some(Failure::Error(error)) = &phase.failure else { panic!("genuine credential expiry required") };
            let rendered = format!("{error:#}");
            assert!(rendered.contains("401")
                && rendered.contains(playscale_core::access::AccessError::CredentialExpired.code()),
                "expected actual401 credential_expired, not any deadline error: {rendered}");
        }
        assert!(f.service.begin_selected_paired_capture().is_err());
        no_paired_effects(&motion, root.path()).await;
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service116_paired_close_witness_failure_retains_native_without_retry() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let before = native_captured(f, root.path(), &expected, &candidate, "service116-close-witness").await;
    f.service.begin_selected_paired_capture().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_paired_capture().await.unwrap();
    let stable = paired_evidence(f, &before).2;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let original = std::fs::symlink_metadata(&motion).unwrap();
    let identity = (original.dev(), original.ino());
    let renamed = motion.with_extension("paired-close-witness-moved");
    std::fs::rename(&motion, &renamed).unwrap();
    {
        let mut phase = f.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, .. } = &mut *phase;
        let target = target.as_mut().unwrap();
        let error = target.settle_source94(&mut pair.as_mut().unwrap()._writer).await.unwrap_err();
        assert!(error.downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound), "{error:#}");
        let native = target.source94_native_capture_custody();
        assert!(native.writer_handed_back && native.exact_close_ack && !native.writer_held);
        assert!(native.native_retained && native.native_captured && native.histories_retained);
        assert!(!native.release_attempted && !native.released);
        assert!(!target.source94_paired_capture_evidence().2);
    }
    assert_eq!(retained(f, &before).2, stable);
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
    std::fs::rename(&renamed, &motion).unwrap();
    let restored = std::fs::symlink_metadata(&motion).unwrap();
    assert_eq!((restored.dev(), restored.ino()), identity);
    {
        let mut phase = f.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, .. } = &mut *phase;
        let error = target.as_mut().unwrap().settle_source94(&mut pair.as_mut().unwrap()._writer).await.unwrap_err();
        assert!(format!("{error:#}").contains("release is uncertain or already attempted"), "{error:#}");
        assert!(target.as_ref().unwrap().source94_native_capture_custody().native_retained);
    }
    assert!(f.service.begin_selected_paired_capture().is_err());
    no_paired_effects(&motion, root.path()).await;
    f.assert_current();
    // Unsupported whole-service destruction is not successful settlement or a
    // cleanup grant. The terminal exact owner is deliberately not retried.
    drop(fixture);
}
