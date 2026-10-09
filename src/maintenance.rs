use crate::{
    App,
    api::{ApiError, admin, json},
    db, now,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Schedule {
    pub library_id: String,
    pub interval_seconds: i64,
    pub next_run: i64,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleRequest {
    /// Zero disables scheduling; otherwise 60 seconds to one year.
    pub interval_seconds: i64,
}
#[utoipa::path(operation_id="list_scan_schedules",get,path="/api/v1/admin/scan-schedules",security(("admin_token"=[])),responses((status=200,description="Persisted schedules",body=Vec<Schedule>)))]
pub async fn schedules(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<Vec<Schedule>>, ApiError> {
    admin(&app, &headers)?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM scan_schedules ORDER BY library_id")
            .fetch_all(&app.db)
            .await?,
    ))
}
#[utoipa::path(operation_id="set_scan_schedule",put,path="/api/v1/admin/scan-schedules/{library}",params(("library"=String,Path)),request_body=ScheduleRequest,security(("admin_token"=[])),responses((status=200,description="Schedule replaced; zero disables",body=Vec<Schedule>),(status=400,description="Invalid interval",body=crate::api::ErrorBody)))]
pub async fn schedule(
    State(app): State<App>,
    Path(library): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ScheduleRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Vec<Schedule>>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    if r.interval_seconds != 0 && !(60..=31_536_000).contains(&r.interval_seconds) {
        return Err(ApiError::bad(
            "Interval must be zero or 60..31536000 seconds",
        ));
    }
    let _lock = app.jobs.lock().await;
    let _: String =
        sqlx::query_scalar("SELECT id FROM libraries WHERE id=? AND managed=0 AND enabled=1")
            .bind(&library)
            .fetch_one(&app.db)
            .await?;
    if r.interval_seconds == 0 {
        sqlx::query("DELETE FROM scan_schedules WHERE library_id=?")
            .bind(library)
            .execute(&app.db)
            .await?;
    } else {
        sqlx::query("INSERT INTO scan_schedules VALUES (?,?,?) ON CONFLICT(library_id) DO UPDATE SET interval_seconds=excluded.interval_seconds,next_run=excluded.next_run").bind(library).bind(r.interval_seconds).bind(now()+r.interval_seconds).execute(&app.db).await?;
    }
    schedules(State(app.clone()), headers).await
}
#[derive(Serialize, ToSchema)]
pub struct CacheStatus {
    pub bytes: u64,
    pub budget_bytes: u64,
    pub max_output_bytes: u64,
    pub retention_seconds: u64,
    pub removed_jobs: usize,
}
async fn status(app: &App, removed_jobs: usize) -> Result<CacheStatus, ApiError> {
    let root = app.processing.root.clone();
    let permit = app
        .processing
        .inspection
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "cache_inspection_busy",
                "Cache inspection is busy",
            )
        })?;
    let bytes = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::processing::cache_bytes(&root)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    let s = &app.processing.settings;
    Ok(CacheStatus {
        bytes,
        budget_bytes: s.cache_bytes,
        max_output_bytes: s.max_output_bytes,
        retention_seconds: s.retention_seconds,
        removed_jobs,
    })
}
#[utoipa::path(operation_id="get_cache_status",get,path="/api/v1/admin/cache",security(("admin_token"=[])),responses((status=200,description="Generated-media cache usage and limits",body=CacheStatus)))]
pub async fn cache_status(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<CacheStatus>, ApiError> {
    admin(&app, &headers)?;
    let _guard = app.processing.maintenance.lock().await;
    Ok(Json(status(&app, 0).await?))
}
#[utoipa::path(operation_id="clean_generated_cache",post,path="/api/v1/admin/cache",security(("admin_token"=[])),responses((status=200,description="Remove expired and failed generated attempts; originals are never removed",body=CacheStatus)))]
pub async fn clean_cache(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<CacheStatus>, ApiError> {
    admin(&app, &headers)?;
    let n = clean(&app).await.map_err(ApiError::internal)?;
    Ok(Json(status(&app, n).await?))
}
pub async fn clean(app: &App) -> anyhow::Result<usize> {
    clean_with(app, |path| {
        if !path.exists() {
            return Ok(false);
        }
        anyhow::ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "cache job directory is a symlink"
        );
        std::fs::remove_dir_all(path)?;
        Ok(true)
    })
    .await
}
async fn clean_with(
    app: &App,
    remove: impl Fn(std::path::PathBuf) -> anyhow::Result<bool> + Send + Sync + 'static,
) -> anyhow::Result<usize> {
    let remove = std::sync::Arc::new(remove);
    let _maintenance = app.processing.maintenance.lock().await;
    let Ok(permit) = app.processing.io.clone().try_acquire_owned() else {
        return Ok(0);
    };
    let permit = std::sync::Arc::new(permit);
    let paths = {
        let _lock = app.jobs.lock().await;
        #[derive(sqlx::FromRow)]
        struct Observation {
            id: String,
            output_file_id: Option<String>,
            attempt: i64,
            phase: String,
            updated_at: i64,
            last_active_playback: Option<i64>,
        }
        // Read bounded pages of observations; the core owns retention eligibility.
        let mut rows = Vec::new();
        let mut offset = 0i64;
        let clock = now();
        loop {
            let page: Vec<Observation> = sqlx::query_as("SELECT p.id,p.output_file_id,p.attempt,p.phase,p.updated_at,(SELECT max(s.updated_at) FROM playback_sessions s WHERE s.file_id=p.output_file_id AND s.status IN ('playing','paused')) AS last_active_playback FROM processing_jobs p WHERE p.cache_cleaned=0 AND (p.phase IN ('failed','cancelled') OR (p.phase='completed' AND p.updated_at<?)) AND NOT EXISTS(SELECT 1 FROM playback_sessions s WHERE s.file_id=p.output_file_id AND s.status IN ('playing','paused') AND s.updated_at>?) ORDER BY p.updated_at,p.id LIMIT 200 OFFSET ?")
                .bind(playscale_core::maintenance::retention_cutoff(clock, app.processing.settings.retention_seconds)).bind(playscale_core::maintenance::playback_cutoff(clock)).bind(offset).fetch_all(&app.db).await?;
            let count = page.len();
            for Observation {
                id,
                output_file_id: file,
                attempt,
                phase,
                updated_at,
                last_active_playback,
            } in page
            {
                let state = playscale_core::maintenance::CacheEntry {
                    job: playscale_core::jobs::Job {
                        phase: serde_json::from_value(serde_json::json!(phase))?,
                        attempt: attempt.try_into()?,
                    },
                    cleaned: false,
                    updated_at,
                    last_active_playback,
                };
                if state.removable(clock, app.processing.settings.retention_seconds) {
                    rows.push((id, file, attempt));
                    if rows.len() == 200 {
                        break;
                    }
                }
            }
            if count < 200 || rows.len() == 200 {
                break;
            }
            offset += count as i64;
        }
        let mut tx = crate::db::begin_write(&app.db).await?;
        let mut paths = Vec::new();
        for (id, file, attempt) in rows {
            if let Some(file) = file {
                sqlx::query(
                    "UPDATE media_files SET available=0 WHERE id=? AND generated=1 AND available=1",
                )
                .bind(file)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query("UPDATE processing_jobs SET expired=1 WHERE id=? AND phase='completed' AND expired=0").bind(&id).execute(&mut *tx).await?;
            paths.push((app.processing.root.join(&id), Some((id, attempt))));
        }
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT id,attempt FROM processing_jobs WHERE phase='queued' AND attempt>0 LIMIT 100",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (id, attempt) in rows {
            let state = playscale_core::maintenance::CacheEntry {
                job: playscale_core::jobs::Job {
                    phase: playscale_core::jobs::Phase::Queued,
                    attempt: attempt.try_into()?,
                },
                cleaned: false,
                updated_at: 0,
                last_active_playback: None,
            };
            if let Some(attempt) = state.abandoned_attempt() {
                paths.push((app.processing.root.join(id).join(attempt.to_string()), None));
            }
        }
        tx.commit().await?;
        paths
    }; // No writer lock or transaction is held during filesystem operations.
    let mut removed = 0;
    for (path, job) in paths {
        let hold = permit.clone();
        let remove = remove.clone();
        let deleted = tokio::task::spawn_blocking(move || {
            let _hold = hold;
            remove(path)
        })
        .await??;
        if deleted {
            removed += 1;
        }
        if let Some((id, attempt)) = job {
            let _lock = app.jobs.lock().await;
            let current = crate::processing::load(app, &id).await?;
            let state = playscale_core::maintenance::CacheEntry {
                job: playscale_core::jobs::Job {
                    phase: serde_json::from_value(serde_json::json!(current.phase))?,
                    attempt: current.attempt.try_into()?,
                },
                cleaned: current.cache_cleaned,
                updated_at: current.updated_at,
                last_active_playback: None,
            };
            if state.acknowledge_removal(attempt.try_into()?) {
                sqlx::query("UPDATE processing_jobs SET cache_cleaned=1 WHERE id=? AND attempt=?")
                    .bind(id)
                    .bind(attempt)
                    .execute(&app.db)
                    .await?;
            }
        }
    }
    Ok(removed)
}
pub async fn tick(app: &App) -> anyhow::Result<()> {
    let rows: Vec<Schedule> = sqlx::query_as("SELECT * FROM scan_schedules WHERE next_run<=?")
        .bind(now())
        .fetch_all(&app.db)
        .await?;
    for row in rows {
        let _lock = app.jobs.lock().await;
        let mut tx = db::begin_write(&app.db).await?;
        let current: Option<Schedule> =
            sqlx::query_as("SELECT * FROM scan_schedules WHERE library_id=?")
                .bind(&row.library_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(current) = current else {
            continue;
        };
        let policy = playscale_core::maintenance::Schedule {
            next_run: current.next_run,
            interval: current.interval_seconds,
        };
        let Some(next) = policy.claim(now()).map_err(anyhow::Error::msg)? else {
            continue;
        };
        db::enqueue_transaction(&mut tx, &row.library_id, false).await?;
        sqlx::query("UPDATE scan_schedules SET next_run=? WHERE library_id=?")
            .bind(next.next_run)
            .bind(row.library_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    clean(app).await?;
    Ok(())
}
pub async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    loop {
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=tokio::time::sleep(std::time::Duration::from_secs(30))=>{if let Err(error)=tick(&app).await {tracing::warn!(%error,"scheduled maintenance failed");let _=crate::storage::record(&app,"cache_and_scans",Some("maintenance_failed")).await;}else{let _=crate::storage::record(&app,"cache_and_scans",None).await;}}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Mutex, Semaphore};
    #[tokio::test]
    async fn stalled_cache_delete_does_not_hold_writer_and_keeps_io_admission() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let library = db::add_library(&db, "fixture", dir.path()).await.unwrap();
        sqlx::query("INSERT INTO items (id,title,kind) VALUES ('item','Fixture','video')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO editions(id,item_id,label) VALUES ('edition','item','Original')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json,available) VALUES ('file','edition',?,'fixture.mp4','hash','stamp',1,'[]',1)").bind(library.id).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at) VALUES ('job','file','hash','h264720p','software','key','failed',0,0)").execute(&db).await.unwrap();
        let app = App {
            health: Arc::new(crate::operations::Health::new(false)),
            db,
            admin_token: Arc::new("test".into()),
            origin: Arc::new("http://localhost".into()),
            authority: Arc::new("localhost".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(1)),
            event_streams: Arc::new(Semaphore::new(1)),
            storage: Arc::new(crate::storage::Runtime::new(
                dir.path().to_owned(),
                Default::default(),
            )),
            processing: Arc::new(crate::processing::Runtime::new(
                dir.path().join("cache"),
                Default::default(),
            )),
            access: Arc::new(crate::v2::Runtime::new(
                playscale_core::access::AccessMode::TrustedHousehold,
                crate::v2::auth::random_key(),
            )),
        };
        let (entered, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let blocked = std::sync::Mutex::new(blocked);
        let worker = app.clone();
        let task = tokio::spawn(async move {
            clean_with(&worker, move |_| {
                entered.send(()).unwrap();
                blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))?;
                Ok(true)
            })
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let writer = tokio::time::timeout(std::time::Duration::from_secs(1), app.jobs.lock())
            .await
            .unwrap();
        sqlx::query("UPDATE profiles SET name='Still responsive' WHERE id='default'")
            .execute(&app.db)
            .await
            .unwrap();
        drop(writer);
        assert!(app.processing.io.clone().try_acquire_owned().is_err());
        // Dropping an async request must not free the uncancellable filesystem slot.
        task.abort();
        let _ = task.await;
        assert!(app.processing.io.clone().try_acquire_owned().is_err());
        release.send(()).unwrap();
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            app.processing.io.clone().acquire_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permit);
    }
}
