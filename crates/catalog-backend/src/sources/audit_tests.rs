use super::*;
use std::{borrow::Cow, os::unix::fs::MetadataExt};

struct Fixture {
    _root: tempfile::TempDir,
    pool: sqlx::SqlitePool,
    backend: NativeCatalog,
    native: PathBuf,
    media: PathBuf,
}
impl Fixture {
    async fn new(mapped: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let native = base.join("native.sqlite3");
        let media = base.join("media");
        std::fs::create_dir(&media).unwrap();
        crate::initialize(native.clone()).await.unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("motion.sqlite3"))
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
        let md = std::fs::metadata(&media).unwrap();
        sqlx::query("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('physical','Physical',?,?,0,1,1,1,'[]',0)")
            .bind(media.to_str().unwrap()).bind(format!("{}:{}",md.dev(),md.ino())).execute(&pool).await.unwrap();
        let backend = NativeCatalog::bind(
            pool.clone(),
            native.clone(),
            crate::inspect(native.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        if mapped {
            backend
                .register_source("physical", "default", "native-source", media.clone())
                .await
                .unwrap();
        }
        Self {
            _root: root,
            pool,
            backend,
            native,
            media,
        }
    }
    async fn capture(&self) -> anyhow::Result<()> {
        self.backend.freeze_motion_source("audit", 0).await?;
        self.backend.capture_motion_import("audit").await?;
        Ok(())
    }
    async fn no_capture(&self) {
        for table in [
            "catabolic_motion_exports",
            "catabolic_reference_audits",
            "catabolic_import_snapshots",
        ] {
            let count: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {table} WHERE import_id='audit'"
            ))
            .fetch_one(&self.pool)
            .await
            .unwrap();
            assert_eq!(count, 0, "failed audit published {table}");
        }
    }
    async fn source_ids(&self) -> Value {
        let raw: String = sqlx::query_scalar(
            "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='audit'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap();
        serde_json::from_str::<Value>(&raw).unwrap()["physical_coverage"]["preserved_ids"]["libraries"].clone()
    }
}

#[tokio::test]
async fn one_connection_retained_writer_qualifies_and_publishes_exact_source() {
    let f = Fixture::new(true).await;
    let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        f.backend
            .qualified_source_mapping_in(&mut tx, "physical")
            .await
            .unwrap()
            .is_some()
    );
    tx.rollback().await.unwrap();
    f.capture().await.unwrap();
    assert_eq!(f.source_ids().await, json!(["physical"]));
}

#[tokio::test]
async fn missing_stale_and_offline_sources_remain_explicit_nonqualification() {
    for mode in 0..3 {
        let f = Fixture::new(mode != 0).await;
        if mode == 1 {
            sqlx::query(
                "UPDATE libraries SET binding_revision=binding_revision+1 WHERE id='physical'",
            )
            .execute(&f.pool)
            .await
            .unwrap();
        } else if mode == 2 {
            std::fs::rename(&f.media, f.media.with_file_name("offline")).unwrap();
        }
        let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(
            f.backend
                .qualified_source_mapping_in(&mut tx, "physical")
                .await
                .unwrap()
                .is_none()
        );
        tx.rollback().await.unwrap();
        // Ordinary auditing still refuses these cases, rather than returning a grant.
        assert!(f.backend.audit_source_mapping("physical").await.is_err());
        f.capture().await.unwrap();
        assert_eq!(f.source_ids().await, json!([]));
    }
}

#[tokio::test]
async fn malformed_mapping_propagates_and_does_not_publish_capture() {
    let f = Fixture::new(false).await;
    // Deliberate invalid persisted input, never a fabricated qualified source.
    sqlx::query("INSERT INTO catabolic_source_mappings(library_id,profile,native_source,mapping_json,created_at) VALUES('physical','default','native-source','{',0)")
        .execute(&f.pool).await.unwrap();
    let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let error = f
        .backend
        .qualified_source_mapping_in(&mut tx, "physical")
        .await
        .unwrap_err();
    assert!(error.is::<serde_json::Error>());
    tx.rollback().await.unwrap();
    assert!(f.capture().await.is_err());
    f.no_capture().await;
}

#[tokio::test]
async fn sql_failure_is_not_a_missing_mapping_and_does_not_publish_capture() {
    let f = Fixture::new(true).await;
    sqlx::query("DROP TABLE catabolic_source_registry")
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let error = f
        .backend
        .qualified_source_mapping_in(&mut tx, "physical")
        .await
        .unwrap_err();
    assert!(error.is::<sqlx::Error>());
    tx.rollback().await.unwrap();
    assert!(f.capture().await.is_err());
    f.no_capture().await;
}

#[tokio::test]
async fn unavailable_native_storage_is_error_not_physical_exclusion() {
    let f = Fixture::new(true).await;
    std::fs::rename(&f.native, f.native.with_file_name("unavailable.sqlite3")).unwrap();
    let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        f.backend
            .qualified_source_mapping_in(&mut tx, "physical")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(f.capture().await.is_err());
    f.no_capture().await;
}

#[tokio::test]
async fn closed_pool_error_never_publishes_capture() {
    let f = Fixture::new(true).await;
    f.pool.close().await;
    let error = f.backend.capture_motion_import("audit").await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<sqlx::Error>(),
        Some(sqlx::Error::PoolClosed)
    ));
    use sqlx::Connection;
    let mut db = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(f._root.path().join("motion.sqlite3"))
            .read_only(true),
    )
    .await
    .unwrap();
    for table in [
        "catabolic_motion_exports",
        "catabolic_reference_audits",
        "catabolic_import_snapshots",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE import_id='audit'"
        ))
        .fetch_one(&mut db)
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn valid_json_invalid_identity_schema_and_mapping_abort_capture() {
    for mode in 0..11 {
        let f = Fixture::new(true).await;
        let mut mapping = f.backend.source_mapping("physical").await.unwrap();
        match mode {
            0 => mapping.catalog.catalog_id.clear(),
            1 => mapping.catalog.schema_version = u32::MAX,
            2 => mapping.profile.clear(),
            3 => mapping.motion_root_identity.clear(),
            4 => mapping.motion_binding_revision = 0,
            5 => mapping.native_binding = Value::Null,
            6 => mapping.native_policy = json!("not a policy"),
            7 => mapping.native_policy["include_regex"] = json!(["["]),
            8 => mapping.native_policy["budgets"] = json!({"unknown": 1}),
            9 => mapping.native_binding["inode"] = json!(u64::MAX),
            _ => mapping.native_binding["root"] = json!("relative/root"),
        }
        // Test-only persisted corruption. Restore the EXACT immutable trigger
        // in the same transaction before invoking the unchanged production audit.
        let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let sql: String = sqlx::query_scalar("SELECT sql FROM sqlite_schema WHERE type='trigger' AND name='catabolic_source_mapping_immutable_update'")
            .fetch_one(&mut *tx).await.unwrap();
        sqlx::query("DROP TRIGGER catabolic_source_mapping_immutable_update")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE catabolic_source_mappings SET mapping_json=? WHERE library_id='physical'",
        )
        .bind(serde_json::to_string(&mapping).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::raw_sql(&sql).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(
            f.backend
                .qualified_source_mapping_in(&mut tx, "physical")
                .await
                .is_err(),
            "corrupt mode {mode} must not be nonqualification"
        );
        tx.rollback().await.unwrap();
        assert!(
            f.capture().await.is_err(),
            "corrupt mode {mode} published capture"
        );
        f.no_capture().await;
    }
}

#[test]
fn only_explicit_typed_mismatches_are_qualification_refusals() {
    use playscale_core::{catalog_backend::AdmissionError, catalog_registration::Rejection};
    for error in [
        AdmissionError::InvalidIdentity,
        AdmissionError::SchemaChanged,
    ] {
        assert!(
            !catalog_qualification_error(error, anyhow::anyhow!("fixture"))
                .is::<QualificationRefusal>()
        );
        assert!(
            !mapping_qualification_error(Rejection::Catalog(error), anyhow::anyhow!("fixture"))
                .is::<QualificationRefusal>()
        );
    }
    for error in [
        AdmissionError::CatalogChanged,
        AdmissionError::RestoreDetected,
    ] {
        assert!(
            catalog_qualification_error(error, anyhow::anyhow!("fixture"))
                .is::<QualificationRefusal>()
        );
        assert!(
            mapping_qualification_error(Rejection::Catalog(error), anyhow::anyhow!("fixture"))
                .is::<QualificationRefusal>()
        );
    }
    assert!(
        !mapping_qualification_error(Rejection::Invalid, anyhow::anyhow!("fixture"))
            .is::<QualificationRefusal>()
    );
    assert!(
        mapping_qualification_error(Rejection::MappingChanged, anyhow::anyhow!("fixture"))
            .is::<QualificationRefusal>()
    );
}

// Rejection-only fixture: an actual A-produced record is deliberately stale
// under independently admitted B. This is NOT a replacement or admission API.
async fn stale_input_fixture(malformed: bool) -> Fixture {
    let old = Fixture::new(true).await;
    let mut mapping = old.backend.source_mapping("physical").await.unwrap();
    let current = Fixture::new(false).await;
    assert_ne!(
        mapping.catalog.catalog_id,
        current.backend.identity().catalog_id
    );
    let db = rusqlite::Connection::open_with_flags(
        &current.native,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let bindings: i64 = db
        .query_row(
            "SELECT count(*) FROM bindings WHERE profile=? AND kind='source' AND owner=?",
            [&mapping.profile, &mapping.native_source],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        bindings, 0,
        "current owner must genuinely lack the historical source"
    );
    drop(db);
    if malformed {
        mapping.native_policy["include_regex"] = json!(["["]);
    }
    sqlx::query("INSERT INTO catabolic_source_mappings(library_id,profile,native_source,mapping_json,created_at) VALUES(?,?,?,?,0)")
        .bind(&mapping.library_id).bind(&mapping.profile).bind(&mapping.native_source)
        .bind(serde_json::to_string(&mapping).unwrap()).execute(&current.pool).await.unwrap();
    current
}

#[tokio::test]
async fn stale_catalog_without_historical_binding_is_nonqualification_not_authority() {
    let f = stale_input_fixture(false).await;
    let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        f.backend
            .qualified_source_mapping_in(&mut tx, "physical")
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    assert!(f.backend.audit_source_mapping("physical").await.is_err());
    f.capture().await.unwrap();
    assert_eq!(f.source_ids().await, json!([]));
    let raw: String = sqlx::query_scalar(
        "SELECT audit_json FROM catabolic_reference_audits WHERE import_id='audit'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap()["table_rows"]["libraries"],
        1
    );
}

#[tokio::test]
async fn malformed_policy_on_absent_historical_source_still_aborts_capture() {
    let f = stale_input_fixture(true).await;
    let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    assert!(
        f.backend
            .qualified_source_mapping_in(&mut tx, "physical")
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(f.capture().await.is_err());
    f.no_capture().await;
}
