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
    let result = make_snapshot(app, permit, None, false).await;
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
    requested: Option<String>,
    include_generated: bool,
) -> anyhow::Result<Snapshot> {
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&app.db)
        .await?;
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&app.db)
        .await?;
    let root = app.storage.root.join("backups");
    let detailed = requested.is_some();
    let destination = root.join(requested.unwrap_or(format!(
        "auto-{:020}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        new_id()
    )));
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
    let assets: Vec<(String, i64)> = if detailed {
        sqlx::query_as("SELECT id,length(bytes) FROM artwork_assets ORDER BY id LIMIT 100001")
            .fetch_all(&mut check)
            .await?
    } else {
        vec![]
    };
    let generated: Vec<(String, String, String, String, i64)> = if include_generated {
        sqlx::query_as("SELECT f.id,f.revision,l.root,f.relative_path,f.bytes FROM media_files f JOIN libraries l ON l.id=f.library_id WHERE f.generated=1 AND f.available=1 ORDER BY f.id LIMIT 100001").fetch_all(&mut check).await?
    } else {
        vec![]
    };
    anyhow::ensure!(
        assets.len() <= 100_000 && generated.len() <= 100_000,
        "backup inventory exceeds supported bound"
    );
    check.close().await?;
    let keep = app.storage.settings.backups_keep;
    let min_free = app.storage.settings.min_free_bytes;
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
        let mut media = Vec::new();
        if include_generated {
            std::fs::create_dir(stage.join("generated"))?;
            let required = generated.iter().try_fold(0u64,|n,(_,_,_,_,bytes)| n.checked_add((*bytes).max(0) as u64)).ok_or_else(||anyhow::anyhow!("generated inventory overflow"))?;
            require_space(&stage, required, min_free)?;
            for (id,revision,source_root,relative,expected_bytes) in generated {
                anyhow::ensure!(!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()||b==b'-'||b==b'_'), "invalid generated identity");
                let mut input = crate::scan::open_file(Path::new(&source_root),Path::new(&relative))?;
                let name=format!("generated/{id}");
                let mut output=std::fs::OpenOptions::new().create_new(true).write(true).open(stage.join(&name))?;
                let mut hash=Sha256::new(); let mut bytes=0u64;
                loop { let n=input.read(&mut buf)?; if n==0 {break;} bytes=bytes.checked_add(n as u64).ok_or_else(||anyhow::anyhow!("media size overflow"))?; anyhow::ensure!(bytes<=expected_bytes as u64,"generated file grew"); hash.update(&buf[..n]);output.write_all(&buf[..n])?; }
                let digest=format!("{:x}",hash.finalize());
                anyhow::ensure!(bytes==expected_bytes as u64 && digest==revision,"generated file changed");
                output.sync_all()?;
                media.push(serde_json::json!({"file_id":id,"file_revision":revision,"path":name,"bytes":bytes.to_string(),"sha256":digest}));
            }
            #[cfg(unix)]
            std::fs::File::open(stage.join("generated"))?.sync_all()?;
        }
        let mut value=serde_json::to_value(&snapshot)?;
        if detailed {
        value["assets"]=serde_json::json!(assets.into_iter().map(|(id,bytes)|serde_json::json!({"asset_id":id,"bytes":bytes.to_string(),"storage":"database"})).collect::<Vec<_>>());
        value["include_generated_media"]=serde_json::json!(include_generated);
        value["generated_media"]=serde_json::json!(media);
        }
        let mut manifest = std::fs::File::create(stage.join("manifest.json"))?;
        let manifest_bytes=serde_json::to_vec_pretty(&value)?;
        anyhow::ensure!(manifest_bytes.len()<=16*1024*1024,"backup manifest too large");
        manifest.write_all(&manifest_bytes)?;
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
    sqlx::query("DELETE FROM processing_jobs WHERE id IN (SELECT id FROM processing_jobs WHERE updated_at<? AND cache_cleaned=1 AND id NOT IN (SELECT job_id FROM preparations) AND phase IN ('completed','failed','cancelled') ORDER BY updated_at LIMIT 500)").bind(cutoff).execute(&mut *tx).await?;
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
        if let Err(error) = crate::v2::admin::run_backup(&app).await {
            tracing::warn!(%error, "requested backup failed");
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
/// Stable job destination makes a crash after rename but before DB acknowledgement
/// recoverable. Never overwrite an existing completed snapshot.
pub(crate) struct VerifiedBackup {
    pub manifest_digest: String,
    pub database_verified: bool,
    pub inventory_complete: bool,
    pub destination_durable: bool,
}
pub(crate) async fn requested_backup(
    app: &App,
    id: &str,
    include_generated: bool,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
) -> anyhow::Result<VerifiedBackup> {
    let _maintenance = app.processing.maintenance.lock().await;
    let name = format!("request-{id}");
    let destination = app.storage.root.join("backups").join(&name);
    if !destination.exists() {
        let stage = destination.with_extension("partial");
        let owned = stage.clone();
        blocking(permit.clone(), move || {
            if owned.exists() {
                anyhow::ensure!(
                    !std::fs::symlink_metadata(&owned)?.file_type().is_symlink()
                        && std::fs::read(owned.join(".playscale-backup"))?
                            == b"playscale-scheduled-v1",
                    "unowned backup stage"
                );
                std::fs::remove_dir_all(owned)?;
            }
            Ok(())
        })
        .await?;
        make_snapshot(app, permit.clone(), Some(name), include_generated).await?;
    }
    let verify = destination.clone();
    let digest = blocking(permit.clone(), move || {
        verify_requested_snapshot(&verify, include_generated)
    })
    .await?;
    // Completeness is derived from the immutable snapshot, not from the list
    // the manifest happens to contain. Omitted files must fail recovery.
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(destination.join("playscale.sqlite3"))
        .read_only(true);
    let mut check = sqlx::SqliteConnection::connect_with(&options).await?;
    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut check)
        .await?;
    anyhow::ensure!(integrity == ["ok"], "backup integrity failed");
    let assets: Vec<(String, i64)> =
        sqlx::query_as("SELECT id,length(bytes) FROM artwork_assets ORDER BY id LIMIT 100001")
            .fetch_all(&mut check)
            .await?;
    let media: Vec<(String, String, i64)> = if include_generated {
        sqlx::query_as("SELECT id,revision,bytes FROM media_files WHERE generated=1 AND available=1 ORDER BY id LIMIT 100001").fetch_all(&mut check).await?
    } else {
        vec![]
    };
    anyhow::ensure!(
        assets.len() <= 100_000 && media.len() <= 100_000,
        "backup inventory exceeds supported bound"
    );
    check.close().await?;
    let expected_assets=serde_json::json!(assets.into_iter().map(|(id,bytes)|serde_json::json!({"asset_id":id,"bytes":bytes.to_string(),"storage":"database"})).collect::<Vec<_>>());
    let expected_media=serde_json::json!(media.into_iter().map(|(id,revision,bytes)|serde_json::json!({"file_id":id,"file_revision":revision,"path":format!("generated/{id}"),"bytes":bytes.to_string(),"sha256":revision})).collect::<Vec<_>>());
    blocking(permit, move || {
        let manifest = read_backup_manifest(&destination)?;
        let value: serde_json::Value = serde_json::from_slice(&manifest)?;
        anyhow::ensure!(
            format!("sha256:{}", crate::v2::sha256(&manifest)) == digest,
            "manifest changed during recovery"
        );
        anyhow::ensure!(
            value["assets"] == expected_assets && value["generated_media"] == expected_media,
            "backup inventory incomplete"
        );
        // Re-establish durability even when the prior process died immediately
        // after rename and before its parent-directory sync/acknowledgement.
        std::fs::File::open(destination.join("manifest.json"))?.sync_all()?;
        std::fs::File::open(destination.join("playscale.sqlite3"))?.sync_all()?;
        #[cfg(unix)]
        {
            if include_generated {
                std::fs::File::open(destination.join("generated"))?.sync_all()?;
            }
            std::fs::File::open(&destination)?.sync_all()?;
            std::fs::File::open(destination.parent().unwrap())?.sync_all()?;
        }
        Ok(VerifiedBackup {
            manifest_digest: digest,
            database_verified: true,
            inventory_complete: true,
            destination_durable: true,
        })
    })
    .await
}
fn verify_requested_snapshot(
    destination: &Path,
    include_generated: bool,
) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    anyhow::ensure!(
        !std::fs::symlink_metadata(destination)?
            .file_type()
            .is_symlink(),
        "backup destination is a symlink"
    );
    let manifest = read_backup_manifest(destination)?;
    let value: serde_json::Value = serde_json::from_slice(&manifest)?;
    anyhow::ensure!(
        value["include_generated_media"].as_bool() == Some(include_generated),
        "backup input mismatch"
    );
    let hash_file = |path: &Path| -> anyhow::Result<(String, u64)> {
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buf = [0; 65536];
        let mut size = 0;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            hash.update(&buf[..n]);
        }
        Ok((format!("{:x}", hash.finalize()), size))
    };
    let (digest, bytes) = hash_file(&destination.join("playscale.sqlite3"))?;
    anyhow::ensure!(
        value["sha256"].as_str() == Some(&digest) && value["bytes"].as_u64() == Some(bytes),
        "backup database integrity changed"
    );
    for media in value["generated_media"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("missing inventory"))?
    {
        let id = media["file_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing media identity"))?;
        anyhow::ensure!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid media identity"
        );
        let (digest, bytes) = hash_file(&destination.join("generated").join(id))?;
        anyhow::ensure!(
            media["sha256"].as_str() == Some(&digest)
                && media["bytes"].as_str() == Some(&bytes.to_string()),
            "backup media integrity changed"
        );
    }
    Ok(format!("sha256:{:x}", Sha256::digest(manifest)))
}

fn read_backup_manifest(destination: &Path) -> anyhow::Result<Vec<u8>> {
    let manifest_path = destination.join("manifest.json");
    anyhow::ensure!(
        std::fs::metadata(&manifest_path)?.len() <= 16 * 1024 * 1024,
        "backup manifest too large"
    );
    let mut manifest = Vec::new();
    std::fs::File::open(manifest_path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut manifest)?;
    anyhow::ensure!(
        manifest.len() <= 16 * 1024 * 1024,
        "backup manifest too large"
    );
    Ok(manifest)
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
