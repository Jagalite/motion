//! Real service startup/stop/export/select/reserve/stage/control97. No detached
//! original writer, fabricated shutdown flags or mock authentication grants.
use super::*;
use super::super::target_reservation::TargetPhase;
use motion_catalog_backend::{RecordedCommitState, RecordedControlPhase};
use std::future::Future;

async fn staged(f: &mut Fixture, path: &Path) {
    // Chosen by the fixture before selection begins. The default remains120s;
    // only the dedicated capture-expiry-cause fixture selects240s at entry.
    f.begin(Instant::now() + f.initial_selection_timeout);
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_pair_for_target().await.unwrap();
    let caller = match f.target_caller.take() {
        Some(caller) => caller,
        None => {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert("authorization", format!("Bearer {}", f.app.admin_token).parse().unwrap());
            crate::v2::auth::resolve(&f.app, &axum::http::Method::GET, &headers).await.unwrap()
        }
    };
    f.service.begin_selected_target_restore(path.into(), caller).unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_target_for_staging().await.unwrap();
    f.service.begin_selected_staging_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
}
fn names(f: &Fixture, parent: &Path) -> (PathBuf, PathBuf) {
    let phase = f.shared.phase.lock().unwrap();
    let record = phase.target.as_ref().unwrap().staging_record().unwrap();
    (parent.join(&record.motion_stage.name), parent.join(&record.native_stage.name))
}
fn file_identity(fd: i32) -> (u64, u64) {
    assert!(fd > 2);
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    assert_eq!(unsafe { libc::fstat(fd, stat.as_mut_ptr()) }, 0);
    let stat = unsafe { stat.assume_init() };
    (stat.st_dev, stat.st_ino)
}
fn no_later_effects(path: &Path) {
    for name in ["playscale.sqlite3", "catabolic.sqlite3", "paired-install.intent.json",
        "paired-install.motion.json", "paired-install.native.json", "paired-install.complete.json"] {
        assert!(!path.join(name).exists(), "unexpected install: {name}");
    }
}
async fn paused(f: &Fixture, at: u8) {
    loop {
        let notified = f.shared.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if f.shared.test.control97_reached.load(Ordering::Acquire) == at { return; }
        assert_eq!(f.shared.status.load(Ordering::Acquire), RUNNING, "failed before control boundary");
        notified.await;
    }
}
fn release(f: &Fixture) {
    *f.shared.test.control97_released.lock().unwrap() = true;
    f.shared.test.control97_release.notify_one();
}
async fn version(path: &Path) -> i64 {
    use sqlx::Connection;
    // This fixture owns the directory; never admit a symlink as its stage.
    assert!(std::fs::symlink_metadata(path).unwrap().file_type().is_file());
    let mut db = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false),
    ).await.unwrap();
    let version = sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&mut db).await.unwrap();
    db.close().await.unwrap();
    version
}

#[tokio::test]
async fn actual_service_control97_retains_same_owners_and_single_workspace() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    staged(&mut f, root.path()).await;
    let (motion, native) = names(&f, root.path());
    let initial_motion = std::fs::read(&motion).unwrap();
    let native_bytes = std::fs::read(&native).unwrap();
    let current = std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap();
    let (selected, workspace, deadline, ancestry) = {
        let phase = f.shared.phase.lock().unwrap();
        (phase.selection.as_ref().unwrap().evidence_for_fixture().0.unwrap().0.as_raw_fd(),
         phase.workspace.as_ref().unwrap() as *const AuthorizationWorkspace as usize,
         phase.selection.as_ref().unwrap().target_deadline(&f.shared.cancel).unwrap(),
         phase.target.as_ref().unwrap().staging_ancestry_evidence())
    };
    let auth_charge = f.shared.authorization_storage.load(Ordering::Acquire);
    let selected_charge = f.shared.selection_storage.load(Ordering::Acquire);
    assert!(f.service.begin_selected_control97_restore().is_err());
    drop(f.service.settle_selected_staging_for_control97());
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.begin_selected_control97_restore().is_err());
    f.service.settle_selected_staging_for_control97().await.unwrap();
    f.service.begin_selected_control97_restore().unwrap();
    drop(f.service.wait_selected_pair_restore()); // unpolled waiter has no custody
    f.service.wait_selected_pair_restore().await.unwrap();
    assert!(f.service.begin_selected_control97_restore().is_err());
    assert_eq!(auth_charge, f.shared.authorization_storage.load(Ordering::Acquire));
    assert_eq!(auth_charge, AuthorizationWorkspace::service_storage_reservation());
    assert_eq!(selected_charge, f.shared.selection_storage.load(Ordering::Acquire));
    assert_eq!(native_bytes, std::fs::read(&native).unwrap());
    assert_eq!(current, std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap());
    assert_ne!(initial_motion, std::fs::read(&motion).unwrap());
    assert_eq!(version(&motion).await, 97);
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Ready97);
        let evidence = target.control97_evidence().unwrap();
        assert_eq!(evidence.2, Some(RecordedControlPhase::Ready97));
        assert_eq!(evidence.3, Some(RecordedCommitState::WriterReacquired));
        assert_eq!(phase.workspace.as_ref().unwrap() as *const AuthorizationWorkspace as usize, workspace);
        assert!(phase.workspace.as_ref().unwrap().reusable());
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert_eq!(phase.selection.as_ref().unwrap().target_deadline(&f.shared.cancel).unwrap(), deadline);
        assert_eq!(phase.selection.as_ref().unwrap().evidence_for_fixture().0.unwrap().0.as_raw_fd(), selected);
        let after = target.staging_ancestry_evidence();
        assert!(after.0 < ancestry.0);
        assert_eq!((after.1, after.2, after.3, after.4), (ancestry.1, ancestry.2, ancestry.3, ancestry.4));
        for outcome in target.staging_evidence().into_iter().flatten() {
            let identity = file_identity(outcome.original_record_fd());
            assert_eq!(identity, (outcome.evidence().file_device, outcome.evidence().file_inode));
        }
    }
    assert!(f.shared.test.control97_async_frame.load(Ordering::Acquire) > 0);
    assert!(f.shared.test.control97_async_frame.load(Ordering::Acquire) <= super::super::target_reservation::ASYNC_FRAME_LIMIT);
    {
        let mut phase = f.shared.phase.lock().unwrap();
        assert_eq!(phase.target.as_mut().unwrap().control97_initial_verification(),
            [false, true, true], "mutated Motion hash stays revoked; native/record stay immutable");
    }
    no_later_effects(root.path());
    f.close().await;
    assert_eq!(version(&motion).await, 97);
    assert_eq!(native_bytes, std::fs::read(native).unwrap());
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service_control97_clone_unwind_keeps_exact_pending_descriptors() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    staged(&mut f, root.path()).await;
    f.service.settle_selected_staging_for_control97().await.unwrap();
    f.shared.test.control97_pause.store(2, Ordering::Release);
    f.shared.test.control97_panic.store(true, Ordering::Release);
    f.service.begin_selected_control97_restore().unwrap();
    paused(&f, 2).await;
    release(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        let evidence = target.control97_evidence().unwrap();
        let original = target.staging_evidence()[0].unwrap();
        assert_eq!(file_identity(evidence.0.unwrap()), file_identity(original.original_record_fd()));
        assert_eq!(file_identity(evidence.1.unwrap()), {
            let meta = std::fs::metadata(root.path()).unwrap(); (meta.dev(), meta.ino())
        });
        assert_eq!(evidence.2, None);
        assert!(phase.workspace.as_ref().unwrap().reusable());
        assert!(matches!(phase.failure, Some(Failure::Panic(_))));
    }
    assert!(f.service.begin_selected_control97_restore().is_err());
    no_later_effects(root.path());
    f.close().await;
}

#[tokio::test]
async fn actual_service_control97_cancel_and_unwind_keep_honest_partial_phases() {
    for (at, panic) in [(3, false), (4, true), (5, false), (5, true), (7, false), (6, false)] {
        let mut f = fixture().await;
        let root = tempfile::tempdir().unwrap();
        staged(&mut f, root.path()).await;
        let (motion, native) = names(&f, root.path());
        let native_bytes = std::fs::read(&native).unwrap();
        f.service.settle_selected_staging_for_control97().await.unwrap();
        f.shared.test.control97_pause.store(at, Ordering::Release);
        f.shared.test.control97_panic.store(panic, Ordering::Release);
        f.service.begin_selected_control97_restore().unwrap();
        paused(&f, at).await;
        if !panic { f.shared.cancel.store(true, Ordering::Release); }
        release(&f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            let evidence = target.control97_evidence().unwrap();
            assert_ne!(target.phase, TargetPhase::Ready97);
            assert_eq!(evidence.3, Some(match at {
                7 => RecordedCommitState::Committed97,
                6 => RecordedCommitState::WriterReacquired,
                _ => RecordedCommitState::NotAttempted,
            }));
            if at == 6 { assert_eq!(target.phase, TargetPhase::Ready97Unretired); }
            assert!(phase.pair.is_some() && phase.selection.is_some());
        }
        assert_eq!(native_bytes, std::fs::read(&native).unwrap());
        no_later_effects(root.path());
        f.close().await;
        assert_eq!(version(&motion).await, if at == 6 || at == 7 { 97 } else { 93 });
        no_later_effects(root.path());
    }
}

#[tokio::test]
async fn actual_service_control97_polled_status_drop_cancels_not_custody() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    staged(&mut f, root.path()).await;
    f.service.settle_selected_staging_for_control97().await.unwrap();
    f.shared.test.control97_pause.store(5, Ordering::Release);
    f.service.begin_selected_control97_restore().unwrap();
    paused(&f, 5).await;
    {
        let mut waiter = std::pin::pin!(f.service.wait_selected_pair_restore());
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }).await;
    }
    assert!(f.shared.cancel.load(Ordering::Acquire));
    assert!(f.service.restore.worker.is_some());
    release(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    let phase = f.shared.phase.lock().unwrap();
    assert_eq!(phase.target.as_ref().unwrap().control97_evidence().unwrap().3,
        Some(RecordedCommitState::NotAttempted));
    drop(phase);
    f.close().await;
}

#[tokio::test]
async fn actual_service_control97_real_busy_commit_keeps_commit_attempted() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    staged(&mut f, root.path()).await;
    let (motion, _) = names(&f, root.path());
    f.service.settle_selected_staging_for_control97().await.unwrap();
    f.shared.test.control97_pause.store(5, Ordering::Release);
    f.service.begin_selected_control97_restore().unwrap();
    paused(&f, 5).await;
    // A genuine read transaction holds SHARED on the old durable93 while the
    // retained writer has pending97. COMMIT must fail, not become Ready97.
    use sqlx::Connection;
    let mut blocker = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&motion)
            .read_only(true)
            .create_if_missing(false),
    ).await.unwrap();
    let mut transaction = blocker.begin().await.unwrap();
    let before = sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&mut *transaction).await.unwrap();
    assert_eq!(before, 93);
    release(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.control97_evidence().unwrap().3, Some(RecordedCommitState::CommitAttempted));
        assert_ne!(target.phase, TargetPhase::Ready97);
    }
    transaction.rollback().await.unwrap();
    blocker.close().await.unwrap();
    f.close().await;
    assert_eq!(version(&motion).await, 93);
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service_control97_real_expiry_refuses_fresh_source_and_commit_fence() {
    for at in [3, 5] {
        let mut f = fixture_with_staging_expiry().await;
        let expiry = f.target_expiry.unwrap();
        let root = tempfile::tempdir().unwrap();
        staged(&mut f, root.path()).await;
        let (motion, _) = names(&f, root.path());
        f.service.settle_selected_staging_for_control97().await.unwrap();
        f.shared.test.control97_pause.store(at, Ordering::Release);
        f.service.begin_selected_control97_restore().unwrap();
        paused(&f, at).await;
        assert!(crate::now() < expiry);
        while crate::now() < expiry { tokio::time::sleep(Duration::from_millis(250)).await; }
        assert!(!f.shared.cancel.load(Ordering::Acquire));
        release(&f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let Some(Failure::Error(error)) = &phase.failure else { panic!("real expiry must refuse") };
            assert!(format!("{error:#}").contains("credential_expired"), "{error:#}");
            assert_eq!(phase.target.as_ref().unwrap().control97_evidence().unwrap().3,
                Some(RecordedCommitState::NotAttempted));
            assert_ne!(phase.target.as_ref().unwrap().phase, TargetPhase::Ready97);
        }
        f.close().await;
        assert_eq!(version(&motion).await, 93);
        no_later_effects(root.path());
    }
}

#[test]
fn actual_service_control97_fixed_layout_is_admitted_without_another_auth_cohort() {
    let (layout, storage, authorization) = ServiceTarget::control97_layout();
    assert!(layout as u64 <= super::super::target_reservation::ASYNC_FRAME_LIMIT);
    assert!(storage >= 2 * layout as u64 + motion_catalog_backend::RecordedControlOperation::inline_storage_reservation());
    assert!(authorization > 0 && authorization < 64 * 1024 * 1024);
    assert!(super::super::target_reservation::storage_reservation() >= storage);
}

#[tokio::test]
async fn actual_service_control97_refuses_foreign_workspace_without_claiming() {
    let mut first = fixture().await;
    let mut second = fixture().await;
    let first_target = tempfile::tempdir().unwrap();
    let second_target = tempfile::tempdir().unwrap();
    staged(&mut first, first_target.path()).await;
    staged(&mut second, second_target.path()).await;
    {
        let mut first_phase = first.shared.phase.lock().unwrap();
        let second_phase = second.shared.phase.lock().unwrap();
        let PhaseOwner { target, pair, .. } = &mut *first_phase;
        let target = target.as_mut().unwrap();
        assert!(target.claim_control97(second_phase.workspace.as_ref().unwrap(),
            &pair.as_ref().unwrap()._stopped, &first.shared.cancel).is_err());
        assert_eq!(target.phase, TargetPhase::Staged);
        assert!(target.control97_evidence().is_none());
    }
    first.close().await;
    second.close().await;
}

#[tokio::test]
async fn actual_service_control97_refuses_stage_sidecar_before_backend_or_auth() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    staged(&mut f, root.path()).await;
    let (motion, _) = names(&f, root.path());
    f.service.settle_selected_staging_for_control97().await.unwrap();
    let journal = format!("{}-journal", motion.display());
    std::fs::write(&journal, b"unexpected sidecar").unwrap();
    f.service.begin_selected_control97_restore().unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.control97_evidence().unwrap().2, None);
        assert!(phase.workspace.as_ref().unwrap().reusable());
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert_eq!(target.phase, TargetPhase::Control97);
    }
    assert_eq!(std::fs::read(&journal).unwrap(), b"unexpected sidecar");
    no_later_effects(root.path());
    f.close().await;
}

#[path = "control97_tests/native96_tests.rs"]
mod native96_tests;
