//! v2 playback, delivery, viewing and preference adapters.
//!
//! Decisions live in `playscale_core::playback_session` (route choice, plan
//! admission, delivery ownership, viewing authority), `playscale_core::delivery`
//! (generations, leases, activation) and `playscale_core::viewing` (ordered
//! progress). This adapter observes the caller, catalog and files, signs plan
//! tokens, and executes the effects through `crate::delivery` and
//! `crate::viewing`, which v1 shares.
use super::{
    Body, Problem,
    auth::Caller,
    catalog::{self, TimelineRow},
    content::file_facts,
    etag, idempotency_key, if_match, timestamp, viewing,
};
use crate::{App, api::ApiError, db, delivery, now, viewing as legacy};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use hmac::{Hmac, Mac};
use playscale_core::{
    access::{self, Permission, Principal},
    playback::{Mode, Support},
    playback_session::{self as core, Claims, Owner, PlanError, Route},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub fn routes() -> Router<App> {
    Router::new()
        .route(
            "/profiles/{profile_id}/preferences",
            get(get_preferences).put(put_preferences),
        )
        .route(
            "/profiles/{profile_id}/timelines/{timeline_id}/viewing",
            get(viewing::get_viewing).put(viewing::put_viewing),
        )
        .route(
            "/profiles/{profile_id}/continue-watching",
            get(viewing::continue_watching),
        )
        .route(
            "/profiles/{profile_id}/timelines/{timeline_id}/next",
            get(viewing::next_timeline),
        )
        .route("/playback/plans", post(plan))
        .route("/playback/delivery-sessions", post(create_delivery))
        .route(
            "/playback/delivery-sessions/{delivery_id}",
            get(get_delivery).delete(close_delivery),
        )
        .route(
            "/playback/delivery-sessions/{delivery_id}/heartbeat",
            post(heartbeat),
        )
        .route(
            "/playback/delivery-sessions/{delivery_id}/changes",
            post(change_delivery),
        )
        .route(
            "/playback/delivery-sessions/{delivery_id}/generations/{generation}/activate",
            post(activate),
        )
        .route("/playback/viewing-sessions", post(viewing::create_viewing))
        .route(
            "/playback/viewing-sessions/{session_id}",
            get(viewing::get_viewing_session),
        )
        .route(
            "/playback/viewing-sessions/{session_id}/events",
            post(viewing::record_viewing),
        )
        .route(
            "/playback/viewing-sessions/{session_id}/delivery",
            put(viewing::replace_viewing_delivery),
        )
        .route(
            "/streams/{delivery_id}/{generation}/master.m3u8",
            get(master_playlist),
        )
        .route(
            "/streams/{delivery_id}/{generation}/variants/{variant_id}/index.m3u8",
            get(variant_playlist),
        )
        .route(
            "/streams/{delivery_id}/{generation}/variants/{variant_id}/init.mp4",
            get(init_segment),
        )
        .route(
            "/streams/{delivery_id}/{generation}/segments/{segment_id}",
            get(media_segment),
        )
}

/// The only HLS variant a live generation publishes.
const VARIANT: &str = "main";

/// Map a shared-service error onto the v2 problem vocabulary.
fn problem(e: ApiError) -> Problem {
    let (status, code, detail) = e.parts();
    let code: &'static str = match code {
        "not_found" => return Problem::not_found(),
        "delivery_closed" => "delivery_closed",
        "generation_conflict" => "generation_conflict",
        "generation_not_ready" => "generation_not_ready",
        "timeline_changed" => "timeline_changed",
        "invalid_position" => "invalid_position",
        "generation_limit" => "generation_limit",
        "source_revision_changed" => "source_revision_changed",
        "route_unsupported" => "route_unsupported",
        "route_unqualified" => "route_unqualified",
        "hdr_unsupported" => "hdr_unsupported",
        "subtitle_unsupported" => "subtitle_unsupported",
        "admission_busy" => "admission_busy",
        "server_stopping" => "server_stopping",
        "delivery_limit" => "delivery_limit",
        "stream_limit" => "stream_limit",
        "idempotency_conflict" => "idempotency_conflict",
        "platform_unsupported" => "platform_unsupported",
        "viewing_revision_conflict" => "viewing_revision_conflict",
        "preferences_revision_conflict" => "preferences_revision_conflict",
        "file_revision_conflict" => "file_revision_conflict",
        "superseded_session" => "superseded_session",
        "closed_session" => "closed_session",
        "stale_sequence" => "stale_sequence",
        "invalid_event" => "invalid_event",
        "invalid_file_switch_status" => "invalid_event",
        "invalid_position_for_duration" => "invalid_position",
        "viewing_revision_exhausted" => "viewing_revision_exhausted",
        "internal_error" => return Problem::internal(detail),
        _ if status == StatusCode::BAD_REQUEST => "invalid_request",
        _ if status == StatusCode::CONFLICT => "conflict",
        _ => "playback_unavailable",
    };
    let mut p = Problem::new(status, code, detail.to_owned());
    if status == StatusCode::SERVICE_UNAVAILABLE {
        p.retry_after = Some(1);
    }
    p
}

fn plan_problem(e: PlanError) -> Problem {
    match e {
        PlanError::Expired => Problem::new(
            StatusCode::CONFLICT,
            "plan_expired",
            "The plan expired; plan playback again",
        ),
        // Another principal's token names nothing this caller may see.
        PlanError::WrongPrincipal => invalid_plan(),
        PlanError::Forbidden(p) => access::AccessError::Forbidden(p).into(),
        PlanError::ProfileForbidden | PlanError::NotFound => Problem::not_found(),
        PlanError::SourceChanged => Problem::new(
            StatusCode::CONFLICT,
            "source_revision_changed",
            "The file changed; plan playback again",
        ),
    }
}

fn invalid_plan() -> Problem {
    Problem::invalid(
        "invalid_plan_token",
        "The plan token is not a playback plan issued to this principal",
    )
}

fn segment(s: &str) -> String {
    let mut url = url::Url::parse("http://local/").unwrap();
    url.path_segments_mut().unwrap().push(s);
    url.path().trim_start_matches('/').into()
}

// ---------------------------------------------------------------- profiles

/// A profile the principal may use. Unknown and forbidden are indistinguishable.
pub(crate) async fn usable_profile(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    profile: &str,
) -> Result<(), Problem> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM profiles WHERE id=?")
        .bind(profile)
        .fetch_optional(conn)
        .await?;
    if exists.is_none() || !principal.may_use_profile(profile) {
        return Err(Problem::not_found());
    }
    Ok(())
}

/// A timeline readable under the principal's catalog scope.
pub(crate) async fn readable_timeline(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    timeline: &str,
) -> Result<TimelineRow, Problem> {
    let row: TimelineRow = sqlx::query_as(
        "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(timeline)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Problem::not_found)?;
    catalog::load_item(conn, &access::catalog_scope(principal), &row.item_id, false).await?;
    Ok(row)
}

fn preferences_json(state: &legacy::PreferenceState) -> Value {
    let p = &state.preferences;
    json!({
        "audio_languages": p.audio_languages,
        "subtitle_languages": p.subtitle_languages,
        "subtitle_mode": p.subtitle_mode,
        "quality_mode": p.quality,
        "allow_client_software_decode": p.allow_client_software_decode,
        "autoplay": p.autoplay,
        "completion_percent": p.completion_percent,
    })
}

fn tagged(status: StatusCode, body: Value, revision: i64) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::ETAG, etag(revision.max(0) as u64));
    response
}

pub async fn get_preferences(
    State(app): State<App>,
    caller: Caller,
    Path(profile): Path<String>,
) -> Result<Response, Problem> {
    let mut conn = app.db.acquire().await?;
    usable_profile(&mut conn, &caller.principal, &profile).await?;
    drop(conn);
    let state = legacy::prefs(&app, &profile).await.map_err(problem)?;
    Ok(tagged(
        StatusCode::OK,
        preferences_json(&state),
        state.revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesInput {
    audio_languages: Vec<String>,
    subtitle_languages: Vec<String>,
    subtitle_mode: String,
    quality_mode: String,
    allow_client_software_decode: bool,
    autoplay: bool,
    completion_percent: f64,
}

pub async fn put_preferences(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    Path(profile): Path<String>,
    body: Body<PreferencesInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let expected = if_match(&headers)?;
    let input = body.value;
    if !matches!(
        input.subtitle_mode.as_str(),
        "off" | "forced" | "always" | "foreign_audio"
    ) || !matches!(input.quality_mode.as_str(), "auto" | "original" | "convert")
        || !(50.0..=100.0).contains(&input.completion_percent)
    {
        return Err(Problem::invalid(
            "invalid_preferences",
            "Unknown subtitle or quality mode, or completion outside 50-100",
        ));
    }
    let mut languages = [input.audio_languages, input.subtitle_languages];
    for list in &mut languages {
        if list.len() > 16
            || list
                .iter()
                .any(|v| !playscale_core::viewing::valid_language(v))
        {
            return Err(Problem::invalid(
                "invalid_preferences",
                "Expected at most sixteen bounded language tags",
            ));
        }
        for v in list.iter_mut() {
            *v = v.to_ascii_lowercase();
        }
        if list.iter().collect::<std::collections::BTreeSet<_>>().len() != list.len() {
            return Err(Problem::invalid(
                "invalid_preferences",
                "Language tags must be unique",
            ));
        }
    }
    let [audio_languages, subtitle_languages] = languages;
    // Authorization, the revision check and the write share one writer
    // transaction, so neither a revocation nor a concurrent write slips between.
    let mut tx = db::begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ViewingWrite))
        .await?;
    usable_profile(&mut tx, &principal, &profile).await?;
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT revision,document_json FROM playback_preferences WHERE profile_id=?",
    )
    .bind(&profile)
    .fetch_optional(&mut *tx)
    .await?;
    let (revision, current) = match row {
        Some((revision, document)) => (
            revision,
            serde_json::from_str::<legacy::Preferences>(&document).map_err(Problem::internal)?,
        ),
        None => (0, legacy::Preferences::default()),
    };
    if u64::try_from(revision).ok() != Some(expected) {
        return Err(access::AccessError::StaleRevision.into());
    }
    let state = legacy::PreferenceState {
        revision: revision + 1,
        preferences: legacy::Preferences {
            audio_languages,
            subtitle_languages,
            subtitle_mode: input.subtitle_mode,
            quality: input.quality_mode,
            // Not part of the v2 document; kept for v1 clients.
            conversion_recipe: current.conversion_recipe,
            allow_client_software_decode: input.allow_client_software_decode,
            autoplay: input.autoplay,
            completion_percent: input.completion_percent,
        },
    };
    sqlx::query("INSERT INTO playback_preferences(profile_id,revision,document_json) VALUES (?,?,?) ON CONFLICT(profile_id) DO UPDATE SET revision=excluded.revision,document_json=excluded.document_json")
        .bind(&profile)
        .bind(state.revision)
        .bind(serde_json::to_string(&state.preferences).map_err(Problem::internal)?)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        preferences_json(&state),
        state.revision,
    ))
}

// -------------------------------------------------------------------- plans

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    file_id: String,
    file_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSelection {
    audio_component_id: Option<String>,
    subtitle_component_id: Option<String>,
    subtitle_policy: String,
    audio_track_id: Option<String>,
    subtitle_track_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityPolicy {
    mode: String,
    /// Unsigned decimal string (contract UInt64).
    max_bitrate_bps: Option<String>,
    max_height: Option<u32>,
    allow_client_software: bool,
    hdr_policy: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCapability {
    #[allow(dead_code)]
    client_id: String,
    #[allow(dead_code)]
    client_build: String,
    #[allow(dead_code)]
    demuxe_asset_digest: Option<String>,
    transports: Vec<String>,
    video_codecs: Vec<String>,
    audio_codecs: Vec<String>,
    #[allow(dead_code)]
    subtitle_modes: Vec<String>,
    #[allow(dead_code)]
    hdr: String,
    max_height: Option<u32>,
    #[allow(dead_code)]
    software_decode: String,
    #[allow(dead_code)]
    cross_origin_isolated: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    profile_id: String,
    timeline_id: String,
    version_id: Option<String>,
    source: Option<FileRef>,
    tracks: TrackSelection,
    quality: QualityPolicy,
    client: ClientCapability,
    #[serde(default)]
    failed_candidate_ids: Vec<String>,
}

/// One original file bound to a timeline's version.
struct Bound {
    version: String,
    file: db::ItemRow,
}

/// The first available original bound to the timeline (or to `version`).
/// The first available original bound to the timeline (or to `version`, or
/// the pinned `file`) that `principal` may read. Unreadable copies are
/// skipped, so a hidden first copy never hides a readable second one.
async fn original_of(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    timeline: &str,
    version: Option<&str>,
    file: Option<&str>,
) -> Result<Option<Bound>, Problem> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT v.id,b.file_id FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         JOIN media_files f ON f.id=b.file_id \
         WHERE v.timeline_id=? AND v.origin='original' AND f.generated=0 AND f.available=1 \
         AND (? IS NULL OR v.id=?) AND (? IS NULL OR f.id=?) \
         AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1) ORDER BY v.id,b.file_id LIMIT 100",
    )
    .bind(timeline)
    .bind(version)
    .bind(version)
    .bind(file)
    .bind(file)
    .fetch_all(&mut *conn)
    .await?;
    for (version, file) in rows {
        if !core::readable(principal, file_facts(conn, &file).await?.as_ref()) {
            continue;
        }
        let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=?")
            .bind(file)
            .fetch_one(&mut *conn)
            .await?;
        return Ok(Some(Bound { version, file }));
    }
    Ok(None)
}

/// The first original bound to the timeline regardless of availability or
/// scope: only a key for its renditions, never itself delivered.
async fn any_original_of(
    conn: &mut sqlx::SqliteConnection,
    timeline: &str,
    version: Option<&str>,
    file: Option<&str>,
) -> Result<Option<Bound>, Problem> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT v.id,b.file_id FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         JOIN media_files f ON f.id=b.file_id \
         WHERE v.timeline_id=? AND v.origin='original' AND f.generated=0 \
         AND (? IS NULL OR v.id=?) AND (? IS NULL OR f.id=?) ORDER BY v.id,b.file_id LIMIT 1",
    )
    .bind(timeline)
    .bind(version)
    .bind(version)
    .bind(file)
    .bind(file)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((version, file)) = row else {
        return Ok(None);
    };
    let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=?")
        .bind(file)
        .fetch_one(conn)
        .await?;
    Ok(Some(Bound { version, file }))
}

/// Whether the version still binds the file to the timeline.
async fn still_bound(conn: &mut sqlx::SqliteConnection, claims: &Claims) -> Result<bool, Problem> {
    let bound: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM media_versions v JOIN version_files b ON b.version_id=v.id \
         WHERE v.id=? AND v.timeline_id=? AND b.file_id=? AND b.part=1",
    )
    .bind(&claims.version)
    .bind(&claims.timeline)
    .bind(&claims.file)
    .fetch_optional(conn)
    .await?;
    Ok(bound.is_some())
}

/// The original a pinned generated version renders, when `version` is one.
async fn pinned_rendition_source(
    conn: &mut sqlx::SqliteConnection,
    timeline: &str,
    version: &str,
) -> Result<Option<String>, Problem> {
    Ok(sqlx::query_scalar(
        "SELECT r.source_file_id FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         JOIN renditions r ON r.file_id=b.file_id WHERE v.id=? AND v.timeline_id=? AND v.origin='generated' LIMIT 1",
    )
    .bind(version)
    .bind(timeline)
    .fetch_optional(conn)
    .await?)
}

/// Generated renditions of `original` bound to the timeline (or to the
/// pinned `version`) whose output and source revisions are still the
/// registered ones (core `renditions::available`), newest first. Each reaches
/// the planning decision, which picks the first one the client can use.
async fn prepared_of(
    conn: &mut sqlx::SqliteConnection,
    timeline: &str,
    original: &str,
    version: Option<&str>,
) -> Result<Vec<Bound>, Problem> {
    let rows: Vec<(String, String, bool, String, String, String, String)> = sqlx::query_as(
        "SELECT v.id,f.id,f.available,f.revision,r.file_revision,s.revision,r.source_revision \
         FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 \
         JOIN media_files f ON f.id=b.file_id JOIN renditions r ON r.file_id=f.id \
         JOIN media_files s ON s.id=r.source_file_id \
         WHERE v.timeline_id=? AND v.origin='generated' AND f.generated=1 AND r.source_file_id=? \
         AND (? IS NULL OR v.id=?) AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1) \
         ORDER BY r.updated_at DESC,v.id LIMIT 16",
    )
    .bind(timeline)
    .bind(original)
    .bind(version)
    .bind(version)
    .fetch_all(&mut *conn)
    .await?;
    let mut found = Vec::new();
    for (version, file, available, revision, registered, source, registered_source) in rows {
        if playscale_core::renditions::available(
            available,
            &revision,
            &registered,
            &source,
            &registered_source,
        ) {
            let file: db::ItemRow = sqlx::query_as("SELECT * FROM catalog_files WHERE id=?")
                .bind(file)
                .fetch_one(&mut *conn)
                .await?;
            found.push(Bound { version, file });
        }
    }
    Ok(found)
}

/// Within the client's height and bitrate limits, by probed facts.
fn fits_budget(file: &db::ItemRow, max_height: Option<u32>, max_bitrate: Option<u64>) -> bool {
    let streams: Vec<db::Track> = serde_json::from_str(&file.tracks_json).unwrap_or_default();
    let height = streams
        .iter()
        .find(|t| t.kind == "video")
        .and_then(|t| t.height);
    max_height.is_none_or(|l| height.is_none_or(|h| h <= l))
        && max_bitrate.is_none_or(|b| {
            let bitrate = file
                .duration_seconds
                .filter(|d| *d > 0.0)
                .map(|d| (file.bytes as f64 * 8.0 / d) as u64);
            bitrate.is_none_or(|r| r <= b)
        })
}

/// Default audio stream (the first) of a file, when it has audio.
fn first_audio(file: &db::ItemRow) -> Option<u32> {
    let streams: Vec<db::Track> = serde_json::from_str(&file.tracks_json).unwrap_or_default();
    streams.iter().any(|t| t.kind == "audio").then_some(0)
}

/// Client-facing codec tags for the browser capability probe.
fn video_tag(codec: &str) -> Option<&'static str> {
    Some(match codec {
        "h264" => "avc1",
        "hevc" | "h265" => "hvc1",
        "vp9" => "vp09",
        "av1" => "av01",
        "vp8" => "vp8",
        _ => return None,
    })
}
fn audio_tag(codec: &str) -> Option<&'static str> {
    Some(match codec {
        "aac" => "mp4a",
        "opus" => "opus",
        "mp3" => "mp3",
        "flac" => "flac",
        "vorbis" => "vorbis",
        _ => return None,
    })
}
/// Codecs no supported browser decodes natively.
fn never_browser(video: &str, audio: Option<&str>) -> bool {
    matches!(
        video,
        "mpeg2video" | "mpeg1video" | "mpeg4" | "msmpeg4v3" | "prores" | "dnxhd" | "vc1" | "wmv3"
    ) || audio.is_some_and(|a| {
        matches!(
            a,
            "ac3" | "eac3" | "dts" | "truehd" | "mlp" | "wmav2" | "pcm_s24le"
        )
    })
}

/// What the client reports about decoding this file's video and default audio.
fn client_support(file: &db::ItemRow, client: &ClientCapability, audio: Option<u32>) -> Support {
    let streams: Vec<db::Track> = serde_json::from_str(&file.tracks_json).unwrap_or_default();
    let Some(video) = streams.iter().find(|t| t.kind == "video") else {
        return Support::Unknown;
    };
    let audio = audio.and_then(|i| {
        streams
            .iter()
            .filter(|t| t.kind == "audio")
            .nth(i as usize)
            .map(|t| t.codec.as_str())
    });
    if never_browser(&video.codec, audio) {
        return Support::Unsupported;
    }
    let listed =
        |tag: Option<&str>, list: &[String]| tag.is_some_and(|t| list.iter().any(|c| c == t));
    let video_ok = listed(video_tag(&video.codec), &client.video_codecs);
    let audio_ok = audio.is_none_or(|a| listed(audio_tag(a), &client.audio_codecs));
    let container = file
        .relative_path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase);
    let browser_container = matches!(container.as_deref(), Some("mp4" | "m4v" | "mov" | "webm"));
    if video_ok && audio_ok && browser_container {
        Support::Supported
    } else {
        // Not reported is not proof of failure: the client may still open it
        // and report the candidate failed.
        Support::Unknown
    }
}

/// `a{n}` / `s{n}`: the n-th audio or subtitle stream of the planned file.
fn track_index(id: &Option<String>, prefix: char, count: usize) -> Result<Option<u32>, Problem> {
    let Some(id) = id else { return Ok(None) };
    id.strip_prefix(prefix)
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| (*n as usize) < count)
        .map(Some)
        .ok_or_else(|| Problem::invalid("unknown_track", "The selected track does not exist"))
}

fn plan_mac(app: &App, payload: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&app.access.key).expect("HMAC accepts any key length");
    mac.update(b"playback-plan:");
    mac.update(payload.as_bytes());
    format!("{:x}", mac.finalize().into_bytes())
}

const PLAN_PREFIX: &str = "mpp_";

fn sign(app: &App, claims: &Claims) -> Result<String, Problem> {
    let json = serde_json::to_vec(claims).map_err(Problem::internal)?;
    let payload: String = json.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{PLAN_PREFIX}{payload}.{}",
        plan_mac(app, &payload)
    ))
}

/// Verify a plan token's MAC. Every pinned fact is rechecked by `core::admit`.
fn open(app: &App, token: &str) -> Result<Claims, Problem> {
    let (payload, mac) = token
        .strip_prefix(PLAN_PREFIX)
        .and_then(|t| t.split_once('.'))
        .ok_or_else(invalid_plan)?;
    if Sha256::digest(plan_mac(app, payload).as_bytes()) != Sha256::digest(mac.as_bytes()) {
        return Err(invalid_plan());
    }
    let bytes = (0..payload.len())
        .step_by(2)
        .map(|i| {
            payload
                .get(i..i + 2)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(invalid_plan)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_plan())
}

fn blocked(input: &PlanInput, reason: &str) -> Value {
    json!({
        "status": "blocked", "plan_token": null, "expires_at": timestamp(now()),
        "candidate_id": null, "profile_id": input.profile_id, "timeline_id": input.timeline_id,
        "version_id": null, "source": null, "transport": null, "operation": null,
        "reason_codes": [reason], "warnings": [], "tracks": tracks_json(&input.tracks, None, None),
    })
}

fn tracks_json(t: &TrackSelection, audio: Option<u32>, subtitle: Option<u32>) -> Value {
    json!({
        "audio_component_id": t.audio_component_id,
        "subtitle_component_id": t.subtitle_component_id,
        "subtitle_policy": t.subtitle_policy,
        "audio_track_id": audio.map(|a| format!("a{a}")),
        "subtitle_track_id": subtitle.map(|s| format!("s{s}")),
    })
}

pub async fn plan(
    State(app): State<App>,
    caller: Caller,
    body: Body<PlanInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::PlaybackRequest)?;
    let input = body.value;
    if !matches!(
        input.tracks.subtitle_policy.as_str(),
        "off" | "auto" | "require"
    ) || !matches!(
        input.quality.hdr_policy.as_str(),
        "preserve_if_supported" | "require_sdr" | "reject_conversion"
    ) || input.failed_candidate_ids.len() > 16
    {
        return Err(Problem::invalid(
            "invalid_plan_input",
            "Unknown subtitle or HDR policy, or too many failed candidates",
        ));
    }
    let max_bitrate = match input.quality.max_bitrate_bps.as_deref() {
        None => None,
        Some(b) => Some(
            b.parse::<u64>()
                .ok()
                .filter(|_| b.len() <= 20 && (b == "0" || !b.starts_with('0')))
                .filter(|_| b.bytes().all(|c| c.is_ascii_digit()))
                .ok_or_else(|| {
                    Problem::invalid(
                        "invalid_plan_input",
                        "max_bitrate_bps must be a decimal string",
                    )
                })?,
        ),
    };
    let mode = match input.quality.mode.as_str() {
        "auto" => Mode::Auto,
        "original" => Mode::Original,
        "convert" => Mode::Convert,
        _ => {
            return Err(Problem::invalid(
                "invalid_plan_input",
                "Unknown quality mode",
            ));
        }
    };
    let principal = &caller.principal;
    let mut conn = app.db.acquire().await?;
    usable_profile(&mut conn, principal, &input.profile_id).await?;
    readable_timeline(&mut conn, principal, &input.timeline_id).await?;
    if input.tracks.audio_component_id.is_some() || input.tracks.subtitle_component_id.is_some() {
        return Ok(Json(blocked(&input, "component_selection_unavailable")).into_response());
    }
    // A pinned generated version names the original it renders; only that
    // rendition may then serve the plan.
    let pinned_source = match input.version_id.as_deref() {
        Some(v) => pinned_rendition_source(&mut conn, &input.timeline_id, v).await?,
        None => None,
    };
    let pinned_prepared = pinned_source.is_some();
    // With a pinned rendition, a source pin names that rendition, not the
    // original it renders.
    let original_pin = pinned_source.as_deref().or(input
        .source
        .as_ref()
        .filter(|_| !pinned_prepared)
        .map(|s| s.file_id.as_str()));
    let version_pin = input.version_id.as_deref().filter(|_| !pinned_prepared);
    // Without an available original in scope, its revision-valid renditions
    // may still serve; the original then only keys them.
    let (bound, original_available) = match original_of(
        &mut conn,
        principal,
        &input.timeline_id,
        version_pin,
        original_pin,
    )
    .await?
    {
        Some(bound) => (bound, true),
        None => {
            match any_original_of(&mut conn, &input.timeline_id, version_pin, original_pin).await? {
                Some(bound) => (bound, false),
                None => return Ok(Json(blocked(&input, "source_unavailable")).into_response()),
            }
        }
    };
    if !pinned_prepared
        && input
            .source
            .as_ref()
            .is_some_and(|s| s.file_revision != bound.file.revision)
    {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "source_revision_changed",
            "The file changed; read the timeline again",
        ));
    }
    let streams: Vec<db::Track> = serde_json::from_str(&bound.file.tracks_json).unwrap_or_default();
    let audios = streams.iter().filter(|t| t.kind == "audio").count();
    let subtitles = streams.iter().filter(|t| t.kind == "subtitle").count();
    let default_audio = (audios > 0).then_some(0);
    let audio = track_index(&input.tracks.audio_track_id, 'a', audios)?.or(default_audio);
    let subtitle = track_index(&input.tracks.subtitle_track_id, 's', subtitles)?;
    if subtitle.is_some() || input.tracks.subtitle_policy == "require" {
        // No subtitle presentation path exists in the v2 delivery contract yet.
        return Ok(Json(blocked(&input, "subtitle_delivery_unavailable")).into_response());
    }
    let height = streams
        .iter()
        .find(|t| t.kind == "video")
        .and_then(|t| t.height);
    let limit = [input.quality.max_height, input.client.max_height]
        .into_iter()
        .flatten()
        .min();
    let mut prepared = Vec::new();
    for p in prepared_of(
        &mut conn,
        &input.timeline_id,
        &bound.file.id,
        input.version_id.as_deref().filter(|_| pinned_prepared),
    )
    .await?
    {
        // A rendition outside the caller's scope is not a candidate.
        if core::readable(principal, file_facts(&mut conn, &p.file.id).await?.as_ref()) {
            prepared.push(p);
        }
    }
    if let Some(pin) = input.source.as_ref().filter(|_| pinned_prepared) {
        prepared.retain(|p| p.file.id == pin.file_id);
        if prepared
            .iter()
            .any(|p| p.file.revision != pin.file_revision)
        {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "source_revision_changed",
                "The file changed; read the timeline again",
            ));
        }
    }
    let source = core::SourceFacts {
        available: original_available && bound.file.available,
        client_support: client_support(&bound.file, &input.client, audio),
        within_budget: fits_budget(&bound.file, limit, max_bitrate),
        prepared: prepared
            .iter()
            .map(|p| core::PreparedFacts {
                file: p.file.id.clone(),
                client_support: client_support(&p.file, &input.client, first_audio(&p.file)),
                within_budget: fits_budget(&p.file, limit, max_bitrate),
            })
            .collect(),
        transcodable: input.quality.hdr_policy != "reject_conversion"
            && delivery::live_transcodable(&bound.file, audio),
        // The software live recipe scales to at most LIVE_MAX_HEIGHT lines and
        // targets quality (CRF), so it cannot promise any bitrate limit.
        conversion_within_budget: core::conversion_fits(
            limit,
            max_bitrate,
            height.map(|h| h.min(delivery::LIVE_MAX_HEIGHT)),
            None,
        ),
    };
    let request = core::Request {
        mode,
        range: input.client.transports.iter().any(|t| t == "http_range"),
        hls: input.client.transports.iter().any(|t| t == "hls"),
        explicit_streams: audio != default_audio,
        pinned_prepared,
        failed: input.failed_candidate_ids.clone(),
    };
    // A prepared plan pins the chosen rendition and plays its default streams.
    let (route, chosen, audio) = match core::plan(&bound.file.id, &source, &request) {
        core::Plan::Ready(route) => (route, bound, audio),
        core::Plan::Prepared(i) => {
            let p = prepared
                .into_iter()
                .nth(i)
                .ok_or_else(|| Problem::internal("prepared plan without rendition"))?;
            let audio = first_audio(&p.file);
            (Route::Prepared, p, audio)
        }
        core::Plan::Blocked(reason) => return Ok(Json(blocked(&input, reason)).into_response()),
    };
    let expires_at = now() + core::PLAN_TTL_SECONDS;
    let claims = Claims {
        principal: principal.id.clone(),
        profile: input.profile_id.clone(),
        timeline: input.timeline_id.clone(),
        version: chosen.version.clone(),
        file: chosen.file.id.clone(),
        file_revision: chosen.file.revision.clone(),
        route,
        audio,
        subtitle,
        expires_at,
    };
    let mut warnings = vec![];
    if route != Route::Transcode && !input.quality.allow_client_software {
        warnings.push("client_decode_capability_unverified");
    }
    Ok(Json(json!({
        "status": "ready",
        "plan_token": sign(&app, &claims)?,
        "expires_at": timestamp(expires_at),
        "candidate_id": route.candidate_id(&chosen.file.id),
        "profile_id": input.profile_id,
        "timeline_id": input.timeline_id,
        "version_id": chosen.version,
        "source": {"file_id": chosen.file.id, "file_revision": chosen.file.revision},
        "transport": route.transport(),
        "operation": serde_json::to_value(route.operation()).map_err(Problem::internal)?,
        "reason_codes": [],
        "warnings": warnings,
        "tracks": tracks_json(&input.tracks, audio, subtitle),
    }))
    .into_response())
}

// --------------------------------------------------------------- deliveries

/// Re-derives the caller inside the admission transaction and rechecks the
/// plan with current file facts and binding (core `admit`). A refusal keeps
/// its v2 problem so its code reaches the client unchanged.
struct PlanAuthority {
    caller: Caller,
    claims: Claims,
    refusal: std::sync::Mutex<Option<Problem>>,
}
impl PlanAuthority {
    fn refuse(&self, p: Problem) -> ApiError {
        let e = problem_to_api(&p);
        *self.refusal.lock().unwrap() = Some(p);
        e
    }
}
impl delivery::AdmissionAuthority for PlanAuthority {
    fn reauthorize<'a>(
        &'a self,
        db: &'a mut sqlx::SqliteConnection,
        _: &'a delivery::CreateRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, ApiError>> + Send + 'a>>
    {
        Box::pin(async move {
            let principal = self
                .caller
                .reauthorize(db, None)
                .await
                .map_err(|p| self.refuse(p))?;
            let facts = file_facts(db, &self.claims.file)
                .await
                .map_err(|p| self.refuse(p))?;
            let bound = still_bound(db, &self.claims)
                .await
                .map_err(|p| self.refuse(p))?;
            core::admit(
                &self.claims,
                &core::Observed {
                    principal: &principal,
                    file: facts.as_ref(),
                    bound,
                    now: now(),
                },
            )
            .map_err(|e| self.refuse(plan_problem(e)))?;
            Ok(principal.id)
        })
    }
}

fn problem_to_api(p: &Problem) -> ApiError {
    ApiError::new(p.status, p.code, &p.detail)
}

fn owner_of(claims: &Claims) -> Owner {
    Owner {
        principal: claims.principal.clone(),
        profile: claims.profile.clone(),
        timeline: claims.timeline.clone(),
        file: claims.file.clone(),
        file_revision: claims.file_revision.clone(),
    }
}

fn generation_json(delivery: &str, owner: &Owner, g: &delivery::GenerationView) -> Value {
    let hls = g.transport == "hls";
    let media_url = (!hls).then(|| {
        format!(
            "/api/v2/media/files/{}/content?revision={}",
            segment(&owner.file),
            url::form_urlencoded::byte_serialize(owner.file_revision.as_bytes())
                .collect::<String>()
        )
    });
    // The v1 view names a manifest only while the generation is served.
    let manifest_url = (hls && g.manifest_url.is_some()).then(|| {
        format!(
            "/api/v2/streams/{}/{}/master.m3u8",
            segment(delivery),
            g.generation
        )
    });
    json!({
        "generation": g.generation,
        "status": g.status,
        "media_url": media_url,
        "manifest_url": manifest_url,
        "media_time_origin_ms": g.media_time_origin_ms,
        "requested_start_ms": g.requested_start_ms,
        "available_start_ms": g.available_start_ms,
        "available_end_ms": g.available_end_ms,
        "transport": g.transport,
        "operation": g.operation,
        "error_code": (g.status == "failed").then_some("generation_failed"),
    })
}

fn delivery_json(view: &delivery::DeliveryView, owner: &Owner) -> Value {
    json!({
        "id": view.id,
        "revision": view.revision,
        "replacement_mode": view.replacement_mode,
        "profile_id": owner.profile,
        "timeline_id": owner.timeline,
        "source": {"file_id": owner.file, "file_revision": owner.file_revision},
        "status": view.status,
        "active": view.active.as_ref().map(|g| generation_json(&view.id, owner, g)),
        "pending": view.pending.as_ref().map(|g| generation_json(&view.id, owner, g)),
        // Whole seconds, rounded down: the client never believes a lease
        // lasts longer than the server will honor it.
        "lease_expires_at": timestamp(now() + (view.lease_expires_in_ms / 1000) as i64),
        "heartbeat_interval_seconds": view.heartbeat_interval_seconds,
        "logical_duration_ms": view.logical_duration_ms,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryCreate {
    plan_token: String,
    start_ms: u64,
}

pub async fn create_delivery(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<DeliveryCreate>,
) -> Result<Response, Problem> {
    caller.require(Permission::PlaybackRequest)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    let claims = open(&app, &input.plan_token)?;
    if claims.principal != caller.principal.id {
        return Err(invalid_plan());
    }
    let owner = owner_of(&claims);
    let request = delivery::CreateRequest {
        file_id: claims.file.clone(),
        file_revision: claims.file_revision.clone(),
        start_ms: input.start_ms,
        audio_track: claims.audio,
        operation: None,
        subtitle_track: claims.subtitle,
        backend: None,
    };
    let planned = delivery::Planned {
        owner: owner.clone(),
        version: claims.version.clone(),
        route: claims.route,
    };
    let authority = Arc::new(PlanAuthority {
        caller: caller.clone(),
        claims,
        refusal: Default::default(),
    });
    let acknowledged = match delivery::admit_planned(
        app.clone(),
        authority.clone(),
        key,
        request,
        planned,
    )
    .await
    {
        Ok(view) => view,
        Err(e) => {
            return Err(authority
                .refusal
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| problem(e)));
        }
    };
    // A replayed acknowledgement reports the delivery's current state when it
    // is still live; a retired one replays as acknowledged.
    let view = delivery::live_view(&app, &acknowledged.id).unwrap_or(acknowledged);
    Ok((StatusCode::CREATED, Json(delivery_json(&view, &owner))).into_response())
}

/// The owner of a delivery the principal controls now (core `may_control`).
/// Unknown deliveries, v1 deliveries and other principals' are not found.
async fn controlled_by(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    id: &str,
) -> Result<Owner, Problem> {
    if !principal.allows(Permission::PlaybackRequest) {
        return Err(access::AccessError::Forbidden(Permission::PlaybackRequest).into());
    }
    let owner = delivery::owner(app, conn, id)
        .await
        .map_err(problem)?
        .ok_or_else(Problem::not_found)?;
    let facts = file_facts(conn, &owner.file).await?;
    if !core::may_control(&owner, principal, facts.as_ref()) {
        return Err(Problem::not_found());
    }
    Ok(owner)
}

/// Read-only authorization (GET, stream bytes): the caller was resolved from
/// current credential state for this request.
async fn observed(app: &App, caller: &Caller, id: &str) -> Result<Owner, Problem> {
    let mut conn = app.db.acquire().await?;
    controlled_by(app, &mut conn, &caller.principal, id).await
}

/// A delivery command authorizes inside the write transaction, so a
/// revocation commits either before the check (and refuses it) or after the
/// effect. Commit the returned transaction after applying the effect.
async fn command<'a>(
    app: &'a App,
    caller: &Caller,
    id: &str,
) -> Result<(sqlx::Transaction<'a, sqlx::Sqlite>, Principal, Owner), Problem> {
    let mut tx = db::begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::PlaybackRequest))
        .await?;
    let owner = controlled_by(app, &mut tx, &principal, id).await?;
    Ok((tx, principal, owner))
}

/// Commands on a delivery this process no longer holds (closed, failed,
/// interrupted by a restart) are conflicts, not unknown resources.
fn require_live(app: &App, id: &str) -> Result<(), Problem> {
    if app.processing.deliveries.is_live(id) {
        Ok(())
    } else {
        Err(Problem::new(
            StatusCode::CONFLICT,
            "delivery_closed",
            "The delivery is closed or was interrupted; plan playback again",
        ))
    }
}

pub async fn get_delivery(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let owner = observed(&app, &caller, &id).await?;
    let view = delivery::any_view(&app, &id)
        .await
        .map_err(problem)?
        .ok_or_else(Problem::not_found)?;
    Ok(Json(delivery_json(&view, &owner)).into_response())
}

/// Idempotent: a delivery that is already closed, failed or interrupted is done.
pub async fn close_delivery(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let (mut tx, _, _) = command(&app, &caller, &id).await?;
    if app.processing.deliveries.is_live(&id) {
        delivery::close_live(&app, &id, &mut tx)
            .await
            .map_err(problem)?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    active_generation: String,
}

pub async fn heartbeat(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    body: Body<Heartbeat>,
) -> Result<Response, Problem> {
    let (tx, _, owner) = command(&app, &caller, &id).await?;
    require_live(&app, &id)?;
    let view = delivery::heartbeat_live(&app, &id, &body.value.active_generation, None)
        .map_err(problem)?;
    tx.commit().await?;
    Ok(Json(delivery_json(&view, &owner)).into_response())
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryChange {
    Seek {
        expected_generation: String,
        position_ms: u64,
    },
    Replan {
        expected_generation: String,
        plan_token: String,
        position_ms: u64,
    },
}

/// Stage a seek or replan. Its acknowledgement is recorded with the staged
/// generation, under the delivery's lock (`delivery::change_live`), so an exact
/// retry of a lost response replays it instead of staging another generation.
pub async fn change_delivery(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body<DeliveryChange>,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    let (mut tx, principal, owner) = command(&app, &caller, &id).await?;
    require_live(&app, &id)?;
    let receipt = delivery::ChangeReceipt {
        principal: principal.id.clone(),
        key,
        digest: body.digest,
    };
    // An acknowledged change replays even if its plan token expired since.
    if let Some(view) = delivery::change_replay(&app, &id, &receipt).map_err(problem)? {
        tx.commit().await?;
        let mut response =
            (StatusCode::ACCEPTED, Json(delivery_json(&view, &owner))).into_response();
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
        return Ok(response);
    }
    let (expected, position, selection) = match body.value {
        DeliveryChange::Seek {
            expected_generation,
            position_ms,
        } => (expected_generation, position_ms, None),
        DeliveryChange::Replan {
            expected_generation,
            plan_token,
            position_ms,
        } => {
            let claims = open(&app, &plan_token)?;
            let facts = file_facts(&mut tx, &claims.file).await?;
            let bound = still_bound(&mut tx, &claims).await?;
            core::admit(
                &claims,
                &core::Observed {
                    principal: &principal,
                    file: facts.as_ref(),
                    bound,
                    now: now(),
                },
            )
            .map_err(plan_problem)?;
            if !core::replan_compatible(&owner, &claims) {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "replan_incompatible",
                    "A different timeline, profile or source needs a new delivery",
                ));
            }
            (
                expected_generation,
                position_ms,
                Some(delivery::Selection {
                    route: claims.route,
                    audio: claims.audio,
                    subtitle: claims.subtitle,
                }),
            )
        }
    };
    let (view, replayed) =
        delivery::change_live(&app, &id, &expected, position, selection, &receipt, &mut tx)
            .await
            .map_err(problem)?;
    tx.commit().await?;
    let mut response = (StatusCode::ACCEPTED, Json(delivery_json(&view, &owner))).into_response();
    if replayed {
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
    }
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateGeneration {
    expected_active_generation: String,
}

/// Activation is idempotent in the core reducer (re-activating the active
/// generation is an acknowledged no-op), so retries need no stored receipt.
pub async fn activate(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    Path((id, generation)): Path<(String, String)>,
    body: Body<ActivateGeneration>,
) -> Result<Response, Problem> {
    idempotency_key(&headers)?;
    let (tx, _, owner) = command(&app, &caller, &id).await?;
    require_live(&app, &id)?;
    let view = delivery::activate_live(
        &app,
        &id,
        &generation,
        Some(&body.value.expected_active_generation),
    )
    .map_err(problem)?;
    tx.commit().await?;
    Ok(Json(delivery_json(&view, &owner)).into_response())
}

// ------------------------------------------------------------------ streams

fn stream_problem(e: ApiError) -> Problem {
    let mut p = problem(e);
    if p.status == StatusCode::SERVICE_UNAVAILABLE {
        p.retry_after = Some(1);
    }
    p
}

pub async fn master_playlist(
    State(app): State<App>,
    caller: Caller,
    Path((id, generation)): Path<(String, String)>,
) -> Result<Response, Problem> {
    observed(&app, &caller, &id).await?;
    let view = delivery::live_view(&app, &id).ok_or_else(Problem::not_found)?;
    let serving = [view.active.as_ref(), view.pending.as_ref()]
        .into_iter()
        .flatten()
        .find(|g| g.generation == generation && g.transport == "hls" && g.manifest_url.is_some())
        .ok_or_else(Problem::not_found)?;
    // The pinned recipe: H.264 (<= 720p) with optional AAC stereo.
    let codecs = if serving.audio_track.is_some() {
        "avc1.64001f,mp4a.40.2"
    } else {
        "avc1.64001f"
    };
    let text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-STREAM-INF:BANDWIDTH=5000000,CODECS=\"{codecs}\"\nvariants/{VARIANT}/index.m3u8\n"
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.apple.mpegurl"),
            (header::CACHE_CONTROL, "private, no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        text,
    )
        .into_response())
}

pub async fn variant_playlist(
    State(app): State<App>,
    caller: Caller,
    Path((id, generation, variant)): Path<(String, String, String)>,
) -> Result<Response, Problem> {
    observed(&app, &caller, &id).await?;
    if variant != VARIANT {
        return Err(Problem::not_found());
    }
    delivery::playlist_with(
        &app,
        &id,
        &generation,
        |i| format!("../../segments/{i}.m4s"),
        "init.mp4",
    )
    .await
    .map_err(stream_problem)
}

pub async fn init_segment(
    State(app): State<App>,
    caller: Caller,
    Path((id, generation, variant)): Path<(String, String, String)>,
) -> Result<Response, Problem> {
    observed(&app, &caller, &id).await?;
    if variant != VARIANT {
        return Err(Problem::not_found());
    }
    delivery::init(State(app), Path((id, generation)))
        .await
        .map_err(stream_problem)
}

pub async fn media_segment(
    State(app): State<App>,
    caller: Caller,
    Path((id, generation, segment)): Path<(String, String, String)>,
) -> Result<Response, Problem> {
    observed(&app, &caller, &id).await?;
    delivery::segment(State(app), Path((id, generation, segment)))
        .await
        .map_err(stream_problem)
}
