use catabolic_store::migration::{self, NativeUpgradeQuiescence};
use motion_catalog_backend::{NativeCatalog, cutover, inspect, native_upgrade};
use sqlx::SqlitePool;
use std::path::PathBuf;
fn quiet() -> NativeUpgradeQuiescence {
    NativeUpgradeQuiescence {
        admission_stopped: true,
        readers_and_workers_stopped: true,
        exclusive_operational_ownership: true,
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    motion: PathBuf,
    native: PathBuf,
    db: SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let native = base.join("native.sqlite3");
        let motion = base.join("motion.sqlite3");
        migration::initialize_native_at(&native, 39).unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|db, _| {
                Box::pin(motion_catalog_backend::projection::configure_connection(db))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&motion)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let backend = NativeCatalog::bind(
            db.clone(),
            native.clone(),
            inspect(native.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        backend
            .freeze_motion_source("upgrade-empty", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("upgrade-empty")
            .await
            .unwrap();
        backend.stage_import("upgrade-empty").await.unwrap();
        backend
            .prepare_motion_cutover("upgrade-empty")
            .await
            .unwrap();
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        drop(backend);
        Self {
            _root: root,
            motion,
            native,
            db,
        }
    }
}
#[tokio::test]
async fn native_upgrade_requires_operator_and_exact_ack_before_restart() {
    let f = Fixture::new().await;
    let original: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&f.db)
            .await
            .unwrap();
    assert!(
        native_upgrade::prepare(&f.motion, &f.native, NativeUpgradeQuiescence::default()).is_err()
    );
    let saved = native_upgrade::prepare(&f.motion, &f.native, quiet()).unwrap();
    assert_eq!(saved["from"]["schema_version"], 39);
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO catabolic_native_upgrade_ack VALUES(1,?)")
            .bind(r#"{"contract_version":1,"from_schema":39,"to_schema":40}"#)
            .execute(&f.db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE catabolic_native_upgrade_intent SET intent_json=intent_json")
            .execute(&f.db)
            .await
            .is_err()
    );
    let proof = native_upgrade::execute(&f.motion, &f.native, quiet()).unwrap();
    assert_eq!(proof["to_schema"], 40);
    assert_eq!(
        native_upgrade::execute(&f.motion, &f.native, quiet()).unwrap(),
        proof
    );
    let mut admission = cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
        .await
        .unwrap();
    assert_eq!(admission.identity.schema_version, 40);
    // A successful restart genuinely owns the upgraded native host.
    assert_eq!(
        admission
            .admission
            .admitted_host()
            .unwrap()
            .identity()
            .unwrap()["schema_version"],
        40
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        original
    );
    assert!(
        sqlx::query("DELETE FROM catabolic_native_upgrade_ack")
            .execute(&f.db)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn native_commit_before_companion_ack_reconciles_exact_saved_plan() {
    let f = Fixture::new().await;
    let saved = native_upgrade::prepare(&f.motion, &f.native, quiet()).unwrap();
    migration::upgrade_native(&f.native, &saved["plan"], quiet(), None, &|_, _, _| Ok(())).unwrap();
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    let proof = native_upgrade::execute(&f.motion, &f.native, quiet()).unwrap();
    assert_eq!(proof["plan_fingerprint"], saved["plan"]["plan_fingerprint"]);
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_ok()
    );
}
#[tokio::test]
async fn post_native_commit_mutation_cannot_be_adopted_as_upgrade_ack() {
    let f = Fixture::new().await;
    let saved = native_upgrade::prepare(&f.motion, &f.native, quiet()).unwrap();
    migration::upgrade_native(&f.native, &saved["plan"], quiet(), None, &|_, _, _| Ok(())).unwrap();
    let db = catabolic_store::connect(&f.native, true).unwrap();
    db.execute("INSERT INTO profiles(id) VALUES('later')", [])
        .unwrap();
    drop(db);
    assert!(native_upgrade::execute(&f.motion, &f.native, quiet()).is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_native_upgrade_ack")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        0
    );
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn lost_upgrade_capability_guard_is_detected_not_repaired() {
    let f = Fixture::new().await;
    sqlx::query("DROP TRIGGER catabolic_native_upgrade_ack_insert")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(native_upgrade::prepare(&f.motion, &f.native, quiet()).is_err());
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_native_upgrade_intent")
            .fetch_one(&f.db)
            .await
            .unwrap(),
        0
    );
}
