//! Packaging qualification for Motion's exact closed companion and pinned runtime.
use catabolic_core::catalog_host_lease::HostAuthorization;
use catabolic_runtime::{
    helper,
    host::{CONTRACT_VERSION, HostSession},
};
use catabolic_store::{Store, migration, observation};
use serde_json::json;
use std::{
    fs,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn scan(host: &mut HostSession) {
    let handle = host.start_scan("media", ()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = host.poll_scan(&handle, Duration::from_millis(50)).unwrap() {
            assert_eq!(result["complete"], true, "{result}");
            break;
        }
        assert!(Instant::now() < deadline, "native scan did not settle");
    }
    assert!(host.worker_inventory().unwrap().admits_new_work());
}

#[test]
fn packaged_companion_observes_real_files_and_releases_native_ownership() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let catalog = root_path.join("catalog.sqlite3");
    let source = root_path.join("media");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("one.txt"), "first observed content").unwrap();
    migration::initialize_native(&catalog).unwrap();
    let store = Store::open(&catalog, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"kind":"source","owner":"media","root":source}),
    )
    .unwrap();
    drop(store);
    let binary = Path::new(env!("CARGO_BIN_EXE_motion-catalog-observer"));
    // Qualification owns these built bytes and the audited two-command entrypoint;
    // this is not a runtime grant derived from a caller-supplied binary/hash.
    let grant = helper::TrustedClosedObservationArtifact::after_packaging_verification(
        binary,
        &catabolic_core::sha256(fs::read(binary).unwrap()),
        helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap();
    let gate = HostAuthorization::new("motion.observer.test".into(), "r1".into()).unwrap();
    let mut host =
        HostSession::open_authorized(&catalog, "default", true, CONTRACT_VERSION, gate.clone())
            .unwrap();
    host.register_closed_observation_helper(&grant).unwrap();
    scan(&mut host);
    host.close().unwrap();
    let store = Store::open(&catalog, true, false).unwrap();
    let file: String = store
        .db
        .query_row(
            "SELECT id FROM files WHERE location='media' AND path='one.txt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut authority = catabolic_store::catalog_import_typed::TypedAuthority::default();
    authority
        .physical_files
        .insert("default".into(), [file.clone()].into());
    let first = catabolic_store::catalog_import_typed::capture_existing_occurrence(
        &store,
        "motion-file",
        "default",
        &file,
        &authority,
    )
    .unwrap();
    assert_eq!(first.observed_status.as_deref(), Some("present"));
    let first_revision = first.exact.revision;
    drop(store);
    fs::write(source.join("one.txt"), "replacement with different size").unwrap();
    let mut host =
        HostSession::open_authorized(&catalog, "default", true, CONTRACT_VERSION, gate).unwrap();
    host.register_closed_observation_helper(&grant).unwrap();
    scan(&mut host);
    host.close().unwrap();
    let store = Store::open(&catalog, true, false).unwrap();
    let current = catabolic_store::catalog_import_typed::capture_existing_occurrence(
        &store,
        "motion-file",
        "default",
        &file,
        &authority,
    )
    .unwrap();
    assert_ne!(current.exact.revision, first_revision);
    assert!(
        catabolic_store::native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}

#[test]
fn companion_rejects_legacy_protocols_and_unfenced_requests() {
    let binary = env!("CARGO_BIN_EXE_motion-catalog-observer");
    let root = tempfile::tempdir().unwrap();
    let absent = root.path().join("must-not-create.sqlite3");
    for protocol in [
        "observation-worker",
        "native-observation-worker-v1",
        "native-observation-worker-v2",
        "native-observation-shared-worker-v1",
    ] {
        let output = Command::new(binary)
            .args([
                "--db",
                absent.to_str().unwrap(),
                "--profile",
                "default",
                protocol,
                "{}",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!absent.exists());
    }
}
