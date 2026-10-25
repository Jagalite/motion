//! Preparation transport over the durable processing service. Publication uses
//! the same persisted identity and current authorization under the writer lock.
use super::{Body, PageQuery, Problem, Scope, auth::Caller, etag, idempotency_key, page};
use crate::App;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use playscale_core::access::{self, Idempotent, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod native_renditions;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/processing/capabilities", get(capabilities))
        .route("/processing/recipes", get(recipes))
        .route("/processing/preparations", post(prepare))
        .route("/processing/renditions", get(renditions))
}

pub(crate) fn recipe(id: &str) -> Option<Value> {
    let (name, video, audio, height) = match id {
        "remux_mp4" => ("MP4 stream copy", None, None, None),
        "audio_aac" => ("MP4 with AAC audio", None, Some("aac"), None),
        "h264720p" => ("H.264 720p", Some("h264"), Some("aac"), Some(720)),
        "video_profile" => ("Bounded video profile", None, Some("aac"), None),
        _ => return None,
    };
    // Version describes the existing worker's complete fixed recipe semantics;
    // changes to encoder arguments or validation require a version bump.
    let semantics = match id {
        "video_profile" => {
            "v1;bounded-video-profile;scale-aspect-even;fps-filter-near;passthrough;yuv420p;bitrate-vbv2;veryfast-threads2;x265-pools2-frame-threads2;videotoolbox-no-software;hvc1;aac;metadata-strip;faststart"
        }
        "remux_mp4" => {
            "v1;mp4;v0-copy;a-selected-copy;no-subtitles;no-data;metadata-strip;faststart"
        }
        "audio_aac" => {
            "v1;mp4;v0-copy;a-selected-aac-192k;no-subtitles;no-data;metadata-strip;faststart"
        }
        _ => {
            "v1;mp4;v0-libx264-veryfast-crf23-threads2;yuv420p;scale-min1280x720-aspect-even;a-selected-aac160k;no-subtitles;no-data;metadata-strip;faststart"
        }
    };
    let backends = if matches!(id, "h264720p" | "video_profile") && cfg!(target_os = "macos") {
        vec!["software", "videotoolbox"]
    } else {
        vec!["software"]
    };
    Some(
        json!({"id":id,"digest":format!("sha256:{}",super::sha256(semantics.as_bytes())),"name":name,"purpose":"rendition","output_container":"mp4","video_codec":video,"audio_codec":audio,"max_height":height,"max_bitrate_bps":Value::Null,"allowed_backends":backends,"qualification":"unqualified"}),
    )
}

// Fixed software recipes retain their historical digest. Parameterized recipes
// bind all persisted encoder inputs, so recovery/publication cannot substitute
// a different profile or backend under a previous preparation identity. A
// kept text subtitle (`subtitle`) is such an input too: a worker that predates
// it (and would strip it) computes another digest and cannot publish.
fn recipe_digest(
    id: &str,
    backend: &str,
    profile: Option<&crate::encoding::VideoProfile>,
    subtitle: bool,
) -> Option<String> {
    let descriptor = recipe(id)?;
    let base = descriptor["digest"].as_str()?;
    if profile.is_none() && backend == "software" && id != "video_profile" && !subtitle {
        return Some(base.to_owned());
    }
    if (id == "video_profile") != profile.is_some() {
        return None;
    }
    let mut inputs = json!({"semantics":base,"backend":backend,"video_profile":profile});
    if subtitle {
        inputs["subtitle"] = json!("selected-text-mov_text");
    }
    Some(format!(
        "sha256:{}",
        super::sha256(&serde_json::to_vec(&inputs).ok()?)
    ))
}
fn software_backend() -> String {
    "software".into()
}

async fn recipes(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<super::Page<Value>>, Problem> {
    caller.require(Permission::ProcessingRequest)?;
    let limit = q.limit()?;
    let after = q.after()?;
    let rows = ["audio_aac", "h264720p", "remux_mp4", "video_profile"]
        .into_iter()
        .filter(|id| *id > after)
        .map(|id| (id.into(), recipe(id).unwrap()))
        .collect();
    let mut tx = app.db.begin().await?;
    let result = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(result))
}

async fn capabilities(State(app): State<App>, caller: Caller) -> Result<Json<Value>, Problem> {
    caller.require(Permission::ProcessingRequest)?;
    let _permit = app
        .processing
        .inspection
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "processing_inspection_busy",
                "Processing inspection is busy",
            )
        })?;
    let configured = app.processing.settings.ffmpeg.clone();
    let digest = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let path = if configured.components().count() > 1 {
            configured
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join(&configured))
                .find(|p| p.is_file())
                .ok_or_else(|| anyhow::anyhow!("FFmpeg executable unavailable"))?
        };
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut bytes = [0; 65536];
        loop {
            let n = file.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            hash.update(&bytes[..n]);
        }
        Ok(format!("sha256:{:x}", hash.finalize()))
    })
    .await
    .map_err(Problem::internal)?
    .map_err(|_| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ffmpeg_unavailable",
            "Configured FFmpeg executable cannot be read",
        )
    })?;
    // Availability is an observation, not a qualification receipt. Probe a
    // bounded encoder process; never infer hardware support from the OS.
    let mut command = tokio::process::Command::new(&app.processing.settings.ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=s=16x16:d=0.04",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo",
            "-vf",
            "scale=16:16",
            "-c:a",
            "aac",
            "-t",
            "0.04",
            "-frames:v",
            "1",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-f",
            "null",
            "-",
        ])
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let available = tokio::time::timeout(std::time::Duration::from_secs(5), command.status())
        .await
        .is_ok_and(|r| r.is_ok_and(|s| s.success()));
    // Filters this FFmpeg was observed to have, not assumed.
    let observed = crate::processing::filters(&app).await;
    let mut filters = vec!["scale"];
    if observed.zscale {
        filters.push("zscale");
    }
    if observed.tonemap {
        filters.push("tonemap");
    }
    Ok(Json(
        json!({"ffmpeg_digest":digest,"pipelines":[{"id":"software_h264","decode":"ffmpeg","filters":filters,"encode":"libx264","backend":"software","available":available,"qualification":"unqualified","receipt_id":Value::Null}],"interactive_slots":0,"background_slots":1}),
    ))
}

fn nullable<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::deserialize(d)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Tracks {
    #[serde(deserialize_with = "nullable")]
    pub audio_component_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub subtitle_component_id: Option<String>,
    pub subtitle_policy: String,
    #[serde(deserialize_with = "nullable")]
    pub audio_track_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub subtitle_track_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileRef {
    pub file_id: String,
    pub file_revision: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    source: FileRef,
    timeline_id: String,
    recipe_id: String,
    #[serde(default = "software_backend")]
    backend: String,
    #[serde(default)]
    video_profile: Option<crate::encoding::VideoProfile>,
    tracks: Tracks,
}

pub(crate) async fn exact_binding(
    conn: &mut sqlx::SqliteConnection,
    timeline: &str,
    file: &str,
    revision: &str,
) -> Result<bool, Problem> {
    let versions: Vec<String> = sqlx::query_scalar("SELECT v.id FROM version_files b JOIN media_versions v ON v.id=b.version_id JOIN timelines t ON t.id=v.timeline_id JOIN media_files f ON f.id=b.file_id WHERE v.timeline_id=? AND b.file_id=? AND b.file_revision=? AND v.origin='original' AND f.edition_id=t.edition_id AND b.start_ms IS NULL AND b.end_ms IS NULL AND (SELECT count(*) FROM version_files p WHERE p.version_id=v.id)=1 ORDER BY v.id")
        .bind(timeline).bind(file).bind(revision).fetch_all(&mut *conn).await?;
    for version in versions {
        if super::playback::native_version_ready(conn, &version).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) async fn publication_facts(
    conn: &mut sqlx::SqliteConnection,
    job: &crate::processing::ProcessingJob,
) -> anyhow::Result<Option<(String, playscale_core::processing::PreparationFacts)>> {
    type Stored = (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
    );
    let row:Option<Stored>=sqlx::query_as("SELECT p.timeline_id,p.recipe_digest,p.credential_hash,p.ingress_id,j.requester_id,p.tracks_json FROM preparations p JOIN processing_jobs j ON j.id=p.job_id WHERE p.job_id=?").bind(&job.id).fetch_optional(&mut *conn).await?;
    let Some((timeline, digest, hash, ingress, owner, tracks)) = row else {
        return Ok(None);
    };
    // The selection is part of the preparation's identity (`recipe_digest`).
    let subtitle = serde_json::from_str::<Value>(&tracks)?["subtitle_track_id"].is_string();
    let principal = crate::metadata_jobs::principal(
        conn,
        &owner,
        hash.as_deref(),
        ingress.as_deref(),
        Permission::ProcessingRequest,
    )
    .await
    .ok();
    let facts = super::content::file_facts(conn, &job.source_file_id)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.code))?;
    let native_scope_valid = match &principal {
        Some(principal) => super::playback::require_native_scope(conn, principal)
            .await
            .is_ok(),
        None => false,
    };
    let source_in_scope = native_scope_valid
        && principal.as_ref().is_some_and(|p| {
            facts
                .as_ref()
                .is_some_and(|f| access::catalog_scope(p).admits(f.library_ids.iter()))
        });
    let enabled:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_files f JOIN libraries l ON l.id=f.library_id WHERE f.id=? AND f.available=1 AND l.enabled=1 AND f.generated=0)").bind(&job.source_file_id).fetch_one(&mut *conn).await?;
    let binding = exact_binding(conn, &timeline, &job.source_file_id, &job.source_revision)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.code))?;
    Ok(Some((
        timeline,
        playscale_core::processing::PreparationFacts {
            requester_authorized: principal.is_some(),
            source_enabled: enabled,
            source_in_scope,
            exact_whole_timeline_binding: binding,
            recipe_unchanged: recipe_digest(
                &job.recipe,
                &job.backend,
                job.video_profile.as_deref(),
                subtitle,
            )
            .is_some_and(|current| current == digest),
        },
    )))
}

/// Resolve a preparation's track selection on the exact source revision:
/// components to their occurrence on that file and revision (never another
/// file or language), positional pins checked against its streams, and the
/// two agreeing when both are given. Returns the record the worker encodes
/// and playback matches (`processing::carries`): `a{n}` and `s{n}` ordinals
/// of the revision, with the requested components.
/// `processing::kept_subtitle` as a problem (preparations and downloads).
pub(crate) fn kept_subtitle_problem(policy: &str, explicit: bool) -> Result<(), Problem> {
    use playscale_core::processing::{KeptSubtitleRefusal, kept_subtitle};
    kept_subtitle(policy, explicit).map_err(|refusal| match refusal {
        KeptSubtitleRefusal::PolicyConflict => Problem::invalid(
            "subtitle_policy_conflict",
            "Explicit subtitles conflict with off",
        ),
        KeptSubtitleRefusal::Unsupported => Problem::invalid(
            "preparation_tracks_unsupported",
            "A prepared version keeps a subtitle only when one is chosen explicitly with require",
        ),
    })
}

/// The status of the playscale rendition whose output is `output_file`
/// (core `processing::rendition_status` over the same facts
/// `listRenditions` reports); None when the file is no rendition output.
pub(crate) async fn output_rendition_status(
    conn: &mut sqlx::SqliteConnection,
    output_file: &str,
) -> Result<Option<&'static str>, Problem> {
    type Row = (
        String,
        String,
        String,
        String,
        bool,
        Option<String>,
        String,
        bool,
        bool,
    );
    let row: Option<Row> = sqlx::query_as("SELECT p.timeline_id,j.source_file_id,j.source_revision,j.phase,j.expired,r.file_revision,f.revision,coalesce(o.available=1 AND o.revision=r.file_revision,0),f.available=1 AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1) FROM preparations p JOIN processing_jobs j ON j.id=p.job_id JOIN media_files f ON f.id=j.source_file_id LEFT JOIN media_files o ON o.id=j.output_file_id LEFT JOIN renditions r ON r.source='playscale' AND r.external_id=j.id AND r.file_id=j.output_file_id WHERE j.output_file_id=? ORDER BY j.id DESC LIMIT 1")
        .bind(output_file)
        .fetch_optional(&mut *conn)
        .await?;
    let Some((timeline, file, revision, phase, expired, _, current, available, source_available)) =
        row
    else {
        return Ok(None);
    };
    let binding = exact_binding(conn, &timeline, &file, &revision).await?;
    Ok(Some(playscale_core::processing::rendition_status(
        &playscale_core::processing::RenditionFacts {
            expired,
            phase: serde_json::from_value(json!(phase)).map_err(Problem::internal)?,
            source_current: source_available && current == revision,
            binding_current: binding,
            output_current: available,
        },
    )))
}

pub(crate) async fn selection(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    timeline: &str,
    source: &FileRef,
    streams: &[crate::db::Track],
    requested: Tracks,
) -> Result<Tracks, Problem> {
    use playscale_core::components::{
        Kind, SubtitleOccurrence, embedded_track, subtitle_occurrence,
    };
    if super::playback::native_bound(conn).await?
        && (requested.audio_component_id.is_some() || requested.subtitle_component_id.is_some())
    {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "native_components_unavailable",
            "Native component selection is not qualified",
        ));
    }
    let version: String = sqlx::query_scalar(
        "SELECT v.id FROM version_files b JOIN media_versions v ON v.id=b.version_id \
         WHERE v.timeline_id=? AND b.file_id=? AND b.file_revision=? AND v.origin='original' \
         ORDER BY v.id LIMIT 1",
    )
    .bind(timeline)
    .bind(&source.file_id)
    .bind(&source.file_revision)
    .fetch_one(&mut *conn)
    .await?;
    let components =
        if requested.audio_component_id.is_some() || requested.subtitle_component_id.is_some() {
            crate::components::timeline_components(&app.db, timeline)
                .await
                .map_err(Problem::internal)?
        } else {
            vec![]
        };
    let ordinal = |kind: &str, index: i64| -> Option<u32> {
        streams
            .iter()
            .filter(|t| t.kind == kind)
            .position(|t| t.index == index)
            .map(|i| i as u32)
    };
    let unavailable = || {
        Problem::invalid(
            "component_unavailable",
            "The component does not occur in this source revision",
        )
    };
    let conflict = || {
        Problem::invalid(
            "track_component_conflict",
            "The track and component pins disagree",
        )
    };
    let count = |kind: &str| streams.iter().filter(|t| t.kind == kind).count();
    let audio_track = super::playback::track_index(&requested.audio_track_id, 'a', count("audio"))?;
    let audio_component = match &requested.audio_component_id {
        None => None,
        Some(id) => Some(
            embedded_track(
                &components,
                id,
                Kind::Audio,
                &version,
                &source.file_id,
                &source.file_revision,
            )
            .and_then(|index| ordinal("audio", index))
            .ok_or_else(unavailable)?,
        ),
    };
    if audio_track
        .zip(audio_component)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(conflict());
    }
    let subtitle_track =
        super::playback::track_index(&requested.subtitle_track_id, 's', count("subtitle"))?;
    let subtitle_component = match &requested.subtitle_component_id {
        None => None,
        Some(id) => match subtitle_occurrence(
            &components,
            id,
            &version,
            &source.file_id,
            &source.file_revision,
        ) {
            Some(SubtitleOccurrence::Embedded(index)) => {
                Some(ordinal("subtitle", index).ok_or_else(unavailable)?)
            }
            Some(SubtitleOccurrence::Sidecar(_)) => {
                return Err(Problem::invalid(
                    "preparation_subtitle_unsupported",
                    "A preparation keeps only an embedded text subtitle",
                ));
            }
            None => return Err(unavailable()),
        },
    };
    if subtitle_track
        .zip(subtitle_component)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(conflict());
    }
    let subtitle = subtitle_track.or(subtitle_component);
    // Only text survives as an MP4 subtitle (mov_text) without changing its
    // meaning; bitmap and styled subtitles would need burning in.
    if let Some(n) = subtitle
        && !streams
            .iter()
            .filter(|t| t.kind == "subtitle")
            .nth(n as usize)
            .is_some_and(|t| playscale_core::delivery::sidecar_subtitle(&t.codec))
    {
        return Err(Problem::invalid(
            "preparation_subtitle_unsupported",
            "A preparation keeps only an embedded text subtitle (SubRip, mov_text or WebVTT)",
        ));
    }
    Ok(Tracks {
        audio_track_id: audio_track.or(audio_component).map(|n| format!("a{n}")),
        subtitle_track_id: subtitle.map(|n| format!("s{n}")),
        ..requested
    })
}

async fn prepare(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    caller.require(Permission::ProcessingRequest)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    let recipe = recipe(&input.recipe_id)
        .ok_or_else(|| Problem::invalid("unknown_recipe", "Select a listed recipe"))?;
    if !recipe["allowed_backends"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.as_str() == Some(&input.backend))
    {
        return Err(Problem::invalid(
            "unsupported_backend",
            "Select a recipe backend supported on this platform",
        ));
    }
    if (input.recipe_id == "video_profile") != input.video_profile.is_some() {
        return Err(Problem::invalid(
            "invalid_video_profile",
            "video_profile is required only for the video_profile recipe",
        ));
    }
    if let Some(profile) = &input.video_profile {
        profile
            .validate()
            .map_err(|reason| Problem::invalid("invalid_video_profile", reason))?;
    }
    recipe_digest(
        &input.recipe_id,
        &input.backend,
        input.video_profile.as_ref(),
        false,
    )
    .ok_or_else(|| Problem::invalid("invalid_video_profile", "Invalid recipe inputs"))?;

    if !matches!(
        input.tracks.subtitle_policy.as_str(),
        "off" | "auto" | "require"
    ) {
        return Err(Problem::invalid(
            "invalid_subtitle_policy",
            "Unknown subtitle policy",
        ));
    }
    // A preparation encodes streams of its exact source revision: the chosen
    // audio and, with `require`, one explicitly chosen text subtitle.
    // Automatic subtitle selection depends on a profile, not a rendition.
    let explicit_subtitle =
        input.tracks.subtitle_component_id.is_some() || input.tracks.subtitle_track_id.is_some();
    kept_subtitle_problem(&input.tracks.subtitle_policy, explicit_subtitle)?;
    let _guard = app.jobs.lock().await;
    let mut tx = super::playback::begin_catalog_application(&app, true).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProcessingRequest))
        .await?;
    super::playback::require_native_scope(&mut tx, &principal).await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createPreparation",
        target: "",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|s| &s.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        if app.native_catalog.is_some() {
            super::playback::readable_timeline(&mut tx, &principal, &input.timeline_id).await?;
            let facts = super::content::file_facts(&mut tx, &input.source.file_id)
                .await?
                .ok_or_else(Problem::not_found)?;
            if !access::catalog_scope(&principal).admits(facts.library_ids.iter()) {
                return Err(Problem::not_found());
            }
        }
        let stored = stored.unwrap();
        let mut response = (stored.status, Json(stored.body)).into_response();
        response
            .headers_mut()
            .insert("idempotent-replayed", HeaderValue::from_static("true"));
        response.headers_mut().insert(header::ETAG, etag(1));
        return Ok(response);
    }
    super::playback::readable_timeline(&mut tx, &principal, &input.timeline_id).await?;
    let facts = super::content::file_facts(&mut tx, &input.source.file_id)
        .await?
        .ok_or_else(Problem::not_found)?;
    if !access::catalog_scope(&principal).admits(facts.library_ids.iter()) {
        return Err(Problem::not_found());
    }
    let source:crate::db::ItemRow=sqlx::query_as("SELECT * FROM catalog_files WHERE id=? AND generated=0 AND available=1 AND library_id IN (SELECT id FROM libraries WHERE enabled=1)").bind(&input.source.file_id).fetch_optional(&mut *tx).await?.ok_or_else(Problem::not_found)?;
    playscale_core::processing::SourceAdmission {
        revision: &source.revision,
        duration: source.duration_seconds,
    }
    .check(&input.source.file_revision)
    .map_err(|code| {
        Problem::new(
            StatusCode::CONFLICT,
            code,
            "Refresh the source and its probe",
        )
    })?;
    if !exact_binding(
        &mut tx,
        &input.timeline_id,
        &input.source.file_id,
        &input.source.file_revision,
    )
    .await?
    {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "preparation_timeline_mismatch",
            "Preparation requires an exact whole-file original version on the requested timeline",
        ));
    }
    let tracks: Vec<crate::db::Track> =
        serde_json::from_str(&source.tracks_json).map_err(Problem::internal)?;
    if matches!(input.recipe_id.as_str(), "h264720p" | "video_profile")
        && !tracks.iter().find(|t| t.kind == "video").is_some_and(|t| {
            t.kind == "video"
                && t.width.is_some()
                && t.height.is_some()
                && !matches!(
                    t.color_transfer.as_deref(),
                    Some("smpte2084" | "arib-std-b67")
                )
        })
    {
        return Err(Problem::invalid(
            "preparation_video_unsupported",
            "Video preparation requires a probed SDR video stream with known dimensions",
        ));
    }
    if input
        .video_profile
        .as_ref()
        .is_some_and(|profile| !profile.admits_duration(source.duration_seconds.unwrap_or(0.0)))
    {
        return Err(Problem::invalid(
            "invalid_video_profile",
            "Source is shorter than half a requested frame interval",
        ));
    }
    let tracks_record = selection(
        &app,
        &mut tx,
        &input.timeline_id,
        &input.source,
        &tracks,
        input.tracks,
    )
    .await?;
    let digest = recipe_digest(
        &input.recipe_id,
        &input.backend,
        input.video_profile.as_ref(),
        tracks_record.subtitle_track_id.is_some(),
    )
    .ok_or_else(|| Problem::invalid("invalid_video_profile", "Invalid recipe inputs"))?;
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM processing_jobs WHERE phase IN ('queued','running','cancelling')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !playscale_core::processing::queue_admits(queued) {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "processing_limit",
            "Processing queue is full",
        ));
    }
    let id = crate::new_id();
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at,requester_id,video_profile) VALUES (?,?,?,?,?,?,'queued',?,?,?,?)").bind(&id).bind(&input.source.file_id).bind(&input.source.file_revision).bind(&input.recipe_id).bind(&input.backend).bind(format!("v2-{id}")).bind(crate::now()).bind(crate::now()).bind(&principal.id).bind(input.video_profile.as_ref().map(sqlx::types::Json)).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO preparations(job_id,timeline_id,recipe_digest,tracks_json,credential_hash,ingress_id) VALUES (?,?,?,?,?,?)").bind(&id).bind(&input.timeline_id).bind(&digest).bind(serde_json::to_string(&tracks_record).map_err(Problem::internal)?).bind(caller.credential().map(|(hash,_)|hash)).bind(caller.ingress_device_id()).execute(&mut *tx).await?;
    if app.native_catalog.is_some() {
        crate::native_processing::capture_admission(
            &app,
            &mut tx,
            &id,
            &input.timeline_id,
            &input.source.file_id,
            &input.source.file_revision,
        )
        .await
        .map_err(Problem::internal)?;
    }
    let value = super::jobs::row(&mut tx, &id, &principal).await?.wire();
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::ACCEPTED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    let mut response = (StatusCode::ACCEPTED, Json(value)).into_response();
    response.headers_mut().insert(header::ETAG, etag(1));
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenditionQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    /// Only this timeline's renditions. A filter narrows the authorized rows;
    /// an unreadable or unknown timeline yields an empty page, never an error.
    timeline_id: Option<String>,
}

async fn renditions(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<RenditionQuery>,
) -> Result<Json<super::Page<Value>>, Problem> {
    caller.require(Permission::ProcessingRequest)?;
    if query
        .timeline_id
        .as_deref()
        .is_some_and(|id| !super::catalog::valid_id(id))
    {
        return Err(Problem::bad(
            "invalid_query",
            "timeline_id must be a valid ID",
        ));
    }
    if app.native_catalog.is_some() {
        return native_renditions::list(&app, &caller, query).await;
    }
    let q = PageQuery {
        cursor: query.cursor,
        limit: query.limit,
    };
    let mut tx = super::playback::begin_catalog_application(&app, false).await?;
    let limit = q.limit()?;
    let (clause, all, ids) =
        super::scope_clause(&access::catalog_scope(&caller.principal), "f.library_id");
    // A given timeline is a direct predicate, so its index drives the read.
    let timeline = if query.timeline_id.is_some() {
        "p.timeline_id=?"
    } else {
        "? IS NULL"
    };
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        bool,
        String,
        String,
    );
    let rows:Vec<Row>=sqlx::query_as(&format!("SELECT j.id,p.timeline_id,j.source_file_id,j.source_revision,p.recipe_digest,j.phase,p.revision,j.expired,j.output_file_id,r.file_revision,f.revision,coalesce(o.available=1 AND o.revision=r.file_revision,0),f.available=1 AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1),j.recipe,p.tracks_json FROM preparations p JOIN processing_jobs j ON j.id=p.job_id JOIN media_files f ON f.id=j.source_file_id LEFT JOIN media_files o ON o.id=j.output_file_id LEFT JOIN renditions r ON r.source='playscale' AND r.external_id=j.id AND r.file_id=j.output_file_id WHERE j.id>? AND {timeline} AND {clause} ORDER BY j.id LIMIT ?")).bind(q.after()?).bind(&query.timeline_id).bind(all).bind(ids).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let mut values = Vec::new();
    for (
        id,
        timeline,
        file,
        revision,
        digest,
        phase,
        resource_revision,
        expired,
        output,
        output_revision,
        current,
        available,
        source_available,
        recipe,
        tracks,
    ) in rows
    {
        let binding = exact_binding(&mut tx, &timeline, &file, &revision).await?;
        let status = playscale_core::processing::rendition_status(
            &playscale_core::processing::RenditionFacts {
                expired,
                phase: serde_json::from_value(json!(phase)).map_err(Problem::internal)?,
                source_current: source_available && current.as_deref() == Some(&revision),
                binding_current: binding,
                output_current: available,
            },
        );
        let output = output.zip(output_revision).map(
            |(file_id, file_revision)| json!({"file_id":file_id,"file_revision":file_revision}),
        );
        // The selection it kept, as recorded on its source revision; a
        // download names exactly this (`createDownload` with `recipe_id`),
        // and its generated version pins exactly this rendition.
        let tracks = serde_json::from_str::<Value>(&tracks).unwrap_or(Value::Null);
        let version: Option<String> = match &output {
            // Only a downloadable binding: the whole output at its revision,
            // as the only part (what `createDownload` admits).
            Some(o) => sqlx::query_scalar("SELECT v.id FROM media_versions v JOIN version_files b ON b.version_id=v.id AND b.part=1 WHERE v.timeline_id=? AND v.origin='generated' AND b.file_id=? AND b.file_revision=? AND b.start_ms IS NULL AND b.end_ms IS NULL AND NOT EXISTS(SELECT 1 FROM version_files x WHERE x.version_id=v.id AND x.part<>1) ORDER BY v.id LIMIT 1")
                .bind(&timeline)
                .bind(o["file_id"].as_str().unwrap_or_default())
                .bind(o["file_revision"].as_str().unwrap_or_default())
                .fetch_optional(&mut *tx)
                .await?,
            None => None,
        };
        values.push((id.clone(),json!({"id":id,"revision":resource_revision.to_string(),"timeline_id":timeline,"source":{"file_id":file,"file_revision":revision},"output":output,"recipe_digest":digest,"job_id":id,"status":status,"recipe_id":recipe,"tracks":tracks,"version_id":version})));
    }
    let result = page(&mut tx, &caller.principal, values, limit).await?;
    tx.commit().await?;
    Ok(Json(result))
}

#[cfg(test)]
mod recipe_identity_tests {
    use super::*;
    #[test]
    fn publication_digest_binds_profile_and_backend_without_changing_fixed_software() {
        let mut profile = crate::encoding::VideoProfile {
            version: 1,
            codec: crate::encoding::VideoCodec::H264,
            max_width: 320,
            max_height: 180,
            video_bitrate: 500_000,
            audio_bitrate: 96_000,
            frame_rate: None,
        };
        let initial = recipe_digest("video_profile", "software", Some(&profile), false).unwrap();
        assert_ne!(
            Some(initial.clone()),
            recipe_digest("video_profile", "videotoolbox", Some(&profile), false)
        );
        profile.max_width = 640;
        assert_ne!(
            Some(initial),
            recipe_digest("video_profile", "software", Some(&profile), false)
        );
        assert_eq!(
            recipe_digest("remux_mp4", "software", None, false).unwrap(),
            recipe("remux_mp4").unwrap()["digest"].as_str().unwrap()
        );
        // A kept subtitle is part of the identity: a worker that would strip
        // it computes the historical digest and cannot publish the job.
        assert_ne!(
            recipe_digest("remux_mp4", "software", None, true).unwrap(),
            recipe_digest("remux_mp4", "software", None, false).unwrap()
        );
        assert!(recipe_digest("video_profile", "software", None, false).is_none());
    }
}
