//! Actual NativeService startup/stop/export/selection ownership. No fabricated
//! PreparedPair, stopped proof, shutdown receipt or selected binding is used.
use super::*;
use crate::App;
use crate::native_service::{NativeService, publication_file};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
#[path = "staging_tests.rs"]
mod staging_tests;
#[path = "control97_tests.rs"]
mod control97_tests;
#[path = "target_tests.rs"]
mod target_tests;

struct Fixture {
    _root: tempfile::TempDir,
    app: App,
    service: NativeService,
    shared: Arc<Shared>,
    competing: File,
    source_fd: i32,
    pair_fd: i32,
    source_motion_path: PathBuf,
    source_motion_identity: (u64, u64),
    selected: PathBuf,
    staging: PathBuf,
    archive_hash: String,
    target_caller: Option<Caller>,
    target_expiry: Option<i64>,
    initial_selection_timeout: Duration,
}
async fn fixture() -> Fixture {
    fixture_with_target_auth(None).await
}
async fn fixture_with_target_auth(auth: Option<target_tests::AuthCase>) -> Fixture {
    fixture_with_auth(auth, false).await
}
async fn fixture_with_staging_expiry() -> Fixture {
    fixture_with_auth(None, true).await
}
async fn fixture_with_auth(auth: Option<target_tests::AuthCase>, expiring: bool) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let native_path = root.path().join("native.sqlite3");
    motion_catalog_backend::initialize(native_path.clone())
        .await
        .unwrap();
    let db = crate::db::connect_native_bootstrap(&root.path().join("motion.sqlite3"))
        .await
        .unwrap();
    // Record the actual main file of the producing pool before the service
    // retains its writer. This is fixture identity evidence, not a writer FD.
    let source_motion_path = PathBuf::from(
        sqlx::query_scalar::<_, String>(
            "SELECT file FROM pragma_database_list WHERE name='main'",
        )
        .fetch_one(&db)
        .await
        .unwrap(),
    );
    let source_motion_metadata = std::fs::symlink_metadata(&source_motion_path).unwrap();
    assert!(source_motion_metadata.file_type().is_file());
    let source_motion_identity = (source_motion_metadata.dev(), source_motion_metadata.ino());
    let backend = motion_catalog_backend::NativeCatalog::bind(
        db.clone(),
        native_path.clone(),
        motion_catalog_backend::inspect(native_path.clone())
            .await
            .unwrap()
            .identity,
    )
    .await
    .unwrap();
    backend.freeze_motion_source("empty", 0).await.unwrap();
    backend.capture_motion_import("empty").await.unwrap();
    backend.stage_import("empty").await.unwrap();
    backend.prepare_motion_cutover("empty").await.unwrap();
    backend
        .activate_motion_cutover(Duration::from_secs(5))
        .await
        .unwrap();
    drop(backend);
    let native = Arc::new(
        crate::native_catalog::Runtime::open(db.clone(), native_path, "default".into())
            .await
            .unwrap(),
    );
    let state = root.path().join("state");
    let cache = root.path().join("cache");
    let staging = root.path().join("selected-staging");
    let pair_path = root.path().join("pair");
    for path in [&state, &cache, &staging, &pair_path] {
        std::fs::create_dir(path).unwrap();
    }
    let app = App {
        health: Arc::new(crate::operations::Health::new(false)),
        db,
        native_catalog: Some(native),
        admin_token: Arc::new("service-selected-fixture-operator-token".into()),
        origin: Arc::new("http://127.0.0.1:8787".into()),
        authority: Arc::new("127.0.0.1:8787".into()),
        ffprobe: Arc::new("ffprobe".into()),
        jobs: Arc::new(tokio::sync::Mutex::new(())),
        streams: Arc::new(tokio::sync::Semaphore::new(2)),
        event_streams: Arc::new(tokio::sync::Semaphore::new(4)),
        processing: Arc::new(crate::processing::Runtime::new(cache, Default::default())),
        storage: Arc::new(crate::storage::Runtime::new(
            state.clone(),
            Default::default(),
        )),
        access: Arc::new(crate::v2::Runtime::new(
            playscale_core::access::AccessMode::Restricted,
            crate::v2::auth::random_key(),
        )),
    };
    let (target_caller, target_expiry) = if expiring {
        let (caller, expiry) = crate::v2::auth::retained::actual_expiring_admin(&app).await;
        (Some(caller), Some(expiry))
    } else {
        (
            match auth {
                Some(case) => Some(target_tests::prepare_caller(&app, case).await),
                None => None,
            },
            None,
        )
    };
    let lock_path = state.join("server.lock");
    let lock = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .unwrap();
    let source_fd = lock.as_raw_fd();
    let competing = File::options()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let startup = crate::native_service::startup_observation::observe(&app).unwrap();
    let mut service = NativeService::start(
        app.clone(),
        None,
        TcpListener::bind("127.0.0.1:0").await.unwrap(),
        None,
        lock,
        CancellationToken::new(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), startup.wait())
        .await
        .unwrap()
        .unwrap();
    let selected = pair_path.join("selected.pair");
    // A real still-serving service cannot start selection or surrender its lock.
    assert!(
        service
            .begin_selected_pair_restore(
                selected.clone(),
                staging.clone(),
                Instant::now() + Duration::from_secs(60)
            )
            .is_err()
    );
    assert!(fs2::FileExt::try_lock_exclusive(&competing).is_err());
    service.stop_for_restore().await.unwrap();
    service
        .prepare_pair_for_restore(pair_path.clone(), Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    let shared = service.restore.shared.as_ref().unwrap().clone();
    let pair_fd = shared
        .phase
        .lock()
        .unwrap()
        .pair
        .as_ref()
        .unwrap()
        .motion
        .as_raw_fd();
    // Fixture-only archive production BORROWS the genuinely exported pair in
    // its actual service slot. It never uses take_pair_for_fixture or creates a
    // replacement current owner. The production container and publisher verify
    // every byte and manifest commitment before making this selected archive.
    let retained = shared.clone();
    tokio::task::spawn_blocking(move || {
        let mut phase = retained.phase.lock().unwrap();
        let pair = phase.pair.as_mut().unwrap();
        pair._stopped.verify_lock().unwrap();
        verify_file(
            &mut pair.motion,
            &pair.manifest.motion_database,
            &retained.cancel,
        )
        .unwrap();
        pair.motion.seek(SeekFrom::Start(0)).unwrap();
        let expected = pair.manifest.clone();
        let stage = publication_file::Sealed::create_checked(
            &pair_path,
            "selected.pair",
            &retained.cancel,
            Some(pair.parent_identity),
            |output| {
                crate::native_service::pair_container::write(
                    output,
                    &pair.manifest,
                    &retained.cancel,
                    |out| {
                        std::io::copy(&mut pair.motion, out)?;
                        Ok(())
                    },
                    |mut out| {
                        pair.native.copy_verified_to(&mut out, &retained.cancel)?;
                        Ok(())
                    },
                )
            },
            |file| {
                crate::native_service::pair_container::read_expected(
                    file,
                    &expected,
                    &retained.cancel,
                )
            },
        )
        .unwrap();
        let publication = stage.publish(&retained.cancel, || pair._stopped.verify_lock());
        assert_eq!(
            publication.phase(),
            playscale_core::paired_backup::publication::Phase::Durable
        );
    })
    .await
    .unwrap();
    let archive_hash = catabolic_core::sha256(std::fs::read(&selected).unwrap());
    Fixture {
        _root: root,
        app,
        service,
        shared,
        competing,
        source_fd,
        pair_fd,
        source_motion_path,
        source_motion_identity,
        selected,
        staging,
        archive_hash,
        target_caller,
        target_expiry,
        initial_selection_timeout: Duration::from_secs(120),
    }
}
impl Fixture {
    fn begin(&mut self, deadline: Instant) {
        self.service
            .begin_selected_pair_restore(self.selected.clone(), self.staging.clone(), deadline)
            .unwrap();
        // Both values were measured from the genuine future/closure before
        // either spawn. Their named shares were admitted by this service's
        // actual native export; JoinHandle/struct layout is not substituted.
        let deadline_frame = self
            .shared
            .test
            .selection_deadline_frame
            .load(Ordering::Acquire);
        let worker_frame = self
            .shared
            .test
            .selection_worker_frame
            .load(Ordering::Acquire);
        assert!(deadline_frame > 0 && deadline_frame <= SELECTION_DEADLINE_FRAME_LIMIT);
        assert!(worker_frame > 0 && worker_frame <= SELECTION_WORKER_FRAME_LIMIT);
        assert!(self.shared.storage >= SERVICE_INPUT_PATH_STORAGE + SELECTION_TASK_STORAGE);
    }
    fn assert_current(&self) {
        assert!(fs2::FileExt::try_lock_exclusive(&self.competing).is_err());
        assert!(self.app.processing.io.clone().try_acquire_owned().is_err());
        assert_eq!(
            self.shared
                .stopped
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lock
                .file
                .as_raw_fd(),
            self.source_fd
        );
    }
    fn assert_phase(&self, selected: bool, bound: bool) {
        self.assert_current();
        let phase = self.shared.phase.lock().unwrap();
        assert_eq!(
            phase.pair.as_ref().unwrap().motion.as_raw_fd(),
            self.pair_fd
        );
        let selection = phase.selection.as_ref().unwrap();
        let (evidence, actual_bound) = selection.evidence_for_fixture();
        assert_eq!(evidence.is_some(), selected);
        assert_eq!(actual_bound, bound);
        if let Some((file, digest)) = evidence {
            assert_eq!(digest, self.archive_hash);
            assert!(file.metadata().unwrap().is_file());
        }
        assert!(phase.image.is_none());
    }
    fn assert_no_retry(&mut self) {
        assert!(
            self.service
                .begin_selected_pair_restore(
                    self.selected.clone(),
                    self.staging.clone(),
                    Instant::now() + Duration::from_secs(60)
                )
                .is_err()
        );
    }
    async fn close(&mut self) {
        self.service.close_restore().await.unwrap();
        assert_eq!(self.shared.status.load(Ordering::Acquire), CLOSED);
        assert!(self.shared.phase.lock().unwrap().selection.is_none());
        assert!(self.shared.phase.lock().unwrap().pair.is_none());
        assert!(self.shared.stopped.lock().unwrap().is_none());
        assert!(self.app.db.is_closed());
        assert!(self.app.processing.io.clone().try_acquire_owned().is_ok());
        fs2::FileExt::try_lock_exclusive(&self.competing).unwrap();
        fs2::FileExt::unlock(&self.competing).unwrap();
        assert!(
            self.app
                .native_catalog
                .as_ref()
                .unwrap()
                .with_host(|_| Ok(()))
                .await
                .is_err()
        );
        assert_eq!(
            catabolic_core::sha256(std::fs::read(&self.selected).unwrap()),
            self.archive_hash
        );
        assert_eq!(std::fs::read_dir(&self.staging).unwrap().count(), 0);
    }
}
async fn reached(shared: &Shared, stage: u8) {
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if shared.test.selection_reached.load(Ordering::Acquire) == stage {
            return;
        }
        assert_eq!(
            shared.status.load(Ordering::Acquire),
            RUNNING,
            "actual selection failed before test boundary"
        );
        changed.await;
    }
}
async fn cancelled_by_deadline(shared: &Shared) {
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if shared.cancel.load(Ordering::Acquire) {
            return;
        }
        changed.await;
    }
}

#[tokio::test]
async fn actual_service_selected_unpolled_waiter_and_no_retry() {
    let mut f = fixture().await;
    f.begin(Instant::now() + Duration::from_secs(60));
    // Dropping an unpolled future cannot undo the synchronous transition.
    drop(f.service.wait_selected_pair_restore());
    f.service.wait_selected_pair_restore().await.unwrap();
    assert!(!f.shared.cancel.load(Ordering::Acquire));
    f.assert_phase(true, true);
    f.assert_no_retry();
    f.close().await;
}

#[tokio::test]
async fn actual_service_selected_abandoned_waiter_retains_exact_owners() {
    use std::future::Future;
    let mut f = fixture().await;
    f.shared.test.selection_pause.store(2, Ordering::Release);
    f.begin(Instant::now() + Duration::from_secs(60));
    reached(&f.shared, 2).await;
    {
        let mut waiter = std::pin::pin!(f.service.wait_selected_pair_restore());
        std::future::poll_fn(|cx| {
            assert!(waiter.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }
    assert!(f.shared.cancel.load(Ordering::Acquire));
    assert!(f.service.restore.worker.is_some() && f.service.restore.selection_timer.is_some());
    f.assert_current();
    let selected_fd = f.shared.test.selection_fd.load(Ordering::Acquire);
    assert!(selected_fd >= 0);
    f.shared.test.selection_release.notify_one();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(true, false);
    assert_eq!(
        f.shared
            .phase
            .lock()
            .unwrap()
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
    f.assert_no_retry();
    f.close().await;
}

#[test]
fn actual_service_selected_queued_expiry_keeps_original_pair() {
    // Keep genuine fixture acquisition and terminal cleanup on their normal
    // runtime. Cleanup itself awaits another blocking worker; forcing that
    // whole lifecycle onto one lane would deadlock independently of selection.
    let fixture_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut f = fixture_runtime.block_on(fixture());
    // Only selection is constrained: occupy its actual sole blocking lane,
    // rather than using an already-expired token or synthetic worker future.
    let selection_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    // An assertion failure must release the deliberate blocker before either
    // runtime drops; keep the actual queued JoinHandle, not a detached substitute.
    struct ReleaseBlocker(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseBlocker {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    selection_runtime.block_on(async {
        let (release, blocked) = std::sync::mpsc::channel();
        let mut release = ReleaseBlocker(Some(release));
        let (entered, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
        });
        ready.await.unwrap();
        f.begin(Instant::now() + Duration::from_millis(50));
        cancelled_by_deadline(&f.shared).await;
        assert_eq!(f.shared.status.load(Ordering::Acquire), RUNNING);
        assert_eq!(f.shared.selection_storage.load(Ordering::Acquire), 0);
        f.assert_phase(false, false);
        release.0.take().unwrap().send(()).unwrap();
        blocker.await.unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        f.assert_phase(false, false);
        f.assert_no_retry();
    });
    // The original runtime and FreshWriter handle remain live throughout. All
    // selection work is joined by service close before either runtime is dropped.
    fixture_runtime.block_on(f.close());
}

#[tokio::test]
async fn actual_service_selected_active_worker_expiry_keeps_original_pair() {
    let mut f = fixture().await;
    f.shared.test.selection_pause.store(1, Ordering::Release);
    f.begin(Instant::now() + Duration::from_millis(100));
    reached(&f.shared, 1).await;
    cancelled_by_deadline(&f.shared).await;
    f.assert_current();
    f.shared.test.selection_release.notify_one();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(false, false);
    f.assert_no_retry();
    f.close().await;
}

#[tokio::test]
async fn actual_service_selected_path_replacement_refuses_without_losing_current() {
    let mut f = fixture().await;
    f.shared.test.selection_pause.store(2, Ordering::Release);
    f.begin(Instant::now() + Duration::from_secs(60));
    reached(&f.shared, 2).await;
    let held_path = f.selected.with_file_name("retained-selected.pair");
    let original = std::fs::metadata(&f.selected).unwrap();
    std::fs::rename(&f.selected, &held_path).unwrap();
    std::fs::copy(&held_path, &f.selected).unwrap();
    f.assert_current();
    f.shared.test.selection_release.notify_one();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert!(!f.shared.cancel.load(Ordering::Acquire));
    f.assert_phase(true, false);
    {
        let phase = f.shared.phase.lock().unwrap();
        let evidence = phase
            .selection
            .as_ref()
            .unwrap()
            .evidence_for_fixture()
            .0
            .unwrap()
            .0
            .metadata()
            .unwrap();
        assert_eq!(
            (evidence.dev(), evidence.ino()),
            (original.dev(), original.ino())
        );
        assert_ne!(
            evidence.ino(),
            std::fs::metadata(&f.selected).unwrap().ino()
        );
    }
    f.assert_no_retry();
    f.close().await;
}

#[tokio::test]
async fn actual_service_selected_panic_retains_exact_selected_phase() {
    let mut f = fixture().await;
    f.shared.test.selection_pause.store(2, Ordering::Release);
    f.shared.test.selection_panic.store(true, Ordering::Release);
    f.begin(Instant::now() + Duration::from_secs(60));
    reached(&f.shared, 2).await;
    f.shared.test.selection_release.notify_one();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    f.assert_phase(true, false);
    assert!(matches!(
        f.shared.phase.lock().unwrap().failure,
        Some(Failure::Panic(_))
    ));
    f.assert_no_retry();
    f.close().await;
}

#[tokio::test]
async fn actual_service_selected_path_share_refuses_before_handoff() {
    let mut f = fixture().await;
    let mut oversized = f.selected.clone();
    oversized.reserve_exact(usize::try_from(SERVICE_INPUT_PATH_STORAGE).unwrap() + 1);
    assert!(
        f.service
            .begin_selected_pair_restore(
                oversized,
                f.staging.clone(),
                Instant::now() + Duration::from_secs(60)
            )
            .is_err()
    );
    assert!(
        f.service
            .begin_selected_pair_restore(f.selected.clone(), f.staging.clone(), Instant::now())
            .is_err()
    );
    assert_eq!(f.shared.status.load(Ordering::Acquire), IDLE);
    assert!(f.shared.phase.lock().unwrap().selection.is_none());
    assert!(f.service.restore.worker.is_none() && f.service.restore.selection_timer.is_none());
    f.assert_current();
    f.begin(Instant::now() + Duration::from_secs(60));
    f.service.wait_selected_pair_restore().await.unwrap();
    f.assert_phase(true, true);
    f.close().await;
}

#[tokio::test]
async fn actual_service_selected_close_refusal_retains_all_available_owners() {
    let mut f = fixture().await;
    f.begin(Instant::now() + Duration::from_secs(60));
    f.service.wait_selected_pair_restore().await.unwrap();
    f.shared
        .test
        .selection_close_failure
        .store(true, Ordering::Release);
    assert!(f.service.close_restore().await.is_err());
    assert_eq!(f.shared.status.load(Ordering::Acquire), FAILED);
    f.assert_phase(true, true);
    assert!(f.shared.phase.lock().unwrap().close_failure.is_some());
    f.assert_no_retry();
    assert!(f.service.close_restore().await.is_err());
    f.assert_phase(true, true);
    // Fixture-only disposal of our injected refusal. Production close remains
    // nonretryable; this directly exercises retained cleanup after removing the
    // test hook and does not certify any recovery or Drop path for real errors.
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
    assert!(f.app.db.is_closed());
    fs2::FileExt::try_lock_exclusive(&f.competing).unwrap();
    fs2::FileExt::unlock(&f.competing).unwrap();
}
