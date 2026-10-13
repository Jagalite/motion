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
    idempotency_key, page,
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
use std::{collections::HashMap, sync::Arc, time::Duration};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/downloads", post(create).get(list))
        .route("/downloads/{download_id}", get(read).delete(cancel))
        .route("/downloads/{download_id}/manifest", get(manifest))
        .route("/downloads/{download_id}/subtitles", get(subtitles))
        .route(
            "/downloads/{download_id}/transfer",
            axum::routing::put(report_transfer),
        )
        .route("/offline/sync", post(sync))
}

impl From<Refusal> for Problem {
    fn from(r: Refusal) -> Self {
        let unsupported = |code, detail| Problem::invalid(code, detail);
        match r {
            Refusal::NotFound => Problem::not_found(),
            Refusal::PreparationUnavailable => unsupported(
                "download_preparation_unavailable",
                "No ready prepared version of that recipe carries exactly this selection; prepare one first, or request the original (recipe_id null)",
            ),
            Refusal::TrackSelectionUnavailable => unsupported(
                "download_tracks_unavailable",
                "An original is downloaded whole; to keep only chosen tracks, download a prepared version (recipe_id)",
            ),
            Refusal::SubtitleUnavailable => unsupported(
                "download_subtitle_unavailable",
                "The selected subtitle cannot be saved for offline playback (only text subtitles can); choose another or none",
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
    subtitle_track_id: Option<String>,
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

const COLUMNS: &str = "id,revision,principal_id,device_id,profile_id,timeline_id,timeline_revision,version_id,file_id,file_revision,content_type,size_bytes,duration_ms,state,subtitle_track_id";

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
    // A prepared rendition is bound only while it is current for its source
    // (`renditions::available`); an original has no rendition record.
    let rendition: Option<(bool, String, String, String, String)> = sqlx::query_as(
        "SELECT (f.available=1),f.revision,r.file_revision,s.revision,r.source_revision FROM renditions r \
         JOIN media_files f ON f.id=r.file_id JOIN media_files s ON s.id=r.source_file_id \
         WHERE r.file_id=? AND r.source='playscale' AND f.generated=1",
    )
    .bind(&row.file_id)
    .fetch_optional(&mut *conn)
    .await?;
    let ready = super::processing::output_rendition_status(conn, &row.file_id).await?;
    let bound = bound.filter(|_| {
        rendition.as_ref().is_none_or(
            |(available, revision, registered, source, registered_source)| {
                playscale_core::renditions::available(
                    *available,
                    revision,
                    registered,
                    source,
                    registered_source,
                ) && ready.is_none_or(|status| status == "ready")
            },
        )
    });
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

/// The device's last report about its copy, as stored.
type Transfer = (i64, i64, String, i64, i64);

async fn transfer(conn: &mut SqliteConnection, id: &str) -> Result<Option<Transfer>, Problem> {
    Ok(sqlx::query_as(
        "SELECT sequence,download_revision,state,bytes_received,reported_at FROM download_transfers WHERE download_id=?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

fn transfer_json(transfer: Option<&Transfer>) -> Value {
    transfer.map_or(Value::Null, |(sequence, revision, state, bytes, at)| {
        json!({
            "sequence": sequence.to_string(),
            "download_revision": revision.to_string(),
            "state": state,
            "bytes_received": bytes.to_string(),
            "reported_at": super::timestamp(*at),
        })
    })
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
        "subtitle_track_id": row.subtitle_track_id,
        // Filled from the device's reports (`current_json`); none yet.
        "device_transfer": null,
    })
}

async fn current_json(
    conn: &mut SqliteConnection,
    principal: &Principal,
    row: &Row,
) -> Result<Value, Problem> {
    let current = observe(conn, principal, row).await?;
    let mut body = download_json(row, core::state(row.stored(), &row.pin(), &current));
    let subtitle_size: Option<i64> = sqlx::query_scalar(
        "SELECT size_bytes FROM download_subtitle_artifacts WHERE download_id=? AND state='ready'",
    )
    .bind(&row.id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(total) = core::transfer_bytes(
        row.size_bytes as u64,
        row.subtitle_track_id.is_some(),
        subtitle_size.map(|n| n as u64),
    ) {
        body["size_bytes"] = json!(total.to_string());
    }
    // As the device reported it; never derived from the server's state.
    body["device_transfer"] = transfer_json(transfer(conn, &row.id).await?.as_ref());
    Ok(body)
}

fn tagged(status: StatusCode, body: Value, _revision: i64) -> Response {
    let validator = HeaderValue::from_str(&format!(
        "\"sha256-{}\"",
        super::sha256(body.to_string().as_bytes())
    ))
    .expect("hex digest is a header");
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, validator);
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

/// The subtitle tracks of a file's revision that convert to WebVTT without
/// losing meaning (core `delivery::sidecar_subtitle`), by track ID (`sN` is
/// the file's N-th subtitle stream).
async fn text_subtitles(conn: &mut SqliteConnection, file: &str) -> Result<Vec<String>, Problem> {
    let tracks: Option<String> =
        sqlx::query_scalar("SELECT tracks_json FROM media_files WHERE id=?")
            .bind(file)
            .fetch_optional(&mut *conn)
            .await?;
    let tracks: Vec<crate::db::Track> = tracks
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    Ok(tracks
        .iter()
        .filter(|t| t.kind == "subtitle")
        .enumerate()
        .filter(|(_, t)| playscale_core::delivery::sidecar_subtitle(&t.codec))
        .map(|(n, _)| format!("s{n}"))
        .collect())
}

// Retain one example of each refusal, or the first eligible candidate.
// This preserves the core's refusal precedence without retaining every row.
fn retain_candidate(found: &mut Vec<Candidate>, candidate: Candidate, request: &Request) -> bool {
    let decision = core::choose(request, std::slice::from_ref(&candidate), 0);
    if decision.is_ok() {
        found.clear();
        found.push(candidate);
        return true;
    }
    if !found
        .iter()
        .any(|c| core::choose(request, std::slice::from_ref(c), 0).err() == decision.clone().err())
    {
        found.push(candidate);
    }
    false
}

async fn candidates(
    conn: &mut SqliteConnection,
    principal: &Principal,
    timeline: &str,
    version: Option<&str>,
    timeline_duration: Option<i64>,
    request: &Request,
) -> Result<Vec<Candidate>, Problem> {
    // A requested version is filtered before the bound. Without one, whole
    // single-part available originals come first, so unsupported or
    // unavailable versions cannot crowd an eligible one out of the window.
    let mut found = Vec::new();
    let mut offset = 0i64;
    loop {
        let rows: Vec<CandidateRow> = sqlx::query_as(
        "SELECT v.id,f.id,f.revision,b.file_revision,v.origin,f.generated,\
         (SELECT count(*) FROM version_files o WHERE o.version_id=v.id),\
         (b.start_ms IS NOT NULL OR b.end_ms IS NOT NULL),\
         (f.available=1 AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1)),\
         f.relative_path,f.bytes,f.duration_seconds \
         FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         JOIN media_files f ON f.id=b.file_id WHERE v.timeline_id=? AND (? IS NULL OR v.id=?) \
         ORDER BY (v.origin='original' AND f.generated=0 AND f.available=1 AND b.start_ms IS NULL AND b.end_ms IS NULL \
         AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1)) DESC,v.id,f.id LIMIT 200 OFFSET ?",
    )
    .bind(timeline)
    .bind(version)
    .bind(version)
    .bind(offset)
    .fetch_all(&mut *conn)
    .await?;
        let count = rows.len();
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
            let subtitles = text_subtitles(conn, &file).await?;
            let duration_ms = timeline_duration
                .filter(|d| *d > 0)
                .map(|d| d as u64)
                .or_else(|| {
                    seconds
                        .filter(|s| s.is_finite() && *s > 0.0)
                        .map(|s| (s * 1000.0).round() as u64)
                });
            let candidate = Candidate {
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
                prepared: None,
                text_subtitles: subtitles,
            };
            if retain_candidate(&mut found, candidate, request) {
                return Ok(found);
            }
        }
        if count < 200 {
            break;
        }
        offset += count as i64;
    }
    Ok(found)
}

/// A prepared rendition bound to the timeline (a generated version) as a
/// download candidate: its recipe, whether it is still current for its
/// source (`renditions::available`), and whether it carries exactly the
/// requested selection, resolved on its source's exact revision
/// (`processing::selection`, then `processing::carries`).
#[allow(clippy::too_many_arguments)]
async fn prepared_candidates(
    app: &App,
    conn: &mut SqliteConnection,
    principal: &Principal,
    timeline: &str,
    version: Option<&str>,
    recipe: &str,
    timeline_duration: Option<i64>,
    tracks: &TrackInput,
    request: &Request,
) -> Result<Vec<Candidate>, Problem> {
    type Row = (
        String,
        String,
        String,
        String,
        bool,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        Option<f64>,
    );
    // Only the requested recipe, newest first, read page by page until a
    // rendition carrying the selection is found or every one was examined.
    let mut found = Vec::new();
    let mut page = 0i64;
    loop {
        // Cache only this bounded page, never every historical source revision.
        let mut selections: HashMap<(String, String), Option<super::processing::Tracks>> =
            HashMap::new();
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT v.id,f.id,f.revision,b.file_revision,(f.available=1 AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1)),\
             r.file_revision,s.id,s.revision,r.source_revision,j.recipe,p.tracks_json,f.relative_path,f.bytes,f.duration_seconds \
             FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
             JOIN media_files f ON f.id=b.file_id JOIN renditions r ON r.file_id=f.id AND r.source='playscale' \
             JOIN processing_jobs j ON j.id=r.external_id JOIN preparations p ON p.job_id=j.id \
             JOIN media_files s ON s.id=r.source_file_id \
             WHERE v.timeline_id=? AND v.origin='generated' AND f.generated=1 AND (? IS NULL OR v.id=?) AND j.recipe=? \
             AND b.start_ms IS NULL AND b.end_ms IS NULL \
             AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1) \
             ORDER BY r.updated_at DESC,v.id LIMIT ? OFFSET ?",
        )
        .bind(timeline)
        .bind(version)
        .bind(version)
        .bind(recipe)
        .bind(PREPARED_PAGE)
        .bind(page * PREPARED_PAGE)
        .fetch_all(&mut *conn)
        .await?;
        let complete = (rows.len() as i64) < PREPARED_PAGE;
        for (
            version,
            file,
            revision,
            bound,
            available,
            registered,
            source,
            source_revision,
            registered_source,
            recipe,
            recorded,
            path,
            bytes,
            seconds,
        ) in rows
        {
            // Ready by the same facts listRenditions reports
            // (`processing::rendition_status`), and its bytes as registered.
            let current = playscale_core::renditions::available(
                available,
                &revision,
                &registered,
                &source_revision,
                &registered_source,
            ) && super::processing::output_rendition_status(conn, &file).await?
                == Some("ready");
            // The selection on the source revision the rendition was prepared
            // from; a selection it cannot express there is carried by nothing.
            let streams: Vec<crate::db::Track> =
                sqlx::query_scalar::<_, String>("SELECT tracks_json FROM media_files WHERE id=?")
                    .bind(&source)
                    .fetch_one(&mut *conn)
                    .await
                    .map(|json| serde_json::from_str(&json).unwrap_or_default())?;
            let requested = super::processing::Tracks {
                audio_component_id: tracks.audio_component_id.clone(),
                subtitle_component_id: tracks.subtitle_component_id.clone(),
                subtitle_policy: match tracks.subtitle_policy {
                    SubtitlePolicy::Off => "off",
                    SubtitlePolicy::Auto => "auto",
                    SubtitlePolicy::Require => "require",
                }
                .into(),
                audio_track_id: tracks.audio_track_id.clone(),
                subtitle_track_id: tracks.subtitle_track_id.clone(),
            };
            let selection_key = (source.clone(), source_revision.clone());
            let resolved = if let Some(cached) = selections.get(&selection_key) {
                cached.clone()
            } else {
                let resolved = super::processing::selection(
                    app,
                    conn,
                    timeline,
                    &super::processing::FileRef {
                        file_id: source.clone(),
                        file_revision: source_revision.clone(),
                    },
                    &streams,
                    requested,
                )
                .await
                .ok();
                selections.insert(selection_key, resolved.clone());
                resolved
            };
            let carries = match resolved {
                Some(selection) => {
                    let ordinal = |id: Option<&String>, prefix: char| {
                        id.and_then(|i| i.strip_prefix(prefix)?.parse::<u32>().ok())
                    };
                    let record: Option<super::processing::Tracks> =
                        serde_json::from_str(&recorded).ok();
                    let default_audio = streams.iter().any(|t| t.kind == "audio").then_some(0);
                    let audio = ordinal(selection.audio_track_id.as_ref(), 'a').or(default_audio);
                    let wanted = playscale_core::processing::Wanted {
                        audio,
                        audio_chosen: selection.audio_track_id.is_some(),
                        default_audio,
                        subtitle: match ordinal(selection.subtitle_track_id.as_ref(), 's') {
                            Some(n) => playscale_core::processing::WantedSubtitle::Text(n),
                            None => playscale_core::processing::WantedSubtitle::None,
                        },
                    };
                    let carried = match record {
                        Some(r) => playscale_core::processing::Carried::Encoded(
                            playscale_core::processing::Encoded {
                                audio: ordinal(r.audio_track_id.as_ref(), 'a'),
                                subtitle: ordinal(r.subtitle_track_id.as_ref(), 's'),
                            },
                        ),
                        None => playscale_core::processing::Carried::Unreadable,
                    };
                    playscale_core::processing::carries(carried, wanted)
                }
                None => false,
            };
            let facts = file_facts(conn, &file).await?;
            let subtitles = text_subtitles(conn, &file).await?;
            let candidate = Candidate {
                version_id: version,
                file_id: file,
                bound_current: current && bound == revision && is_sha256(&revision),
                file_revision: revision,
                original: false,
                parts: 1,
                interval: false,
                available,
                readable: access::admit_file_bytes(principal, facts.as_ref()).is_ok(),
                content_type: content_type(&path),
                size_bytes: u64::try_from(bytes).unwrap_or(0),
                duration_ms: timeline_duration
                    .filter(|d| *d > 0)
                    .map(|d| d as u64)
                    .or_else(|| {
                        seconds
                            .filter(|s| s.is_finite() && *s > 0.0)
                            .map(|s| (s * 1000.0).round() as u64)
                    }),
                prepared: Some(playscale_core::downloads::Prepared {
                    recipe_id: recipe,
                    carries_selection: carries,
                }),
                text_subtitles: subtitles,
            };
            if retain_candidate(&mut found, candidate, request) {
                return Ok(found);
            }
        }
        if complete {
            break;
        }
        page += 1;
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
    let request = Request {
        version_id: input.version_id.clone(),
        recipe_id: input.recipe_id.clone(),
        tracks: Tracks {
            audio_component_id: input.tracks.audio_component_id.clone(),
            subtitle_component_id: input.tracks.subtitle_component_id.clone(),
            audio_track_id: input.tracks.audio_track_id.clone(),
            subtitle_track_id: input.tracks.subtitle_track_id.clone(),
            subtitle_policy: input.tracks.subtitle_policy,
        },
    };
    let found = match input.recipe_id.as_deref() {
        None => {
            candidates(
                &mut tx,
                &principal,
                &input.timeline_id,
                input.version_id.as_deref(),
                timeline_duration,
                &request,
            )
            .await?
        }
        Some(recipe) => {
            // The same rule as preparing it: a kept subtitle is explicit and
            // required (`processing::kept_subtitle`).
            let t = &input.tracks;
            super::processing::kept_subtitle_problem(
                match t.subtitle_policy {
                    SubtitlePolicy::Off => "off",
                    SubtitlePolicy::Auto => "auto",
                    SubtitlePolicy::Require => "require",
                },
                t.subtitle_component_id.is_some() || t.subtitle_track_id.is_some(),
            )?;
            prepared_candidates(
                &app,
                &mut tx,
                &principal,
                &input.timeline_id,
                input.version_id.as_deref(),
                recipe,
                timeline_duration,
                &input.tracks,
                &request,
            )
            .await?
        }
    };
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
        subtitle_track_id: choice.subtitle_track_id,
    };
    sqlx::query(&format!(
        "INSERT INTO downloads({COLUMNS},created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
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
    .bind(&row.subtitle_track_id)
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
    // Its subtitle file is no longer served. Released in the background:
    // a conversion still running holds the file's lock until it has seen
    // the cancellation.
    tokio::spawn(release_subtitles(app.clone()));
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Where a download's subtitle file is kept: beside the processing and
/// delivery directories, never inside them (their cleanup owns those).
fn subtitles_path(app: &App, download_id: &str) -> std::path::PathBuf {
    let root = &app.processing.root;
    root.parent()
        .unwrap_or(root)
        .join("download-subtitles")
        .join(format!("{download_id}.vtt"))
}

/// The lock ordering a download's subtitle file: conversion and
/// publication hold it from their state check to the rename, and removal
/// holds it too, so a cancelled download's file is never published after
/// its removal, and one download converts at most once at a time.
fn subtitles_lock(path: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
    type Locks = HashMap<std::path::PathBuf, std::sync::Weak<tokio::sync::Mutex<()>>>;
    static LOCKS: std::sync::LazyLock<std::sync::Mutex<Locks>> =
        std::sync::LazyLock::new(Default::default);
    let mut locks = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(std::sync::Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(path.to_owned(), Arc::downgrade(&lock));
    lock
}

/// Download subtitle conversions started by this process (bounded
/// instrumentation; tests assert no conversion is duplicated).
static CONVERSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[doc(hidden)]
pub fn subtitle_conversions() -> u64 {
    CONVERSIONS.load(std::sync::atomic::Ordering::SeqCst)
}

/// The conversion error a remembered failure replays.
fn failure(expired: bool) -> crate::api::ApiError {
    if expired {
        crate::api::ApiError::conflict("source_revision_changed", "Request a new download")
    } else {
        crate::api::ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "subtitles_unavailable",
            "The subtitle track could not be converted; try again",
        )
    }
}

/// Record a failed conversion as one would (tests order waiting reads
/// against a failure with it).
#[doc(hidden)]
pub fn remember_subtitle_failure(app: &App, download_id: &str, expired: bool) {
    failures().lock().unwrap_or_else(|e| e.into_inner()).insert(
        subtitles_path(app, download_id),
        (std::time::Instant::now(), expired),
    );
}

/// How a download's last conversion failed, and when, by its file path:
/// reads that were already waiting share that outcome (an expired source
/// stays expired) instead of each converting again. Kept only for failing
/// downloads, and briefly.
fn failures() -> &'static std::sync::Mutex<HashMap<std::path::PathBuf, (std::time::Instant, bool)>>
{
    type Failures = HashMap<std::path::PathBuf, (std::time::Instant, bool)>;
    static FAILURES: std::sync::LazyLock<std::sync::Mutex<Failures>> =
        std::sync::LazyLock::new(Default::default);
    &FAILURES
}

/// Hold a download's subtitle lock (tests order a cancellation against a
/// conversion with it).
#[doc(hidden)]
pub async fn hold_subtitles(app: &App, download_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
    subtitles_lock(&subtitles_path(app, download_id))
        .lock_owned()
        .await
}

async fn active(app: &App, download_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT 1 FROM downloads WHERE id=? AND state='ready'")
        .bind(download_id)
        .fetch_optional(&app.db)
        .await
        .map_or(true, |row| row.is_some())
}

/// Remove subtitle files (and partial ones) whose download is no longer an
/// active demand (cancelled or gone), each under its lock. Best effort: a
/// leftover is removed by a later call.
pub(crate) async fn release_subtitles(app: App) {
    let directory = subtitles_path(&app, "x");
    let Some(directory) = directory.parent() else {
        return;
    };
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.split_once(".vtt").map(|(id, _)| id.to_owned()) else {
            continue;
        };
        if active(&app, &id).await {
            continue;
        }
        let _held = subtitles_lock(&subtitles_path(&app, &id))
            .lock_owned()
            .await;
        let _ = tokio::fs::remove_file(entry.path()).await;
    }
}

/// The download's subtitle file: converted once from the pinned file
/// revision's track (`delivery::download_subtitles`), then kept, so its
/// digest is stable for the download. Conversion runs in its own task under
/// the download's subtitle lock: concurrent and retried reads share it, a
/// request that stops waiting never leaves a partial file in place, and a
/// cancelled download is never converted or published.

// Cached subtitle reads are bounded globally, including response lifetimes.
fn subtitle_read_slot() -> Result<tokio::sync::OwnedSemaphorePermit, Problem> {
    static SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
        std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
    SLOTS.clone().try_acquire_owned().map_err(|_| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "subtitle_reads_busy",
            "Subtitle reads are busy; retry shortly",
        )
    })
}
async fn read_subtitle(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(std::io::Error::other("subtitle exceeds bound"));
    }
    Ok(bytes)
}
async fn record_subtitle(
    app: &App,
    id: &str,
    bytes: Option<&[u8]>,
) -> Result<(), crate::api::ApiError> {
    use sha2::Digest;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM downloads WHERE id=? AND state='ready')")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if !active {
        return Err(crate::api::ApiError::conflict(
            "download_cancelled",
            "The download was cancelled",
        ));
    }
    let digest = bytes.map(|b| format!("{:x}", sha2::Sha256::digest(b)));
    let changed = sqlx::query("INSERT INTO download_subtitle_artifacts(download_id,state,size_bytes,digest) VALUES (?,?,?,?) ON CONFLICT(download_id) DO NOTHING")
        .bind(id).bind(if bytes.is_some() { "ready" } else { "failed" }).bind(bytes.map_or(0, |b| b.len() as i64)).bind(digest).execute(&mut *tx).await?.rows_affected();
    if changed != 0 {
        sqlx::query("UPDATE downloads SET revision=revision+1,updated_at=? WHERE id=?")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let recorded: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT state,size_bytes,digest FROM download_subtitle_artifacts WHERE download_id=?",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if let Some(bytes) = bytes {
        let expected = format!("{:x}", sha2::Sha256::digest(bytes));
        if recorded.0 != "ready"
            || recorded.1 != bytes.len() as i64
            || recorded.2.as_deref() != Some(expected.as_str())
        {
            return Err(crate::api::ApiError::conflict(
                "subtitle_artifact_changed",
                "The pinned subtitle artifact changed",
            ));
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn ensure_subtitles(app: &App, row: &Row, track: &str) -> Result<Vec<u8>, Problem> {
    let failed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM download_subtitle_artifacts WHERE download_id=? AND state='failed')").bind(&row.id).fetch_one(&app.db).await?;
    if failed {
        return Err(Problem::invalid(
            "subtitle_conversion_failed",
            "The selected subtitle cannot be converted; cancel this download and choose another track",
        ));
    }
    let path = subtitles_path(app, &row.id);
    if let Ok(bytes) = read_subtitle(&path).await {
        record_subtitle(app, &row.id, Some(&bytes))
            .await
            .map_err(Problem::internal)?;
        return Ok(bytes);
    }
    let index = track
        .strip_prefix('s')
        .and_then(|n| n.parse::<u32>().ok())
        .ok_or_else(|| Problem::internal("invalid pinned subtitle track"))?;
    let (app, id, file, revision) = (
        app.clone(),
        row.id.clone(),
        row.file_id.clone(),
        row.file_revision.clone(),
    );
    let asked = std::time::Instant::now();
    let converted = tokio::spawn(async move {
        let path = subtitles_path(&app, &id);
        let _held = subtitles_lock(&path).lock_owned().await;
        let failed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM download_subtitle_artifacts WHERE download_id=? AND state='failed')").bind(&id).fetch_one(&app.db).await?;
        if failed {
            return Err(crate::api::ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "subtitle_conversion_failed",
                "The selected subtitle cannot be converted",
            ));
        }
        // Another read converted it while this one waited.
        if let Ok(bytes) = read_subtitle(&path).await {
            record_subtitle(&app, &id, Some(&bytes)).await?;
            return Ok(bytes);
        }
        let cancelled =
            || crate::api::ApiError::conflict("download_cancelled", "The download was cancelled");
        if !active(&app, &id).await {
            return Err(cancelled());
        }
        // Or tried and failed while this read waited: share that outcome
        // rather than converting again for a read that may already have
        // stopped waiting.
        let failed = failures()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&path)
            .copied()
            .filter(|(at, _)| *at >= asked);
        if let Some((_, expired)) = failed {
            return Err(failure(expired));
        }
        CONVERSIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let text =
            match crate::delivery::download_subtitles(&app, &id, &file, &revision, index).await {
                Ok(text) => {
                    failures()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&path);
                    text
                }
                Err(error) => {
                    if error.parts().1 == "subtitle_conversion_failed" {
                        record_subtitle(&app, &id, None).await?;
                    }
                    let expired = error.parts().1 == "source_revision_changed";
                    let mut failed = failures().lock().unwrap_or_else(|e| e.into_inner());
                    failed.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(600));
                    failed.insert(path.clone(), (std::time::Instant::now(), expired));
                    return Err(error);
                }
            };
        let reserve_root = app.processing.root.clone();
        let required = text.len() as u64;
        let reserve = app.storage.settings.min_free_bytes;
        tokio::task::spawn_blocking(move || {
            crate::storage::require_space(&reserve_root, required, reserve)
        })
        .await
        .map_err(crate::api::ApiError::internal)?
        .map_err(crate::api::ApiError::internal)?;
        let partial = path.with_extension(format!("vtt.{}", new_id()));
        let written = async {
            tokio::fs::create_dir_all(path.parent().unwrap_or(&path)).await?;
            tokio::fs::write(&partial, &text).await?;
            tokio::fs::File::open(&partial).await?.sync_all().await
        }
        .await;
        // Published only while the download is still active, under the lock
        // its removal takes.
        let published = match written {
            Ok(()) if active(&app, &id).await => tokio::fs::rename(&partial, &path)
                .await
                .map_err(crate::api::ApiError::internal),
            Ok(()) => Err(cancelled()),
            Err(error) => Err(crate::api::ApiError::internal(error)),
        };
        if published.is_err() {
            let _ = tokio::fs::remove_file(&partial).await;
        }
        published?;
        #[cfg(unix)]
        tokio::fs::File::open(
            path.parent()
                .ok_or_else(|| crate::api::ApiError::internal("missing subtitle directory"))?,
        )
        .await
        .map_err(crate::api::ApiError::internal)?
        .sync_all()
        .await
        .map_err(crate::api::ApiError::internal)?;
        record_subtitle(&app, &id, Some(&text)).await?;
        Ok(text)
    });
    match tokio::time::timeout(Duration::from_secs(30), converted).await {
        Ok(Ok(Ok(text))) => Ok(text),
        Ok(Ok(Err(error))) if error.parts().1 == "source_revision_changed" => Err(Problem::new(
            StatusCode::CONFLICT,
            "download_expired",
            "The source changed or is no longer readable; request a new download",
        )),
        Ok(Ok(Err(error))) if error.parts().1 == "download_cancelled" => Err(Problem::new(
            StatusCode::CONFLICT,
            "download_cancelled",
            "The download was cancelled",
        )),
        Ok(Ok(Err(error))) if error.parts().1 == "subtitle_conversion_failed" => {
            Err(Problem::invalid(
                "subtitle_conversion_failed",
                "The selected subtitle cannot be converted; cancel this download and choose another track",
            ))
        }
        // Still converting, or a transient failure: try again later.
        _ => Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "download_subtitles_unavailable",
            "The download's subtitles are not ready yet; try again shortly",
        )),
    }
}

#[derive(Deserialize)]
struct SubtitlesQuery {
    digest: String,
}

/// The download's subtitle file, only at the digest its manifest names:
/// the exact bytes the device verifies.
async fn subtitles(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(query): Query<SubtitlesQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let permit = subtitle_read_slot()?;
    let mut tx = app.db.begin().await?;
    let row = owned(&mut tx, &caller.principal, &id).await?;
    let current = observe(&mut tx, &caller.principal, &row).await?;
    tx.commit().await?;
    if !core::transferable(core::state(row.stored(), &row.pin(), &current)) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "download_expired",
            "The source changed or is no longer readable; request a new download",
        ));
    }
    let bytes = match row.subtitle_track_id {
        Some(_) => read_subtitle(&subtitles_path(&app, &row.id)).await.ok(),
        None => None,
    }
    .ok_or_else(Problem::not_found)?;
    use sha2::Digest;
    if query.digest != format!("sha256:{:x}", sha2::Sha256::digest(&bytes)) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "download_subtitles_changed",
            "The subtitles changed; read the manifest again",
        ));
    }
    let body = axum::body::Body::from_stream(async_stream::stream! {
        let _permit = permit;
        yield Ok::<_, std::io::Error>(bytes);
    });
    Ok(([(header::CONTENT_TYPE, "text/vtt; charset=utf-8")], body).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferInput {
    sequence: String,
    download_revision: String,
    state: core::TransferState,
    bytes_received: String,
}

/// The download's own device reports its local copy
/// (`core::accept_transfer`): about the current revision, in the device's
/// order, consistent with the size; after the download ended only the
/// copy's removal. Returns the download with the stored report.
pub async fn report_transfer(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    body: Body<TransferInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::DownloadsManage)?;
    let input = body.value;
    let number = |v: &str, name: &str| {
        viewing::decimal(v).ok_or_else(|| {
            Problem::invalid(
                "invalid_transfer",
                format!("{name} must be a decimal number"),
            )
        })
    };
    let incoming = core::TransferReport {
        sequence: number(&input.sequence, "sequence")?,
        download_revision: number(&input.download_revision, "download_revision")?,
        state: input.state,
        bytes_received: number(&input.bytes_received, "bytes_received")?,
    };
    // Stored losslessly as SQLite integers: the supported range ends there.
    if [
        incoming.sequence,
        incoming.download_revision,
        incoming.bytes_received,
    ]
    .iter()
    .any(|v| i64::try_from(*v).is_err())
    {
        return Err(Problem::invalid(
            "invalid_transfer",
            "sequence, download_revision and bytes_received must not exceed 9223372036854775807",
        ));
    }
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::DownloadsManage))
        .await?;
    // Only the download's own principal and device: everyone else is not found.
    let row = owned(&mut tx, &principal, &id).await?;
    let current = observe(&mut tx, &principal, &row).await?;
    let ended = core::state(row.stored(), &row.pin(), &current) != core::State::Ready;
    let previous = match transfer(&mut tx, &row.id).await? {
        None => None,
        Some((sequence, revision, state, bytes, _)) => Some(core::TransferReport {
            sequence: sequence as u64,
            download_revision: revision as u64,
            state: serde_json::from_value(Value::String(state)).map_err(Problem::internal)?,
            bytes_received: bytes as u64,
        }),
    };
    let subtitle_size: Option<i64> = sqlx::query_scalar(
        "SELECT size_bytes FROM download_subtitle_artifacts WHERE download_id=? AND state='ready'",
    )
    .bind(&row.id)
    .fetch_optional(&mut *tx)
    .await?;
    let transfer_size = core::transfer_bytes(
        row.size_bytes as u64,
        row.subtitle_track_id.is_some(),
        subtitle_size.map(|n| n as u64),
    );
    if transfer_size.is_none() && incoming.state == core::TransferState::VerifiedLocal {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "download_subtitles_unavailable",
            "Required subtitles have not been prepared",
        ));
    }
    let refused =
        |code: &'static str, detail: &str| Problem::new(StatusCode::CONFLICT, code, detail);
    match core::accept_transfer(
        previous.as_ref(),
        &incoming,
        row.revision as u64,
        transfer_size.unwrap_or(row.size_bytes as u64),
        ended,
    ) {
        Ok(core::TransferDecision::Record) => {
            sqlx::query("INSERT INTO download_transfers(download_id,sequence,download_revision,state,bytes_received,reported_at) VALUES (?,?,?,?,?,?) ON CONFLICT(download_id) DO UPDATE SET sequence=excluded.sequence,download_revision=excluded.download_revision,state=excluded.state,bytes_received=excluded.bytes_received,reported_at=excluded.reported_at")
                .bind(&row.id)
                .bind(incoming.sequence as i64)
                .bind(incoming.download_revision as i64)
                .bind(serde_json::to_value(incoming.state).map_err(Problem::internal)?.as_str())
                .bind(incoming.bytes_received as i64)
                .bind(now())
                .execute(&mut *tx)
                .await?;
        }
        Ok(core::TransferDecision::Unchanged) => {}
        Err(core::TransferRefusal::DownloadChanged) => {
            return Err(refused(
                "download_changed",
                "The download changed; read it again",
            ));
        }
        Err(core::TransferRefusal::Stale) => {
            return Err(refused(
                "transfer_report_stale",
                "A later report was already received",
            ));
        }
        Err(core::TransferRefusal::Conflict) => {
            return Err(refused(
                "transfer_report_conflict",
                "That report sequence was used for another report",
            ));
        }
        Err(core::TransferRefusal::Ended) => {
            return Err(refused(
                "download_ended",
                "This download ended; only the copy's removal can be reported",
            ));
        }
        Err(core::TransferRefusal::InvalidProgress) => {
            return Err(Problem::invalid(
                "invalid_transfer",
                "The progress does not fit the download (bytes beyond its size, or verified while incomplete)",
            ));
        }
    }
    let body = current_json(&mut tx, &principal, &row).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, body, row.revision))
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
    let _subtitle_read = subtitle_read_slot()?;
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
    let (title, item): (String, String) = sqlx::query_as(
        "SELECT i.title,i.id FROM timelines t JOIN editions e ON e.id=t.edition_id JOIN items i ON i.id=e.item_id WHERE t.id=?",
    )
    .bind(&row.timeline_id)
    .fetch_one(&mut *tx)
    .await?;
    // The title's descriptive metadata, as the catalog resolves it, bounded
    // for the device (`offline::offline_metadata`); carried in the manifest
    // itself, so the manifest pins it.
    let metadata = crate::metadata::documents(&mut tx, &item)
        .await
        .map_err(Problem::internal)?;
    let metadata = playscale_core::offline::offline_metadata(
        &playscale_core::metadata::resolve(&metadata.docs).values,
    );
    let view = viewing::load_view(&mut tx, &row.profile_id, &row.timeline_id).await?;
    // Read in the same snapshot as the base, so the pair names one history.
    let (server_id, restore_epoch): (String, String) =
        sqlx::query_as("SELECT server_id,restore_epoch FROM server_identity WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    // The viewing base as it was restored, when this history began with a
    // restore whose baseline was captured; unknown otherwise.
    let captured: Option<String> = sqlx::query_scalar(
        "SELECT restore_epoch FROM restore_baseline_epoch WHERE singleton=1 AND captured=1",
    )
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
    // The title's selected poster travels with the download, pinned by its
    // content digest (an artwork asset's ID is its SHA-256); only one the
    // device can fetch (an asset of the title, as asset content requires).
    let explicit: Option<Option<String>> = sqlx::query_scalar(
        "SELECT asset_id FROM artwork_selections WHERE item_id=? AND role='poster'",
    )
    .bind(&item)
    .fetch_optional(&mut *tx)
    .await?;
    let contributions: Vec<(String, String)> = sqlx::query_as("SELECT source,asset_id FROM artwork_contributions WHERE item_id=? AND role='poster' ORDER BY source").bind(&item).fetch_all(&mut *tx).await?;
    let local = contributions
        .iter()
        .find(|(source, _)| source == "local")
        .map(|(_, id)| id.clone());
    let candidates = contributions.iter().map(|(_, id)| id.clone()).collect();
    let (selected, _) = playscale_core::artwork::resolve(explicit.flatten(), local, &candidates);
    let poster: Option<(String, String, i64)> = match selected {
        None => None,
        Some(selected) => sqlx::query_as("SELECT a.id,a.mime,length(a.bytes) FROM artwork_assets a WHERE a.id=? AND EXISTS(SELECT 1 FROM (SELECT item_id,asset_id FROM artwork_contributions UNION SELECT item_id,asset_id FROM item_artwork_assets) c WHERE c.item_id=? AND c.asset_id=a.id)").bind(selected).bind(&item).fetch_optional(&mut *tx).await?,
    };
    tx.commit().await?;
    // The selected text subtitle travels as WebVTT, pinned by digest.
    let subtitles = match &row.subtitle_track_id {
        Some(track) => Some(ensure_subtitles(&app, &row, track).await?),
        None => None,
    };
    // Conversion may have awaited external work; reauthorize all state before
    // returning names, metadata or artifact grants from the earlier snapshot.
    let mut current_tx = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut current_tx, Some(Permission::DownloadsManage))
        .await?;
    let latest = owned(&mut current_tx, &principal, &id).await?;
    let observation = observe(&mut current_tx, &principal, &latest).await?;
    if !core::transferable(core::state(latest.stored(), &latest.pin(), &observation)) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "download_expired",
            "The download changed during subtitle preparation; read it again",
        ));
    }
    current_tx.commit().await?;
    let row = latest;
    let mut files = vec![json!({
        "asset_id": row.file_id,
        "relative_name": format!("media.{}", extension(&row.content_type)),
        "digest": format!("sha256:{}", row.file_revision),
        "size_bytes": row.size_bytes.to_string(),
        "media_url": media_url(&row.file_id, &row.file_revision),
        "role": "media",
    })];
    if let Some((asset, mime, bytes)) = poster.filter(|(id, mime, bytes)| {
        is_sha256(id)
            && matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp")
            && (1..=MAX_ARTWORK_BYTES).contains(bytes)
    }) {
        let extension = match mime.as_str() {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        };
        files.push(json!({
            "asset_id": asset,
            "relative_name": format!("poster.{extension}"),
            "digest": format!("sha256:{asset}"),
            "size_bytes": bytes.to_string(),
            "media_url": format!(
                "/api/v2/media/assets/{asset}/content?revision={}",
                super::metadata::ASSET_REVISION
            ),
            "role": "artwork",
        }));
    }
    let mut body = json!({
            "download_id": row.id,
            "download_revision": row.revision.to_string(),
            "timeline_id": row.timeline_id,
            "timeline_revision": row.timeline_revision.to_string(),
            "title": title,
            "duration_ms": row.duration_ms,
            "files": files,
            "viewing_revision": view.revision.to_string(),
            "manual_epoch": view.manual_epoch.to_string(),
            "viewing_epoch": format!("{server_id}:{restore_epoch}"),
            "restored_viewing_revision": restored.map(|(r, _)| r.to_string()),
            "restored_manual_epoch": restored.map(|(_, e)| e.to_string()),
    });
    if let Some(metadata) = metadata {
        body["metadata"] = json!(metadata);
    }
    if let Some(bytes) = subtitles {
        use sha2::Digest;
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        let files = body["files"].as_array_mut().expect("files");
        files.push(json!({
            "asset_id": format!("{}-subtitles", row.id),
            "relative_name": "subtitles.vtt",
            "digest": format!("sha256:{digest}"),
            "size_bytes": bytes.len().to_string(),
            "media_url": format!(
                "/api/v2/downloads/{}/subtitles?digest=sha256:{digest}",
                row.id
            ),
            "role": "subtitle",
        }));
    }
    Ok(tagged(StatusCode::OK, body, row.revision))
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
        "DELETE FROM download_transfers WHERE download_id IN (SELECT id FROM downloads WHERE profile_id=?)",
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
/// Prepared renditions of one recipe read per page, and pages at most.
const PREPARED_PAGE: i64 = 200;
/// Largest poster that travels with a download (the artwork upload bound).
const MAX_ARTWORK_BYTES: i64 = 8 * 1024 * 1024;

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
    if let Some((_, through)) = &reanchor
        && !core::reanchor_batch(*through, acknowledged, events.first().map(|e| e.sequence))
    {
        return Err(Problem::invalid(
            "invalid_reanchor",
            "The re-anchor must match the acknowledgement and the first retained sequence",
        ));
    }
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
                "event_id": event.id, "outcome": "rejected", "code": if acknowledged.is_some() { "acknowledgement_required" } else { "sequence_gap" },
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
