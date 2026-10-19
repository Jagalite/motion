use playscale::{
    catalog_backend::{NativeCatalog, initialize, inspect},
    db,
};
use std::time::Duration;
#[tokio::test]
async fn both_database_owners_share_sqlite_and_reopen_after_migration() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("native.sqlite3");
    initialize(path.clone()).await.unwrap();
    let db = db::connect(&root.join("motion.sqlite3")).await.unwrap();
    let native = NativeCatalog::bind(
        db.clone(),
        path.clone(),
        inspect(path).await.unwrap().identity,
    )
    .await
    .unwrap();
    let motion: String = sqlx::query_scalar("SELECT sqlite_source_id()")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(native.status().await.unwrap().sqlite_source_id, motion);
    let math: f64 = sqlx::query_scalar("SELECT sqrt(9)")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(math, 3.0);
    assert!(
        sqlx::query_scalar::<_, String>("SELECT soundex('a')")
            .fetch_one(&db)
            .await
            .is_err()
    );
    native.drain(Duration::from_secs(1)).await.unwrap();
    db.close().await;
    let reopened = db::connect(&root.join("motion.sqlite3")).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success=1")
        .fetch_one(&reopened)
        .await
        .unwrap();
    // Migration numbers have historical gaps; count actual embedded migrations.
    assert_eq!(
        count,
        sqlx::migrate!("./migrations").migrations.len() as i64
    );
    let latest: i64 =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&reopened)
            .await
            .unwrap();
    assert_eq!(latest, 69);
    reopened.close().await;
}

#[tokio::test]
async fn application_startup_refuses_frozen_source_but_operator_control_can_abandon() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let native_path = root.join("native.sqlite3");
    let motion_path = root.join("motion.sqlite3");
    initialize(native_path.clone()).await.unwrap();
    let motion = db::connect(&motion_path).await.unwrap();
    let native = NativeCatalog::bind(
        motion.clone(),
        native_path.clone(),
        inspect(native_path.clone()).await.unwrap().identity,
    )
    .await
    .unwrap();
    native.freeze_motion_source("cutover", 0).await.unwrap();
    native.drain(Duration::from_secs(1)).await.unwrap();
    motion.close().await;
    assert!(
        db::connect(&motion_path)
            .await
            .unwrap_err()
            .to_string()
            .contains("source is frozen")
    );
    let control = db::connect_catalog_control(&motion_path).await.unwrap();
    let native = NativeCatalog::bind(
        control.clone(),
        native_path.clone(),
        inspect(native_path).await.unwrap().identity,
    )
    .await
    .unwrap();
    assert!(!native.source_state().await.unwrap().writes_allowed());
    native.abandon_motion_freeze(1).await.unwrap();
    control.close().await;
    db::connect(&motion_path).await.unwrap().close().await;
}

#[tokio::test]
async fn startup_does_not_trust_live_flag_when_core_state_is_invalid() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("motion.sqlite3");
    let pool = db::connect(&path).await.unwrap();
    // SQL projection agrees with mode/revision, but its history is impossible.
    sqlx::query("UPDATE catabolic_source_state SET state_json=?")
        .bind(r#"{"revision":0,"mode":{"kind":"live"},"used_migrations":{"invalid":99}}"#)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    assert!(
        db::connect(&path)
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid persisted source admission")
    );
    assert!(db::connect_catalog_control(&path).await.is_err());
}
