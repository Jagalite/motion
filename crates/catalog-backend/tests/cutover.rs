use motion_catalog_backend::{NativeCatalog, cutover, initialize, inspect};
use sqlx::SqlitePool;
use std::path::PathBuf;
struct Fixture {
    _root: tempfile::TempDir,
    native: PathBuf,
    db: SqlitePool,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let native = base.join("native.sqlite3");
        initialize(native.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(motion_catalog_backend::projection::configure_connection(
                    connection,
                ))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        Self {
            _root: root,
            native,
            db,
        }
    }
    async fn backend(&self) -> NativeCatalog {
        NativeCatalog::bind(
            self.db.clone(),
            self.native.clone(),
            inspect(self.native.clone()).await.unwrap().identity,
        )
        .await
        .unwrap()
    }
}
#[tokio::test]
async fn native_startup_never_falls_back_when_checkpoint_missing() {
    let f = Fixture::new().await;
    let _backend = f.backend().await;
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn unresolved_catalog_cannot_prepare_cutover_or_seal_source() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    sqlx::query("INSERT INTO items(id,title,kind) VALUES('x','X','video')")
        .execute(&f.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_origins(item_id,title) VALUES('x','X')")
        .execute(&f.db)
        .await
        .unwrap();
    // Logical work-only migration is now supported. An unbound logical library
    // still has an explicit unresolved scope/source mapping obligation.
    sqlx::query(
        "INSERT INTO catalog_libraries(id,name,kind) VALUES('unmapped','Unmapped','mixed')",
    )
    .execute(&f.db)
    .await
    .unwrap();
    backend.freeze_motion_source("migration", 0).await.unwrap();
    backend.capture_motion_import("migration").await.unwrap();
    backend.stage_import("migration").await.unwrap();
    assert!(backend.prepare_motion_cutover("migration").await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_cutover")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(matches!(
        backend.source_state().await.unwrap().mode,
        playscale_core::catalog_source::Mode::Frozen { .. }
    ));
}
#[tokio::test]
async fn freeze_blocks_application_writes_until_actual_host_acknowledgment() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend.freeze_motion_source("migration", 0).await.unwrap();
    assert!(
        sqlx::query("INSERT INTO items(id,title,kind) VALUES('x','X','video')")
            .execute(&f.db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO profiles(id,name) VALUES('p','P')")
            .execute(&f.db)
            .await
            .is_err()
    );
    backend.seal_motion_freeze(1).await.unwrap();
    assert!(
        sqlx::query("INSERT INTO items(id,title,kind) VALUES('x','X','video')")
            .execute(&f.db)
            .await
            .is_err()
    );
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn empty_cutover_activates_reconciles_and_retains_native_owner() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .freeze_motion_source("empty-migration", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("empty-migration")
        .await
        .unwrap();
    backend.stage_import("empty-migration").await.unwrap();
    let prepared = backend
        .prepare_motion_cutover("empty-migration")
        .await
        .unwrap();
    assert_eq!(
        prepared.phase,
        playscale_core::catalog_cutover::Phase::Validated
    );
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    let active = backend
        .activate_motion_cutover(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(
        active.phase,
        playscale_core::catalog_cutover::Phase::Activated
    );
    assert!(
        backend.status().await.is_err(),
        "old gate must remain closed"
    );
    assert_eq!(backend.reconcile_motion_cutover().await.unwrap(), active);
    drop(backend);
    let mut admission = cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
        .await
        .unwrap();
    assert_eq!(
        admission.admission.decision(),
        catabolic_store::catalog_import::Reconciliation::AdmitCatabolicOnly
    );
    let identity = admission
        .admission
        .admitted_host()
        .unwrap()
        .identity()
        .unwrap();
    assert_eq!(
        identity["catalog_id"].as_str(),
        Some(active.catalog_id.as_str())
    );
    // Application ownership reopens without resurrecting legacy catalog writers.
    sqlx::query("INSERT INTO profiles(id,name) VALUES('application-profile','App')")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(
        sqlx::query("INSERT INTO items(id,title,kind) VALUES('legacy','No','video')")
            .execute(&f.db)
            .await
            .is_err()
    );
    admission
        .admission
        .admitted_host()
        .unwrap()
        .close()
        .unwrap();
}

#[tokio::test]
async fn complete_partition_manifest_is_required_for_restart() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    backend
        .freeze_motion_source("partitioned", 0)
        .await
        .unwrap();
    let plan = backend
        .capture_motion_export_plan("partitioned")
        .await
        .unwrap();
    assert_eq!(plan.partitions.len(), 1);
    backend
        .prepare_motion_partition_cutover("partitioned")
        .await
        .unwrap();
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    backend
        .activate_motion_partition_cutover(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    let mut server = cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
        .await
        .unwrap();
    server.admission.admitted_host().unwrap().close().unwrap();
    drop(server);
    // Durable schema prevents deleting proof after it has acknowledged ownership.
    assert!(
        sqlx::query("DELETE FROM catabolic_cutover_parts")
            .execute(&f.db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE catabolic_cutover_sets SET manifest_digest='wrong'")
            .execute(&f.db)
            .await
            .is_err()
    );
    sqlx::query("INSERT INTO profiles(id,name) VALUES('after-partition','After')")
        .execute(&f.db)
        .await
        .unwrap();
    assert!(
        sqlx::query("INSERT INTO items(id,title,kind) VALUES('legacy','Legacy','video')")
            .execute(&f.db)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn more_than_1024_works_activate_only_as_complete_bounded_set() {
    let f = Fixture::new().await;
    let backend = f.backend().await;
    let mut tx = f.db.begin().await.unwrap();
    for index in 0..1100 {
        let id = format!("work-{index:04}");
        sqlx::query("INSERT INTO items(id,title,kind) VALUES(?,?,'video')")
            .bind(&id)
            .bind(&id)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item_origins(item_id,title) VALUES(?,?)")
            .bind(&id)
            .bind(&id)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    backend.freeze_motion_source("large", 0).await.unwrap();
    let plan = backend.capture_motion_export_plan("large").await.unwrap();
    assert!(plan.partitions.len() > 1);
    backend
        .prepare_motion_partition_cutover("large")
        .await
        .unwrap();
    assert!(
        cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
            .await
            .is_err()
    );
    backend
        .activate_motion_partition_cutover(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    // Re-executing all saved commands only replays original receipts, including
    // on the same already-closed original operator gate.
    let before = inspect(f.native.clone()).await.unwrap().generation;
    backend
        .activate_motion_partition_cutover(std::time::Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(before, inspect(f.native.clone()).await.unwrap().generation);
    let mut server = cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
        .await
        .unwrap();
    server.admission.admitted_host().unwrap().close().unwrap();
    drop(server);
    let native = catabolic_store::Store::open(&f.native, false, false).unwrap();
    let count: i64 = native
        .db
        .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1100);
}

#[tokio::test]
async fn imported_work_revision_uses_atomic_receipt_baseline_and_survives_restart() {
    let f = Fixture::new().await;
    sqlx::query("INSERT INTO items(id,title,kind,catalog_revision,match_state) VALUES('preserved','Original','video',9,'unmatched')").execute(&f.db).await.unwrap();
    sqlx::query("INSERT INTO item_origins(item_id,title) VALUES('preserved','Original')")
        .execute(&f.db)
        .await
        .unwrap();
    let backend = f.backend().await;
    backend
        .freeze_motion_source("revision-baseline", 0)
        .await
        .unwrap();
    backend
        .capture_motion_export_plan("revision-baseline")
        .await
        .unwrap();
    backend
        .prepare_motion_partition_cutover("revision-baseline")
        .await
        .unwrap();
    backend
        .activate_motion_partition_cutover(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    let (catalog,incarnation,base,native):(String,String,i64,i64)=sqlx::query_as("SELECT catalog_id,incarnation,motion_base,native_base FROM catabolic_revision_bridges WHERE native_id='preserved'").fetch_one(&f.db).await.unwrap();
    assert_eq!(base, 9);
    assert!(native >= 1);
    let mut conn = f.db.acquire().await.unwrap();
    assert_eq!(
        motion_catalog_backend::revision_bridge::wire_revision_in(
            &mut conn,
            "preserved",
            &catalog,
            &incarnation,
            native
        )
        .await
        .unwrap(),
        9
    );
    assert_eq!(
        motion_catalog_backend::revision_bridge::wire_revision_in(
            &mut conn,
            "preserved",
            &catalog,
            &incarnation,
            native + 1
        )
        .await
        .unwrap(),
        10
    );
    assert!(
        motion_catalog_backend::revision_bridge::wire_revision_in(
            &mut conn,
            "preserved",
            &catalog,
            "restored",
            native
        )
        .await
        .is_err()
    );
    drop(conn);
    assert!(
        sqlx::query("UPDATE catabolic_revision_bridges SET native_base=native_base+1")
            .execute(&f.db)
            .await
            .is_err()
    );
    let mut server = cutover::open_server(f.db.clone(), f.native.clone(), "default".into())
        .await
        .unwrap();
    let host = server.admission.admitted_host().unwrap();
    let path = f
        ._root
        .path()
        .canonicalize()
        .unwrap()
        .join("motion.sqlite3");
    motion_catalog_backend::projection::ProjectionWriter::reserve(&path)
        .unwrap()
        .rebuild(host)
        .unwrap();
    let wire: i64 = sqlx::query_scalar("SELECT catalog_revision FROM items WHERE id='preserved'")
        .fetch_one(&f.db)
        .await
        .unwrap();
    assert_eq!(wire, 9);
    host.close().unwrap();
}
