//! Read-only playback plans. Effects remain in the explicit processing/session APIs.
use crate::{
    App,
    api::{ApiError, json},
    processing, renditions, viewing,
};
use axum::{
    Json,
    extract::{Path, State},
};
use playscale_core::playback::{Candidate, Decision, Mode, Support, decide};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub fn default_recipe() -> String {
    "h264720p".into()
}
pub fn valid_recipe(value: &str) -> bool {
    matches!(value, "remux_mp4" | "audio_aac" | "h264720p")
}
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub file_id: String,
    pub revision: String,
}
#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClientSupport {
    pub file_id: String,
    pub revision: String,
    /// supported, unsupported, or unknown for this exact file on this client.
    pub support: String,
}
#[derive(Default, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Request {
    /// Overrides the profile default: auto, original, or convert.
    pub mode: Option<String>,
    /// remux_mp4, audio_aac, h264720p, or video_profile. Overrides the viewer recipe.
    pub recipe: Option<String>,
    /// Required exactly when recipe is video_profile; immutable encode parameters.
    pub video_profile: Option<crate::encoding::VideoProfile>,
    /// software (default) or videotoolbox. Only used for new processing proposals.
    pub backend: Option<String>,
    /// An explicit version is pinned: failure never silently selects another version.
    pub selected: Option<FileIdentity>,
    /// Pin an original and its derived renditions, preserving the chosen edition across mode changes.
    pub source: Option<FileIdentity>,
    /// Full-file client capability evidence. Omitted means unknown; use failed_versions for open errors.
    pub client_support: Vec<ClientSupport>,
    /// Failed attempts to skip for this request, without claiming a codec incompatibility.
    pub failed_versions: Vec<FileIdentity>,
    /// Average file bitrate ceiling, not a peak/network throughput guarantee.
    pub max_average_bitrate: Option<u64>,
}
#[derive(Serialize, ToSchema)]
pub struct Selection {
    pub file_id: String,
    pub revision: String,
    pub media_url: String,
    pub label: String,
    pub source_file_id: String,
    pub source_revision: String,
    /// original or existing_rendition; does not describe browser decoding.
    pub delivery: String,
    /// original, remux, audio_conversion, video_transcode, or unknown.
    pub operation: String,
    pub client_support: String,
}
#[derive(Serialize, ToSchema)]
pub struct Preparation {
    pub source_file_id: String,
    pub source_revision: String,
    pub recipe: String,
    pub backend: String,
    pub operation: String,
    pub requires_admin: bool,
    /// Reuse this active job instead of submitting another one.
    pub job_id: Option<String>,
    pub ready_before_playback: bool,
    pub video_profile: Option<crate::encoding::VideoProfile>,
}
#[derive(Serialize, ToSchema)]
pub struct Plan {
    pub mode: String,
    /// ready, preparation_required, or blocked.
    pub status: String,
    pub reason: String,
    pub selection: Option<Selection>,
    pub preparation: Option<Preparation>,
    pub warnings: Vec<String>,
}
fn operation(recipe: &str) -> &'static str {
    match recipe {
        "remux_mp4" => "remux",
        "audio_aac" => "audio_conversion",
        "h264720p" | "video_profile" => "video_transcode",
        _ => "unknown",
    }
}

#[utoipa::path(operation_id="plan_playback",post,path="/api/v1/profiles/{profile}/items/{id}/playback-plan",params(("profile"=String,Path),("id"=String,Path)),request_body=Request,responses((status=200,description="Read-only plan; never starts processing or a session",body=Plan),(status=400,description="Invalid mode, constraints or client evidence",body=crate::api::ErrorBody),(status=404,description="Unknown profile or item",body=crate::api::ErrorBody),(status=409,description="Explicit version no longer belongs to this item at this revision",body=crate::api::ErrorBody)))]
pub async fn plan(
    State(app): State<App>,
    Path((profile, id)): Path<(String, String)>,
    body: Result<Json<Request>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Plan>, ApiError> {
    let request = json(body)?;
    let prefs = viewing::prefs(&app, &profile).await?.preferences;
    let mode = request.mode.as_deref().unwrap_or(&prefs.quality);
    let policy = match mode {
        "auto" => Mode::Auto,
        "original" => Mode::Original,
        "convert" => Mode::Convert,
        _ => return Err(ApiError::bad("Expected auto, original, or convert")),
    };
    let recipe = request
        .recipe
        .as_deref()
        .unwrap_or(&prefs.conversion_recipe);
    let backend = request.backend.as_deref().unwrap_or("software");
    if (recipe == "video_profile") != request.video_profile.is_some() {
        return Err(ApiError::bad(
            "video_profile is required only for the video_profile recipe",
        ));
    }
    if let Some(profile) = &request.video_profile {
        profile.validate().map_err(ApiError::bad)?;
    }
    if (!valid_recipe(recipe) && recipe != "video_profile")
        || !matches!(backend, "software" | "videotoolbox")
        || request.max_average_bitrate == Some(0)
        || request.client_support.len() > 64
        || request.failed_versions.len() > 64
    {
        return Err(ApiError::bad(
            "Invalid recipe, backend, bitrate, or evidence count",
        ));
    }
    if request
        .failed_versions
        .iter()
        .any(|v| v.file_id.len() > 256 || v.revision.len() > 256)
    {
        return Err(ApiError::bad("Invalid failed version identity"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for evidence in &request.client_support {
        if !matches!(
            evidence.support.as_str(),
            "supported" | "unsupported" | "unknown"
        ) || evidence.file_id.len() > 256
            || evidence.revision.len() > 256
            || !seen.insert((&evidence.file_id, &evidence.revision))
        {
            return Err(ApiError::bad("Invalid or duplicate client evidence"));
        }
    }
    // Scans, rendition registration, processing publication and cleanup use this lock.
    let _guard = app.jobs.lock().await;
    let mut choices = renditions::choices(State(app.clone()), Path(id)).await?.0;
    if let Some(source) = &request.source {
        if !choices
            .originals
            .iter()
            .any(|c| c.file_id == source.file_id && c.revision == source.revision)
        {
            return Err(ApiError::conflict(
                "playback_source_conflict",
                "Source does not match an original of this item at this revision",
            ));
        }
        choices.originals.retain(|c| c.file_id == source.file_id);
        choices
            .renditions
            .retain(|c| c.source_file_id == source.file_id && c.source_revision == source.revision);
    }
    let mut selections = Vec::new();
    let mut candidates = Vec::new();
    for original in choices.originals {
        let profile_source = request.video_profile.is_none()
            || original
                .tracks
                .iter()
                .find(|t| t.kind == "video")
                .is_some_and(|t| {
                    t.width.is_some()
                        && t.height.is_some()
                        && !matches!(
                            t.color_transfer.as_deref(),
                            Some("smpte2084" | "arib-std-b67")
                        )
                });
        let processable = profile_source
            && request
                .video_profile
                .as_ref()
                .is_none_or(|p| p.admits_duration(original.duration_seconds.unwrap_or(0.0)))
            && original
                .duration_seconds
                .is_some_and(|v| v.is_finite() && v > 0.0);
        selections.push(Selection {
            source_file_id: original.file_id.clone(),
            source_revision: original.revision.clone(),
            file_id: original.file_id,
            revision: original.revision,
            media_url: original.media_url,
            label: original.edition_label,
            delivery: "original".into(),
            operation: "original".into(),
            client_support: "unknown".into(),
        });
        candidates.push((
            Candidate {
                original: true,
                available: original.available,
                support: Support::Unknown,
                matches_recipe: false,
                within_budget: true,
                can_process: processable,
            },
            original.bytes,
            original.duration_seconds,
        ));
    }
    for rendition in choices.renditions {
        let (bytes, duration): (i64, Option<f64>) =
            sqlx::query_as("SELECT bytes,duration_seconds FROM media_files WHERE id=?")
                .bind(&rendition.file_id)
                .fetch_one(&app.db)
                .await?;
        // Registration provenance is informational. Only completed local jobs establish
        // that a rendition satisfies one of our fixed recipe contracts.
        let actual_recipe: Option<String> = sqlx::query_scalar(
            "SELECT p.recipe FROM processing_jobs p JOIN renditions published ON published.source='playscale' AND published.external_id=p.id AND published.file_id=p.output_file_id AND published.source_file_id=p.source_file_id AND published.source_revision=p.source_revision WHERE p.output_file_id=? AND p.source_file_id=? AND p.source_revision=? AND published.file_revision=? AND p.phase='completed' AND p.expired=0 ORDER BY p.id LIMIT 1")
            .bind(&rendition.file_id).bind(&rendition.source_file_id).bind(&rendition.source_revision).bind(&rendition.file_revision)
            .fetch_optional(&app.db).await?;
        let actual_profile: Option<sqlx::types::Json<crate::encoding::VideoProfile>> = sqlx::query_scalar("SELECT video_profile FROM processing_jobs WHERE output_file_id=? AND phase='completed' AND expired=0 ORDER BY id LIMIT 1")
            .bind(&rendition.file_id).fetch_optional(&app.db).await?.flatten();
        selections.push(Selection {
            source_file_id: rendition.source_file_id,
            source_revision: rendition.source_revision,
            file_id: rendition.file_id,
            revision: rendition.file_revision,
            media_url: rendition.media_url,
            label: rendition.label,
            delivery: "existing_rendition".into(),
            operation: operation(actual_recipe.as_deref().unwrap_or("")).into(),
            client_support: "unknown".into(),
        });
        candidates.push((
            Candidate {
                original: false,
                available: rendition.available,
                support: Support::Unknown,
                matches_recipe: actual_recipe.as_deref() == Some(recipe)
                    && actual_profile.as_deref() == request.video_profile.as_ref(),
                within_budget: true,
                can_process: false,
            },
            bytes,
            duration,
        ));
    }
    for (selection, (candidate, bytes, duration)) in selections.iter_mut().zip(&mut candidates) {
        if let Some(evidence) = request
            .client_support
            .iter()
            .find(|e| e.file_id == selection.file_id && e.revision == selection.revision)
        {
            selection.client_support = evidence.support.clone();
            candidate.support = match evidence.support.as_str() {
                "supported" => Support::Supported,
                "unsupported" => Support::Unsupported,
                _ => Support::Unknown,
            };
        }
        if request
            .failed_versions
            .iter()
            .any(|v| v.file_id == selection.file_id && v.revision == selection.revision)
        {
            // Still eligible as a processing source: failure is not evidence the source is missing.
            candidate.support = Support::Unsupported;
        }
        if let Some(limit) = request.max_average_bitrate {
            candidate.within_budget = duration.is_some_and(|seconds| {
                seconds.is_finite()
                    && seconds > 0.0
                    && *bytes >= 0
                    && (*bytes as f64 * 8.0 / seconds) <= limit as f64
            });
        }
    }
    let selected = request
        .selected
        .as_ref()
        .map(|identity| {
            selections
                .iter()
                .position(|c| c.file_id == identity.file_id && c.revision == identity.revision)
                .ok_or_else(|| {
                    ApiError::conflict(
                        "playback_revision_conflict",
                        "Selected version does not match this item and revision",
                    )
                })
        })
        .transpose()?;
    let candidates: Vec<_> = candidates.into_iter().map(|c| c.0).collect();
    let mut plan = Plan {
        mode: mode.into(),
        status: "blocked".into(),
        reason: String::new(),
        selection: None,
        preparation: None,
        warnings: vec![],
    };
    match decide(policy, &candidates, selected) {
        Decision::Play(index) => {
            let selection = selections.swap_remove(index);
            plan.reason = if selection.client_support == "unknown" {
                "client_check_required"
            } else {
                "client_supported_version"
            }
            .into();
            plan.status = "ready".into();
            plan.selection = Some(selection);
        }
        Decision::Blocked(reason) => plan.reason = reason.into(),
        Decision::Prepare(index) => {
            let caps = processing::capabilities(State(app.clone())).await?.0;
            let admitted = caps.backends.iter().any(|b| {
                matches!(
                    (b, backend),
                    (processing::Backend::Software, "software")
                        | (processing::Backend::Videotoolbox, "videotoolbox")
                )
            });
            if !admitted
                || (backend == "videotoolbox" && !matches!(recipe, "h264720p" | "video_profile"))
            {
                plan.reason = "processing_backend_unavailable".into();
            } else {
                let source = &selections[index];
                let job_id = sqlx::query_scalar("SELECT id FROM processing_jobs WHERE source_file_id=? AND source_revision=? AND recipe=? AND backend=? AND video_profile IS ? AND phase IN ('queued','running') ORDER BY created_at,id LIMIT 1")
                    .bind(&source.file_id).bind(&source.revision).bind(recipe).bind(backend).bind(request.video_profile.clone().map(sqlx::types::Json)).fetch_optional(&app.db).await?;
                plan.status = "preparation_required".into();
                plan.reason = "explicit_processing_required".into();
                plan.preparation = Some(Preparation {
                    source_file_id: source.file_id.clone(),
                    source_revision: source.revision.clone(),
                    recipe: recipe.into(),
                    backend: backend.into(),
                    operation: operation(recipe).into(),
                    requires_admin: true,
                    job_id,
                    ready_before_playback: true,
                    video_profile: request.video_profile.clone(),
                });
                plan.warnings.push("Processing must finish and validate before playback; client compatibility remains unverified.".into());
                plan.warnings.push("Fixed recipes use the first video/audio streams and omit subtitles; HDR handling is not qualified.".into());
                if !caps.validated_jobs.iter().any(|j| j.backend == backend) {
                    plan.warnings.push("No successful H.264 job evidence is recorded for this encoder on this installation.".into());
                }
            }
        }
    }
    if request.max_average_bitrate.is_some() {
        plan.warnings.push("The bitrate constraint uses whole-file averages, not peaks. Proposed conversions are not guaranteed to meet it; re-plan after completion.".into());
    }
    Ok(Json(plan))
}
