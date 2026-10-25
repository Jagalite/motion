//! Actual service owns Ready97, source workspace and native intent throughout.
use super::*;
use motion_catalog_backend::{NativeIntentCommitState, NativeIntentPreparationPhase};

async fn ready97(f: &mut Fixture, path: &Path) {
    staged(f, path).await;
    f.service
        .settle_selected_staging_for_control97()
        .await
        .unwrap();
    f.service.begin_selected_control97_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service
        .settle_selected_control97_for_native96()
        .await
        .unwrap();
}
async fn paused96(f: &Fixture, at: u8) {
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.test.native96_reached.load(Ordering::Acquire) == at {
            return;
        }
        assert_eq!(
            f.shared.status.load(Ordering::Acquire),
            RUNNING,
            "failed before native96 boundary"
        );
        changed.await;
    }
}
fn release96(f: &Fixture) {
    *f.shared.test.native96_released.lock().unwrap() = true;
    f.shared.test.native96_release.notify_one();
}
// A failing assertion must not strand the blocking test worker in its pause.
// Releasing this gate is not worker completion: successful paths still await
// wait_selected_pair_restore and close the actual service explicitly.
struct Native96PauseRelease(Arc<Shared>);
impl Drop for Native96PauseRelease {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.cancel.store(true, Ordering::Release);
        }
        *self.0.test.native96_released.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.0.test.native96_release.notify_one();
    }
}
async fn rows(path: &Path, table: &str) -> i64 {
    use sqlx::Connection;
    let mut db = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true),
    )
    .await
    .unwrap();
    let count = sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(&mut db)
        .await
        .unwrap();
    db.close().await.unwrap();
    count
}
#[tokio::test]
async fn actual_service_native96_keeps_whole_phase_and_single_retired_workspace() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready97(&mut f, root.path()).await;
    let (motion, native) = names(&f, root.path());
    let bytes = std::fs::read(&native).unwrap();
    let original = std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap();
    let auth = f.shared.authorization_storage.load(Ordering::Acquire);
    f.service.begin_selected_native96_restore().unwrap();
    drop(f.service.wait_selected_pair_restore());
    f.service.wait_selected_pair_restore().await.unwrap();
    assert!(f.service.begin_selected_native96_restore().is_err());
    assert_eq!(auth, f.shared.authorization_storage.load(Ordering::Acquire));
    assert_eq!(auth, AuthorizationWorkspace::service_storage_reservation());
    {
        let phase = f.shared.phase.lock().unwrap();
        assert!(
            phase.workspace.is_none(),
            "workspace moves with whole typed successor"
        );
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::ReadyIntent96);
        let e = target.native96_evidence();
        assert!(!e.0 && !e.1 && e.2.is_none());
        assert_eq!(e.3, Some((true, NativeIntentCommitState::WriterReacquired)));
        assert_eq!(
            file_identity(e.4.unwrap()),
            file_identity(target.staging_evidence()[1].unwrap().original_record_fd())
        );
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
    }
    assert_eq!(
        bytes,
        std::fs::read(&native).unwrap(),
        "native preparation does not apply migration"
    );
    assert_eq!(
        original,
        std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap()
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_intents").await, 1);
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks").await, 0);
    no_later_effects(root.path());
    f.close().await;
}
#[tokio::test]
async fn actual_service_native96_cancel_and_unwind_after_hash_keep_locks_and_fd() {
    for panic in [false, true] {
        let mut f = fixture().await;
        let root = tempfile::tempdir().unwrap();
        ready97(&mut f, root.path()).await;
        let (motion, native) = names(&f, root.path());
        let bytes = std::fs::read(&native).unwrap();
        f.shared.test.native96_pause.store(2, Ordering::Release);
        f.service.begin_selected_native96_restore().unwrap();
        paused96(&f, 2).await;
        if panic {
            f.shared.test.native96_panic.store(true, Ordering::Release);
        } else {
            f.shared.cancel.store(true, Ordering::Release);
        }
        release96(&f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert!(f.service.begin_selected_native96_restore().is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Native96);
            let e = target.native96_evidence();
            assert!(e.4.is_some() && e.3.is_none());
            assert_eq!(e.2.unwrap().0, NativeIntentPreparationPhase::SourceLocked);
            assert!(!e.2.unwrap().2);
        }
        assert_eq!(bytes, std::fs::read(&native).unwrap());
        assert_eq!(rows(&motion, "catabolic_native_bootstrap_intents").await, 0);
        no_later_effects(root.path());
        f.close().await;
    }
}
#[tokio::test]
async fn actual_service_native96_retains_durable_commit_and_pending_workspace() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready97(&mut f, root.path()).await;
    let (motion, _) = names(&f, root.path());
    f.shared.test.native96_pause.store(5, Ordering::Release);
    f.service.begin_selected_native96_restore().unwrap();
    paused96(&f, 5).await;
    f.shared.test.native96_panic.store(true, Ordering::Release);
    release96(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let e = phase.target.as_ref().unwrap().native96_evidence();
        assert_eq!(e.2.unwrap().1, NativeIntentCommitState::WriterReacquired);
        assert!(!e.2.unwrap().2 && e.3.is_none());
    }
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_intents").await, 1);
    f.close().await;
}

#[tokio::test]
async fn actual_service_native96_source_writer_exclusion_and_real_expiry_after_hash() {
    use sqlx::Connection;
    let mut f = fixture_with_staging_expiry().await;
    let expiry = f.target_expiry.unwrap();
    let root = tempfile::tempdir().unwrap();
    ready97(&mut f, root.path()).await;
    let (motion, native) = names(&f, root.path());
    f.shared.test.native96_pause.store(2, Ordering::Release);
    let _pause_release = Native96PauseRelease(f.shared.clone());
    f.service.begin_selected_native96_restore().unwrap();
    paused96(&f, 2).await;
    // The actual current WRITE prevents any supported permission-row update.
    // Do not fabricate revocation through a bypass of that retained writer.
    // pair_fd names the anonymous exported snapshot, not the current writer's
    // database. Its /proc target is deliberately unlinked and cannot be opened
    // as a SQLite pathname. Compete against the original fixture database,
    // whose path and inode were observed from its producing SQLite pool.
    let current_path = &f.source_motion_path;
    let current_metadata = std::fs::symlink_metadata(current_path).unwrap();
    assert!(current_metadata.file_type().is_file());
    assert_eq!(
        (current_metadata.dev(), current_metadata.ino()),
        f.source_motion_identity,
        "competitor must target the original current Motion database"
    );
    let mut competing = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(current_path)
            .create_if_missing(false)
            .busy_timeout(Duration::ZERO),
    )
    .await
    .unwrap();
    let blocked = sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut competing)
        .await
        .unwrap_err();
    assert_eq!(
        blocked
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("5"),
        "the actual source writer must reject the competitor with SQLITE_BUSY"
    );
    competing.close().await.unwrap();
    assert!(
        catabolic_store::Store::open(&native, true, false).is_err(),
        "real native writer lease excludes cooperating Store"
    );
    assert!(
        crate::now() < expiry,
        "fixture must reach the full-hash boundary while credential is valid"
    );
    while crate::now() < expiry {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!f.shared.cancel.load(Ordering::Acquire));
    release96(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let Some(Failure::Error(error)) = &phase.failure else {
            panic!("expected actual authorization expiry")
        };
        assert!(
            format!("{error:#}").contains("credential_expired"),
            "{error:#}"
        );
        let e = phase.target.as_ref().unwrap().native96_evidence();
        assert_eq!(e.2.unwrap().0, NativeIntentPreparationPhase::SourceLocked);
        assert_eq!(e.2.unwrap().1, NativeIntentCommitState::NotAttempted);
    }
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_intents").await, 0);
    f.close().await;
}

#[tokio::test]
async fn actual_service_native96_real_busy_commit_keeps_uncertain_owner() {
    use sqlx::Connection;
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready97(&mut f, root.path()).await;
    let (motion, _) = names(&f, root.path());
    f.shared.test.native96_pause.store(4, Ordering::Release);
    f.service.begin_selected_native96_restore().unwrap();
    paused96(&f, 4).await;
    let mut blocker = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&motion)
            .read_only(true),
    )
    .await
    .unwrap();
    let mut transaction = blocker.begin().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_native_bootstrap_intents")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "real reader sees only pre-intent durable snapshot"
    );
    release96(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let e = phase.target.as_ref().unwrap().native96_evidence();
        assert_eq!(e.2.unwrap().1, NativeIntentCommitState::CommitAttempted);
        assert!(e.3.is_none() && e.4.is_some());
    }
    transaction.rollback().await.unwrap();
    blocker.close().await.unwrap();
    f.close().await;
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_intents").await, 0);
    no_later_effects(root.path());
}

#[path = "native96_tests/source94_tests.rs"]
mod source94_tests;
#[path = "native96_tests/source96_tests.rs"]
mod source96_tests;
