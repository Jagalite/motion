//! Live HLS delivery sessions: an experimental v1 surface for the v2 delivery
//! lifecycle. Decisions live in `playscale_core::delivery`; this adapter runs one
//! supervised FFmpeg per generation under an interactive execution reservation,
//! reports atomically published segments, and serves only what the reducer offers.
use crate::{
    App,
    api::{ApiError, admin, json},
    db, execution, new_id, scan,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use playscale_core::{
    delivery::{self as core, Delivery, Effect, Input, Operation, Pin},
    work::Class,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

/// Concurrent delivery sessions. Each may briefly run two encoders while overlapping.
pub const MAX_SESSIONS: usize = 4;
/// Bound owned admission tasks, including requests whose HTTP waiters vanished.
pub const MAX_ADMISSIONS: usize = 32;
/// Requested segment length; keyframes are forced on this cadence.
pub const SEGMENT_SECONDS: u64 = 4;
/// Pinned HLS target duration. Longer segments fail the generation.
pub const TARGET_SECONDS: u64 = 6;
/// Free space a generation needs beyond the configured floor before it starts.
/// Pacing and retention bound the segments it keeps; this covers them at the
/// recipe's peak rate with margin.
pub const GENERATION_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
/// A published segment larger than this fails its generation.
pub const MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Recipe identity bound into every transcoding generation pin.
pub const RECIPE: &str = "hls-fmp4-h264-720p-aac-stereo-v1";
/// Stream copy of H.264 and AAC into fMP4 HLS, source timestamps preserved.
pub const REMUX_RECIPE: &str = "hls-fmp4-copy-h264-copy-aac-v1";
/// Stream copy of H.264 with audio converted to stereo AAC.
pub const AUDIO_RECIPE: &str = "hls-fmp4-copy-h264-aac-stereo-v1";
/// Copied segments end at source keyframes; GOPs up to this long are served.
pub const COPY_TARGET_SECONDS: u64 = 12;

/// Live routes a client may request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LiveOperation {
    VideoTranscode,
    Remux,
    AudioConvert,
}
impl LiveOperation {
    fn operation(self) -> Operation {
        match self {
            LiveOperation::VideoTranscode => Operation::VideoTranscode,
            LiveOperation::Remux => Operation::Remux,
            LiveOperation::AudioConvert => Operation::AudioConvert,
        }
    }
}

fn copies_video(operation: Operation) -> bool {
    matches!(operation, Operation::Remux | Operation::AudioConvert)
}

fn target_seconds(operation: Operation) -> u64 {
    if copies_video(operation) {
        COPY_TARGET_SECONDS
    } else {
        TARGET_SECONDS
    }
}
const HEARTBEAT_SECONDS: u64 = 10;

pub struct Runtime {
    /// Exclusively owned scratch space; emptied on startup.
    pub root: PathBuf,
    started: Instant,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    admission: tokio::sync::Mutex<()>,
    admission_slots: Arc<tokio::sync::Semaphore>,
    stopping: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
struct Source {
    root: PathBuf,
    relative: PathBuf,
    root_identity: String,
    fingerprint: String,
}

struct Inner {
    delivery: Delivery,
    /// Cancellation for each generation worker that may still be running.
    workers: HashMap<u64, WorkerControl>,
}

/// Control of one generation worker: cancellation and pacing (true = paused).
struct WorkerControl {
    stop: CancellationToken,
    pace: tokio::sync::mpsc::UnboundedSender<bool>,
}

struct Session {
    id: String,
    file_id: String,
    revision: String,
    source: Source,
    inner: Mutex<Inner>,
}

impl Runtime {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            started: Instant::now(),
            sessions: Mutex::new(HashMap::new()),
            admission: tokio::sync::Mutex::new(()),
            admission_slots: Arc::new(tokio::sync::Semaphore::new(MAX_ADMISSIONS)),
            stopping: std::sync::atomic::AtomicBool::new(false),
        }
    }
    /// Monotonic milliseconds for lease and drain deadlines.
    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
    fn session(&self, id: &str) -> Result<Arc<Session>, ApiError> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(ApiError::not_found)
    }
    /// Whether this process holds the delivery live in memory.
    pub fn is_live(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(id)
    }
    pub fn active_sessions(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

impl Session {
    fn directory(&self, app: &App, generation: u64) -> PathBuf {
        app.processing
            .deliveries
            .root
            .join(&self.id)
            .join(generation.to_string())
    }
}

/// Apply one input and dispatch its effects while holding the session lock, so
/// effects of consecutive transitions cannot be reordered (e.g. Stop before Start).
fn apply(app: &App, session: &Arc<Session>, input: Input) -> Result<Delivery, core::Error> {
    apply_if(app, session, |_| true, input)
}

/// `apply`, refused with a generation conflict unless `current` holds for the
/// state the input would apply to (checked under the same lock).
fn apply_if(
    app: &App,
    session: &Arc<Session>,
    current: impl FnOnce(&Delivery) -> bool,
    input: Input,
) -> Result<Delivery, core::Error> {
    let mut inner = session.inner.lock().unwrap();
    if !current(&inner.delivery) {
        return Err(core::Error::GenerationConflict);
    }
    let (next, effects) = core::transition(&inner.delivery, &input)?;
    let outline = |d: &Delivery| (d.status, d.active, d.pending, d.last_generation);
    if outline(&next) != outline(&inner.delivery) {
        persist(app, session, &next);
    }
    inner.delivery = next.clone();
    dispatch(app, session, &mut inner, effects);
    Ok(next)
}

/// Days a finished delivery's record is kept for diagnostics.
const RECORD_RETENTION_SECONDS: i64 = 7 * 86_400;

/// Record the delivery's state for diagnostics and restart recovery. Written
/// when its status or generations change; a newer revision is never replaced.
fn persist(app: &App, session: &Session, d: &Delivery) {
    let app = app.clone();
    let id = session.id.clone();
    let file_id = session.file_id.clone();
    let file_revision = session.revision.clone();
    let d = d.clone();
    tokio::spawn(async move {
        if let Err(error) = record(&app.db, &id, &file_id, &file_revision, &d).await {
            tracing::warn!(?error, "could not record delivery state");
        }
    });
}

/// Monotonic snapshots cannot resurrect a session fenced by restart recovery.
/// Await this at acknowledgement and eviction boundaries; intermediate snapshots
/// are diagnostic and may lag the in-memory reducer.
async fn record<'e>(
    db: impl sqlx::Executor<'e, Database = sqlx::Sqlite>,
    id: &str,
    file_id: &str,
    file_revision: &str,
    d: &Delivery,
) -> anyhow::Result<()> {
    let state = serde_json::to_string(d)?;
    sqlx::query("INSERT INTO delivery_sessions(id,file_id,file_revision,revision,status,state_json,updated_at) VALUES (?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,status=excluded.status,state_json=excluded.state_json,updated_at=excluded.updated_at WHERE excluded.revision > delivery_sessions.revision AND delivery_sessions.status != 'interrupted'")
        .bind(id)
        .bind(file_id)
        .bind(file_revision)
        .bind(i64::try_from(d.revision)?)
        .bind(name(d.status))
        .bind(state)
        .bind(crate::now())
        .execute(db)
        .await?;
    Ok(())
}

/// A delivery this process no longer holds: its last recorded state.
async fn recorded(app: &App, id: &str) -> Result<Option<(String, String, Delivery)>, ApiError> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT file_id,file_revision,state_json FROM delivery_sessions WHERE id=?")
            .bind(id)
            .fetch_optional(&app.db)
            .await?;
    row.map(|(file, revision, state)| {
        serde_json::from_str(&state)
            .map(|d| (file, revision, d))
            .map_err(ApiError::internal)
    })
    .transpose()
}

/// Commands on a delivery that is no longer live: closed or interrupted (409);
/// unknown (404).
async fn not_live(app: &App, id: &str) -> ApiError {
    match recorded(app, id).await {
        Ok(Some(_)) => error(core::Error::DeliveryClosed),
        Ok(None) => ApiError::not_found(),
        Err(error) => error,
    }
}

fn dispatch(app: &App, session: &Arc<Session>, inner: &mut Inner, effects: Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::Start { generation } => {
                let stop = CancellationToken::new();
                let (pace, paced) = tokio::sync::mpsc::unbounded_channel();
                inner.workers.insert(
                    generation,
                    WorkerControl {
                        stop: stop.clone(),
                        pace,
                    },
                );
                tokio::spawn(run_generation(
                    app.clone(),
                    session.clone(),
                    generation,
                    stop,
                    paced,
                ));
            }
            Effect::Stop { generation } => {
                if let Some(worker) = inner.workers.get(&generation) {
                    worker.stop.cancel();
                }
            }
            Effect::Pace { generation, paused } => {
                if let Some(worker) = inner.workers.get(&generation) {
                    let _ = worker.pace.send(paused);
                }
            }
            Effect::Discard {
                generation,
                below_index,
            } => {
                let directory = session.directory(app, generation);
                tokio::task::spawn_blocking(move || discard(&directory, below_index));
            }
            Effect::Cleanup { generation } => {
                inner.workers.remove(&generation);
                let directory = session.directory(app, generation);
                tokio::task::spawn_blocking(move || {
                    if directory.exists()
                        && let Err(error) = std::fs::remove_dir_all(&directory)
                    {
                        tracing::warn!(%error, path=%directory.display(), "delivery cleanup failed");
                    }
                });
            }
        }
    }
}

fn discard(directory: &FsPath, below: u32) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(index) = name
            .to_str()
            .and_then(|n| n.strip_prefix("seg"))
            .and_then(|n| n.strip_suffix(".m4s"))
            .and_then(|n| n.parse::<u32>().ok())
            && index < below
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// One entry of FFmpeg's own (atomically replaced) playlist.
#[derive(Debug, PartialEq)]
struct Published {
    index: u32,
    duration_ms: u64,
}

/// Parse FFmpeg's playlist. Only `segN.m4s` entries are accepted.
fn parse_playlist(text: &str) -> (Vec<Published>, bool) {
    let mut out = Vec::new();
    let mut duration = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            duration = value
                .split(',')
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .map(|v| (v * 1000.0).round() as u64);
        } else if !line.starts_with('#')
            && !line.is_empty()
            && let (Some(duration_ms), Some(index)) = (
                duration.take(),
                line.strip_prefix("seg")
                    .and_then(|n| n.strip_suffix(".m4s"))
                    .and_then(|n| n.parse().ok()),
            )
        {
            out.push(Published { index, duration_ms });
        }
    }
    (out, text.lines().any(|l| l == "#EXT-X-ENDLIST"))
}

/// FFmpeg's own playlist only needs to cover what one observation can miss.
const FFMPEG_LIST_SIZE: &str = "30";

fn arguments(
    operation: Operation,
    start_ms: u64,
    audio: Option<u32>,
    directory: &FsPath,
) -> Vec<String> {
    let copy = copies_video(operation);
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-protocol_whitelist",
        "file",
        "-ss",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("{}.{:03}", start_ms / 1000, start_ms % 1000));
    if copy {
        // Keep source timestamps: copied video starts at the preceding keyframe,
        // and its media time is timeline time (the route requires a zero start).
        args.push("-copyts".into());
    }
    args.extend(
        [
            // The validated source descriptor inherited as fd 3.
            "-i",
            "/dev/fd/3",
        ]
        .map(str::to_owned),
    );
    args.extend(["-map", "0:v:0"].map(str::to_owned));
    if let Some(audio) = audio {
        args.push("-map".into());
        args.push(format!("0:a:{audio}"));
    }
    args.extend(["-sn", "-dn", "-map_metadata", "-1"].map(str::to_owned));
    if copy {
        args.extend(["-c:v", "copy"].map(str::to_owned));
    } else {
        args.extend(
            [
                "-vf",
                "scale=w='min(1280,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "23",
                "-threads",
                "2",
                "-force_key_frames",
            ]
            .map(str::to_owned),
        );
        args.push(format!("expr:gte(t,n_forced*{SEGMENT_SECONDS})"));
    }
    if operation == Operation::Remux {
        args.extend(["-c:a", "copy"].map(str::to_owned));
    } else {
        args.extend(["-c:a", "aac", "-b:a", "160k", "-ac", "2"].map(str::to_owned));
    }
    args.extend(
        [
            "-f",
            "hls",
            "-hls_list_size",
            FFMPEG_LIST_SIZE,
            "-hls_segment_type",
            "fmp4",
            "-hls_fmp4_init_filename",
            "init.mp4",
            "-hls_flags",
            "temp_file+independent_segments",
            "-start_number",
            "0",
            "-hls_time",
        ]
        .map(str::to_owned),
    );
    args.push(SEGMENT_SECONDS.to_string());
    args.push("-hls_segment_filename".into());
    args.push(directory.join("seg%d.m4s").to_string_lossy().into_owned());
    args.push(directory.join("ffmpeg.m3u8").to_string_lossy().into_owned());
    args
}

/// Video start time of segment 0, in milliseconds. The init fragment and
/// segment are piped to FFprobe, so no path is interpreted by it.
async fn segment_zero_start(app: &App, directory: &FsPath) -> Option<u64> {
    use tokio::io::AsyncWriteExt;
    let mut bytes = tokio::fs::read(directory.join("init.mp4")).await.ok()?;
    bytes.extend(tokio::fs::read(directory.join("seg0.m4s")).await.ok()?);
    let mut probe = Command::new(app.ffprobe.as_ref())
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "pipe",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=start_time",
            "-of",
            "csv=p=0",
            "-i",
            "pipe:0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdin = probe.stdin.take()?;
    let writer = tokio::spawn(async move {
        // FFprobe may stop reading once it has what it needs.
        let _ = stdin.write_all(&bytes).await;
    });
    let output = probe.wait_with_output().await.ok()?;
    let _ = writer.await;
    let seconds: f64 = String::from_utf8(output.stdout).ok()?.trim().parse().ok()?;
    (output.status.success() && seconds.is_finite() && seconds >= 0.0)
        .then(|| (seconds * 1000.0).round() as u64)
}

/// Report newly published segments (and completion) in order.
async fn observe(
    app: &App,
    session: &Arc<Session>,
    generation: u64,
    directory: &FsPath,
    reported: &mut u32,
    start_ms: u64,
    operation: Operation,
) {
    // Stop writing before the volume drops below the configured free-space floor.
    let floor = app.storage.settings.min_free_bytes;
    let space = {
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || fs2::available_space(directory)).await
    };
    if !matches!(space, Ok(Ok(free)) if free >= floor) {
        tracing::warn!(delivery=%session.id, generation, "live output below the free-space floor");
        let _ = apply(app, session, Input::Failed { generation });
        return;
    }
    let Ok(text) = tokio::fs::read_to_string(directory.join("ffmpeg.m3u8")).await else {
        return;
    };
    let (published, ended) = parse_playlist(&text);
    // FFmpeg's bounded playlist dropped segments that were never reported: the
    // generation's output (and its pacing) can no longer be accounted for.
    if published.first().is_some_and(|s| s.index > *reported) {
        tracing::warn!(delivery=%session.id, generation, "segment publication gap");
        let _ = apply(app, session, Input::Failed { generation });
        return;
    }
    let from = *reported;
    for segment in published.iter().filter(|s| s.index >= from) {
        if segment.index != *reported {
            return;
        }
        let Ok(metadata) =
            tokio::fs::metadata(directory.join(format!("seg{}.m4s", segment.index))).await
        else {
            return;
        };
        if metadata.len() > MAX_SEGMENT_BYTES {
            tracing::warn!(delivery=%session.id, generation, bytes = metadata.len(), "segment exceeds the size cap");
            let _ = apply(app, session, Input::Failed { generation });
            return;
        }
        let input = if segment.index == 0 {
            if copies_video(operation) {
                // Copied output keeps source time (timeline time): segment 0 begins
                // at the keyframe FFmpeg seeked to, measured from the output itself.
                let Some(begin) = segment_zero_start(app, directory).await else {
                    tracing::warn!(delivery=%session.id, generation, "segment 0 start not measurable");
                    let _ = apply(app, session, Input::Failed { generation });
                    return;
                };
                Input::Ready {
                    generation,
                    media_time_origin_ms: 0,
                    first_segment_start_ms: begin,
                    first_segment_ms: segment.duration_ms,
                    target_duration_s: target_seconds(operation),
                }
            } else {
                // Input seeking before decoding starts output at the requested time.
                Input::Ready {
                    generation,
                    media_time_origin_ms: start_ms,
                    first_segment_start_ms: 0,
                    first_segment_ms: segment.duration_ms,
                    target_duration_s: target_seconds(operation),
                }
            }
        } else {
            Input::Segment {
                generation,
                index: segment.index,
                duration_ms: segment.duration_ms,
                now_ms: app.processing.deliveries.now_ms(),
            }
        };
        let _ = apply(app, session, input);
        *reported += 1;
    }
    if ended && *reported > 0 && published.last().is_some_and(|s| s.index + 1 == *reported) {
        let _ = apply(
            app,
            session,
            Input::Completed {
                generation,
                final_index: *reported - 1,
            },
        );
    }
}

/// Open the source beneath its validated root and check its identity. The
/// encoder reads this very descriptor, so a later path replacement cannot
/// substitute other bytes or escape the library root.
fn source_valid(source: &Source) -> anyhow::Result<std::fs::File> {
    anyhow::ensure!(
        db::root_identity(&std::fs::metadata(&source.root)?) == source.root_identity,
        "source root changed"
    );
    let file = scan::open_file(&source.root, &source.relative)?;
    anyhow::ensure!(
        scan::fingerprint(&file.metadata()?) == source.fingerprint,
        "source changed"
    );
    Ok(file)
}

/// Bound on one playlist observation; a stalled filesystem read must not block
/// cancellation or termination accounting.
const OBSERVE_DEADLINE: Duration = Duration::from_secs(5);

async fn run_generation(
    app: App,
    session: Arc<Session>,
    generation: u64,
    stop: CancellationToken,
    mut paced: tokio::sync::mpsc::UnboundedReceiver<bool>,
) {
    let stopped = move |app: &App, session: &Arc<Session>| {
        let _ = apply(app, session, Input::Stopped { generation });
    };
    let fail = |app: &App, session: &Arc<Session>| {
        let _ = apply(app, session, Input::Failed { generation });
        stopped(app, session);
    };
    let lease = tokio::select! {
        _ = stop.cancelled() => return stopped(&app, &session),
        lease = app.processing.execution.reserve(
            format!("delivery:{}:{generation}", session.id),
            Class::Interactive,
            1,
        ) => lease,
    };
    let Ok(lease) = lease else {
        return fail(&app, &session);
    };
    let lease = Arc::new(lease);
    let (pin, start_ms) = {
        let inner = session.inner.lock().unwrap();
        match inner.delivery.generations.get(&generation) {
            Some(g) => (g.pin.clone(), g.requested_start_ms),
            None => return,
        }
    };
    let operation = pin.operation;
    let directory = session.directory(&app, generation);
    let audio = pin
        .tracks
        .iter()
        .find_map(|t| t.strip_prefix("audio:").and_then(|i| i.parse().ok()));
    // Cancellation/timeout fences preparation, but a stalled filesystem operation
    // still owns capacity and prevents generation cleanup until it actually exits.
    let prepared = {
        let directory = directory.clone();
        let source = session.source.clone();
        let floor = app.storage.settings.min_free_bytes;
        let late_app = app.clone();
        let late_session = session.clone();
        let prepared = execution::prepare(
            lease.clone(),
            &stop,
            OBSERVE_DEADLINE,
            move || {
                std::fs::create_dir_all(&directory)?;
                crate::storage::require_space(&directory, GENERATION_RESERVE_BYTES, floor)?;
                let input = source_valid(&source)?;
                let (witness, held) = execution::Witness::create(&directory.join(".owner"))?;
                anyhow::Ok((input, witness, held))
            },
            move || {
                stopped(&late_app, &late_session);
            },
        )
        .await;
        let Some(prepared) = prepared else {
            tracing::warn!(delivery=%session.id, generation, "delivery preparation fenced; awaiting actual filesystem exit");
            let _ = apply(&app, &session, Input::Failed { generation });
            return;
        };
        prepared
    };
    let (input, witness, held) = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => {
            tracing::warn!(delivery=%session.id, generation, %error, "delivery generation not started");
            return fail(&app, &session);
        }
        Err(error) => {
            tracing::warn!(delivery=%session.id, generation, %error, "delivery preparation panicked");
            return fail(&app, &session);
        }
    };
    let mut supervisor = Command::new(&app.processing.supervisor);
    supervisor
        .arg("--internal-ffmpeg-supervisor")
        .arg(&app.processing.settings.ffmpeg)
        .args(arguments(operation, start_ms, audio, &directory))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    // fd 3: the validated source; fd 4: the execution witness.
    #[cfg(unix)]
    execution::inherit(&mut supervisor, &[&input, &held.0]);
    if stop.is_cancelled() {
        return stopped(&app, &session);
    }
    if let Err(error) = lease.started(&directory.join(".owner")) {
        tracing::warn!(%error, "delivery witness unreadable; refusing to spawn");
        return fail(&app, &session);
    }
    let child = supervisor.spawn();
    drop((input, held));
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "delivery worker spawn failed");
            // The witness was never inherited; nothing else can hold it.
            return fail(&app, &session);
        }
    };
    let mut heartbeat = child.stdin.take();
    if let Some(mut stderr) = child.stderr.take() {
        // Drain continuously; retain a bounded tail for diagnostics.
        let id = session.id.clone();
        tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buffer).await {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..n]);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 8192);
                }
            }
            if !tail.is_empty() {
                tracing::warn!(delivery=%id, generation, stderr=%String::from_utf8_lossy(&tail), "encoder diagnostics");
            }
        });
    }
    let mut reported = 0u32;
    let mut poll = tokio::time::interval(Duration::from_millis(200));
    let exit = loop {
        tokio::select! {
            _ = stop.cancelled() => break None,
            _ = lease.termination_requested().cancelled() => break None,
            status = child.wait() => break Some(status),
            Some(paused) = paced.recv() => {
                use tokio::io::AsyncWriteExt;
                if let Some(control) = heartbeat.as_mut() {
                    let byte: &[u8] = if paused { b"p" } else { b"r" };
                    if control.write_all(byte).await.is_err() {
                        break None;
                    }
                }
            }
            _ = poll.tick() => {
                let observed = tokio::select! {
                    _ = stop.cancelled() => break None,
                    observed = tokio::time::timeout(
                        OBSERVE_DEADLINE,
                        observe(&app, &session, generation, &directory, &mut reported, start_ms, operation),
                    ) => observed,
                };
                if observed.is_err() {
                    tracing::warn!(delivery=%session.id, generation, "playlist observation stalled");
                    let _ = apply(&app, &session, Input::Failed { generation });
                    break None;
                }
            }
        }
    };
    drop(heartbeat);
    let (app2, session2) = (app.clone(), session.clone());
    match exit {
        Some(status) => {
            // Natural exit: publish anything written after the last poll.
            let _ = tokio::time::timeout(
                OBSERVE_DEADLINE,
                observe(
                    &app,
                    &session,
                    generation,
                    &directory,
                    &mut reported,
                    start_ms,
                    operation,
                ),
            )
            .await;
            if !status.as_ref().is_ok_and(|s| s.success()) {
                let _ = apply(&app, &session, Input::Failed { generation });
            }
            // Stopped only once every inheritor of the witness is gone.
            execution::confirm_exit(
                &lease,
                None,
                Some(witness),
                execution::TERMINATION_DEADLINE,
                move || stopped(&app2, &session2),
            )
            .await;
        }
        None => {
            // Closing the control pipe makes the supervisor terminate the group.
            execution::confirm_exit(
                &lease,
                Some(child),
                Some(witness),
                execution::TERMINATION_DEADLINE,
                move || stopped(&app2, &session2),
            )
            .await;
        }
    }
}

/// Fail current generations whose source is no longer an available, enabled,
/// unchanged catalog file (e.g. after library detach or a rescan).
async fn revalidate(app: &App, session: &Arc<Session>) {
    let current: Result<Option<String>, _> = sqlx::query_scalar("SELECT revision FROM catalog_files WHERE id=? AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
        .bind(&session.file_id)
        .fetch_optional(&app.db)
        .await;
    if let Ok(current) = current
        && current.as_deref() != Some(session.revision.as_str())
    {
        let generations: Vec<u64> = {
            let inner = session.inner.lock().unwrap();
            [inner.delivery.pending, inner.delivery.active]
                .into_iter()
                .flatten()
                .collect()
        };
        for generation in generations {
            let _ = apply(app, session, Input::Failed { generation });
        }
    }
}

/// Tick every session; forget sessions whose workers and files are gone.
pub async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = stop.cancelled() => {
                app.processing.deliveries.stopping.store(true, std::sync::atomic::Ordering::SeqCst);
                let _admission = app.processing.deliveries.admission.lock().await;
                let sessions: Vec<_> = app.processing.deliveries.sessions.lock().unwrap().values().cloned().collect();
                for session in sessions {
                    if let Ok(state) = apply(&app, &session, Input::Close)
                        && let Err(error) = record(&app.db, &session.id, &session.file_id, &session.revision, &state).await
                    {
                        tracing::warn!(?error, "could not record delivery shutdown");
                    }
                }
                return Ok(());
            }
            _ = tick.tick() => {}
        }
        let sessions: Vec<_> = app
            .processing
            .deliveries
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for session in sessions {
            revalidate(&app, &session).await;
            let now = app.processing.deliveries.now_ms();
            let Ok(state) = apply(&app, &session, Input::Tick { now_ms: now }) else {
                continue;
            };
            if state.status.fenced() && state.generations.is_empty() {
                if let Err(error) = record(
                    &app.db,
                    &session.id,
                    &session.file_id,
                    &session.revision,
                    &state,
                )
                .await
                {
                    tracing::warn!(
                        ?error,
                        "retaining delivery until its terminal state is recorded"
                    );
                    continue;
                }
                app.processing
                    .deliveries
                    .sessions
                    .lock()
                    .unwrap()
                    .remove(&session.id);
                // Generation cleanups may still be running; retry until it is gone.
                let directory = app.processing.deliveries.root.join(&session.id);
                for _ in 0..50 {
                    match tokio::fs::remove_dir_all(&directory).await {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        _ => break,
                    }
                }
            }
        }
    }
}

/// Live output never resumes after a restart. Encoders of the previous process
/// that are still alive keep their capacity and directory until they are gone.
/// Recorded deliveries that were live become interrupted through the core
/// reducer's own Interrupt transition.
pub async fn recover(app: &App) -> anyhow::Result<()> {
    // Take SQLite's writer lock before reading snapshots, so a late old-runtime
    // write cannot advance a revision between the read and the restart fence.
    let mut transaction = app.db.begin().await?;
    sqlx::query("DELETE FROM delivery_sessions WHERE updated_at < ?")
        .bind(crate::now() - RECORD_RETENTION_SECONDS)
        .execute(&mut *transaction)
        .await?;
    let live: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,state_json FROM delivery_sessions WHERE status NOT IN ('closed','failed','interrupted')",
    )
    .fetch_all(&mut *transaction)
    .await?;
    for (id, state) in live {
        let delivery: Delivery = serde_json::from_str(&state)?;
        // Workers died with the old process; their files are handled below.
        let (interrupted, _) = core::transition(&delivery, &Input::Interrupt)
            .map_err(|e| anyhow::anyhow!("interrupt rejected: {e:?}"))?;
        sqlx::query(
            "UPDATE delivery_sessions SET revision=?,status=?,state_json=?,updated_at=? WHERE id=?",
        )
        .bind(i64::try_from(interrupted.revision)?)
        .bind(name(interrupted.status))
        .bind(serde_json::to_string(&interrupted)?)
        .bind(crate::now())
        .bind(id)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    let root = app.processing.deliveries.root.clone();
    let held = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || crate::processing::held_witnesses(&root)).await??
    };
    let mut keep = Vec::new();
    for (path, witness) in held {
        // live/<delivery>/<generation>/.owner: keep the delivery directory while
        // any of its generations survives; remove only the released generation,
        // then the delivery directory once it is empty.
        let (Some(generation), Some(delivery)) = (
            path.parent().map(FsPath::to_owned),
            path.parent().and_then(FsPath::parent).map(FsPath::to_owned),
        ) else {
            continue;
        };
        keep.push(delivery.clone());
        app.processing.execution.recovered(
            format!("recovered:{}", path.display()),
            Class::Interactive,
            1,
            witness,
            move || {
                let _ = std::fs::remove_dir_all(generation);
                let _ = std::fs::remove_dir(delivery);
            },
        );
    }
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&root)?;
        for entry in std::fs::read_dir(&root)? {
            let path = entry?.path();
            if !keep.contains(&path) {
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
        }
        anyhow::Ok(())
    })
    .await?
}

/// Distinguish an omitted field (outer `None`) from an explicit `null`.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub file_id: String,
    pub file_revision: String,
    pub start_ms: u64,
    /// Zero-based audio stream among the source's audio streams. Null omits audio.
    pub audio_track: Option<u32>,
    /// Live route; omitted means video_transcode. Copy routes require H.264
    /// starting at zero (remux: AAC or no audio; audio_convert: non-AAC audio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<LiveOperation>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeRequest {
    pub expected_generation: String,
    pub position_ms: u64,
    /// Replace the audio choice (null removes audio); omitted keeps the current
    /// selection, which makes the change a seek.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<u32>)]
    pub audio_track: Option<Option<u32>>,
    /// Replace the live route; omitted keeps the current one.
    #[serde(default)]
    pub operation: Option<LiveOperation>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ActivateRequest {
    /// The generation the client is replacing; null only before any was active.
    pub expected_active_generation: Option<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRequest {
    pub active_generation: String,
    /// Logical playhead; bounds how far the encoder may run ahead.
    #[serde(default)]
    pub position_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct GenerationView {
    pub generation: String,
    /// starting, ready, active, retiring, retired, or failed.
    pub status: String,
    pub manifest_url: Option<String>,
    pub media_time_origin_ms: u64,
    pub requested_start_ms: u64,
    pub available_start_ms: u64,
    pub available_end_ms: u64,
    pub transport: String,
    pub operation: String,
    pub complete: bool,
    /// The encoder is paused because its output is far enough ahead of the playhead.
    pub paused: bool,
    pub audio_track: Option<u32>,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct DeliveryView {
    pub id: String,
    pub revision: String,
    /// none, overlap, or disruptive.
    pub replacement_mode: String,
    pub file_id: String,
    pub file_revision: String,
    /// starting, ready, transitioning, closing, closed, failed, or interrupted.
    pub status: String,
    pub active: Option<GenerationView>,
    pub pending: Option<GenerationView>,
    pub lease_expires_in_ms: u64,
    pub heartbeat_interval_seconds: u64,
    pub logical_duration_ms: Option<u64>,
}

fn name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn view(app: &App, session: &Session, d: &Delivery) -> DeliveryView {
    view_of(app, &session.id, &session.file_id, &session.revision, d)
}

fn view_of(app: &App, id: &str, file_id: &str, file_revision: &str, d: &Delivery) -> DeliveryView {
    let generation = |n: Option<u64>| {
        n.and_then(|n| d.generations.get(&n).map(|g| (n, g)))
            .map(|(n, g)| GenerationView {
                generation: n.to_string(),
                status: name(g.status),
                manifest_url: d
                    .serves(n, app.processing.deliveries.now_ms())
                    .then(|| format!("/api/v1/streams/{id}/{n}/index.m3u8")),
                media_time_origin_ms: g.media_time_origin_ms,
                requested_start_ms: g.requested_start_ms,
                available_start_ms: g.available_start_ms,
                available_end_ms: g.available_end_ms,
                transport: "hls".into(),
                operation: name(g.pin.operation),
                complete: g.complete,
                paused: g.paused,
                audio_track: g
                    .pin
                    .tracks
                    .iter()
                    .find_map(|t| t.strip_prefix("audio:").and_then(|i| i.parse().ok())),
            })
    };
    DeliveryView {
        id: id.to_owned(),
        revision: d.revision.to_string(),
        replacement_mode: name(d.replacement),
        file_id: file_id.to_owned(),
        file_revision: file_revision.to_owned(),
        status: name(d.status),
        active: generation(d.active),
        pending: generation(d.pending),
        // A recorded delivery's lease belonged to another process's clock.
        lease_expires_in_ms: if d.status.fenced() {
            0
        } else {
            d.lease_expires_ms
                .saturating_sub(app.processing.deliveries.now_ms())
        },
        heartbeat_interval_seconds: HEARTBEAT_SECONDS,
        logical_duration_ms: d.duration_ms,
    }
}

fn error(e: core::Error) -> ApiError {
    let (status, code, message) = match e {
        core::Error::DeliveryClosed => (
            StatusCode::CONFLICT,
            "delivery_closed",
            "Delivery is closed or its lease expired",
        ),
        core::Error::GenerationConflict => (
            StatusCode::CONFLICT,
            "generation_conflict",
            "Expected generation is not current",
        ),
        core::Error::GenerationNotReady => (
            StatusCode::CONFLICT,
            "generation_not_ready",
            "Generation is not ready for activation",
        ),
        core::Error::TimelineChanged => (
            StatusCode::CONFLICT,
            "timeline_changed",
            "A delivery cannot change timeline",
        ),
        core::Error::InvalidPosition => (
            StatusCode::BAD_REQUEST,
            "invalid_position",
            "Position is outside the timeline",
        ),
        core::Error::GenerationLimit => (
            StatusCode::CONFLICT,
            "generation_limit",
            "Create a new delivery",
        ),
    };
    ApiError::new(status, code, message)
}

fn generation_number(value: &str) -> Result<u64, ApiError> {
    value
        .parse()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad("Generation must be a positive decimal string"))
}

fn tracks(audio: Option<u32>) -> Vec<String> {
    // Explicit choices only: "audio:none" records an intentional absence.
    let mut tracks = vec!["video:0".to_owned()];
    tracks.push(audio.map_or_else(|| "audio:none".into(), |a| format!("audio:{a}")));
    tracks
}

fn pin(file: &db::ItemRow, audio: Option<u32>, operation: Operation) -> Pin {
    let recipe = match operation {
        Operation::Remux => REMUX_RECIPE,
        Operation::AudioConvert => AUDIO_RECIPE,
        _ => RECIPE,
    };
    Pin {
        source_file: file.id.clone(),
        source_revision: file.revision.clone(),
        tracks: tracks(audio),
        operation,
        recipe_digest: Some(recipe.into()),
    }
}

fn pinned_audio(pin: &Pin) -> Option<u32> {
    pin.tracks
        .iter()
        .find_map(|t| t.strip_prefix("audio:").and_then(|i| i.parse().ok()))
}

/// Check the exact selected streams against the core's live route rule.
fn validate_route(
    file: &db::ItemRow,
    audio: Option<u32>,
    operation: Operation,
    copy_enabled: bool,
) -> Result<(), ApiError> {
    if copies_video(operation) && !copy_enabled {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "route_unqualified",
            "Live stream-copy routes are not enabled on this server",
        ));
    }
    let streams: Vec<db::Track> =
        serde_json::from_str(&file.tracks_json).map_err(ApiError::internal)?;
    let Some(video) = streams.iter().find(|t| t.kind == "video") else {
        return Err(ApiError::bad("Live conversion requires a video stream"));
    };
    let audios: Vec<&db::Track> = streams.iter().filter(|t| t.kind == "audio").collect();
    let selected = match audio {
        Some(index) => Some(
            *audios
                .get(index as usize)
                .ok_or_else(|| ApiError::bad("Unknown audio track"))?,
        ),
        None => None,
    };
    let hdr = matches!(
        video.color_transfer.as_deref(),
        Some("smpte2084" | "arib-std-b67")
    );
    let source = core::SourceStreams {
        video: Some(video.codec.as_str()),
        audio: selected.map(|t| t.codec.as_str()),
        hdr,
        // Every stream, not only the video: the earliest one is the container
        // start that input seeking is relative to.
        starts_at_zero: streams
            .iter()
            .all(|t| t.start_time_seconds.is_some_and(|s| s.abs() < 0.0005)),
        eight_bit_420: matches!(video.pixel_format.as_deref(), Some("yuv420p" | "yuvj420p")),
    };
    if core::live_route_supported(operation, &source) {
        return Ok(());
    }
    Err(if hdr {
        // No live route has a qualified tone-mapping path.
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "hdr_unsupported",
            "Live conversion of HDR video is not supported",
        )
    } else {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "route_unsupported",
            "The selected streams cannot use this live route",
        )
    })
}

#[utoipa::path(operation_id="create_delivery",post,path="/api/v1/deliveries",params(("Idempotency-Key"=Option<String>,Header,description="16–128 bytes; exact retries replay the durable acknowledgement")),request_body=CreateRequest,security(("admin_token"=[])),responses((status=201,description="Delivery admitted; its first generation starts encoding",body=DeliveryView),(status=400,description="Invalid position or track",body=crate::api::ErrorBody),(status=404,description="Unknown or unavailable file",body=crate::api::ErrorBody),(status=409,description="Source revision or idempotency conflict",body=crate::api::ErrorBody),(status=503,description="Too many deliveries",body=crate::api::ErrorBody)))]
pub async fn create(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<CreateRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<DeliveryView>), ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let key = headers
        .get("idempotency-key")
        .map(|value| {
            let key = value
                .to_str()
                .map_err(|_| ApiError::bad("Invalid Idempotency-Key"))?;
            if !(16..=128).contains(&key.len()) {
                return Err(ApiError::bad("Idempotency-Key must be 16–128 bytes"));
            }
            Ok(key.to_owned())
        })
        .transpose()?;
    admit_request(app, Arc::new(LegacyAdmin), key, r).await
}

/// Authority adapters revalidate current permission and source visibility inside
/// the same SQLite write transaction as admission/replay. This prevents a policy
/// change between authorization and receipt publication. No encoder may be started
/// by this callback. The returned principal is the durable receipt scope.
pub trait AdmissionAuthority: Send + Sync + 'static {
    fn reauthorize<'a>(
        &'a self,
        db: &'a mut sqlx::SqliteConnection,
        request: &'a CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ApiError>> + Send + 'a>>;
}
struct LegacyAdmin;
impl AdmissionAuthority for LegacyAdmin {
    fn reauthorize<'a>(
        &'a self,
        _: &'a mut sqlx::SqliteConnection,
        _: &'a CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ApiError>> + Send + 'a>>
    {
        // This adapter's admin token is immutable for the lifetime of App and
        // was checked before entering admission. V2 implements current DB policy.
        Box::pin(async { Ok("legacy-admin".into()) })
    }
}

/// Admission survives loss of its HTTP waiter. Authority is revalidated even for
/// an exact replay; durable admission and execution dispatch remain ordered.
pub async fn admit_request(
    app: App,
    authority: Arc<dyn AdmissionAuthority>,
    key: Option<String>,
    r: CreateRequest,
) -> Result<(StatusCode, Json<DeliveryView>), ApiError> {
    if key
        .as_ref()
        .is_some_and(|key| !(16..=128).contains(&key.len()))
    {
        return Err(ApiError::bad("Invalid delivery admission identity"));
    }
    let slot = app
        .processing
        .deliveries
        .admission_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admission_busy",
                "Too many pending delivery admissions",
            )
        })?;
    tokio::spawn(async move {
        let _slot = slot;
        admit_owned(&app, authority, key, r).await
    })
    .await
    .map_err(ApiError::internal)?
}

async fn admit_owned(
    app: &App,
    authority: Arc<dyn AdmissionAuthority>,
    key: Option<String>,
    r: CreateRequest,
) -> Result<(StatusCode, Json<DeliveryView>), ApiError> {
    use playscale_core::delivery_admission::{self as admission, Decision, Identity, Receipt};
    use sha2::{Digest, Sha256};
    let runtime = &app.processing.deliveries;
    let _admission = runtime.admission.lock().await;
    let mut transaction = db::begin_write(&app.db).await?;
    let principal = authority.reauthorize(&mut transaction, &r).await?;
    if principal.is_empty() || principal.len() > 256 {
        return Err(ApiError::bad("Invalid admission principal"));
    }
    let identity = key.map(|key| Identity {
        principal,
        key,
        digest: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&r).expect("serializable request"))
        ),
    });
    if let Some(identity) = &identity {
        let saved: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT principal,request_key,request_digest,delivery_id,acknowledgement_json FROM delivery_admissions WHERE principal=? AND request_key=?"
        ).bind(&identity.principal).bind(&identity.key).fetch_optional(&mut *transaction).await?;
        let receipt = saved
            .as_ref()
            .map(|(principal, key, digest, id, _)| Receipt {
                identity: Identity {
                    principal: principal.clone(),
                    key: key.clone(),
                    digest: digest.clone(),
                },
                delivery_id: id.clone(),
            });
        match admission::decide(identity, receipt.as_ref()) {
            Ok(Decision::Replay { delivery_id }) => {
                let acknowledgement: DeliveryView =
                    serde_json::from_str(&saved.unwrap().4).map_err(ApiError::internal)?;
                if acknowledgement.id != delivery_id {
                    return Err(ApiError::internal("Delivery receipt identity mismatch"));
                }
                transaction.commit().await?;
                return Ok((StatusCode::CREATED, Json(acknowledgement)));
            }
            Ok(Decision::Create) => {}
            Err(_) => {
                return Err(ApiError::conflict(
                    "idempotency_conflict",
                    "Key was already used for a different delivery request",
                ));
            }
        }
    }
    if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_stopping",
            "Delivery admission is closed",
        ));
    }

    let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
        .bind(&r.file_id)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if file.revision != r.file_revision {
        return Err(ApiError::conflict(
            "source_revision_changed",
            "Refresh the item",
        ));
    }
    let operation = r
        .operation
        .unwrap_or(LiveOperation::VideoTranscode)
        .operation();
    validate_route(
        &file,
        r.audio_track,
        operation,
        app.processing.settings.experimental_copy_routes,
    )?;
    let duration_ms = file
        .duration_seconds
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| (d * 1000.0) as u64)
        .ok_or_else(|| ApiError::bad("Live conversion requires a probed duration"))?;
    let (root, root_identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&file.library_id)
            .fetch_one(&mut *transaction)
            .await?;
    if !cfg!(unix) {
        // Encoder input is the inherited validated descriptor (Unix only).
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "platform_unsupported",
            "Live conversion is not available on this platform",
        ));
    }
    let (delivery, effects) = Delivery::admit(
        // The current catalog binds one timeline per edition.
        file.edition_id.clone(),
        pin(&file, r.audio_track, operation),
        r.start_ms,
        Some(duration_ms),
        runtime.now_ms(),
    )
    .map_err(error)?;
    let session = Arc::new(Session {
        id: new_id(),
        file_id: file.id.clone(),
        revision: file.revision.clone(),
        source: Source {
            root: root.into(),
            relative: file.relative_path.clone().into(),
            root_identity,
            fingerprint: file.fingerprint.clone(),
        },
        inner: Mutex::new(Inner {
            delivery: delivery.clone(),
            workers: HashMap::new(),
        }),
    });
    if runtime.sessions.lock().unwrap().len() >= MAX_SESSIONS {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_limit",
            "Too many live deliveries",
        ));
    }
    let acknowledgement = view(app, &session, &delivery);
    record(
        &mut *transaction,
        &session.id,
        &session.file_id,
        &session.revision,
        &delivery,
    )
    .await
    .map_err(ApiError::internal)?;
    if let Some(identity) = identity {
        sqlx::query("INSERT INTO delivery_admissions(principal,request_key,request_digest,delivery_id,acknowledgement_json,created_at) VALUES (?,?,?,?,?,?)")
            .bind(identity.principal).bind(identity.key).bind(identity.digest).bind(&session.id)
            .bind(serde_json::to_string(&acknowledgement).map_err(ApiError::internal)?)
            .bind(crate::now()).execute(&mut *transaction).await?;
    }
    transaction.commit().await?;
    // No await between durable admission and registration/dispatch. The owned
    // task survives a dropped HTTP waiter; shutdown serializes on admission.
    {
        let mut sessions = runtime.sessions.lock().unwrap();
        dispatch(app, &session, &mut session.inner.lock().unwrap(), effects);
        sessions.insert(session.id.clone(), session.clone());
    }
    Ok((StatusCode::CREATED, Json(acknowledgement)))
}

#[utoipa::path(operation_id="get_delivery",get,path="/api/v1/deliveries/{id}",params(("id"=String,Path)),responses((status=200,description="Readiness, active and pending generations",body=DeliveryView),(status=404,description="Unknown or forgotten delivery",body=crate::api::ErrorBody)))]
pub async fn get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<DeliveryView>, ApiError> {
    let Ok(session) = app.processing.deliveries.session(&id) else {
        let (file, revision, d) = recorded(&app, &id).await?.ok_or_else(ApiError::not_found)?;
        return Ok(Json(view_of(&app, &id, &file, &revision, &d)));
    };
    let d = session.inner.lock().unwrap().delivery.clone();
    Ok(Json(view(&app, &session, &d)))
}

#[utoipa::path(operation_id="close_delivery",delete,path="/api/v1/deliveries/{id}",params(("id"=String,Path)),security(("admin_token"=[])),responses((status=204,description="Retirement acknowledged; capacity is released after confirmed worker termination"),(status=404,description="Unknown delivery",body=crate::api::ErrorBody)))]
pub async fn close(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    admin(&app, &headers)?;
    let Ok(session) = app.processing.deliveries.session(&id) else {
        // Closing is idempotent: a recorded (closed or interrupted) delivery is done.
        return match recorded(&app, &id).await? {
            Some(_) => Ok(StatusCode::NO_CONTENT),
            None => Err(ApiError::not_found()),
        };
    };
    let state = apply(&app, &session, Input::Close).map_err(error)?;
    record(
        &app.db,
        &session.id,
        &session.file_id,
        &session.revision,
        &state,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(operation_id="heartbeat_delivery",post,path="/api/v1/deliveries/{id}/heartbeat",params(("id"=String,Path)),request_body=HeartbeatRequest,security(("admin_token"=[])),responses((status=200,description="Lease renewed",body=DeliveryView),(status=409,description="Closed, expired, or stale generation",body=crate::api::ErrorBody)))]
pub async fn heartbeat(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<HeartbeatRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<DeliveryView>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let Ok(session) = app.processing.deliveries.session(&id) else {
        return Err(not_live(&app, &id).await);
    };
    let d = apply(
        &app,
        &session,
        Input::Heartbeat {
            active_generation: generation_number(&r.active_generation)?,
            position_ms: r.position_ms,
            now_ms: app.processing.deliveries.now_ms(),
        },
    )
    .map_err(error)?;
    Ok(Json(view(&app, &session, &d)))
}

#[utoipa::path(operation_id="change_delivery",post,path="/api/v1/deliveries/{id}/changes",params(("id"=String,Path)),request_body=ChangeRequest,security(("admin_token"=[])),responses((status=202,description="Pending generation staged; overlap or disruptive replacement is reported",body=DeliveryView),(status=400,description="Invalid position or track",body=crate::api::ErrorBody),(status=409,description="Stale expected generation or closed delivery",body=crate::api::ErrorBody)))]
pub async fn change(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ChangeRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<DeliveryView>), ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let Ok(session) = app.processing.deliveries.session(&id) else {
        return Err(not_live(&app, &id).await);
    };
    let replan = match (r.audio_track, r.operation) {
        (None, None) => None,
        (audio, operation) => {
            // Unchanged parts of the newest selection carry over.
            let current = {
                let inner = session.inner.lock().unwrap();
                let d = &inner.delivery;
                d.pending
                    .or(d.active)
                    .and_then(|n| d.generations.get(&n))
                    .map(|g| g.pin.clone())
            };
            let current = current.ok_or_else(|| error(core::Error::GenerationConflict))?;
            let audio = audio.unwrap_or_else(|| pinned_audio(&current));
            let operation = operation.map_or(current.operation, LiveOperation::operation);
            let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
                .bind(&session.file_id)
                .fetch_optional(&app.db)
                .await?
                .ok_or_else(ApiError::not_found)?;
            if file.revision != session.revision {
                return Err(ApiError::conflict(
                    "source_revision_changed",
                    "Create a new delivery",
                ));
            }
            validate_route(
                &file,
                audio,
                operation,
                app.processing.settings.experimental_copy_routes,
            )?;
            Some((
                current,
                (file.edition_id.clone(), pin(&file, audio, operation)),
            ))
        }
    };
    // A partial change was merged with the selection read before the catalog
    // query; if another change replaced that selection meanwhile, the merge is
    // stale and must not supersede it.
    let (basis, replan) = match replan {
        Some((basis, replan)) => (Some(basis), Some(replan)),
        None => (None, None),
    };
    let d = apply_if(
        &app,
        &session,
        |d| {
            basis.as_ref().is_none_or(|basis| {
                d.pending
                    .or(d.active)
                    .and_then(|n| d.generations.get(&n))
                    .is_some_and(|g| &g.pin == basis)
            })
        },
        Input::Change {
            expected_generation: generation_number(&r.expected_generation)?,
            position_ms: r.position_ms,
            replan,
            // Overlap only when another interactive worker is admissible now.
            overlap: app.processing.execution.fits_now(Class::Interactive, 1),
            now_ms: app.processing.deliveries.now_ms(),
        },
    )
    .map_err(error)?;
    Ok((StatusCode::ACCEPTED, Json(view(&app, &session, &d))))
}

#[utoipa::path(operation_id="activate_generation",post,path="/api/v1/deliveries/{id}/generations/{generation}/activate",params(("id"=String,Path),("generation"=String,Path)),request_body=ActivateRequest,security(("admin_token"=[])),responses((status=200,description="Generation activated; the replaced one is fenced",body=DeliveryView),(status=409,description="Stale expected generation, not ready, or closed",body=crate::api::ErrorBody)))]
pub async fn activate(
    State(app): State<App>,
    Path((id, generation)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ActivateRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<DeliveryView>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let Ok(session) = app.processing.deliveries.session(&id) else {
        return Err(not_live(&app, &id).await);
    };
    let d = apply(
        &app,
        &session,
        Input::Activate {
            generation: generation_number(&generation)?,
            expected_active: r
                .expected_active_generation
                .as_deref()
                .map(generation_number)
                .transpose()?,
            now_ms: app.processing.deliveries.now_ms(),
        },
    )
    .map_err(error)?;
    Ok(Json(view(&app, &session, &d)))
}

fn stream_headers(content_type: &'static str) -> [(header::HeaderName, &'static str); 3] {
    [
        (header::CONTENT_TYPE, content_type),
        // Generation-bound and capability-addressed: never a shared cache object.
        (header::CACHE_CONTROL, "private, no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
    ]
}

/// Admit a stream request. Cheap local checks come first; a stream permit then
/// bounds concurrent catalog queries and transfers; finally the catalog is
/// re-read, so a detached library, unavailable file or new revision fences the
/// delivery immediately and an unreadable catalog fails closed.
async fn admit_stream(
    app: &App,
    id: &str,
    local: impl Fn(&Delivery, u64) -> bool,
) -> Result<(Arc<Session>, tokio::sync::OwnedSemaphorePermit), ApiError> {
    let session = app.processing.deliveries.session(id)?;
    if !local(
        &session.inner.lock().unwrap().delivery,
        app.processing.deliveries.now_ms(),
    ) {
        return Err(ApiError::not_found());
    }
    let permit = app.streams.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "stream_limit",
            "Too many open streams",
        )
    })?;
    let current: Option<String> = sqlx::query_scalar("SELECT revision FROM catalog_files WHERE id=? AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
        .bind(&session.file_id)
        .fetch_optional(&app.db)
        .await?;
    if current.as_deref() != Some(session.revision.as_str()) {
        revalidate(app, &session).await;
        return Err(ApiError::not_found());
    }
    Ok((session, permit))
}

pub async fn playlist(
    State(app): State<App>,
    Path((id, generation)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let generation = generation_number(&generation)?;
    let serves = |d: &Delivery, now| {
        d.serves(generation, now) && d.generations[&generation].pin.operation.segmented()
    };
    let (session, _permit) = admit_stream(&app, &id, serves).await?;
    let text = {
        let inner = session.inner.lock().unwrap();
        let d = &inner.delivery;
        // State may have changed while the catalog was read.
        if !serves(d, app.processing.deliveries.now_ms()) {
            return Err(ApiError::not_found());
        }
        core::media_playlist(
            &d.generations[&generation],
            |i| format!("segments/{i}.m4s"),
            "init.mp4",
        )
    };
    Ok((stream_headers("application/vnd.apple.mpegurl"), text).into_response())
}

/// Stream an owned output file; the admission permit is held until the response
/// body completes or is dropped.
async fn file(
    permit: tokio::sync::OwnedSemaphorePermit,
    path: PathBuf,
) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let mut chunks = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(async_stream::stream! {
        let _permit = permit;
        while let Some(chunk) = std::future::poll_fn(|cx| {
            futures_core::Stream::poll_next(std::pin::Pin::new(&mut chunks), cx)
        })
        .await
        {
            yield chunk;
        }
    });
    Ok((stream_headers("video/mp4"), body).into_response())
}

pub async fn init(
    State(app): State<App>,
    Path((id, generation)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let generation = generation_number(&generation)?;
    let (session, permit) = admit_stream(&app, &id, |d, now| {
        d.serves(generation, now) && d.generations[&generation].next_segment > 0
    })
    .await?;
    file(permit, session.directory(&app, generation).join("init.mp4")).await
}

pub async fn segment(
    State(app): State<App>,
    Path((id, generation, segment)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let generation = generation_number(&generation)?;
    let index: u32 = segment
        .strip_suffix(".m4s")
        .and_then(|n| n.parse().ok())
        .ok_or_else(ApiError::not_found)?;
    let (session, permit) =
        admit_stream(&app, &id, |d, now| d.fetchable(generation, index, now)).await?;
    file(
        permit,
        session
            .directory(&app, generation)
            .join(format!("seg{index}.m4s")),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{Operation, db, validate_route};

    #[tokio::test]
    async fn copy_routes_are_refused_unless_enabled_and_eligible() {
        use axum::response::IntoResponse;
        use http_body_util::BodyExt;
        let tracks = |pixel_format: &str, start: f64| {
            serde_json::json!([
                {"index":0,"kind":"video","codec":"h264","language":null,"pixel_format":pixel_format,"start_time_seconds":0.0},
                {"index":1,"kind":"audio","codec":"aac","language":null,"start_time_seconds":start}
            ])
            .to_string()
        };
        let file = |tracks_json: String| db::ItemRow {
            id: "f".into(),
            item_id: "i".into(),
            edition_id: "e".into(),
            edition_label: "Original".into(),
            kind: "video".into(),
            library_id: "l".into(),
            relative_path: "a.mkv".into(),
            title: "A".into(),
            revision: "r".into(),
            fingerprint: "x".into(),
            bytes: 1,
            duration_seconds: Some(60.0),
            tracks_json,
            available: true,
        };
        async fn code(result: Result<(), crate::api::ApiError>) -> Option<String> {
            let response = result.err()?.into_response();
            let bytes = response.into_body().collect().await.ok()?.to_bytes();
            let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            body["code"].as_str().map(str::to_owned)
        }
        let eligible = file(tracks("yuv420p", 0.0));
        assert!(validate_route(&eligible, Some(0), Operation::Remux, true).is_ok());
        assert_eq!(
            code(validate_route(&eligible, Some(0), Operation::Remux, false))
                .await
                .as_deref(),
            Some("route_unqualified")
        );
        assert!(validate_route(&eligible, Some(0), Operation::VideoTranscode, false).is_ok());
        // Audio starting before zero moves the container start; 10-bit video
        // cannot be copied into the browser pipeline.
        for ineligible in [
            file(tracks("yuv420p", -0.5)),
            file(tracks("yuv420p10le", 0.0)),
        ] {
            assert_eq!(
                code(validate_route(&ineligible, Some(0), Operation::Remux, true))
                    .await
                    .as_deref(),
                Some("route_unsupported")
            );
        }
    }

    #[tokio::test]
    async fn recorded_state_is_monotonic_and_restart_fence_is_permanent() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::connect(&dir.path().join("record.sqlite"))
            .await
            .unwrap();
        let (initial, _) = Delivery::admit(
            "timeline".into(),
            Pin {
                source_file: "file".into(),
                source_revision: "revision".into(),
                tracks: tracks(None),
                operation: Operation::VideoTranscode,
                recipe_digest: Some(RECIPE.into()),
            },
            0,
            Some(60_000),
            0,
        )
        .unwrap();
        let (ready, _) = core::transition(
            &initial,
            &Input::Ready {
                generation: 1,
                media_time_origin_ms: 0,
                first_segment_start_ms: 0,
                first_segment_ms: 4000,
                target_duration_s: 6,
            },
        )
        .unwrap();
        record(&db, "id", "file", "revision", &ready).await.unwrap();
        record(&db, "id", "file", "revision", &initial)
            .await
            .unwrap();
        let stored: String =
            sqlx::query_scalar("SELECT state_json FROM delivery_sessions WHERE id='id'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(serde_json::from_str::<Delivery>(&stored).unwrap(), ready);
        let (interrupted, _) = core::transition(&ready, &Input::Interrupt).unwrap();
        record(&db, "id", "file", "revision", &interrupted)
            .await
            .unwrap();
        let mut late = ready;
        for now_ms in 1..5 {
            late = core::transition(
                &late,
                &Input::Heartbeat {
                    active_generation: 1,
                    position_ms: Some(now_ms),
                    now_ms,
                },
            )
            .unwrap()
            .0;
        }
        assert!(late.revision > interrupted.revision);
        record(&db, "id", "file", "revision", &late).await.unwrap();
        let stored: String =
            sqlx::query_scalar("SELECT state_json FROM delivery_sessions WHERE id='id'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<Delivery>(&stored).unwrap(),
            interrupted
        );
    }

    use super::*;
    #[test]
    fn playlist_parser_accepts_only_owned_segment_names() {
        let (segments, ended) = parse_playlist(
            "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.004000,\nseg0.m4s\n#EXTINF:3.5,\n../escape.m4s\n#EXTINF:2.0,\nseg1.m4s\n#EXT-X-ENDLIST\n",
        );
        assert_eq!(
            segments,
            [
                Published {
                    index: 0,
                    duration_ms: 4004
                },
                Published {
                    index: 1,
                    duration_ms: 2000
                }
            ]
        );
        assert!(ended);
        assert_eq!(tracks(None), ["video:0", "audio:none"]);
    }
}
