//! Trick-play previews (completion 6C): thumbnail sheets of a timeline's
//! whole single-file original, rendered by a bounded job from one exact
//! file revision, validated and published only while that revision is
//! current (core `previews`), served only to callers who may read the file,
//! and removed once the revision is no longer current.
mod artifacts;
#[cfg(test)]
mod native_tests;

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

/// A sealed pre-97 database keeps its existing capabilities, but cannot
/// mint preview artifact ACKs before its authenticated ordered upgrade.
async fn artifact_schema(conn: &mut sqlx::SqliteConnection) -> Result<bool, sqlx::Error> {
    use sha2::Digest;
    let ddl = include_str!("../../migrations/0097_preview_sheet_artifacts.sql");
    let checksum: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version=97 AND success=1")
            .fetch_optional(&mut *conn)
            .await?;
    if checksum.as_deref() != Some(sha2::Sha384::digest(ddl.as_bytes()).as_slice()) {
        return Ok(false);
    }
    let actual: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type='table' AND name='preview_sheet_artifacts'",
    )
    .fetch_optional(&mut *conn)
    .await?;
    let expected = ddl[ddl.find("CREATE TABLE").expect("artifact DDL")..]
        .trim()
        .trim_end_matches(';');
    Ok(actual
        .as_deref()
        .is_some_and(|sql| sql.trim().trim_end_matches(';') == expected))
}

async fn require_artifact_schema(conn: &mut sqlx::SqliteConnection) -> Result<(), Problem> {
    if !artifact_schema(conn).await? {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "preview_artifact_schema_unavailable",
            "Preview artifacts require an authenticated database upgrade",
        ));
    }
    Ok(())
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
    let native = super::playback::native_bound(conn).await?;
    let mut after = (String::new(), String::new());
    loop {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT v.id,f.id,f.revision,f.duration_seconds,t.duration_ms,f.tracks_json FROM timelines t JOIN media_versions v ON v.timeline_id=t.id \
             JOIN version_files b ON b.version_id=v.id AND b.part=1 JOIN media_files f ON f.id=b.file_id \
             WHERE t.id=? AND v.origin='original' AND f.generated=0 AND f.available=1 AND b.file_revision=f.revision \
             AND b.start_ms IS NULL AND b.end_ms IS NULL \
             AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1) \
             AND (? OR f.library_id IN (SELECT id FROM libraries WHERE enabled=1)) \
             AND (v.id>? OR (v.id=? AND f.id>?)) ORDER BY v.id,f.id LIMIT 100",
        )
        .bind(timeline)
        .bind(native)
        .bind(&after.0)
        .bind(&after.0)
        .bind(&after.1)
        .fetch_all(&mut *conn)
        .await?;
        let Some((version, file, ..)) = rows.last() else {
            return Ok(None);
        };
        after = (version.clone(), file.clone());
        for (version, file, revision, mut seconds, _timeline_ms, mut tracks) in rows {
            if native {
                if !super::playback::native_version_ready(conn, &version).await? {
                    continue;
                }
                let Some(facts) =
                    crate::delivery::native_previews::facts_in(conn, &file, &revision).await?
                else {
                    continue;
                };
                seconds = facts.seconds;
                tracks = facts.tracks;
            }
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
    if super::playback::native_bound(conn)
        .await
        .map_err(|e| sqlx::Error::Protocol(format!("{e:?}")))?
    {
        return crate::delivery::native_previews::facts_in(conn, file, revision)
            .await
            .map(|facts| facts.map(|facts| facts.identity()))
            .map_err(|e| sqlx::Error::Protocol(format!("{e:?}")));
    }
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
    if super::playback::native_bound(tx).await? {
        if let Some(row) = &ready {
            if !(1..=20).contains(&row.7) {
                return Err(Problem::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "preview_artifact_unqualified",
                    "Preview output has no qualified artifact inventory",
                ));
            }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preview_sheet_artifacts WHERE file_id=? AND file_revision=? AND layout_identity=? AND sheet BETWEEN 1 AND ?")
                .bind(file).bind(revision).bind(&identity).bind(row.7).fetch_one(&mut *tx).await?;
            if count != row.7 {
                return Err(Problem::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "preview_artifact_unqualified",
                    "Preview output requires a qualified rebuild",
                ));
            }
        }
    }
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
    let mut tx = super::playback::begin_catalog_application(&app, false).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    require_artifact_schema(&mut tx).await?;
    super::playback::readable_timeline(&mut tx, &principal, &timeline).await?;
    let Some((file, revision, ..)) = original(&mut tx, &principal, &timeline).await? else {
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
    let mut tx = super::playback::begin_catalog_application(&app, true).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProcessingRequest))
        .await?;
    require_artifact_schema(&mut tx).await?;
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
                .bind(caller.credential().map(|(hash, _)| hash)).bind(caller.ingress_device_id()).bind(crate::now())
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
    let (mut reply, result) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let stop = CancellationToken::new();
        let work = sheet_owned(app, caller, file, sheet, query, stop.clone());
        tokio::pin!(work);
        let outcome = tokio::select! { result=&mut work=>result, _=reply.closed()=>{stop.cancel();work.await} };
        let _ = reply.send(outcome);
    });
    result.await.map_err(Problem::internal)?
}

async fn sheet_owned(
    app: App,
    caller: Caller,
    file: String,
    sheet: u32,
    query: SheetQuery,
    stop: CancellationToken,
) -> Result<Response, Problem> {
    let lease = std::sync::Arc::new(tokio::select! {
        _=stop.cancelled()=>return Err(Problem::not_found()),
        lease=app.processing.execution.reserve(format!("preview-read:{file}"), playscale_core::work::Class::Interactive, 1)=>lease.map_err(|_| Problem::not_found())?,
    });
    let admission = std::sync::Arc::new(
        app.processing
            .artifacts
            .pin([file.clone()])
            .ok_or_else(Problem::not_found)?,
    );
    let guard = app.jobs.clone().lock_owned().await;
    let mut tx = super::playback::begin_catalog_application(&app, false).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    require_artifact_schema(&mut tx).await?;
    if !playscale_core::playback_session::readable(
        &principal,
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
    let sheets = sheets.ok_or_else(Problem::not_found)?;
    if sheet == 0 || i64::from(sheet) > sheets {
        return Err(Problem::not_found());
    }
    if app.native_catalog.is_some() {
        let facts = crate::delivery::native_previews::facts_in(&mut tx, &file, &query.revision)
            .await?
            .ok_or_else(Problem::not_found)?;
        let (writer, source_current) = crate::delivery::native_previews::validate_path_in(
            tx,
            &facts,
            lease.clone(),
            admission.clone(),
            &stop,
        )
        .await
        .map_err(|_| Problem::not_found())?;
        tx = writer;
        if !source_current {
            return Err(Problem::not_found());
        }
    }
    let artifact: Option<(String, i64)> = if app.native_catalog.is_some() {
        Some(sqlx::query_as("SELECT digest,size_bytes FROM preview_sheet_artifacts WHERE file_id=? AND file_revision=? AND sheet=? AND layout_identity=?")
            .bind(&file).bind(&query.revision).bind(i64::from(sheet)).bind(&query.layout)
            .fetch_optional(&mut *tx).await?.ok_or_else(Problem::not_found)?)
    } else {
        None
    };
    let root = previews_root(&app);
    let relative = PathBuf::from(&file)
        .join(&query.revision)
        .join(format!("sheet-{sheet}.jpg"));
    let (tx, bytes) = crate::execution::prepare(
        lease,
        &stop,
        Duration::from_secs(5),
        move || -> anyhow::Result<_> {
            let _custody = (guard, admission);
            use std::io::Read;
            let mut file = crate::scan::open_file(&root, &relative)?;
            let before = file.metadata()?;
            anyhow::ensure!(
                before.is_file() && before.len() <= core::MAX_SHEET_BYTES,
                "invalid preview sheet"
            );
            let mut bytes = Vec::new();
            std::io::Read::by_ref(&mut file)
                .take(core::MAX_SHEET_BYTES + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() as u64 <= core::MAX_SHEET_BYTES
                    && crate::scan::fingerprint(&before)
                        == crate::scan::fingerprint(&file.metadata()?),
                "preview sheet changed"
            );
            if let Some((digest, size)) = artifact {
                anyhow::ensure!(
                    size == bytes.len() as i64 && digest == super::sha256(&bytes),
                    "preview artifact does not match ACK"
                );
            }
            Ok((tx, bytes))
        },
        || {},
    )
    .await
    .ok_or_else(Problem::not_found)?
    .map_err(Problem::internal)?
    .map_err(|_| Problem::not_found())?;
    if stop.is_cancelled() {
        return Err(Problem::not_found());
    }
    tx.commit().await?;
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
    worker_inner(app, stop, None).await
}

pub(crate) async fn service_worker(
    app: App,
    stop: CancellationToken,
    intent: crate::native_service::ServiceStopIntent,
) -> anyhow::Result<()> {
    worker_inner(app, stop, Some(intent)).await
}

#[derive(Debug)]
struct PreviewReservationRejected(playscale_core::work::Rejection);
impl std::fmt::Display for PreviewReservationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "preview reservation rejected: {:?}", self.0)
    }
}
impl std::error::Error for PreviewReservationRejected {}

pub(crate) async fn reserve_execution(
    coordinator: &std::sync::Arc<crate::execution::Coordinator>,
    stop: &CancellationToken,
    intent: Option<&crate::native_service::ServiceStopIntent>,
    units: u32,
) -> Result<Option<crate::execution::Lease>, playscale_core::work::Rejection> {
    use playscale_core::work::{Class, Rejection};
    tokio::select! {
        _ = stop.cancelled() => Ok(None),
        result = coordinator.reserve(format!("preview:{}", crate::new_id()), Class::Maintenance, units) => {
            match result {
                Ok(lease) => Ok(Some(lease)),
                Err(Rejection::AdmissionClosed) if intent.is_some_and(|i| i.requested_for(coordinator)) => Ok(None),
                Err(error) => Err(error),
            }
        }
    }
}

async fn worker_inner(
    app: App,
    stop: CancellationToken,
    intent: Option<crate::native_service::ServiceStopIntent>,
) -> anyhow::Result<()> {
    let stop = stop.child_token();
    let _stop_on_drop = stop.clone().drop_guard();
    {
        let mut conn = app.db.acquire().await?;
        if !artifact_schema(&mut conn).await? {
            drop(conn);
            stop.cancelled().await;
            return Ok(());
        }
    }
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
        let Some(lease) = reserve_execution(&app.processing.execution, &stop, intent.as_ref(), 1)
            .await
            .map_err(PreviewReservationRejected)?
        else {
            return Ok(());
        };
        let lease = std::sync::Arc::new(lease);
        let started = match start(&app).await {
            Ok(job) => job,
            Err(error)
                if stop.is_cancelled()
                    && error
                        .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
                        .is_some() =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if let Some(job) = started {
            let owned_app = app.clone();
            let owned_stop = stop.clone();
            let owned_lease = lease.clone();
            tokio::spawn(async move {
                let admission = std::sync::Arc::new(
                    owned_app
                        .processing
                        .artifacts
                        .pin([job.file.clone()])
                        .ok_or_else(|| anyhow::anyhow!("preview source is being collected"))?,
                );
                let rendered = render(
                    &owned_app,
                    &job,
                    &owned_stop,
                    &owned_lease,
                    admission.clone(),
                )
                .await;
                if owned_stop.is_cancelled() {
                    return Ok(());
                }
                finish(
                    &owned_app,
                    &job,
                    rendered,
                    owned_lease.clone(),
                    admission,
                    &owned_stop,
                )
                .await
            })
            .await??;
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
    let mut tx = super::playback::begin_catalog_application_typed(app, true).await?;
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
    admission: std::sync::Arc<impl Send + Sync + 'static>,
) -> anyhow::Result<(
    Layout,
    Vec<Sheet>,
    bool,
    Option<Value>,
    std::sync::Arc<std::fs::File>,
    Vec<artifacts::Artifact>,
)> {
    let mut initial_writer = super::playback::begin_catalog_application_typed(app, true).await?;
    let native = super::playback::native_bound(&mut initial_writer)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let mut native_writer = if native {
        Some(initial_writer)
    } else {
        drop(initial_writer);
        None
    };
    let native_facts = if let Some(tx) = native_writer.as_mut() {
        let (requester, credential, ingress): (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT requester_id,credential_hash,ingress_id FROM preview_jobs WHERE id=? AND phase='running' AND attempt=?")
            .bind(&job.id).bind(job.attempt).fetch_one(&mut **tx).await?;
        let principal = crate::metadata_jobs::principal(
            tx,
            &requester,
            credential.as_deref(),
            ingress.as_deref(),
            Permission::ProcessingRequest,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let readable = playscale_core::playback_session::readable(
            &principal,
            super::content::file_facts(tx, &job.file)
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}"))?
                .as_ref(),
        );
        anyhow::ensure!(readable, "preview requester no longer authorized");
        Some(
            crate::delivery::native_previews::facts_in(tx, &job.file, &job.revision)
                .await
                .map_err(|e| anyhow::anyhow!("{e:?}"))?
                .ok_or_else(|| anyhow::anyhow!("preview original unavailable"))?,
        )
    } else {
        None
    };
    let facts: (Option<f64>, String) = if let Some(facts) = &native_facts {
        (facts.seconds, facts.tracks.clone())
    } else {
        sqlx::query_as(
            "SELECT duration_seconds,tracks_json FROM media_files WHERE id=? AND revision=?",
        )
        .bind(&job.file)
        .bind(&job.revision)
        .fetch_one(&app.db)
        .await?
    };
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
    let (input, fingerprint) = if let (Some(tx), Some(facts)) =
        (native_writer.take(), native_facts.as_ref())
    {
        crate::delivery::native_previews::open_in(tx, facts, lease.clone(), admission.clone(), stop)
            .await?
    } else {
        crate::delivery::open_source(app, &job.file, &job.revision, lease.clone(), stop)
            .await
            .map_err(|e| anyhow::anyhow!("source: {}", e.parts().1))?
    };
    let input = std::sync::Arc::new(input);
    if app.native_catalog.is_some() {
        let root = previews_root(app);
        let guard = app.jobs.clone().lock_owned().await;
        // Pool-owned transaction must outlive this borrowed App when the
        // blocking capacity walk retains custody after caller cancellation.
        let mut writer: sqlx::Transaction<'static, sqlx::Sqlite> =
            app.db.begin_with("BEGIN IMMEDIATE").await?;
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM preview_jobs WHERE phase IN ('running','cancelling')",
        )
        .fetch_one(&mut *writer)
        .await?;
        let owners = app.processing.execution.snapshot();
        // A recovered/requeued attempt may still own blocking preparation
        // before publishing its witness. Count all unsettled owners
        // conservatively and add disjoint sets (duplicates can refuse capacity),
        // rather than treating absent witness or phase as exit.
        let reserved = (active.max(1) as u64)
            .checked_add((owners.running + owners.terminating + owners.stuck.len()) as u64)
            .ok_or_else(|| anyhow::anyhow!("preview owner reservation overflow"))?;
        let custody = (lease.clone(), admission.clone(), input.clone());
        let (writer, result) = tokio::task::spawn_blocking(move || {
            let _custody = (custody, guard);
            let result = artifacts::capacity(&root, reserved);
            (writer, result)
        })
        .await?;
        writer.commit().await?;
        result?;
    }
    let staging = staging_dir(app, &job.id, job.attempt);
    // Attempts are descriptor-relative and create-only, including ancestors.
    let staging_descriptor = std::sync::Arc::new(artifacts::create_staging(
        &previews_root(app),
        &format!("{}-{}", job.id, job.attempt),
    )?);
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
    // fd 5 anchors renderer output to the created staging directory.
    let output = PathBuf::from("/dev/fd/5/thumb-%d.jpg");
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
    use std::os::fd::AsRawFd;
    let owner = PathBuf::from(format!("/dev/fd/{}/.owner", staging_descriptor.as_raw_fd()));
    let (witness, held) = crate::execution::Witness::create(&owner)?;
    #[cfg(unix)]
    crate::execution::inherit(&mut supervisor, &[&input, &held.0, &staging_descriptor]);
    lease.started(&owner)?;
    let child = supervisor.spawn();
    drop(held);
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            let retained = (input.clone(), staging_descriptor.clone(), admission.clone());
            crate::execution::confirm_exit(
                lease,
                None,
                Some(witness),
                crate::execution::TERMINATION_DEADLINE,
                move || drop(retained),
            )
            .await;
            return Err(error.into());
        }
    };
    let heartbeat = child.stdin.take();
    let deadline = tokio::time::Instant::now() + RENDER_DEADLINE;
    let status = loop {
        tokio::select! {
            status = child.wait() => break Some(status),
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
    let retained_input = input.clone();
    let retained_admission = admission.clone();
    let retained_staging = staging_descriptor.clone();
    let confirmed = crate::execution::confirm_exit(
        lease,
        (!status.as_ref().is_some_and(Result::is_ok)).then_some(child),
        Some(witness),
        crate::execution::TERMINATION_DEADLINE,
        move || {
            drop((retained_input, retained_admission, retained_staging));
        },
    )
    .await;
    anyhow::ensure!(confirmed, "renderer outlived its supervisor");
    let status = status.ok_or_else(|| anyhow::anyhow!("rendering stopped"))??;
    anyhow::ensure!(status.success(), "renderer failed");
    // The renderer read the descriptor after it was validated: observe it
    // again before trusting what it rendered.
    let checked_input = input.clone();
    let stat_custody = (lease.clone(), admission.clone());
    let unchanged = tokio::task::spawn_blocking(move || {
        let _custody = stat_custody;
        checked_input
            .metadata()
            .is_ok_and(|m| crate::scan::fingerprint(&m) == fingerprint)
    })
    .await?;
    let assembly_custody = (lease.clone(), admission.clone(), input.clone());
    let assembled = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let _custody = assembly_custody;
        artifacts::named_directory(&staging, &staging_descriptor)?;
        let output = artifacts::assemble(&staging_descriptor, &layout)?;
        artifacts::named_directory(&staging, &staging_descriptor)?;
        Ok(output)
    })
    .await??;
    let sheets = assembled.iter().map(|a| a.sheet).collect();
    Ok((
        layout,
        sheets,
        unchanged,
        native_facts.map(|f| f.commitment),
        input,
        assembled,
    ))
}

/// Publish a valid attempt's sheets, or discard them (core
/// `PublicationFacts`), and finish the job (core `jobs::transition`), under
/// the jobs lock in one write transaction with the attempt checked.
async fn finish(
    app: &App,
    job: &Started,
    rendered: anyhow::Result<(
        Layout,
        Vec<Sheet>,
        bool,
        Option<Value>,
        std::sync::Arc<std::fs::File>,
        Vec<artifacts::Artifact>,
    )>,
    lease: std::sync::Arc<crate::execution::Lease>,
    admission: std::sync::Arc<impl Send + Sync + 'static>,
    stop: &CancellationToken,
) -> anyhow::Result<()> {
    if let Err(error) = &rendered {
        tracing::warn!(job_id=%job.id, %error, "preview rendering failed");
    }
    let _guard = app.jobs.lock().await;
    let mut tx = match super::playback::begin_catalog_application_typed(app, true).await {
        Ok(tx) => tx,
        Err(error)
            if stop.is_cancelled()
                && error
                    .downcast_ref::<crate::native_catalog::OrdinaryAdmissionDraining>()
                    .is_some() =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
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
        Ok((layout, sheets, unchanged, _, _, _)) => (
            Some(*layout),
            *unchanged && core::valid_sheets(layout, sheets),
        ),
        Err(_) => (None, false),
    };
    let mut source_current = source_current;
    if app.native_catalog.is_some() {
        let current = crate::delivery::native_previews::facts_in(&mut tx, &job.file, &job.revision)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        match (current, rendered.as_ref().ok().and_then(|r| r.3.as_ref())) {
            (Some(current), Some(expected)) if &current.commitment == expected => {
                let (writer, path_current) = crate::delivery::native_previews::validate_path_in(
                    tx,
                    &current,
                    lease.clone(),
                    admission.clone(),
                    stop,
                )
                .await?;
                tx = writer;
                source_current &= path_current;
            }
            _ => source_current = false,
        }
    }
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
    let _publication_input = rendered.as_ref().ok().map(|r| r.4.clone());
    let mut publication_custody = None;
    if effects.iter().any(|e| matches!(e, Effect::Publish { .. }))
        && let Some(layout) = layout
    {
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
            // Publish only exact in-memory artifacts through held directories.
            // Collisions/refusals leave an unacknowledged orphan, never delete
            // a possibly substituted path.
            let root = previews_root(app);
            let file = job.file.clone();
            let revision = job.revision.clone();
            let (_, _, _, _, input, output) = rendered?;
            let retained = (lease.clone(), admission.clone(), input);
            let (writer, output, custody) =
                tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                    let _retained = retained;
                    let custody = artifacts::publish(&root, &file, &revision, &output)?;
                    Ok((tx, output, custody))
                })
                .await??;
            tx = writer;
            publication_custody = Some(custody);
            sqlx::query("INSERT INTO previews(file_id,file_revision,job_id,interval_ms,tile_width,tile_height,columns,rows,tiles,sheets,created_at,layout_identity) VALUES (?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id,file_revision) DO UPDATE SET job_id=excluded.job_id,interval_ms=excluded.interval_ms,tile_width=excluded.tile_width,tile_height=excluded.tile_height,columns=excluded.columns,rows=excluded.rows,tiles=excluded.tiles,sheets=excluded.sheets,created_at=excluded.created_at,layout_identity=excluded.layout_identity")
                .bind(&job.file).bind(&job.revision).bind(&job.id)
                .bind(layout.interval_ms as i64).bind(layout.tile_width).bind(layout.tile_height)
                .bind(layout.columns).bind(layout.rows).bind(layout.tiles).bind(layout.sheets)
                .bind(crate::now()).bind(&identity).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM preview_sheet_artifacts WHERE file_id=? AND file_revision=?")
                .bind(&job.file)
                .bind(&job.revision)
                .execute(&mut *tx)
                .await?;
            for (index, artifact) in output.iter().enumerate() {
                sqlx::query("INSERT INTO preview_sheet_artifacts(file_id,file_revision,sheet,layout_identity,digest,size_bytes) VALUES (?,?,?,?,?,?)")
                    .bind(&job.file).bind(&job.revision).bind((index+1) as i64)
                    .bind(&identity).bind(&artifact.digest).bind(artifact.bytes.len() as i64)
                    .execute(&mut *tx).await?;
            }
        }
    }
    sqlx::query("UPDATE preview_jobs SET phase=?,error=? WHERE id=?")
        .bind(crate::db::phase_name(next.phase))
        .bind((next.phase == jobs::Phase::Failed).then_some("preview_failed"))
        .bind(&job.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    drop(publication_custody);
    drop(_guard);
    // Staging/orphan retention owns later cleanup; no check-then-unlink here.
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
                    && entry.file_type().is_ok_and(|kind| kind.is_dir())
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
        let mut tx = super::playback::begin_catalog_application(app, true)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
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
        if app.native_catalog.is_some() {
            // ACK withdrawal is safe. Native path deletion is deferred until
            // an identity-safe cleanup owner exists; render admission counts
            // these retained bytes and bounds further output.
            return Ok(());
        }
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
