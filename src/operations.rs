use crate::{
    App,
    api::{ApiError, admin},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::Serialize;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use utoipa::ToSchema;

pub struct Health {
    pub started: Instant,
    pub worker_running: AtomicBool,
    pub shutting_down: AtomicBool,
    pub demuxe_available: bool,
}
impl Health {
    pub fn new(demuxe_available: bool) -> Self {
        Self {
            started: Instant::now(),
            worker_running: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            demuxe_available,
        }
    }
}
#[derive(Serialize, ToSchema)]
pub struct Readiness {
    pub ready: bool,
    pub database: bool,
    pub worker: bool,
    pub shutting_down: bool,
}
async fn health(app: &App) -> Readiness {
    let database = matches!(tokio::time::timeout(std::time::Duration::from_secs(2),sqlx::query_scalar::<_,i64>("SELECT count(*) FROM _sqlx_migrations WHERE success=1").fetch_one(&app.db)).await,Ok(Ok(n)) if n>0);
    let worker = app.health.worker_running.load(Ordering::Relaxed);
    let shutting_down = app.health.shutting_down.load(Ordering::Relaxed);
    Readiness {
        ready: database && worker && !shutting_down,
        database,
        worker,
        shutting_down,
    }
}
#[utoipa::path(operation_id="server_readiness",get,path="/ready",responses((status=200,description="Database responds and supervised worker is running",body=Readiness),(status=503,description="Not ready or shutting down",body=Readiness)))]
pub async fn ready(State(app): State<App>) -> (StatusCode, Json<Readiness>) {
    let r = health(&app).await;
    (
        if r.ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(r),
    )
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct JobCount {
    pub phase: String,
    pub count: i64,
}
#[derive(Serialize, ToSchema)]
pub struct Diagnostics {
    pub version: String,
    pub uptime_seconds: u64,
    pub readiness: Readiness,
    pub demuxe_present_at_startup: bool,
    pub ffprobe_available: bool,
    pub libraries: i64,
    pub files: i64,
    pub unavailable_files: i64,
    pub jobs: Vec<JobCount>,
    pub stream_slots_available: usize,
    pub database_pages: i64,
    pub database_page_size: i64,
}
#[utoipa::path(operation_id="server_diagnostics",get,path="/api/v1/admin/diagnostics",security(("admin_token"=[])),responses((status=200,description="Bounded operational summary; no secrets or filesystem paths",body=Diagnostics),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=500,description="Database query failed",body=crate::api::ErrorBody)))]
pub async fn diagnostics(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<Diagnostics>, ApiError> {
    admin(&app, &headers)?;
    let readiness = health(&app).await;
    let probe = tokio::process::Command::new(app.ffprobe.as_ref())
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let ffprobe_available = matches!(tokio::time::timeout(std::time::Duration::from_secs(2),probe).await,Ok(Ok(status)) if status.success());
    let mut tx = app.db.begin().await?;
    let libraries = sqlx::query_scalar("SELECT count(*) FROM libraries")
        .fetch_one(&mut *tx)
        .await?;
    let (files, unavailable_files): (i64, i64) =
        sqlx::query_as("SELECT count(*),coalesce(sum(available=0),0) FROM media_files")
            .fetch_one(&mut *tx)
            .await?;
    let jobs =
        sqlx::query_as("SELECT phase,count(*) AS count FROM jobs GROUP BY phase ORDER BY phase")
            .fetch_all(&mut *tx)
            .await?;
    let database_pages = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&mut *tx)
        .await?;
    let database_page_size = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(Diagnostics {
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_seconds: app.health.started.elapsed().as_secs(),
        readiness,
        demuxe_present_at_startup: app.health.demuxe_available,
        ffprobe_available,
        libraries,
        files,
        unavailable_files,
        jobs,
        stream_slots_available: app.streams.available_permits(),
        database_pages,
        database_page_size,
    }))
}
