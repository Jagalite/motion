//! Genuine service97→intent96→apply/bootstrap/ACK, not a detached conversion task.
use super::*;
use motion_catalog_backend::NativeBootstrapCommitState as Commit;

async fn ready_intent(f: &mut Fixture, path: &Path) {
    failure_diagnostics::enable(&f.shared);
    ready97(f, path).await;
    f.service.begin_selected_native96_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service
        .settle_selected_native96_for_source96()
        .await
        .unwrap();
}
async fn paused_source(f: &Fixture, stage: u8) {
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.test.source96_reached.load(Ordering::Acquire) == stage {
            return;
        }
        let status = f.shared.status.load(Ordering::Acquire);
        if status == FAILED {
            failure_diagnostics::report(&f.shared, "source96 before requested boundary");
        }
        assert_eq!(
            status,
            RUNNING,
            "source96 failed before requested boundary"
        );
        changed.await;
    }
}
fn release_source(f: &Fixture) {
    *f.shared.test.source96_released.lock().unwrap() = true;
    f.shared.test.source96_release.notify_one();
}
#[tokio::test]
async fn actual_service_source96_owns_complete_source_and_single_workspace_then_closes() {
    let mut f = fixture().await;
    let root = tempfile::tempdir().unwrap();
    ready_intent(&mut f, root.path()).await;
    let (motion, native) = names(&f, root.path());
    let current = std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap();
    let auth = f.shared.authorization_storage.load(Ordering::Acquire);
    let deadline = f
        .shared
        .phase
        .lock()
        .unwrap()
        .selection
        .as_ref()
        .unwrap()
        .target_deadline(&f.shared.cancel)
        .unwrap();
    f.service.begin_selected_source96_restore().unwrap();
    drop(f.service.wait_selected_pair_restore());
    f.service
        .wait_selected_pair_restore()
        .await
        .inspect_err(|_| failure_diagnostics::report(&f.shared, "source96 expected completion"))
        .unwrap();
    assert!(f.service.begin_selected_source96_restore().is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::SourceReady);
        assert_eq!(target.source96_identity().unwrap().schema_version, 41);
        assert_eq!(target.source96_states(), (false, false, false, true, None));
        assert!(phase.workspace.is_none());
        assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        assert_eq!(
            phase
                .selection
                .as_ref()
                .unwrap()
                .target_deadline(&f.shared.cancel)
                .unwrap(),
            deadline
        );
    }
    assert_eq!(auth, f.shared.authorization_storage.load(Ordering::Acquire));
    assert_eq!(auth, AuthorizationWorkspace::service_storage_reservation());
    let frame = f.shared.test.source96_async_frame.load(Ordering::Acquire);
    assert!(
        frame > 0 && frame <= super::super::super::super::target_reservation::ASYNC_FRAME_LIMIT
    );
    assert_eq!(
        current,
        std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap()
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_intents").await, 1);
    assert_eq!(
        rows(&motion, "catabolic_native_bootstrap_commands").await,
        1
    );
    assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks").await, 1);
    assert_ne!(std::fs::metadata(native).unwrap().len(), 0);
    no_later_effects(root.path());
    f.close().await;
    no_later_effects(root.path());
}
#[tokio::test]
async fn actual_service_source96_cancel_and_panic_keep_each_whole_phase() {
    for (stage, panic) in [(1, false), (2, true), (3, false), (4, true), (5, true)] {
        let mut f = fixture().await;
        let root = tempfile::tempdir().unwrap();
        ready_intent(&mut f, root.path()).await;
        f.shared.test.source96_pause.store(stage, Ordering::Release);
        f.service.begin_selected_source96_restore().unwrap();
        paused_source(&f, stage).await;
        if panic {
            f.shared.test.source96_panic.store(true, Ordering::Release);
        } else {
            f.shared.cancel.store(true, Ordering::Release);
        }
        release_source(&f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert!(f.service.begin_selected_source96_restore().is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Source96);
            let e = target.source96_states();
            assert!(e.0 || e.1 || e.2 || e.3);
            assert!(target.source96_identity().is_none());
        }
        f.assert_current();
        no_later_effects(root.path());
        f.close().await;
    }
}
#[tokio::test]
async fn actual_service_source96_post_effect_unwind_keeps_unretired_authority_and_results() {
    for stage in [2, 3, 4, 5] {
        let mut f = fixture().await;
        let root = tempfile::tempdir().unwrap();
        ready_intent(&mut f, root.path()).await;
        let (motion, _) = names(&f, root.path());
        f.shared
            .test
            .source96_inner_panic
            .store(stage, Ordering::Release);
        f.service.begin_selected_source96_restore().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Source96);
            assert!(
                phase.pair.as_ref().unwrap()._writer.authorization_claimed(),
                "reader retained before retirement ACK"
            );
            let e = target.source96_states();
            if stage == 2 {
                assert!(e.0);
            } else {
                assert!(e.2);
                assert_eq!(e.4.unwrap().1, Commit::WriterReacquired);
            }
        }
        assert_eq!(
            rows(&motion, "catabolic_native_bootstrap_acks").await,
            if stage == 5 { 1 } else { 0 }
        );
        f.close().await;
    }
}
#[tokio::test]
async fn actual_service_source96_unpolled_reader_purposes_are_claimed_and_settled() {
    for stage in [1, 2, 3, 4] {
        let mut f = fixture().await;
        let root = tempfile::tempdir().unwrap();
        ready_intent(&mut f, root.path()).await;
        f.shared
            .test
            .source96_unpolled
            .store(stage, Ordering::Release);
        f.service.begin_selected_source96_restore().unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Source96);
            let e = target.source96_states();
            assert!(e.0 || e.2);
            let writer = &phase.pair.as_ref().unwrap()._writer;
            assert!(writer.authorization_claimed(), "unpolled purpose owns its exact source claim");
            assert!(!writer.retains_authorization_reader(), "no unpolled connection");
            assert!(!writer.retains_authorization_acquisition(), "no unpolled opening frame");
        }
        assert!(f.service.begin_selected_source96_restore().is_err());
        f.close().await;
    }
}
#[tokio::test]
async fn actual_service_source96_real_busy_command_and_ack_commit_retain_uncertainty() {
    use sqlx::Connection;
    for stage in [1, 3] {
        let mut f = fixture().await;
        let root = tempfile::tempdir().unwrap();
        ready_intent(&mut f, root.path()).await;
        let (motion, _) = names(&f, root.path());
        f.shared.test.source96_pause.store(stage, Ordering::Release);
        f.service.begin_selected_source96_restore().unwrap();
        paused_source(&f, stage).await;
        let mut blocker = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
                .read_only(true),
        )
        .await
        .unwrap();
        let mut transaction = blocker.begin().await.unwrap();
        let before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM catabolic_native_bootstrap_acks")
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        assert_eq!(before, 0);
        release_source(&f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let e = phase.target.as_ref().unwrap().source96_states();
            assert!(e.2);
            let states = e.4.unwrap();
            assert_eq!(
                if stage == 1 { states.1 } else { states.2 },
                Commit::CommitAttempted
            );
        }
        transaction.rollback().await.unwrap();
        blocker.close().await.unwrap();
        f.close().await;
        assert_eq!(rows(&motion, "catabolic_native_bootstrap_acks").await, 0);
    }
}
#[tokio::test]
async fn actual_service_source96_real_credential_expiry_denies_next_effect() {
    let mut f = fixture_with_staging_expiry().await;
    let expiry = f.target_expiry.unwrap();
    let root = tempfile::tempdir().unwrap();
    ready_intent(&mut f, root.path()).await;
    let (motion, _) = names(&f, root.path());
    f.shared.test.source96_pause.store(1, Ordering::Release);
    f.service.begin_selected_source96_restore().unwrap();
    paused_source(&f, 1).await;
    assert!(
        crate::now() < expiry,
        "credential must remain valid at admitted apply boundary"
    );
    while crate::now() < expiry {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!f.shared.cancel.load(Ordering::Acquire));
    release_source(&f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    {
        let phase = f.shared.phase.lock().unwrap();
        let Some(Failure::Error(error)) = &phase.failure else {
            panic!("actual expiry error required")
        };
        assert!(
            format!("{error:#}").contains("credential_expired"),
            "{error:#}"
        );
        assert!(phase.target.as_ref().unwrap().source96_states().0);
    }
    assert_eq!(
        rows(&motion, "catabolic_native_bootstrap_commands").await,
        0
    );
    f.close().await;
}
