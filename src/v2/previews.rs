//! Trick-play previews (completion 6C): thumbnail sheets of a timeline's
//! whole single-file original, rendered by a bounded job from one exact
//! file revision, validated and published only while that revision is
//! current (core `previews`), served only to callers who may read the file,
//! and removed once the revision is no longer current.
use super::{Problem, Scope, auth::Caller, etag, idempotency_key};
use crate::{App, db::begin_write};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::{
    access::{self, Idempotent, Permission, Principal},
    jobs::{self, Effect, Input, Job},
    previews::{self as core, Layout, PublicationFacts, Sheet},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;

pub fn routes() -> Router<App> {
    Router::new()
        .route(
            "/catalog/timelines/{timeline_id}/previews",
            get(manifest).post(create),
        )
        .route("/media/files/{file_id}/previews/{sheet}", get(sheet))
}

/// How long one rendering may run before it is stopped.
const RENDER_DEADLINE: Duration = Duration::from_secs(30 * 60);
/// At most this many preview jobs wait or run at once.
pub(crate) const MAX_ACTIVE_JOBS: i64 = 100;

/// The timeline's whole, single-file, available original the caller may
/// read, with its duration and picture size: (file, revision, duration ms,
/// width, height). Candidates are paged until a readable one is found.
async fn original(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    timeline: &str,
) -> Result<Option<(String, String, Option<u64>, u32, u32)>, Problem> {
    type Row = (String, String, String, Option<f64>, Option<i64>, String);
    let mut after = (String::new(), String::new());
    loop {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT v.id,f.id,f.revision,f.duration_seconds,t.duration_ms,f.tracks_json FROM timelines t JOIN media_versions v ON v.timeline_id=t.id \
             JOIN version_files b ON b.version_id=v.id AND b.part=1 JOIN media_files f ON f.id=b.file_id \
             WHERE t.id=? AND v.origin='original' AND f.generated=0 AND f.available=1 AND b.file_revision=f.revision \
             AND b.start_ms IS NULL AND b.end_ms IS NULL \
             AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1) \
             AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1) \
             AND (v.id>? OR (v.id=? AND f.id>?)) ORDER BY v.id,f.id LIMIT 100",
        )
        .bind(timeline)
        .bind(&after.0)
        .bind(&after.0)
        .bind(&after.1)
        .fetch_all(&mut *conn)
        .await?;
        let Some((version, file, ..)) = rows.last() else {
            return Ok(None);
        };
        after = (version.clone(), file.clone());
        for (_, file, revision, seconds, _timeline_ms, tracks) in rows {
            if !playscale_core::playback_session::readable(
                principal,
                super::content::file_facts(conn, &file).await?.as_ref(),
            ) {
                continue;
            }
            let tracks: Vec<crate::db::Track> = serde_json::from_str(&tracks).unwrap_or_default();
            let Some(video) = tracks.iter().find(|t| t.kind == "video") else {
                continue;
            };
            let duration = seconds
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| (s * 1000.0).floor() as u64);
            if duration
                .and_then(|d| {
                    core::layout(
                        d,
                        video.display_dimensions().0,
                        video.display_dimensions().1,
                    )
                })
                .is_none()
            {
                continue;
            }
            return Ok(Some((
                file,
                revision,
                duration,
                video.display_dimensions().0,
                video.display_dimensions().1,
            )));
        }
    }
}

/// Technical metadata is part of preview identity even when media bytes are unchanged.
pub(crate) async fn layout_identity(
    conn: &mut sqlx::SqliteConnection,
    file: &str,
    revision: &str,
) -> Result<Option<String>, sqlx::Error> {
    let facts: Option<(Option<f64>, String)> = sqlx::query_as("SELECT duration_seconds,tracks_json FROM media_files WHERE id=? AND revision=? AND available=1")
        .bind(file).bind(revision).fetch_optional(conn).await?;
    Ok(facts.map(|facts| super::sha256(serde_json::to_string(&facts).unwrap().as_bytes())))
}

pub(crate) async fn admission(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    file: &str,
    revision: &str,
    identity: &str,
    exclude: &str,
) -> Result<core::Admission, Problem> {
    let current = layout_identity(conn, file, revision).await?;
    let readable = playscale_core::playback_session::readable(
        principal,
        super::content::file_facts(conn, file).await?.as_ref(),
    );
    let duplicate: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM preview_jobs WHERE file_id=? AND file_revision=? AND layout_identity=? AND id<>? AND phase IN ('queued','running','cancelling')) OR EXISTS(SELECT 1 FROM previews WHERE file_id=? AND file_revision=? AND layout_identity=?)")
        .bind(file).bind(revision).bind(identity).bind(exclude).bind(file).bind(revision).bind(identity).fetch_one(&mut *conn).await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM preview_jobs WHERE phase IN ('queued','running','cancelling')",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(core::AdmissionFacts {
        source_current: current.as_deref() == Some(identity),
        requester_authorized: readable,
        layout_valid: !identity.is_empty(),
        equivalent_work: duplicate != 0,
        active_jobs: active as u64,
        capacity: MAX_ACTIVE_JOBS as u64,
    }
    .decide())
}

/// Where a revision's published sheets live, beside the processing and
/// delivery directories (never inside them), and where attempts render.
fn previews_root(app: &App) -> PathBuf {
    let root = &app.processing.root;
    root.parent().unwrap_or(root).join("previews")
}
fn published_dir(app: &App, file: &str, revision: &str) -> PathBuf {
    previews_root(app).join(file).join(revision)
}
fn staging_dir(app: &App, job: &str, attempt: i64) -> PathBuf {
    previews_root(app)
        .join(".staging")
        .join(format!("{job}-{attempt}"))
}

fn sheet_url(file: &str, revision: &str, identity: &str, n: u32) -> String {
    format!("/api/v2/media/files/{file}/previews/{n}?revision={revision}&layout={identity}")
}

type PreviewRow = (String, i64, i64, i64, i64, i64, i64, i64);

/// The manifest of a revision's previews: ready (layout and sheets),
/// pending (a job is rendering them) or none.
async fn manifest_body(
    tx: &mut sqlx::SqliteConnection,
    timeline: &str,
    file: &str,
    revision: &str,
) -> Result<Value, Problem> {
    let identity = layout_identity(tx, file, revision)
        .await?
        .unwrap_or_default();
    let ready: Option<PreviewRow> = sqlx::query_as(
        "SELECT job_id,interval_ms,tile_width,tile_height,columns,rows,tiles,sheets FROM previews WHERE file_id=? AND file_revision=? AND layout_identity=?",
    )
    .bind(file)
    .bind(revision)
    .bind(&identity)
    .fetch_optional(&mut *tx)
    .await?;
    let pending: Option<String> = sqlx::query_scalar(
        "SELECT id FROM preview_jobs WHERE file_id=? AND file_revision=? AND layout_identity=? AND phase IN ('queued','running','cancelling') LIMIT 1",
    )
    .bind(file)
    .bind(revision)
    .bind(&identity)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(match ready {
        Some((job, interval, width, height, columns, rows, tiles, sheets)) => json!({
            "timeline_id": timeline, "status": "ready", "file_id": file, "file_revision": revision,
            "job_id": job, "interval_ms": interval, "tile_width": width, "tile_height": height,
            "columns": columns, "rows": rows, "tiles": tiles,
            "sheets": (1..=u32::try_from(sheets).unwrap_or(0)).map(|n| sheet_url(file, revision, &identity, n)).collect::<Vec<_>>(),
        }),
        None => json!({
            "timeline_id": timeline,
            "status": if pending.is_some() { "pending" } else { "none" },
            "file_id": file, "file_revision": revision, "job_id": pending,
        }),
    })
}

/// The timeline's previews: ready (with their layout and sheets), pending
/// (a job is rendering them) or none.
async fn manifest(
    State(app): State<App>,
    caller: Caller,
    Path(timeline): Path<String>,
) -> Result<Json<Value>, Problem> {
    caller.require(Permission::CatalogRead)?;
    let mut tx = app.db.begin().await?;
    super::playback::readable_timeline(&mut tx, &caller.principal, &timeline).await?;
    let Some((file, revision, ..)) = original(&mut tx, &caller.principal, &timeline).await? else {
        return Ok(Json(json!({"timeline_id": timeline, "status": "none"})));
    };
    let body = manifest_body(&mut tx, &timeline, &file, &revision).await?;
    tx.commit().await?;
    Ok(Json(body))
}

/// Request previews of the timeline's original at its current revision:
/// the manifest, pending (202 when this request queued the job) or as it
/// already is (200). Never duplicated: an active job for that revision, or
/// its published previews, answer instead of a new job.
async fn create(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    Path(timeline): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::ProcessingRequest)?;
    let key = idempotency_key(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProcessingRequest))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createTimelinePreviews",
        target: &timeline,
        key: &key,
    };
    let digest = super::sha256(b"");
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|r| &r.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        let stored = stored.unwrap();
        super::playback::readable_timeline(&mut tx, &principal, &timeline).await?;
        let Some((file, revision, ..)) = original(&mut tx, &principal, &timeline).await? else {
            return Err(Problem::not_found());
        };
        let body = manifest_body(&mut tx, &timeline, &file, &revision).await?;
        let mut response = (stored.status, Json(body)).into_response();
        response
            .headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(response);
    }
    super::playback::readable_timeline(&mut tx, &principal, &timeline).await?;
    let (file, revision, duration, width, height) = original(&mut tx, &principal, &timeline)
        .await?
        .ok_or_else(|| {
            Problem::invalid(
                "preview_unavailable",
                "Previews are made of a timeline's whole single-file original, which this one has none of",
            )
        })?;
    if duration
        .and_then(|d| core::layout(d, width, height))
        .is_none()
    {
        return Err(Problem::invalid(
            "preview_unavailable",
            "The original's duration or picture size is unknown",
        ));
    }
    let identity = layout_identity(&mut tx, &file, &revision)
        .await?
        .ok_or_else(Problem::not_found)?;
    let status = match admission(&mut tx, &principal, &file, &revision, &identity, "").await? {
        core::Admission::Existing => StatusCode::OK,
        core::Admission::Invalid => return Err(Problem::not_found()),
        core::Admission::Full => {
            return Err(Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "processing_limit",
                "The preview queue is full",
            ));
        }
        core::Admission::Enqueue => {
            let id = crate::new_id();
            sqlx::query("INSERT INTO preview_jobs(id,file_id,file_revision,layout_identity,requester_id,credential_hash,ingress_id,created_at) VALUES (?,?,?,?,?,?,?,?)")
                .bind(&id).bind(&file).bind(&revision).bind(&identity).bind(&principal.id)
                .bind(caller.credential.as_ref().map(|(hash, _)| hash)).bind(&caller.ingress).bind(crate::now())
                .execute(&mut *tx).await?;
            StatusCode::ACCEPTED
        }
    };
    // The manifest, as any reader sees it (the job by its ID only: its
    // details are its requester's).
    let value = manifest_body(&mut tx, &timeline, &file, &revision).await?;
    record
        .save(&mut tx, &digest, status, Some(&value), None)
        .await?;
    tx.commit().await?;
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(header::ETAG, etag(1));
    Ok(response)
}

#[derive(Deserialize)]
struct SheetQuery {
    revision: String,
    layout: String,
}

/// One published sheet (JPEG), only to a caller who may read the file, and
/// only for its current revision.
async fn sheet(
    State(app): State<App>,
    caller: Caller,
    Path((file, sheet)): Path<(String, u32)>,
    Query(query): Query<SheetQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let _guard = app.jobs.lock().await;
    let mut tx = app.db.begin().await?;
    if !playscale_core::playback_session::readable(
        &caller.principal,
        super::content::file_facts(&mut tx, &file).await?.as_ref(),
    ) {
        return Err(Problem::not_found());
    }
    if layout_identity(&mut tx, &file, &query.revision)
        .await?
        .as_deref()
        != Some(query.layout.as_str())
    {
        return Err(Problem::not_found());
    }
    let sheets: Option<i64> = sqlx::query_scalar(
        "SELECT p.sheets FROM previews p JOIN media_files f ON f.id=p.file_id AND f.revision=p.file_revision \
         WHERE p.file_id=? AND p.file_revision=? AND p.layout_identity=? AND f.available=1",
    )
    .bind(&file)
    .bind(&query.revision)
    .bind(&query.layout)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let sheets = sheets.ok_or_else(Problem::not_found)?;
    if sheet == 0 || i64::from(sheet) > sheets {
        return Err(Problem::not_found());
    }
    let path = published_dir(&app, &file, &query.revision).join(format!("sheet-{sheet}.jpg"));
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| Problem::not_found())?;
    if file
        .metadata()
        .await
        .map_err(|_| Problem::not_found())?
        .len()
        > core::MAX_SHEET_BYTES
    {
        return Err(Problem::not_found());
    }
    let mut bytes = Vec::new();
    file.take(core::MAX_SHEET_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Problem::not_found())?;
    if bytes.len() as u64 > core::MAX_SHEET_BYTES {
        return Err(Problem::not_found());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "image/jpeg".to_owned()),
            (header::CACHE_CONTROL, "private, max-age=86400".to_owned()),
            (
                header::ETAG,
                format!("\"{}-{}-{sheet}\"", query.revision, query.layout),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Render queued preview jobs one at a time, and remove previews whose
/// revision is no longer current.
pub async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    recover(&app).await?;
    let mut last_retention = std::time::Instant::now() - Duration::from_secs(3600);
    loop {
        if stop.is_cancelled() {
            return Ok(());
        }
        if last_retention.elapsed() > Duration::from_secs(60) {
            if let Err(error) = retain(&app).await {
                tracing::warn!(%error, "preview retention will retry");
            }
            last_retention = std::time::Instant::now();
        }
        let queued: Option<String> =
            sqlx::query_scalar("SELECT id FROM preview_jobs WHERE phase='queued' LIMIT 1")
                .fetch_optional(&app.db)
                .await?;
        if queued.is_none() {
            tokio::select! {_ = stop.cancelled() => return Ok(()), _ = tokio::time::sleep(Duration::from_millis(500)) => {}}
            continue;
        }
        // Capacity first, as preparations do: a preview never starves
        // interactive playback of encoder units.
        let lease = tokio::select! {
            _ = stop.cancelled() => return Ok(()),
            lease = app.processing.execution.reserve(format!("preview:{}", crate::new_id()), playscale_core::work::Class::Maintenance, 1)
                => lease.map_err(|e| anyhow::anyhow!("preview reservation rejected: {e:?}"))?,
        };
        let lease = std::sync::Arc::new(lease);
        if let Some(job) = start(&app).await? {
            let rendered = render(&app, &job, &stop, &lease).await;
            // Stopping the server leaves the attempt to be recovered at the
            // next start, not failed.
            if stop.is_cancelled() {
                return Ok(());
            }
            // A renderer not confirmed gone keeps its staging (and witness),
            // so recovery after a restart still accounts for it.
            finish(&app, &job, rendered, lease.is_stuck()).await?;
        }
        drop(lease);
    }
}

struct Started {
    id: String,
    attempt: i64,
    file: String,
    revision: String,
}

/// Attempts a stopped server left running or cancelling are recovered
/// (core `jobs::transition` with `Recover`): requeued or cancelled.
async fn recover(app: &App) -> anyhow::Result<()> {
    // Renderers of a previous process still alive (their witness held) keep
    // their capacity until they are gone, before any attempt is requeued;
    // their staging is left alone until then (`retain`).
    let staging = previews_root(app).join(".staging");
    for (path, witness) in
        tokio::task::spawn_blocking(move || crate::processing::held_witnesses(&staging)).await??
    {
        app.processing.execution.recovered(
            format!("recovered:{}", path.display()),
            playscale_core::work::Class::Maintenance,
            1,
            witness,
            || {},
        );
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id,phase,attempt FROM preview_jobs WHERE phase IN ('running','cancelling')",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, phase, attempt) in rows {
        let current = Job {
            phase: serde_json::from_value(json!(phase))?,
            attempt: attempt.try_into()?,
        };
        let (next, _) = jobs::transition(&current, Input::Recover);
        sqlx::query("UPDATE preview_jobs SET phase=? WHERE id=?")
            .bind(crate::db::phase_name(next.phase))
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Start the oldest queued job (core `jobs::transition`).
async fn start(app: &App) -> anyhow::Result<Option<Started>> {
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let row: Option<(String, String, i64, String, String)> = sqlx::query_as(
        "SELECT id,phase,attempt,file_id,file_revision FROM preview_jobs WHERE phase='queued' ORDER BY created_at,id LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, phase, attempt, file, revision)) = row else {
        return Ok(None);
    };
    let current = Job {
        phase: serde_json::from_value(json!(phase))?,
        attempt: attempt.try_into()?,
    };
    let (next, effects) = jobs::transition(&current, Input::Start);
    sqlx::query("UPDATE preview_jobs SET phase=?,attempt=? WHERE id=?")
        .bind(crate::db::phase_name(next.phase))
        .bind(next.attempt)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(
        matches!(effects.as_slice(), [Effect::Run { .. }]).then(|| Started {
            id,
            attempt: next.attempt.into(),
            file,
            revision,
        }),
    )
}

/// Whether this attempt should go on: still running, still this attempt.
async fn running(app: &App, job: &Started) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM preview_jobs WHERE id=? AND phase='running' AND attempt=?",
    )
    .bind(&job.id)
    .bind(job.attempt)
    .fetch_optional(&app.db)
    .await
    .map_or(true, |r| r.is_some())
}

/// Render the attempt's sheets into its staging directory from the
/// validated source: only key frames are decoded. Stopped when the job is
/// cancelled, the server stops or the deadline passes; a renderer that does
/// not exit when killed is handed to the lease's reaper. The layout and the
/// observed sheets, with whether the source stayed unchanged.
async fn render(
    app: &App,
    job: &Started,
    stop: &CancellationToken,
    lease: &std::sync::Arc<crate::execution::Lease>,
) -> anyhow::Result<(Layout, Vec<Sheet>, bool)> {
    let facts: (Option<f64>, String) = sqlx::query_as(
        "SELECT duration_seconds,tracks_json FROM media_files WHERE id=? AND revision=?",
    )
    .bind(&job.file)
    .bind(&job.revision)
    .fetch_one(&app.db)
    .await?;
    let expected: String =
        sqlx::query_scalar("SELECT layout_identity FROM preview_jobs WHERE id=?")
            .bind(&job.id)
            .fetch_one(&app.db)
            .await?;
    anyhow::ensure!(
        super::sha256(serde_json::to_string(&facts)?.as_bytes()) == expected,
        "preview layout changed"
    );
    let tracks: Vec<crate::db::Track> = serde_json::from_str(&facts.1)?;
    let video = tracks
        .iter()
        .find(|t| t.kind == "video")
        .ok_or_else(|| anyhow::anyhow!("no video"))?;
    let duration = facts
        .0
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 1000.0).floor() as u64)
        .ok_or_else(|| anyhow::anyhow!("no duration"))?;
    let layout = core::layout(
        duration,
        video.display_dimensions().0,
        video.display_dimensions().1,
    )
    .ok_or_else(|| anyhow::anyhow!("no layout"))?;
    let (input, fingerprint) =
        crate::delivery::open_source(app, &job.file, &job.revision, lease.clone(), stop)
            .await
            .map_err(|e| anyhow::anyhow!("source: {}", e.parts().1))?;
    let staging = staging_dir(app, &job.id, job.attempt);
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging).await?;
    let (w, h) = (layout.tile_width, layout.tile_height);
    // One thumbnail per tile, at exactly 0, interval, 2·interval, …: the
    // last frame at or before each (key frames only; `round=up` places a
    // frame in the first slot at or after its time, so each slot keeps the
    // latest frame not after it), and one for a final part-interval
    // (`eof_action=pass`). Sheets are assembled here, so their tiles are
    // counted, never padded with blanks.
    let filter = format!(
        "fps=fps=1000/{interval}:start_time=0:round=up:eof_action=pass,\
         scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1",
        interval = layout.interval_ms,
    );
    let tiles = layout.tiles.to_string();
    let output = staging.join("thumb-%d.jpg");
    let mut supervisor = tokio::process::Command::new(&app.processing.supervisor);
    supervisor
        .arg("--internal-ffmpeg-supervisor")
        .arg(&app.processing.settings.ffmpeg)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-skip_frame",
            "nokey",
            "-i",
            "/dev/fd/3",
        ])
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-fps_mode",
            "passthrough",
        ])
        .args(["-frames:v", &tiles, "-q:v", "5", "-f", "image2"])
        .arg(&output)
        // The supervisor ends the renderer when this pipe closes, even if
        // the server is killed: no renderer outlives its owner.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    // fd 3: the validated source; fd 4: the ownership witness. Capacity
    // returns only once the witness is free (recovered after a restart).
    let owner = staging.join(".owner");
    let (witness, held) = crate::execution::Witness::create(&owner)?;
    #[cfg(unix)]
    crate::execution::inherit(&mut supervisor, &[&input, &held.0]);
    lease.started(&owner)?;
    let child = supervisor.spawn();
    drop(held);
    let mut child = child?;
    let heartbeat = child.stdin.take();
    let deadline = tokio::time::Instant::now() + RENDER_DEADLINE;
    let status = loop {
        tokio::select! {
            status = child.wait() => break Some(status?),
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if stop.is_cancelled()
                    || lease.termination_requested().is_cancelled()
                    || tokio::time::Instant::now() >= deadline
                    || !running(app, job).await
                {
                    break None;
                }
            }
        }
    };
    drop(heartbeat);
    let confirmed = crate::execution::confirm_exit(
        lease,
        status.is_none().then_some(child),
        Some(witness),
        crate::execution::TERMINATION_DEADLINE,
        || {},
    )
    .await;
    anyhow::ensure!(confirmed, "renderer outlived its supervisor");
    let status = status.ok_or_else(|| anyhow::anyhow!("rendering stopped"))?;
    anyhow::ensure!(status.success(), "renderer failed");
    // The renderer read the descriptor after it was validated: observe it
    // again before trusting what it rendered.
    let unchanged = tokio::task::spawn_blocking(move || {
        input
            .metadata()
            .is_ok_and(|m| crate::scan::fingerprint(&m) == fingerprint)
    })
    .await?;
    let observed = tokio::task::spawn_blocking(move || -> anyhow::Result<(bool, Vec<Sheet>)> {
        let mut thumbnails = Vec::new();
        for n in 1.. {
            let path = staging.join(format!("thumb-{n}.jpg"));
            let Ok(meta) = std::fs::metadata(&path) else {
                break;
            };
            let (width, height) = image::image_dimensions(&path)?;
            thumbnails.push(Sheet {
                width,
                height,
                bytes: meta.len(),
            });
        }
        if !core::valid_thumbnails(&layout, &thumbnails) {
            return Ok((false, vec![]));
        }
        // Assemble each sheet row by row from its thumbnails.
        let per_sheet = layout.columns * layout.rows;
        let mut sheets = Vec::new();
        for sheet in 0..layout.sheets {
            let mut canvas = image::RgbImage::new(
                layout.tile_width * layout.columns,
                layout.tile_height * layout.rows,
            );
            for within in 0..per_sheet {
                let n = sheet * per_sheet + within;
                if n >= layout.tiles {
                    break;
                }
                let tile = image::open(staging.join(format!("thumb-{}.jpg", n + 1)))?.to_rgb8();
                image::imageops::replace(
                    &mut canvas,
                    &tile,
                    i64::from(within % layout.columns * layout.tile_width),
                    i64::from(within / layout.columns * layout.tile_height),
                );
            }
            let path = staging.join(format!("sheet-{}.jpg", sheet + 1));
            let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 80)
                .encode_image(&canvas)?;
            std::io::Write::flush(&mut file)?;
            drop(file);
            sheets.push(Sheet {
                width: canvas.width(),
                height: canvas.height(),
                bytes: std::fs::metadata(&path)?.len(),
            });
        }
        for n in 1..=layout.tiles {
            std::fs::remove_file(staging.join(format!("thumb-{n}.jpg")))?;
        }
        Ok((true, sheets))
    })
    .await??;
    Ok((layout, observed.1, unchanged && observed.0))
}

/// Publish a valid attempt's sheets, or discard them (core
/// `PublicationFacts`), and finish the job (core `jobs::transition`), under
/// the jobs lock in one write transaction with the attempt checked.
async fn finish(
    app: &App,
    job: &Started,
    rendered: anyhow::Result<(Layout, Vec<Sheet>, bool)>,
    stuck: bool,
) -> anyhow::Result<()> {
    if let Err(error) = &rendered {
        tracing::warn!(job_id=%job.id, %error, "preview rendering failed");
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let (phase, attempt, requester, credential, ingress): (
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT phase,attempt,requester_id,credential_hash,ingress_id FROM preview_jobs WHERE id=?",
    )
    .bind(&job.id)
    .fetch_one(&mut *tx)
    .await?;
    let identity: String =
        sqlx::query_scalar("SELECT layout_identity FROM preview_jobs WHERE id=?")
            .bind(&job.id)
            .fetch_one(&mut *tx)
            .await?;
    let source_current = layout_identity(&mut tx, &job.file, &job.revision)
        .await?
        .as_deref()
        == Some(identity.as_str());
    let requester_authorized = match crate::metadata_jobs::principal(
        &mut tx,
        &requester,
        credential.as_deref(),
        ingress.as_deref(),
        Permission::ProcessingRequest,
    )
    .await
    {
        Ok(principal) => playscale_core::playback_session::readable(
            &principal,
            super::content::file_facts(&mut tx, &job.file)
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}"))?
                .as_ref(),
        ),
        Err(_) => false,
    };
    let (layout, sheets_valid) = match &rendered {
        Ok((layout, sheets, unchanged)) => (
            Some(*layout),
            *unchanged && core::valid_sheets(layout, sheets),
        ),
        Err(_) => (None, false),
    };
    let facts = PublicationFacts {
        attempt_current: phase == "running" && attempt == job.attempt,
        source_current,
        sheets_valid,
        requester_authorized,
    };
    let current = Job {
        phase: serde_json::from_value(json!(phase))?,
        attempt: attempt.try_into()?,
    };
    let (next, effects) = jobs::transition(
        &current,
        Input::Finished {
            attempt: job.attempt.try_into()?,
            success: facts.publishable(),
        },
    );
    let staging = staging_dir(app, &job.id, job.attempt);
    if effects.iter().any(|e| matches!(e, Effect::Publish { .. }))
        && let Some(layout) = layout
    {
        let target = published_dir(app, &job.file, &job.revision);
        let published = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM previews WHERE file_id=? AND file_revision=? AND layout_identity=?",
        )
        .bind(&job.file)
        .bind(&job.revision)
        .bind(&identity)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if !published {
            // Durable sheets and their directory entries, then the immutable
            // directory under durable ancestors, then the row: a committed
            // row never names sheets a power loss could take away.
            let (from, to, root) = (staging.clone(), target.clone(), previews_root(app));
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let sync = |dir: &std::path::Path| -> anyhow::Result<()> {
                    #[cfg(unix)]
                    std::fs::File::open(dir)?.sync_all()?;
                    Ok(())
                };
                let _ = std::fs::remove_file(from.join(".owner"));
                for entry in std::fs::read_dir(&from)? {
                    std::fs::File::open(entry?.path())?.sync_all()?;
                }
                sync(&from)?;
                if let Some(parent) = root.parent() {
                    sync(parent)?;
                }
                if to.exists() {
                    std::fs::remove_dir_all(&to)?;
                }
                let parent = to.parent().unwrap_or(&to).to_path_buf();
                let created = !parent.exists();
                std::fs::create_dir_all(&parent)?;
                if created {
                    sync(&root)?;
                }
                std::fs::rename(&from, &to)?;
                sync(&to)?;
                sync(&parent)?;
                Ok(())
            })
            .await??;
            sqlx::query("INSERT INTO previews(file_id,file_revision,job_id,interval_ms,tile_width,tile_height,columns,rows,tiles,sheets,created_at,layout_identity) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id,file_revision) DO UPDATE SET job_id=excluded.job_id,interval_ms=excluded.interval_ms,tile_width=excluded.tile_width,tile_height=excluded.tile_height,columns=excluded.columns,rows=excluded.rows,tiles=excluded.tiles,sheets=excluded.sheets,created_at=excluded.created_at,layout_identity=excluded.layout_identity")
                .bind(&job.file).bind(&job.revision).bind(&job.id)
                .bind(layout.interval_ms as i64).bind(layout.tile_width).bind(layout.tile_height)
                .bind(layout.columns).bind(layout.rows).bind(layout.tiles).bind(layout.sheets)
                .bind(crate::now()).bind(&identity).execute(&mut *tx).await?;
        }
    }
    sqlx::query("UPDATE preview_jobs SET phase=?,error=? WHERE id=?")
        .bind(crate::db::phase_name(next.phase))
        .bind((next.phase == jobs::Phase::Failed).then_some("preview_failed"))
        .bind(&job.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    drop(_guard);
    if !stuck {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    Ok(())
}

fn directory_page(root: &std::path::Path) -> Vec<std::fs::DirEntry> {
    static WALKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, std::fs::ReadDir>>,
    > = std::sync::OnceLock::new();
    let mut walks = WALKS.get_or_init(Default::default).lock().unwrap();
    if !walks.contains_key(root) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return vec![];
        };
        walks.insert(root.to_owned(), entries);
    }
    let entries = walks.get_mut(root).unwrap();
    let mut page = Vec::new();
    let mut ended = false;
    for _ in 0..100 {
        match entries.next() {
            Some(Ok(entry)) => page.push(entry),
            Some(Err(_)) => {}
            None => {
                ended = true;
                break;
            }
        }
    }
    if ended {
        walks.remove(root);
    }
    page
}

/// Keep directory enumeration cursors between sweeps: bounded work per pass,
/// including libraries with many revision directories and no catalog rows.
fn orphan_page(root: &std::path::Path) -> Vec<(String, String, PathBuf)> {
    struct Walk {
        files: std::fs::ReadDir,
        revisions: Option<(String, std::fs::ReadDir)>,
    }
    static WALKS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, Walk>>> =
        std::sync::OnceLock::new();
    let mut walks = WALKS.get_or_init(Default::default).lock().unwrap();
    if !walks.contains_key(root) {
        let Ok(files) = std::fs::read_dir(root) else {
            return vec![];
        };
        walks.insert(
            root.to_owned(),
            Walk {
                files,
                revisions: None,
            },
        );
    }
    let walk = walks.get_mut(root).unwrap();
    let mut result = Vec::new();
    let mut ended = false;
    for _ in 0..100 {
        if let Some((file, revisions)) = &mut walk.revisions {
            if let Some(Ok(entry)) = revisions.next() {
                result.push((
                    file.clone(),
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.path(),
                ));
                continue;
            }
            walk.revisions = None;
        }
        match walk.files.next() {
            Some(Ok(entry)) => {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with('.')
                    && let Ok(revisions) = std::fs::read_dir(entry.path())
                {
                    walk.revisions = Some((name, revisions));
                }
            }
            Some(Err(_)) => {}
            None => {
                ended = true;
                break;
            }
        }
    }
    if ended {
        walks.remove(root);
    }
    result
}

/// Remove previews whose revision is no longer the file's current one (or
/// whose file is gone), their sheets, and attempts' leftovers. Rows go
/// first: a removed row is never served, whatever the directory's state.
#[doc(hidden)]
pub async fn retain(app: &App) -> anyhow::Result<()> {
    {
        let _guard = app.jobs.lock().await;
        let mut tx = begin_write(&app.db).await?;
        let mut stale: Vec<(String, String)> = sqlx::query_as(
            "SELECT file_id,file_revision FROM previews p WHERE NOT EXISTS(SELECT 1 FROM media_files f WHERE f.id=p.file_id AND f.revision=p.file_revision AND f.available=1) LIMIT 100",
        )
        .fetch_all(&mut *tx)
        .await?;
        // Published rows whose sheets are not all on disk (a database
        // restored without them): withdrawn, so a request renders again.
        let cursor: String =
            sqlx::query_scalar("SELECT after_file FROM preview_retention_cursor WHERE id=1")
                .fetch_one(&mut *tx)
                .await?;
        let published: Vec<(String, String, i64, String)> =
            sqlx::query_as("SELECT file_id,file_revision,sheets,layout_identity FROM previews WHERE file_id>? ORDER BY file_id LIMIT 100")
                .bind(&cursor)
                .fetch_all(&mut *tx)
                .await?;
        let next = if published.len() < 100 {
            ""
        } else {
            published.last().map(|r| r.0.as_str()).unwrap_or("")
        };
        sqlx::query("UPDATE preview_retention_cursor SET after_file=? WHERE id=1")
            .bind(next)
            .execute(&mut *tx)
            .await?;
        for (file, revision, sheets, identity) in published {
            let dir = published_dir(app, &file, &revision);
            if layout_identity(&mut tx, &file, &revision).await?.as_deref()
                != Some(identity.as_str())
                || !(1..=20).contains(&sheets)
                || (1..=sheets).any(|n| !dir.join(format!("sheet-{n}.jpg")).is_file())
            {
                stale.push((file, revision));
            }
        }
        for (file, revision) in &stale {
            sqlx::query("DELETE FROM previews WHERE file_id=? AND file_revision=?")
                .bind(file)
                .bind(revision)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        // Published directories without a row (a publication interrupted
        // between its rename and its commit), under the lock publication
        // holds, so none is removed while being published.
        let mut orphans = Vec::new();
        for (file_name, revision_name, path) in orphan_page(&previews_root(app)) {
            let row: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM previews WHERE file_id=? AND file_revision=?")
                    .bind(&file_name)
                    .bind(&revision_name)
                    .fetch_optional(&app.db)
                    .await?;
            if row.is_none() {
                orphans.push(path);
            }
        }
        for orphan in orphans {
            let _ = tokio::fs::remove_dir_all(orphan).await;
        }
        // Publication uses this same lock, including replacement of this path.
        for (file, revision) in stale {
            let _ = tokio::fs::remove_dir_all(published_dir(app, &file, &revision)).await;
        }
    }
    // Staging of attempts no longer running.
    let staging = previews_root(app).join(".staging");
    {
        for entry in directory_page(&staging) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some((id, attempt)) = name.rsplit_once('-') else {
                continue;
            };
            let live: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM preview_jobs WHERE id=? AND attempt=? AND phase IN ('running','cancelling')",
            )
            .bind(id)
            .bind(attempt.parse::<i64>().unwrap_or(-1))
            .fetch_optional(&app.db)
            .await?;
            // A renderer still holding its witness (of this or a previous
            // process) keeps its staging until it is gone.
            let owner = entry.path().join(".owner");
            let held = tokio::task::spawn_blocking(move || {
                crate::execution::Witness::open(&owner).is_ok_and(|w| !w.released())
            })
            .await?;
            if live.is_none() && !held {
                let _ = tokio::fs::remove_dir_all(entry.path()).await;
            }
        }
    }
    Ok(())
}
