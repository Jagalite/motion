//! Authored source-only service90 tests. Prerequisites are the real physical
//! source97/96/94 route, not a forged checkpoint; they are not yet qualified by
//! passing runtime evidence. No owner extraction or mutable capture accessor.
use super::*;

async fn positive_input(f: &mut Fixture, root: &Path, expected: &LifecycleProof, candidate: &Path, operation: &str) {
    acknowledge94(f, root, expected, candidate, operation).await;
    f.service.settle_selected_relocation94_for_capture_input().await.unwrap();
    f.service.begin_selected_source94_capture_input().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
}
fn retained_ingress(f: &Fixture, before: &Custody94, reconciled: bool) -> (usize, usize) {
    let phase = f.shared.phase.lock().unwrap();
    let target = phase.target.as_ref().unwrap();
    let pair = phase.pair.as_ref().unwrap();
    let evidence = target.source94_service_ingress_evidence(&pair._stopped).unwrap().unwrap();
    assert_eq!(evidence.0, before.deadline);
    assert!(evidence.3, "original wrapper phase is preserved, not fabricated");
    assert_eq!(evidence.4, reconciled, "original reconciliation history must remain intact");
    assert!(!evidence.6, "moves-only service ingress never prepares a writer");
    assert!(target.source94_evidence().is_none(), "whole94 moved inside existing capture owner");
    assert!(phase.workspace.is_none() && phase.image.is_none());
    assert_eq!(pair.motion.as_raw_fd(), f.pair_fd);
    assert!(!pair._writer.authorization_claimed(), "handoff is not fresh authorization");
    pair._stopped.verify_lock().unwrap();
    let selected = phase.selection.as_ref().unwrap().evidence_for_fixture();
    assert!(selected.1);
    let (file, digest) = selected.0.unwrap();
    assert_eq!(file.as_raw_fd(), before.selected_fd);
    assert_eq!(file_identity(file.as_raw_fd()), before.selected_identity);
    assert_eq!(digest, f.archive_hash);
    let ancestry = target.staging_ancestry_evidence();
    assert_eq!((ancestry.1, ancestry.2, ancestry.3, ancestry.4),
        (before.ancestry.1, before.ancestry.2, before.ancestry.3, before.ancestry.4));
    assert_eq!(f.shared.authorization_storage.load(Ordering::Acquire), before.authorization_storage);
    assert_eq!(f.shared.selection_storage.load(Ordering::Acquire), before.selection_storage);
    assert_eq!(std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap(), before.pair_bytes);
    let lock = File::open(&before.target_lock_path).unwrap();
    assert_eq!(file_identity(lock.as_raw_fd()), before.target_lock_identity);
    assert!(fs2::FileExt::try_lock_exclusive(&lock).is_err());
    (evidence.1, evidence.2)
}

#[tokio::test]
async fn actual_service90_joined_moves_only_adoption_and_dropped_cleanup_waiter_keep_stable_custody() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service90-retained-ingress";
    positive_input(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let before = custody94(f, root.path());
    let caller = f.shared.phase.lock().unwrap().target.as_ref().unwrap().source94_ingress_fixture_caller_address();
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    assert!(f.service.retain_selected_source94_capture_ingress().is_err());
    drop(f.service.settle_selected_capture_input_for_ingress());
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    assert!(f.service.retain_selected_source94_capture_ingress().is_err());
    same_custody94(f, &before, true);
    f.service.settle_selected_capture_input_for_ingress().await.unwrap();
    f.service.retain_selected_source94_capture_ingress().unwrap();
    assert!(f.service.restore.worker.is_none() && f.service.restore.selection_timer.is_none());
    assert_eq!(f.shared.status.load(Ordering::Acquire), COMPLETE);
    let stable = retained_ingress(f, &before, false);
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94CaptureIngressRetained);
        assert_eq!(target.source94_ingress_fixture_caller_address(), caller);
        assert_eq!(phase.selection.as_ref().unwrap().target_deadline(&f.shared.cancel).unwrap(), before.deadline);
    }
    drop(f.service.wait_selected_pair_restore());
    drop(f.service.close_restore());
    assert_eq!(retained_ingress(f, &before, false), stable, "post-install addresses remain stable");
    // Pause the real independently service-owned cleanup worker. Poll and drop
    // only its waiter, then permit the same worker to finish. No inline task owns
    // the capture operation and no synthetic close ACK is injected.
    f.shared.test.source94_pause.store(21, Ordering::Release);
    {
        let mut waiter = std::pin::pin!(f.service.close_restore());
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }).await;
    }
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.test.source94_reached.load(Ordering::Acquire) == 21 { break; }
        changed.await;
    }
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.close_started);
    release94(f);
    f.close().await;
    assert!(f.shared.phase.lock().unwrap().target.is_none(), "successful whole-service teardown releases the terminal target only after exact cleanup");
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
    one_effect94(&motion, &native, operation).await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service90_missing_admission_uses_production_return_path_and_keeps_original_wrapper() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    positive_input(f, root.path(), &expected, &candidate, "service90-missing-admission").await;
    f.service.settle_selected_capture_input_for_ingress().await.unwrap();
    let before = custody94(f, root.path());
    {
        let mut phase = f.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, selection, .. } = &mut *phase;
        assert!(target.as_mut().unwrap().missing_service_ingress_admission_for_fixture(
            pair.as_mut().unwrap(), selection.as_mut().unwrap(), &f.shared).is_err());
    }
    same_custody94(f, &before, true);
    assert_eq!(f.shared.phase.lock().unwrap().target.as_ref().unwrap().phase, TargetPhase::Source94CaptureInputReady);
    // Refused moves-only bridge did not fabricate admission or discard history.
    f.service.retain_selected_source94_capture_ingress().unwrap();
    retained_ingress(f, &before, false);
    f.close().await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service90_cancel_or_post_install_panic_keeps_exact_owned_failure() {
    for panic_after_move in [false, true] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let operation = "service90-failed-handoff";
        positive_input(f, root.path(), &expected, &candidate, operation).await;
        f.service.settle_selected_capture_input_for_ingress().await.unwrap();
        let before = custody94(f, root.path());
        if panic_after_move {
            f.shared.test.source94_pause.store(20, Ordering::Release);
            *f.shared.test.source94_released.lock().unwrap() = true;
            f.shared.test.source94_panic.store(true, Ordering::Release);
        } else {
            f.shared.cancel.store(true, Ordering::Release);
        }
        assert!(f.service.retain_selected_source94_capture_ingress().is_err());
        assert_eq!(f.shared.status.load(Ordering::Acquire), FAILED);
        assert!(f.service.retain_selected_source94_capture_ingress().is_err());
        if panic_after_move {
            retained_ingress(f, &before, false);
            let phase = f.shared.phase.lock().unwrap();
            assert!(matches!(phase.failure, Some(Failure::Panic(_))));
            assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Source94CaptureIngress);
        } else {
            same_custody94(f, &before, false);
            assert!(matches!(f.shared.phase.lock().unwrap().failure, Some(Failure::Error(_))));
        }
        f.close().await;
        no_later_effects(root.path());
    }
}

#[tokio::test]
async fn actual_service90_failed_target_close_worker_never_grants_ingress() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    acknowledge94(f, root.path(), &expected, &candidate, "service90-predecessor-failure").await;
    let before = custody94(f, root.path());
    f.service.settle_selected_relocation94_for_capture_input().await.unwrap();
    f.shared.test.source94_pause.store(19, Ordering::Release);
    f.service.begin_selected_source94_capture_input().unwrap();
    paused94(f, 19).await;
    f.shared.test.source94_panic.store(true, Ordering::Release);
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert!(f.service.settle_selected_capture_input_for_ingress().await.is_err());
    assert!(f.service.retain_selected_source94_capture_ingress().is_err());
    same_custody94(f, &before, true);
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert!(target.source94_service_ingress_evidence(&phase.pair.as_ref().unwrap()._stopped).unwrap().is_none());
        assert_eq!(target.phase, TargetPhase::Source94ClosingTarget);
        assert!(matches!(phase.failure, Some(Failure::Panic(_))));
    }
    f.close().await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service90_later_cleanup_failure_does_not_claim_service_closed_or_discard_ingress() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    positive_input(f, root.path(), &expected, &candidate, "service90-close-failure").await;
    f.service.settle_selected_capture_input_for_ingress().await.unwrap();
    let before = custody94(f, root.path());
    f.service.retain_selected_source94_capture_ingress().unwrap();
    let stable = retained_ingress(f, &before, false);
    // Failure is AFTER exact ingress cleanup, before current-pair writer close.
    // It must not become CLOSED or release the still-owned service/pair/locks.
    f.shared.test.selection_close_failure.store(true, Ordering::Release);
    assert!(f.service.close_restore().await.is_err());
    assert_eq!(f.shared.status.load(Ordering::Acquire), FAILED);
    assert_eq!(retained_ingress(f, &before, false), stable);
    {
        let phase = f.shared.phase.lock().unwrap();
        assert!(phase.close_failure.is_some());
        assert_eq!(phase.target.as_ref().unwrap().source94_service_ingress_closed(), Some(true));
        assert!(phase.pair.is_some() && phase.selection.is_some());
    }
    assert!(f.shared.stopped.lock().unwrap().is_some());
    assert!(f.service.retain_selected_source94_capture_ingress().is_err());
    assert!(f.service.close_restore().await.is_err());
    no_later_effects(root.path());
    // This intentionally ends at retained terminal failure, not successful
    // service teardown. Full service Drop remains a separate known limitation.
}


#[tokio::test]
async fn actual_service90_genuine_receipt_reconciliation_history_survives_moves_only_adoption() {
    let Physical94 { mut fixture, expected, old, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service90-reconciled-history";
    ready94(f, root.path()).await;
    f.service.begin_selected_relocation94_to_receipt(request94(operation, &expected, &candidate)).unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_relocation94_for_reconcile().await.unwrap();
    for path in [&old, &candidate] {
        std::fs::remove_file(path.join("one.txt")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
    f.service.begin_selected_reconcile94_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_relocation94_for_capture_input().await.unwrap();
    f.service.begin_selected_source94_capture_input().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    let before = custody94(f, root.path());
    assert!(f.shared.phase.lock().unwrap().target.as_ref().unwrap().source94_evidence().unwrap().2);
    f.service.settle_selected_capture_input_for_ingress().await.unwrap();
    f.service.retain_selected_source94_capture_ingress().unwrap();
    retained_ingress(f, &before, true);
    let (motion, native) = names(f, root.path());
    one_effect94(&motion, &native, operation).await;
    assert!(!old.exists() && !candidate.exists());
    f.close().await;
    one_effect94(&motion, &native, operation).await;
    no_later_effects(root.path());
}
