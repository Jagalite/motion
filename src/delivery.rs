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
    playback_session::{Owner, Route},
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

/// Concurrent encoding delivery sessions. Each may briefly run two encoders
/// while overlapping.
pub const MAX_SESSIONS: usize = 4;
/// Concurrent byte-route (original) deliveries. They run no encoder; the bound
/// only limits the memory and lease bookkeeping this process holds.
pub const MAX_BYTE_SESSIONS: usize = 64;
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
/// Hardware (VideoToolbox) transcode; never falls back to software encoding.
pub const VIDEOTOOLBOX_RECIPE: &str = "hls-fmp4-h264-720p-videotoolbox-aac-stereo-v1";
/// Output height bound of the transcoding recipes (`scale=...min(720,ih)`).
pub const LIVE_MAX_HEIGHT: u32 = 720;
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
    /// Concurrent subtitle conversions (each reads the whole source once).
    subtitle_slots: Arc<tokio::sync::Semaphore>,
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
    /// Acknowledged v2 changes: request identity and acknowledged view,
    /// recorded under this lock with the transition they acknowledge. They are
    /// exactly as durable as the staged generation (a restart interrupts the
    /// delivery and both). Every accepted change stages a generation, so the
    /// reducer's generation limit bounds them; none is evicted while the
    /// delivery is live.
    changes: Vec<(
        playscale_core::delivery_admission::Identity,
        serde_json::Value,
    )>,
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
    /// The v2 principal and context this delivery was admitted for; None for
    /// v1 (operator) deliveries, which v2 callers can never observe.
    owner: Option<Owner>,
    source: Source,
    inner: Mutex<Inner>,
    /// Serializes subtitle sidecar extraction for this delivery.
    subtitles: tokio::sync::Mutex<()>,
}

impl Runtime {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            started: Instant::now(),
            sessions: Mutex::new(HashMap::new()),
            admission: tokio::sync::Mutex::new(()),
            admission_slots: Arc::new(tokio::sync::Semaphore::new(MAX_ADMISSIONS)),
            subtitle_slots: Arc::new(tokio::sync::Semaphore::new(2)),
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
    /// Live (encoding, byte-route) deliveries. A delivery whose generations
    /// include a segmented one counts as encoding.
    fn counts(&self) -> (usize, usize) {
        let sessions = self.sessions.lock().unwrap();
        let encoding = sessions
            .values()
            .filter(|s| {
                s.inner
                    .lock()
                    .unwrap()
                    .delivery
                    .generations
                    .values()
                    .any(|g| g.pin.operation.segmented())
            })
            .count();
        (encoding, sessions.len() - encoding)
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
    transition_locked(app, session, &mut inner, &input)
}

/// One reducer step with the session lock held by the caller.
fn transition_locked(
    app: &App,
    session: &Arc<Session>,
    inner: &mut Inner,
    input: &Input,
) -> Result<Delivery, core::Error> {
    let (next, effects) = core::transition(&inner.delivery, input)?;
    let outline = |d: &Delivery| (d.status, d.active, d.pending, d.last_generation);
    if outline(&next) != outline(&inner.delivery) {
        persist(app, session, &next);
    }
    inner.delivery = next.clone();
    dispatch(app, session, inner, effects);
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
    hardware: bool,
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
            ]
            .map(str::to_owned),
        );
        if hardware {
            // -allow_sw 0: fail rather than silently encode in software.
            args.extend(
                [
                    "-c:v",
                    "h264_videotoolbox",
                    "-allow_sw",
                    "0",
                    "-realtime",
                    "1",
                    "-b:v",
                    "2500k",
                    "-maxrate",
                    "2500k",
                    "-bufsize",
                    "5000k",
                ]
                .map(str::to_owned),
            );
        } else {
            args.extend(
                [
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-threads", "2",
                ]
                .map(str::to_owned),
            );
        }
        args.push("-force_key_frames".into());
        args.push(format!("expr:gte(t,n_forced*{SEGMENT_SECONDS})"));
    }
    if copy {
        // Keep negative B-frame decode times rather than shifting every stream,
        // so presentation times stay equal to the source (and the timeline).
        args.extend(["-avoid_negative_ts", "disabled"].map(str::to_owned));
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

/// H.264 bitstream details of a source, observed through its validated descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyDetails {
    pub revision: String,
    pub profile: String,
    pub level: i64,
    pub progressive: bool,
    pub rotated: bool,
}
impl CopyDetails {
    fn observed(&self) -> core::VideoDetails<'_> {
        core::VideoDetails {
            profile: &self.profile,
            level: self.level,
            progressive: self.progressive,
            rotated: self.rotated,
        }
    }
}

/// Default time a live encoder has to publish its first segment (generous for a
/// first launch of the tools), and the longest it may run without publishing a
/// further one. `processing.startup_timeout_seconds` and
/// `no_progress_timeout_seconds` override them.
const LIVE_STARTUP_SECONDS: u64 = 120;
const LIVE_NO_PROGRESS_SECONDS: u64 = 30;

/// Bound on one source probe.
const PROBE_DEADLINE: Duration = Duration::from_secs(15);
/// Probe output larger than this is not trusted.
const MAX_PROBE_OUTPUT: u64 = 8 * 1024 * 1024;
/// FFmpeg's live segment length; a cut happens at the first keyframe after it.
const HLS_TIME_SECONDS: u64 = SEGMENT_SECONDS;

/// Why a probe produced no answer.
enum ProbeFailure {
    /// FFprobe could not run, failed, or timed out and was reaped.
    Unavailable,
    /// It timed out and did not exit when killed: ownership must be retained.
    Stalled(tokio::process::Child),
}

/// Run FFprobe on an already validated source descriptor (inherited as fd 3),
/// so the probed bytes are the ones the encoder will read. The descriptor's
/// offset is shared with that child, so it is rewound once the child has exited.
async fn probe_descriptor(
    app: &App,
    input: &std::fs::File,
    args: &[&str],
) -> Result<Vec<u8>, ProbeFailure> {
    let mut full = vec!["-v", "error"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["-i", "/dev/fd/3"]);
    run_descriptor(
        app.ffprobe.as_ref(),
        input,
        &full,
        MAX_PROBE_OUTPUT,
        PROBE_DEADLINE,
    )
    .await
}

/// Run a media tool reading the validated descriptor as fd 3 and collect its
/// bounded stdout. The descriptor is rewound once the child has exited; a child
/// that does not exit when killed is returned for its owner to account for.
async fn run_descriptor(
    program: &FsPath,
    input: &std::fs::File,
    args: &[&str],
    max_output: u64,
    deadline: Duration,
) -> Result<Vec<u8>, ProbeFailure> {
    use std::io::{Seek, SeekFrom};
    use tokio::io::AsyncReadExt;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    #[cfg(unix)]
    execution::inherit(&mut command, &[input]);
    let mut child = command.spawn().map_err(|_| ProbeFailure::Unavailable)?;
    let mut stdout = child.stdout.take().ok_or(ProbeFailure::Unavailable)?;
    let mut output = Vec::new();
    // Read one byte past the limit so overflow is detected, not truncated.
    let finished = tokio::time::timeout(deadline, async {
        (&mut stdout)
            .take(max_output + 1)
            .read_to_end(&mut output)
            .await?;
        child.wait().await
    })
    .await;
    let rewind = || (&*input).seek(SeekFrom::Start(0)).is_ok();
    let overflow = output.len() as u64 > max_output;
    match finished {
        Ok(Ok(status)) if status.success() && !overflow && rewind() => Ok(output),
        Ok(_) => {
            rewind();
            Err(ProbeFailure::Unavailable)
        }
        Err(_) => {
            let _ = child.start_kill();
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(_) => {
                    rewind();
                    Err(ProbeFailure::Unavailable)
                }
                Err(_) => Err(ProbeFailure::Stalled(child)),
            }
        }
    }
}

/// Observe the details stream copy eligibility needs, before any admission
/// transaction (a probe must not hold the database writer).
async fn probe_copy_details(app: &App, file_id: &str) -> Result<CopyDetails, ApiError> {
    let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
        .bind(file_id)
        .fetch_optional(&app.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let (root, root_identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&file.library_id)
            .fetch_one(&app.db)
            .await?;
    let source = Source {
        root: root.into(),
        relative: file.relative_path.clone().into(),
        root_identity,
        fingerprint: file.fingerprint.clone(),
    };
    let input = tokio::task::spawn_blocking(move || source_valid(&source))
        .await
        .map_err(ApiError::internal)?
        .map_err(|error| {
            // A missing file is absent; anything else means it changed.
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                ApiError::not_found()
            } else {
                ApiError::conflict("source_revision_changed", "Refresh the item")
            }
        })?;
    let stdout = match probe_descriptor(
        app,
        &input,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=profile,level,field_order:stream_side_data=rotation",
            "-of",
            "json",
        ],
    )
    .await
    {
        Ok(stdout) => stdout,
        Err(failure) => {
            if let ProbeFailure::Stalled(mut child) = failure {
                // Not tied to execution capacity: admission has not reserved any.
                tracing::error!(file_id, "source probe did not exit when killed");
                tokio::spawn(async move {
                    let _ = child.wait().await;
                });
            }
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "probe_unavailable",
                "The source could not be probed; try again",
            ));
        }
    };
    parse_copy_details(&stdout, file.revision).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "route_unsupported",
            "The source's video stream could not be qualified for stream copy",
        )
    })
}

fn parse_copy_details(stdout: &[u8], revision: String) -> Option<CopyDetails> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let stream = value["streams"].get(0)?;
    let rotated = stream["side_data_list"].as_array().is_some_and(|list| {
        list.iter().any(|d| {
            d["rotation"]
                .as_f64()
                .is_some_and(|r| r.rem_euclid(360.0) != 0.0)
        })
    });
    Some(CopyDetails {
        revision,
        profile: stream["profile"].as_str()?.to_owned(),
        level: stream["level"].as_i64()?,
        // Unknown field order is not assumed progressive.
        progressive: stream["field_order"].as_str() == Some("progressive"),
        rotated,
    })
}

/// The source keyframe a stream copy of `start_ms` begins at, in microseconds.
/// Keyframes are the demuxer's sync packets, which is where the HLS muxer cuts;
/// the core decides from them whether every segment in the observed window fits
/// the copy target. A stalled probe stays owned by `lease`.
async fn copy_keyframe(
    app: &App,
    input: &std::fs::File,
    start_ms: u64,
    duration_ms: Option<u64>,
    lease: &execution::Lease,
) -> Option<u64> {
    let from = start_ms.saturating_sub(COPY_TARGET_SECONDS * 1000);
    let mut until = start_ms + 60_000 + COPY_TARGET_SECONDS * 1000;
    // Reaching the end of the media also bounds the final segment.
    let eof = duration_ms.is_some_and(|d| d <= until);
    if let Some(d) = duration_ms.filter(|_| eof) {
        until = d;
    }
    let interval = format!(
        "{}.{:03}%{}.{:03}",
        from / 1000,
        from % 1000,
        until / 1000,
        until % 1000
    );
    let stdout = match probe_descriptor(
        app,
        input,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,flags",
            "-of",
            "csv=p=0",
            "-read_intervals",
            &interval,
        ],
    )
    .await
    {
        Ok(stdout) => stdout,
        Err(ProbeFailure::Stalled(mut child)) => {
            lease.stuck(async move {
                let _ = child.wait().await;
            });
            return None;
        }
        Err(ProbeFailure::Unavailable) => return None,
    };
    let mut keyframes_us = Vec::new();
    let mut last_us = 0;
    for line in String::from_utf8(stdout).ok()?.lines() {
        let (pts, flags) = line.split_once(',')?;
        // A packet without a timestamp leaves coverage unknown.
        let pts: f64 = pts
            .trim()
            .parse()
            .ok()
            .filter(|t: &f64| t.is_finite() && *t >= 0.0)?;
        let us = (pts * 1_000_000.0).round() as u64;
        last_us = last_us.max(us);
        if flags.contains('K') {
            keyframes_us.push(us);
        }
    }
    keyframes_us.sort_unstable();
    keyframes_us.dedup();
    let keyframes_ms: Vec<u64> = keyframes_us.iter().map(|us| us / 1000).collect();
    // Coverage reaches the requested end, or the last packet at end of media.
    let end_ms = if eof { duration_ms? } else { until };
    if !eof && last_us / 1000 + COPY_TARGET_SECONDS * 1000 < until {
        return None;
    }
    let start = core::copy_start(
        &keyframes_ms,
        start_ms,
        HLS_TIME_SECONDS,
        COPY_TARGET_SECONDS,
        end_ms,
    )?;
    keyframes_us.into_iter().find(|us| us / 1000 == start)
}

/// Largest init segment read for its video track.
const MAX_INIT_BYTES: u64 = 1024 * 1024;

/// Whether a published segment's first video sample is an IDR access unit.
/// Unreadable or unexpected output is not independent. The video track is read
/// from the generation's init segment once.
async fn segment_independent(
    directory: &FsPath,
    index: u32,
    video: &mut Option<crate::fmp4::VideoTrack>,
) -> bool {
    let directory = directory.to_owned();
    let known = *video;
    let checked = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let track = match known {
            Some(track) => track,
            None => {
                let mut init = Vec::new();
                std::fs::File::open(directory.join("init.mp4"))
                    .ok()?
                    .take(MAX_INIT_BYTES + 1)
                    .read_to_end(&mut init)
                    .ok()?;
                if init.len() as u64 > MAX_INIT_BYTES {
                    return None;
                }
                crate::fmp4::video_track(&init)?
            }
        };
        let mut segment = std::fs::File::open(directory.join(format!("seg{index}.m4s"))).ok()?;
        Some((track, crate::fmp4::starts_with_idr(track, &mut segment)?))
    })
    .await
    .ok()
    .flatten();
    match checked {
        Some((track, independent)) => {
            *video = Some(track);
            independent
        }
        None => false,
    }
}

/// Video start time of segment 0, in microseconds. The init fragment and
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
        .then(|| (seconds * 1_000_000.0).round() as u64)
}

/// Report newly published segments (and completion) in order. True once the
/// generation's completion has been reported.
async fn observe(
    app: &App,
    session: &Arc<Session>,
    generation: u64,
    directory: &FsPath,
    reported: &mut u32,
    video: &mut Option<crate::fmp4::VideoTrack>,
    start_ms: u64,
    operation: Operation,
    copy_start: Option<u64>,
) -> bool {
    // Stop writing before the volume drops below the configured free-space floor.
    let floor = app.storage.settings.min_free_bytes;
    let space = {
        let directory = directory.to_owned();
        tokio::task::spawn_blocking(move || fs2::available_space(directory)).await
    };
    if !matches!(space, Ok(Ok(free)) if free >= floor) {
        tracing::warn!(delivery=%session.id, generation, "live output below the free-space floor");
        let _ = apply(app, session, Input::Failed { generation });
        return false;
    }
    let Ok(text) = tokio::fs::read_to_string(directory.join("ffmpeg.m3u8")).await else {
        return false;
    };
    let (published, ended) = parse_playlist(&text);
    // FFmpeg's bounded playlist dropped segments that were never reported: the
    // generation's output (and its pacing) can no longer be accounted for.
    if published.first().is_some_and(|s| s.index > *reported) {
        tracing::warn!(delivery=%session.id, generation, "segment publication gap");
        let _ = apply(app, session, Input::Failed { generation });
        return false;
    }
    let from = *reported;
    for segment in published.iter().filter(|s| s.index >= from) {
        if segment.index != *reported {
            return false;
        }
        let Ok(metadata) =
            tokio::fs::metadata(directory.join(format!("seg{}.m4s", segment.index))).await
        else {
            return false;
        };
        if metadata.len() > MAX_SEGMENT_BYTES {
            tracing::warn!(delivery=%session.id, generation, bytes = metadata.len(), "segment exceeds the size cap");
            let _ = apply(app, session, Input::Failed { generation });
            return false;
        }
        // The playlist declares independent segments: each must begin with an
        // IDR. A copied source's keyframe may be an open-GOP recovery point.
        if !segment_independent(directory, segment.index, video).await {
            tracing::warn!(delivery=%session.id, generation, index = segment.index, "segment does not begin with an IDR");
            let _ = apply(app, session, Input::Failed { generation });
            return false;
        }
        let input = if segment.index == 0 {
            if copies_video(operation) {
                // Copied output keeps source time (timeline time): segment 0 begins
                // at the keyframe FFmpeg seeked to, measured from the output itself.
                // Media time equals timeline time only if FFmpeg kept the source
                // timestamps: the measured start must be the probed keyframe.
                // Both times are FFprobe's microsecond values for the same packet.
                let begin = segment_zero_start(app, directory).await;
                let Some(begin) = begin
                    .filter(|b| copy_start.is_some_and(|expected| b.abs_diff(expected) <= 1))
                    .map(|us| us / 1000)
                else {
                    tracing::warn!(delivery=%session.id, generation, ?begin, ?copy_start, "segment 0 does not start at the source keyframe");
                    let _ = apply(app, session, Input::Failed { generation });
                    return false;
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
        return true;
    }
    false
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
    let (pin, start_ms, duration_ms) = {
        let inner = session.inner.lock().unwrap();
        match inner.delivery.generations.get(&generation) {
            Some(g) => (
                g.pin.clone(),
                g.requested_start_ms,
                inner.delivery.duration_ms,
            ),
            None => return,
        }
    };
    let operation = pin.operation;
    // A hardware encode also needs a hardware session, held exactly as long as
    // the CPU reservation: released only after the encoder's exit is confirmed.
    let hardware_lease = if pin.recipe_digest.as_deref() == Some(VIDEOTOOLBOX_RECIPE) {
        let reserved = tokio::select! {
            _ = stop.cancelled() => return stopped(&app, &session),
            reserved = app.processing.hardware.reserve(
                format!("delivery-hw:{}:{generation}", session.id),
                Class::Interactive,
                1,
            ) => reserved,
        };
        match reserved {
            Ok(lease) => Some(lease),
            Err(_) => return fail(&app, &session),
        }
    } else {
        None
    };
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
    // A stream copy begins at the source keyframe at or before the request; know
    // which one before encoding, so the published start can be verified.
    let copy_start = if copies_video(operation) {
        // Bounded by its own deadline; not abandoned on stop, which would leave
        // the probe unowned.
        let probed = copy_keyframe(&app, &input, start_ms, duration_ms, &lease).await;
        if stop.is_cancelled() {
            return stopped(&app, &session);
        }
        let Some(keyframe) = probed else {
            tracing::warn!(delivery=%session.id, generation, "no usable keyframe or keyframe spacing exceeds the copy target");
            return fail(&app, &session);
        };
        Some(keyframe)
    } else {
        None
    };
    let mut supervisor = Command::new(&app.processing.supervisor);
    supervisor
        .arg("--internal-ffmpeg-supervisor")
        .arg(&app.processing.settings.ffmpeg)
        .args(arguments(
            operation,
            pin.recipe_digest.as_deref() == Some(VIDEOTOOLBOX_RECIPE),
            start_ms,
            audio,
            &directory,
        ))
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
    if let Err(error) = lease.started(&directory.join(".owner")).and_then(|()| {
        hardware_lease.as_ref().map_or(Ok(()), |hardware| {
            hardware.started(&directory.join(".owner"))
        })
    }) {
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
    let mut video = None;
    let mut poll = tokio::time::interval(Duration::from_millis(200));
    // Liveness, decided by the core deadline policy. Its clock advances only
    // while the encoder is allowed to run: a paced (paused) encoder is not stalled.
    let settings = &app.processing.settings;
    let mut liveness = playscale_core::execution_deadline::Deadline::new(
        u64::MAX,
        Some(
            settings
                .startup_timeout_seconds
                .unwrap_or(LIVE_STARTUP_SECONDS)
                * 1000,
        ),
        Some(
            settings
                .no_progress_timeout_seconds
                .unwrap_or(LIVE_NO_PROGRESS_SECONDS)
                * 1000,
        ),
    );
    let mut running_ms = 0u64;
    let mut paused_now = false;
    let mut last_tick = tokio::time::Instant::now();
    let mut completed = false;
    let exit = loop {
        tokio::select! {
            _ = stop.cancelled() => break None,
            _ = lease.termination_requested().cancelled() => break None,
            status = child.wait() => break Some(status),
            Some(paused) = paced.recv() => {
                use tokio::io::AsyncWriteExt;
                let now = tokio::time::Instant::now();
                if !paused_now {
                    running_ms += now.duration_since(last_tick).as_millis() as u64;
                }
                last_tick = now;
                paused_now = paused;
                if let Some(control) = heartbeat.as_mut() {
                    let byte: &[u8] = if paused { b"p" } else { b"r" };
                    if control.write_all(byte).await.is_err() {
                        break None;
                    }
                }
            }
            _ = poll.tick() => {
                // Progress found by this pass is dated to its start: a pass slowed
                // by the filesystem must not push valid output past the deadline.
                let now = tokio::time::Instant::now();
                if !paused_now {
                    running_ms += now.duration_since(last_tick).as_millis() as u64;
                }
                last_tick = now;
                let observed = tokio::select! {
                    _ = stop.cancelled() => break None,
                    observed = tokio::time::timeout(
                        OBSERVE_DEADLINE,
                        observe(
                            &app,
                            &session,
                            generation,
                            &directory,
                            &mut reported,
                            &mut video,
                            start_ms,
                            operation,
                            copy_start,
                        ),
                    ) => observed,
                };
                let Ok(ended) = observed else {
                    tracing::warn!(delivery=%session.id, generation, "playlist observation stalled");
                    let _ = apply(&app, &session, Input::Failed { generation });
                    break None;
                };
                // A completed generation has nothing left to publish; its exit
                // decides the outcome.
                completed |= ended;
                if completed {
                    continue;
                }
                // Published segments are the progress measure.
                liveness.observe(running_ms, u64::from(reported));
                if let Some(expired) = liveness.expired(running_ms) {
                    // An encoder that already exited is judged by its exit instead.
                    if let Ok(Some(status)) = child.try_wait() {
                        break Some(Ok(status));
                    }
                    tracing::warn!(delivery=%session.id, generation, ?expired, "live encoder made no progress");
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
                    &mut video,
                    start_ms,
                    operation,
                    copy_start,
                ),
            )
            .await;
            if !status.as_ref().is_ok_and(|s| s.success()) {
                let _ = apply(&app, &session, Input::Failed { generation });
            }
            // Stopped only once every inheritor of the witness is gone.
            let confirmed = execution::confirm_exit(
                &lease,
                None,
                Some(witness),
                execution::TERMINATION_DEADLINE,
                move || stopped(&app2, &session2),
            )
            .await;
            // The hardware session ends with the same, now confirmed, execution.
            if confirmed && let Some(hardware) = &hardware_lease {
                hardware.settled();
            }
        }
        None => {
            // Closing the control pipe makes the supervisor terminate the group.
            let confirmed = execution::confirm_exit(
                &lease,
                Some(child),
                Some(witness),
                execution::TERMINATION_DEADLINE,
                move || stopped(&app2, &session2),
            )
            .await;
            // The hardware session ends with the same, now confirmed, execution.
            if confirmed && let Some(hardware) = &hardware_lease {
                hardware.settled();
            }
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
    /// Zero-based text subtitle stream, delivered as a WebVTT sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_track: Option<u32>,
    /// Encoder for video_transcode; omitted means software. videotoolbox is
    /// macOS hardware encoding and fails rather than falling back to software.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<crate::processing::Backend>,
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
    /// Replace the transcode encoder; omitted keeps the current one.
    #[serde(default)]
    pub backend: Option<crate::processing::Backend>,
    /// Replace the text subtitle (null removes it); omitted keeps the current one.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<u32>)]
    pub subtitle_track: Option<Option<u32>>,
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
    /// software or videotoolbox for a transcode; null for stream copy.
    pub backend: Option<String>,
    pub subtitle_track: Option<u32>,
    /// WebVTT sidecar for the selected text subtitle, in timeline time.
    pub subtitles_url: Option<String>,
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
                manifest_url: (g.pin.operation.segmented()
                    && d.serves(n, app.processing.deliveries.now_ms()))
                .then(|| format!("/api/v1/streams/{id}/{n}/index.m3u8")),
                media_time_origin_ms: g.media_time_origin_ms,
                requested_start_ms: g.requested_start_ms,
                available_start_ms: g.available_start_ms,
                available_end_ms: g.available_end_ms,
                transport: if g.pin.operation.segmented() {
                    "hls".into()
                } else {
                    "http_range".into()
                },
                operation: name(g.pin.operation),
                complete: g.complete,
                paused: g.paused,
                backend: (g.pin.operation == Operation::VideoTranscode).then(|| {
                    if g.pin.recipe_digest.as_deref() == Some(VIDEOTOOLBOX_RECIPE) {
                        "videotoolbox".to_owned()
                    } else {
                        "software".to_owned()
                    }
                }),
                subtitle_track: pinned_subtitle(&g.pin),
                subtitles_url: (pinned_subtitle(&g.pin).is_some()
                    && d.serves(n, app.processing.deliveries.now_ms()))
                .then(|| format!("/api/v1/streams/{id}/{n}/subtitles.vtt")),
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

fn tracks(audio: Option<u32>, subtitle: Option<u32>) -> Vec<String> {
    // Explicit choices only: "none" records an intentional absence.
    vec![
        "video:0".to_owned(),
        audio.map_or_else(|| "audio:none".into(), |a| format!("audio:{a}")),
        subtitle.map_or_else(|| "subtitle:none".into(), |s| format!("subtitle:{s}")),
    ]
}

fn pinned_subtitle(pin: &Pin) -> Option<u32> {
    pin.tracks
        .iter()
        .find_map(|t| t.strip_prefix("subtitle:").and_then(|i| i.parse().ok()))
}

/// The selected subtitle stream must exist and be a text format (sidecar).
fn validate_subtitle(file: &db::ItemRow, subtitle: Option<u32>) -> Result<(), ApiError> {
    let Some(index) = subtitle else {
        return Ok(());
    };
    let streams: Vec<db::Track> =
        serde_json::from_str(&file.tracks_json).map_err(ApiError::internal)?;
    let codec = streams
        .iter()
        .filter(|t| t.kind == "subtitle")
        .nth(index as usize)
        .map(|t| t.codec.as_str())
        .ok_or_else(|| ApiError::bad("Unknown subtitle track"))?;
    if core::sidecar_subtitle(codec) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "subtitle_unsupported",
            "Only text subtitles are delivered (as WebVTT); bitmap subtitles are not yet supported",
        ))
    }
}

fn pin(
    file: &db::ItemRow,
    audio: Option<u32>,
    subtitle: Option<u32>,
    operation: Operation,
    hardware: bool,
) -> Pin {
    let recipe = match operation {
        Operation::Remux => REMUX_RECIPE,
        Operation::AudioConvert => AUDIO_RECIPE,
        _ if hardware => VIDEOTOOLBOX_RECIPE,
        _ => RECIPE,
    };
    Pin {
        source_file: file.id.clone(),
        source_revision: file.revision.clone(),
        tracks: tracks(audio, subtitle),
        operation,
        recipe_digest: Some(recipe.into()),
    }
}

/// Whether a live software transcode of `file` with this audio stream is
/// admissible here (the core live-route rule, a probed duration, Unix).
pub(crate) fn live_transcodable(file: &db::ItemRow, audio: Option<u32>) -> bool {
    cfg!(unix)
        && file
            .duration_seconds
            .is_some_and(|d| d.is_finite() && d > 0.0)
        && validate_route(file, audio, Operation::VideoTranscode, false, None).is_ok()
}

/// A stored representation (original or prepared rendition) served by byte
/// ranges: no recipe and no encoder. The browser presents the file's default
/// streams; the pin records that choice.
fn byte_pin(file: &db::ItemRow, audio: Option<u32>, operation: Operation) -> Pin {
    Pin {
        source_file: file.id.clone(),
        source_revision: file.revision.clone(),
        tracks: tracks(audio, None),
        operation,
        recipe_digest: None,
    }
}

/// Whether a backend can run `operation` here. Hardware encoding is a transcode
/// choice and only exists on macOS.
fn validate_backend(
    operation: Operation,
    backend: &crate::processing::Backend,
) -> Result<bool, ApiError> {
    let hardware = *backend == crate::processing::Backend::Videotoolbox;
    if hardware && (operation != Operation::VideoTranscode || !cfg!(target_os = "macos")) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "backend_unsupported",
            "Hardware encoding is only available for video_transcode on macOS",
        ));
    }
    Ok(hardware)
}

fn backend_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "backend_unavailable",
        "This server cannot open a hardware encoder session",
    )
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
    details: Option<&CopyDetails>,
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
        details: details
            .filter(|d| d.revision == file.revision)
            .map(CopyDetails::observed),
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
    /// Conditions only a new admission must meet (for example an unexpired
    /// plan). Checked after the receipt lookup, so an exact retry of an
    /// acknowledged admission replays after reauthorization alone.
    fn fresh(&self) -> Result<(), ApiError> {
        Ok(())
    }
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

/// A v2 admission: the plan's owner and route. The authority adapter rechecks
/// the plan (`playback_session::admit`) inside the admission transaction.
pub(crate) struct Planned {
    pub owner: Owner,
    pub version: String,
    pub route: Route,
}

/// Admit a planned (v2) delivery. Original routes need no encoder; transcodes
/// use the same live admission as v1. Receipts are scoped to the principal.
pub(crate) async fn admit_planned(
    app: App,
    authority: Arc<dyn AdmissionAuthority>,
    key: String,
    r: CreateRequest,
    planned: Planned,
) -> Result<DeliveryView, ApiError> {
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
        admit_owned(&app, authority, Some(key), r, Some(planned))
            .await
            .map(|(_, Json(view))| view)
    })
    .await
    .map_err(ApiError::internal)?
}

/// The v2 owner of a delivery this process holds live.
pub(crate) fn live_owner(app: &App, id: &str) -> Option<Owner> {
    app.processing
        .deliveries
        .session(id)
        .ok()
        .and_then(|s| s.owner.clone())
}

/// Current view of a live delivery.
pub(crate) fn live_view(app: &App, id: &str) -> Option<DeliveryView> {
    let session = app.processing.deliveries.session(id).ok()?;
    let d = session.inner.lock().unwrap().delivery.clone();
    Some(view(app, &session, &d))
}

pub(crate) fn heartbeat_live(
    app: &App,
    id: &str,
    active_generation: &str,
    position_ms: Option<u64>,
) -> Result<DeliveryView, ApiError> {
    let session = app.processing.deliveries.session(id)?;
    let d = apply(
        app,
        &session,
        Input::Heartbeat {
            active_generation: generation_number(active_generation)?,
            position_ms,
            now_ms: app.processing.deliveries.now_ms(),
        },
    )
    .map_err(error)?;
    Ok(view(app, &session, &d))
}

/// Activate a generation; its acknowledgement is recorded under the delivery
/// lock with the transition, like a change receipt, so an exact retry replays
/// it and the same key with another request conflicts.
pub(crate) fn activate_live(
    app: &App,
    id: &str,
    generation: &str,
    expected_active: Option<&str>,
    receipt: &ChangeReceipt,
) -> Result<(DeliveryView, bool), ApiError> {
    let session = app.processing.deliveries.session(id)?;
    let input = Input::Activate {
        generation: generation_number(generation)?,
        expected_active: expected_active.map(generation_number).transpose()?,
        now_ms: app.processing.deliveries.now_ms(),
    };
    let mut inner = session.inner.lock().unwrap();
    if let Some(view) = replayed_change(&inner, receipt)? {
        return Ok((view, true));
    }
    let d = transition_locked(app, &session, &mut inner, &input).map_err(error)?;
    let acknowledged = view(app, &session, &d);
    inner.changes.push((
        receipt.identity(),
        serde_json::to_value(&acknowledged).map_err(ApiError::internal)?,
    ));
    Ok((acknowledged, false))
}

/// Report the client's logical playhead for the active generation, which
/// bounds how far its encoder may run ahead (pacing). Observation only: a
/// stale generation or a fenced delivery is ignored.
pub(crate) fn report_playhead(app: &App, id: &str, generation: u64, position_ms: u64) {
    let Ok(session) = app.processing.deliveries.session(id) else {
        return;
    };
    let _ = apply(
        app,
        &session,
        Input::Heartbeat {
            active_generation: generation,
            position_ms: Some(position_ms),
            now_ms: app.processing.deliveries.now_ms(),
        },
    );
}

/// Close a live delivery and record the fenced state through `conn` (the
/// caller's write transaction, which holds the writer lock).
pub(crate) async fn close_live(
    app: &App,
    id: &str,
    conn: &mut sqlx::SqliteConnection,
) -> Result<(), ApiError> {
    let session = app.processing.deliveries.session(id)?;
    let state = apply(app, &session, Input::Close).map_err(error)?;
    record(
        conn,
        &session.id,
        &session.file_id,
        &session.revision,
        &state,
    )
    .await
    .map_err(ApiError::internal)
}

/// Exact streams for a planned replan. The source file stays the delivery's.
pub(crate) struct Selection {
    pub route: Route,
    pub audio: Option<u32>,
    pub subtitle: Option<u32>,
}

/// Stage a seek (`selection` None keeps the newest pin) or a planned replan.
/// The replacement pin is validated against the current catalog row and the
/// core live-route rule before the reducer stages it.
pub(crate) async fn change_live(
    app: &App,
    id: &str,
    expected_generation: &str,
    position_ms: u64,
    selection: Option<Selection>,
    receipt: &ChangeReceipt,
    conn: &mut sqlx::SqliteConnection,
) -> Result<(DeliveryView, bool), ApiError> {
    let session = app.processing.deliveries.session(id)?;
    if let Some(view) = replayed_change(&session.inner.lock().unwrap(), receipt)? {
        return Ok((view, true));
    }
    let expected_generation = generation_number(expected_generation)?;
    let (basis, replan) = match selection {
        None => (None, None),
        Some(selection) => {
            let basis = {
                let inner = session.inner.lock().unwrap();
                let d = &inner.delivery;
                d.pending
                    .or(d.active)
                    .and_then(|n| d.generations.get(&n))
                    .map(|g| (g.pin.clone(), d.timeline.clone()))
            };
            let (basis, timeline) = basis.ok_or_else(|| error(core::Error::GenerationConflict))?;
            let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)")
                .bind(&session.file_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(ApiError::not_found)?;
            if file.revision != session.revision {
                return Err(ApiError::conflict(
                    "source_revision_changed",
                    "Create a new delivery",
                ));
            }
            let pin = match selection.route {
                Route::Original => byte_pin(&file, selection.audio, Operation::Original),
                Route::Prepared => byte_pin(&file, selection.audio, Operation::Prepared),
                Route::Transcode => {
                    validate_route(
                        &file,
                        selection.audio,
                        Operation::VideoTranscode,
                        app.processing.settings.experimental_copy_routes,
                        None,
                    )?;
                    validate_subtitle(&file, selection.subtitle)?;
                    pin(
                        &file,
                        selection.audio,
                        selection.subtitle,
                        Operation::VideoTranscode,
                        false,
                    )
                }
            };
            (Some(basis), Some((timeline, pin)))
        }
    };
    let input = Input::Change {
        expected_generation,
        position_ms,
        replan,
        // A byte route has no worker; a software transcode overlaps when
        // another interactive worker is admissible now.
        overlap: app.processing.execution.fits_now(Class::Interactive, 1),
        now_ms: app.processing.deliveries.now_ms(),
    };
    // The receipt is checked again and recorded under the same lock as the
    // transition: a retry either replays this acknowledgement or, if the first
    // attempt never reached the reducer, applies the change once.
    let mut inner = session.inner.lock().unwrap();
    if let Some(view) = replayed_change(&inner, receipt)? {
        return Ok((view, true));
    }
    let d = &inner.delivery;
    let current = basis.as_ref().is_none_or(|basis| {
        d.pending
            .or(d.active)
            .and_then(|n| d.generations.get(&n))
            .is_some_and(|g| &g.pin == basis)
    });
    if !current {
        return Err(error(core::Error::GenerationConflict));
    }
    let d = transition_locked(app, &session, &mut inner, &input).map_err(error)?;
    let acknowledged = view(app, &session, &d);
    inner.changes.push((
        receipt.identity(),
        serde_json::to_value(&acknowledged).map_err(ApiError::internal)?,
    ));
    Ok((acknowledged, false))
}

/// Identity of a v2 change request: principal, Idempotency-Key, body digest.
pub(crate) struct ChangeReceipt {
    pub principal: String,
    pub key: String,
    pub digest: String,
}
impl ChangeReceipt {
    fn identity(&self) -> playscale_core::delivery_admission::Identity {
        playscale_core::delivery_admission::Identity {
            principal: self.principal.clone(),
            key: self.key.clone(),
            digest: self.digest.clone(),
        }
    }
}

/// The acknowledgement of an exact retry (core `delivery_admission::decide`);
/// a conflict when the key named a different request.
fn replayed_change(inner: &Inner, r: &ChangeReceipt) -> Result<Option<DeliveryView>, ApiError> {
    use playscale_core::delivery_admission::{self as admission, Decision, Receipt};
    let request = r.identity();
    let Some((identity, view)) = inner
        .changes
        .iter()
        .find(|(i, _)| i.principal == request.principal && i.key == request.key)
    else {
        return Ok(None);
    };
    let receipt = Receipt {
        identity: identity.clone(),
        delivery_id: String::new(),
    };
    match admission::decide(&request, Some(&receipt)) {
        Ok(Decision::Replay { .. }) => serde_json::from_value(view.clone())
            .map(Some)
            .map_err(ApiError::internal),
        Ok(Decision::Create) => Ok(None),
        Err(_) => Err(ApiError::conflict(
            "idempotency_conflict",
            "The Idempotency-Key was used with a different request",
        )),
    }
}

/// An exact retry of an acknowledged change, checked before the request's
/// plan token is admitted again (it may have expired since).
pub(crate) fn change_replay(
    app: &App,
    id: &str,
    receipt: &ChangeReceipt,
) -> Result<Option<DeliveryView>, ApiError> {
    let session = app.processing.deliveries.session(id)?;
    replayed_change(&session.inner.lock().unwrap(), receipt)
}

/// The v2 owner of a delivery: held live, or recorded at its admission.
pub(crate) async fn owner(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<Owner>, ApiError> {
    if let Some(owner) = live_owner(app, id) {
        return Ok(Some(owner));
    }
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT principal_id,profile_id,timeline_id,file_id,file_revision FROM delivery_owners WHERE delivery_id=?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(
        |(principal, profile, timeline, file, file_revision)| Owner {
            principal,
            profile,
            timeline,
            file,
            file_revision,
        },
    ))
}

/// Current view of a delivery: live, or its last recorded (fenced) state.
pub(crate) async fn any_view(app: &App, id: &str) -> Result<Option<DeliveryView>, ApiError> {
    if let Some(view) = live_view(app, id) {
        return Ok(Some(view));
    }
    Ok(recorded(app, id)
        .await?
        .map(|(file, revision, d)| view_of(app, id, &file, &revision, &d)))
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
        admit_owned(&app, authority, key, r, None).await
    })
    .await
    .map_err(ApiError::internal)?
}

async fn admit_owned(
    app: &App,
    authority: Arc<dyn AdmissionAuthority>,
    key: Option<String>,
    r: CreateRequest,
    planned: Option<Planned>,
) -> Result<(StatusCode, Json<DeliveryView>), ApiError> {
    // Byte routes (original or prepared) run no encoder.
    let original = planned
        .as_ref()
        .is_some_and(|p| matches!(p.route, Route::Original | Route::Prepared));
    let version = planned.as_ref().map(|p| p.version.clone());
    use playscale_core::delivery_admission::{self as admission, Decision, Identity, Receipt};
    use sha2::{Digest, Sha256};
    let runtime = &app.processing.deliveries;
    // Stream-copy eligibility needs the source's bitstream details; probe them
    // before taking the admission lock or the database writer.
    // A probe failure is reported only for a new admission: an exact retry of an
    // acknowledged one replays its receipt below.
    let copy_probe = match r.operation.map(LiveOperation::operation) {
        Some(operation)
            if !original
                && copies_video(operation)
                && app.processing.settings.experimental_copy_routes =>
        {
            Some(probe_copy_details(app, &r.file_id).await)
        }
        _ => None,
    };
    // Probed once per process; outside the admission lock and the writer.
    let hardware_ready = r.backend != Some(crate::processing::Backend::Videotoolbox)
        || crate::processing::videotoolbox_available(app).await;
    let _admission = runtime.admission.lock().await;
    let mut transaction = db::begin_write(&app.db).await?;
    let principal = authority.reauthorize(&mut transaction, &r).await?;
    if principal.is_empty() || principal.len() > 256 {
        return Err(ApiError::bad("Invalid admission principal"));
    }
    let identity = key.map(|key| Identity {
        principal,
        key,
        // v1 receipts keep their original digest; a planned (v2) admission also
        // pins the owner and route, so the same key cannot name another plan.
        digest: format!(
            "{:x}",
            Sha256::digest(
                match &planned {
                    None => serde_json::to_vec(&r),
                    Some(p) => serde_json::to_vec(&(&r, &p.owner, &p.version, p.route)),
                }
                .expect("serializable request")
            )
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
    authority.fresh()?;
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
    if let Some(planned) = &planned
        && (planned.owner.file != file.id || planned.owner.file_revision != file.revision)
    {
        return Err(ApiError::conflict(
            "source_revision_changed",
            "Refresh the item",
        ));
    }
    let operation = if let Some(p) = planned.as_ref().filter(|_| original) {
        p.route.operation()
    } else {
        r.operation
            .unwrap_or(LiveOperation::VideoTranscode)
            .operation()
    };
    let copy_details = copy_probe.transpose()?;
    if !hardware_ready {
        return Err(backend_unavailable());
    }
    let hardware = validate_backend(
        operation,
        r.backend
            .as_ref()
            .unwrap_or(&crate::processing::Backend::Software),
    )?;
    if copy_details
        .as_ref()
        .is_some_and(|d| d.revision != file.revision)
    {
        return Err(ApiError::conflict(
            "source_revision_changed",
            "Refresh the item",
        ));
    }
    if !original {
        validate_route(
            &file,
            r.audio_track,
            operation,
            app.processing.settings.experimental_copy_routes,
            copy_details.as_ref(),
        )?;
    }
    validate_subtitle(&file, r.subtitle_track)?;
    let duration_ms = file
        .duration_seconds
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| (d * 1000.0) as u64);
    if duration_ms.is_none() && !original {
        return Err(ApiError::bad("Live conversion requires a probed duration"));
    }
    let (root, root_identity): (String, String) =
        sqlx::query_as("SELECT root,root_identity FROM libraries WHERE id=?")
            .bind(&file.library_id)
            .fetch_one(&mut *transaction)
            .await?;
    if !cfg!(unix) && !original {
        // Encoder input is the inherited validated descriptor (Unix only).
        return Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "platform_unsupported",
            "Live conversion is not available on this platform",
        ));
    }
    let (delivery, effects) = Delivery::admit(
        // A planned delivery names its timeline; v1 binds one timeline per edition.
        planned
            .as_ref()
            .map_or_else(|| file.edition_id.clone(), |p| p.owner.timeline.clone()),
        if original {
            byte_pin(&file, r.audio_track, operation)
        } else {
            pin(&file, r.audio_track, r.subtitle_track, operation, hardware)
        },
        r.start_ms,
        duration_ms,
        runtime.now_ms(),
    )
    .map_err(error)?;
    let session = Arc::new(Session {
        id: new_id(),
        file_id: file.id.clone(),
        revision: file.revision.clone(),
        owner: planned.as_ref().map(|p| p.owner.clone()),
        source: Source {
            root: root.into(),
            relative: file.relative_path.clone().into(),
            root_identity,
            fingerprint: file.fingerprint.clone(),
        },
        inner: Mutex::new(Inner {
            delivery: delivery.clone(),
            workers: HashMap::new(),
            changes: Default::default(),
        }),
        subtitles: tokio::sync::Mutex::new(()),
    });
    // Byte routes run no encoder and do not consume encoding delivery slots.
    let (encoding, bytes) = runtime.counts();
    if (!original && encoding >= MAX_SESSIONS) || (original && bytes >= MAX_BYTE_SESSIONS) {
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
    if let (Some(owner), Some(version)) = (&session.owner, &version) {
        sqlx::query("INSERT INTO delivery_owners(delivery_id,principal_id,profile_id,timeline_id,version_id,file_id,file_revision,created_at) VALUES (?,?,?,?,?,?,?,?)")
            .bind(&session.id)
            .bind(&owner.principal)
            .bind(&owner.profile)
            .bind(&owner.timeline)
            .bind(version)
            .bind(&owner.file)
            .bind(&owner.file_revision)
            .bind(crate::now())
            .execute(&mut *transaction)
            .await?;
    }
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
    let replan = match (
        r.audio_track,
        r.operation,
        r.backend.as_ref(),
        r.subtitle_track,
    ) {
        (None, None, None, None) => None,
        (audio, operation, backend, subtitle) => {
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
            let subtitle = subtitle.unwrap_or_else(|| pinned_subtitle(&current));
            let operation = operation.map_or(current.operation, LiveOperation::operation);
            let hardware = match backend {
                Some(backend) => {
                    let hardware = validate_backend(operation, backend)?;
                    if hardware && !crate::processing::videotoolbox_available(&app).await {
                        return Err(backend_unavailable());
                    }
                    hardware
                }
                // Keep the hardware choice only while transcoding.
                None => {
                    operation == Operation::VideoTranscode
                        && current.recipe_digest.as_deref() == Some(VIDEOTOOLBOX_RECIPE)
                }
            };
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
            let details =
                if copies_video(operation) && app.processing.settings.experimental_copy_routes {
                    Some(probe_copy_details(&app, &session.file_id).await?)
                } else {
                    None
                };
            if details
                .as_ref()
                .is_some_and(|d| d.revision != file.revision)
            {
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
                details.as_ref(),
            )?;
            validate_subtitle(&file, subtitle)?;
            Some((
                current,
                (
                    file.edition_id.clone(),
                    pin(&file, audio, subtitle, operation, hardware),
                ),
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
    // Whether the staged generation will need a hardware session.
    let replan_hardware = match &replan {
        Some((_, pin)) => pin.recipe_digest.as_deref() == Some(VIDEOTOOLBOX_RECIPE),
        None => {
            let inner = session.inner.lock().unwrap();
            let d = &inner.delivery;
            d.pending
                .or(d.active)
                .and_then(|n| d.generations.get(&n))
                .is_some_and(|g| g.pin.recipe_digest.as_deref() == Some(VIDEOTOOLBOX_RECIPE))
        }
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
            // Overlap only when another interactive worker (and, for a hardware
            // encode, another hardware session) is admissible now.
            overlap: app.processing.execution.fits_now(Class::Interactive, 1)
                && (!replan_hardware || app.processing.hardware.fits_now(Class::Interactive, 1)),
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
    playlist_with(
        &app,
        &id,
        &generation,
        |i| format!("segments/{i}.m4s"),
        "init.mp4",
    )
    .await
}

/// The media playlist of a serving segmented generation, with URIs relative
/// to wherever the caller serves it.
pub(crate) async fn playlist_with(
    app: &App,
    id: &str,
    generation: &str,
    segment_uri: impl Fn(u32) -> String,
    init_uri: &str,
) -> Result<Response, ApiError> {
    let generation = generation_number(generation)?;
    let serves = |d: &Delivery, now| {
        d.serves(generation, now) && d.generations[&generation].pin.operation.segmented()
    };
    let (session, _permit) = admit_stream(app, id, serves).await?;
    let text = {
        let inner = session.inner.lock().unwrap();
        let d = &inner.delivery;
        // State may have changed while the catalog was read.
        if !serves(d, app.processing.deliveries.now_ms()) {
            return Err(ApiError::not_found());
        }
        core::media_playlist(&d.generations[&generation], segment_uri, init_uri)
    };
    Ok((stream_headers("application/vnd.apple.mpegurl"), text).into_response())
}

/// Stream an owned output file; the admission permit is held until the response
/// body completes or is dropped.
async fn file(
    permit: tokio::sync::OwnedSemaphorePermit,
    path: PathBuf,
) -> Result<Response, ApiError> {
    streamed(permit, path, "video/mp4").await
}

/// Stream an owned file, holding the transfer permit until the body ends.
async fn streamed(
    permit: tokio::sync::OwnedSemaphorePermit,
    path: PathBuf,
    content_type: &'static str,
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
    Ok((stream_headers(content_type), body).into_response())
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

/// WebVTT sidecar of a generation's pinned text subtitle, in timeline time
/// (cues keep source time, which is timeline time). Converted once per
/// delivery from the validated source descriptor and cached with its files.
pub async fn subtitles(
    State(app): State<App>,
    Path((id, generation)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let generation = generation_number(&generation)?;
    let pinned = |d: &Delivery, now| {
        d.serves(generation, now) && pinned_subtitle(&d.generations[&generation].pin).is_some()
    };
    let session = app.processing.deliveries.session(&id)?;
    let index = {
        let inner = session.inner.lock().unwrap();
        let d = &inner.delivery;
        if !pinned(d, app.processing.deliveries.now_ms()) {
            return Err(ApiError::not_found());
        }
        pinned_subtitle(&d.generations[&generation].pin).ok_or_else(ApiError::not_found)?
    };
    let path = app
        .processing
        .deliveries
        .root
        .join(&session.id)
        .join(format!("subtitles-{index}.vtt"));
    // Convert (once) before taking a transfer permit, in a task that owns the
    // converter even if this request is cancelled.
    tokio::spawn(ensure_subtitles(
        app.clone(),
        session.clone(),
        index,
        path.clone(),
    ))
    .await
    .map_err(ApiError::internal)??;
    let (_, permit) = admit_stream(&app, &id, pinned).await?;
    streamed(permit, path, "text/vtt; charset=utf-8").await
}

/// Largest sidecar accepted from the converter.
const MAX_SUBTITLE_BYTES: u64 = 16 * 1024 * 1024;

fn subtitles_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "subtitles_unavailable",
        "The subtitle track could not be converted; try again",
    )
}

/// Produce the cached sidecar unless it exists. Waiters for the same delivery
/// queue on its lock (no permits held); conversions share a small global slot
/// pool; a converter that does not exit keeps both until it is reaped.
async fn ensure_subtitles(
    app: App,
    session: Arc<Session>,
    index: u32,
    path: PathBuf,
) -> Result<(), ApiError> {
    let _extraction = session.subtitles.lock().await;
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(());
    }
    let _slot = app
        .processing
        .deliveries
        .subtitle_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| subtitles_unavailable())?;
    let source = session.source.clone();
    let opened = tokio::time::timeout(
        PROBE_DEADLINE,
        tokio::task::spawn_blocking(move || source_valid(&source)),
    )
    .await
    .map_err(|_| subtitles_unavailable())?
    .map_err(ApiError::internal)?;
    let input = opened
        .map_err(|_| ApiError::conflict("source_revision_changed", "Create a new delivery"))?;
    let map = format!("0:s:{index}");
    let converted = run_descriptor(
        &app.processing.settings.ffmpeg,
        &input,
        &[
            "-v",
            "error",
            "-nostdin",
            "-i",
            "/dev/fd/3",
            "-map",
            &map,
            "-c:s",
            "webvtt",
            "-f",
            "webvtt",
            "pipe:1",
        ],
        MAX_SUBTITLE_BYTES,
        Duration::from_secs(120),
    )
    .await;
    let text = match converted {
        Ok(text) if text.starts_with(b"WEBVTT") => text,
        Ok(_) | Err(ProbeFailure::Unavailable) => return Err(subtitles_unavailable()),
        Err(ProbeFailure::Stalled(mut child)) => {
            tracing::error!(delivery=%session.id, "subtitle conversion did not exit when killed");
            // Keep the lock and slot until the converter is actually gone.
            let _ = child.wait().await;
            return Err(subtitles_unavailable());
        }
    };
    // Publish into the existing delivery directory only: if cleanup removed it,
    // the delivery is gone and nothing is resurrected.
    let partial = path.with_extension("vtt.partial");
    let written = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &text).await?;
        file.sync_all().await?;
        tokio::fs::rename(&partial, &path).await
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(ApiError::not_found());
    }
    Ok(())
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
    use super::{CopyDetails, Operation, db, parse_copy_details, validate_route};

    #[test]
    fn copy_details_parse_profile_level_field_order_and_rotation() {
        let parse = |json: &str| parse_copy_details(json.as_bytes(), "r".into());
        let plain =
            parse(r#"{"streams":[{"profile":"High","level":40,"field_order":"progressive"}]}"#)
                .unwrap();
        assert_eq!(
            (
                plain.profile.as_str(),
                plain.level,
                plain.progressive,
                plain.rotated
            ),
            ("High", 40, true, false)
        );
        let rotated = parse(r#"{"streams":[{"profile":"Main","level":31,"field_order":"progressive","side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]}]}"#).unwrap();
        assert!(rotated.rotated);
        let upright = parse(r#"{"streams":[{"profile":"Main","level":31,"field_order":"progressive","side_data_list":[{"rotation":0}]}]}"#).unwrap();
        assert!(!upright.rotated);
        assert!(
            !parse(r#"{"streams":[{"profile":"High","level":40}]}"#)
                .unwrap()
                .progressive
        );
        assert!(
            !parse(r#"{"streams":[{"profile":"High","level":40,"field_order":"tt"}]}"#)
                .unwrap()
                .progressive
        );
        assert_eq!(parse(r#"{"streams":[]}"#), None);
    }

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
        let details = CopyDetails {
            revision: "r".into(),
            profile: "High".into(),
            level: 40,
            progressive: true,
            rotated: false,
        };
        let eligible = file(tracks("yuv420p", 0.0));
        // Details observed for another revision do not count.
        let stale = CopyDetails {
            revision: "old".into(),
            ..details.clone()
        };
        assert_eq!(
            code(validate_route(
                &eligible,
                Some(0),
                Operation::Remux,
                true,
                Some(&stale)
            ))
            .await
            .as_deref(),
            Some("route_unsupported")
        );
        assert!(validate_route(&eligible, Some(0), Operation::Remux, true, Some(&details)).is_ok());
        assert_eq!(
            code(validate_route(
                &eligible,
                Some(0),
                Operation::Remux,
                false,
                Some(&details)
            ))
            .await
            .as_deref(),
            Some("route_unqualified")
        );
        assert!(validate_route(&eligible, Some(0), Operation::VideoTranscode, false, None).is_ok());
        // Audio starting before zero moves the container start; 10-bit video
        // cannot be copied into the browser pipeline.
        for ineligible in [
            file(tracks("yuv420p", -0.5)),
            file(tracks("yuv420p10le", 0.0)),
        ] {
            assert_eq!(
                code(validate_route(
                    &ineligible,
                    Some(0),
                    Operation::Remux,
                    true,
                    Some(&details)
                ))
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
                tracks: tracks(None, None),
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
        assert_eq!(
            tracks(None, None),
            ["video:0", "audio:none", "subtitle:none"]
        );
        assert_eq!(
            tracks(Some(1), Some(0)),
            ["video:0", "audio:1", "subtitle:0"]
        );
    }
}
