use crate::{App, new_id, now};
use anyhow::Context;
use playscale_core::jobs::{Input, Job, Phase, transition};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, time::Duration};
use utoipa::ToSchema;

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct Library {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Track {
    pub index: i64,
    pub kind: String,
    pub codec: String,
    pub language: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub average_frame_rate: Option<crate::encoding::FrameRate>,
    #[serde(default)]
    pub bitrate: Option<u64>,
    #[serde(default)]
    pub pixel_format: Option<String>,
    #[serde(default)]
    pub color_transfer: Option<String>,
    #[serde(default)]
    pub start_time_seconds: Option<f64>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct Item {
    pub id: String,
    pub file_id: String,
    pub edition_id: String,
    pub edition_label: String,
    pub kind: String,
    pub library_id: String,
    pub title: String,
    pub revision: String,
    pub bytes: i64,
    pub duration_seconds: Option<f64>,
    pub tracks: Vec<Track>,
    pub available: bool,
    pub media_url: String,
}
#[derive(Debug, FromRow)]
pub struct ItemRow {
    pub id: String,
    pub item_id: String,
    pub edition_id: String,
    pub edition_label: String,
    pub kind: String,
    pub library_id: String,
    pub relative_path: String,
    pub title: String,
    pub revision: String,
    pub fingerprint: String,
    pub bytes: i64,
    pub duration_seconds: Option<f64>,
    pub tracks_json: String,
    pub available: bool,
}
impl ItemRow {
    pub fn public(self) -> Item {
        Item {
            media_url: format!("/media/{}?revision={}", self.id, self.revision),
            id: self.item_id,
            file_id: self.id,
            edition_id: self.edition_id,
            edition_label: self.edition_label,
            kind: self.kind,
            library_id: self.library_id,
            title: self.title,
            revision: self.revision,
            bytes: self.bytes,
            duration_seconds: self.duration_seconds,
            tracks: serde_json::from_str(&self.tracks_json).unwrap_or_default(),
            available: self.available,
        }
    }
}
#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct JobRow {
    pub id: String,
    pub library_id: String,
    pub phase: String,
    pub attempt: i64,
    pub error: Option<String>,
    pub created_at: i64,
    pub full_scan: bool,
    pub reused_files: i64,
    pub inspected_files: i64,
    /// `complete` or `partial` once published; absence was inferred only in
    /// the completely listed directories.
    pub outcome: Option<String>,
    pub complete_directories: i64,
    pub incomplete_directories: i64,
}
impl JobRow {
    pub fn state(&self) -> anyhow::Result<Job> {
        Ok(Job {
            phase: serde_json::from_value(serde_json::Value::String(self.phase.clone()))?,
            attempt: self.attempt.try_into()?,
        })
    }
}
#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub revision: i64,
}
#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct Progress {
    pub profile_id: String,
    pub item_id: String,
    pub position_seconds: f64,
    pub updated_at: i64,
}

pub async fn connect(path: &Path) -> anyhow::Result<SqlitePool> {
    connect_with(path, &sqlx::migrate!("./migrations")).await
}

/// A verified copy of the database taken before pending migrations were applied.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradeBackup {
    pub path: std::path::PathBuf,
    pub from_version: i64,
    pub to_version: i64,
}

/// Open, back up when an upgrade is pending, migrate, then run data upgrades.
/// Each migration runs in its own transaction; a failure leaves the schema at
/// the last complete version and the verified backup in place for restore.
pub async fn connect_with(
    path: &Path,
    migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<SqlitePool> {
    let db = open(path).await?;
    let backup = prepare_upgrade(&db, path, migrator).await?;
    if let Err(error) = migrator.run(&db).await {
        db.close().await;
        return Err(match &backup {
            Some(b) => anyhow::Error::new(error).context(format!(
                "schema migration failed; verified pre-upgrade backup at {}",
                b.path.display()
            )),
            None => anyhow::Error::new(error).context("schema migration failed"),
        });
    }
    crate::upgrade::run(&db, backup.as_ref()).await?;
    Ok(db)
}

async fn prepare_upgrade(
    db: &SqlitePool,
    path: &Path,
    migrator: &sqlx::migrate::Migrator,
) -> anyhow::Result<Option<UpgradeBackup>> {
    let tracked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(db)
    .await?;
    if tracked == 0 {
        return Ok(None);
    }
    let applied: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version")
            .fetch_all(db)
            .await?;
    let known: std::collections::BTreeSet<i64> = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .collect();
    if let Some(newer) = applied.iter().find(|v| !known.contains(v)) {
        anyhow::bail!(
            "database schema version {newer} is unknown to this server; restore a compatible build or a pre-upgrade backup"
        );
    }
    let (Some(&from_version), Some(&to_version)) = (applied.last(), known.last()) else {
        return Ok(None);
    };
    if applied.len() == known.len() {
        return Ok(None);
    }
    let dir = path
        .parent()
        .context("database path has no directory")?
        .join("upgrade-backups");
    tokio::fs::create_dir_all(&dir).await?;
    let size = tokio::fs::metadata(path).await?.len();
    crate::storage::require_space(&dir, size, 64 * 1024 * 1024)?;
    let target = dir.join(format!(
        "pre-upgrade-{from_version}-to-{to_version}-{}-{}.sqlite3",
        now(),
        new_id()
    ));
    let target_text = target.to_str().context("backup path must be UTF-8")?;
    sqlx::query("VACUUM INTO ?")
        .bind(target_text)
        .execute(db)
        .await
        .context("pre-upgrade backup failed; schema left unchanged")?;
    verify_backup(&target, applied.len())
        .await
        .context("pre-upgrade backup failed verification; schema left unchanged")?;
    let backup = UpgradeBackup {
        path: target,
        from_version,
        to_version,
    };
    tracing::info!(path=%backup.path.display(), from_version, to_version, "verified pre-upgrade backup");
    Ok(Some(backup))
}

/// A backup is usable only if SQLite can read it completely and it carries the
/// exact migration history it was taken at.
pub async fn verify_backup(path: &Path, applied_migrations: usize) -> anyhow::Result<()> {
    use sqlx::Connection;
    let mut conn = sqlx::SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(path).read_only(true),
    )
    .await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut conn)
        .await?;
    anyhow::ensure!(integrity == "ok", "integrity check failed: {integrity}");
    let dangling = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut conn)
        .await?;
    anyhow::ensure!(dangling.is_empty(), "backup has dangling references");
    let migrations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&mut conn)
            .await?;
    anyhow::ensure!(
        usize::try_from(migrations)? == applied_migrations,
        "backup migration history differs"
    );
    conn.close().await?;
    Ok(())
}

async fn open(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(5));
    let db = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;
    Ok(db)
}

pub async fn add_library(db: &SqlitePool, name: &str, root: &Path) -> anyhow::Result<Library> {
    anyhow::ensure!(
        !name.trim().is_empty() && name.len() <= 200,
        "name must contain 1–200 bytes"
    );
    let root = tokio::fs::canonicalize(root)
        .await
        .context("library root does not exist")?;
    anyhow::ensure!(
        tokio::fs::metadata(&root).await?.is_dir(),
        "library root must be a directory"
    );
    let root = root.to_str().context("library root must be UTF-8")?;
    let identity = root_identity(&tokio::fs::metadata(root).await?);
    add_verified_library(db, name, root, &identity).await
}

/// Register the result of an admitted filesystem inspection. Runtime callers
/// serialize this operation with relocation using App.jobs.
pub(crate) async fn add_verified_library(
    db: &SqlitePool,
    name: &str,
    root: &str,
    identity: &str,
) -> anyhow::Result<Library> {
    anyhow::ensure!(
        !name.trim().is_empty() && name.len() <= 200,
        "invalid library name"
    );
    let existing: Option<String> =
        sqlx::query_scalar("SELECT root_identity FROM libraries WHERE root=?")
            .bind(root)
            .fetch_optional(db)
            .await?;
    anyhow::ensure!(
        existing.as_ref().is_none_or(|v| v == identity),
        "library root identity changed"
    );
    sqlx::query("INSERT INTO libraries (id,name,root,root_identity) VALUES (?,?,?,?) ON CONFLICT(root) DO NOTHING")
        .bind(new_id()).bind(name.trim()).bind(root).bind(identity).execute(db).await?;
    Ok(sqlx::query_as("SELECT id,name FROM libraries WHERE root=?")
        .bind(root)
        .fetch_one(db)
        .await?)
}

pub fn root_identity(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        format!("{:?}", meta.created().ok())
    }
}

/// Reserve SQLite's writer before observing state that this transaction changes.
/// App.jobs serializes domain writers; background diagnostic writes can otherwise
/// invalidate a deferred read snapshot before its first write (SQLITE_BUSY_SNAPSHOT).
pub(crate) async fn begin_write(
    db: &SqlitePool,
) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, sqlx::Error> {
    db.begin_with("BEGIN IMMEDIATE").await
}

pub async fn enqueue(app: &App, library: &str) -> anyhow::Result<JobRow> {
    enqueue_mode(app, library, false).await
}
pub async fn enqueue_mode(app: &App, library: &str, full: bool) -> anyhow::Result<JobRow> {
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let id = enqueue_transaction(&mut tx, library, full).await?;
    tx.commit().await?;
    get_job(&app.db, &id).await
}
/// Caller holds App.jobs; scheduled admission and advancement share this transaction.
pub(crate) async fn enqueue_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    library: &str,
    full: bool,
) -> anyhow::Result<String> {
    let exists: i64 =
        sqlx::query_scalar("SELECT count(*) FROM libraries WHERE id=? AND managed=0 AND enabled=1")
            .bind(library)
            .fetch_one(&mut **tx)
            .await?;
    anyhow::ensure!(exists == 1, "unknown library");
    let existing = sqlx::query_as::<_, JobRow>(
        "SELECT * FROM jobs WHERE library_id=? AND phase IN ('queued','running','cancelling')",
    )
    .bind(library)
    .fetch_optional(&mut **tx)
    .await?;
    playscale_core::scan::admit(existing.as_ref().map(|j| j.full_scan), full)
        .map_err(anyhow::Error::msg)?;
    if let Some(row) = existing {
        return Ok(row.id);
    }
    let id = new_id();
    sqlx::query(
        "INSERT INTO jobs (id,library_id,phase,created_at,full_scan) VALUES (?,?,'queued',?,?)",
    )
    .bind(&id)
    .bind(library)
    .bind(now())
    .bind(full)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

pub async fn get_job(db: &SqlitePool, id: &str) -> anyhow::Result<JobRow> {
    Ok(sqlx::query_as("SELECT * FROM jobs WHERE id=?")
        .bind(id)
        .fetch_one(db)
        .await?)
}
pub fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Queued => "queued",
        Phase::Running => "running",
        Phase::Cancelling => "cancelling",
        Phase::Cancelled => "cancelled",
        Phase::Completed => "completed",
        Phase::Failed => "failed",
    }
}
pub async fn cancel(app: &App, id: &str) -> anyhow::Result<JobRow> {
    let _guard = app.jobs.lock().await;
    let row = get_job(&app.db, id).await?;
    let (next, _) = transition(&row.state()?, Input::Cancel);
    sqlx::query("UPDATE jobs SET phase=? WHERE id=?")
        .bind(phase_name(next.phase))
        .bind(id)
        .execute(&app.db)
        .await?;
    get_job(&app.db, id).await
}
pub async fn recover(db: &SqlitePool) -> anyhow::Result<()> {
    let rows: Vec<JobRow> =
        sqlx::query_as("SELECT * FROM jobs WHERE phase IN ('running','cancelling')")
            .fetch_all(db)
            .await?;
    let mut tx = db.begin().await?;
    for row in rows {
        let (next, _) = transition(&row.state()?, Input::Recover);
        sqlx::query("UPDATE jobs SET phase=? WHERE id=?")
            .bind(phase_name(next.phase))
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod transaction_tests {
    use super::*;
    #[tokio::test]
    async fn state_transaction_reserves_writer_before_observation() {
        use sqlx::Connection;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let pool = connect(&path).await.unwrap();
        let mut observer = sqlx::SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(&path)
                .busy_timeout(Duration::ZERO),
        )
        .await
        .unwrap();
        let mut tx = begin_write(&pool).await.unwrap();
        let _: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        // A background status writer cannot commit between observation and mutation.
        let error = sqlx::query("INSERT INTO maintenance_state VALUES ('test',NULL,1,NULL)")
            .execute(&mut observer)
            .await
            .unwrap_err();
        assert!(matches!(error, sqlx::Error::Database(ref e) if e.code().as_deref()==Some("5")));
        sqlx::query("UPDATE profiles SET name='Reserved writer' WHERE id='default'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("INSERT INTO maintenance_state VALUES ('test',NULL,1,NULL)")
            .execute(&mut observer)
            .await
            .unwrap();
    }
}
