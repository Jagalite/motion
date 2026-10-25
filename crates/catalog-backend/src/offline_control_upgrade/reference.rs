//! Independently rebuilt closed control schemas. This is descriptive validation,
//! not source-freeze authority, native admission, or approval to run migrations.
use anyhow::{Context, ensure};
use rusqlite::{Connection, types::ValueRef};
use sqlx::{
    migrate::{Migration, MigrationType, Migrator},
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::borrow::Cow;

const MAX_OBJECTS: i64 = 10_000;
const MAX_BYTES: i64 = 16 * 1024 * 1024;
// Same finite logical byte ceiling as the complete row-preservation inventory.
const MAX_LEDGER_BYTES: i64 = 8 * 1024 * 1024;
const SHAPE: &str = "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE NOT (type='trigger' AND name GLOB 'motion_source_freeze_*') ORDER BY type,name";
const SIZE: &str = "SELECT count(*),coalesce(sum(length(CAST(type AS BLOB))+length(CAST(name AS BLOB))+length(CAST(tbl_name AS BLOB))+coalesce(length(CAST(sql AS BLOB)),0)),0) FROM sqlite_schema";
type Shape = (String, String, String, Option<String>);

// Same Text/UTF-8 and NULL semantics as Row::get::<_, String/Option<String>>,
// without cloning up to16MiB of schema text during each exact comparison.
fn schema_text<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> rusqlite::Result<&'a str> {
    let value = row.get_ref(index)?;
    match value {
        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map_err(|error| rusqlite::Error::Utf8Error(index, error)),
        _ => Err(rusqlite::Error::InvalidColumnType(index,
            row.as_ref().column_name(index)?.to_owned(), value.data_type())),
    }
}
fn borrowed_shape<'a>(row: &'a rusqlite::Row<'_>) -> rusqlite::Result<(&'a str, &'a str, &'a str, Option<&'a str>)> {
    // Keep original left-to-right conversion/error order.
    let kind = schema_text(row, 0)?;
    let name = schema_text(row, 1)?;
    let table = schema_text(row, 2)?;
    let sql = match row.get_ref(3)? {
        ValueRef::Null => None,
        _ => Some(schema_text(row, 3)?),
    };
    Ok((kind, name, table, sql))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlVersion {
    Retained93,
    Complete97,
}

pub(super) struct ClosedReference {
    migrations: Vec<Migration>,
    prefix_len: usize,
    schema93: Vec<Shape>,
    schema97: Vec<Shape>,
}

/// Fixed resources only. SQLx computes each resource's SHA384 checksum using
/// the same constructor as its normal migration loader.
fn migrations() -> anyhow::Result<(Vec<Migration>, usize)> {
    let compiled = sqlx::migrate!("../../migrations");
    let mut all: Vec<_> = compiled
        .iter()
        .filter(|m| m.version <= 93)
        .cloned()
        .collect();
    ensure!(
        all.last().map(|m| m.version) == Some(93),
        "closed control prefix93 absent"
    );
    ensure!(
        all.iter()
            .all(|m| !m.migration_type.is_down_migration() && m.checksum.len() == 48),
        "invalid compiled control prefix"
    );
    let prefix_len = all.len();
    for (version, description, sql) in [
        (
            94,
            "catabolic source relocations",
            include_str!("../source_relocations/schema94.sql"),
        ),
        (
            95,
            "catabolic paired restore",
            include_str!("../paired_restore/schema95.sql"),
        ),
        (
            96,
            "catabolic native bootstrap",
            include_str!("../native_bootstrap/schema96.sql"),
        ),
        (
            97,
            "preview sheet artifacts",
            include_str!("../../../../migrations/0097_preview_sheet_artifacts.sql"),
        ),
    ] {
        all.push(Migration::new(
            version,
            Cow::Borrowed(description),
            MigrationType::Simple,
            Cow::Borrowed(sql),
            false,
        ));
    }
    Ok((all, prefix_len))
}

fn runner(migrations: &[Migration]) -> Migrator {
    let mut result = sqlx::migrate!("../../migrations");
    result.migrations = Cow::Owned(migrations.to_vec());
    result
}

async fn schema(pool: &sqlx::SqlitePool) -> anyhow::Result<Vec<Shape>> {
    let (count, bytes): (i64, i64) = sqlx::query_as(SIZE).fetch_one(pool).await?;
    ensure!(
        (0..=MAX_OBJECTS).contains(&count) && (0..=MAX_BYTES).contains(&bytes),
        "closed reference schema exceeds bound"
    );
    Ok(sqlx::query_as(SHAPE).fetch_all(pool).await?)
}

impl ClosedReference {
    /// Actual owned capacities only; compiled borrowed resources allocate no
    /// additional buffer. This describes retention, never schema authority.
    pub(super) fn retained_allocations(&self) -> anyhow::Result<u64> {
        let mut total = self
            .migrations
            .capacity()
            .checked_mul(std::mem::size_of::<Migration>())
            .context("control reference allocation overflow")? as u64;
        let mut add = |bytes: usize| -> anyhow::Result<()> {
            total = total
                .checked_add(u64::try_from(bytes)?)
                .context("control reference allocation overflow")?;
            ensure!(
                total <= 256 * 1024 * 1024,
                "control reference allocation bound exceeded"
            );
            Ok(())
        };
        for migration in &self.migrations {
            if let Cow::Owned(value) = &migration.description {
                add(value.capacity())?;
            }
            if let Cow::Owned(value) = &migration.sql {
                add(value.capacity())?;
            }
            if let Cow::Owned(value) = &migration.checksum {
                add(value.capacity())?;
            }
        }
        for schema in [&self.schema93, &self.schema97] {
            add(schema
                .capacity()
                .checked_mul(std::mem::size_of::<Shape>())
                .context("control reference allocation overflow")?)?;
            for (kind, name, table, sql) in schema {
                add(kind.capacity())?;
                add(name.capacity())?;
                add(table.capacity())?;
                if let Some(sql) = sql {
                    add(sql.capacity())?;
                }
            }
        }
        Ok(total)
    }

    pub(super) async fn build() -> anyhow::Result<Self> {
        let (migrations, prefix_len) = migrations()?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .foreign_keys(true),
            )
            .await?;
        let result = async {
            runner(&migrations[..prefix_len]).run(&pool).await?;
            let schema93 = schema(&pool).await?;
            // Run the full fixed chain through SQLx, preserving each migration
            // transaction and FK behavior, not concatenated execute_batch SQL.
            runner(&migrations).run(&pool).await?;
            let schema97 = schema(&pool).await?;
            Ok(Self {
                migrations,
                prefix_len,
                schema93,
                schema97,
            })
        }
        .await;
        pool.close().await;
        result
    }

    /// Exact compiled suffix for the enclosing fixed atomic upgrade owner.
    /// A borrowed description is not an execution capability.
    pub(super) fn suffix(&self) -> &[Migration] {
        &self.migrations[self.prefix_len..]
    }

    pub(super) fn verify_in(&self, db: &Connection) -> anyhow::Result<ControlVersion> {
        ensure!(
            !db.is_autocommit(),
            "closed control verification requires retained transaction"
        );
        let count: i64 = db.query_row("SELECT count(*) FROM _sqlx_migrations", [], |r| r.get(0))?;
        ensure!(
            count == self.prefix_len as i64 || count == self.migrations.len() as i64,
            "partial or extra control migration ledger"
        );
        let ledger_bytes: i64 = db.query_row(
            "SELECT coalesce(sum(length(CAST(description AS BLOB))+length(CAST(installed_on AS BLOB))+length(CAST(checksum AS BLOB))+length(CAST(version AS BLOB))+length(CAST(success AS BLOB))+length(CAST(execution_time AS BLOB))),0) FROM _sqlx_migrations",
            [], |r| r.get(0),
        )?;
        ensure!(
            (0..=MAX_LEDGER_BYTES).contains(&ledger_bytes),
            "control ledger metadata exceeds bound"
        );
        let version = if count == self.prefix_len as i64 {
            ControlVersion::Retained93
        } else {
            ControlVersion::Complete97
        };
        let expected = &self.migrations[..count as usize];
        let mut statement =
            db.prepare("SELECT version,success,checksum,description,installed_on,execution_time FROM _sqlx_migrations ORDER BY version")?;
        let mut rows = statement.query([])?;
        for migration in expected {
            let row = rows.next()?.context("control migration ledger gap")?;
            ensure!(
                matches!(row.get_ref(0)?, ValueRef::Integer(v) if v == migration.version)
                    && matches!(row.get_ref(1)?, ValueRef::Integer(1)),
                "control migration ledger version/success type or value differs"
            );
            ensure!(
                matches!(row.get_ref(2)?, ValueRef::Blob(v) if v.len() == 48 && v == migration.checksum.as_ref()),
                "control migration checksum storage or value differs"
            );
            ensure!(
                matches!(row.get_ref(3)?, ValueRef::Text(v) if v == migration.description.as_bytes()),
                "control migration description differs from compiled resource"
            );
            // Historical timestamp bytes are retained verbatim, not parsed or
            // normalized. The complete aggregate was bounded before this read.
            ensure!(
                matches!(row.get_ref(4)?, ValueRef::Text(v) if v.len() <= MAX_LEDGER_BYTES as usize),
                "control migration installed_on storage differs"
            );
            ensure!(
                matches!(row.get_ref(5)?, ValueRef::Integer(v) if v >= 0),
                "control migration execution_time storage or value differs"
            );
        }
        ensure!(rows.next()?.is_none(), "extra control migration ledger row");
        let (count, bytes): (i64, i64) = db.query_row(SIZE, [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        ensure!(
            (0..=MAX_OBJECTS).contains(&count) && (0..=MAX_BYTES).contains(&bytes),
            "control schema exceeds bound"
        );
        let statistics: i64 = db.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('sqlite_stat1','sqlite_stat2','sqlite_stat3','sqlite_stat4')", [], |r| r.get(0))?;
        ensure!(
            statistics == 0,
            "control statistics schema requires explicit review"
        );
        let expected = match version {
            ControlVersion::Retained93 => &self.schema93,
            ControlVersion::Complete97 => &self.schema97,
        };
        let mut statement = db.prepare(SHAPE)?;
        let mut rows = statement.query([])?;
        for object in expected {
            let row = rows.next()?.context("control schema object missing")?;
            let actual = borrowed_shape(row)?;
            ensure!(
                actual == (object.0.as_str(), object.1.as_str(), object.2.as_str(), object.3.as_deref()),
                "control schema differs from closed resource reference"
            );
        }
        ensure!(rows.next()?.is_none(), "unrecognized control schema object");
        Ok(version)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn borrowed_shape_preserves_string_and_optional_string_decoding() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for query in [
            "SELECT 'table','café','name',NULL",
            "SELECT 'table','name','name','CREATE TABLE name(x)'",
            "SELECT 12,'name','name',NULL",
            "SELECT X'61','name','name',NULL",
            "SELECT NULL,'name','name',NULL",
            "SELECT CAST(X'FF' AS TEXT),'name','name',NULL",
            "SELECT 'table','name','name',12",
            "SELECT 'table','name','name',CAST(X'FF' AS TEXT)",
        ] {
            let mut statement = db.prepare(query).unwrap();
            let mut rows = statement.query([]).unwrap();
            let row = rows.next().unwrap().unwrap();
            let old: rusqlite::Result<super::Shape> = (|| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))();
            let borrowed = super::borrowed_shape(row);
            match (old, borrowed) {
                (Ok(old), Ok(new)) => assert_eq!(new, (old.0.as_str(), old.1.as_str(), old.2.as_str(), old.3.as_deref())),
                (Err(old), Err(new)) => assert_eq!(format!("{old:?}"), format!("{new:?}")),
                _ => panic!("borrowed schema decoder changed acceptance"),
            }
        }
    }

    #[tokio::test]
    async fn retained_reference_accounts_actual_extra_string_capacity() {
        let mut reference = super::ClosedReference::build().await.unwrap();
        let before = reference.retained_allocations().unwrap();
        let value = &mut reference.schema93[0].1;
        let old_capacity = value.capacity();
        value.reserve(4096);
        let growth = value.capacity() - old_capacity;
        assert!(growth > 0);
        assert_eq!(
            reference.retained_allocations().unwrap(),
            before + growth as u64
        );
    }

    use super::*;
    async fn fixture(complete: bool) -> (tempfile::TempDir, Connection) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control.sqlite3");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        let (all, prefix) = migrations().unwrap();
        runner(if complete { &all } else { &all[..prefix] })
            .run(&pool)
            .await
            .unwrap();
        pool.close().await;
        let db = Connection::open(path).unwrap();
        db.pragma_update(None, "foreign_keys", true).unwrap();
        super::super::set_ledger(&db, false).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        (root, db)
    }
    // Intentional corrupt-state fixtures use only the existing fixed ledger
    // capability. The schema guards stay installed; denial is restored before
    // every verification, including when the attempted mutation fails.
    fn corrupt_ledger(db: &Connection, sql: &str) {
        super::super::set_ledger(db, true).unwrap();
        let changed = db.execute_batch(sql);
        super::super::set_ledger(db, false).unwrap();
        changed.unwrap();
        let denied: i64 = db
            .query_row(
                "SELECT motion_native_projection('_sqlx_migrations')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(denied, 0);
    }
    #[tokio::test]
    async fn independently_built_exact93_and97_match_closed_reference() {
        let reference = ClosedReference::build().await.unwrap();
        for (complete, expected) in [
            (false, ControlVersion::Retained93),
            (true, ControlVersion::Complete97),
        ] {
            let (_root, db) = fixture(complete).await;
            assert_eq!(reference.verify_in(&db).unwrap(), expected);
            db.execute_batch("ROLLBACK").unwrap();
            assert!(reference.verify_in(&db).is_err());
        }
        assert_eq!(
            reference
                .suffix()
                .iter()
                .map(|m| m.version)
                .collect::<Vec<_>>(),
            vec![94, 95, 96, 97]
        );
    }
    #[tokio::test]
    async fn changed_ledger_types_checksums_gaps_and_partial_suffix_refuse() {
        let reference = ClosedReference::build().await.unwrap();
        let (_root, db) = fixture(true).await;
        for change in [
            "DELETE FROM _sqlx_migrations WHERE version=94",
            "DELETE FROM _sqlx_migrations WHERE version>=95",
            "UPDATE _sqlx_migrations SET success=0 WHERE version=93",
            "UPDATE _sqlx_migrations SET success='invalid' WHERE version=93",
            "UPDATE _sqlx_migrations SET version='invalid' WHERE version=97",
            "UPDATE _sqlx_migrations SET checksum=zeroblob(48) WHERE version=93",
            "UPDATE _sqlx_migrations SET checksum=CAST(checksum AS TEXT) WHERE version=93",
            "UPDATE _sqlx_migrations SET checksum=zeroblob(47) WHERE version=93",
            "UPDATE _sqlx_migrations SET version=100 WHERE version=97",
        ] {
            db.execute_batch("SAVEPOINT mutation").unwrap();
            corrupt_ledger(&db, change);
            assert!(reference.verify_in(&db).is_err(), "{change}");
            db.execute_batch("ROLLBACK TO mutation; RELEASE mutation")
                .unwrap();
        }
        assert_eq!(
            reference.verify_in(&db).unwrap(),
            ControlVersion::Complete97
        );
    }
    #[tokio::test]
    async fn malformed_or_oversized_ledger_metadata_refuses_before_row_capture() {
        let reference = ClosedReference::build().await.unwrap();
        let (_root, db) = fixture(true).await;
        for change in [
            "UPDATE _sqlx_migrations SET description='different' WHERE version=93",
            "UPDATE _sqlx_migrations SET description=CAST(description AS BLOB) WHERE version=93",
            "UPDATE _sqlx_migrations SET installed_on=3 WHERE version=93",
            "UPDATE _sqlx_migrations SET installed_on=zeroblob(10) WHERE version=93",
            "UPDATE _sqlx_migrations SET installed_on=printf('%.*c',8388609,'x') WHERE version=93",
            "UPDATE _sqlx_migrations SET execution_time=-1 WHERE version=93",
            "UPDATE _sqlx_migrations SET execution_time='invalid' WHERE version=93",
            "UPDATE _sqlx_migrations SET execution_time=0.5 WHERE version=93",
        ] {
            db.execute_batch("SAVEPOINT mutation").unwrap();
            corrupt_ledger(&db, change);
            assert!(reference.verify_in(&db).is_err(), "{change}");
            db.execute_batch("ROLLBACK TO mutation; RELEASE mutation")
                .unwrap();
        }
        // Arbitrary bounded historical TEXT is not reinterpreted as a clock.
        corrupt_ledger(
            &db,
            "UPDATE _sqlx_migrations SET installed_on='historically retained bytes' WHERE version=93",
        );
        assert_eq!(
            reference.verify_in(&db).unwrap(),
            ControlVersion::Complete97
        );
    }
    #[tokio::test]
    async fn changed_missing_extra_or_statistics_schema_refuses() {
        let reference = ClosedReference::build().await.unwrap();
        let (_root, db) = fixture(false).await;
        for change in [
            "CREATE TABLE unexpected(value)",
            "CREATE INDEX unexpected_index ON items(id)",
            "ANALYZE",
        ] {
            db.execute_batch("SAVEPOINT mutation").unwrap();
            db.execute_batch(change).unwrap();
            assert!(reference.verify_in(&db).is_err(), "{change}");
            db.execute_batch("ROLLBACK TO mutation; RELEASE mutation")
                .unwrap();
        }
        let (_root97, db97) = fixture(true).await;
        db97.execute_batch("DROP TABLE preview_sheet_artifacts")
            .unwrap();
        assert!(reference.verify_in(&db97).is_err());
    }
}
