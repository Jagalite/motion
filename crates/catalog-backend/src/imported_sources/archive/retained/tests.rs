use super::*;
use std::{borrow::Cow, fs::File, os::unix::fs::MetadataExt, time::Duration};
const LIMIT: u64 = 256 * 1024 * 1024;
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
async fn fixture() -> (tempfile::TempDir, Connection, Identity) {
    let root = tempfile::tempdir().unwrap();
    let motion = root.path().join("motion.sqlite3");
    let native = root.path().join("native.sqlite3");
    let media = root.path().join("media");
    std::fs::create_dir(&media).unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        // Export holds its writer while the actual source audit acquires a reader.
        .max_connections(2)
        .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&motion)
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
    // Actual original Motion library input, not a fabricated native receipt,
    // checkpoint or bridge. The real exporter/activation ACK publishes all three.
    sqlx::query("INSERT INTO catalog_libraries(id,revision,name,kind,language) VALUES('archived',7,'Archived library','mixed','en')").execute(&pool).await.unwrap();
    // A logical-library-only source is not an admitted cutover cohort. Supply
    // an actual empty registered physical source, preserving the real gate.
    let md = std::fs::metadata(&media).unwrap();
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('physical','Physical',?,?,0,1,1,1,'[]',0)")
        .bind(media.to_str().unwrap()).bind(format!("{}:{}", md.dev(), md.ino())).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO library_sources(library_id,source_id) VALUES('archived','physical')")
        .execute(&pool)
        .await
        .unwrap();
    crate::initialize(native.clone()).await.unwrap();
    let identity = crate::inspect(native.clone()).await.unwrap().identity;
    let backend = crate::NativeCatalog::bind(pool.clone(), native, identity.clone())
        .await
        .unwrap();
    backend.register_motion_source("physical").await.unwrap();
    let mapping = backend.audit_source_mapping("physical").await.unwrap();
    assert_eq!(mapping.library_id, "physical");
    backend
        .freeze_motion_source("archived-library", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("archived-library")
        .await
        .unwrap();
    backend.stage_import("archived-library").await.unwrap();
    let inventory = backend
        .motion_reference_inventory("archived-library")
        .await
        .unwrap();
    assert!(inventory.activation_ready);
    assert_eq!(inventory.qualification, "physical_source");
    assert_eq!(inventory.unresolved_evidence, 0);
    backend
        .prepare_motion_cutover("archived-library")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(Duration::from_secs(30))
        .await
        .unwrap();
    let binding: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&pool)
            .await
            .unwrap();
    drop(backend);
    pool.close().await;
    let reference = crate::offline_control_upgrade::UpgradeReference::build()
        .await
        .unwrap();
    let upgraded = reference
        .upgrade(
            File::open(&motion).unwrap(),
            &motion,
            &catabolic_core::sha256(std::fs::read(&motion).unwrap()),
            &catabolic_core::sha256(binding.as_bytes()),
            0,
            &AtomicBool::new(false),
        )
        .unwrap();
    drop(upgraded);
    let db = Connection::open(motion).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON; PRAGMA synchronous=FULL; BEGIN IMMEDIATE").unwrap();
    (root, db, identity)
}
fn capture(db: &Connection, id: &Identity) -> RetainedImportedArchive {
    RetainedImportedArchive::capture_in(db, id, LIMIT, deadline(), &AtomicBool::new(false)).unwrap()
}
#[tokio::test]
async fn actual_export_activation_and_immutable_library_bridge_capture() {
    let (_root, db, id) = fixture().await;
    let witness = capture(&db, &id);
    let bridge = witness.library_bridge("motion.library:archived").unwrap();
    assert_eq!(bridge.import_id(), "archived-library");
    assert_eq!(bridge.motion_base(), 7);
    assert_eq!(bridge.catalog_id(), id.catalog_id);
    assert_eq!(bridge.incarnation(), id.incarnation);
    assert!(bridge.native_base() >= 1);
    assert_eq!(bridge.item_id(), "motion.library:archived");
    assert!(!bridge.proof_raw().is_empty());
    assert_eq!(witness.parts().count(), 1);
    assert!(witness.library_bridge("unknown").is_err());
    witness
        .verify_in(&db, &id, LIMIT, deadline(), &AtomicBool::new(false))
        .unwrap();
    assert!(witness.retained_allocations().unwrap() > 0);
    let transferred = witness
        .into_transferred(&db, &id, LIMIT, deadline(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(transferred.parts().len(), 1);
    assert_eq!(
        transferred
            .library_bridge("motion.library:archived")
            .unwrap()
            .motion_base(),
        7
    );
    assert!(transferred.retained_allocations().unwrap() > 0);
}
#[tokio::test]
async fn archive_refuses_foreign_identity_ended_writer_and_simultaneous_overflow() {
    let (root, db, id) = fixture().await;
    let witness = capture(&db, &id);
    for foreign in [
        Identity {
            catalog_id: "foreign".into(),
            ..id.clone()
        },
        Identity {
            incarnation: "foreign".into(),
            ..id.clone()
        },
        Identity {
            schema_version: 41,
            ..id.clone()
        },
    ] {
        assert!(
            RetainedImportedArchive::capture_in(
                &db,
                &foreign,
                LIMIT,
                deadline(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
    assert!(
        RetainedImportedArchive::capture_in(&db, &id, 1, deadline(), &AtomicBool::new(false))
            .is_err()
    );
    assert!(
        witness
            .verify_in(
                &db,
                &id,
                witness.retained_allocations().unwrap(),
                deadline(),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(
        RetainedImportedArchive::capture_in(&db, &id, LIMIT, deadline(), &AtomicBool::new(true))
            .is_err()
    );
    assert!(
        RetainedImportedArchive::capture_in(
            &db,
            &id,
            LIMIT,
            Instant::now(),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    db.execute_batch("ROLLBACK").unwrap();
    assert!(
        witness
            .verify_in(&db, &id, LIMIT, deadline(), &AtomicBool::new(false))
            .is_err()
    );
    let foreign = Connection::open(root.path().join("motion.sqlite3")).unwrap();
    foreign.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(
        witness
            .verify_in(&foreign, &id, LIMIT, deadline(), &AtomicBool::new(false))
            .is_err()
    );
}
fn corrupt_bridge(db: &Connection, sql: &str) {
    // Test-only corruption under a savepoint. Restore every exact trigger byte
    // before attempting admission, so missing guards never explain refusal.
    let guards:Vec<(String,String)>=db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='trigger' AND tbl_name='catabolic_revision_bridges' ORDER BY name").unwrap()
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(!guards.is_empty());
    for (name, _) in &guards {
        db.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
    }
    db.execute_batch(sql).unwrap();
    for (_, sql) in &guards {
        db.execute_batch(sql).unwrap();
    }
    crate::source_freeze::verify_sync(db).unwrap();
}
#[tokio::test]
async fn archive_missing_foreign_tampered_bridge_and_changed_original_refuse() {
    let (_root, db, id) = fixture().await;
    let witness = capture(&db, &id);
    for sql in [
        "DELETE FROM catabolic_revision_bridges WHERE native_id='motion.library:archived'",
        "UPDATE catabolic_revision_bridges SET catalog_id='foreign'",
        "UPDATE catabolic_revision_bridges SET incarnation='foreign'",
        "UPDATE catabolic_revision_bridges SET native_id='motion.library:other'",
        "UPDATE catabolic_revision_bridges SET motion_base=8",
        "UPDATE catabolic_revision_bridges SET native_base=native_base+1",
        "UPDATE catabolic_revision_bridges SET proof_json=json_set(proof_json,'$.import_id','other')",
    ] {
        db.execute_batch("SAVEPOINT corruption").unwrap();
        corrupt_bridge(&db, sql);
        assert!(
            RetainedImportedArchive::capture_in(
                &db,
                &id,
                LIMIT,
                deadline(),
                &AtomicBool::new(false)
            )
            .is_err(),
            "{sql}"
        );
        assert!(
            witness
                .verify_in(&db, &id, LIMIT, deadline(), &AtomicBool::new(false))
                .is_err(),
            "{sql}"
        );
        db.execute_batch("ROLLBACK TO corruption; RELEASE corruption")
            .unwrap();
    }
    db.execute_batch("SAVEPOINT corruption").unwrap();
    corrupt_bridge(
        &db,
        "UPDATE catabolic_revision_bridges SET native_base=native_base+1,proof_json=json_set(proof_json,'$.native_base',native_base+1)",
    );
    // A coherent forged baseline needs the original publication witness, not
    // merely a row-shape validator. Exact retained bytes reject the change.
    assert!(
        witness
            .verify_in(&db, &id, LIMIT, deadline(), &AtomicBool::new(false))
            .is_err()
    );
    db.execute_batch("ROLLBACK TO corruption; RELEASE corruption")
        .unwrap();
    witness
        .verify_in(&db, &id, LIMIT, deadline(), &AtomicBool::new(false))
        .unwrap();
}
