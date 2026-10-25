//! Migration runs through real SQLx transactions, including sealed source guards.
use motion_catalog_backend::{NativeCatalog, initialize, inspect};
use sqlx::SqlitePool;

const REBUILD: &str = include_str!("../../../migrations/0084_catabolic_retired_locators.sql");
async fn old_database() -> (tempfile::TempDir, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|c, _| Box::pin(motion_catalog_backend::projection::configure_connection(c)))
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(dir.path().join("motion.sqlite3"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("../../migrations");
    migrations.migrations = std::borrow::Cow::Owned(
        migrations
            .iter()
            .filter(|m| m.version < 84)
            .cloned()
            .collect(),
    );
    migrations.run(&db).await.unwrap();
    sqlx::raw_sql("INSERT INTO libraries(id,name,root,root_identity) VALUES('s','Source','/fixture','root'); INSERT INTO items(id,title,kind) VALUES('w','Work','video'); INSERT INTO editions(id,item_id,label) VALUES('e','w','Edition'); INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES('old','e','s','real.mp4','original-revision','original-fingerprint',123); INSERT INTO nfo_origins VALUES('w','old'); INSERT INTO sidecar_subtitles VALUES('old','captions.srt','subtitle-fingerprint',NULL,0,0,'srt');").execute(&db).await.unwrap();
    (dir, db)
}
async fn schema(db: &SqlitePool) -> Vec<(String, String)> {
    sqlx::query_as("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
        .fetch_all(db)
        .await
        .unwrap()
}
#[tokio::test]
async fn rebuild_preserves_sealed_guards_fk_ids_real_paths_and_events() {
    let (dir, db) = old_database().await;
    let native = dir.path().join("native.sqlite3");
    initialize(native.clone()).await.unwrap();
    let backend = NativeCatalog::bind(
        db.clone(),
        native.clone(),
        inspect(native).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend
        .freeze_motion_source("schema-rebuild", 0)
        .await
        .unwrap();
    let guards: Vec<(String, String)> = sqlx::query_as(
        "SELECT trigger_name,trigger_sql FROM catabolic_source_guards ORDER BY trigger_name",
    )
    .fetch_all(&db)
    .await
    .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&db)
        .await
        .unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT catalog_revision FROM items WHERE id='w'")
        .fetch_one(&db)
        .await
        .unwrap();
    motion_catalog_backend::control_migration::migrate(
        &dir.path().join("motion.sqlite3"),
        &sqlx::migrate!("../../migrations"),
    )
    .await
    .unwrap();
    let saved: (String, String, String, i64) =
        sqlx::query_as("SELECT id,relative_path,revision,native_history FROM media_files")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        saved,
        (
            "old".into(),
            "real.mp4".into(),
            "original-revision".into(),
            0
        )
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT file_id FROM nfo_origins")
            .fetch_one(&db)
            .await
            .unwrap(),
        "old"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT file_id FROM sidecar_subtitles")
            .fetch_one(&db)
            .await
            .unwrap(),
        "old"
    );
    assert_eq!(
        events,
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM change_events")
            .fetch_one(&db)
            .await
            .unwrap()
    );
    assert_eq!(
        revision,
        sqlx::query_scalar::<_, i64>("SELECT catalog_revision FROM items WHERE id='w'")
            .fetch_one(&db)
            .await
            .unwrap()
    );
    assert_eq!(
        guards,
        sqlx::query_as::<_, (String, String)>(
            "SELECT trigger_name,trigger_sql FROM catabolic_source_guards ORDER BY trigger_name"
        )
        .fetch_all(&db)
        .await
        .unwrap()
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&db)
            .await
            .unwrap()
            .is_empty()
    );
    // Guard verification runs on the next source transition; no journal rewrite.
    backend.seal_motion_freeze(1).await.unwrap();
    assert!(
        sqlx::query("UPDATE _sqlx_migrations SET execution_time=0 WHERE version=84")
            .execute(&db)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT motion_native_projection('_sqlx_migrations')")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    assert!(
        sqlx::query("UPDATE media_files SET native_history=1,available=0")
            .execute(&db)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn failed_rebuild_rolls_back_schema_rows_and_guard_state() {
    for boundary in [
        "DROP TABLE media_files;",
        "INSERT INTO motion_rebuild_assert VALUES(NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check));",
    ] {
        let (_dir, db) = old_database().await;
        let before = schema(&db).await;
        let end = REBUILD.rfind(boundary).unwrap() + boundary.len();
        let script = format!(
            "{}\nINSERT INTO motion_rebuild_assert VALUES(0);",
            &REBUILD[..end]
        );
        let mut tx = db.begin().await.unwrap();
        assert!(sqlx::raw_sql(&script).execute(&mut *tx).await.is_err());
        tx.rollback().await.unwrap();
        assert_eq!(before, schema(&db).await);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT relative_path FROM media_files WHERE id='old'")
                .fetch_one(&db)
                .await
                .unwrap(),
            "real.mp4"
        );
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&db)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
#[tokio::test]
async fn unknown_schema_is_rejected_without_losing_existing_rows() {
    let (_dir, db) = old_database().await;
    sqlx::query("CREATE INDEX unreviewed_file_index ON media_files(bytes)")
        .execute(&db)
        .await
        .unwrap();
    let before = schema(&db).await;
    assert!(sqlx::migrate!("../../migrations").run(&db).await.is_err());
    assert_eq!(before, schema(&db).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM media_files")
            .fetch_one(&db)
            .await
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn retired_locator_preserves_original_id_and_allows_only_one_current_row() {
    let (_dir, db) = old_database().await;
    sqlx::migrate!("../../migrations").run(&db).await.unwrap();
    sqlx::query("UPDATE media_files SET native_history=1,available=0 WHERE id='old'")
        .execute(&db)
        .await
        .unwrap();
    let insert = "INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES(?,'e','s','real.mp4','new-revision','new-fingerprint',456)";
    sqlx::query(insert)
        .bind("current")
        .execute(&db)
        .await
        .unwrap();
    assert!(
        sqlx::query(insert)
            .bind("duplicate")
            .execute(&db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE media_files SET available=1 WHERE id='old'")
            .execute(&db)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT file_id FROM nfo_origins")
            .fetch_one(&db)
            .await
            .unwrap(),
        "old"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT relative_path FROM media_files WHERE id='old'")
            .fetch_one(&db)
            .await
            .unwrap(),
        "real.mp4"
    );
}

#[tokio::test]
async fn preexisting_fk_corruption_is_not_normalized_by_rebuild() {
    let (_dir, db) = old_database().await;
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys=OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE nfo_origins SET file_id='missing'")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    let before = schema(&db).await;
    assert!(sqlx::migrate!("../../migrations").run(&db).await.is_err());
    assert_eq!(before, schema(&db).await);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT file_id FROM nfo_origins")
            .fetch_one(&db)
            .await
            .unwrap(),
        "missing"
    );
}

#[tokio::test]
async fn missing_sealed_source_guards_are_not_silently_repaired() {
    let (dir, db) = old_database().await;
    let native = dir.path().join("native.sqlite3");
    initialize(native.clone()).await.unwrap();
    let backend = NativeCatalog::bind(
        db.clone(),
        native.clone(),
        inspect(native).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend.freeze_motion_source("guard-loss", 0).await.unwrap();
    backend.seal_motion_freeze(1).await.unwrap();
    sqlx::raw_sql("DROP TRIGGER motion_source_freeze_insert_media_files; DROP TRIGGER motion_source_freeze_update_media_files; DROP TRIGGER motion_source_freeze_delete_media_files;").execute(&db).await.unwrap();
    let before = schema(&db).await;
    assert!(sqlx::migrate!("../../migrations").run(&db).await.is_err());
    assert_eq!(before, schema(&db).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM catabolic_source_guards WHERE table_name='media_files'"
        )
        .fetch_one(&db)
        .await
        .unwrap(),
        3
    );
}

#[tokio::test]
async fn failed_qualified_migration_keeps_ledger_and_catalog_closed() {
    let (dir, db) = old_database().await;
    let native = dir.path().join("native.sqlite3");
    initialize(native.clone()).await.unwrap();
    let backend = NativeCatalog::bind(
        db.clone(),
        native.clone(),
        inspect(native).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend
        .freeze_motion_source("failed-schema-evolution", 0)
        .await
        .unwrap();
    // This unrelated schema extension must not be silently discarded by rebuild.
    sqlx::query("CREATE INDEX unexpected_locator_extension ON media_files(bytes)")
        .execute(&db)
        .await
        .unwrap();
    let before = schema(&db).await;
    assert!(
        motion_catalog_backend::control_migration::migrate(
            &dir.path().join("motion.sqlite3"),
            &sqlx::migrate!("../../migrations"),
        )
        .await
        .is_err()
    );
    assert_eq!(before, schema(&db).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&db)
            .await
            .unwrap(),
        83
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT motion_native_projection('_sqlx_migrations')")
            .fetch_one(&db)
            .await
            .unwrap(),
        0
    );
    assert!(
        sqlx::query("UPDATE _sqlx_migrations SET execution_time=execution_time+1 WHERE version=83")
            .execute(&db)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE media_files SET available=0 WHERE id='old'")
            .execute(&db)
            .await
            .is_err()
    );
    assert!(!backend.source_state().await.unwrap().writes_allowed());
}

#[tokio::test]
async fn sealed_control_chain_resumes_exact_prefix_and_rejects_changed_sql() {
    let (dir, db) = old_database().await;
    let native = dir.path().join("native.sqlite3");
    initialize(native.clone()).await.unwrap();
    let backend = NativeCatalog::bind(
        db.clone(),
        native.clone(),
        inspect(native).await.unwrap().identity,
    )
    .await
    .unwrap();
    backend
        .freeze_motion_source("additive-control-chain", 0)
        .await
        .unwrap();
    backend.seal_motion_freeze(1).await.unwrap();
    let path = dir.path().join("motion.sqlite3");
    let prefix = |version| {
        let mut migrations = sqlx::migrate!("../../migrations");
        migrations.migrations = std::borrow::Cow::Owned(
            migrations
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        );
        migrations
    };
    motion_catalog_backend::control_migration::migrate(&path, &prefix(84))
        .await
        .unwrap();
    let mut changed = prefix(85);
    let mut entries = changed.iter().cloned().collect::<Vec<_>>();
    let last = entries.last_mut().unwrap();
    last.sql = std::borrow::Cow::Owned(format!(
        "{}\nCREATE TABLE unexpected_control(id TEXT);",
        last.sql
    ));
    changed.migrations = std::borrow::Cow::Owned(entries);
    assert!(
        motion_catalog_backend::control_migration::migrate(&path, &changed)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&db)
            .await
            .unwrap(),
        84
    );
    motion_catalog_backend::control_migration::migrate(&path, &prefix(85))
        .await
        .unwrap();
    assert!(!backend.source_state().await.unwrap().writes_allowed());
    motion_catalog_backend::control_migration::migrate(&path, &prefix(86))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&db)
            .await
            .unwrap(),
        86
    );
    assert!(!backend.source_state().await.unwrap().writes_allowed());
    assert!(
        sqlx::query("UPDATE _sqlx_migrations SET execution_time=0 WHERE version=86")
            .execute(&db)
            .await
            .is_err()
    );
}
