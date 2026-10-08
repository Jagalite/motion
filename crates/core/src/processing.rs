//! Durable processing facts and completion admission. Persist the returned job
//! and any Publish effect atomically; failed persistence leaves the old state.
use crate::jobs::{self, Effect, Input, Job};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct State {
    pub job: Job,
    pub source_revision: String,
    pub published_output: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Completion {
    pub attempt: u32,
    /// Revision read from the authoritative catalog under the writer lock.
    /// None means that the source is unavailable.
    pub current_source_revision: Option<String>,
    /// Only supplied after the adapter has validated the output bytes.
    pub validated_output: Option<String>,
}

pub fn finish(state: &State, result: &Completion) -> (State, Vec<Effect>) {
    // Duplicate or superseded completions cannot alter even ancillary metadata.
    if result.attempt != state.job.attempt || state.published_output.is_some() {
        return (state.clone(), vec![]);
    }
    let success = result.validated_output.is_some()
        && result.current_source_revision.as_ref() == Some(&state.source_revision);
    let (job, effects) = jobs::transition(
        &state.job,
        Input::Finished {
            attempt: result.attempt,
            success,
        },
    );
    let mut next = State {
        job,
        ..state.clone()
    };
    if effects
        == [Effect::Publish {
            attempt: result.attempt,
        }]
    {
        next.published_output = result.validated_output.clone();
    }
    (next, effects)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestIdentity {
    pub source_file: String,
    pub source_revision: String,
    pub recipe: String,
    pub backend: String,
    pub options: Option<serde_json::Value>,
}
pub fn reuse(existing: &RequestIdentity, requested: &RequestIdentity) -> Result<(), &'static str> {
    if existing == requested {
        Ok(())
    } else {
        Err("idempotency_conflict")
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub kind: String,
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
}
#[derive(Clone, Debug)]
pub struct VideoRequirements {
    pub codec: String,
    pub max_width: u32,
    pub max_height: u32,
    pub frame_rate: Option<f64>,
}
pub struct OutputObservation<'a> {
    pub bytes: i64,
    pub duration: Option<f64>,
    pub tracks: &'a [Track],
}
pub struct OutputRequirements<'a> {
    pub max_bytes: u64,
    pub source_duration: f64,
    pub source_tracks: &'a [Track],
    pub recipe: &'a str,
    pub video: Option<VideoRequirements>,
}
pub fn validate_output(
    output: &OutputObservation<'_>,
    required: &OutputRequirements<'_>,
) -> Result<(), &'static str> {
    if output.bytes <= 0 || (output.bytes as u64).saturating_add(65536) >= required.max_bytes {
        return Err("output budget exceeded");
    }
    if !output.duration.is_some_and(|d| {
        (d - required.source_duration).abs() <= 0.5f64.max(required.source_duration * 0.01)
    }) || output.tracks.is_empty()
    {
        return Err("output is truncated or unprobeable");
    }
    for kind in ["video", "audio"] {
        if required.source_tracks.iter().any(|t| t.kind == kind)
            && !output.tracks.iter().any(|t| t.kind == kind)
        {
            return Err("output lost required track");
        }
    }
    if let Some(profile) = &required.video {
        let video = output
            .tracks
            .iter()
            .find(|t| t.kind == "video")
            .ok_or("missing output video")?;
        if video.codec != profile.codec
            || video.width.is_none_or(|w| w > profile.max_width)
            || video.height.is_none_or(|h| h > profile.max_height)
        {
            return Err("output violates video profile");
        }
        if let Some(rate) = profile.frame_rate
            && !video
                .frame_rate
                .is_some_and(|actual| (actual - rate).abs() < 0.01)
        {
            return Err("output frame rate mismatch");
        }
    }
    if required.recipe == "h264720p"
        && output
            .tracks
            .iter()
            .any(|t| t.kind == "video" && t.codec != "h264")
    {
        return Err("wrong video codec");
    }
    if required.recipe != "remux_mp4"
        && output
            .tracks
            .iter()
            .any(|t| t.kind == "audio" && t.codec != "aac")
    {
        return Err("wrong audio codec");
    }
    Ok(())
}

pub struct SourceAdmission<'a> {
    pub revision: &'a str,
    pub duration: Option<f64>,
}
impl SourceAdmission<'_> {
    pub fn check(&self, expected_revision: &str) -> Result<(), &'static str> {
        if self.revision != expected_revision {
            return Err("source_revision_changed");
        }
        if self.duration.is_none_or(|d| !d.is_finite() || d <= 0.0) {
            return Err("invalid_source_duration");
        }
        Ok(())
    }
}
pub fn queue_admits(active_jobs: i64) -> bool {
    active_jobs < 100
}
