use catabolic_runtime::helper::{ClosedObservationProtocol, TrustedClosedObservationArtifact};
use catabolic_store::{Store, observation};
use motion_catalog_backend::{NativeCatalog, initialize, inspect, sources::ScanCancellation};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
struct Fixture {
    _root: tempfile::TempDir,
    catalog: PathBuf,
    media: PathBuf,
    db: sqlx::SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let catalog = base.join("native.sqlite3");
        let media = base.join("media");
        fs::create_dir(&media).unwrap();
        initialize(catalog.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|conn, _| {
                Box::pin(motion_catalog_backend::projection::configure_connection(
                    conn,
                ))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE libraries(id TEXT PRIMARY KEY,name TEXT NOT NULL DEFAULT '',root TEXT NOT NULL,root_identity TEXT NOT NULL,binding_revision INTEGER NOT NULL,managed INTEGER NOT NULL,exclusions_json TEXT NOT NULL DEFAULT '[]',enabled INTEGER NOT NULL DEFAULT 1,revision INTEGER NOT NULL DEFAULT 0,scan_barrier INTEGER NOT NULL DEFAULT 0);").execute(&db).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0066_catabolic_command_journal.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0070_catabolic_source_mappings.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0077_catabolic_native_source_registry.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        let metadata = fs::metadata(&media).unwrap();
        use std::os::unix::fs::MetadataExt;
        let root_identity = format!("{}:{}", metadata.dev(), metadata.ino());
        sqlx::query("INSERT INTO libraries(id,root,root_identity,binding_revision,managed) VALUES('motion-source',?,?,1,0)").bind(media.to_str().unwrap()).bind(root_identity).execute(&db).await.unwrap();
        Self {
            _root: root,
            catalog,
            media,
            db,
        }
    }
    async fn backend(&self) -> NativeCatalog {
        NativeCatalog::bind(
            self.db.clone(),
            self.catalog.clone(),
            inspect(self.catalog.clone()).await.unwrap().identity,
        )
        .await
        .unwrap()
    }
}
fn helper() -> TrustedClosedObservationArtifact {
    let binary = Path::new(env!("CARGO_BIN_EXE_motion-catalog-observer"));
    TrustedClosedObservationArtifact::after_packaging_verification(
        binary,
        &catabolic_core::sha256(fs::read(binary).unwrap()),
        ClosedObservationProtocol::NoChildWithExactJoinV2,
    )
    .unwrap()
}
#[tokio::test]
async fn explicit_source_mapping_survives_restart_and_refuses_changed_identity() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let mapping = backend
        .register_source("motion-source", "default", "native-source", f.media.clone())
        .await
        .unwrap();
    assert_eq!(mapping.library_id, "motion-source");
    assert_eq!(mapping.native_source, "native-source");
    assert_eq!(
        backend
            .register_source("motion-source", "default", "native-source", f.media.clone())
            .await
            .unwrap(),
        mapping
    );
    assert!(
        backend
            .register_source("motion-source", "default", "different", f.media.clone())
            .await
            .is_err()
    );
    drop(backend);
    let backend = f.backend().await;
    assert_eq!(
        backend.audit_source_mapping("motion-source").await.unwrap(),
        mapping
    );
    sqlx::query("UPDATE libraries SET binding_revision=2 WHERE id='motion-source'")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(backend.audit_source_mapping("motion-source").await.is_err());
    assert!(
        backend
            .register_source("motion-source", "default", "native-source", f.media.clone())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn conflicting_native_binding_is_not_replaced() {
    let f = Fixture::new().await;
    let other = f.media.with_file_name("other");
    fs::create_dir(&other).unwrap();
    let store = Store::open(&f.catalog, true, false).unwrap();
    observation::execute(
        &store,
        "default",
        "bind",
        &json!({"owner":"native-source","root":other}),
    )
    .unwrap();
    drop(store);
    let backend = f.backend().await;
    assert!(
        backend
            .register_source("motion-source", "default", "native-source", f.media.clone())
            .await
            .is_err()
    );
    let store = Store::open(&f.catalog, false, false).unwrap();
    assert_eq!(
        observation::binding(&store, "default", "source", "native-source").unwrap()["root"],
        other.to_str().unwrap()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_mappings")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[tokio::test]
async fn native_binding_committed_before_host_ack_can_be_adopted_only_exactly() {
    let f = Fixture::new().await;
    let store = Store::open(&f.catalog, true, false).unwrap();
    let original = observation::execute(
        &store,
        "default",
        "bind",
        &json!({"owner":"native-source","root":f.media}),
    )
    .unwrap();
    drop(store);
    let backend = f.backend().await;
    let mapping = backend
        .register_source("motion-source", "default", "native-source", f.media.clone())
        .await
        .unwrap();
    assert_eq!(mapping.native_binding, original);
    assert!(
        sqlx::query("DELETE FROM catabolic_source_mappings")
            .execute(&f.db)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn native_binding_drift_and_root_replacement_are_refused() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .register_source("motion-source", "default", "native-source", f.media.clone())
        .await
        .unwrap();
    let old = f.media.with_file_name("moved");
    fs::rename(&f.media, &old).unwrap();
    fs::create_dir(&f.media).unwrap();
    assert!(backend.audit_source_mapping("motion-source").await.is_err());
    assert!(
        backend
            .register_source("motion-source", "default", "native-source", f.media.clone())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn scan_owns_real_session_and_preserves_mapping_across_restart() {
    let f = Fixture::new().await;
    fs::write(f.media.join("one.txt"), "first").unwrap();
    let backend = f.backend().await;
    backend
        .register_source("motion-source", "default", "native-source", f.media.clone())
        .await
        .unwrap();
    let first = backend
        .scan_source(
            "motion-source",
            helper(),
            ScanCancellation::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(first["complete"], true, "{first}");
    backend.drain(Duration::from_secs(5)).await.unwrap();
    drop(backend);
    fs::write(f.media.join("two.txt"), "second").unwrap();
    let backend = f.backend().await;
    let second = backend
        .scan_source(
            "motion-source",
            helper(),
            ScanCancellation::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(second["complete"], true, "{second}");
    let store = Store::open(&f.catalog, false, false).unwrap();
    assert!(
        catabolic_store::native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
    let count: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM files WHERE location='native-source'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}
#[tokio::test]
async fn cancelled_and_revoked_admission_never_start_work() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .register_source("motion-source", "default", "native-source", f.media.clone())
        .await
        .unwrap();
    let cancellation = ScanCancellation::default();
    cancellation.cancel();
    assert!(
        backend
            .scan_source(
                "motion-source",
                helper(),
                cancellation,
                Duration::from_secs(1)
            )
            .await
            .is_err()
    );
    backend.drain(Duration::from_secs(1)).await.unwrap();
    assert!(
        backend
            .scan_source(
                "motion-source",
                helper(),
                ScanCancellation::default(),
                Duration::from_secs(1)
            )
            .await
            .is_err()
    );
    let store = Store::open(&f.catalog, false, false).unwrap();
    assert!(
        catabolic_store::native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}
#[tokio::test]
async fn scan_preserves_motion_exclusions_and_rejects_policy_drift() {
    let f = Fixture::new().await;
    fs::create_dir(f.media.join("private")).unwrap();
    fs::write(f.media.join("private/secret.txt"), "excluded").unwrap();
    fs::write(f.media.join("visible.unknown_extension"), "included").unwrap();
    sqlx::query("UPDATE libraries SET exclusions_json='[\"private\"]' WHERE id='motion-source'")
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    let mapping = backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    assert_eq!(mapping.exclusions, vec!["private"]);
    assert_eq!(mapping.native_policy["extensions"], serde_json::Value::Null);
    backend
        .scan_source(
            "motion-source",
            helper(),
            ScanCancellation::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let store = Store::open(&f.catalog, true, false).unwrap();
    let paths: Vec<String> = store
        .db
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(paths, vec!["visible.unknown_extension"]);
    observation::execute(
        &store,
        "default",
        "scan_policy",
        &json!({"source":mapping.native_source,"apply":true,"policy":{"exclusions":[]}}),
    )
    .unwrap();
    drop(store);
    assert!(backend.audit_source_mapping("motion-source").await.is_err());
    assert!(
        backend
            .register_motion_source("motion-source")
            .await
            .is_err()
    );
}
#[tokio::test]
async fn source_mapping_requires_every_unmanaged_source() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    assert!(backend.audit_source_mappings().await.is_err());
    backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    assert_eq!(backend.audit_source_mappings().await.unwrap().len(), 1);
}

/// Wait for actual native claim ownership, not elapsed time or an assumed child
/// PID. The fixture's large tree keeps this real traversal observable.
async fn wait_for_owned_worker(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let observed = path.to_owned();
        let owned = tokio::task::spawn_blocking(move || {
            let store = Store::open(&observed, false, false)?;
            Ok::<_, anyhow::Error>(
                !catabolic_store::native_worker_inventory::inspect(&store)?.admits_new_work(),
            )
        })
        .await
        .unwrap()
        .unwrap();
        if owned {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native worker ownership was not observed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn populate_slow_tree(root: &Path) {
    for directory in 0..100 {
        let folder = root.join(format!("folder-{directory}"));
        fs::create_dir(&folder).unwrap();
        for file in 0..50 {
            fs::write(folder.join(format!("file-{file}.txt")), "fixture media").unwrap();
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_active_async_scan_cancels_demand_and_retains_cleanup_for_drain() {
    let f = Fixture::new().await;
    populate_slow_tree(&f.media);
    let backend = f.backend().await;
    backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    let worker = backend.clone();
    let task = tokio::spawn(async move {
        worker
            .scan_source(
                "motion-source",
                helper(),
                ScanCancellation::default(),
                Duration::from_secs(30),
            )
            .await
    });
    wait_for_owned_worker(&f.catalog).await;
    task.abort();
    let _ = task.await;
    let cleanup_started = std::time::Instant::now();
    let short = backend.drain(Duration::ZERO).await;
    if let Err(error) = short {
        assert!(
            error.to_string().contains("Timeout"),
            "unexpected short drain failure: {error}"
        );
    }
    backend.drain(Duration::from_secs(120)).await.unwrap();
    eprintln!(
        "dropped scan exact cleanup: {:?}",
        cleanup_started.elapsed()
    );
    let store = Store::open(&f.catalog, false, false).unwrap();
    assert!(
        catabolic_store::native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
    assert!(
        backend.status().await.is_err(),
        "revocation must remain closed"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_running_cancel_waits_for_native_cleanup() {
    let f = Fixture::new().await;
    populate_slow_tree(&f.media);
    let backend = f.backend().await;
    backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    let cancellation = ScanCancellation::default();
    let worker = backend.clone();
    let token = cancellation.clone();
    let task = tokio::spawn(async move {
        worker
            .scan_source("motion-source", helper(), token, Duration::from_secs(30))
            .await
    });
    wait_for_owned_worker(&f.catalog).await;
    cancellation.cancel();
    assert!(task.await.unwrap().is_err());
    backend.drain(Duration::from_secs(120)).await.unwrap();
    let store = Store::open(&f.catalog, false, false).unwrap();
    assert!(
        catabolic_store::native_worker_inventory::inspect(&store)
            .unwrap()
            .admits_new_work()
    );
}

#[tokio::test]
async fn retained_host_rejects_wrong_incarnation_profile_or_binding_before_scan() {
    use catabolic_core::catalog_host_lease::HostAuthorization;
    use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
    use motion_catalog_backend::sources::scan_with_host;
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let mapping = backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    let gate = HostAuthorization::new("motion.source.fence.test".into(), "r1".into()).unwrap();
    let mut host =
        HostSession::open_authorized(&f.catalog, "default", true, CONTRACT_VERSION, gate).unwrap();
    for field in 0..4 {
        let mut changed = mapping.clone();
        match field {
            0 => changed.catalog.incarnation.push_str("-restored"),
            1 => changed.profile = "different-profile".into(),
            2 => changed.native_binding["inode"] = json!(0),
            _ => changed.native_policy["revision"] = json!(999),
        }
        assert!(
            scan_with_host(
                &mut host,
                &changed,
                &ScanCancellation::default(),
                Duration::from_secs(1),
                ()
            )
            .is_err(),
            "field {field}"
        );
        assert!(host.worker_inventory().unwrap().admits_new_work());
    }
    host.close().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn source_root_fifo_is_rejected_before_native_binding() {
    let f = Fixture::new().await;
    let fifo = f._root.path().join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    sqlx::query("UPDATE libraries SET root=? WHERE id='motion-source'")
        .bind(fifo.to_str().unwrap())
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    assert!(
        backend
            .register_motion_source("motion-source")
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_mappings")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[cfg(unix)]
#[tokio::test]
async fn exact_occurrence_bridge_preserves_motion_ids_and_rejects_stale_fingerprint() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new().await;
    fs::write(f.media.join("one.txt"), "exact bytes").unwrap();
    let metadata = fs::metadata(f.media.join("one.txt")).unwrap();
    let modified = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fingerprint = format!(
        "{}:{modified}:{}:{}:{}:{}",
        metadata.len(),
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec()
    );
    sqlx::raw_sql("CREATE TABLE media_files(id TEXT PRIMARY KEY,library_id TEXT,edition_id TEXT,relative_path TEXT,revision TEXT,fingerprint TEXT,bytes INTEGER,available INTEGER); CREATE TABLE version_files(version_id TEXT,file_id TEXT,part INTEGER,file_revision TEXT);")
        .execute(&f.db).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0076_catabolic_occurrence_mappings.sql"
    ))
    .execute(&f.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO media_files VALUES('motion-file','motion-source','edition','one.txt','content-revision',?,11,1)").bind(fingerprint).execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO version_files VALUES('version','motion-file',1,'content-revision')")
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    backend
        .scan_source(
            "motion-source",
            helper(),
            ScanCancellation::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let mut tx = f.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let captured = backend
        .capture_motion_occurrences(&mut tx, "import-one")
        .await
        .unwrap();
    assert!(captured.unresolved.is_empty(), "{:?}", captured.unresolved);
    assert_eq!(captured.mappings.len(), 1);
    let mapped = &captured.mappings[0];
    assert_eq!(mapped.physical.source_occurrence_id, "motion-file");
    assert_eq!(mapped.association.item_id, "motion.edition:edition");
    assert_eq!(mapped.version_files[0]["version_id"], "version");
    assert_ne!(mapped.physical.exact.file_id, "motion-file");
    tx.commit().await.unwrap();
    let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
        "motion.projection.test".into(),
        "r1".into(),
    )
    .unwrap();
    let mut host = catabolic_runtime::host::HostSession::open_authorized(
        &f.catalog,
        "default",
        true,
        catabolic_runtime::host::CONTRACT_VERSION,
        gate,
    )
    .unwrap();
    let projected =
        motion_catalog_backend::occurrence_bridge::project_with_host(&host, &captured.mappings)
            .unwrap();
    assert_eq!(projected.files[0]["id"], "motion-file");
    assert_eq!(projected.files[0]["available"], 1);
    host.close().unwrap();
    assert_eq!(
        backend
            .occurrence_authority("import-one")
            .await
            .unwrap()
            .physical_files["default"]
            .len(),
        1
    );
    sqlx::query("UPDATE media_files SET fingerprint='10:1:2:3:4:5'")
        .execute(&f.db)
        .await
        .unwrap();
    let mut tx = f.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        backend
            .capture_motion_occurrences(&mut tx, "import-one")
            .await
            .is_err()
    );
    let stale = backend
        .capture_motion_occurrences(&mut tx, "import-two")
        .await
        .unwrap();
    assert!(stale.mappings.is_empty());
    assert!(stale.unresolved.contains_key("motion-file"));
    tx.rollback().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_scan_keeps_motion_binding_writer_fenced_until_cleanup() {
    let f = Fixture::new().await;
    populate_slow_tree(&f.media);
    let backend = f.backend().await;
    backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    let cancel = ScanCancellation::default();
    let token = cancel.clone();
    let worker = backend.clone();
    let task = tokio::spawn(async move {
        worker
            .scan_source("motion-source", helper(), token, Duration::from_secs(30))
            .await
    });
    wait_for_owned_worker(&f.catalog).await;
    let mut edit = f.db.acquire().await.unwrap();
    sqlx::query("PRAGMA busy_timeout=0")
        .execute(&mut *edit)
        .await
        .unwrap();
    assert!(
        sqlx::query(
            "UPDATE libraries SET binding_revision=binding_revision+1 WHERE id='motion-source'"
        )
        .execute(&mut *edit)
        .await
        .is_err(),
        "binding writer escaped retained scan fence"
    );
    cancel.cancel();
    assert!(task.await.unwrap().is_err());
    sqlx::query(
        "UPDATE libraries SET binding_revision=binding_revision+1 WHERE id='motion-source'",
    )
    .execute(&mut *edit)
    .await
    .unwrap();
    backend.drain(Duration::from_secs(120)).await.unwrap();
}

#[tokio::test]
async fn native_registry_registers_without_legacy_projection_and_replays_exactly() {
    let f = Fixture::new().await;
    let root = f._root.path().join("new-native-source");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("new.txt"), "native").unwrap();
    let backend = f.backend().await;
    let mapping = backend
        .register_native_source(
            "new-source-id",
            "logical-library",
            "Native library",
            root.clone(),
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        backend
            .register_native_source(
                "new-source-id",
                "logical-library",
                "Native library",
                root.clone(),
                vec![]
            )
            .await
            .unwrap(),
        mapping
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM libraries WHERE id='new-source-id'")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        backend.source_mapping("new-source-id").await.unwrap(),
        mapping
    );
    backend
        .scan_source(
            "new-source-id",
            helper(),
            ScanCancellation::default(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert!(
        backend
            .register_native_source(
                "new-source-id",
                "different-library",
                "Native library",
                root,
                vec![]
            )
            .await
            .is_err()
    );
    assert_eq!(backend.native_sources().await.unwrap().len(), 1);
    // Simulate only the disposable exact source row, not native ownership.
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity,binding_revision,managed) VALUES(?,?,?,?,1,0)")
        .bind("new-source-id").bind("Native library").bind(&mapping.motion_root)
        .bind(&mapping.motion_root_identity).execute(&f.db).await.unwrap();
    assert_eq!(
        backend
            .register_native_source(
                "new-source-id",
                "logical-library",
                "Native library",
                mapping.motion_root.clone().into(),
                vec![]
            )
            .await
            .unwrap(),
        mapping
    );
    sqlx::query("UPDATE libraries SET name='tampered' WHERE id='new-source-id'")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(
        backend
            .register_native_source(
                "new-source-id",
                "logical-library",
                "Native library",
                mapping.motion_root.clone().into(),
                vec![]
            )
            .await
            .is_err()
    );
    // A matching path without an existing immutable registration is still a collision.
    assert!(
        backend
            .register_native_source(
                "motion-source",
                "logical-library",
                "Legacy",
                f.media.clone(),
                vec![]
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn retained_host_registration_requires_exact_immutable_acknowledgment() {
    use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
    let f = Fixture::new().await;
    let root = f._root.path().join("retained-source");
    fs::create_dir(&root).unwrap();
    let backend = f.backend().await;
    let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
        "motion.registration.test".into(),
        "r1".into(),
    )
    .unwrap();
    let mut host =
        HostSession::open_authorized(&f.catalog, "default", true, CONTRACT_VERSION, gate).unwrap();
    let prepared = motion_catalog_backend::source_registry::register_with_host(
        &mut host,
        "new-id",
        "logical",
        "Name",
        root.clone(),
        vec![],
    )
    .unwrap();
    let mapped = backend.acknowledge_native_source(prepared).await.unwrap();
    assert_eq!(
        host.source_binding(&mapped.native_source).unwrap(),
        mapped.native_binding
    );
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity,binding_revision,managed) VALUES('new-id','Name',?,?,1,0)")
        .bind(&mapped.motion_root).bind(&mapped.motion_root_identity).execute(&f.db).await.unwrap();
    let replay = motion_catalog_backend::source_registry::register_with_host(
        &mut host,
        "new-id",
        "logical",
        "Name",
        root.clone(),
        vec![],
    )
    .unwrap();
    assert_eq!(
        backend.acknowledge_native_source(replay).await.unwrap(),
        mapped
    );
    sqlx::query("UPDATE libraries SET managed=1 WHERE id='new-id'")
        .execute(&f.db)
        .await
        .unwrap();
    let mismatched = motion_catalog_backend::source_registry::register_with_host(
        &mut host,
        "new-id",
        "logical",
        "Name",
        root,
        vec![],
    )
    .unwrap();
    assert!(backend.acknowledge_native_source(mismatched).await.is_err());
    host.close().unwrap();
}

#[tokio::test]
async fn replaced_motion_root_is_rejected_before_initial_native_registration() {
    let f = Fixture::new().await;
    fs::rename(&f.media, f._root.path().join("old-root")).unwrap();
    fs::create_dir(&f.media).unwrap();
    let backend = f.backend().await;
    assert!(
        backend
            .register_motion_source("motion-source")
            .await
            .is_err()
    );
    let store = Store::open(&f.catalog, false, false).unwrap();
    let count: i64 = store
        .db
        .query_row(
            "SELECT count(*) FROM bindings WHERE kind='source'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_retained_host_stays_open_after_exact_cleanup() {
    use catabolic_runtime::host::{CONTRACT_VERSION, HostSession};
    let f = Fixture::new().await;
    populate_slow_tree(&f.media);
    let backend = f.backend().await;
    let mapping = backend
        .register_motion_source("motion-source")
        .await
        .unwrap();
    let cancellation = ScanCancellation::default();
    let token = cancellation.clone();
    let path = f.catalog.clone();
    let task = tokio::task::spawn_blocking(move || {
        let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
            "motion.retained.cancel".into(),
            "r1".into(),
        )
        .unwrap();
        let mut host =
            HostSession::open_authorized(&path, "default", true, CONTRACT_VERSION, gate).unwrap();
        host.register_closed_observation_helper(&helper()).unwrap();
        let outcome = motion_catalog_backend::sources::scan_with_host(
            &mut host,
            &mapping,
            &token,
            Duration::from_secs(120),
            (),
        );
        assert!(outcome.is_err());
        assert!(host.worker_inventory().unwrap().admits_new_work());
        assert!(
            host.identity().is_ok(),
            "successful cancellation cleanup must not strand retained server host closed"
        );
        host.close().unwrap();
    });
    wait_for_owned_worker(&f.catalog).await;
    cancellation.cancel();
    task.await.unwrap();
}
