//! Actual NativeService stop/export/select/reserve/stage path. No fixture takes
//! a PreparedPair, target, stage, current token or workspace out of the service.
use super::super::target_reservation::TargetPhase;
use super::*;
use playscale_core::paired_backup::publication::Phase as PublicationPhase;
use std::future::Future;

async fn reserved(f: &mut Fixture, path: &Path, deadline: Instant) {
    f.begin(deadline);
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_pair_for_target().await.unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", f.app.admin_token).parse().unwrap(),
    );
    let caller = match f.target_caller.take() {
        Some(caller) => caller,
        None => crate::v2::auth::resolve(&f.app, &axum::http::Method::GET, &headers)
            .await
            .unwrap(),
    };
    f.service
        .begin_selected_target_restore(path.into(), caller)
        .unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    assert!(f.service.begin_selected_staging_restore().is_err());
    f.service
        .settle_selected_target_for_staging()
        .await
        .unwrap();
    f.assert_phase(true, true);
}
fn names(path: &Path) -> [String; 3] {
    let reservation: serde_json::Value = serde_json::from_slice(
        &std::fs::read(path.join("paired-install.reservation.json")).unwrap(),
    )
    .unwrap();
    let id = reservation["install_id"].as_str().unwrap();
    [
        format!("paired-conversion-{id}.motion.sqlite3"),
        format!("paired-conversion-{id}.native.sqlite3"),
        "paired-install.conversion.json".into(),
    ]
}
fn current_bytes(f: &Fixture) -> Vec<u8> {
    f.assert_phase(true, true);
    std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap()
}
fn no_install(path: &Path) {
    for name in [
        "playscale.sqlite3",
        "catabolic.sqlite3",
        "paired-install.intent.json",
        "paired-install.motion.json",
        "paired-install.native.json",
        "paired-install.complete.json",
    ] {
        assert!(
            !path.join(name).exists(),
            "unexpected final install: {name}"
        );
    }
}
async fn stage_reached(shared: &Shared, value: u8) {
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if shared.test.staging_reached.load(Ordering::Acquire) == value {
            return;
        }
        assert_eq!(shared.status.load(Ordering::Acquire), RUNNING);
        changed.await;
    }
}
fn release(shared: &Shared) {
    *shared.test.staging_released.lock().unwrap() = true;
    shared.test.staging_release.notify_one();
}
fn exact_fd(fd: i32) -> (u64, u64, u64) {
    assert!(fd > 2);
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
    let stat = unsafe { stat.assume_init() };
    (stat.st_dev, stat.st_ino, stat.st_nlink)
}

#[tokio::test]
async fn actual_service_staging_success_uses_same_owners_and_one_auth_charge() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    reserved(
        &mut f,
        target.path(),
        Instant::now() + Duration::from_secs(120),
    )
    .await;
    let source = current_bytes(&f);
    let ancestry = f
        .shared
        .phase
        .lock()
        .unwrap()
        .target
        .as_ref()
        .unwrap()
        .staging_ancestry_evidence();
    let (selected_fd, workspace, charge, selection_charge) = {
        let phase = f.shared.phase.lock().unwrap();
        (
            phase
                .selection
                .as_ref()
                .unwrap()
                .evidence_for_fixture()
                .0
                .unwrap()
                .0
                .as_raw_fd(),
            phase.workspace.as_ref().unwrap() as *const AuthorizationWorkspace as usize,
            f.shared.authorization_storage.load(Ordering::Acquire),
            f.shared.selection_storage.load(Ordering::Acquire),
        )
    };
    f.service.begin_selected_staging_restore().unwrap();
    drop(f.service.wait_selected_pair_restore()); // unpolled status waiter
    f.service.wait_selected_pair_restore().await.unwrap();
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.selection_timer.is_some());
    assert!(f.service.begin_selected_staging_restore().is_err());
    assert_eq!(source, current_bytes(&f));
    assert_eq!(
        charge,
        AuthorizationWorkspace::service_storage_reservation()
    );
    assert_eq!(
        f.shared.authorization_storage.load(Ordering::Acquire),
        charge
    );
    assert_eq!(
        f.shared.selection_storage.load(Ordering::Acquire),
        selection_charge
    );
    {
        let phase = f.shared.phase.lock().unwrap();
        assert_eq!(
            phase.workspace.as_ref().unwrap() as *const AuthorizationWorkspace as usize,
            workspace
        );
        assert!(phase.workspace.as_ref().unwrap().reusable());
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert_eq!(
            phase
                .selection
                .as_ref()
                .unwrap()
                .evidence_for_fixture()
                .0
                .unwrap()
                .0
                .as_raw_fd(),
            selected_fd
        );
        let owner = phase.target.as_ref().unwrap();
        assert_eq!(owner.phase, TargetPhase::Staged);
        let after = owner.staging_ancestry_evidence();
        assert!(after.0 < ancestry.0);
        assert_eq!(
            (after.1, after.2, after.3, after.4),
            (ancestry.1, ancestry.2, ancestry.3, ancestry.4)
        );
        let outcomes = owner.staging_evidence();
        for (index, outcome) in outcomes.into_iter().enumerate() {
            let outcome = outcome.unwrap();
            assert_eq!(outcome.phase(), PublicationPhase::Durable);
            let evidence = outcome.evidence();
            let named = std::fs::metadata(target.path().join(&evidence.name)).unwrap();
            assert_eq!(
                (named.dev(), named.ino()),
                (evidence.file_device, evidence.file_inode)
            );
            assert_eq!(
                outcome.original_record_fd(),
                f.shared.test.staging_fds[index].load(Ordering::Acquire)
            );
            assert_eq!(
                catabolic_core::sha256(std::fs::read(target.path().join(&evidence.name)).unwrap()),
                evidence.artifact.sha256
            );
        }
        let record = owner.staging_record().unwrap();
        let bytes = std::fs::read(target.path().join("paired-install.conversion.json")).unwrap();
        assert_eq!(bytes, serde_json::to_vec(record).unwrap());
        assert_eq!(record.motion_stage, *outcomes[0].unwrap().evidence());
        assert_eq!(record.native_stage, *outcomes[1].unwrap().evidence());
        assert_eq!(record.source_motion_schema_version, 93);
        assert_eq!(record.source_native_identity.schema_version, 40);
        assert!(record.intended_transition_id.starts_with("paired-upgrade-"));
    }
    no_install(target.path());
    f.close().await;
    for name in names(target.path()) {
        assert!(target.path().join(name).exists());
    }
    no_install(target.path());
}

#[tokio::test]
async fn actual_service_staging_unpolled_retirement_never_claims_stage() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    // Complete genuine target work but deliberately retain its actual handles.
    f.begin(Instant::now() + Duration::from_secs(120));
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_pair_for_target().await.unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", f.app.admin_token).parse().unwrap(),
    );
    let caller = crate::v2::auth::resolve(&f.app, &axum::http::Method::GET, &headers)
        .await
        .unwrap();
    f.shared
        .test
        .target_join_pause
        .store(true, Ordering::Release);
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.test.target_join_reached.load(Ordering::Acquire) {
            break;
        }
        changed.await;
    }
    drop(f.service.settle_selected_target_for_staging());
    {
        let mut retirement = std::pin::pin!(f.service.settle_selected_target_for_staging());
        std::future::poll_fn(|cx| {
            assert!(retirement.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.selection_timer.is_some());
    assert!(f.service.begin_selected_staging_restore().is_err());
    assert_eq!(
        f.shared
            .phase
            .lock()
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .phase,
        TargetPhase::Reserved
    );
    for name in names(target.path()) {
        assert!(!target.path().join(name).exists());
    }
    *f.shared.test.target_join_released.lock().unwrap() = true;
    f.shared.test.target_join_release.notify_one();
    f.service
        .settle_selected_target_for_staging()
        .await
        .unwrap();
    f.service.begin_selected_staging_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn actual_service_staging_abandoned_waiter_retains_partial_durable_prefix() {
    for boundary in [2, 3, 4, 5] {
        let mut f = fixture().await;
        let target = tempfile::tempdir().unwrap();
        reserved(
            &mut f,
            target.path(),
            Instant::now() + Duration::from_secs(120),
        )
        .await;
        let source = current_bytes(&f);
        f.shared
            .test
            .staging_pause
            .store(boundary, Ordering::Release);
        f.service.begin_selected_staging_restore().unwrap();
        stage_reached(&f.shared, boundary).await;
        {
            let mut waiter = std::pin::pin!(f.service.wait_selected_pair_restore());
            std::future::poll_fn(|cx| {
                assert!(waiter.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        assert!(f.shared.cancel.load(Ordering::Acquire));
        f.assert_current();
        assert!(f.service.restore.worker.is_some());
        let held =
            f.shared.test.staging_fds[(boundary - 2).min(2) as usize].load(Ordering::Acquire);
        let identity = exact_fd(held);
        release(&f.shared);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_eq!(source, current_bytes(&f));
        assert_eq!(exact_fd(held), identity);
        let expected = if boundary == 5 {
            3
        } else {
            usize::from(boundary - 2)
        };
        for (index, name) in names(target.path()).into_iter().enumerate() {
            assert_eq!(target.path().join(name).exists(), index < expected);
        }
        if boundary == 5 {
            let phase = f.shared.phase.lock().unwrap();
            assert_eq!(
                phase.target.as_ref().unwrap().phase,
                TargetPhase::StagedUnretired
            );
            assert!(!phase.workspace.as_ref().unwrap().reusable());
            assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        }
        assert!(f.service.begin_selected_staging_restore().is_err());
        no_install(target.path());
        f.close().await;
        no_install(target.path());
    }
}

#[tokio::test]
async fn actual_service_staging_partial_motion_native_record_preparation_retains_fds() {
    for failed in 1..=3 {
        let mut f = fixture().await;
        let target = tempfile::tempdir().unwrap();
        reserved(
            &mut f,
            target.path(),
            Instant::now() + Duration::from_secs(120),
        )
        .await;
        let source = current_bytes(&f);
        f.shared
            .test
            .staging_prepare_failure
            .store(failed, Ordering::Release);
        f.service.begin_selected_staging_restore().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_eq!(source, current_bytes(&f));
        {
            let phase = f.shared.phase.lock().unwrap();
            let owner = phase.target.as_ref().unwrap();
            assert_eq!(owner.phase, TargetPhase::Staging);
            let preparation = owner.staging_preparation(usize::from(failed - 1)).unwrap();
            assert_eq!(
                preparation.phase(),
                publication_file::PreparationPhase::Failed
            );
            let (parent, file) = preparation.original_fds();
            assert!(parent.is_some());
            assert_eq!(exact_fd(file.unwrap()).2, 0);
            assert!(preparation.retained_length().unwrap() > 0);
            assert!(owner.staging_evidence()[usize::from(failed - 1)].is_none());
            assert_eq!(owner.staging_record().is_some(), failed == 3);
            assert!(phase.workspace.as_ref().unwrap().reusable());
        }
        for (index, name) in names(target.path()).into_iter().enumerate() {
            assert_eq!(
                target.path().join(name).exists(),
                index < usize::from(failed - 1)
            );
        }
        no_install(target.path());
        assert!(f.service.begin_selected_staging_restore().is_err());
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service_staging_link_collision_keeps_original_and_unrelated_name() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    reserved(
        &mut f,
        target.path(),
        Instant::now() + Duration::from_secs(120),
    )
    .await;
    let native = target.path().join(&names(target.path())[1]);
    f.shared.test.staging_pause.store(3, Ordering::Release);
    f.service.begin_selected_staging_restore().unwrap();
    stage_reached(&f.shared, 3).await;
    let fd = f.shared.test.staging_fds[1].load(Ordering::Acquire);
    let original = exact_fd(fd);
    std::fs::write(&native, b"unrelated collision").unwrap();
    release(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_eq!(exact_fd(fd), original);
    assert_eq!(std::fs::read(&native).unwrap(), b"unrelated collision");
    assert_eq!(
        f.shared
            .phase
            .lock()
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .staging_evidence()[1]
            .unwrap()
            .phase(),
        PublicationPhase::Collision
    );
    f.close().await;
    assert_eq!(std::fs::read(&native).unwrap(), b"unrelated collision");
    no_install(target.path());
}

#[tokio::test]
async fn actual_service_staging_directory_sync_failure_keeps_visible_exact_prefix() {
    for failed in 1..=3 {
        let mut f = fixture().await;
        let target = tempfile::tempdir().unwrap();
        reserved(
            &mut f,
            target.path(),
            Instant::now() + Duration::from_secs(120),
        )
        .await;
        f.shared
            .test
            .staging_sync_failure
            .store(failed, Ordering::Release);
        f.service.begin_selected_staging_restore().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        let fd = f.shared.test.staging_fds[usize::from(failed - 1)].load(Ordering::Acquire);
        let original = exact_fd(fd);
        assert_eq!(original.2, 1);
        {
            let phase = f.shared.phase.lock().unwrap();
            let outcomes = phase.target.as_ref().unwrap().staging_evidence();
            assert_eq!(
                outcomes[usize::from(failed - 1)].unwrap().phase(),
                PublicationPhase::LinkedUnconfirmed
            );
            assert!(phase.workspace.as_ref().unwrap().reusable());
        }
        for (index, name) in names(target.path()).into_iter().enumerate() {
            assert_eq!(
                target.path().join(name).exists(),
                index < usize::from(failed)
            );
        }
        no_install(target.path());
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service_staging_inherits_deadline_without_waiter_or_fresh_budget() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    reserved(&mut f, target.path(), deadline).await;
    let charge = f.shared.authorization_storage.load(Ordering::Acquire);
    f.shared.test.staging_pause.store(3, Ordering::Release);
    f.service.begin_selected_staging_restore().unwrap();
    stage_reached(&f.shared, 3).await;
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    while !f.shared.cancel.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
    release(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_eq!(
        f.shared.authorization_storage.load(Ordering::Acquire),
        charge
    );
    let names = names(target.path());
    assert!(target.path().join(&names[0]).exists());
    assert!(!target.path().join(&names[1]).exists());
    assert!(!target.path().join(&names[2]).exists());
    no_install(target.path());
    f.close().await;
}

#[tokio::test]
async fn actual_service_revoked_caller_never_reaches_staging_claim() {
    let mut f = fixture_with_target_auth(Some(super::target_tests::AuthCase::Revoked)).await;
    let target = tempfile::tempdir().unwrap();
    f.begin(Instant::now() + Duration::from_secs(120));
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_pair_for_target().await.unwrap();
    f.service
        .begin_selected_target_restore(target.path().into(), f.target_caller.take().unwrap())
        .unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert!(
        f.service
            .settle_selected_target_for_staging()
            .await
            .is_err()
    );
    assert!(f.service.begin_selected_staging_restore().is_err());
    assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
    f.assert_phase(true, true);
    f.close().await;
}

#[tokio::test]
async fn actual_service_staging_abandoned_close_and_failed_close_keep_partial_slots() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    reserved(
        &mut f,
        target.path(),
        Instant::now() + Duration::from_secs(120),
    )
    .await;
    f.shared.test.staging_pause.store(5, Ordering::Release);
    f.service.begin_selected_staging_restore().unwrap();
    stage_reached(&f.shared, 5).await;
    let fd = f.shared.test.staging_fds[2].load(Ordering::Acquire);
    let original = exact_fd(fd);
    {
        let mut waiter = std::pin::pin!(f.service.close_restore());
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert!(f.shared.cancel.load(Ordering::Acquire));
    assert!(f.service.restore.worker.is_some());
    f.shared
        .test
        .selection_close_failure
        .store(true, Ordering::Release);
    release(&f.shared);
    assert!(f.service.close_restore().await.is_err());
    assert_eq!(exact_fd(fd), original);
    f.assert_phase(true, true);
    assert!(f.shared.phase.lock().unwrap().target.is_some());
    assert!(f.shared.phase.lock().unwrap().workspace.is_some());
    assert!(f.service.close_restore().await.is_err());
    // Fixture-only removal of the deliberate failure; no production retry.
    f.shared
        .test
        .selection_close_failure
        .store(false, Ordering::Release);
    let retained = f.shared.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(close_shared(&retained)))
        .await
        .unwrap()
        .unwrap();
    assert!(f.shared.phase.lock().unwrap().target.is_none());
    assert!(f.shared.phase.lock().unwrap().workspace.is_none());
    assert!(f.shared.stopped.lock().unwrap().is_none());
    for name in names(target.path()) {
        assert!(target.path().join(name).exists());
    }
    no_install(target.path());
}

#[tokio::test]
async fn actual_service_staging_fresh_native_auth_refuses_genuine_credential_expiry() {
    // Public pairing and child-token issuance happen before the actual service
    // stops. No auth rows, Caller fields, clocks, TTLs or writers are fabricated.
    let mut f = fixture_with_staging_expiry().await;
    let expiry = f.target_expiry.unwrap();
    let target = tempfile::tempdir().unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    reserved(&mut f, target.path(), deadline).await;
    let source = current_bytes(&f);
    f.shared.test.staging_pause.store(3, Ordering::Release);
    f.service.begin_selected_staging_restore().unwrap();
    stage_reached(&f.shared, 3).await;
    assert!(
        crate::now() < expiry,
        "credential must reach real staging preflight while valid"
    );
    while crate::now() < expiry {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(Instant::now() < deadline);
    assert!(!f.shared.cancel.load(Ordering::Acquire));
    release(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_eq!(source, current_bytes(&f));
    let names = names(target.path());
    assert!(target.path().join(&names[0]).exists());
    assert!(!target.path().join(&names[1]).exists());
    assert!(!target.path().join(&names[2]).exists());
    {
        let phase = f.shared.phase.lock().unwrap();
        let Some(Failure::Error(error)) = &phase.failure else {
            panic!("expected genuine authentication refusal")
        };
        assert!(
            format!("{error:#}").contains("credential_expired"),
            "{error:#}"
        );
        assert!(!f.shared.cancel.load(Ordering::Acquire));
        assert!(Instant::now() < deadline);
        assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::Staging);
        assert_eq!(
            phase.target.as_ref().unwrap().staging_evidence()[1]
                .unwrap()
                .phase(),
            PublicationPhase::Abandoned
        );
        assert!(!phase.workspace.as_ref().unwrap().reusable());
        assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert!(
            phase
                .pair
                .as_ref()
                .unwrap()
                ._writer
                .retains_authorization_reader()
        );
    }
    no_install(target.path());
    f.close().await;
}

#[tokio::test]
async fn actual_service_staging_worker_panic_retains_durable_motion_and_anonymous_native() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    reserved(
        &mut f,
        target.path(),
        Instant::now() + Duration::from_secs(120),
    )
    .await;
    let source = current_bytes(&f);
    f.shared.test.staging_pause.store(3, Ordering::Release);
    f.shared.test.staging_panic.store(true, Ordering::Release);
    f.service.begin_selected_staging_restore().unwrap();
    stage_reached(&f.shared, 3).await;
    let fd = f.shared.test.staging_fds[1].load(Ordering::Acquire);
    let original = exact_fd(fd);
    release(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_eq!(current_bytes(&f), source);
    assert_eq!(exact_fd(fd), original);
    {
        let phase = f.shared.phase.lock().unwrap_or_else(|p| p.into_inner());
        assert!(matches!(&phase.failure, Some(Failure::Panic(_))));
        let outcomes = phase.target.as_ref().unwrap().staging_evidence();
        assert_eq!(outcomes[0].unwrap().phase(), PublicationPhase::Durable);
        assert_eq!(outcomes[1].unwrap().phase(), PublicationPhase::Ready);
        assert!(outcomes[2].is_none());
        assert!(phase.workspace.as_ref().unwrap().reusable());
    }
    no_install(target.path());
    f.close().await;
}

#[tokio::test]
async fn actual_service_staging_refuses_moved_original_source_parent_edge() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    reserved(
        &mut f,
        target.path(),
        Instant::now() + Duration::from_secs(120),
    )
    .await;
    let source = current_bytes(&f);
    let held_staging = target.path().join("moved-original-selected-staging");
    // Move a genuine selected extraction directory under the reserved target.
    // The exact selected image/parent FDs stay live and bytes stay unchanged;
    // only a formerly disjoint original ancestry edge changes.
    std::fs::rename(&f.staging, &held_staging).unwrap();
    f.service.begin_selected_staging_restore().unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_eq!(source, current_bytes(&f));
    {
        let phase = f.shared.phase.lock().unwrap();
        let Some(Failure::Error(error)) = &phase.failure else {
            panic!("expected ancestry refusal")
        };
        assert!(
            format!("{error:#}").contains("ancestry changed"),
            "{error:#}"
        );
        assert!(phase.workspace.as_ref().unwrap().reusable());
        let motion = phase.target.as_ref().unwrap().staging_evidence()[0].unwrap();
        assert_eq!(motion.phase(), PublicationPhase::Ready);
        assert_eq!(exact_fd(motion.original_record_fd()).2, 0);
    }
    for name in names(target.path()) {
        assert!(!target.path().join(name).exists());
    }
    no_install(target.path());
    // Fixture cleanup restores only the moved directory; no publication retry,
    // path adoption, selected-owner reconstruction or auth mutation takes place.
    std::fs::rename(&held_staging, &f.staging).unwrap();
    f.close().await;
}

#[tokio::test]
async fn actual_service_staging_full_preflight_rejects_future_native_sidecar() {
    let mut f = fixture().await;
    let target = tempfile::tempdir().unwrap();
    reserved(
        &mut f,
        target.path(),
        Instant::now() + Duration::from_secs(120),
    )
    .await;
    let source = current_bytes(&f);
    let sidecar = target
        .path()
        .join(format!("{}-wal", names(target.path())[1]));
    std::fs::write(&sidecar, b"unrelated native sidecar").unwrap();
    f.service.begin_selected_staging_restore().unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_eq!(source, current_bytes(&f));
    {
        let phase = f.shared.phase.lock().unwrap();
        let owner = phase.target.as_ref().unwrap();
        // Motion copy itself was prepared. Full preflight refused a native
        // sidecar before any fresh auth/link or native preparation occurred.
        assert_eq!(
            owner.staging_evidence()[0].unwrap().phase(),
            PublicationPhase::Ready
        );
        assert!(owner.staging_evidence()[1].is_none());
        assert!(phase.workspace.as_ref().unwrap().reusable());
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
    }
    for name in names(target.path()) {
        assert!(!target.path().join(name).exists());
    }
    f.close().await;
    assert_eq!(std::fs::read(sidecar).unwrap(), b"unrelated native sidecar");
    no_install(target.path());
}
