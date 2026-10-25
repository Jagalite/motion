//! SOURCE-ONLY genuine public backend ingress integration, not executed.
//! Physical source -> real service -> retained97/96 -> genuine whole94 ->
//! actual target close -> cfg(test) whole-owner detachment -> public backend
//! acquire/close. The harness keeps the original workspace beside whole94. Caller,
//! selection, pair, original writer and stopped-service locks remain in the
//! actual fixture. Extraction/reinsertion DOES NOT establish service custody,
//! a production capture continuation, fresh authorization, native capture or95.
use super::*;
use crate::native_service::pair::restore_input::target::conversion::source_conversion::retained_source_relocation::CaptureIngressFixture94;

async fn genuine_capture_input94(
    f: &mut Fixture, root: &Path, expected: &LifecycleProof, candidate: &Path, operation: &str,
) {
    acknowledge94(f, root, expected, candidate, operation).await;
    f.service.settle_selected_relocation94_for_capture_input().await.unwrap();
    f.service.begin_selected_source94_capture_input().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    // Join the ACTUAL just-completed target-close worker and timer. Success
    // status alone is not permission to detach an owner still borrowed by them.
    f.service.restore.settle_source94_predecessor(TargetPhase::Source94CaptureInputReady)
        .await.unwrap();
    assert!(f.service.restore.worker.is_none());
    assert!(f.service.restore.selection_timer.is_none());
    let phase = f.shared.phase.lock().unwrap();
    assert!(phase.failure.is_none());
    let target = phase.target.as_ref().unwrap();
    assert_eq!(target.phase, TargetPhase::Source94CaptureInputReady);
    assert_eq!(target.source94_capture_input_evidence(), Some((true, true)));
    assert_eq!(target.source94_evidence().unwrap(), (Backend94::CaptureInputReady, true, false, false));
}
fn admit(f: &Fixture, cancel: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let mut phase = f.shared.phase.lock().unwrap();
    let PhaseOwner { target, pair, selection, .. } = &mut *phase;
    target.as_mut().unwrap().admit_source94_ingress_for_fixture(
        pair.as_mut().unwrap(), selection.as_mut().unwrap(), &f.shared, cancel,
    )
}
fn detach(f: &Fixture) -> anyhow::Result<CaptureIngressFixture94> {
    assert!(f.service.restore.worker.is_none());
    assert!(f.service.restore.selection_timer.is_none());
    let mut phase = f.shared.phase.lock().unwrap();
    let PhaseOwner { target, pair, selection, .. } = &mut *phase;
    target.as_mut().unwrap().begin_source94_ingress_for_fixture(
        pair.as_mut().unwrap(), selection.as_mut().unwrap(), &f.shared,
    )
}
fn caller_address(f: &Fixture) -> usize {
    f.shared.phase.lock().unwrap().target.as_ref().unwrap()
        .source94_ingress_fixture_caller_address()
}
fn external_custody(f: &Fixture, before: &Custody94, caller: usize, capture: &CaptureIngressFixture94) {
    let phase = f.shared.phase.lock().unwrap();
    let target = phase.target.as_ref().unwrap();
    assert_eq!(target.phase, TargetPhase::Source94CaptureInputReady,
        "test detachment must not invent a production service phase");
    assert!(target.source94_evidence().is_none(), "whole94 is explicitly detached, not service-adopted");
    assert_eq!(target.source94_ingress_fixture_caller_address(), caller);
    let pair = phase.pair.as_ref().unwrap();
    assert_eq!(pair.motion.as_raw_fd(), f.pair_fd);
    assert!(!pair._writer.authorization_claimed(), "backend ingress obtains no fresh source authentication");
    pair._stopped.verify_lock().unwrap();
    capture.verify_workspace(&pair._stopped).unwrap();
    let selection = phase.selection.as_ref().unwrap();
    let (selected, bound) = selection.evidence_for_fixture();
    let (selected, digest) = selected.unwrap();
    assert!(bound);
    assert_eq!(selected.as_raw_fd(), before.selected_fd);
    assert_eq!(file_identity(selected.as_raw_fd()), before.selected_identity);
    assert_eq!(digest, f.archive_hash);
    assert_eq!(selection.target_deadline(&f.shared.cancel).unwrap(), before.deadline);
    assert!(phase.workspace.is_none() && phase.image.is_none());
    let ancestry = target.staging_ancestry_evidence();
    assert!(ancestry.0 <= before.ancestry.0);
    assert_eq!((ancestry.1, ancestry.2, ancestry.3, ancestry.4),
        (before.ancestry.1, before.ancestry.2, before.ancestry.3, before.ancestry.4));
    drop(phase);
    assert_eq!(f.shared.authorization_storage.load(Ordering::Acquire), before.authorization_storage);
    assert_eq!(f.shared.selection_storage.load(Ordering::Acquire), before.selection_storage);
    assert_eq!(std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap(), before.pair_bytes);
    let target_lock = File::open(&before.target_lock_path).unwrap();
    assert_eq!(file_identity(target_lock.as_raw_fd()), before.target_lock_identity);
    assert!(fs2::FileExt::try_lock_exclusive(&target_lock).is_err());
    f.assert_current();
}
async fn no_capture_grant(capture: &mut CaptureIngressFixture94, cancel: &AtomicBool) {
    assert!(capture.capture_mut().writer_in().await.is_err());
    assert!(capture.capture_mut().capture(cancel).await.is_err());
    assert!(capture.capture_mut().verify(cancel).await.is_err());
}

#[tokio::test]
async fn genuine_whole94_public_capture_ingress_acquires_and_closes_without_capture_authority() {
    let Physical94 { mut fixture, expected, candidate, requester: _requester, reserve_admin: _reserve_admin, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-ingress-positive";
    genuine_capture_input94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let before = custody94(f, root.path());
    let caller = caller_address(f);
    admit(f, &f.shared.cancel).unwrap();
    same_custody94(f, &before, true);
    let mut capture = detach(f).unwrap();
    let workspace = capture.workspace_address();
    assert_eq!(capture.capture_mut().deadline(), before.deadline);
    capture.capture_mut().prepare_retained_input().await.unwrap();
    assert!(capture.capture_mut().retained_input_prepared());
    assert_eq!(capture.workspace_address(), workspace);
    external_custody(f, &before, caller, &capture);
    no_capture_grant(&mut capture, &f.shared.cancel).await;
    capture = capture.assert_finish_refused();
    capture.capture_mut().close().await.unwrap();
    assert!(!capture.capture_mut().retained_input_prepared());
    assert!(capture.capture_mut().prepare_retained_input().await.is_err());
    capture.capture_mut().close().await.unwrap();
    external_custody(f, &before, caller, &capture);
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
    one_effect94(&motion, &native, operation).await;
    no_later_effects(root.path());
    // Backend terminal close has completed before ordinary service cleanup.
    // This separate ownership sequence is not a service continuation claim.
    f.close().await;
    no_later_effects(root.path());
    drop(capture);
}

#[tokio::test]
async fn genuine_whole94_missing_admission_and_wrong_cancel_keep_original_proof_and_owner() {
    let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    let operation = "service94-ingress-refused";
    genuine_capture_input94(f, root.path(), &expected, &candidate, operation).await;
    let (motion, native) = names(f, root.path());
    let native_bytes = std::fs::read(&native).unwrap();
    let before = custody94(f, root.path());
    // Missing admission round-trips the whole original wrapper into its exact
    // slot. Its proof address and existing retained evidence must not change.
    assert!(detach(f).is_err());
    same_custody94(f, &before, true);
    let wrong_cancel = Arc::new(AtomicBool::new(false));
    assert!(!Arc::ptr_eq(&wrong_cancel, &f.shared.cancel));
    assert!(admit(f, &wrong_cancel).is_err());
    same_custody94(f, &before, true);
    // Original cancellation is monotonic: never clear it to manufacture reuse.
    f.shared.cancel.store(true, Ordering::Release);
    assert!(admit(f, &f.shared.cancel).is_err());
    same_custody94(f, &before, false);
    assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
    one_effect94(&motion, &native, operation).await;
    no_later_effects(root.path());
    f.close().await;
}

#[tokio::test]
async fn genuine_whole94_unpolled_prepare_and_close_keep_terminal_backend_custody() {
    // Two distinct public-API abandonment boundaries, not an injected future.
    for abandon_prepare in [true, false] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        let operation = if abandon_prepare { "service94-ingress-unpolled-prepare" } else { "service94-ingress-unpolled-close" };
        genuine_capture_input94(f, root.path(), &expected, &candidate, operation).await;
        let (motion, native) = names(f, root.path());
        let native_bytes = std::fs::read(&native).unwrap();
        let before = custody94(f, root.path());
        let caller = caller_address(f);
        admit(f, &f.shared.cancel).unwrap();
        same_custody94(f, &before, true);
        let mut capture = detach(f).unwrap();
        let workspace = capture.workspace_address();
        if abandon_prepare {
            // Constructor installs the actual acquire frame synchronously;
            // dropping this borrower must not launch it or restore readiness.
            drop(capture.capture_mut().prepare_retained_input());
        } else {
            // Close is synchronously terminal even when never first-polled.
            drop(capture.capture_mut().close());
        }
        assert!(!capture.capture_mut().retained_input_prepared());
        assert!(capture.capture_mut().prepare_retained_input().await.is_err());
        assert_eq!(capture.capture_mut().deadline(), before.deadline);
        assert_eq!(capture.workspace_address(), workspace);
        external_custody(f, &before, caller, &capture);
        no_capture_grant(&mut capture, &f.shared.cancel).await;
        capture.capture_mut().close().await.unwrap();
        assert!(!capture.capture_mut().retained_input_prepared());
        external_custody(f, &before, caller, &capture);
        assert_eq!(std::fs::read(&native).unwrap(), native_bytes);
        one_effect94(&motion, &native, operation).await;
        no_later_effects(root.path());
        f.close().await;
        no_later_effects(root.path());
        drop(capture);
    }
}
