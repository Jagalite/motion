//! Genuine startup/stop/export/selected-binding/target-worker seam. Tests never
//! fabricate a stopped proof, workspace admission, current token or target grant.
use super::super::target_reservation::{self, TargetPhase};
use super::*;
use axum::{
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Clone, Copy)]
pub(super) enum AuthCase {
    Denied,
    Revoked,
}
async fn request(
    app: &App,
    method: &str,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
    etag: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", app.authority.as_str())
        .header("content-type", "application/json");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(etag) = etag {
        builder = builder.header("if-match", etag);
    }
    if method == "POST" && path.ends_with("/approve") {
        builder = builder.header(
            "idempotency-key",
            format!("target-fixture-{}", uuid::Uuid::new_v4()),
        );
    }
    let response = crate::api::router(app.clone(), None)
        .oneshot(
            builder
                .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, headers, value)
}
pub(super) async fn prepare_caller(app: &App, case: AuthCase) -> Caller {
    match case {
        AuthCase::Revoked => {
            let caller = crate::v2::auth::retained::actual_paired_admin(app).await;
            // Keep the real last-admin guard active; do not bypass it with SQL.
            let _reserve = crate::v2::auth::retained::actual_paired_admin(app).await;
            let path = format!(
                "/api/v2/devices/{}",
                caller.principal.device_id.as_ref().unwrap()
            );
            let (status, headers, body) =
                request(app, "GET", &path, None, Some(&app.admin_token), None).await;
            assert_eq!(status, StatusCode::OK, "{body:?}");
            let (status, _, body) = request(
                app,
                "DELETE",
                &path,
                None,
                Some(&app.admin_token),
                Some(headers["etag"].to_str().unwrap()),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body:?}");
            caller
        }
        AuthCase::Denied => {
            let (status, _, pairing) = request(app, "POST", "/api/v2/auth/pairings",
                Some(json!({"device_name":"Target read-only caller","client_name":"actual-service-target"})), None, None).await;
            assert_eq!(status, StatusCode::CREATED, "{pairing:?}");
            let id = pairing["id"].as_str().unwrap();
            let (status, _, approval) = request(app, "POST", &format!("/api/v2/auth/pairings/{id}/approve"),
                Some(json!({"user_code":pairing["user_code"],"profile_ids":[],"permissions":["catalog:read"]})), Some(&app.admin_token), None).await;
            assert_eq!(status, StatusCode::OK, "{approval:?}");
            let (status, _, claimed) = request(
                app,
                "POST",
                &format!("/api/v2/auth/pairings/{id}/claim"),
                Some(json!({"device_code":pairing["device_code"]})),
                None,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{claimed:?}");
            let mut headers = HeaderMap::new();
            headers.insert(
                "authorization",
                format!("Bearer {}", claimed["access_token"].as_str().unwrap())
                    .parse()
                    .unwrap(),
            );
            crate::v2::auth::resolve(app, &Method::GET, &headers)
                .await
                .unwrap()
        }
    }
}
async fn operator(f: &Fixture) -> Caller {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", f.app.admin_token).parse().unwrap(),
    );
    crate::v2::auth::resolve(&f.app, &Method::GET, &headers)
        .await
        .unwrap()
}
async fn selected(f: &mut Fixture) {
    f.begin(Instant::now() + Duration::from_secs(120));
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_pair_for_target().await.unwrap();
    f.assert_phase(true, true);
}
async fn target_reached(shared: &Shared, stage: u8) {
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if shared.test.target_reached.load(Ordering::Acquire) == stage {
            return;
        }
        assert_eq!(shared.status.load(Ordering::Acquire), RUNNING);
        changed.await;
    }
}
fn release_target(shared: &Shared) {
    *shared.test.target_released.lock().unwrap() = true;
    shared.test.target_release.notify_one();
}
fn assert_no_images(path: &Path) {
    for name in [
        "playscale.sqlite3",
        "catabolic.sqlite3",
        "paired-install.conversion.json",
        "paired-install.intent.json",
        "paired-install.complete.json",
    ] {
        assert!(
            !path.join(name).exists(),
            "unexpected conversion/install effect: {name}"
        );
    }
}

#[tokio::test]
async fn actual_service_target_success_retains_same_pair_selection_and_workspace() {
    let mut f = fixture().await;
    // Real initial export paid fixed host slots, not the unused auth cohort.
    assert_eq!(f.shared.authorization_storage.load(Ordering::Acquire), 0);
    assert!(f.shared.storage < AuthorizationWorkspace::service_storage_reservation());
    {
        let phase = f.shared.phase.lock().unwrap();
        assert_eq!(
            phase
                .pair
                .as_ref()
                .unwrap()
                ._stopped
                .retained_restore_storage()
                .unwrap(),
            f.shared.storage
        );
    }
    selected(&mut f).await;
    let high_water = f.shared.selection_storage.load(Ordering::Acquire);
    let before_storage = f
        .shared
        .phase
        .lock()
        .unwrap()
        .pair
        .as_ref()
        .unwrap()
        ._stopped
        .retained_restore_storage()
        .unwrap();
    assert_eq!(before_storage, f.shared.storage + high_water);
    assert_eq!(f.shared.authorization_storage.load(Ordering::Acquire), 0);
    let caller = operator(&f).await;
    let target = tempfile::tempdir().unwrap();
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    drop(f.service.wait_selected_pair_restore()); // unpolled waiter owns nothing
    f.service.wait_selected_pair_restore().await.unwrap();
    f.assert_phase(true, true);
    assert_eq!(
        f.shared.selection_storage.load(Ordering::Acquire),
        high_water
    );
    assert_eq!(
        f.shared.authorization_storage.load(Ordering::Acquire),
        AuthorizationWorkspace::service_storage_reservation()
    );
    assert!(f.shared.test.target_async_frame.load(Ordering::Acquire) > 0);
    assert!(
        f.shared.test.target_async_frame.load(Ordering::Acquire)
            <= target_reservation::ASYNC_FRAME_LIMIT
    );
    {
        let phase = f.shared.phase.lock().unwrap();
        assert_eq!(
            phase
                .pair
                .as_ref()
                .unwrap()
                ._stopped
                .retained_restore_storage()
                .unwrap(),
            before_storage + AuthorizationWorkspace::service_storage_reservation()
        );
        let target_owner = phase.target.as_ref().unwrap();
        assert_eq!(target_owner.phase, TargetPhase::Reserved);
        let (lock, reservation) = target_owner.evidence();
        assert_eq!(
            reservation.unwrap().phase(),
            playscale_core::paired_backup::publication::Phase::Durable
        );
        assert!(phase.workspace.as_ref().unwrap().reusable());
        phase
            .workspace
            .as_ref()
            .unwrap()
            .verify_service(&phase.pair.as_ref().unwrap()._stopped)
            .unwrap();
        assert_eq!(
            lock.unwrap().metadata().unwrap().ino(),
            std::fs::metadata(target.path().join("server.lock"))
                .unwrap()
                .ino()
        );
        assert!(
            crate::native_service::require_complete_pair_install(
                target.path(),
                &target.path().join("catabolic.sqlite3"),
                lock.unwrap()
            )
            .is_err()
        );
    }
    let marker: Value =
        serde_json::from_slice(&std::fs::read(target.path().join("server.lock")).unwrap()).unwrap();
    let bytes = std::fs::read(target.path().join("paired-install.reservation.json")).unwrap();
    let reservation: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(marker["install_id"], reservation["install_id"]);
    assert_eq!(marker["reservation_sha256"], catabolic_core::sha256(bytes));
    assert_eq!(reservation["pair_sha256"], f.archive_hash);
    assert_no_images(target.path());
    let caller = operator(&f).await;
    assert!(
        f.service
            .begin_selected_target_restore(target.path().into(), caller)
            .is_err()
    );
    assert_eq!(
        f.shared.authorization_storage.load(Ordering::Acquire),
        AuthorizationWorkspace::service_storage_reservation()
    );
    assert_eq!(
        f.shared
            .phase
            .lock()
            .unwrap()
            .pair
            .as_ref()
            .unwrap()
            ._stopped
            .retained_restore_storage()
            .unwrap(),
        before_storage + AuthorizationWorkspace::service_storage_reservation()
    );
    f.close().await;
    assert!(target.path().join("server.lock").exists());
    assert!(
        target
            .path()
            .join("paired-install.reservation.json")
            .exists()
    );
}

#[tokio::test]
async fn actual_service_target_retirement_drop_never_starts_target_or_loses_handles() {
    use std::future::Future;
    let mut f = fixture().await;
    f.shared
        .test
        .selection_join_pause
        .store(true, Ordering::Release);
    f.begin(Instant::now() + Duration::from_secs(120));
    f.service.wait_selected_pair_restore().await.unwrap();
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.test.selection_join_reached.load(Ordering::Acquire) {
            break;
        }
        changed.await;
    }
    drop(f.service.settle_selected_pair_for_target());
    {
        let mut retirement = std::pin::pin!(f.service.settle_selected_pair_for_target());
        std::future::poll_fn(|cx| {
            assert!(retirement.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert!(f.service.restore.worker.is_some());
    assert!(f.service.restore.selection_timer.is_some());
    assert_eq!(f.shared.status.load(Ordering::Acquire), COMPLETE);
    assert!(f.shared.phase.lock().unwrap().target.is_none());
    assert!(!f.shared.cancel.load(Ordering::Acquire));
    let target = tempfile::tempdir().unwrap();
    let caller = operator(&f).await;
    assert!(
        f.service
            .begin_selected_target_restore(target.path().into(), caller)
            .is_err()
    );
    *f.shared.test.selection_join_released.lock().unwrap() = true;
    f.shared.test.selection_join_release.notify_one();
    f.service.settle_selected_pair_for_target().await.unwrap();
    let caller = operator(&f).await;
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn actual_service_target_abandoned_waiter_retains_anonymous_and_partial_named_owners() {
    use std::future::Future;
    for stage in [1, 2, 3] {
        let mut f = fixture().await;
        selected(&mut f).await;
        let target = tempfile::tempdir().unwrap();
        f.shared.test.target_pause.store(stage, Ordering::Release);
        let caller = operator(&f).await;
        f.service
            .begin_selected_target_restore(target.path().into(), caller)
            .unwrap();
        target_reached(&f.shared, stage).await;
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
        f.assert_current();
        release_target(&f.shared);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        f.assert_phase(true, true);
        {
            let phase = f.shared.phase.lock().unwrap();
            let (lock, reservation) = phase.target.as_ref().unwrap().evidence();
            assert!(lock.is_some() && reservation.is_some());
            assert_eq!(
                lock.unwrap().metadata().unwrap().nlink(),
                if stage == 1 { 0 } else { 1 }
            );
        }
        assert_eq!(target.path().join("server.lock").exists(), stage >= 2);
        assert_eq!(
            target
                .path()
                .join("paired-install.reservation.json")
                .exists(),
            stage == 3
        );
        assert_no_images(target.path());
        f.close().await;
        assert_eq!(target.path().join("server.lock").exists(), stage >= 2);
    }
}

#[tokio::test]
async fn actual_service_target_panic_keeps_marked_lock_and_retires_real_auth_reader() {
    let mut f = fixture().await;
    selected(&mut f).await;
    let target = tempfile::tempdir().unwrap();
    f.shared.test.target_pause.store(2, Ordering::Release);
    f.shared.test.target_panic.store(true, Ordering::Release);
    let caller = operator(&f).await;
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    target_reached(&f.shared, 2).await;
    f.assert_current();
    release_target(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(true, true);
    {
        let phase = f.shared.phase.lock().unwrap();
        assert!(matches!(phase.failure, Some(Failure::Panic(_))));
        assert!(phase.workspace.as_ref().unwrap().reusable());
        let (lock, reservation) = phase.target.as_ref().unwrap().evidence();
        assert_eq!(lock.unwrap().metadata().unwrap().nlink(), 1);
        assert_eq!(
            reservation.unwrap().phase(),
            playscale_core::paired_backup::publication::Phase::Ready
        );
    }
    assert!(target.path().join("server.lock").exists());
    assert!(
        !target
            .path()
            .join("paired-install.reservation.json")
            .exists()
    );
    f.close().await;
}

#[tokio::test]
async fn actual_service_target_uncertain_reservation_retains_exact_named_inode() {
    let mut f = fixture().await;
    selected(&mut f).await;
    let target = tempfile::tempdir().unwrap();
    f.shared
        .test
        .target_reservation_sync_failure
        .store(true, Ordering::Release);
    let caller = operator(&f).await;
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(true, true);
    let named = std::fs::metadata(target.path().join("paired-install.reservation.json")).unwrap();
    {
        let phase = f.shared.phase.lock().unwrap();
        let (_, reservation) = phase.target.as_ref().unwrap().evidence();
        let reservation = reservation.unwrap();
        assert_eq!(
            reservation.phase(),
            playscale_core::paired_backup::publication::Phase::LinkedUnconfirmed
        );
        assert_eq!(reservation.evidence().file_inode, named.ino());
    }
    assert_no_images(target.path());
    f.close().await;
    assert_eq!(
        std::fs::metadata(target.path().join("paired-install.reservation.json"))
            .unwrap()
            .ino(),
        named.ino()
    );
}

#[tokio::test]
async fn actual_service_target_refuses_ancestry_sidecar_and_selected_substitution() {
    for case in 0..4 {
        let mut f = fixture().await;
        selected(&mut f).await;
        let target = tempfile::tempdir().unwrap();
        let path = match case {
            0 => f._root.path().to_owned(),
            1 => {
                std::fs::write(
                    target.path().join("playscale.sqlite3-journal"),
                    b"unrelated",
                )
                .unwrap();
                target.path().into()
            }
            2 => {
                let held = f.selected.with_file_name("held-selected.pair");
                std::fs::rename(&f.selected, &held).unwrap();
                std::fs::copy(&held, &f.selected).unwrap();
                target.path().into()
            }
            _ => {
                let alias = target.path().join("alias");
                std::os::unix::fs::symlink(f._root.path(), &alias).unwrap();
                alias
            }
        };
        let caller = operator(&f).await;
        f.service
            .begin_selected_target_restore(path.clone(), caller)
            .unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        f.assert_phase(true, true);
        assert!(!path.join("server.lock").exists());
        assert!(!path.join("paired-install.reservation.json").exists());
        assert_no_images(target.path());
        f.close().await;
        if case == 1 {
            assert_eq!(
                std::fs::read(target.path().join("playscale.sqlite3-journal")).unwrap(),
                b"unrelated"
            );
        }
    }
}

#[tokio::test]
async fn actual_service_target_parent_replacement_refuses_after_anonymous_staging() {
    let mut f = fixture().await;
    selected(&mut f).await;
    let container = tempfile::tempdir().unwrap();
    let target = container.path().join("target");
    let held = container.path().join("held");
    std::fs::create_dir(&target).unwrap();
    f.shared.test.target_pause.store(1, Ordering::Release);
    let caller = operator(&f).await;
    f.service
        .begin_selected_target_restore(target.clone(), caller)
        .unwrap();
    target_reached(&f.shared, 1).await;
    std::fs::rename(&target, &held).unwrap();
    std::fs::create_dir(&target).unwrap();
    release_target(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(true, true);
    for path in [&target, &held] {
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    }
    f.close().await;
}

#[tokio::test]
async fn actual_service_target_current_denial_and_revocation_precede_all_named_effects() {
    for case in [AuthCase::Denied, AuthCase::Revoked] {
        let mut f = fixture_with_target_auth(Some(case)).await;
        selected(&mut f).await;
        let target = tempfile::tempdir().unwrap();
        let caller = f.target_caller.take().unwrap();
        f.service
            .begin_selected_target_restore(target.path().into(), caller)
            .unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        f.assert_phase(true, true);
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
        {
            let phase = f.shared.phase.lock().unwrap();
            assert_eq!(
                phase.target.as_ref().unwrap().phase,
                TargetPhase::Authenticating
            );
            assert!(!phase.workspace.as_ref().unwrap().reusable());
            assert!(
                phase
                    .pair
                    .as_ref()
                    .unwrap()
                    ._writer
                    .retains_authorization_reader()
            );
            assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
        }
        // Denied prepare ended only its borrow. Explicit service close now
        // joins that exact retained reader and original writer, without ever
        // claiming workspace reuse, retrying auth or publishing target files.
        f.assert_current();
        f.close().await;
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn actual_service_target_original_deadline_cancels_before_lock_link() {
    let mut f = fixture().await;
    let deadline = Instant::now() + Duration::from_secs(30);
    f.begin(deadline);
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service.settle_selected_pair_for_target().await.unwrap();
    let target = tempfile::tempdir().unwrap();
    f.shared.test.target_pause.store(1, Ordering::Release);
    let caller = operator(&f).await;
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    target_reached(&f.shared, 1).await;
    // No request waiter owns the deadline or target continuation. The inherited
    // selection deadline, not a refreshed target window, drives this timer.
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.cancel.load(Ordering::Acquire) {
            break;
        }
        changed.await;
    }
    assert_eq!(f.shared.status.load(Ordering::Acquire), RUNNING);
    release_target(&f.shared);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(true, true);
    assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
    {
        let phase = f.shared.phase.lock().unwrap();
        let (lock, reservation) = phase.target.as_ref().unwrap().evidence();
        assert_eq!(lock.unwrap().metadata().unwrap().nlink(), 0);
        assert!(reservation.is_some());
        assert!(!phase.workspace.as_ref().unwrap().reusable());
    }
    // Close joins the actual already installed retirement frame despite expiry;
    // it does not manufacture workspace reuse or erase named admission.
    let charged = f.shared.authorization_storage.load(Ordering::Acquire);
    f.close().await;
    assert!(charged > 0);
}

#[tokio::test]
async fn actual_service_target_workspace_rejects_another_real_service() {
    let mut first = fixture().await;
    selected(&mut first).await;
    let target = tempfile::tempdir().unwrap();
    let caller = operator(&first).await;
    first
        .service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    first.service.wait_selected_pair_restore().await.unwrap();
    let mut second = fixture().await;
    {
        let first_phase = first.shared.phase.lock().unwrap();
        let second_phase = second.shared.phase.lock().unwrap();
        assert!(
            first_phase
                .workspace
                .as_ref()
                .unwrap()
                .verify_service(&second_phase.pair.as_ref().unwrap()._stopped)
                .is_err()
        );
        assert_eq!(
            second.shared.authorization_storage.load(Ordering::Acquire),
            0
        );
        assert!(second_phase.target.is_none());
        assert!(second_phase.workspace.is_none());
    }
    first.close().await;
    second.close().await;
}

#[tokio::test]
async fn actual_service_target_close_failure_keeps_named_and_original_custody() {
    let mut f = fixture().await;
    selected(&mut f).await;
    let target = tempfile::tempdir().unwrap();
    let caller = operator(&f).await;
    f.service
        .begin_selected_target_restore(target.path().into(), caller)
        .unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    let named = std::fs::metadata(target.path().join("server.lock"))
        .unwrap()
        .ino();
    f.shared
        .test
        .selection_close_failure
        .store(true, Ordering::Release);
    assert!(f.service.close_restore().await.is_err());
    f.assert_phase(true, true);
    {
        let phase = f.shared.phase.lock().unwrap();
        assert!(phase.close_failure.is_some());
        let (lock, reservation) = phase.target.as_ref().unwrap().evidence();
        assert_eq!(lock.unwrap().metadata().unwrap().ino(), named);
        assert_eq!(
            reservation.unwrap().phase(),
            playscale_core::paired_backup::publication::Phase::Durable
        );
        assert!(phase.workspace.is_some());
    }
    assert_eq!(f.shared.status.load(Ordering::Acquire), FAILED);
    assert!(
        f.shared
            .stopped
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .native
            .is_some()
    );
    assert_eq!(
        std::fs::metadata(target.path().join("server.lock"))
            .unwrap()
            .ino(),
        named
    );
    // This deliberately failed close is terminal. Dropping the service below
    // does not assert async cleanup; service destruction remains unqualified.
}
