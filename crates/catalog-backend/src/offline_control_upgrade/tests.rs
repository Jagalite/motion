use super::*;
use std::borrow::Cow;
pub(super) async fn sealed() -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("motion.sqlite3");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("../../migrations");
    migrations.migrations = Cow::Owned(
        migrations
            .iter()
            .filter(|m| m.version <= 93)
            .cloned()
            .collect(),
    );
    migrations.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO profiles(id,name) VALUES('retained','Original profile')")
        .execute(&pool)
        .await
        .unwrap();
    let native = root.path().join("native.sqlite3");
    crate::initialize(native.clone()).await.unwrap();
    let identity = crate::inspect(native.clone()).await.unwrap().identity;
    let backend = crate::NativeCatalog::bind(pool.clone(), native, identity)
        .await
        .unwrap();
    backend
        .freeze_motion_source("ordered-control", 0)
        .await
        .unwrap();
    backend.seal_motion_freeze(1).await.unwrap();
    let raw: String = sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
        .fetch_one(&pool)
        .await
        .unwrap();
    drop(backend);
    pool.close().await;
    (root, path, catabolic_core::sha256(raw.as_bytes()))
}
fn initial(path: &Path) -> String {
    catabolic_core::sha256(std::fs::read(path).unwrap())
}
#[tokio::test]
async fn actual_sealed93_atomic_upgrade_preserves_rows_and_retains_exact97() {
    let (_root, path, binding) = sealed().await;
    let reference = UpgradeReference::build().await.unwrap();
    let owner = reference
        .upgrade(
            File::open(&path).unwrap(),
            &path,
            &initial(&path),
            &binding,
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        owner
            .db
            .query_row("SELECT name FROM profiles WHERE id='retained'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "Original profile"
    );
    assert_eq!(
        owner
            .db
            .query_row(
                "SELECT count(*) FROM _sqlx_migrations WHERE version BETWEEN 94 AND 97",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        4
    );
    assert!(
        owner
            .db
            .execute(
                "INSERT INTO catabolic_native_bootstrap_intents VALUES('unowned','{}')",
                []
            )
            .is_err()
    );
    drop(owner);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        reference.0.verify_in(&db).unwrap(),
        ControlVersion::Complete97
    );
    db.execute_batch("ROLLBACK").unwrap();
    drop(db);
    assert!(
        reference
            .upgrade(
                File::open(&path).unwrap(),
                &path,
                &initial(&path),
                &binding,
                0,
                &AtomicBool::new(false)
            )
            .is_err(),
        "fresh stage hash must not replace original93 preservation proof"
    );
}
#[tokio::test]
async fn wrong_binding_cancel_and_partial_schema_refuse_without_advancement() {
    let reference = UpgradeReference::build().await.unwrap();
    for mode in 0..3 {
        let (_root, path, binding) = sealed().await;
        if mode == 2 {
            Connection::open(&path)
                .unwrap()
                .execute_batch("CREATE TABLE unreviewed_partial(value)")
                .unwrap();
        }
        let result = reference.upgrade(
            File::open(&path).unwrap(),
            &path,
            &initial(&path),
            if mode == 0 { "wrong" } else { &binding },
            0,
            &AtomicBool::new(mode == 1),
        );
        assert!(result.is_err());
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            93
        );
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE name='catabolic_native_bootstrap_intents'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
}
#[tokio::test]
async fn held_stage_replacement_refuses_without_modifying_replacement() {
    let (_root, path, binding) = sealed().await;
    let reference = UpgradeReference::build().await.unwrap();
    let file = File::open(&path).unwrap();
    let hash = initial(&path);
    let old = path.with_extension("old");
    std::fs::rename(&path, &old).unwrap();
    std::fs::copy(&old, &path).unwrap();
    assert!(
        reference
            .upgrade(file, &path, &hash, &binding, 0, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(initial(&path), hash);
}

#[tokio::test]
async fn post_effect_errors_rollback_every_resource_and_exact_retry() {
    let reference = UpgradeReference::build().await.unwrap();
    for phase in [94, 95, 96, 97, 100] {
        let (_root, path, binding) = sealed().await;
        let image = initial(&path);
        FAULT.with(|f| f.set(Some((phase, false))));
        let result = reference.upgrade(
            File::open(&path).unwrap(),
            &path,
            &image,
            &binding,
            0,
            &AtomicBool::new(false),
        );
        FAULT.with(|f| f.set(None));
        assert!(result.is_err());
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            93
        );
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('catabolic_source_relocation_admissions','catabolic_native_bootstrap_intents','preview_sheet_artifacts')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
        drop(db);
        assert!(
            reference
                .upgrade(
                    File::open(&path).unwrap(),
                    &path,
                    &initial(&path),
                    &binding,
                    0,
                    &AtomicBool::new(false)
                )
                .is_ok()
        );
    }
}
#[test]
#[ignore = "child process crash helper invoked by actual process-loss test"]
fn process_loss_child() {
    let path = PathBuf::from(std::env::var_os("MOTION_CONTROL_CRASH_PATH").expect("child path"));
    let binding = std::env::var("MOTION_CONTROL_CRASH_BINDING").unwrap();
    let phase = std::env::var("MOTION_CONTROL_CRASH_PHASE")
        .unwrap()
        .parse()
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let reference = runtime.block_on(UpgradeReference::build()).unwrap();
    FAULT.with(|f| f.set(Some((phase, true))));
    let _ = reference.upgrade(
        File::open(&path).unwrap(),
        &path,
        &initial(&path),
        &binding,
        0,
        &AtomicBool::new(false),
    );
    panic!("crash checkpoint not reached");
}
#[tokio::test]
async fn actual_process_loss_before_commit_reopens93_and_retries() {
    let reference = UpgradeReference::build().await.unwrap();
    for phase in [94, 100] {
        let (_root, path, binding) = sealed().await;
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "offline_control_upgrade::tests::process_loss_child",
                "--nocapture",
            ])
            .env("MOTION_CONTROL_CRASH_PATH", &path)
            .env("MOTION_CONTROL_CRASH_BINDING", &binding)
            .env("MOTION_CONTROL_CRASH_PHASE", phase.to_string())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            93
        );
        assert_eq!(
            db.query_row("SELECT name FROM profiles WHERE id='retained'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "Original profile"
        );
        drop(db);
        assert!(
            reference
                .upgrade(
                    File::open(&path).unwrap(),
                    &path,
                    &initial(&path),
                    &binding,
                    0,
                    &AtomicBool::new(false)
                )
                .is_ok()
        );
    }
}

#[tokio::test]
async fn paired_memory_reservation_refuses_before_schema_effects() {
    assert!(admit_memory(None, u64::MAX, 1).is_err());
    let (_root, path, binding) = sealed().await;
    let reference = UpgradeReference::build().await.unwrap();
    assert!(
        reference
            .upgrade(
                File::open(&path).unwrap(),
                &path,
                &initial(&path),
                &binding,
                256 * 1024 * 1024,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(
        Connection::open(&path)
            .unwrap()
            .query_row("SELECT max(version) FROM _sqlx_migrations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        93
    );
}
