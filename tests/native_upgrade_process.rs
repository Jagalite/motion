//! Real packaged CLI, never a test-harness observer or automatic startup upgrade.
use motion_catalog_backend::{NativeCatalog, cutover, inspect};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
struct Fixture {
    _root: tempfile::TempDir,
    data: PathBuf,
    native: PathBuf,
    motion: PathBuf,
    db: SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().canonicalize().unwrap();
        let motion = data.join("playscale.sqlite3");
        let native = data.join("custom-native.sqlite3");
        catabolic_store::migration::initialize_native_at(&native, 39).unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|c, _| {
                Box::pin(motion_catalog_backend::projection::configure_connection(c))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&motion)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let backend = NativeCatalog::bind(
            db.clone(),
            native.clone(),
            inspect(native.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        backend
            .freeze_motion_source("operator-upgrade", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("operator-upgrade")
            .await
            .unwrap();
        backend.stage_import("operator-upgrade").await.unwrap();
        backend
            .prepare_motion_cutover("operator-upgrade")
            .await
            .unwrap();
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        drop(backend);
        Self {
            _root: root,
            data,
            native,
            motion,
            db,
        }
    }
}
async fn command(
    data: &Path,
    native: &Path,
    action: &str,
    confirmed: bool,
    listen: &str,
) -> std::process::Output {
    let mut c = tokio::process::Command::new(env!("CARGO_BIN_EXE_playscale"));
    c.arg("--data-dir")
        .arg(data)
        .arg("--catalog-path")
        .arg(native)
        .arg("--native-schema-upgrade")
        .arg(action)
        .arg("--listen")
        .arg(listen)
        .kill_on_drop(true);
    if confirmed {
        c.arg("--confirm-native-upgrade-quiescence");
    }
    tokio::time::timeout(std::time::Duration::from_secs(120), c.output())
        .await
        .expect("offline operator process timed out")
        .unwrap()
}
#[tokio::test]
async fn packaged_offline_upgrade_requires_confirmation_and_exclusive_server_lock() {
    let f = Fixture::new().await;
    let original: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&f.db)
            .await
            .unwrap();
    // Occupied listener proves this explicit branch does not start HTTP.
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap().to_string();
    assert!(
        !command(&f.data, &f.native, "prepare", false, &address)
            .await
            .status
            .success()
    );
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(f.data.join("server.lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    let rejected = command(&f.data, &f.native, "prepare", true, &address).await;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("another server owns"));
    drop(lock);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_native_upgrade_intent")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        0
    );
    let prepared = command(&f.data, &f.native, "prepare", true, &address).await;
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let intent: serde_json::Value = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(intent["from"]["schema_version"], 39);
    assert_eq!(
        inspect(f.native.clone())
            .await
            .unwrap()
            .identity
            .schema_version,
        39
    );
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    let executed = command(&f.data, &f.native, "execute", true, &address).await;
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    let proof: serde_json::Value = serde_json::from_slice(&executed.stdout).unwrap();
    assert_eq!(
        proof["plan_fingerprint"],
        intent["plan"]["plan_fingerprint"]
    );
    assert_eq!(proof["to_schema"], 40);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        original
    );
    let mut admitted = cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
        .await
        .unwrap();
    assert_eq!(
        admitted
            .admission
            .admitted_host()
            .unwrap()
            .identity()
            .unwrap()["schema_version"],
        40
    );
    assert!(f.motion.is_file());
}
