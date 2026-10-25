//! Exact temporary sealed-store corridor: retain93 while only compiled97 is
//! pending. This does not execute97 or grant native catalog admission.
use super::*;
use anyhow::ensure;
use std::borrow::Cow;

/// Ordered94–97 uses a distinct exact reference, never the deferred97 corridor.
pub(super) async fn ordered97(pool: &SqlitePool) -> anyhow::Result<bool> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let selected: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version BETWEEN 94 AND 96)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if selected {
        motion_catalog_backend::verify_ordered97_in(&mut tx).await?;
        motion_catalog_backend::control_migration::verify_sealed_compatibility_in(&mut tx).await?;
    }
    tx.rollback().await?;
    Ok(selected)
}

pub(super) async fn migrator(
    pool: &SqlitePool,
    full: &sqlx::migrate::Migrator,
) -> anyhow::Result<sqlx::migrate::Migrator> {
    let suffix: Vec<_> = full.iter().filter(|m| m.version > 93).collect();
    ensure!(suffix.len() == 1, "unqualified sealed migration suffix");
    let next = suffix[0];
    use sha2::Digest;
    let resource = include_str!("../../migrations/0097_preview_sheet_artifacts.sql");
    ensure!(
        next.version == 97
            && !next.migration_type.is_down_migration()
            && next.sql.as_ref() == resource
            && next.checksum.as_ref() == sha2::Sha384::digest(resource.as_bytes()).as_slice(),
        "unqualified sealed deferred migration resource"
    );
    let compiled = sqlx::migrate!("./migrations");
    let known_prefix: Vec<_> = compiled.iter().filter(|m| m.version <= 93).collect();
    let offered_prefix: Vec<_> = full.iter().filter(|m| m.version <= 93).collect();
    ensure!(
        known_prefix.len() == offered_prefix.len()
            && known_prefix
                .iter()
                .zip(&offered_prefix)
                .all(|(a, b)| a.version == b.version
                    && a.sql == b.sql
                    && a.checksum == b.checksum
                    && !b.migration_type.is_down_migration()),
        "sealed compiled prefix differs"
    );
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&mut *tx)
        .await?;
    ensure!(
        (1..=94).contains(&count),
        "sealed ledger count exceeds closed prefix"
    );
    let bad_checksums: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE typeof(checksum)!='blob' OR length(checksum)!=48")
        .fetch_one(&mut *tx).await?;
    ensure!(bad_checksums == 0, "sealed checksum storage invalid");
    let ledger: Vec<(i64, i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *tx)
            .await?;
    let current = ledger.last().context("sealed migration ledger empty")?.0;
    ensure!(
        (83..=93).contains(&current) || current == 97,
        "unsupported sealed compatibility schema"
    );
    let prefix: Vec<_> = compiled.iter().filter(|m| m.version <= current).collect();
    ensure!(
        prefix.len() == ledger.len(),
        "sealed migration ledger gap or extra row"
    );
    for (actual, expected) in ledger.iter().zip(prefix) {
        ensure!(
            actual.0 == expected.version
                && actual.1 == 1
                && !expected.migration_type.is_down_migration()
                && actual.2.as_slice() == expected.checksum.as_ref(),
            "sealed migration ledger mismatch"
        );
    }
    // Rebuild only compiled schema in an isolated in-memory reference. No
    // caller SQL or application hooks execute on the retained source.
    let mut reference_migrations = sqlx::migrate!("./migrations");
    reference_migrations.migrations = Cow::Owned(
        compiled
            .iter()
            .filter(|m| m.version <= current)
            .cloned()
            .collect(),
    );
    let reference = SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(crate::catalog_backend::projection::configure_connection(
                connection,
            ))
        })
        .connect_with(
            SqliteConnectOptions::new()
                .filename(":memory:")
                .foreign_keys(true),
        )
        .await?;
    reference_migrations.run(&reference).await?;
    let statistics: i64 = sqlx::query_scalar("SELECT count(*) FROM sqlite_schema WHERE name IN ('sqlite_stat1','sqlite_stat2','sqlite_stat3','sqlite_stat4')")
        .fetch_one(&mut *tx).await?;
    ensure!(
        statistics == 0,
        "sealed compatibility statistics schema requires explicit review; ANALYZE/optimize tables are not admitted by this corridor"
    );
    type Shape = (String, String, String, Option<String>);
    const SHAPE: &str = "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE NOT (type='trigger' AND name GLOB 'motion_source_freeze_*') ORDER BY type,name";
    let (objects, bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*),coalesce(sum(length(CAST(type AS BLOB))+length(CAST(name AS BLOB))+length(CAST(tbl_name AS BLOB))+coalesce(length(CAST(sql AS BLOB)),0)),0) FROM sqlite_schema",
    )
    .fetch_one(&mut *tx)
    .await?;
    ensure!(
        (0..=10000).contains(&objects) && (0..=16777216).contains(&bytes),
        "sealed schema inventory exceeds bound"
    );
    let actual: Vec<Shape> = sqlx::query_as(SHAPE).fetch_all(&mut *tx).await?;
    let expected: Vec<Shape> = sqlx::query_as(SHAPE).fetch_all(&reference).await?;
    ensure!(
        actual == expected,
        "sealed compatibility schema differs from compiled prefix"
    );
    crate::catalog_backend::control_migration::verify_sealed_compatibility_in(&mut tx).await?;
    tx.rollback().await?;
    reference.close().await;
    let ceiling = if current == 97 { 97 } else { 93 };
    let mut target = sqlx::migrate!("./migrations");
    target.migrations = Cow::Owned(
        compiled
            .iter()
            .filter(|m| m.version <= ceiling)
            .cloned()
            .collect(),
    );
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn sealed(change: Option<&str>) -> (tempfile::TempDir, std::path::PathBuf, SqlitePool) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("motion.sqlite3");
        let db = open(&path).await.unwrap();
        let mut prefix = sqlx::migrate!("./migrations");
        prefix.migrations =
            Cow::Owned(prefix.iter().filter(|m| m.version <= 93).cloned().collect());
        prefix.run(&db).await.unwrap();
        if let Some(sql) = change {
            sqlx::raw_sql(sql).execute(&db).await.unwrap();
        }
        let native = root.path().join("native.sqlite3");
        crate::catalog_backend::initialize(native.clone())
            .await
            .unwrap();
        let identity = crate::catalog_backend::inspect(native.clone())
            .await
            .unwrap()
            .identity;
        let backend = crate::catalog_backend::NativeCatalog::bind(db.clone(), native, identity)
            .await
            .unwrap();
        backend.freeze_motion_source("retained93", 0).await.unwrap();
        backend.seal_motion_freeze(1).await.unwrap();
        drop(backend);
        (root, path, db)
    }
    #[tokio::test]
    async fn exact_retained93_reopens_without_backup_or_preview97_effect() {
        let (root, path, db) = sealed(None).await;
        db.close().await;
        for _ in 0..2 {
            let db = connect_catalog_control(&path).await.unwrap();
            let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&db)
                .await
                .unwrap();
            assert_eq!(version, 93);
            let preview: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM sqlite_schema WHERE name='preview_sheet_artifacts'",
            )
            .fetch_one(&db)
            .await
            .unwrap();
            assert_eq!(preview, 0);
            assert!(!root.path().join("upgrade-backups").exists());
            db.close().await;
        }
    }
    #[tokio::test]
    async fn unknown_suffix_changed_checksum_and_partial97_refuse_before_backup() {
        let (root, path, db) = sealed(None).await;
        for change in 0..3 {
            let mut offered = sqlx::migrate!("./migrations");
            let mut migrations = offered.iter().cloned().collect::<Vec<_>>();
            let last = migrations.last_mut().unwrap();
            match change {
                0 => last.version = 98,
                1 => last.checksum = Cow::Owned(vec![0; 48]),
                _ => last.sql = Cow::Borrowed("CREATE TABLE unrelated(value)"),
            }
            offered.migrations = Cow::Owned(migrations);
            assert!(migrator(&db, &offered).await.is_err());
        }
        db.close().await;
        assert!(!root.path().join("upgrade-backups").exists());
        drop(path);
        for sql in [
            "CREATE TABLE preview_sheet_artifacts(unreviewed TEXT)",
            "ANALYZE",
            "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version=93",
        ] {
            let (root, path, db) = sealed(Some(sql)).await;
            db.close().await;
            assert!(connect_catalog_control(&path).await.is_err());
            assert!(!root.path().join("upgrade-backups").exists());
        }
    }
    #[tokio::test]
    async fn fresh97_remains_supported_without_deferred_corridor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fresh.sqlite3");
        let db = connect_catalog_control(&path).await.unwrap();
        let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(version, 97);
        let table: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_schema WHERE name='preview_sheet_artifacts'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(table, 1);
        let native = root.path().join("native.sqlite3");
        crate::catalog_backend::initialize(native.clone())
            .await
            .unwrap();
        let identity = crate::catalog_backend::inspect(native.clone())
            .await
            .unwrap()
            .identity;
        let backend = crate::catalog_backend::NativeCatalog::bind(db.clone(), native, identity)
            .await
            .unwrap();
        backend.freeze_motion_source("fresh97", 0).await.unwrap();
        backend.seal_motion_freeze(1).await.unwrap();
        drop(backend);
        db.close().await;
        let reopened = connect_catalog_control(&path).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT max(version) FROM _sqlx_migrations")
                .fetch_one(&reopened)
                .await
                .unwrap(),
            97
        );
        reopened.close().await;
    }
    #[tokio::test]
    async fn genuine_ordered97_reopens_without_upgrade_and_refuses_extra_schema() {
        let (root, path, db) = sealed(None).await;
        let raw: String =
            sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
                .fetch_one(&db)
                .await
                .unwrap();
        db.close().await;
        let lock = std::fs::File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.path().join("server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        motion_catalog_backend::OfflineControlUpgrade::prepare()
            .await
            .unwrap()
            .upgrade(
                std::fs::File::open(&path).unwrap(),
                &path,
                &catabolic_core::sha256(std::fs::read(&path).unwrap()),
                &catabolic_core::sha256(raw.as_bytes()),
                motion_catalog_backend::OfflineControlShutdownAcknowledgment::for_stopped_target(
                    &path,
                    motion_catalog_backend::OfflineControlShutdownFacts {
                        service_admission_stopped: true,
                        native_workers_joined: true,
                        application_owners_drained: true,
                        exclusive_operational_ownership_retained: true,
                    },
                )
                .unwrap(),
                0,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap()
            .close()
            .unwrap();
        for _ in 0..2 {
            let reopened = connect_catalog_control(&path).await.unwrap();
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM _sqlx_migrations WHERE version BETWEEN 94 AND 97"
                )
                .fetch_one(&reopened)
                .await
                .unwrap(),
                4
            );
            reopened.close().await;
        }
        assert!(!root.path().join("upgrade-backups").exists());
        let extra = open(&path).await.unwrap();
        sqlx::query("CREATE TABLE unrelated_extra(value)")
            .execute(&extra)
            .await
            .unwrap();
        extra.close().await;
        assert!(connect_catalog_control(&path).await.is_err());
    }
}
