//! Durable, explicitly requested local conversions. Recipes are data, never command strings.
use crate::{
    App,
    api::{ApiError, admin, json},
    db, new_id, now, scan,
};
use anyhow::Context;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use playscale_core::jobs::{Effect, Input, Job, transition};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Semaphore};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

mod progress;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub ffmpeg: PathBuf,
    pub cache_bytes: u64,
    pub max_output_bytes: u64,
    pub retention_seconds: u64,
    pub timeout_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            cache_bytes: 20 * 1024 * 1024 * 1024,
            max_output_bytes: 2 * 1024 * 1024 * 1024,
            retention_seconds: 30 * 86400,
            timeout_seconds: 7200,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.max_output_bytes >= 1024 * 1024
                && self.max_output_bytes <= self.cache_bytes
                && self.cache_bytes <= i64::MAX as u64,
            "invalid processing cache/output budget"
        );
        anyhow::ensure!(
            (60..=31_536_000).contains(&self.retention_seconds)
                && (1..=86400).contains(&self.timeout_seconds),
            "invalid processing retention/timeout"
        );
        Ok(())
    }
}
pub struct Runtime {
    pub root: PathBuf,
    pub settings: Settings,
    // The permit follows uncancellable blocking work, including after shutdown.
    pub io: Arc<Semaphore>,
    pub inspection: Arc<Semaphore>,
    pub maintenance: tokio::sync::Mutex<()>,
    /// Shared encoder capacity; released only after confirmed worker termination.
    pub execution: Arc<crate::execution::Coordinator>,
    /// Executable that accepts `--internal-ffmpeg-supervisor`.
    pub supervisor: PathBuf,
    /// Live delivery sessions share the execution infrastructure.
    pub deliveries: crate::delivery::Runtime,
}
impl Runtime {
    pub fn new(root: PathBuf, settings: Settings) -> Self {
        Self {
            settings,
            io: Arc::new(Semaphore::new(1)),
            inspection: Arc::new(Semaphore::new(1)),
            maintenance: tokio::sync::Mutex::new(()),
            execution: crate::execution::Coordinator::new(crate::execution::BUDGET),
            supervisor: std::env::current_exe().unwrap_or_default(),
            deliveries: crate::delivery::Runtime::new(root.parent().unwrap_or(&root).join("live")),
            root,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Recipe {
    RemuxMp4,
    AudioAac,
    H264720p,
    VideoProfile,
}
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Software,
    Videotoolbox,
}
#[derive(Clone, Serialize, Deserialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub source_file_id: String,
    pub source_revision: String,
    pub recipe: Recipe,
    pub backend: Backend,
    pub idempotency_key: String,
    #[serde(default)]
    pub video_profile: Option<crate::encoding::VideoProfile>,
}
#[derive(Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct ProcessingJob {
    pub id: String,
    pub source_file_id: String,
    pub source_revision: String,
    pub recipe: String,
    pub backend: String,
    #[serde(skip)]
    pub idempotency_key: String,
    pub phase: String,
    pub attempt: i64,
    pub progress_seconds: f64,
    pub output_file_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expired: bool,
    pub cache_cleaned: bool,
    #[schema(value_type = Option<crate::encoding::VideoProfile>)]
    pub video_profile: Option<sqlx::types::Json<crate::encoding::VideoProfile>>,
}
fn name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}
impl ProcessingJob {
    fn state(&self) -> anyhow::Result<Job> {
        Ok(Job {
            phase: serde_json::from_value(serde_json::json!(self.phase))?,
            attempt: self.attempt.try_into()?,
        })
    }
    fn relative(&self) -> PathBuf {
        PathBuf::from(&self.id).join(self.attempt.to_string())
    }
}
pub async fn load(app: &App, id: &str) -> Result<ProcessingJob, sqlx::Error> {
    sqlx::query_as("SELECT * FROM processing_jobs WHERE id=?")
        .bind(id)
        .fetch_one(&app.db)
        .await
}
#[utoipa::path(operation_id="submit_processing",post,path="/api/v1/processing-jobs",request_body=Request,security(("admin_token"=[])),responses((status=200,description="Idempotent existing request",body=ProcessingJob),(status=201,description="Queued immutable recipe",body=ProcessingJob),(status=400,description="Invalid recipe",body=crate::api::ErrorBody),(status=409,description="Source or idempotency conflict",body=crate::api::ErrorBody),(status=503,description="Queue full",body=crate::api::ErrorBody)))]
pub async fn submit(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<Request>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<ProcessingJob>), ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    if (r.recipe == Recipe::VideoProfile) != r.video_profile.is_some() {
        return Err(ApiError::bad(
            "video_profile is required only for the video_profile recipe",
        ));
    }
    if let Some(profile) = &r.video_profile {
        profile.validate().map_err(ApiError::bad)?;
    }
    if r.idempotency_key.is_empty()
        || r.idempotency_key.len() > 128
        || r.source_revision.len() != 64
        || (r.backend == Backend::Videotoolbox
            && (!matches!(r.recipe, Recipe::H264720p | Recipe::VideoProfile)
                || !cfg!(target_os = "macos")))
    {
        return Err(ApiError::bad(
            "Invalid recipe, backend, revision or idempotency key",
        ));
    }
    let _lock = app.jobs.lock().await;
    if let Some(old) =
        sqlx::query_as::<_, ProcessingJob>("SELECT * FROM processing_jobs WHERE idempotency_key=?")
            .bind(&r.idempotency_key)
            .fetch_optional(&app.db)
            .await?
    {
        use playscale_core::processing::{RequestIdentity, reuse};
        let existing = RequestIdentity {
            source_file: old.source_file_id.clone(),
            source_revision: old.source_revision.clone(),
            recipe: old.recipe.clone(),
            backend: old.backend.clone(),
            options: old
                .video_profile
                .as_deref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(ApiError::internal)?,
        };
        let requested = RequestIdentity {
            source_file: r.source_file_id.clone(),
            source_revision: r.source_revision.clone(),
            recipe: name(&r.recipe),
            backend: name(&r.backend),
            options: r
                .video_profile
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(ApiError::internal)?,
        };
        reuse(&existing, &requested)
            .map_err(|code| ApiError::conflict(code, "Key already identifies another request"))?;
        return Ok((StatusCode::OK, Json(old)));
    }
    let row: db::ItemRow =
        sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND available=1 AND generated=0 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
            .bind(&r.source_file_id)
            .fetch_one(&app.db)
            .await?;
    playscale_core::processing::SourceAdmission {
        revision: &row.revision,
        duration: row.duration_seconds,
    }
    .check(&r.source_revision)
    .map_err(|code| {
        if code == "source_revision_changed" {
            ApiError::conflict(code, "Refresh the source")
        } else {
            ApiError::bad("Processing requires a probed, positive source duration")
        }
    })?;
    if r.video_profile
        .as_ref()
        .is_some_and(|profile| !profile.admits_duration(row.duration_seconds.unwrap_or(0.0)))
    {
        return Err(ApiError::bad(
            "Source is shorter than half a requested frame interval; choose a higher frame rate",
        ));
    }
    if r.recipe == Recipe::VideoProfile {
        let tracks: Vec<db::Track> =
            serde_json::from_str(&row.tracks_json).map_err(ApiError::internal)?;
        let video = tracks
            .iter()
            .find(|t| t.kind == "video")
            .ok_or_else(|| ApiError::bad("Video profiles require a probed video stream"))?;
        if video.width.is_none()
            || video.height.is_none()
            || matches!(
                video.color_transfer.as_deref(),
                Some("smpte2084" | "arib-std-b67")
            )
        {
            return Err(ApiError::bad(
                "Video profiles require known dimensions and SDR input; rescan legacy metadata",
            ));
        }
    }
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM processing_jobs WHERE phase IN ('queued','running','cancelling')",
    )
    .fetch_one(&app.db)
    .await?;
    if !playscale_core::processing::queue_admits(queued) {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "processing_limit",
            "Processing queue is full",
        ));
    }
    let id = new_id();
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at,video_profile) VALUES (?,?,?,?,?,?,'queued',?,?,?)")
        .bind(&id).bind(r.source_file_id).bind(r.source_revision).bind(name(&r.recipe)).bind(name(&r.backend)).bind(r.idempotency_key).bind(now()).bind(now()).bind(r.video_profile.map(sqlx::types::Json)).execute(&app.db).await?;
    Ok((StatusCode::CREATED, Json(load(&app, &id).await?)))
}
#[utoipa::path(operation_id="list_processing_jobs",get,path="/api/v1/processing-jobs",responses((status=200,description="Most recent 100 processing jobs",body=Vec<ProcessingJob>)))]
pub async fn list(State(app): State<App>) -> Result<Json<Vec<ProcessingJob>>, ApiError> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM processing_jobs ORDER BY created_at DESC,id DESC LIMIT 100")
            .fetch_all(&app.db)
            .await?,
    ))
}
#[utoipa::path(operation_id="get_processing_job",get,path="/api/v1/processing-jobs/{id}",params(("id"=String,Path)),responses((status=200,description="Processing state and validated output identity",body=ProcessingJob),(status=404,description="Unknown job",body=crate::api::ErrorBody)))]
pub async fn get_job(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<ProcessingJob>, ApiError> {
    Ok(Json(load(&app, &id).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub action: String,
}
#[utoipa::path(operation_id="control_processing_job",post,path="/api/v1/processing-jobs/{id}/control",params(("id"=String,Path)),request_body=Control,security(("admin_token"=[])),responses((status=200,description="Cancel or retry failed/cancelled job",body=ProcessingJob),(status=409,description="Retry is not possible",body=crate::api::ErrorBody)))]
pub async fn control(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Control>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ProcessingJob>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let _lock = app.jobs.lock().await;
    let old = load(&app, &id).await?;
    let input = match r.action.as_str() {
        "cancel" => Input::Cancel,
        "retry" if old.state().map_err(ApiError::internal)?.retryable() => Input::Retry,
        "retry" => {
            return Err(ApiError::conflict(
                "not_retryable",
                "Only failed or cancelled jobs can retry",
            ));
        }
        _ => return Err(ApiError::bad("action must be cancel or retry")),
    };
    let previous = old.state().map_err(ApiError::internal)?;
    let (next, _) = transition(&previous, input);
    if next == previous {
        return Ok(Json(old));
    }
    sqlx::query(
        "UPDATE processing_jobs SET phase=?,cache_cleaned=0,error=NULL,updated_at=? WHERE id=?",
    )
    .bind(db::phase_name(next.phase))
    .bind(now())
    .bind(&id)
    .execute(&app.db)
    .await?;
    Ok(Json(load(&app, &id).await?))
}
/// Witness files under `root` still held by live executions. Fails rather than
/// under-reporting: an unreadable directory or witness means ownership is unknown.
pub fn held_witnesses(root: &FsPath) -> anyhow::Result<Vec<(PathBuf, crate::execution::Witness)>> {
    if !root.try_exists()? {
        return Ok(Vec::new());
    }
    let mut held = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == ".owner" {
            let witness = crate::execution::Witness::open(entry.path())?;
            if !witness.released() {
                held.push((entry.path().to_owned(), witness));
            }
        }
    }
    Ok(held)
}
pub async fn recover(app: &App) -> anyhow::Result<()> {
    // Encoders of a previous process that are still alive keep their capacity until
    // they are gone; their attempt directories are left alone until then.
    let root = app.processing.root.clone();
    for (path, witness) in tokio::task::spawn_blocking(move || held_witnesses(&root)).await?? {
        app.processing.execution.recovered(
            format!("recovered:{}", path.display()),
            playscale_core::work::Class::Preparation,
            1,
            witness,
            || {},
        );
    }
    // A DB-only restore must not borrow another installation's disposable cache.
    sqlx::query("UPDATE media_files SET available=0 WHERE generated=1 AND library_id IN (SELECT id FROM libraries WHERE managed=1 AND root<>?)")
        .bind(app.processing.root.to_string_lossy().as_ref()).execute(&app.db).await?;
    sqlx::query("UPDATE processing_jobs SET expired=1 WHERE phase='completed' AND output_file_id IN (SELECT id FROM media_files WHERE generated=1 AND available=0)").execute(&app.db).await?;
    let rows: Vec<ProcessingJob> =
        sqlx::query_as("SELECT * FROM processing_jobs WHERE phase IN ('running','cancelling')")
            .fetch_all(&app.db)
            .await?;
    for row in rows {
        let (next, _) = transition(&row.state()?, Input::Recover);
        sqlx::query(
            "UPDATE processing_jobs SET phase=?,progress_seconds=0,updated_at=? WHERE id=?",
        )
        .bind(db::phase_name(next.phase))
        .bind(now())
        .bind(row.id)
        .execute(&app.db)
        .await?;
    }
    Ok(())
}
async fn active(app: &App, job: &ProcessingJob, stop: &CancellationToken) -> anyhow::Result<()> {
    let row = load(app, &job.id).await?;
    anyhow::ensure!(
        !stop.is_cancelled() && row.state()?.executing(job.attempt.try_into()?),
        "processing interrupted"
    );
    Ok(())
}
// One permit for the entire attempt; clones remain held by blocking filesystem operations.
async fn blocking<T: Send + 'static>(
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await?
}
pub fn cache_bytes(root: &FsPath) -> anyhow::Result<u64> {
    if !root.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    for e in walkdir::WalkDir::new(root).follow_links(false) {
        let e = e?;
        anyhow::ensure!(
            !e.file_type().is_symlink(),
            "managed cache contains a symlink"
        );
        if e.file_type().is_file() {
            total = total
                .checked_add(e.metadata()?.len())
                .context("cache size overflow")?;
        }
    }
    Ok(total)
}
async fn command(
    app: &App,
    job: &ProcessingJob,
    stop: &CancellationToken,
    lease: &crate::execution::Lease,
    owner: &FsPath,
    cmd: &mut Command,
    progress: bool,
) -> anyhow::Result<()> {
    // A separate supervisor watches this pipe. Even SIGKILL of the server closes
    // it, so an encoder cannot outlive its owner and overlap a recovered attempt.
    let command = cmd.as_std();
    let mut supervisor = Command::new(&app.processing.supervisor);
    supervisor
        .arg("--internal-ffmpeg-supervisor")
        .arg(command.get_program())
        .args(command.get_args())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());
    // The encoder tree inherits the witness; capacity returns only once it is free.
    let (witness, held) = crate::execution::Witness::create(owner)?;
    #[cfg(unix)]
    crate::execution::inherit(&mut supervisor, &[&held.0]);
    let child = supervisor.spawn();
    drop(held);
    let mut child = child?;
    lease.started(owner)?;
    let heartbeat = child.stdin.take();
    let mut stderr = child.stderr.take().context("missing encoder stderr")?;
    let diagnostics = tokio::spawn(async move {
        let mut tail = Vec::new();
        let mut buffer = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buffer).await {
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buffer[..n]);
            if tail.len() > 16384 {
                tail.drain(..tail.len() - 16384);
            }
        }
        String::from_utf8_lossy(&tail).into_owned()
    });
    let mut stdout = child
        .stdout
        .take()
        .context("missing encoder progress pipe")?;
    let mut output_buffer = [0u8; 4096];
    let mut decoder = progress::Decoder::default();
    let deadline = tokio::time::sleep(Duration::from_secs(app.processing.settings.timeout_seconds));
    tokio::pin!(deadline);
    let mut output_open = true;
    let mut pending_progress = 0.0f64;
    let mut poll = tokio::time::interval(Duration::from_millis(200));
    let result: anyhow::Result<()> = async {
        loop {tokio::select!{
            _=&mut deadline=>anyhow::bail!("processing timeout"),
            _=stop.cancelled()=>anyhow::bail!("server stopping"),
            _=lease.termination_requested().cancelled()=>anyhow::bail!("termination requested"),
            status=child.wait()=>{anyhow::ensure!(status?.success(),"FFmpeg failed");break;},
            read=stdout.read(&mut output_buffer),if output_open=>{
                let count = read?;
                let observed = if count == 0 {
                    output_open = false;
                    decoder.finish()
                } else {
                    decoder.push(&output_buffer[..count])
                };
                if let Some(seconds) = observed {
                    pending_progress = seconds;
                }
            },
            _=poll.tick()=>{active(app,job,stop).await?;if progress{sqlx::query("UPDATE processing_jobs SET progress_seconds=max(progress_seconds,?),updated_at=? WHERE id=? AND attempt=? AND phase='running'").bind(pending_progress).bind(now()).bind(&job.id).bind(job.attempt).execute(&app.db).await?;}}
        }} Ok(())
    }.await;
    drop(heartbeat);
    // On failure, EOF asks the supervisor to terminate the encoder's process group;
    // deliberately do not kill the supervisor first. Either way capacity is released
    // only after the supervisor exits and the witness is free; otherwise the
    // reservation is held as stuck.
    let confirmed = crate::execution::confirm_exit(
        lease,
        result.is_err().then_some(child),
        Some(witness),
        crate::execution::TERMINATION_DEADLINE,
        || {},
    )
    .await;
    let result = result.and_then(|()| {
        anyhow::ensure!(confirmed, "encoder descendants outlived the supervisor");
        Ok(())
    });
    if result.is_err()
        && let Ok(Ok(stderr)) = tokio::time::timeout(Duration::from_secs(1), diagnostics).await
    {
        tracing::warn!(job_id=%job.id,%stderr,"encoder diagnostics");
    }
    result
}
async fn convert(
    app: &App,
    job: &ProcessingJob,
    stop: &CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    lease: &crate::execution::Lease,
) -> anyhow::Result<(scan::Found, String)> {
    active(app, job, stop).await?;
    let source: db::ItemRow =
        sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND available=1 AND generated=0 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
            .bind(&job.source_file_id)
            .fetch_one(&app.db)
            .await?;
    anyhow::ensure!(
        source.revision == job.source_revision,
        "stale source revision"
    );
    let (root, identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&source.library_id)
            .fetch_one(&app.db)
            .await?;
    let directory = app.processing.root.join(job.relative());
    let stage = directory.clone();
    let cache = app.processing.root.clone();
    let settings = app.processing.settings.clone();
    let expected = job.source_revision.clone();
    let stamp = source.fingerprint.clone();
    let relative = source.relative_path.clone();
    let copy_stop = stop.child_token();
    let cancellation = copy_stop.clone();
    let min_free = app.storage.settings.min_free_bytes;
    let previous = job.attempt - 1;
    let snapshot = blocking(permit.clone(), move || {
        use sha2::{Digest, Sha256};
        use std::io::{Read, Write};
        std::fs::create_dir_all(&cache)?;
        if previous > 0 {
            let old = stage.parent().unwrap().join(previous.to_string());
            if old.exists() {
                std::fs::remove_dir_all(old)?;
            }
        }
        crate::storage::require_space(
            &cache,
            (source.bytes as u64)
                .saturating_add(settings.max_output_bytes)
                .saturating_add(1024 * 1024),
            min_free,
        )?;
        anyhow::ensure!(
            cache_bytes(&cache)?
                .saturating_add(source.bytes as u64)
                .saturating_add(settings.max_output_bytes)
                .saturating_add(1024 * 1024)
                <= settings.cache_bytes,
            "cache budget exhausted"
        );
        anyhow::ensure!(
            db::root_identity(&std::fs::metadata(&root)?) == identity,
            "source root changed"
        );
        let mut input = scan::open_file(FsPath::new(&root), FsPath::new(&relative))?;
        anyhow::ensure!(
            scan::fingerprint(&input.metadata()?) == stamp,
            "source changed"
        );
        std::fs::create_dir_all(&stage)?;
        let mut copy = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.join("input"))?;
        let mut hash = Sha256::new();
        let mut buf = [0u8; 128 * 1024];
        let mut bytes = 0u64;
        loop {
            anyhow::ensure!(!cancellation.is_cancelled(), "processing stopped");
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            anyhow::ensure!(bytes <= source.bytes as u64, "source grew");
            hash.update(&buf[..n]);
            copy.write_all(&buf[..n])?;
        }
        copy.sync_all()?;
        anyhow::ensure!(
            format!("{:x}", hash.finalize()) == expected
                && scan::fingerprint(&input.metadata()?) == stamp,
            "source changed while snapshotting"
        );
        Ok(())
    });
    tokio::pin!(snapshot);
    loop {
        tokio::select! {
            result=&mut snapshot=>{result?;break;},
            _=tokio::time::sleep(Duration::from_millis(200))=>{
                if let Err(error)=active(app,job,stop).await {copy_stop.cancel();let _=snapshot.await;return Err(error);}
            }
        }
    }
    active(app, job, stop).await?;
    let output = directory.join("output.mp4");
    let mut cmd = Command::new(&app.processing.settings.ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-n",
        "-protocol_whitelist",
        "file,pipe",
        "-format_whitelist",
        "mov,matroska,webm,avi,mpegts,mp3,flac,ogg,wav",
        "-i",
    ])
    .arg(directory.join("input"));
    cmd.args([
        "-map",
        "0:v:0?",
        "-map",
        "0:a:0?",
        "-sn",
        "-dn",
        "-map_metadata",
        "-1",
    ]);
    match job.recipe.as_str() {
        "remux_mp4" => {
            cmd.args(["-c", "copy"]);
        }
        "audio_aac" => {
            cmd.args(["-c:v", "copy", "-c:a", "aac", "-b:a", "192k"]);
        }
        "h264720p" => {
            cmd.args(["-vf","scale=w='min(1280,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2","-pix_fmt","yuv420p","-c:v"]);
            if job.backend == "videotoolbox" {
                cmd.args(["h264_videotoolbox", "-allow_sw", "0", "-b:v", "2500k"]);
            } else {
                cmd.args([
                    "libx264", "-preset", "veryfast", "-crf", "23", "-threads", "2",
                ]);
            }
            cmd.args(["-c:a", "aac", "-b:a", "160k"]);
        }
        "video_profile" => {
            let profile = job
                .video_profile
                .as_ref()
                .context("missing video profile")?;
            profile.validate().map_err(anyhow::Error::msg)?;
            cmd.args(profile.arguments(job.backend == "videotoolbox"));
        }
        _ => anyhow::bail!("unknown recipe"),
    }
    cmd.args(["-movflags", "+faststart", "-fs"])
        .arg(app.processing.settings.max_output_bytes.to_string())
        .args(["-progress", "pipe:1", "-stats_period", "0.2", "-f", "mp4"])
        .arg(&output);
    command(
        app,
        job,
        stop,
        lease,
        &directory.join(".owner"),
        &mut cmd,
        true,
    )
    .await?;
    let found = scan::inspect(
        app.processing.root.clone(),
        job.relative().join("output.mp4"),
        &app.ffprobe,
        stop.clone(),
        permit.clone(),
    )
    .await?;
    let duration = source.duration_seconds.context("source duration unknown")?;
    let source_tracks: Vec<db::Track> = serde_json::from_str(&source.tracks_json)?;
    let observations = |tracks: &[db::Track]| {
        tracks
            .iter()
            .map(|t| playscale_core::processing::Track {
                kind: t.kind.clone(),
                codec: t.codec.clone(),
                width: t.width,
                height: t.height,
                frame_rate: t.average_frame_rate.as_ref().and_then(|r| r.value()),
            })
            .collect::<Vec<_>>()
    };
    playscale_core::processing::validate_output(
        &playscale_core::processing::OutputObservation {
            bytes: found.bytes,
            duration: found.duration,
            tracks: &observations(&found.tracks),
        },
        &playscale_core::processing::OutputRequirements {
            max_bytes: app.processing.settings.max_output_bytes,
            source_duration: duration,
            source_tracks: &observations(&source_tracks),
            recipe: &job.recipe,
            video: job.video_profile.as_ref().map(|p| {
                playscale_core::processing::VideoRequirements {
                    codec: p.codec_name().into(),
                    max_width: p.max_width,
                    max_height: p.max_height,
                    frame_rate: p.frame_rate.as_ref().and_then(|r| r.value()),
                }
            }),
        },
    )
    .map_err(anyhow::Error::msg)?;
    let mut decode = Command::new(&app.processing.settings.ffmpeg);
    decode
        .args(["-v", "error", "-xerror", "-nostdin", "-i"])
        .arg(&output)
        .args([
            "-map",
            "0:v:0?",
            "-map",
            "0:a:0?",
            "-progress",
            "pipe:1",
            "-f",
            "null",
            "-",
        ]);
    command(
        app,
        job,
        stop,
        lease,
        &directory.join(".owner"),
        &mut decode,
        false,
    )
    .await?;
    let (root, identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&source.library_id)
            .fetch_one(&app.db)
            .await?;
    let relative = source.relative_path.clone();
    let stamp = source.fingerprint.clone();
    let durable_output = output.clone();
    blocking(permit.clone(), move || {
        std::fs::File::open(&durable_output)?.sync_all()?;
        #[cfg(unix)]
        std::fs::File::open(durable_output.parent().context("output parent")?)?.sync_all()?;
        anyhow::ensure!(
            db::root_identity(&std::fs::metadata(&root)?) == identity,
            "source root changed"
        );
        let file = scan::open_file(FsPath::new(&root), FsPath::new(&relative))?;
        anyhow::ensure!(
            scan::fingerprint(&file.metadata()?) == stamp,
            "source changed during processing"
        );
        Ok(())
    })
    .await?;
    let library = db::add_library(&app.db, "Generated media", &app.processing.root).await?;
    sqlx::query("UPDATE libraries SET managed=1 WHERE id=?")
        .bind(&library.id)
        .execute(&app.db)
        .await?;
    tokio::fs::remove_file(directory.join("input")).await?;
    Ok((found, library.id))
}
async fn finish(
    app: &App,
    job: &ProcessingJob,
    result: Option<(scan::Found, String)>,
) -> anyhow::Result<()> {
    let _lock = app.jobs.lock().await;
    let current = load(app, &job.id).await?;
    let source: Option<(String, String)> =
        sqlx::query_as("SELECT revision,edition_id FROM media_files WHERE id=? AND available=1")
            .bind(&job.source_file_id)
            .fetch_optional(&app.db)
            .await?;
    let previous = playscale_core::processing::State {
        job: current.state()?,
        source_revision: current.source_revision.clone(),
        published_output: current.output_file_id.clone(),
    };
    let output_id = result.as_ref().map(|_| new_id());
    let (next, effects) = playscale_core::processing::finish(
        &previous,
        &playscale_core::processing::Completion {
            attempt: job.attempt.try_into()?,
            current_source_revision: source.as_ref().map(|s| s.0.clone()),
            validated_output: output_id.clone(),
        },
    );
    if next == previous && effects.is_empty() {
        return Ok(());
    }
    let next = next.job;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let mut output = None;
    if effects.iter().any(|e| matches!(e, Effect::Publish { .. })) {
        let (found, library) = result.unwrap();
        let edition = &source.unwrap().1;
        let file = output_id.expect("publication requires validated output");
        let item: String = sqlx::query_scalar("SELECT item_id FROM editions WHERE id=?")
            .bind(edition)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds,tracks_json,available,generated) VALUES (?,?,?,?,?,?,?,?,?,1,1)").bind(&file).bind(edition).bind(library).bind(found.relative).bind(&found.revision).bind(found.fingerprint).bind(found.bytes).bind(found.duration).bind(serde_json::to_string(&found.tracks)?).execute(&mut *tx).await?;
        // A derived rendition joins a timeline only if one of the edition's
        // versions pins exactly the source content this job read. Content, not
        // file identity: the source may be a byte-identical copy (occurrence)
        // of the bound file.
        let pinned: Vec<(String, i64, bool)> = sqlx::query_as(
            "SELECT v.timeline_id,(SELECT count(*) FROM version_files p WHERE p.version_id=v.id),b.start_ms IS NOT NULL OR b.end_ms IS NOT NULL FROM version_files b JOIN media_versions v ON v.id=b.version_id JOIN timelines t ON t.id=v.timeline_id WHERE b.file_revision=? AND t.edition_id=? AND v.origin='original'",
        )
        .bind(&job.source_revision)
        .bind(edition)
        .fetch_all(&mut *tx)
        .await?;
        let pinned: Vec<playscale_core::identity::PinnedSource> = pinned
            .into_iter()
            .map(
                |(timeline, parts, interval)| playscale_core::identity::PinnedSource {
                    timeline,
                    parts: usize::try_from(parts).unwrap_or(usize::MAX),
                    interval,
                },
            )
            .collect();
        let (timeline, equivalence) = match playscale_core::identity::rendition_placement(&pinned) {
            playscale_core::identity::RenditionPlacement::Join { timeline } => {
                (timeline, playscale_core::identity::Equivalence::Declared)
            }
            playscale_core::identity::RenditionPlacement::OwnTimeline => {
                let timeline = new_id();
                sqlx::query("INSERT INTO timelines (id,edition_id) VALUES (?,?)")
                    .bind(&timeline)
                    .bind(edition)
                    .execute(&mut *tx)
                    .await?;
                (timeline, playscale_core::identity::Equivalence::Unknown)
            }
        };
        crate::curation::create_version(
            &mut tx,
            &timeline,
            &file,
            playscale_core::identity::Origin::Generated,
            equivalence,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let recipe = serde_json::json!({"version":1,"recipe":job.recipe,"backend":job.backend,"job_id":job.id,"attempt":job.attempt,"video_profile":job.video_profile});
        sqlx::query("INSERT INTO renditions VALUES (?,?,?,?,?,?,?,?,?,?,?)")
            .bind(new_id())
            .bind(item)
            .bind("playscale")
            .bind(&job.id)
            .bind(&file)
            .bind(found.revision)
            .bind(&job.source_file_id)
            .bind(&job.source_revision)
            .bind(&job.recipe)
            .bind(recipe.to_string())
            .bind(now())
            .execute(&mut *tx)
            .await?;
        output = Some(file);
    }
    sqlx::query("UPDATE processing_jobs SET phase=?,output_file_id=coalesce(?,output_file_id),error=?,updated_at=? WHERE id=? AND attempt=?").bind(db::phase_name(next.phase)).bind(output).bind(if db::phase_name(next.phase)=="failed"{Some("processing_failed: source changed, budget exceeded, unsupported media, or encoder failure; see server logs")}else{None}).bind(now()).bind(&job.id).bind(job.attempt).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        // Hold no capacity while idle: only reserve when work is queued.
        let queued: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM processing_jobs WHERE phase='queued' LIMIT 1")
                .fetch_optional(&app.db)
                .await?;
        if queued.is_none() {
            tokio::select! {_=stop.cancelled()=>return Ok(()),_=tokio::time::sleep(Duration::from_millis(500))=>{}}
            continue;
        }
        // Reserve encoder capacity before any I/O permit, so a stuck previous worker
        // blocks replacement here without holding up maintenance.
        let lease = tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            lease = app.processing.execution.reserve(format!("processing:{}", new_id()), playscale_core::work::Class::Preparation, 1) => lease.map_err(|e| anyhow::anyhow!("encoder reservation rejected: {e:?}"))?,
        };
        let permit = tokio::select! {_ = stop.cancelled()=>return Ok(()),p=app.processing.io.clone().acquire_owned()=>Arc::new(p?)};
        let job = {
            let _lock = app.jobs.lock().await;
            let row = sqlx::query_as::<_, ProcessingJob>(
                "SELECT * FROM processing_jobs WHERE phase='queued' ORDER BY created_at,id LIMIT 1",
            )
            .fetch_optional(&app.db)
            .await?;
            if let Some(row) = row {
                let (next, effects) = transition(&row.state()?, Input::Start);
                let runs = matches!(effects.as_slice(), [Effect::Run { .. }]);
                sqlx::query("UPDATE processing_jobs SET phase=?,attempt=?,progress_seconds=0,error=?,updated_at=? WHERE id=?").bind(db::phase_name(next.phase)).bind(next.attempt).bind((!runs).then_some("job attempt limit reached")).bind(now()).bind(&row.id).execute(&app.db).await?;
                if runs {
                    Some(load(&app, &row.id).await?)
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(job) = job {
            // Do not detach a blocked snapshot and start another job. The permit and this
            // waiter both remain until it finishes; cancellation is checked before FFmpeg.
            let result = convert(&app, &job, &stop, permit.clone(), &lease).await;
            if stop.is_cancelled() {
                return Ok(());
            }
            if let Err(error) = &result {
                tracing::warn!(job_id=%job.id,%error,"processing failed");
            }
            finish(&app, &job, result.ok()).await?;
            // A stuck attempt's directory holds its live witness: keep it for recovery.
            if load(&app, &job.id).await?.phase != "completed" && !lease.is_stuck() {
                let directory = app.processing.root.join(job.relative());
                blocking(permit.clone(), move || {
                    if directory.exists() {
                        std::fs::remove_dir_all(directory)?;
                    }
                    Ok(())
                })
                .await?;
            }
        }
        drop(permit);
        drop(lease);
        tokio::select! {_=stop.cancelled()=>return Ok(()),_=tokio::time::sleep(Duration::from_millis(200))=>{}}
    }
}

/// Private child entry point. No HTTP caller can supply executable arguments.
pub fn supervise_encoder(args: impl Iterator<Item = std::ffi::OsString>) -> anyhow::Result<i32> {
    let mut args = args;
    let program = args.next().context("missing encoder")?;
    supervise(program, args.collect(), std::io::stdin(), SUPERVISOR_GRACE)
}

/// Time between asking an encoder's process group to stop and killing it.
pub const SUPERVISOR_GRACE: Duration = Duration::from_secs(2);

/// Run `program` in its own process group and own that group until it is gone.
/// EOF on `control` (the server closed the pipe or died) terminates the group:
/// SIGTERM, then SIGKILL after `grace`. A natural exit also kills any remaining
/// group members. The group is signalled only while the leader is an unreaped
/// child, so its ID cannot have been reused by an unrelated process group.
/// Descendants that create their own session or group escape this containment.
pub fn supervise(
    program: std::ffi::OsString,
    args: Vec<std::ffi::OsString>,
    control: impl std::io::Read + Send + 'static,
    grace: Duration,
) -> anyhow::Result<i32> {
    use std::sync::atomic::{AtomicU8, Ordering};
    // Control protocol: b'p' pauses the group, b'r' resumes it; EOF, an error or
    // any other byte terminates it.
    const RUN: u8 = 0;
    const PAUSE: u8 = 1;
    const STOP: u8 = 2;
    let wanted = Arc::new(AtomicU8::new(RUN));
    let flag = wanted.clone();
    std::thread::spawn(move || {
        let mut control = control;
        let mut byte = [0u8; 1];
        loop {
            let next = match control.read(&mut byte) {
                Ok(1) if byte[0] == b'p' => PAUSE,
                Ok(1) if byte[0] == b'r' => RUN,
                _ => STOP,
            };
            flag.store(next, Ordering::SeqCst);
            if next == STOP {
                return;
            }
        }
    });
    let mut paused = false;
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn()?;
    loop {
        if group::exited(&mut child)? {
            group::signal(&mut child, group::Signal::Kill);
            let status = child.wait()?;
            group::wait_empty(&child);
            return Ok(status.code().unwrap_or(1));
        }
        let wanted = wanted.load(Ordering::SeqCst);
        if wanted != STOP && (wanted == PAUSE) != paused {
            paused = wanted == PAUSE;
            group::signal(
                &mut child,
                if paused {
                    group::Signal::Pause
                } else {
                    group::Signal::Resume
                },
            );
        }
        if wanted == STOP {
            // A stopped process acts on SIGTERM only after it is continued.
            group::signal(&mut child, group::Signal::Resume);
            group::signal(&mut child, group::Signal::Terminate);
            let deadline = std::time::Instant::now() + grace;
            while std::time::Instant::now() < deadline && !group::exited(&mut child)? {
                std::thread::sleep(Duration::from_millis(25));
            }
            group::signal(&mut child, group::Signal::Kill);
            let _ = child.wait();
            group::wait_empty(&child);
            return Ok(1);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

mod group {
    pub enum Signal {
        Terminate,
        Kill,
        Pause,
        Resume,
    }
    /// Whether the child exited, without reaping it (its PID/PGID stay reserved).
    #[cfg(unix)]
    pub fn exited(child: &mut std::process::Child) -> std::io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // macOS can report a stopped child here despite WEXITED alone; only an
        // actual exit counts.
        Ok(unsafe { info.si_pid() } != 0
            && matches!(
                info.si_code,
                libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
            ))
    }
    /// After the leader is reaped, wait (bounded) until no member of its group
    /// remains. The group ID cannot be reused while any member exists, and only
    /// the null signal is sent here, so a later reuse cannot be harmed. The
    /// execution witness remains the server's authoritative evidence.
    #[cfg(unix)]
    pub fn wait_empty(child: &std::process::Child) {
        let group = -(child.id() as libc::pid_t);
        for _ in 0..400 {
            if unsafe { libc::kill(group, 0) } != 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    #[cfg(not(unix))]
    pub fn wait_empty(_: &std::process::Child) {}
    #[cfg(unix)]
    pub fn signal(child: &mut std::process::Child, signal: Signal) {
        let signal = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
            Signal::Pause => libc::SIGSTOP,
            Signal::Resume => libc::SIGCONT,
        };
        // ESRCH (group already empty) is expected and ignored.
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), signal);
        }
    }
    // Without a Job Object adapter only the direct child is owned. It is killed
    // through its handle, never by a possibly reused PID.
    #[cfg(not(unix))]
    pub fn exited(child: &mut std::process::Child) -> std::io::Result<bool> {
        Ok(child.try_wait()?.is_some())
    }
    #[cfg(not(unix))]
    pub fn signal(child: &mut std::process::Child, signal: Signal) {
        // No pausing without a Job Object adapter; only termination is supported.
        if matches!(signal, Signal::Terminate | Signal::Kill) {
            let _ = child.kill();
        }
    }
}

#[cfg(all(test, unix))]
mod supervisor_tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }
    fn wait_gone(pid: i32) -> bool {
        (0..200).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            !alive(pid)
        })
    }
    fn grandchild(dir: &FsPath) -> i32 {
        let path = dir.join("pid");
        for _ in 0..500 {
            if let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("grandchild did not start");
    }

    #[test]
    fn control_eof_kills_whole_group_including_term_ignoring_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!(
            "trap '' TERM; (trap '' TERM; exec sleep 300) & echo $! > {}/pid; wait",
            dir.path().display()
        );
        let (server, supervisor) = UnixStream::pair().unwrap();
        let task = std::thread::spawn(move || {
            supervise(
                "/bin/sh".into(),
                vec!["-c".into(), script.into()],
                supervisor,
                Duration::from_millis(200),
            )
        });
        let pid = grandchild(dir.path());
        assert!(alive(pid));
        drop(server);
        assert_eq!(task.join().unwrap().unwrap(), 1);
        assert!(wait_gone(pid), "descendant survived supervisor");
    }

    #[test]
    fn pause_and_resume_the_group_then_terminate_while_paused() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let ticks = dir.path().join("ticks");
        let script = format!(
            "echo $$ > {}/pid; while :; do echo x >> {}; sleep 0.05; done",
            dir.path().display(),
            ticks.display()
        );
        let (mut server, supervisor) = UnixStream::pair().unwrap();
        let task = std::thread::spawn(move || {
            supervise(
                "/bin/sh".into(),
                vec!["-c".into(), script.into()],
                supervisor,
                Duration::from_millis(500),
            )
        });
        let pid = grandchild(dir.path());
        let size = || std::fs::metadata(&ticks).map_or(0, |m| m.len());
        std::thread::sleep(Duration::from_millis(300));
        server.write_all(b"p").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let paused = size();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(size(), paused, "group kept running while paused");
        server.write_all(b"r").unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(size() > paused, "group did not resume");
        server.write_all(b"p").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(server);
        assert_eq!(task.join().unwrap().unwrap(), 1);
        assert!(wait_gone(pid));
    }

    #[test]
    fn natural_exit_cleans_up_remaining_group_members() {
        let dir = tempfile::tempdir().unwrap();
        let script = format!("sleep 300 & echo $! > {}/pid; exit 3", dir.path().display());
        let (_server, supervisor) = UnixStream::pair().unwrap();
        let code = supervise(
            "/bin/sh".into(),
            vec!["-c".into(), script.into()],
            supervisor,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(code, 3);
        assert!(wait_gone(grandchild(dir.path())));
    }
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct EncoderEvidence {
    pub backend: String,
    pub job_id: String,
    pub validated_at: i64,
}
#[derive(Serialize, ToSchema)]
pub struct Capabilities {
    pub recipes: Vec<Recipe>,
    pub backends: Vec<Backend>,
    pub worker_concurrency: usize,
    pub queue_limit: usize,
    /// Successful, fully decoded jobs on this installation; not a universal codec guarantee.
    pub validated_jobs: Vec<EncoderEvidence>,
}
#[utoipa::path(operation_id="processing_capabilities",get,path="/api/v1/processing-capabilities",responses((status=200,description="Supported recipes and configured backend choices with successful-job evidence",body=Capabilities)))]
pub async fn capabilities(State(app): State<App>) -> Result<Json<Capabilities>, ApiError> {
    let mut backends = vec![Backend::Software];
    if cfg!(target_os = "macos") {
        backends.push(Backend::Videotoolbox);
    }
    let validated_jobs=sqlx::query_as("SELECT backend,id AS job_id,updated_at AS validated_at FROM processing_jobs WHERE phase='completed' AND recipe='h264720p' ORDER BY updated_at DESC,id LIMIT 20").fetch_all(&app.db).await?;
    Ok(Json(Capabilities {
        recipes: vec![
            Recipe::RemuxMp4,
            Recipe::AudioAac,
            Recipe::H264720p,
            Recipe::VideoProfile,
        ],
        backends,
        worker_concurrency: 1,
        queue_limit: 100,
        validated_jobs,
    }))
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[tokio::test]
    async fn publication_is_atomic_and_duplicate_stale_completions_are_noops() {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let library = db::add_library(&pool, "fixture", dir.path()).await.unwrap();
        sqlx::query("INSERT INTO items (id,title,kind) VALUES ('item','Fixture','video')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO editions(id,item_id,label) VALUES ('edition','item','Original')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,tracks_json,available) VALUES ('source','edition',?,'source.mp4','source-revision','stamp',1,'[]',1)").bind(&library.id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,attempt,created_at,updated_at) VALUES ('job','source','source-revision','h264720p','software','key','running',1,0,0)").execute(&pool).await.unwrap();
        let app = App {
            health: Arc::new(crate::operations::Health::new(false)),
            db: pool,
            admin_token: Arc::new("test".into()),
            origin: Arc::new("http://localhost".into()),
            authority: Arc::new("localhost".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(tokio::sync::Mutex::new(())),
            streams: Arc::new(Semaphore::new(1)),
            event_streams: Arc::new(Semaphore::new(1)),
            storage: Arc::new(crate::storage::Runtime::new(
                dir.path().to_owned(),
                Default::default(),
            )),
            processing: Arc::new(Runtime::new(dir.path().join("cache"), Default::default())),
            access: Arc::new(crate::v2::Runtime::new(
                playscale_core::access::AccessMode::TrustedHousehold,
                crate::v2::auth::random_key(),
            )),
        };
        let output = || {
            Some((
                scan::Found {
                    relative: "job/1/output.mp4".into(),
                    title: "Output".into(),
                    revision: "output-revision".into(),
                    fingerprint: "output-stamp".into(),
                    bytes: 1,
                    duration: Some(1.0),
                    tracks: vec![],
                    reused: false,
                },
                library.id.clone(),
            ))
        };
        let job = load(&app, "job").await.unwrap();
        sqlx::query("CREATE TRIGGER reject_processing_commit BEFORE UPDATE OF phase ON processing_jobs WHEN NEW.phase='completed' BEGIN SELECT RAISE(ABORT,'injected publication failure'); END").execute(&app.db).await.unwrap();
        assert!(finish(&app, &job, output()).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files WHERE generated=1")
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM renditions")
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(load(&app, "job").await.unwrap().phase, "running");
        sqlx::query("DROP TRIGGER reject_processing_commit")
            .execute(&app.db)
            .await
            .unwrap();
        finish(&app, &job, output()).await.unwrap();
        let published = load(&app, "job").await.unwrap().output_file_id.unwrap();
        sqlx::query("UPDATE processing_jobs SET updated_at=123,error='sentinel' WHERE id='job'")
            .execute(&app.db)
            .await
            .unwrap();
        finish(&app, &job, output()).await.unwrap();
        let current = load(&app, "job").await.unwrap();
        assert_eq!(current.output_file_id.as_deref(), Some(published.as_str()));
        assert_eq!(current.updated_at, 123);
        assert_eq!(current.error.as_deref(), Some("sentinel"));
        // A delayed result from an earlier attempt must not touch a running retry.
        sqlx::query("UPDATE processing_jobs SET phase='running',attempt=2,output_file_id=NULL WHERE id='job'").execute(&app.db).await.unwrap();
        finish(&app, &job, output()).await.unwrap();
        assert_eq!(load(&app, "job").await.unwrap().updated_at, 123);
        let retry = load(&app, "job").await.unwrap();
        sqlx::query("UPDATE media_files SET revision='replacement' WHERE id='source'")
            .execute(&app.db)
            .await
            .unwrap();
        finish(&app, &retry, output()).await.unwrap();
        assert_eq!(load(&app, "job").await.unwrap().phase, "failed");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM renditions")
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}
