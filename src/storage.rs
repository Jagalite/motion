//! Local operational storage: bounded logs, verifiable snapshots, and retention.
use crate::{
    App,
    api::{ApiError, admin},
    new_id, now,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use sqlx::Connection;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub backup_interval_seconds: u64,
    pub backups_keep: usize,
    pub history_retention_seconds: u64,
    pub min_free_bytes: u64,
    pub log_bytes: u64,
    pub log_files: usize,
    pub incremental_scans: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            backup_interval_seconds: 86400,
            backups_keep: 7,
            history_retention_seconds: 90 * 86400,
            min_free_bytes: 1024 * 1024 * 1024,
            log_bytes: 8 * 1024 * 1024,
            log_files: 5,
            incremental_scans: cfg!(unix),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.backup_interval_seconds == 0
                || (60..=31_536_000).contains(&self.backup_interval_seconds),
            "invalid backup interval"
        );
        anyhow::ensure!(
            (1..=100).contains(&self.backups_keep)
                && (86400..=31_536_000).contains(&self.history_retention_seconds),
            "invalid retention"
        );
        anyhow::ensure!(
            (65536..=1024 * 1024 * 1024).contains(&self.log_bytes)
                && (1..=20).contains(&self.log_files)
                && self.min_free_bytes <= i64::MAX as u64,
            "invalid storage limits"
        );
        Ok(())
    }
}
pub struct Runtime {
    pub root: PathBuf,
    pub settings: Settings,
    pub backup_io: Arc<Semaphore>,
    pub scan_io: Arc<Semaphore>,
    pub library_io: Arc<Semaphore>,
}
impl Runtime {
    pub fn new(root: PathBuf, settings: Settings) -> Self {
        Self {
            root,
            settings,
            backup_io: Arc::new(Semaphore::new(1)),
            scan_io: Arc::new(Semaphore::new(1)),
            library_io: Arc::new(Semaphore::new(1)),
        }
    }
}
pub fn require_space(path: &Path, required: u64, reserve: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        playscale_core::maintenance::enough_space(fs2::available_space(path)?, required, reserve),
        "insufficient free space for operation and reserve"
    );
    Ok(())
}
async fn blocking<T: Send + 'static>(
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(move || {
        let _hold = permit;
        f()
    })
    .await?
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct MaintenanceState {
    pub name: String,
    pub last_success: Option<i64>,
    pub last_attempt: Option<i64>,
    pub error: Option<String>,
}
#[derive(Serialize, ToSchema)]
pub struct StorageReport {
    pub free_bytes: u64,
    pub reserve_bytes: u64,
    pub backup_interval_seconds: u64,
    pub backups_keep: usize,
    pub history_retention_seconds: u64,
    pub state: Vec<MaintenanceState>,
}
#[utoipa::path(operation_id="storage_report",get,path="/api/v1/admin/storage",security(("admin_token"=[])),responses((status=200,description="Disk reserve, retention, and maintenance health",body=StorageReport),(status=503,description="Inspection busy",body=crate::api::ErrorBody)))]
pub async fn report(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<StorageReport>, ApiError> {
    admin(&app, &headers)?;
    let hold = app
        .storage
        .library_io
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_busy",
                "Storage inspection is busy",
            )
        })?;
    let root = app.storage.root.clone();
    let free_bytes = blocking(Arc::new(hold), move || Ok(fs2::available_space(root)?))
        .await
        .map_err(ApiError::internal)?;
    let s = &app.storage.settings;
    Ok(Json(StorageReport {
        free_bytes,
        reserve_bytes: s.min_free_bytes,
        backup_interval_seconds: s.backup_interval_seconds,
        backups_keep: s.backups_keep,
        history_retention_seconds: s.history_retention_seconds,
        state: sqlx::query_as("SELECT * FROM maintenance_state ORDER BY name")
            .fetch_all(&app.db)
            .await?,
    }))
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct Snapshot {
    pub format: u8,
    pub created_at: i64,
    pub sha256: String,
    pub bytes: u64,
    pub schema_versions: Vec<i64>,
}
#[utoipa::path(operation_id="create_backup",post,path="/api/v1/admin/storage",security(("admin_token"=[])),responses((status=201,description="Consistent validated local snapshot",body=Snapshot),(status=503,description="Backup busy or storage failure",body=crate::api::ErrorBody)))]
pub async fn backup_now(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Snapshot>), ApiError> {
    admin(&app, &headers)?;
    // The task owns admission through completion even if its HTTP caller disconnects.
    let task = tokio::spawn(async move { backup(&app).await });
    let value = task.await.map_err(ApiError::internal)?.map_err(|e| {
        tracing::warn!(%e,"snapshot failed");
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "backup_unavailable",
            "Backup failed or is busy; inspect storage status and logs",
        )
    })?;
    Ok((StatusCode::CREATED, Json(value)))
}
pub async fn record(app: &App, name: &str, error: Option<&str>) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO maintenance_state VALUES (?,?,?,?) ON CONFLICT(name) DO UPDATE SET last_success=CASE WHEN excluded.error IS NULL THEN excluded.last_success ELSE maintenance_state.last_success END,last_attempt=excluded.last_attempt,error=excluded.error").bind(name).bind(if error.is_none(){Some(now())}else{None}).bind(now()).bind(error).execute(&app.db).await?;
    Ok(())
}
pub async fn backup(app: &App) -> anyhow::Result<Snapshot> {
    let permit = Arc::new(app.storage.backup_io.clone().try_acquire_owned()?);
    let result = make_snapshot(app, permit).await;
    record(
        app,
        "backup",
        result
            .as_ref()
            .err()
            .map(|_| "backup_failed: inspect disk space and server logs"),
    )
    .await?;
    result
}
async fn make_snapshot(
    app: &App,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<Snapshot> {
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&app.db)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&app.db)
        .await?;
    let root = app.storage.root.join("backups");
    let destination = root.join(format!(
        "auto-{:020}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        new_id()
    ));
    let stage = destination.with_extension("partial");
    let prepare = stage.clone();
    let settings = app.storage.settings.clone();
    blocking(permit.clone(), move || {
        std::fs::create_dir_all(prepare.parent().unwrap())?;
        // Interrupted snapshots must be reclaimable even when disk pressure
        // prevents another snapshot from completing. Keep complete backups here.
        maintain_backups(prepare.parent().unwrap(), None)?;
        require_space(
            prepare.parent().unwrap(),
            (pages as u64)
                .saturating_mul(page_size as u64)
                .saturating_mul(2),
            settings.min_free_bytes,
        )?;
        std::fs::create_dir(&prepare)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&prepare, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::write(prepare.join(".playscale-backup"), b"playscale-scheduled-v1")?;
        Ok(())
    })
    .await?;
    let database = stage.join("playscale.sqlite3");
    sqlx::query("VACUUM INTO ?")
        .bind(database.to_string_lossy().as_ref())
        .execute(&app.db)
        .await?;
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&database)
        .read_only(true);
    let mut check = sqlx::SqliteConnection::connect_with(&options).await?;
    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut check)
        .await?;
    anyhow::ensure!(integrity == ["ok"], "snapshot integrity failed");
    anyhow::ensure!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut check)
            .await?
            .is_empty(),
        "snapshot foreign key check failed"
    );
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success=1 ORDER BY version")
            .fetch_all(&mut check)
            .await?;
    check.close().await?;
    let keep = app.storage.settings.backups_keep;
    blocking(permit, move || {
        use sha2::{Digest, Sha256};
        let mut file = std::fs::File::open(&database)?;
        let mut hash = Sha256::new();
        let mut buf = [0u8; 128 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        file.sync_all()?;
        let snapshot = Snapshot {
            format: 1,
            created_at: now(),
            sha256: format!("{:x}", hash.finalize()),
            bytes: file.metadata()?.len(),
            schema_versions: versions,
        };
        let mut manifest = std::fs::File::create(stage.join("manifest.json"))?;
        manifest.write_all(&serde_json::to_vec_pretty(&snapshot)?)?;
        manifest.sync_all()?;
        #[cfg(unix)]
        std::fs::File::open(&stage)?.sync_all()?;
        std::fs::rename(&stage, &destination)?;
        #[cfg(unix)]
        std::fs::File::open(&root)?.sync_all()?;
        retain_snapshots(&root, keep)?;
        Ok(snapshot)
    })
    .await
}
pub fn retain_snapshots(root: &Path, keep: usize) -> anyhow::Result<()> {
    maintain_backups(root, Some(keep))
}
fn maintain_backups(root: &Path, keep: Option<usize>) -> anyhow::Result<()> {
    let mut complete = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir()
            || !name.starts_with("auto-")
            || std::fs::read(path.join(".playscale-backup"))
                .ok()
                .as_deref()
                != Some(b"playscale-scheduled-v1")
        {
            continue;
        }
        if name.ends_with(".partial") {
            if entry
                .metadata()?
                .modified()?
                .elapsed()
                .unwrap_or_default()
                .as_secs()
                > 86400
            {
                std::fs::remove_dir_all(path)?;
            }
            continue;
        }
        if keep.is_some()
            && let Ok(bytes) = std::fs::read(path.join("manifest.json"))
            && bytes.len() < 65536
            && serde_json::from_slice::<Snapshot>(&bytes).is_ok()
        {
            complete.push(path);
        }
    }
    complete.sort();
    complete.reverse();
    for path in complete.into_iter().skip(keep.unwrap_or(usize::MAX)) {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}
pub async fn prune_history(app: &App) -> anyhow::Result<()> {
    let cutoff = now() - app.storage.settings.history_retention_seconds as i64;
    let _lock = app.jobs.lock().await;
    let mut tx = app.db.begin().await?;
    sqlx::query("DELETE FROM jobs WHERE id IN (SELECT id FROM jobs WHERE phase IN ('completed','failed','cancelled') AND created_at<? ORDER BY created_at LIMIT 500)").bind(cutoff).execute(&mut *tx).await?;
    // Keep the most recent authoritative session to preserve the legacy-write barrier.
    sqlx::query("DELETE FROM playback_sessions WHERE id IN (SELECT id FROM playback_sessions WHERE updated_at<? AND status IN ('ended','stopped','superseded','invalidated') AND id NOT IN (SELECT session_id FROM viewing_state WHERE session_id IS NOT NULL) ORDER BY updated_at LIMIT 500)").bind(cutoff).execute(&mut *tx).await?;
    // Published rendition provenance stays in the catalog. Idempotency keys have this documented retention horizon.
    sqlx::query("DELETE FROM processing_jobs WHERE id IN (SELECT id FROM processing_jobs WHERE updated_at<? AND cache_cleaned=1 AND phase IN ('completed','failed','cancelled') ORDER BY updated_at LIMIT 500)").bind(cutoff).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    loop {
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=tokio::time::sleep(Duration::from_secs(30))=>{}}
        if let Err(e) = prune_history(&app).await {
            tracing::warn!(%e,"history retention failed");
            let _ = record(&app, "history", Some("history_prune_failed")).await;
        } else {
            let _ = record(&app, "history", None).await;
        }
        let interval = app.storage.settings.backup_interval_seconds;
        if interval == 0 {
            continue;
        }
        let last: Result<Option<Option<i64>>, sqlx::Error> =
            sqlx::query_scalar("SELECT last_attempt FROM maintenance_state WHERE name='backup'")
                .fetch_optional(&app.db)
                .await;
        let last = match last {
            Ok(last) => last.flatten(),
            Err(e) => {
                tracing::warn!(%e,"backup schedule unavailable");
                continue;
            }
        };
        if playscale_core::maintenance::due(last, now(), interval)
            && let Err(e) = backup(&app).await
        {
            tracing::warn!(%e,"scheduled backup failed");
        }
    }
}
/// Used behind tracing_appender's bounded nonblocking queue, never on an HTTP task.
pub struct RollingLog {
    root: PathBuf,
    file: std::fs::File,
    bytes: u64,
    max: u64,
    keep: usize,
}
impl RollingLog {
    pub fn new(root: PathBuf, max: u64, keep: usize) -> std::io::Result<Self> {
        if max == 0 || keep == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "log limits must be positive",
            ));
        }
        std::fs::create_dir_all(&root)?;
        // Apply reduced limits to previously owned generations as well.
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let generation = name
                .strip_prefix("server.")
                .and_then(|s| s.strip_suffix(".log"))
                .and_then(|s| s.parse::<usize>().ok());
            if !entry.file_type()?.is_file() {
                continue;
            }
            if generation.is_some_and(|n| n > keep) {
                std::fs::remove_file(entry.path())?;
            } else if (name == "server.log" || generation.is_some_and(|n| n > 0))
                && entry.metadata()?.len() > max
            {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(entry.path())?
                    .set_len(max)?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("server.log"))?;
        let bytes = file.metadata()?.len();
        Ok(Self {
            root,
            file,
            bytes,
            max,
            keep,
        })
    }
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        for n in (1..self.keep).rev() {
            let old = self.root.join(format!("server.{n}.log"));
            if old.exists() {
                std::fs::rename(old, self.root.join(format!("server.{}.log", n + 1)))?;
            }
        }
        if self.keep > 0 {
            std::fs::rename(self.root.join("server.log"), self.root.join("server.1.log"))?;
        }
        self.file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.root.join("server.log"))?;
        self.bytes = 0;
        Ok(())
    }
}
impl Write for RollingLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut offset = 0;
        while offset < buf.len() {
            if self.bytes >= self.max {
                self.rotate()?;
            }
            let n = ((self.max - self.bytes) as usize).min(buf.len() - offset);
            self.file.write_all(&buf[offset..offset + n])?;
            self.bytes += n as u64;
            offset += n;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rolling_logs_bound_bytes_and_generations() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RollingLog::new(dir.path().to_owned(), 64, 2).unwrap();
        log.write_all(&[b'x'; 500]).unwrap();
        log.flush().unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(files.len(), 3);
        for f in files {
            assert!(f.unwrap().metadata().unwrap().len() <= 64);
        }
        drop(log);
        let _smaller = RollingLog::new(dir.path().to_owned(), 16, 1).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            assert!(entry.unwrap().metadata().unwrap().len() <= 16);
        }
    }
    #[test]
    fn disk_reserve_rejects_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(require_space(dir.path(), u64::MAX, 1).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
