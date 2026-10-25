//! Genuine service-owned94 fixtures, authored source-only and not executed.
//! Every case starts a real service, scans an actual physical source, stops and
//! exports that exact service, and follows retained97/96 into the same94 owner.
//! No legacy actor or process-global recovery sink participates in these cases.
use super::*;
use motion_catalog_backend::{
    SourceRelocation94CommitState as Commit94, SourceRelocation94Request,
    source_lifecycle::LifecycleProof,
};

struct Physical94 {
    fixture: Fixture,
    expected: LifecycleProof,
    old: PathBuf,
    candidate: PathBuf,
    requester: Caller,
    reserve_admin: Caller,
}

// Same public-source producer and packaged observer as the genuine mode7 pair
// fixture. No direct native SQL writes, replacement UUID policy, or synthetic
// source/session grants are used to make a candidate qualify.
async fn observe_physical_source(app: &App, root: &Path) -> (PathBuf, PathBuf, String) {
    use tower::ServiceExt;
    let old = root.join("observed-source");
    let candidate = root.join("relocated-observed-source");
    std::fs::create_dir(&old).unwrap();
    std::fs::create_dir(&candidate).unwrap();
    std::fs::write(old.join("one.txt"), b"actual pair observation").unwrap();
    std::fs::copy(old.join("one.txt"), candidate.join("one.txt")).unwrap();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v2/sources")
        .header("host", app.authority.as_str())
        .header("authorization", format!("Bearer {}", app.admin_token))
        .header("idempotency-key", "service94-public-source")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({
                "name":"Service94 observed source", "root_path":old, "exclusions":[]
            })
            .to_string(),
        ))
        .unwrap();
    let response = crate::api::router(app.clone(), None)
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        status,
        axum::http::StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let source_id = body["id"].as_str().unwrap().to_owned();
    let binary = PathBuf::from(
        std::env::var_os("CATABOLIC_HOST_TEST_HELPER")
            .expect("actual packaging-verified helper required"),
    );
    let artifact =
        catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
            &binary,
            &catabolic_core::sha256(std::fs::read(&binary).unwrap()),
            catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
        )
        .unwrap();
    app.native_catalog
        .as_ref()
        .unwrap()
        .register_observer(artifact)
        .await
        .unwrap();
    crate::native_discovery::scan(app, &source_id, CancellationToken::new())
        .await
        .unwrap();
    (old, candidate, source_id)
}

async fn physical_fixture(expiring: bool) -> Physical94 {
    physical_fixture_with_capture_expiry(expiring, false).await
}
async fn physical_native_capture_expiry_fixture() -> Physical94 {
    physical_fixture_with_capture_expiry(true, true).await
}
async fn physical_fixture_with_capture_expiry(expiring: bool, capture_expiry: bool) -> Physical94 {
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
    let (old, candidate, source_id) = observe_physical_source(&app, root.path()).await;
    // Both real credentials precede the immutable capture. The reserve admin
    // exists only for the supported target-policy withdrawal fixture.
    let reserve_admin = crate::v2::auth::retained::actual_paired_admin(&app).await;
    // Non-expiring fixtures keep their original issuance order. The expiry
    // fixture delays only its own genuine credential until immediately before
    // stop; neither a captured target nor an authorized caller is replaced.
    let requester = if expiring {
        None
    } else {
        Some(crate::v2::auth::retained::actual_paired_admin(&app).await)
    };
    let mut proof_writer = app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let expected =
        motion_catalog_backend::source_lifecycle::require_active_in(&mut proof_writer, &source_id)
            .await
            .unwrap();
    proof_writer.rollback().await.unwrap();
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
    // Complete genuine public pairing/approval and child issuance while native
    // admission remains open. Pairing approval itself consults the native host.
    // This exact credential must precede stop and the immutable pair capture.
    let (requester, target_expiry) = match requester {
        Some(caller) => (caller, None),
        None => {
            let (caller, expiry) = if capture_expiry {
                crate::v2::auth::retained::actual_expiring_admin_for_native_capture(&app).await
            } else {
                crate::v2::auth::retained::actual_expiring_admin_for_source94(&app).await
            };
            (caller, Some(expiry))
        }
    };
    let target_caller = Some(requester.clone());
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
    let fixture = Fixture {
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
        // Fixed before initial selection. Never reset at97/96/94/capture handoff.
        initial_selection_timeout: Duration::from_secs(if capture_expiry { 240 } else { 120 }),
    };
    Physical94 {
        fixture,
        expected,
        old,
        candidate,
        requester,
        reserve_admin,
    }
}

async fn ready94(f: &mut Fixture, path: &Path) {
    failure_diagnostics::enable(&f.shared);
    ready97(f, path).await;
    // Keep the inherited staged helper's original absolute deadline. Neither
    // source96 nor its positively joined94 handoff obtains a fresh time budget.
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
    f.service.begin_selected_native96_restore().unwrap();
    f.service.wait_selected_pair_restore().await.unwrap();
    f.service
        .settle_selected_native96_for_source96()
        .await
        .unwrap();
    f.service.begin_selected_source96_restore().unwrap();
    f.service
        .wait_selected_pair_restore()
        .await
        .inspect_err(|_| failure_diagnostics::report(&f.shared, "source94 source96 prerequisite"))
        .unwrap();
    // Joining source96's worker and timer is not inferred from success status.
    drop(f.service.settle_selected_source96_for_relocation94());
    assert!(f.service.restore.worker.is_some());
    f.service
        .settle_selected_source96_for_relocation94()
        .await
        .unwrap();
    assert_eq!(
        f.shared
            .phase
            .lock()
            .unwrap()
            .selection
            .as_ref()
            .unwrap()
            .target_deadline(&f.shared.cancel)
            .unwrap(),
        deadline
    );
}

async fn paused94(f: &Fixture, at: u8) {
    loop {
        let changed = f.shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if f.shared.test.source94_reached.load(Ordering::Acquire) == at {
            return;
        }
        let status = f.shared.status.load(Ordering::Acquire);
        if status == FAILED {
            failure_diagnostics::report(&f.shared, "source94 before requested boundary");
        }
        assert_eq!(
            status,
            RUNNING,
            "source94 stopped before requested boundary"
        );
        changed.await;
    }
}
fn release94(f: &Fixture) {
    *f.shared.test.source94_released.lock().unwrap() = true;
    f.shared.test.source94_release.notify_one();
}
async fn native_effects(path: &Path) -> i64 {
    // Read-only observations never grant native dispatch or Motion ACK.
    rows(path, "catalog_source_successions").await
}
async fn native_receipts(path: &Path, operation: &str) -> i64 {
    use sqlx::Connection;
    let mut db = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .create_if_missing(false),
    )
    .await
    .unwrap();
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM catalog_source_successions s JOIN catalog_command_receipts r ON r.catalog_id=s.catalog_id AND r.host_namespace=s.command_host_namespace AND r.operation_id=s.command_operation_id WHERE s.command_operation_id=?")
        .bind(operation).fetch_one(&mut db).await.unwrap();
    db.close().await.unwrap();
    count
}
async fn assert_durable_rows(motion: &Path, intents: i64, acks: i64) {
    assert_eq!(
        rows(motion, "catabolic_source_relocation_admissions").await,
        intents
    );
    assert_eq!(
        rows(motion, "catabolic_source_relocation_commands").await,
        intents
    );
    assert_eq!(rows(motion, "catabolic_source_relocation_acks").await, acks);
}
fn request94(
    operation: &str,
    expected: &LifecycleProof,
    candidate: &Path,
) -> SourceRelocation94Request {
    SourceRelocation94Request::relocate(
        operation.to_owned(),
        expected.clone(),
        candidate.to_owned(),
    )
    .unwrap()
}

async fn assert_original_target_cas(motion: &Path, expected: &LifecycleProof) {
    use sqlx::Connection;
    // Native96 changes the schema, not catalog/incarnation, and retains the
    // original Motion lifecycle rows. Observe the genuine converted target's
    // complete proof before asking94 to consume it; do not assume that source
    // provenance alone supplies the target CAS, or fabricate replacement fields.
    let mut observed = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(motion)
            .read_only(true)
            .create_if_missing(false),
    )
    .await
    .unwrap();
    let mut snapshot = observed.begin().await.unwrap();
    let actual =
        motion_catalog_backend::source_lifecycle::current_in(&mut snapshot, &expected.source_id)
            .await
            .unwrap();
    assert_eq!(
        &actual, expected,
        "genuine96 target must retain the entire original lifecycle CAS"
    );
    snapshot.rollback().await.unwrap();
    observed.close().await.unwrap();
}

fn assert_retained94(
    f: &Fixture,
    native_result: Option<bool>,
    receipt: bool,
    intent_commit: Commit94,
    ack_commit: Commit94,
) {
    let phase = f.shared.phase.lock().unwrap();
    let evidence = phase
        .target
        .as_ref()
        .unwrap()
        .source94_retained_evidence()
        .unwrap();
    assert_eq!(
        evidence,
        (native_result, receipt, true, intent_commit, ack_commit),
        "actual typed results, permanent receipt observation, and original86 close admission remain in the same whole94 owner"
    );
}

#[tokio::test]
async fn actual_service94_relocates_and_acks_without_replacing_original_custody() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        requester,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    ready94(f, root.path()).await;
    let (motion, native) = names(f, root.path());
    assert_original_target_cas(&motion, &expected).await;
    let original = std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap();
    let auth = f.shared.authorization_storage.load(Ordering::Acquire);
    let selected = f.shared.selection_storage.load(Ordering::Acquire);
    let (deadline, selected_fd, ancestry) = {
        let phase = f.shared.phase.lock().unwrap();
        (
            phase
                .selection
                .as_ref()
                .unwrap()
                .target_deadline(&f.shared.cancel)
                .unwrap(),
            phase
                .selection
                .as_ref()
                .unwrap()
                .evidence_for_fixture()
                .0
                .unwrap()
                .0
                .as_raw_fd(),
            phase.target.as_ref().unwrap().staging_ancestry_evidence(),
        )
    };
    assert!(
        f.service.begin_selected_reconcile94_restore().is_err(),
        "absent intent/receipt cannot enter receipt-only execution"
    );
    f.service
        .begin_selected_relocation94_restore(request94("service94-success", &expected, &candidate))
        .unwrap();
    drop(f.service.wait_selected_pair_restore());
    f.service
        .wait_selected_pair_restore()
        .await
        .inspect_err(|_| failure_diagnostics::report(&f.shared, "source94 expected completion"))
        .unwrap();
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::ReturnedOk,
    );
    {
        let phase = f.shared.phase.lock().unwrap();
        let target = phase.target.as_ref().unwrap();
        assert_eq!(target.phase, TargetPhase::Source94Ready);
        assert!(
            phase.workspace.is_none(),
            "same workspace travels with whole94 owner"
        );
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
        let after = target.staging_ancestry_evidence();
        assert_eq!(
            (after.1, after.2, after.3, after.4),
            (ancestry.1, ancestry.2, ancestry.3, ancestry.4)
        );
    }
    assert_eq!(auth, f.shared.authorization_storage.load(Ordering::Acquire));
    assert_eq!(selected, f.shared.selection_storage.load(Ordering::Acquire));
    let frame = f.shared.test.source94_async_frame.load(Ordering::Acquire);
    assert!(
        frame > 0 && frame <= super::super::super::super::target_reservation::ASYNC_FRAME_LIMIT
    );
    assert_eq!(
        original,
        std::fs::read(format!("/proc/self/fd/{}", f.pair_fd)).unwrap()
    );
    assert_durable_rows(&motion, 1, 1).await;
    assert_eq!(native_effects(&native).await, 1);
    assert_eq!(native_receipts(&native, "service94-success").await, 1);
    use sqlx::Connection;
    let mut observed = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&motion)
            .read_only(true),
    )
    .await
    .unwrap();
    let (saved_requester, predecessor, successor): (String, String, String) = sqlx::query_as(
        "SELECT a.requester_json,a.predecessor_json,k.successor_json FROM catabolic_source_relocation_admissions a JOIN catabolic_source_relocation_acks k USING(operation_id) WHERE a.operation_id='service94-success'")
        .fetch_one(&mut observed).await.unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved_requester).unwrap();
    assert_eq!(saved["requester_id"], requester.principal.id);
    assert_eq!(saved["credential_hash"], requester.credential().unwrap().0);
    assert_eq!(
        serde_json::from_str::<LifecycleProof>(&predecessor).unwrap(),
        expected
    );
    let next: LifecycleProof = serde_json::from_str(&successor).unwrap();
    assert_eq!(next.source_id, expected.source_id);
    assert_eq!(next.catalog_id, expected.catalog_id);
    assert_eq!(next.incarnation, expected.incarnation);
    assert_eq!(next.epoch, expected.epoch + 1);
    assert_eq!(next.binding_revision, expected.binding_revision + 1);
    assert_ne!(next.mapping_digest, expected.mapping_digest);
    assert_eq!(
        motion_catalog_backend::source_lifecycle::current_in(&mut observed, &next.source_id)
            .await
            .unwrap(),
        next
    );
    observed.close().await.unwrap();
    assert!(
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-replacement",
                &expected,
                &candidate
            ))
            .is_err()
    );
    assert!(f.service.begin_selected_reconcile94_restore().is_err());
    f.assert_current();
    no_later_effects(root.path());
    f.close().await;
    no_later_effects(root.path());
}

#[tokio::test]
async fn actual_service94_clean_receipt_pending_reconciles_once_without_roots_or_execute() {
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
    assert_original_target_cas(&motion, &expected).await;
    f.service
        .begin_selected_relocation94_to_receipt(request94(
            "service94-lost-ack",
            &expected,
            &candidate,
        ))
        .unwrap();
    f.service
        .wait_selected_pair_restore()
        .await
        .inspect_err(|_| failure_diagnostics::report(&f.shared, "source94 expected completion"))
        .unwrap();
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::NotAttempted,
    );
    {
        let phase = f.shared.phase.lock().unwrap();
        assert_eq!(
            phase.target.as_ref().unwrap().phase,
            TargetPhase::ReceiptPending
        );
        assert!(
            !phase.pair.as_ref().unwrap()._writer.authorization_claimed(),
            "clean pause requires genuine normal reader retirement ACK"
        );
        assert!(phase.workspace.is_none());
    }
    assert_durable_rows(&motion, 1, 0).await;
    assert_eq!(native_effects(&native).await, 1);
    assert_eq!(native_receipts(&native, "service94-lost-ack").await, 1);
    // No changed CAS/candidate can replace the immutable first claim. The
    // finite service API does not accept a replacement Caller at all.
    let mut different = expected.clone();
    different.binding_revision += 1;
    assert!(
        f.service
            .begin_selected_relocation94_restore(request94("service94-lost-ack", &different, &old))
            .is_err()
    );
    assert!(
        f.service.begin_selected_reconcile94_restore().is_err(),
        "success status is not worker/timer retirement"
    );
    drop(f.service.settle_selected_relocation94_for_reconcile());
    assert!(f.service.restore.worker.is_some());
    f.service
        .settle_selected_relocation94_for_reconcile()
        .await
        .unwrap();
    for path in [&old, &candidate] {
        std::fs::remove_file(path.join("one.txt")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
    f.service.begin_selected_reconcile94_restore().unwrap();
    f.service
        .wait_selected_pair_restore()
        .await
        .inspect_err(|_| failure_diagnostics::report(&f.shared, "source94 expected completion"))
        .unwrap();
    assert_retained94(
        f,
        Some(true),
        true,
        Commit94::ReturnedOk,
        Commit94::ReturnedOk,
    );
    assert_eq!(
        f.shared
            .phase
            .lock()
            .unwrap()
            .target
            .as_ref()
            .unwrap()
            .phase,
        TargetPhase::Source94Ready
    );
    assert_durable_rows(&motion, 1, 1).await;
    assert_eq!(
        native_effects(&native).await,
        1,
        "receipt-only retry cannot execute"
    );
    assert_eq!(native_receipts(&native, "service94-lost-ack").await, 1);
    assert!(f.service.begin_selected_reconcile94_restore().is_err());
    assert!(!old.exists() && !candidate.exists());
    no_later_effects(root.path());
    f.close().await;
}

#[tokio::test]
async fn actual_service94_refuses_full_cas_and_changed_candidate_without_native_effect() {
    for change_after_intent in [false, true] {
        let Physical94 {
            mut fixture,
            mut expected,
            candidate,
            ..
        } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        assert!(
            SourceRelocation94Request::relocate(
                "service94-nonnormalized".into(),
                expected.clone(),
                candidate.join("..")
            )
            .is_err(),
            "lexical candidate identity must not be silently normalized"
        );
        let mut oversized = candidate.clone();
        oversized.reserve(16 * 1024);
        assert!(
            SourceRelocation94Request::relocate(
                "service94-capacity".into(),
                expected.clone(),
                oversized
            )
            .is_err(),
            "actual retained path capacity must fit the unchanged cohort"
        );
        assert_eq!(
            f.shared
                .phase
                .lock()
                .unwrap()
                .target
                .as_ref()
                .unwrap()
                .phase,
            TargetPhase::SourceReady,
            "request validation cannot consume source96 custody"
        );
        if change_after_intent {
            f.shared.test.source94_pause.store(2, Ordering::Release);
        } else {
            expected.binding_revision += 1;
        }
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-refused",
                &expected,
                &candidate,
            ))
            .unwrap();
        if change_after_intent {
            paused94(f, 2).await;
            std::fs::remove_file(candidate.join("one.txt")).unwrap();
            release94(f);
        }
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_retained94(
            f,
            change_after_intent.then_some(false),
            false,
            if change_after_intent {
                Commit94::ReturnedOk
            } else {
                Commit94::NotAttempted
            },
            Commit94::NotAttempted,
        );
        assert!(
            f.service.begin_selected_reconcile94_restore().is_err(),
            "absence of a permanent receipt is not a recovery grant"
        );
        assert_eq!(native_effects(&native).await, 0);
        assert_eq!(native_receipts(&native, "service94-refused").await, 0);
        assert_durable_rows(&motion, i64::from(change_after_intent), 0).await;
        f.assert_current();
        no_later_effects(root.path());
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service94_fresh_target_policy_denial_cannot_use_current_source_permission() {
    use sqlx::Connection;
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        requester,
        reserve_admin,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    ready94(f, root.path()).await;
    let (motion, native) = names(f, root.path());
    assert_original_target_cas(&motion, &expected).await;
    let mut writer = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&motion)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    motion_catalog_backend::projection::configure_connection(&mut writer)
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();
    crate::v2::auth::retained::deny_target_source_management_in(
        &mut writer,
        &reserve_admin,
        &requester,
    )
    .await
    .unwrap();
    sqlx::query("COMMIT").execute(&mut writer).await.unwrap();
    writer.close().await.unwrap();
    f.service
        .begin_selected_relocation94_restore(request94(
            "service94-target-denied",
            &expected,
            &candidate,
        ))
        .unwrap();
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_retained94(
        f,
        None,
        false,
        Commit94::NotAttempted,
        Commit94::NotAttempted,
    );
    {
        let phase = f.shared.phase.lock().unwrap();
        let Some(Failure::Error(error)) = &phase.failure else {
            panic!("actual authorization failure required")
        };
        assert!(
            format!("{error:#}").contains("403") || format!("{error:#}").contains("forbidden"),
            "{error:#}"
        );
        assert_ne!(
            phase.target.as_ref().unwrap().phase,
            TargetPhase::Source94Ready
        );
    }
    assert_durable_rows(&motion, 0, 0).await;
    assert_eq!(native_effects(&native).await, 0);
    f.assert_current();
    f.close().await;
}

#[tokio::test]
async fn actual_service94_real_expiry_denies_each_final_insert_commit_and_dispatch_fence() {
    use sqlx::Connection;
    // Each case uses a fresh real token and unchanged original 120-second
    // selection deadline. Expiry occurs after full checks, at the final seam.
    // This proves first-poll authorization, not SQL worker execution-time auth.
    for stage in [10, 11, 12, 13, 14] {
        let Physical94 {
            mut fixture,
            expected,
            candidate,
            ..
        } = physical_fixture(true).await;
        let f = &mut fixture;
        let expiry = f.target_expiry.unwrap();
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-expiry",
                &expected,
                &candidate,
            ))
            .unwrap();
        paused94(f, stage).await;
        // Current source revocation cannot bypass the real retained WRITE. A
        // failed supported competing writer is not represented as revocation.
        let mut competing = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(f._root.path().join("motion.sqlite3"))
                .create_if_missing(false)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        assert!(
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut competing)
                .await
                .is_err()
        );
        competing.close().await.unwrap();
        assert!(
            crate::now() < expiry,
            "genuine credential must reach boundary before expiry"
        );
        while crate::now() < expiry {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            !f.shared.cancel.load(Ordering::Acquire),
            "this is credential expiry, not deadline cancellation"
        );
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_retained94(
            f,
            (stage >= 13).then_some(true),
            stage >= 13,
            if stage >= 12 {
                Commit94::ReturnedOk
            } else {
                Commit94::NotAttempted
            },
            Commit94::NotAttempted,
        );
        {
            let phase = f.shared.phase.lock().unwrap();
            let Some(Failure::Error(error)) = &phase.failure else {
                panic!("real credential expiry required")
            };
            assert!(
                format!("{error:#}").contains("credential_expired"),
                "{error:#}"
            );
            assert_ne!(
                phase.target.as_ref().unwrap().phase,
                TargetPhase::Source94Ready
            );
        }
        assert_durable_rows(&motion, i64::from(stage >= 12), 0).await;
        assert_eq!(native_effects(&native).await, i64::from(stage >= 13));
        assert!(
            f.service.begin_selected_reconcile94_restore().is_err(),
            "expired owner cannot revive the clean receipt-only corridor"
        );
        f.assert_current();
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service94_polled_waiter_drop_cancels_but_does_not_take_the_owner() {
    let Physical94 {
        mut fixture,
        expected,
        candidate,
        ..
    } = physical_fixture(false).await;
    let f = &mut fixture;
    let root = tempfile::tempdir().unwrap();
    ready94(f, root.path()).await;
    let (motion, native) = names(f, root.path());
    assert_original_target_cas(&motion, &expected).await;
    f.shared.test.source94_pause.store(1, Ordering::Release);
    f.service
        .begin_selected_relocation94_restore(request94("service94-waiter", &expected, &candidate))
        .unwrap();
    paused94(f, 1).await;
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
    release94(f);
    assert!(f.service.wait_selected_pair_restore().await.is_err());
    assert_retained94(
        f,
        None,
        false,
        Commit94::NotAttempted,
        Commit94::NotAttempted,
    );
    assert!(f.service.begin_selected_reconcile94_restore().is_err());
    assert_durable_rows(&motion, 0, 0).await;
    assert_eq!(native_effects(&native).await, 0);
    f.assert_current();
    f.close().await;
}

#[tokio::test]
async fn actual_service94_cancel_and_panic_retain_committed_intent_and_permanent_native_receipt() {
    for (stage, panic) in [
        (1, true),
        (2, false),
        (2, true),
        (3, false),
        (3, true),
        (4, true),
    ] {
        let Physical94 {
            mut fixture,
            expected,
            candidate,
            ..
        } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-interrupted",
                &expected,
                &candidate,
            ))
            .unwrap();
        paused94(f, stage).await;
        if panic {
            f.shared.test.source94_panic.store(true, Ordering::Release);
        } else {
            f.shared.cancel.store(true, Ordering::Release);
        }
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_retained94(
            f,
            (stage >= 3).then_some(true),
            stage >= 3,
            if stage >= 2 {
                Commit94::ReturnedOk
            } else {
                Commit94::NotAttempted
            },
            if stage == 4 {
                Commit94::ReturnedOk
            } else {
                Commit94::NotAttempted
            },
        );
        {
            let phase = f.shared.phase.lock().unwrap();
            assert!(phase.target.is_some() && phase.selection.is_some() && phase.pair.is_some());
            assert!(
                phase.workspace.is_none(),
                "the moved workspace is not replaced after failure"
            );
            assert!(phase.failure.is_some());
        }
        assert_durable_rows(&motion, i64::from(stage >= 2), i64::from(stage == 4)).await;
        assert_eq!(native_effects(&native).await, i64::from(stage >= 3));
        assert_eq!(
            native_receipts(&native, "service94-interrupted").await,
            i64::from(stage >= 3)
        );
        assert!(
            f.service.begin_selected_reconcile94_restore().is_err(),
            "a panic/cancellation is not the clean ReceiptPending checkpoint"
        );
        assert!(
            f.service
                .begin_selected_relocation94_restore(request94(
                    "service94-interrupted",
                    &expected,
                    &candidate
                ))
                .is_err()
        );
        f.assert_current();
        no_later_effects(root.path());
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service94_post_native_return_and_reader_retirement_unwind_keep_exact_results() {
    // 15 is normal reader retirement after intent commit; 16 is immediately
    // after native returns, BEFORE receipt reread; 17 follows receipt reread.
    for stage in [15, 16, 17] {
        let Physical94 {
            mut fixture,
            expected,
            candidate,
            ..
        } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.shared.test.source94_panic.store(true, Ordering::Release);
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-inner-unwind",
                &expected,
                &candidate,
            ))
            .unwrap();
        paused94(f, stage).await;
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_retained94(
            f,
            (stage >= 16).then_some(true),
            stage == 17,
            Commit94::ReturnedOk,
            Commit94::NotAttempted,
        );
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            let evidence = target.source94_evidence().unwrap();
            assert!(
                !evidence.1,
                "native/SQL effect success does not restore workspace reuse"
            );
            assert!(!evidence.2 && !evidence.3);
            assert!(
                phase.pair.as_ref().unwrap()._writer.authorization_claimed(),
                "the real reader remains claimed before retirement ACK"
            );
            assert!(target.source94_successor().is_none());
            assert!(matches!(phase.failure, Some(Failure::Panic(_))));
        }
        assert_durable_rows(&motion, 1, 0).await;
        assert_eq!(native_effects(&native).await, i64::from(stage >= 16));
        assert_eq!(
            native_receipts(&native, "service94-inner-unwind").await,
            i64::from(stage >= 16)
        );
        assert!(
            f.service.begin_selected_reconcile94_restore().is_err(),
            "unretired authentication cannot masquerade as clean ReceiptPending"
        );
        f.assert_current();
        f.close().await;
    }
}

#[tokio::test]
async fn actual_service94_real_busy_intent_and_ack_commit_preserve_the_actual_writer() {
    use sqlx::Connection;
    for stage in [11, 14] {
        let Physical94 {
            mut fixture,
            expected,
            candidate,
            ..
        } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        f.shared.test.source94_pause.store(stage, Ordering::Release);
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-real-busy",
                &expected,
                &candidate,
            ))
            .unwrap();
        paused94(f, stage).await;
        // A genuine SQLite SHARED reader holds the last committed snapshot,
        // forcing the actual installed COMMIT runner to return SQLITE_BUSY.
        let mut blocker = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
                .read_only(true)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        let mut snapshot = blocker.begin().await.unwrap();
        let before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM catabolic_source_relocation_acks")
                .fetch_one(&mut *snapshot)
                .await
                .unwrap();
        assert_eq!(before, 0);
        release94(f);
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_retained94(
            f,
            (stage == 14).then_some(true),
            stage == 14,
            if stage == 11 {
                Commit94::ReturnedErr
            } else {
                Commit94::ReturnedOk
            },
            if stage == 14 {
                Commit94::ReturnedErr
            } else {
                Commit94::NotAttempted
            },
        );
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_ne!(target.phase, TargetPhase::Source94Ready);
            let evidence = target.source94_evidence().unwrap();
            assert!(!evidence.1 && !evidence.3);
            assert!(phase.pair.as_ref().unwrap()._writer.authorization_claimed());
            assert!(phase.failure.is_some());
        }
        assert!(
            f.service.begin_selected_reconcile94_restore().is_err(),
            "failed/uncertain commit is never a receipt recovery grant"
        );
        snapshot.rollback().await.unwrap();
        blocker.close().await.unwrap();
        // Cleanup settles this same returned connection and actual SQLite
        // transaction state. It may roll back but must never reissue COMMIT.
        f.close().await;
        assert_durable_rows(&motion, i64::from(stage == 14), 0).await;
        assert_eq!(native_effects(&native).await, i64::from(stage == 14));
        no_later_effects(root.path());
    }
}

#[tokio::test]
async fn actual_service94_unpolled_authorization_purposes_remain_claimed_and_close_without_execution()
 {
    for purpose in [1, 2, 3] {
        let Physical94 {
            mut fixture,
            expected,
            candidate,
            ..
        } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        f.shared
            .test
            .source94_unpolled
            .store(purpose, Ordering::Release);
        f.service
            .begin_selected_relocation94_restore(request94(
                "service94-unpolled",
                &expected,
                &candidate,
            ))
            .unwrap();
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        assert_retained94(
            f,
            (purpose == 3).then_some(true),
            purpose == 3,
            if purpose >= 2 {
                Commit94::ReturnedOk
            } else {
                Commit94::NotAttempted
            },
            Commit94::NotAttempted,
        );
        {
            let phase = f.shared.phase.lock().unwrap();
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Source94);
            let evidence = target.source94_evidence().unwrap();
            assert!(
                !evidence.1,
                "unpolled purpose still claimed the exact workspace"
            );
            assert!(!evidence.2 && !evidence.3);
            let writer = &phase.pair.as_ref().unwrap()._writer;
            assert!(writer.authorization_claimed(), "unpolled purpose owns its exact source claim");
            assert!(!writer.retains_authorization_reader(), "no unpolled connection");
            assert!(!writer.retains_authorization_acquisition(), "no unpolled opening frame");
        }
        assert_durable_rows(&motion, i64::from(purpose >= 2), 0).await;
        assert_eq!(native_effects(&native).await, i64::from(purpose == 3));
        assert!(f.service.begin_selected_reconcile94_restore().is_err());
        f.assert_current();
        f.close().await;
        assert_durable_rows(&motion, i64::from(purpose >= 2), 0).await;
        assert_eq!(native_effects(&native).await, i64::from(purpose == 3));
    }
}


#[tokio::test]
async fn actual_service94_public_saved_completion_requires_prepared_original_work() {
    // Each case gets a fresh genuine service owner. Ack and Reconcile reach
    // real ReceiptPending only after normal Dispatch reader retirement; no
    // successful state is manufactured by the negative fixture branch.
    for purpose in [1, 2, 3, 4] {
        let Physical94 { mut fixture, expected, candidate, .. } = physical_fixture(false).await;
        let f = &mut fixture;
        let root = tempfile::tempdir().unwrap();
        ready94(f, root.path()).await;
        let (motion, native) = names(f, root.path());
        assert_original_target_cas(&motion, &expected).await;
        if purpose == 4 {
            f.service.begin_selected_relocation94_to_receipt(request94(
                "service94-unprepared-saved", &expected, &candidate,
            )).unwrap();
            f.service
                .wait_selected_pair_restore()
                .await
                .inspect_err(|_| failure_diagnostics::report(&f.shared, "source94 expected completion"))
                .unwrap();
            assert_retained94(f, Some(true), true, Commit94::ReturnedOk, Commit94::NotAttempted);
            {
                let phase = f.shared.phase.lock().unwrap();
                assert_eq!(phase.target.as_ref().unwrap().phase, TargetPhase::ReceiptPending);
                assert!(!phase.pair.as_ref().unwrap()._writer.authorization_claimed());
            }
            f.service.settle_selected_relocation94_for_reconcile().await.unwrap();
        }
        f.shared.test.source94_unprepared_saved.store(purpose, Ordering::Release);
        if purpose == 4 {
            f.service.begin_selected_reconcile94_restore().unwrap();
        } else {
            f.service.begin_selected_relocation94_restore(request94(
                "service94-unprepared-saved", &expected, &candidate,
            )).unwrap();
        }
        assert!(f.service.wait_selected_pair_restore().await.is_err());
        {
            let phase = f.shared.phase.lock().unwrap();
            let Some(Failure::Error(error)) = &phase.failure else {
                panic!("fixed public saved-path refusal must complete without fixture panic");
            };
            assert!(format!("{error:#}").contains("saved94 fixture refused unprepared"), "{error:#}");
            let target = phase.target.as_ref().unwrap();
            assert_eq!(target.phase, TargetPhase::Source94);
            assert_eq!(target.source94_evidence().unwrap().0, motion_catalog_backend::SourceRelocation94Phase::Interrupted);
            assert!(target.source94_successor().is_none());
        }
        let has_intent = purpose >= 2;
        let has_receipt = purpose >= 3;
        assert_retained94(f, has_receipt.then_some(true), has_receipt,
            if has_intent { Commit94::ReturnedOk } else { Commit94::NotAttempted },
            Commit94::NotAttempted);
        assert_durable_rows(&motion, i64::from(has_intent), 0).await;
        assert_eq!(native_effects(&native).await, i64::from(has_receipt));
        assert!(f.service.begin_selected_reconcile94_restore().is_err());
        f.assert_current();
        f.close().await;
        assert_durable_rows(&motion, i64::from(has_intent), 0).await;
        assert_eq!(native_effects(&native).await, i64::from(has_receipt));
        no_later_effects(root.path());
    }
}

#[path = "source94_tests/positive_close_tests.rs"]
mod positive_close_tests;
