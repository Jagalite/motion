//! Genuine service94 positive target-close coverage, authored source-only.
//! These cases retain the original service continuation and never extract a
//! source, fabricate a receipt/ACK, acquire a capture writer, or execute95.
//! A clean receipt-only reconciliation is not saved-ACK or outcome=None replay.
use super::*;
use motion_catalog_backend::SourceRelocation94Phase as Backend94;

struct Custody94 {
    deadline: Instant,
    selected_fd: i32,
    selected_identity: (u64, u64),
    target_lock_path: PathBuf,
    target_lock_identity: (u64, u64),
    pair_bytes: Vec<u8>,
    authorization_storage: u64,
    selection_storage: u64,
    ancestry: (usize, usize, usize, usize, usize),
    owner_workspace: (usize, usize),
    successor: LifecycleProof,
    successor_address: usize,
}

fn custody94(f: &Fixture, root: &Path) -> Custody94 {
    let phase = f.shared.phase.lock().unwrap();
    let selection = phase.selection.as_ref().unwrap();
    let selected_fd = selection
        .evidence_for_fixture()
        .0
        .unwrap()
        .0
        .as_raw_fd();
    let target = phase.target.as_ref().unwrap();
    let successor = target.source94_successor().unwrap();
    let target_lock_path = root.join("server.lock");
    let target_lock = File::open(&target_lock_path).unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&target_lock).is_err());
    Custody94 {
        deadline: selection.target_deadline(&f.shared.cancel).unwrap(),
        selected_fd,
        selected_identity: file_identity(selected_fd),
        target_lock_path,
        target_lock_identity: file_identity(target_lock.as_raw_fd()),
        pair_bytes: std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap(),
        authorization_storage: f.shared.authorization_storage.load(Ordering::Acquire),
        selection_storage: f.shared.selection_storage.load(Ordering::Acquire),
        ancestry: target.staging_ancestry_evidence(),
        owner_workspace: target.source94_custody_evidence().unwrap(),
        successor: successor.clone(),
        successor_address: successor as *const LifecycleProof as usize,
    }
}

fn same_custody94(f: &Fixture, before: &Custody94, live: bool) {
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::ReturnedOk,
    );
    {
        let phase = f.shared.phase.lock().unwrap();
        let selection = phase.selection.as_ref().unwrap();
        let target = phase.target.as_ref().unwrap();
        let successor = target.source94_successor().unwrap();
        assert_eq!(successor, &before.successor);
        assert_eq!(
            successor as *const LifecycleProof as usize,
            before.successor_address,
            "the original proof remains in the retained94 graph"
        );
        assert_eq!(
            target.source94_custody_evidence().unwrap(),
            before.owner_workspace,
            "neither the whole94 owner nor its original workspace is replaced"
        );
        assert!(phase.workspace.is_none());
        assert!(phase.image.is_none(), "closing94 is not image capture");
        assert_eq!(phase.pair.as_ref().unwrap().motion.as_raw_fd(), f.pair_fd);
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        let (selected, bound) = selection.evidence_for_fixture();
        let (selected, digest) = selected.unwrap();
        assert!(bound);
        assert_eq!(selected.as_raw_fd(), before.selected_fd);
        assert_eq!(file_identity(selected.as_raw_fd()), before.selected_identity);
        assert_eq!(digest, f.archive_hash);
        if live {
            assert_eq!(
                selection.target_deadline(&f.shared.cancel).unwrap(),
                before.deadline,
                "target closure never supplies a fresh time budget"
            );
        } else {
            assert!(selection.target_deadline(&f.shared.cancel).is_err());
        }
        let after = target.staging_ancestry_evidence();
        assert_eq!(
            (after.1, after.2, after.3, after.4),
            (
                before.ancestry.1,
                before.ancestry.2,
                before.ancestry.3,
                before.ancestry.4,
            ),
            "the original retained ancestry lengths and capacities remain"
        );
        assert!(after.0 <= before.ancestry.0, "ancestry budget cannot reset");
    }
    assert_eq!(
        f.shared.authorization_storage.load(Ordering::Acquire),
        before.authorization_storage
    );
    assert_eq!(
        f.shared.selection_storage.load(Ordering::Acquire),
        before.selection_storage
    );
    assert_eq!(
        std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap(),
        before.pair_bytes
    );
    let target_lock = File::open(&before.target_lock_path).unwrap();
    assert_eq!(file_identity(target_lock.as_raw_fd()), before.target_lock_identity);
    assert!(
        fs2::FileExt::try_lock_exclusive(&target_lock).is_err(),
        "the original target lock remains exclusively retained"
    );
    f.assert_current();
}

async fn acknowledge94(
    f: &mut Fixture,
    root: &Path,
    expected: &LifecycleProof,
    candidate: &Path,
    operation: &str,
) {
    ready94(f, root).await;
    let (motion, _) = names(f, root);
    assert_original_target_cas(&motion, expected).await;
    f.service
        .begin_selected_relocation94_restore(request94(operation, expected, candidate))
        .unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::ReturnedOk,
    );
}

async fn one_effect94(motion: &Path, native: &Path, operation: &str) {
    assert_durable_rows(motion, 1, 1).await;
    assert_eq!(native_effects(native).await, 1);
    assert_eq!(native_receipts(native, operation).await, 1);
}

fn no_capture_retry94(f: &mut Fixture, expected: &LifecycleProof, candidate: &Path) {
    assert!(f.service.begin_selected_source94_capture_input().is_err());
    assert!(f.service.begin_selected_reconcile94_restore().is_err());
    assert!(
        f.service
            .begin_selected_relocation94_restore(request94(
                "positive-close-cannot-replace",
                expected,
                candidate,
            ))
            .is_err()
    );
}

#[tokio::test]
async fn actual_service94_positive_target_close_retains_whole_owner_and_exact_predecessors() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-positive-close";
    acknowledge94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let before = custody94(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.selection_timer.is_some());
    assert!(
        f.service.begin_selected_source94_capture_input().is_err(),
        "success status cannot substitute for exact worker and timer joins"
    );
    drop(f.service.settle_selected_relocation94_for_capture_input());
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.selection_timer.is_some());
    assert!(f.service.begin_selected_source94_capture_input().is_err());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    assert!(f.service.restore.worker.is_none());
    assert!(f.service.restore.selection_timer.is_none());
    same_custody94(f, &before, true);
    f.service.begin_selected_source94_capture_input().unwrap();
    drop(f.service.wait_selected_pair_restore());
    f.service.wait_selected_pair_restore().await.unwrap();
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94CaptureInputReady);
        assert_eq!(
            target.source94_evidence().unwrap(),
            (Backend94::CaptureInputReady, true, false, false)
        );
        assert_eq!(target.source94_capture_input_evidence(), Some((true, true)));
        assert!(phase.failure.is_none());
    }
    same_custody94(f, &before, true);
    one_effect94(&motion, &native, operation).await;
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
    no_capture_retry94(f, &expected, &candidate);
    no_later_effects(root.path());
    f.close().await;
    one_effect94(&motion, &native, operation).await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service94_receipt_checkpoint_must_reconcile_before_positive_target_close() {
    let Physical94 {
        mut fixture,
        expected,
        old,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    ready94(f, root.path()).await;
    let (motion, native) = names(f, root.path());
    let operation = "service94-positive-close-reconcile";
    f.service
        .begin_selected_relocation94_to_receipt(request94(operation, &expected, &candidate))
        .unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::NotAttempted,
    );
    assert!(
        f.service
            .settle_selected_relocation94_for_capture_input()
            .await
            .is_err(),
        "a genuine native receipt is not the complete94 ACK checkpoint"
    );
    assert!(f.service.begin_selected_source94_capture_input().is_err());
    f.service
        .settle_selected_relocation94_for_reconcile()
        .await
        .unwrap();
    assert!(f.service.begin_selected_source94_capture_input().is_err());
    for path in [&old, &candidate] {
        std::fs::remove_file(path.join("one.txt")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
    f.service.begin_selected_reconcile94_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    let before = custody94(f, root.path());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    f.service.begin_selected_source94_capture_input().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94CaptureInputReady);
        assert_eq!(
            target.source94_evidence().unwrap(),
            (Backend94::CaptureInputReady, true, true, false)
        );
        assert_eq!(target.source94_capture_input_evidence(), Some((true, true)));
    }
    same_custody94(f, &before, true);
    one_effect94(&motion, &native, operation).await;
    assert!(!old.exists() && !candidate.exists());
    no_capture_retry94(f, &expected, &candidate);
    no_later_effects(root.path());
    f.close().await;
    one_effect94(&motion, &native, operation).await;
}

#[tokio::test]
async fn actual_service94_dropped_request_waiter_cancels_without_taking_close_custody() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-positive-close-waiter";
    acknowledge94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let before = custody94(f, root.path());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    f.shared.test.source94_pause.store(18, Ordering::Release);
    f.service.begin_selected_source94_capture_input().unwrap();
    paused94(f, 18).await;
    {
        let mut waiter = std::pin::pin!(f.service.wait_selected_pair_restore());
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert!(f.shared.cancel.load(Ordering::Acquire));
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.selection_timer.is_some());
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94ClosingTarget);
        assert_eq!(target.source94_capture_input_evidence(), Some((false, false)));
        assert!(phase.failure.is_some());
    }
    same_custody94(f, &before, false);
    one_effect94(&motion, &native, operation).await;
    no_capture_retry94(f, &expected, &candidate);
    f.close().await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service94_unpolled_target_close_claim_cannot_be_reused() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-positive-close-unpolled";
    acknowledge94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let before = custody94(f, root.path());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    f.shared.test.source94_unpolled.store(4, Ordering::Release);
    f.service.begin_selected_source94_capture_input().unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94ClosingTarget);
        assert_eq!(
            target.source94_evidence().unwrap(),
            (Backend94::ClosingTargetForCapture, true, false, false)
        );
        assert_eq!(target.source94_capture_input_evidence(), Some((false, false)));
        assert!(phase.failure.is_some());
    }
    same_custody94(f, &before, true);
    one_effect94(&motion, &native, operation).await;
    no_capture_retry94(f, &expected, &candidate);
    f.close().await;
    one_effect94(&motion, &native, operation).await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service94_cancel_and_panic_on_both_sides_of_positive_close_are_terminal_only() {
    for (stage, panic) in [(18, false), (18, true), (19, false), (19, true)] {
        let Physical94 {
            mut fixture,
            expected,
            candidate,
            ..
        } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let operation = "service94-positive-close-interrupted";
        acknowledge94(f, root.path(), &expected, &candidate, operation).await;
        let (motion, native) = names(f, root.path());
        let before = custody94(f, root.path());
        f.service
            .settle_selected_relocation94_for_capture_input()
            .await
            .unwrap();
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.service.begin_selected_source94_capture_input().unwrap();
        paused94(f, stage).await;
        if panic {
            f.shared.test.source94_panic.store(true, Ordering::Release);
        } else {
            f.shared.cancel.store(true, Ordering::Release);
        }
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Source94ClosingTarget);
            assert_eq!(
                target.source94_capture_input_evidence(),
                Some((false, stage == 19)),
                "a historical successful SQLite close is not service checkpoint authority"
            );
            assert_eq!(
                target.source94_evidence().unwrap(),
                (
                    if stage == 19 {
                        Backend94::CaptureInputReady
                    } else {
                        Backend94::Ready94
                    },
                    true,
                    false,
                    false,
                )
            );
            if panic {
                assert!(matches!(phase.failure, Some(Failure::Panic(_))));
            } else {
                assert!(matches!(phase.failure, Some(Failure::Error(_))));
            }
        }
        same_custody94(f, &before, panic);
        one_effect94(&motion, &native, operation).await;
        no_capture_retry94(f, &expected, &candidate);
        f.close().await;
        one_effect94(&motion, &native, operation).await;
        no_later_effects(root.path());
    }
}

#[tokio::test]
async fn actual_service94_original_deadline_expiring_after_close_cannot_publish_checkpoint() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-positive-close-expiry";
    acknowledge94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let before = custody94(f, root.path());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    f.shared.test.source94_pause.store(19, Ordering::Release);
    f.service.begin_selected_source94_capture_input().unwrap();
    paused94(f, 19).await;
    assert!(Instant::now() < before.deadline);
    // Deliberately wait for the genuine original120-second selection deadline.
    // No reset, synthetic clock, replacement cancellation flag, or edited phase.
    tokio::time::sleep_until(tokio::time::Instant::from_std(before.deadline)).await;
    while Instant::now() < before.deadline {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94ClosingTarget);
        assert_eq!(target.source94_capture_input_evidence(), Some((false, true)));
        assert_eq!(target.source94_evidence().unwrap().0, Backend94::CaptureInputReady);
        assert!(phase.failure.is_some());
    }
    same_custody94(f, &before, false);
    one_effect94(&motion, &native, operation).await;
    no_capture_retry94(f, &expected, &candidate);
    f.close().await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service94_post_close_stage_identity_change_cannot_publish_checkpoint() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-positive-close-identity";
    acknowledge94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let before = custody94(f, root.path());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    f.shared.test.source94_pause.store(19, Ordering::Release);
    f.service.begin_selected_source94_capture_input().unwrap();
    paused94(f, 19).await;
    let moved = root.path().join("original-closed94-motion");
    std::fs::rename(&motion, &moved).unwrap();
    std::fs::copy(&moved, &motion).unwrap();
    {
        use std::os::unix::fs::MetadataExt;
        let original = std::fs::metadata(&moved).unwrap();
        let replacement = std::fs::metadata(&motion).unwrap();
        assert_ne!(
            (original.dev(), original.ino()),
            (replacement.dev(), replacement.ino())
        );
    }
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94ClosingTarget);
        assert!(target.source94_capture_input_evidence().unwrap().1);
        assert!(phase.failure.is_some());
    }
    same_custody94(f, &before, true);
    one_effect94(&motion, &native, operation).await;
    no_capture_retry94(f, &expected, &candidate);
    // Restore only the fixture pathname for terminal witness cleanup. The
    // failed one-shot service must not turn back into a checkpoint afterward.
    std::fs::remove_file(&motion).unwrap();
    std::fs::rename(&moved, &motion).unwrap();
    assert!(f.service.begin_selected_source94_capture_input().is_err());
    f.close().await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service94_terminal_source_close_with_reusable_workspace_is_not_capture_input() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-terminal-not-checkpoint";
    acknowledge94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let before = custody94(f, root.path());
    f.service
        .settle_selected_relocation94_for_capture_input()
        .await
        .unwrap();
    {
        let mut phase = f.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, .. } = &mut *phase;
        let target = target.as_mut().unwrap();
        // Exercise actual terminal cleanup, not a fabricated Settling/Closed
        // phase. An already reusable workspace legitimately remains reusable.
        target
            .settle_source94(&mut pair.as_mut().unwrap()._writer)
            .await
            .unwrap();
        assert_eq!(
            target.source94_evidence().unwrap(),
            (Backend94::Closed, true, false, true)
        );
        assert_eq!(target.source94_custody_evidence().unwrap(), before.owner_workspace);
        assert_eq!(target.source94_capture_input_evidence(), Some((false, false)));
        assert_eq!(target.phase, TargetPhase::Source94Ready);
    }
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::ReturnedOk,
    );
    assert!(
        f.service.begin_selected_source94_capture_input().is_err(),
        "resource closure plus reusable workspace cannot revive the source"
    );
    no_capture_retry94(f, &expected, &candidate);
    one_effect94(&motion, &native, operation).await;
    f.assert_current();
    f.close().await;
    no_later_effects(root.path());
}

// Separate backend ingress integration. These child cases explicitly detach
// the genuine whole owner and do not establish production service custody.
#[cfg(test)]
#[path = "capture_ingress_tests.rs"]
mod capture_ingress_tests;

// Separate production service moves-only adoption. No detached backend owner.
#[cfg(test)]
#[path = "service_capture_ingress_tests.rs"]
mod service_capture_ingress_tests;

// Actual authorization-to-acquisition service path, still no native capture.
#[cfg(test)]
#[path = "service_capture_acquisition_tests.rs"]
mod service_capture_acquisition_tests;
