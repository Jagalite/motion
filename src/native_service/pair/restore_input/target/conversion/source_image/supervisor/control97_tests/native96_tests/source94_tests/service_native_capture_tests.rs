//! Source-only coverage of the genuine service-owned finite Native checkpoint.
//! Authored, not executed. These fixtures do not fabricate backend readiness,
//! image success, close ACK, original deadlines or replacement authorization.
use super::*;
use motion_catalog_backend::RetainedNativeCaptureCustody;

// An assertion failure must release the actual blocking service worker before
// fixture destruction. This is cancellation, not a successful join or ACK.
struct NativeCapturePauseRelease(Arc<Shared>);
impl Drop for NativeCapturePauseRelease {
    fn drop(&mut self) {
        if std::thread::panicking() { self.0.cancel.store(true, Ordering::Release); }
        *self.0.test.source94_released.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.0.test.source94_release.notify_one();
    }
}

async fn prepared(f: &mut Fixture, root: &Path, expected: &LifecycleProof, candidate: &Path, op: &str) -> Custody94 {
    let before = ingress(f, root, expected, candidate, op).await;
    f.service.begin_selected_capture_acquisition().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_capture_acquisition().await.unwrap();
    before
}
fn native_evidence(f: &Fixture, before: &Custody94) -> (RetainedNativeCaptureCustody, (bool, bool, bool), (usize, usize)) {
    let (_, _, stable) = retained(f, before);
    let phase = f.shared.phase.lock().unwrap();
    let target = phase.target.as_ref().unwrap();
    (target.source94_native_capture_custody(), target.source94_native_capture_evidence(), stable)
}

#[tokio::test]
async fn actual_service97_native_capture_is_opaque_and_original_service_owned() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let op = "service97-native-capture";
    let before = prepared(f, root.path(), &expected, &candidate, op).await;
    let stable = retained(f, &before).2;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let _release = NativeCapturePauseRelease(Arc::clone(&f.shared));
    f.shared.test.source94_pause.store(27, Ordering::Release);
    f.service.begin_selected_native_capture().unwrap();
    paused94(f, 27).await;
    // Waiters never own or cancel the actual original service operation.
    drop(f.service.wait_selected_pair_restore());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    release94(f);
    f.service.wait_selected_pair_restore().await.unwrap();
    let (custody, state, after) = native_evidence(f, &before);
    assert_eq!(stable, after);
    assert_eq!(state, (true, true, true));
    let frame = f.shared.test.source94_async_frame.load(Ordering::Acquire);
    assert!(frame > 0 && frame <= crate::native_service::pair::restore_input::target::conversion::source_image::supervisor::target_reservation::ASYNC_FRAME_LIMIT);
    assert!(custody.parts && !custody.inputs && custody.histories_retained);
    assert!(custody.target_connect && custody.target_close && custody.target_close_ack);
    assert!(!custody.target_reader);
    assert!(custody.native_attempted && custody.native_captured && custody.writer_held);
    assert!(!custody.writer_handed_back && !custody.exact_close_ack && !custody.release_attempted && !custody.released);
    {
        let mut phase = f.shared.phase.lock().unwrap();
        assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94NativeCaptured);
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        phase.target.as_mut().unwrap().refuse_source94_native_onward_for_fixture().await;
    }
    assert!(f.service.begin_selected_native_capture().is_err());
    assert!(f.service.begin_selected_capture_acquisition().is_err());
    drop(f.service.settle_selected_native_capture());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    f.service.settle_selected_native_capture().await.unwrap();
    assert!(f.service.restore.worker.is_none() && f.service.restore.selection_timer.is_none());
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes, "immutable capture does not mutate the native source");
    one_effect94(&motion, &native, op).await;
    // Observe the terminal exact-close/release history before whole-service
    // destruction removes the target. No caller extracts the copied image.
    {
        let mut phase = f.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, .. } = &mut *phase;
        let target = target.as_mut().unwrap();
        target.settle_source94(&mut pair.as_mut().unwrap()._writer).await.unwrap();
        let custody = target.source94_native_capture_custody();
        assert!(custody.histories_retained && custody.writer_handed_back);
        assert!(custody.exact_close_ack && custody.release_attempted && custody.released);
        assert!(!custody.writer_held);
    }
    f.close().await;
    one_effect94(&motion, &native, op).await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service97_unpolled_preflight_auth_and_close_keep_original_custody() {
    for injection in [8, 9, 10, 11, 12] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = prepared(f, root.path(), &expected, &candidate, "service97-unpolled").await;
        let stable = retained(f, &before).2;
        f.shared.test.source94_unpolled.store(injection, Ordering::Release);
        f.service.begin_selected_native_capture().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let (custody, state, after) = native_evidence(f, &before);
        assert_eq!(stable, after);
        assert!(custody.histories_retained && custody.writer_held);
        assert_eq!(custody.inputs, injection <= 9);
        assert_eq!(custody.native_attempted, injection == 12);
        assert_eq!(custody.native_captured, injection == 12);
        assert_eq!(custody.target_connect, injection >= 10);
        assert_eq!(custody.target_reader, injection == 10);
        assert_eq!(custody.target_close, injection >= 11);
        assert_eq!(custody.target_close_ack, injection == 12);
        assert_eq!(state, (injection == 12, false, false));
        assert!(!custody.release_attempted && !custody.exact_close_ack);
        assert!(f.service.begin_selected_native_capture().is_err());
        let phase = f.shared.phase.lock().unwrap();
        assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94NativeCapturing);
        drop(phase);
        no_later_effects(root.path());
        // Terminal cleanup is the only remaining action, even for an admission
        // future that was never polled. Do not reconstruct a capture grant.
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service97_cancel_or_wrapper_panic_preserves_captured_output_and_history() {
    for stage in [25, 26, 27, 28] {
        for panic_at_boundary in [false, true] {
            let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
            let f = &mut fixture;
            let root = tempfile::tempdir().unwrap();
            let before = prepared(f, root.path(), &expected, &candidate, "service97-interrupted").await;
            let stable = retained(f, &before).2;
            let _release = NativeCapturePauseRelease(Arc::clone(&f.shared));
            f.shared.test.source94_pause.store(stage, Ordering::Release);
            f.service.begin_selected_native_capture().unwrap();
            paused94(f, stage).await;
            if panic_at_boundary { f.shared.test.source94_panic.store(true, Ordering::Release); }
            else { f.shared.cancel.store(true, Ordering::Release); }
            release94(f);
            assert!(f.service.wait_selected_pair_restore().await.is_err());
            let (custody, state, after) = native_evidence(f, &before);
            assert_eq!(stable, after);
            assert!(custody.parts && custody.writer_held && custody.histories_retained);
            assert_eq!(custody.native_attempted, stage == 28);
            assert_eq!(custody.native_captured, stage == 28);
            // Cancellation at26 permits the actual resource-only target close;
            // the final auth fence then refuses Native. Panic at26 unwinds
            // before constructing that close, so it cannot have its ACK.
            assert_eq!(custody.target_close_ack,
                stage >= 27 || (stage == 26 && !panic_at_boundary));
            assert_eq!(state, (stage == 28, false, false));
            assert!(!custody.release_attempted && !custody.exact_close_ack);
            let phase = f.shared.phase.lock().unwrap();
            assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94NativeCapturing);
            assert!(matches!(&phase.failure, Some(Failure::Panic(_))) == panic_at_boundary);
            drop(phase);
            assert!(f.service.begin_selected_native_capture().is_err());
            no_later_effects(root.path());
            f.close().await;
        }
    }
}

#[tokio::test]
async fn actual_service97_real_credential_expiry_blocks_native_after_preflight_and_target_close() {
    use sqlx::Connection;
    for stage in [25, 27] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_native_capture_expiry_fixture().await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = prepared(f, root.path(), &expected, &candidate, "service97-expiry").await;
        let expiry = f.target_expiry.unwrap();
        let (motion, _) = names(f, root.path());
        let _release = NativeCapturePauseRelease(Arc::clone(&f.shared));
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.service.begin_selected_native_capture().unwrap();
        paused94(f, stage).await;
        // Both cooperative WRITE owners prevent a supported late policy write.
        // Failed contention is not fabricated revocation or fabricated auth.
        for path in [&motion, &f.source_motion_path] {
            let mut competing = sqlx::SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path).create_if_missing(false).busy_timeout(Duration::ZERO)).await.unwrap();
            assert!(sqlx::query("BEGIN IMMEDIATE").execute(&mut competing).await.is_err());
            competing.close().await.unwrap();
        }
        assert!(crate::now() < expiry, "actual credential must reach seam before expiry");
        let native_deadline = f.shared.test.source94_native_deadline.lock().unwrap()
            .expect("actual Native admission deadline must be recorded before the pause");
        let remaining = u64::try_from(expiry - crate::now()).unwrap();
        // Fail setup honestly if the real pipeline timing cannot isolate this
        // cause. Never let Native's tighter60s deadline masquerade as auth denial.
        assert!(Instant::now() + Duration::from_secs(remaining + 1) < native_deadline,
            "genuine credential expiry must strictly precede the actual Native deadline");
        while crate::now() < expiry { tokio::time::sleep(Duration::from_millis(100)).await; }
        assert!(Instant::now() < native_deadline, "Native deadline must remain live at auth denial");
        assert!(!f.shared.cancel.load(Ordering::Acquire), "credential expiry is not original deadline expiry");
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let (custody, state, _) = native_evidence(f, &before);
        assert!(custody.histories_retained && custody.writer_held);
        assert!(!custody.native_attempted && !custody.native_captured);
        assert_eq!(custody.target_close_ack, stage == 27);
        assert_eq!(state, (false, false, false));
        let phase = f.shared.phase.lock().unwrap();
        let Some(Failure::Error(error)) = &phase.failure else { panic!("real credential expiry error required") };
        let rendered = format!("{error:#}");
        assert!(rendered.contains("401")
            && rendered.contains(playscale_core::access::AccessError::CredentialExpired.code()),
            "actual401 credential_expired required, not any deadline/error: {rendered}");
        drop(phase);
        assert!(f.service.begin_selected_native_capture().is_err());
        no_later_effects(root.path());
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service97_witness_failure_after_exact_sqlx_close_retains_native_without_release() {
    for captured in [false, true] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let before = prepared(f, root.path(), &expected, &candidate, "service97-witness-close").await;
        let (motion, native) = names(f, root.path());
        let native_bytes = std::fs::read(&native).unwrap();
        let original = std::fs::symlink_metadata(&motion).unwrap();
        let identity = (original.dev(), original.ino());
        if !captured { f.shared.test.source94_unpolled.store(9, Ordering::Release); }
        f.service.begin_selected_native_capture().unwrap();
        let result = f.service.wait_selected_pair_restore().await;
        assert_eq!(result.is_ok(), captured);
        if captured {
            f.service.settle_selected_native_capture().await.unwrap();
        } else {
            // Join the actual failed work's handles without manufacturing a
            // successful predecessor checkpoint or altering its FAILED status.
            f.service.restore.worker.as_mut().unwrap().await.unwrap();
            f.service.restore.worker = None;
            f.service.restore.selection_timer.as_mut().unwrap().await.unwrap();
            f.service.restore.selection_timer = None;
            assert_eq!(f.shared.status.load(Ordering::Acquire), FAILED);
        }
        let (custody, _, stable) = native_evidence(f, &before);
        assert_eq!(custody.inputs, !captured);
        assert_eq!(custody.native_captured, captured);
        assert!(custody.native_retained && custody.writer_held && custody.histories_retained);
        assert!(!custody.exact_close_ack && !custody.release_attempted && !custody.released);

        // Real filesystem fault on the existing witness, not a fabricated SQLx
        // outcome. The held original inode and writer are not replaced.
        let renamed = motion.with_extension("native-close-witness-moved");
        std::fs::rename(&motion, &renamed).unwrap();
        let moved = std::fs::symlink_metadata(&renamed).unwrap();
        assert_eq!((moved.dev(), moved.ino()), identity);
        {
            let mut phase = f.shared.phase.lock().unwrap();
            let PhaseOwner { target, pair, .. } = &mut *phase;
            let target = target.as_mut().unwrap();
            let pair = pair.as_mut().unwrap();
            pair._stopped.verify_lock().unwrap();
            let error = target.settle_source94(&mut pair._writer).await.unwrap_err();
            assert!(error.downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "missing original witness path must deny Native release: {error:#}");
            let custody = target.source94_native_capture_custody();
            assert!(custody.parts && !custody.inputs && custody.histories_retained);
            assert!(custody.writer_handed_back && !custody.writer_held && custody.exact_close_ack,
                "this case requires the real same-connection SQLx close ACK, not close uncertainty");
            assert!(custody.native_retained);
            assert_eq!(custody.native_captured, captured);
            assert!(!custody.release_attempted && !custody.released);
            assert!(!target.source94_native_capture_evidence().2);
        }
        assert_eq!(native_evidence(f, &before).2, stable);
        assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
        std::fs::rename(&renamed, &motion).unwrap();
        let restored = std::fs::symlink_metadata(&motion).unwrap();
        assert_eq!((restored.dev(), restored.ino()), identity);
        {
            let mut phase = f.shared.phase.lock().unwrap();
            let PhaseOwner { target, pair, .. } = &mut *phase;
            let target = target.as_mut().unwrap();
            // Restoring only the pathname cannot re-arm a terminal release
            // attempt or recreate operational source/image authority.
            let error = target.settle_source94(&mut pair.as_mut().unwrap()._writer).await.unwrap_err();
            assert!(format!("{error:#}").contains("release is uncertain or already attempted"), "{error:#}");
            let custody = target.source94_native_capture_custody();
            assert!(custody.histories_retained && custody.native_retained && custody.exact_close_ack);
            assert_eq!(custody.native_captured, captured);
            assert!(!custody.release_attempted && !custody.released);
        }
        assert!(f.service.begin_selected_native_capture().is_err());
        f.assert_current();
        no_later_effects(root.path());
        // Intentionally no f.close().await.unwrap(): witness failure is terminal
        // retained custody. Fixture destruction is unsupported owner teardown,
        // not positive cleanup or permission to release through a retry.
        drop(fixture);
    }
}

#[path = "service_paired_capture_tests.rs"]
mod service_paired_capture_tests;
