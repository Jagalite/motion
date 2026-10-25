//! Schema-only evolution of a sealed compatibility store. No application upgrade
//! hook runs and the never-pooled connection can bypass only migration-ledger
//! guards, not catalog, source, authorization or application data guards.
use anyhow::{Context, ensure};
use rusqlite::ffi;
use sqlx::Connection;
use std::{ffi::CStr, path::Path};

unsafe extern "C" fn ledger(
    context: *mut ffi::sqlite3_context,
    argc: i32,
    argv: *mut *mut ffi::sqlite3_value,
) {
    let allowed = if argc == 1 && !argv.is_null() {
        let raw = unsafe { ffi::sqlite3_value_text(*argv) };
        !raw.is_null() && unsafe { CStr::from_ptr(raw.cast()) }.to_bytes() == b"_sqlx_migrations"
    } else {
        false
    };
    unsafe { ffi::sqlite3_result_int(context, i32::from(allowed)) };
}
/// Only exact compiled transitions 83→84→85→86→87→88→89→90→91→92→93 may evolve a sealed store.
/// A partial chain resumes at its durable migration prefix. Future migrations
/// need an explicit reviewed entry; the ledger capability is never generic.
pub async fn migrate(path: &Path, migrator: &sqlx::migrate::Migrator) -> anyhow::Result<()> {
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "migration requires regular existing Motion database"
    );
    let mut connection = sqlx::SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Full),
    )
    .await?;
    crate::projection::configure_connection(&mut connection).await?;
    let ledger_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(&mut connection)
    .await?;
    let canonical = "CREATE TABLE _sqlx_migrations ( version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL )";
    ensure!(
        ledger_schema
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            == canonical,
        "migration ledger schema changed"
    );
    let current: i64 =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&mut connection)
            .await?;
    ensure!(
        (83..=92).contains(&current),
        "unsupported sealed schema transition from {current}"
    );
    let pending: Vec<_> = migrator.iter().filter(|m| m.version > current).collect();
    ensure!(
        !pending.is_empty(),
        "sealed migration has no qualified transition"
    );
    for (offset, migration) in pending.iter().enumerate() {
        let version = current + offset as i64 + 1;
        let expected = match version {
            84 => include_str!("../../../migrations/0084_catabolic_retired_locators.sql"),
            85 => include_str!("../../../migrations/0085_catabolic_command_attempts.sql"),
            86 => {
                include_str!("../../../migrations/0086_catabolic_imported_source_descriptors.sql")
            }
            87 => include_str!("../../../migrations/0087_catabolic_generated_publications.sql"),
            88 => include_str!("../../../migrations/0088_catabolic_aggregate_issuance.sql"),
            89 => include_str!("../../../migrations/0089_catabolic_native_upgrade.sql"),
            90 => {
                include_str!("../../../migrations/0090_catabolic_physical_library_projection.sql")
            }
            91 => include_str!("../../../migrations/0091_catabolic_source_lifecycle.sql"),
            92 => include_str!("../../../migrations/0092_catabolic_scan_schedules.sql"),
            93 => include_str!("../../../migrations/0093_catabolic_source_watches.sql"),
            _ => anyhow::bail!("unqualified sealed schema migration {version}"),
        };
        ensure!(
            migration.version == version
                && !migration.migration_type.is_down_migration()
                && migration.sql.as_ref() == expected,
            "unqualified sealed schema migration"
        );
    }
    let target = pending.last().unwrap().version;
    let state = crate::source_freeze::read(&mut connection).await?;
    ensure!(
        !state.writes_allowed(),
        "sealed migration capability requires closed source admission"
    );
    crate::source_freeze::verify(&mut connection).await?;
    {
        let mut handle = connection.lock_handle().await?;
        let code = unsafe {
            ffi::sqlite3_create_function_v2(
                handle.as_raw_handle().as_ptr(),
                c"motion_native_projection".as_ptr(),
                1,
                ffi::SQLITE_UTF8 | ffi::SQLITE_INNOCUOUS,
                std::ptr::null_mut(),
                Some(ledger),
                None,
                None,
                None,
            )
        };
        ensure!(
            code == ffi::SQLITE_OK,
            "cannot install isolated migration ledger capability"
        );
    }
    let result = migrator
        .run(&mut connection)
        .await
        .context("qualified sealed cache migration failed");
    // Revoke before verification; close on every outcome. Cancellation drops the
    // owned connection rather than returning an armed connection to any pool.
    let revoke = crate::projection::configure_connection(&mut connection).await;
    let verified = if result.is_ok() && revoke.is_ok() {
        let applied: i64 =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
                .fetch_one(&mut connection)
                .await?;
        ensure!(
            applied == target,
            "qualified migration did not record expected schema"
        );
        crate::source_freeze::verify(&mut connection).await
    } else {
        Ok(())
    };
    let closed = connection.close().await;
    result?;
    revoke?;
    verified?;
    closed?;
    Ok(())
}

/// Verify retained sealed Motion evidence under the caller's stable transaction.
/// No native path is opened and no current native admission grant is returned.
pub async fn verify_sealed_compatibility_in(db: &mut sqlx::SqliteConnection) -> anyhow::Result<()> {
    ensure!(
        !crate::source_freeze::read(db).await?.writes_allowed(),
        "sealed compatibility requires closed source"
    );
    crate::source_freeze::verify(db).await?;
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id,identity_json FROM catabolic_binding LIMIT 2")
            .fetch_all(&mut *db)
            .await?;
    ensure!(
        rows.len() == 1 && rows[0].0 == 1 && rows[0].1.len() <= 65536,
        "sealed binding inventory invalid"
    );
    let frozen: crate::Identity = serde_json::from_str(&rows[0].1)?;
    let current = crate::native_upgrade::effective_identity_in(db, &frozen).await?;
    current
        .admit(&current)
        .map_err(|e| anyhow::anyhow!("sealed binding unsupported: {e:?}"))?;
    Ok(())
}
