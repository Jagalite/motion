//! `/api/v2` device-owned offline downloads and offline viewing reconciliation.
//!
//! Decisions live in `playscale_core::downloads`: which exact original file a
//! download pins, its derived state (expiry is never stored), and how each
//! offline event is reconciled with the timeline viewing authority. This
//! adapter observes the facts those decisions need and commits each decided
//! change in one SQLite write transaction. Transfer, verification, local
//! publication and removal belong to the device.
use super::{
    Body, PageQuery, Problem, Scope,
    auth::Caller,
    content::file_facts,
    etag, idempotency_key, page,
    playback::{readable_timeline, usable_profile},
    viewing,
};
use crate::{App, db::begin_write, new_id, now};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use playscale_core::{
    access::{self, Idempotent, Permission, Principal},
    downloads::{
        self as core, Candidate, Code, Current, Decision, Lineage, OfflineEvent, Outcome, Pin,
        Recorded, Refusal, Request, Stored, SubtitlePolicy, Target, Tracks,
    },
    timeline_viewing::{MAX_POSITION_MS, Status},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/downloads", post(create).get(list))
        .route("/downloads/{download_id}", get(read).delete(cancel))
        .route("/downloads/{download_id}/manifest", get(manifest))
        .route("/offline/sync", post(sync))
}

impl From<Refusal> for Problem {
    fn from(r: Refusal) -> Self {
        let unsupported = |code, detail| Problem::invalid(code, detail);
        match r {
            Refusal::NotFound => Problem::not_found(),
            Refusal::PreparationUnavailable => unsupported(
                "download_preparation_unavailable",
                "Downloading a prepared rendition is not available; request the original (recipe_id null)",
            ),
            Refusal::TrackSelectionUnavailable => unsupported(
                "download_tracks_unavailable",
                "Downloads copy the complete original file; component/track pins and required subtitles are not available",
            ),
            Refusal::MultipartUnavailable => unsupported(
                "download_multipart_unavailable",
                "Only multipart or interval versions bind this timeline; they cannot be downloaded yet",
            ),
            Refusal::ContainerUnavailable => unsupported(
                "download_container_unavailable",
                "No version is an MP4, WebM or audio file the offline player can present",
            ),
            Refusal::DurationUnknown => unsupported(
                "download_duration_unknown",
                "The timeline duration is unknown, so offline progress could not be validated",
            ),
            Refusal::TooLarge => unsupported(
                "download_too_large",
                "The original is larger than a device download can hold (64 GiB)",
            ),
            Refusal::Quota => Problem::new(
                StatusCode::CONFLICT,
                "download_quota",
                "This device already holds the maximum number of downloads",
            ),
        }
    }
}

/// The caller's own device: downloads and offline history are device-owned.
fn device(principal: &Principal, requested: Option<&str>) -> Result<String, Problem> {
    let own = principal.device_id.as_deref().ok_or_else(|| {
        Problem::invalid(
            "device_required",
            "Downloads belong to a paired device; use a device credential",
        )
    })?;
    if requested.is_some_and(|r| r != own) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "device_mismatch",
            "A device can only manage its own downloads and offline history",
        ));
    }
    Ok(own.to_owned())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    revision: i64,
    principal_id: String,
    device_id: String,
    profile_id: String,
    timeline_id: String,
    timeline_revision: i64,
    version_id: String,
    file_id: String,
    file_revision: String,
    content_type: String,
    size_bytes: i64,
    duration_ms: i64,
    state: String,
}

impl Row {
    fn stored(&self) -> Stored {
        if self.state == "cancelled" {
            Stored::Cancelled
        } else {
            Stored::Ready
        }
    }
    fn pin(&self) -> Pin {
        Pin {
            timeline_revision: self.timeline_revision as u64,
            version_id: self.version_id.clone(),
            file_id: self.file_id.clone(),
            file_revision: self.file_revision.clone(),
        }
    }
}

const COLUMNS: &str = "id,revision,principal_id,device_id,profile_id,timeline_id,timeline_revision,version_id,file_id,file_revision,content_type,size_bytes,duration_ms,state";

/// A download of the caller's principal and device. Everything else is not found.
async fn owned(
    conn: &mut SqliteConnection,
    principal: &Principal,
    id: &str,
) -> Result<Row, Problem> {
    let device = device(principal, None)?;
    let row: Row = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM downloads WHERE id=? AND principal_id=? AND device_id=?"
    ))
    .bind(id)
    .bind(&principal.id)
    .bind(&device)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Problem::not_found)?;
    if !principal.may_use_profile(&row.profile_id) {
        return Err(Problem::not_found());
    }
    Ok(row)
}

/// Current facts about the pinned source, observed for the core state.
async fn observe(
    conn: &mut SqliteConnection,
    principal: &Principal,
    row: &Row,
) -> Result<Current, Problem> {
    let timeline_revision: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM timelines WHERE id=?")
            .bind(&row.timeline_id)
            .fetch_optional(&mut *conn)
            .await?;
    let bound: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         WHERE v.id=? AND v.timeline_id=? AND b.file_id=? AND b.file_revision=? \
         AND b.start_ms IS NULL AND b.end_ms IS NULL \
         AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1)",
    )
    .bind(&row.version_id)
    .bind(&row.timeline_id)
    .bind(&row.file_id)
    .bind(&row.file_revision)
    .fetch_optional(&mut *conn)
    .await?;
    let facts = file_facts(conn, &row.file_id).await?;
    let readable = match readable_timeline(conn, principal, &row.timeline_id).await {
        Ok(_) => access::admit_file_bytes(principal, facts.as_ref()).is_ok(),
        Err(p) if p.status == StatusCode::NOT_FOUND => false,
        Err(p) => return Err(p),
    };
    Ok(Current {
        timeline_revision: timeline_revision.map(|r| r as u64),
        bound: bound.is_some(),
        file_revision: facts.map(|f| f.revision),
        readable,
    })
}

fn state_name(state: core::State) -> &'static str {
    match state {
        core::State::Ready => "ready",
        core::State::Cancelled => "cancelled",
        core::State::Expired => "expired",
    }
}

fn download_json(row: &Row, state: core::State) -> Value {
    json!({
        "id": row.id,
        "revision": row.revision.to_string(),
        "device_id": row.device_id,
        "profile_id": row.profile_id,
        "timeline_id": row.timeline_id,
        "state": state_name(state),
        // The manifest is read at /downloads/{id}/manifest; no separate asset.
        "manifest_asset_id": null,
        "size_bytes": row.size_bytes.to_string(),
        "job_id": null,
    })
}

async fn current_json(
    conn: &mut SqliteConnection,
    principal: &Principal,
    row: &Row,
) -> Result<Value, Problem> {
    let current = observe(conn, principal, row).await?;
    Ok(download_json(
        row,
        core::state(row.stored(), &row.pin(), &current),
    ))
}

fn tagged(status: StatusCode, body: Value, revision: i64) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::ETAG, etag(revision.max(0) as u64));
    response
}

// ------------------------------------------------------------------ create

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackInput {
    audio_component_id: Option<String>,
    subtitle_component_id: Option<String>,
    subtitle_policy: SubtitlePolicy,
    audio_track_id: Option<String>,
    subtitle_track_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadInput {
    device_id: String,
    profile_id: String,
    timeline_id: String,
    version_id: Option<String>,
    recipe_id: Option<String>,
    tracks: TrackInput,
}

/// The offline player identifies media by container type. `.m4v` is MP4.
fn content_type(relative_path: &str) -> Option<String> {
    let guessed = mime_guess::from_path(relative_path).first()?;
    Some(match guessed.essence_str() {
        "video/x-m4v" => "video/mp4".into(),
        "audio/x-m4a" | "audio/m4a" => "audio/mp4".into(),
        other => other.into(),
    })
}

fn is_sha256(revision: &str) -> bool {
    revision.len() == 64
        && revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// version, file, current revision, bound revision, origin, generated,
/// parts, interval, available, relative path, bytes, duration seconds
type CandidateRow = (
    String,
    String,
    String,
    String,
    String,
    bool,
    i64,
    bool,
    bool,
    String,
    i64,
    Option<f64>,
);

async fn candidates(
    conn: &mut SqliteConnection,
    principal: &Principal,
    timeline: &str,
    version: Option<&str>,
    timeline_duration: Option<i64>,
) -> Result<Vec<Candidate>, Problem> {
    // A requested version is filtered before the bound. Without one, whole
    // single-part available originals come first, so unsupported or
    // unavailable versions cannot crowd an eligible one out of the window.
    let rows: Vec<CandidateRow> = sqlx::query_as(
        "SELECT v.id,f.id,f.revision,b.file_revision,v.origin,f.generated,\
         (SELECT count(*) FROM version_files o WHERE o.version_id=v.id),\
         (b.start_ms IS NOT NULL OR b.end_ms IS NOT NULL),\
         (f.available=1 AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1)),\
         f.relative_path,f.bytes,f.duration_seconds \
         FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         JOIN media_files f ON f.id=b.file_id WHERE v.timeline_id=? AND (? IS NULL OR v.id=?) \
         ORDER BY (v.origin='original' AND f.generated=0 AND f.available=1 AND b.start_ms IS NULL AND b.end_ms IS NULL \
         AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1)) DESC,v.id,f.id LIMIT 500",
    )
    .bind(timeline)
    .bind(version)
    .bind(version)
    .fetch_all(&mut *conn)
    .await?;
    let mut found = Vec::with_capacity(rows.len());
    for (
        version,
        file,
        revision,
        bound,
        origin,
        generated,
        parts,
        interval,
        available,
        path,
        bytes,
        seconds,
    ) in rows
    {
        let facts = file_facts(conn, &file).await?;
        let duration_ms = timeline_duration
            .filter(|d| *d > 0)
            .map(|d| d as u64)
            .or_else(|| {
                seconds
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .map(|s| (s * 1000.0).round() as u64)
            });
        found.push(Candidate {
            version_id: version,
            file_id: file,
            // The device verifies the SHA-256 content revision; any other
            // revision form cannot be attested.
            bound_current: bound == revision && is_sha256(&revision),
            file_revision: revision,
            original: origin == "original" && !generated,
            parts: u32::try_from(parts).unwrap_or(u32::MAX),
            interval,
            available,
            readable: access::admit_file_bytes(principal, facts.as_ref()).is_ok(),
            content_type: content_type(&path),
            size_bytes: u64::try_from(bytes).unwrap_or(0),
            duration_ms,
        });
    }
    Ok(found)
}

pub async fn create(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<DownloadInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::DownloadsManage))
        .await?;
    let device = device(&principal, Some(&input.device_id))?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createDownload",
        target: &device,
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        // Replay names the download; report its current state while it is
        // still the caller's.
        let id = stored
            .and_then(|s| s.body)
            .and_then(|b| b["id"].as_str().map(str::to_owned))
            .ok_or_else(|| Problem::internal("download receipt without an id"))?;
        let row = owned(&mut tx, &principal, &id).await?;
        let body = current_json(&mut tx, &principal, &row).await?;
        tx.commit().await?;
        let mut response = tagged(StatusCode::ACCEPTED, body, row.revision);
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
        return Ok(response);
    }
    usable_profile(&mut tx, &principal, &input.profile_id).await?;
    readable_timeline(&mut tx, &principal, &input.timeline_id).await?;
    let (timeline_revision, timeline_duration): (i64, Option<i64>) =
        sqlx::query_as("SELECT revision,duration_ms FROM timelines WHERE id=?")
            .bind(&input.timeline_id)
            .fetch_one(&mut *tx)
            .await?;
    let found = candidates(
        &mut tx,
        &principal,
        &input.timeline_id,
        input.version_id.as_deref(),
        timeline_duration,
    )
    .await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM downloads WHERE device_id=? AND principal_id=? AND state='ready'",
    )
    .bind(&device)
    .bind(&principal.id)
    .fetch_one(&mut *tx)
    .await?;
    let t = input.tracks;
    let choice = core::choose(
        &Request {
            version_id: input.version_id,
            recipe_id: input.recipe_id,
            tracks: Tracks {
                audio_component_id: t.audio_component_id,
                subtitle_component_id: t.subtitle_component_id,
                audio_track_id: t.audio_track_id,
                subtitle_track_id: t.subtitle_track_id,
                subtitle_policy: t.subtitle_policy,
            },
        },
        &found,
        usize::try_from(active).unwrap_or(usize::MAX),
    )?;
    let at = now();
    let row = Row {
        id: new_id(),
        revision: 1,
        principal_id: principal.id.clone(),
        device_id: device.clone(),
        profile_id: input.profile_id,
        timeline_id: input.timeline_id,
        timeline_revision,
        version_id: choice.version_id,
        file_id: choice.file_id,
        file_revision: choice.file_revision,
        content_type: choice.content_type,
        size_bytes: i64::try_from(choice.size_bytes).map_err(Problem::internal)?,
        duration_ms: i64::try_from(choice.duration_ms).map_err(Problem::internal)?,
        state: "ready".into(),
    };
    sqlx::query(&format!(
        "INSERT INTO downloads({COLUMNS},created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
    ))
    .bind(&row.id)
    .bind(row.revision)
    .bind(&row.principal_id)
    .bind(&row.device_id)
    .bind(&row.profile_id)
    .bind(&row.timeline_id)
    .bind(row.timeline_revision)
    .bind(&row.version_id)
    .bind(&row.file_id)
    .bind(&row.file_revision)
    .bind(&row.content_type)
    .bind(row.size_bytes)
    .bind(row.duration_ms)
    .bind(&row.state)
    .bind(at)
    .bind(at)
    .execute(&mut *tx)
    .await?;
    let ack = download_json(&row, core::State::Ready);
    scope
        .save(
            &mut tx,
            &body.digest,
            StatusCode::ACCEPTED,
            Some(&ack),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::ACCEPTED, ack, row.revision))
}

// -------------------------------------------------------------------- reads

pub async fn list(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let device = device(&caller.principal, None)?;
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let profiles: Vec<&String> = caller.principal.grant.profile_ids.iter().collect();
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM downloads WHERE device_id=? AND principal_id=? AND id>? \
         AND (? OR profile_id IN (SELECT value FROM json_each(?))) ORDER BY id LIMIT ?"
    ))
    .bind(&device)
    .bind(&caller.principal.id)
    .bind(q.after()?)
    .bind(caller.principal.is_admin())
    .bind(serde_json::to_string(&profiles).map_err(Problem::internal)?)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in &rows {
        items.push((
            row.id.clone(),
            current_json(&mut tx, &caller.principal, row).await?,
        ));
    }
    let body = page(&mut tx, &caller.principal, items, limit).await?;
    tx.commit().await?;
    Ok(Json(body).into_response())
}

pub async fn read(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let mut tx = app.db.begin().await?;
    let row = owned(&mut tx, &caller.principal, &id).await?;
    let body = current_json(&mut tx, &caller.principal, &row).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, body, row.revision))
}

/// Cancel the server demand and revoke its byte grants. Idempotent. The
/// device removes its local copy separately; nothing here can erase it.
pub async fn cancel(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::DownloadsManage))
        .await?;
    let row = owned(&mut tx, &principal, &id).await?;
    let (_, changed) = core::cancel(row.stored());
    if changed {
        sqlx::query(
            "UPDATE downloads SET state='cancelled',revision=revision+1,updated_at=? WHERE id=?",
        )
        .bind(now())
        .bind(&row.id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE content_tickets SET revoked=1 WHERE download_id=?")
        .bind(&row.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn media_url(file_id: &str, revision: &str) -> String {
    let mut url = url::Url::parse("http://motion.invalid/api/v2/media/files/").unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend([file_id, "content"]);
    url.query_pairs_mut().append_pair("revision", revision);
    format!("{}?{}", url.path(), url.query().unwrap_or_default())
}

fn extension(content_type: &str) -> &'static str {
    match content_type {
        "video/webm" => "webm",
        "audio/mp4" => "m4a",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        _ => "mp4",
    }
}

/// Exact files, checksums and the viewing base the device pins its offline
/// events to. Read again after reconciling to rebase later offline events.
pub async fn manifest(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let mut tx = app.db.begin().await?;
    let row = owned(&mut tx, &caller.principal, &id).await?;
    let current = observe(&mut tx, &caller.principal, &row).await?;
    match core::state(row.stored(), &row.pin(), &current) {
        core::State::Ready => {}
        core::State::Cancelled => {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "download_cancelled",
                "The download was cancelled",
            ));
        }
        core::State::Expired => {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "download_expired",
                "The source changed or is no longer readable; request a new download",
            ));
        }
    }
    let title: String = sqlx::query_scalar(
        "SELECT i.title FROM timelines t JOIN editions e ON e.id=t.edition_id JOIN items i ON i.id=e.item_id WHERE t.id=?",
    )
    .bind(&row.timeline_id)
    .fetch_one(&mut *tx)
    .await?;
    let view = viewing::load_view(&mut tx, &row.profile_id, &row.timeline_id).await?;
    // Read in the same snapshot as the base, so the pair names one history.
    let (server_id, restore_epoch): (String, String) =
        sqlx::query_as("SELECT server_id,restore_epoch FROM server_identity WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    // The viewing base as it was restored, when this history began with a
    // restore whose baseline was captured; unknown otherwise.
    let captured: Option<String> =
        sqlx::query_scalar("SELECT restore_epoch FROM restore_baseline_epoch WHERE singleton=1")
            .fetch_optional(&mut *tx)
            .await?;
    let restored = if captured.as_deref() == Some(restore_epoch.as_str()) {
        let row: Option<(i64, i64)> = sqlx::query_as(
            "SELECT revision,manual_epoch FROM restore_baselines WHERE profile_id=? AND timeline_id=?",
        )
        .bind(&row.profile_id)
        .bind(&row.timeline_id)
        .fetch_optional(&mut *tx)
        .await?;
        Some(row.unwrap_or((0, 0)))
    } else {
        None
    };
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        json!({
            "download_id": row.id,
            "timeline_id": row.timeline_id,
            "timeline_revision": row.timeline_revision.to_string(),
            "title": title,
            "duration_ms": row.duration_ms,
            "files": [{
                "asset_id": row.file_id,
                "relative_name": format!("media.{}", extension(&row.content_type)),
                "digest": format!("sha256:{}", row.file_revision),
                "size_bytes": row.size_bytes.to_string(),
                "media_url": media_url(&row.file_id, &row.file_revision),
                "role": "media",
            }],
            "viewing_revision": view.revision.to_string(),
            "manual_epoch": view.manual_epoch.to_string(),
            "viewing_epoch": format!("{server_id}:{restore_epoch}"),
            "restored_viewing_revision": restored.map(|(r, _)| r.to_string()),
            "restored_manual_epoch": restored.map(|(_, e)| e.to_string()),
        }),
        row.revision,
    ))
}

/// A download byte grant: the exact pinned file revision of a ready download
/// of the caller's device. Used by content access.
pub(crate) async fn ticket_file(
    conn: &mut SqliteConnection,
    principal: &Principal,
    id: &str,
) -> Result<(String, String), Problem> {
    let row = owned(conn, principal, id).await?;
    let current = observe(conn, principal, &row).await?;
    if !core::transferable(core::state(row.stored(), &row.pin(), &current)) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "download_unavailable",
            "The download is cancelled or expired",
        ));
    }
    Ok((row.file_id, row.file_revision))
}

/// Remove a profile's downloads and offline history (inside the caller's transaction).
pub(crate) async fn delete_profile(
    conn: &mut SqliteConnection,
    profile: &str,
) -> Result<(), Problem> {
    for statement in [
        "UPDATE content_tickets SET revoked=1 WHERE download_id IN (SELECT id FROM downloads WHERE profile_id=?)",
        "DELETE FROM downloads WHERE profile_id=?",
        "DELETE FROM offline_events WHERE profile_id=?",
        "DELETE FROM offline_sequences WHERE profile_id=?",
        "DELETE FROM offline_lineage WHERE profile_id=?",
    ] {
        sqlx::query(statement)
            .bind(profile)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------- reconcile

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireEvent {
    event_id: String,
    device_sequence: String,
    download_id: String,
    profile_id: String,
    timeline_id: String,
    timeline_revision: String,
    base_viewing_revision: String,
    base_manual_epoch: String,
    position_ms: u64,
    status: String,
    device_time: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncInput {
    device_id: String,
    events: Vec<WireEvent>,
    /// The device's own compaction point; see `core::retain`.
    #[serde(default)]
    acknowledged_through: Option<String>,
    /// Continue the device's sequence on a restored server; see
    /// `core::reanchor`.
    #[serde(default)]
    reanchor: Option<ReanchorInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReanchorInput {
    from_viewing_epoch: String,
    through: String,
}

const MAX_BATCH: usize = 200;

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn parse(e: &WireEvent) -> Option<OfflineEvent> {
    let status = match e.status.as_str() {
        "playing" => Status::Playing,
        "paused" => Status::Paused,
        "ended" => Status::Ended,
        "stopped" => Status::Stopped,
        _ => return None,
    };
    let sequence = viewing::decimal(&e.device_sequence).filter(|s| *s >= 1)?;
    (valid_id(&e.event_id)
        && valid_id(&e.download_id)
        && valid_id(&e.timeline_id)
        && e.position_ms <= MAX_POSITION_MS
        && !e.device_time.is_empty()
        && e.device_time.len() <= 64)
        .then_some(())?;
    Some(OfflineEvent {
        id: e.event_id.clone(),
        sequence,
        download_id: e.download_id.clone(),
        timeline_id: e.timeline_id.clone(),
        timeline_revision: viewing::decimal(&e.timeline_revision)?,
        base_viewing_revision: viewing::decimal(&e.base_viewing_revision)?,
        base_manual_epoch: viewing::decimal(&e.base_manual_epoch)?,
        position_ms: e.position_ms,
        status,
        device_time: e.device_time.clone(),
    })
}

#[derive(Serialize, Deserialize)]
struct StoredEvent {
    event: OfflineEvent,
}

fn outcome_name(o: Outcome) -> &'static str {
    match o {
        Outcome::Applied => "applied",
        Outcome::HistoryOnly => "history_only",
        Outcome::Rejected => "rejected",
    }
}

fn outcome_of(s: &str) -> Option<Outcome> {
    Some(match s {
        "applied" => Outcome::Applied,
        "history_only" => Outcome::HistoryOnly,
        "rejected" => Outcome::Rejected,
        _ => return None,
    })
}

fn code_of(s: &str) -> Option<Code> {
    Some(match s {
        "not_found" => Code::NotFound,
        "invalid_event" => Code::InvalidEvent,
        "invalid_position" => Code::InvalidPosition,
        "timeline_changed" => Code::TimelineChanged,
        "manual_override_newer" => Code::ManualOverrideNewer,
        "viewing_newer" => Code::ViewingNewer,
        _ => return None,
    })
}

async fn prior(
    conn: &mut SqliteConnection,
    device: &str,
    profile: &str,
    event: &OfflineEvent,
) -> Result<Vec<Recorded>, Problem> {
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT event_json,outcome,code FROM offline_events WHERE device_id=? AND profile_id=? AND (sequence=? OR event_id=?)",
    )
    .bind(device)
    .bind(profile)
    .bind(event.sequence as i64)
    .bind(&event.id)
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|(json, outcome, code)| {
            Ok(Recorded {
                event: serde_json::from_str::<StoredEvent>(&json)
                    .map_err(Problem::internal)?
                    .event,
                outcome: outcome_of(&outcome).ok_or_else(|| Problem::internal("bad outcome"))?,
                code: code.as_deref().and_then(code_of),
            })
        })
        .collect()
}

/// The timeline as the event's download saw it. Without a curated timeline
/// duration, the effective duration is the one the event's own download (the
/// copy that was played) was authorized with: the probed duration of its
/// pinned original. A download of another device, profile or timeline
/// supplies nothing, and an event without an effective duration has no
/// target (`not_found`).
async fn target(
    conn: &mut SqliteConnection,
    principal: &Principal,
    device: &str,
    profile: &str,
    event: &OfflineEvent,
) -> Result<Option<Target>, Problem> {
    match readable_timeline(conn, principal, &event.timeline_id).await {
        Ok(_) => {}
        Err(p) if p.status == StatusCode::NOT_FOUND => return Ok(None),
        Err(p) => return Err(p),
    }
    let (revision, duration): (i64, Option<i64>) =
        sqlx::query_as("SELECT revision,duration_ms FROM timelines WHERE id=?")
            .bind(&event.timeline_id)
            .fetch_one(&mut *conn)
            .await?;
    let duration = match duration.filter(|d| *d > 0) {
        Some(d) => Some(d),
        None => sqlx::query_scalar(
            "SELECT duration_ms FROM downloads WHERE id=? AND device_id=? AND profile_id=? AND timeline_id=?",
        )
        .bind(&event.download_id)
        .bind(device)
        .bind(profile)
        .bind(&event.timeline_id)
        .fetch_optional(&mut *conn)
        .await?,
    };
    // Without an effective duration the event cannot be validated (an
    // unknown or foreign download names none): it is not reconcilable here.
    let Some(duration) = duration.filter(|d| *d > 0) else {
        return Ok(None);
    };
    Ok(Some(Target {
        timeline_revision: revision as u64,
        duration_ms: Some(duration as u64),
    }))
}

async fn lineage(
    conn: &mut SqliteConnection,
    device: &str,
    profile: &str,
    timeline: &str,
) -> Result<Option<Lineage>, Problem> {
    let row: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT base_viewing_revision,base_manual_epoch,produced_revision FROM offline_lineage WHERE device_id=? AND profile_id=? AND timeline_id=?",
    )
    .bind(device)
    .bind(profile)
    .bind(timeline)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|(b, e, p)| Lineage {
        base_viewing_revision: b as u64,
        base_manual_epoch: e as u64,
        produced_revision: p as u64,
    }))
}

/// Reconcile an ordered batch for one device and profile. Each event is
/// decided against the state its predecessor left, inside one transaction.
/// A gap or conflicting reuse is reported and consumes nothing; every other
/// event consumes its sequence exactly once, so the device can compact its
/// log through `next_device_sequence - 1`.
pub async fn sync(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<SyncInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let input = body.value;
    if input.events.is_empty() || input.events.len() > MAX_BATCH {
        return Err(Problem::invalid(
            "invalid_batch",
            "Send between 1 and 200 events",
        ));
    }
    let profile = input.events[0].profile_id.clone();
    if input.events.iter().any(|e| e.profile_id != profile) {
        return Err(Problem::invalid(
            "mixed_profiles",
            "One batch reconciles one profile's device log",
        ));
    }
    let events: Vec<OfflineEvent> = input
        .events
        .iter()
        .map(parse)
        .collect::<Option<_>>()
        .ok_or_else(|| Problem::invalid("invalid_event", "An offline event is malformed"))?;
    let reanchor = match &input.reanchor {
        None => None,
        Some(r) => Some((
            r.from_viewing_epoch.clone(),
            viewing::decimal(&r.through).ok_or_else(|| {
                Problem::invalid(
                    "invalid_reanchor",
                    "reanchor.through must be a decimal sequence",
                )
            })?,
        )),
    };
    let acknowledged = match &input.acknowledged_through {
        None => None,
        Some(v) => Some(viewing::decimal(v).ok_or_else(|| {
            Problem::invalid(
                "invalid_acknowledgement",
                "acknowledged_through must be a decimal sequence",
            )
        })?),
    };
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ViewingWrite))
        .await?;
    let device = device(&principal, Some(&input.device_id))?;
    // The batch's bases belong to one server history (the manifest's
    // viewing_epoch). After a restore or server replacement nothing is
    // consumed or applied; the device keeps its log for explicit recovery.
    viewing::require_history_identity(&mut tx, &headers).await?;
    // The whole batch waits (and the device keeps its log) while the profile
    // is unusable; events are never consumed for a profile the caller lacks.
    usable_profile(&mut tx, &principal, &profile).await?;
    let completion = viewing::completion_percent(&mut tx, &profile).await?;
    let (mut next, mut pruned): (i64, i64) = sqlx::query_as(
        "SELECT next_sequence,pruned_through FROM offline_sequences WHERE device_id=? AND profile_id=?",
    )
    .bind(&device)
    .bind(&profile)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or((1, 0));
    // A device whose compaction point belongs to an earlier history of this
    // server continues its sequence after it (`core::reanchor`), in this
    // transaction, before retention and the batch are decided.
    if let Some((from, through)) = &reanchor {
        let current: (String, String) =
            sqlx::query_as("SELECT server_id,restore_epoch FROM server_identity WHERE singleton=1")
                .fetch_one(&mut *tx)
                .await?;
        let (moved, retention) = core::reanchor(
            (&current.0, &current.1),
            from,
            *through,
            next as u64,
            core::Retention::server(pruned as u64),
        )
        .map_err(|refusal| {
            Problem::invalid(
                "invalid_reanchor",
                match refusal {
                    core::ReanchorRefusal::Invalid => "The re-anchor is malformed",
                    core::ReanchorRefusal::OtherServer => {
                        "A re-anchor continues only this server's own earlier history"
                    }
                    core::ReanchorRefusal::SameHistory => {
                        "The current history's sequence is never skipped"
                    }
                },
            )
        })?;
        if moved as i64 != next || retention.pruned_through as i64 != pruned {
            sqlx::query(
                "DELETE FROM offline_events WHERE device_id=? AND profile_id=? AND sequence<=?",
            )
            .bind(&device)
            .bind(&profile)
            .bind(retention.pruned_through as i64)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO offline_sequences(device_id,profile_id,next_sequence,pruned_through) VALUES (?,?,?,?) ON CONFLICT(device_id,profile_id) DO UPDATE SET next_sequence=excluded.next_sequence,pruned_through=excluded.pruned_through")
                .bind(&device)
                .bind(&profile)
                .bind(moved as i64)
                .bind(retention.pruned_through as i64)
                .execute(&mut *tx)
                .await?;
            next = moved as i64;
            pruned = retention.pruned_through as i64;
        }
    }
    // Records the device has compacted are never resent by it: prune them,
    // in this transaction, before deciding the batch against the new floor.
    let retention = core::retain(
        next as u64,
        core::Retention::server(pruned as u64),
        acknowledged,
    )
    .map_err(|_| {
        Problem::invalid(
            "acknowledgement_ahead",
            "acknowledged_through is beyond the events this server consumed",
        )
    })?;
    if retention.pruned_through as i64 > pruned {
        sqlx::query(
            "DELETE FROM offline_events WHERE device_id=? AND profile_id=? AND sequence<=?",
        )
        .bind(&device)
        .bind(&profile)
        .bind(retention.pruned_through as i64)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE offline_sequences SET pruned_through=? WHERE device_id=? AND profile_id=?",
        )
        .bind(retention.pruned_through as i64)
        .bind(&device)
        .bind(&profile)
        .execute(&mut *tx)
        .await?;
    }
    let mut results = Vec::with_capacity(events.len());
    for event in &events {
        let prior = prior(&mut tx, &device, &profile, event).await?;
        let target = target(&mut tx, &principal, &device, &profile, event).await?;
        let view = viewing::load_view(&mut tx, &profile, &event.timeline_id).await?;
        let chain = lineage(&mut tx, &device, &profile, &event.timeline_id).await?;
        let decision = core::reconcile(
            retention,
            next as u64,
            &prior,
            event,
            target.as_ref(),
            &view,
            chain.as_ref(),
            completion,
        );
        let current = |v: &playscale_core::timeline_viewing::View| {
            target
                .as_ref()
                .map(|_| viewing::view_json(&profile, &event.timeline_id, v))
        };
        let result = match decision {
            Decision::Duplicate(r) => json!({
                "event_id": event.id, "outcome": "duplicate",
                "code": outcome_name(r.outcome), "current_viewing": current(&view),
            }),
            Decision::Conflict => json!({
                "event_id": event.id, "outcome": "rejected", "code": "event_conflict",
                "current_viewing": null,
            }),
            Decision::SequenceGap => json!({
                "event_id": event.id, "outcome": "rejected", "code": "sequence_gap",
                "current_viewing": null,
            }),
            Decision::Acknowledged => json!({
                "event_id": event.id, "outcome": "rejected", "code": "sequence_acknowledged",
                "current_viewing": null,
            }),
            Decision::RetentionFull => json!({
                "event_id": event.id, "outcome": "rejected", "code": "acknowledgement_required",
                "current_viewing": null,
            }),
            Decision::Consumed {
                recorded,
                view: applied,
                fenced,
                lineage: chained,
            } => {
                sqlx::query("INSERT INTO offline_events(device_id,profile_id,sequence,event_id,timeline_id,event_json,outcome,code,created_at) VALUES (?,?,?,?,?,?,?,?,?)")
                    .bind(&device)
                    .bind(&profile)
                    .bind(event.sequence as i64)
                    .bind(&event.id)
                    .bind(&event.timeline_id)
                    .bind(serde_json::to_string(&StoredEvent { event: event.clone() }).map_err(Problem::internal)?)
                    .bind(outcome_name(recorded.outcome))
                    .bind(recorded.code.map(Code::as_str))
                    .bind(now())
                    .execute(&mut *tx)
                    .await?;
                next += 1;
                sqlx::query("INSERT INTO offline_sequences(device_id,profile_id,next_sequence) VALUES (?,?,?) ON CONFLICT(device_id,profile_id) DO UPDATE SET next_sequence=excluded.next_sequence")
                    .bind(&device)
                    .bind(&profile)
                    .bind(next)
                    .execute(&mut *tx)
                    .await?;
                if let Some(v) = &applied {
                    viewing::store_view(&mut tx, &profile, &event.timeline_id, v).await?;
                    viewing::supersede(&mut tx, fenced.as_deref()).await?;
                }
                if let Some(l) = chained {
                    sqlx::query("INSERT INTO offline_lineage(device_id,profile_id,timeline_id,base_viewing_revision,base_manual_epoch,produced_revision) VALUES (?,?,?,?,?,?) \
                         ON CONFLICT(device_id,profile_id,timeline_id) DO UPDATE SET base_viewing_revision=excluded.base_viewing_revision,base_manual_epoch=excluded.base_manual_epoch,produced_revision=excluded.produced_revision")
                        .bind(&device)
                        .bind(&profile)
                        .bind(&event.timeline_id)
                        .bind(l.base_viewing_revision as i64)
                        .bind(l.base_manual_epoch as i64)
                        .bind(l.produced_revision as i64)
                        .execute(&mut *tx)
                        .await?;
                }
                json!({
                    "event_id": event.id,
                    "outcome": outcome_name(recorded.outcome),
                    "code": recorded.code.map(Code::as_str),
                    "current_viewing": current(applied.as_ref().unwrap_or(&view)),
                })
            }
        };
        results.push(result);
    }
    tx.commit().await?;
    Ok(Json(json!({"results": results, "next_device_sequence": next.to_string()})).into_response())
}
