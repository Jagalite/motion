//! Durable processing facts and completion admission. Persist the returned job
//! and any Publish effect atomically; failed persistence leaves the old state.
use crate::jobs::{self, Effect, Input, Job};
use serde::{Deserialize, Serialize};

/// DB-only restore cannot reuse disposable outputs belonging to another cache
/// root. Keep source files and unmanaged libraries outside this authority.
pub fn restored_output_unavailable(
    generated: bool,
    managed: bool,
    root: &str,
    current_root: &str,
) -> bool {
    generated && managed && root != current_root
}
pub fn restored_receipt_expired(
    phase: crate::jobs::Phase,
    generated: bool,
    available: bool,
) -> bool {
    phase == crate::jobs::Phase::Completed && generated && !available
}
/// A held witness of unknown provenance may be a hardware encoder; reserve
/// conservatively until its process ownership witness is released.
pub fn recovered_hardware_required(backend: Option<&str>) -> bool {
    backend != Some("software")
}

/// Platform support is observed by the adapter; recipe/backend compatibility
/// is one shared policy used by both API generations.
pub fn backend_allowed(recipe: &str, backend: &str, videotoolbox_available: bool) -> bool {
    matches!(
        recipe,
        "remux_mp4" | "audio_aac" | "h264720p" | "video_profile"
    ) && (backend == "software"
        || (backend == "videotoolbox"
            && videotoolbox_available
            && matches!(recipe, "h264720p" | "video_profile")))
}

/// Technical stream observations supplied by the probe adapter. Indices are
/// demuxer indices; returned selections are ordinals within each stream kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparationStream {
    pub index: i64,
    pub kind: String,
    pub codec: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparationSelection {
    pub audio_track: Option<String>,
    pub subtitle_track: Option<String>,
    pub audio_component: Option<String>,
    pub subtitle_component: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedStreams {
    pub audio: Option<u32>,
    pub subtitle: Option<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionRefusal {
    UnknownTrack,
    ComponentUnavailable,
    ConflictingPins,
    UnsupportedSubtitle,
}

pub fn preparation_selection(
    requested: &PreparationSelection,
    streams: &[PreparationStream],
    components: &[crate::components::Component],
    version: &str,
    file: &str,
    revision: &str,
) -> Result<SelectedStreams, SelectionRefusal> {
    use crate::components::{Kind, SubtitleOccurrence, embedded_track, subtitle_occurrence};
    let ordinal = |kind: &str, index| {
        streams
            .iter()
            .filter(|t| t.kind == kind)
            .position(|t| t.index == index)
            .map(|n| n as u32)
    };
    let pin = |id: &Option<String>,
               prefix: char,
               kind: &str|
     -> Result<Option<u32>, SelectionRefusal> {
        id.as_ref()
            .map(|id| {
                id.strip_prefix(prefix)
                    .and_then(|n| n.parse::<u32>().ok())
                    .filter(|n| (*n as usize) < streams.iter().filter(|t| t.kind == kind).count())
                    .ok_or(SelectionRefusal::UnknownTrack)
            })
            .transpose()
    };
    let audio_track = pin(&requested.audio_track, 'a', "audio")?;
    let audio_component = requested
        .audio_component
        .as_ref()
        .map(|id| {
            embedded_track(components, id, Kind::Audio, version, file, revision)
                .and_then(|index| ordinal("audio", index))
                .ok_or(SelectionRefusal::ComponentUnavailable)
        })
        .transpose()?;
    if audio_track
        .zip(audio_component)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(SelectionRefusal::ConflictingPins);
    }
    let subtitle_track = pin(&requested.subtitle_track, 's', "subtitle")?;
    let subtitle_component = requested
        .subtitle_component
        .as_ref()
        .map(
            |id| match subtitle_occurrence(components, id, version, file, revision) {
                Some(SubtitleOccurrence::Embedded(index)) => {
                    ordinal("subtitle", index).ok_or(SelectionRefusal::ComponentUnavailable)
                }
                Some(SubtitleOccurrence::Sidecar(_)) => Err(SelectionRefusal::UnsupportedSubtitle),
                None => Err(SelectionRefusal::ComponentUnavailable),
            },
        )
        .transpose()?;
    if subtitle_track
        .zip(subtitle_component)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(SelectionRefusal::ConflictingPins);
    }
    let subtitle = subtitle_track.or(subtitle_component);
    if subtitle.is_some_and(|n| {
        !streams
            .iter()
            .filter(|t| t.kind == "subtitle")
            .nth(n as usize)
            .is_some_and(|t| crate::delivery::sidecar_subtitle(&t.codec))
    }) {
        return Err(SelectionRefusal::UnsupportedSubtitle);
    }
    Ok(SelectedStreams {
        audio: audio_track.or(audio_component),
        subtitle,
    })
}

/// Encoder profile bounds shared by legacy and v2 submission. Argument spelling
/// and probing remain adapter mechanics, not separate eligibility authorities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoProfileFacts {
    pub version: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub video_bitrate: u64,
    pub audio_bitrate: u64,
    pub frame_rate: Option<(u32, u32)>,
}
impl VideoProfileFacts {
    pub fn valid(&self) -> bool {
        self.version == 1
            && (128..=3840).contains(&self.max_width)
            && (72..=2160).contains(&self.max_height)
            && self.max_width.is_multiple_of(2)
            && self.max_height.is_multiple_of(2)
            && (64_000..=80_000_000).contains(&self.video_bitrate)
            && (32_000..=320_000).contains(&self.audio_bitrate)
            && self.frame_rate.is_none_or(|(n, d)| {
                n > 0
                    && d > 0
                    && n <= 120_000
                    && d <= 100_000
                    && (1.0..=120.0).contains(&(f64::from(n) / f64::from(d)))
            })
    }
    pub fn admits_duration(&self, duration: f64) -> bool {
        duration.is_finite()
            && duration > 0.0
            && self.frame_rate.is_none_or(|(n, d)| {
                n > 0 && d > 0 && duration * (f64::from(n) / f64::from(d)) >= 0.5
            })
    }
}

/// The legacy fixed 720p recipe historically admits unprobed input; its encoder
/// validates the actual output. The preparation API promises probe-qualified
/// video before admission. Preserve this API compatibility difference explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceContract {
    Legacy,
    Preparation,
}
pub fn source_observation_required(contract: SourceContract, recipe: &str) -> bool {
    recipe == "video_profile" || (contract == SourceContract::Preparation && recipe == "h264720p")
}

pub fn video_source_eligible(
    contract: SourceContract,
    recipe: &str,
    video: Option<(Option<u32>, Option<u32>, Option<&str>)>,
) -> bool {
    !source_observation_required(contract, recipe)
        || video.is_some_and(|(width, height, transfer)| {
            width.is_some()
                && height.is_some()
                && !matches!(transfer, Some("smpte2084" | "arib-std-b67"))
        })
}

/// Preserve fixed recipe receipts while binding every parameterized encoder
/// input. `semantics` is the versioned adapter descriptor for actual FFmpeg
/// arguments/validation; changing that contract must change its digest.
pub fn recipe_identity(
    id: &str,
    semantics: &str,
    backend: &str,
    profile: Option<serde_json::Value>,
    subtitle: bool,
) -> Option<String> {
    use sha2::{Digest, Sha256};
    if profile.is_none() && backend == "software" && id != "video_profile" && !subtitle {
        return Some(semantics.to_owned());
    }
    if (id == "video_profile") != profile.is_some() {
        return None;
    }
    let mut inputs =
        serde_json::json!({"semantics": semantics,"backend":backend,"video_profile":profile});
    if subtitle {
        inputs["subtitle"] = serde_json::json!("selected-text-mov_text");
    }
    Some(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&inputs).ok()?)
    ))
}

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
    /// The preparation encodes one selected text subtitle (as `mov_text`);
    /// otherwise the output carries no subtitle at all.
    pub subtitle: bool,
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
    let subtitles: Vec<&Track> = output
        .tracks
        .iter()
        .filter(|t| t.kind == "subtitle")
        .collect();
    match (required.subtitle, subtitles.as_slice()) {
        (true, [t]) if t.codec == "mov_text" => {}
        (false, []) => {}
        (true, _) => return Err("output lost required track"),
        (false, _) => return Err("output carries an unselected subtitle"),
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

/// Why a selection cannot be kept by a rendition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeptSubtitleRefusal {
    /// An explicit subtitle with `off`.
    PolicyConflict,
    /// `auto`, or `require` without an explicit subtitle: automatic selection
    /// depends on a profile, never on a rendition.
    Unsupported,
}

/// Whether a rendition's selection is expressible: it keeps a subtitle only
/// when one is chosen explicitly with `require`, and none with `off`. Shared
/// by preparing a rendition and downloading one.
pub fn kept_subtitle(policy: &str, explicit: bool) -> Result<(), KeptSubtitleRefusal> {
    match (policy, explicit) {
        ("off", false) | ("require", true) => Ok(()),
        ("off", true) => Err(KeptSubtitleRefusal::PolicyConflict),
        _ => Err(KeptSubtitleRefusal::Unsupported),
    }
}

/// What a playscale preparation encoded from its source revision, as
/// recorded when it was requested: zero-based audio and subtitle stream
/// ordinals of that revision. No audio selection means the recipe's default
/// (the first audio stream, if any); no subtitle means none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub audio: Option<u32>,
    pub subtitle: Option<u32>,
}

/// What a rendition is known to carry of the bound source revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carried {
    /// Recorded by a playscale preparation of exactly that revision.
    Encoded(Encoded),
    /// Not recorded for that revision (another source or revision, or an
    /// external rendition): only its own default streams are known.
    Unrecorded,
    /// A record that cannot be read describes nothing.
    Unreadable,
}

/// The subtitle a playback plan selected from the bound source revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WantedSubtitle {
    None,
    /// Embedded text subtitle stream (zero-based ordinal).
    Text(u32),
    /// A sidecar or burned subtitle: never part of a rendition.
    Other,
}

/// The streams a playback plan selected from the bound source revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub audio: Option<u32>,
    /// The audio was chosen (a track or component pin, or not the default).
    pub audio_chosen: bool,
    /// The source's default audio: its first audio stream, if any.
    pub default_audio: Option<u32>,
    pub subtitle: WantedSubtitle,
}

/// Whether a rendition presents exactly the selected streams: the recorded
/// audio and text subtitle of a preparation of the bound revision, never a
/// substitute. An unrecorded rendition carries only an unchosen default
/// audio and no subtitle.
pub fn carries(carried: Carried, wanted: Wanted) -> bool {
    let subtitle = match wanted.subtitle {
        WantedSubtitle::None => None,
        WantedSubtitle::Text(index) => Some(index),
        WantedSubtitle::Other => return false,
    };
    match carried {
        Carried::Encoded(encoded) => {
            encoded.audio.or(wanted.default_audio) == wanted.audio && encoded.subtitle == subtitle
        }
        Carried::Unrecorded => !wanted.audio_chosen && subtitle.is_none(),
        Carried::Unreadable => false,
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryRefusal {
    StaleRevision,
    NotRetryable,
    QueueFull,
}

/// Plan a processing retry from observations held under the writer boundary.
/// Exact receipt replay and authorization precede this decision. The adapter
/// must reserve the writer before counting active jobs and retain it through
/// the phase update and any receipt write.
pub fn retry(
    job: &crate::jobs::Job,
    revisions: Option<(u64, u64)>,
    active_jobs: i64,
) -> Result<crate::jobs::Job, RetryRefusal> {
    if let Some((current, reviewed)) = revisions {
        crate::jobs::admit_retry(job, current, reviewed).map_err(|reason| match reason {
            crate::jobs::RetryRefusal::StaleRevision => RetryRefusal::StaleRevision,
            crate::jobs::RetryRefusal::NotRetryable => RetryRefusal::NotRetryable,
        })?;
    } else if !job.retryable() {
        return Err(RetryRefusal::NotRetryable);
    }
    if !queue_admits(active_jobs) {
        return Err(RetryRefusal::QueueFull);
    }
    Ok(crate::jobs::transition(job, crate::jobs::Input::Retry).0)
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn retry_checks_revision_phase_attempt_and_capacity_in_order() {
        let job = Job {
            phase: jobs::Phase::Failed,
            attempt: 2,
        };
        assert_eq!(
            retry(&job, Some((3, 2)), 100),
            Err(RetryRefusal::StaleRevision)
        );
        assert_eq!(retry(&job, Some((3, 3)), 100), Err(RetryRefusal::QueueFull));
        assert_eq!(
            retry(&job, None, 99),
            Ok(Job {
                phase: jobs::Phase::Queued,
                attempt: 2
            })
        );
        let exhausted = Job {
            attempt: u32::MAX,
            ..job.clone()
        };
        assert_eq!(retry(&exhausted, None, 0), Err(RetryRefusal::NotRetryable));
        let queued = Job {
            phase: jobs::Phase::Queued,
            ..job
        };
        assert_eq!(retry(&queued, None, 0), Err(RetryRefusal::NotRetryable));
    }
}

/// Candidate original-version binding and the authoritative facts needed to
/// decide whether it is the requested whole-file timeline binding.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WholeBinding {
    pub version: String,
    pub timeline: String,
    pub file: String,
    pub file_revision: String,
    pub origin: String,
    pub file_edition: String,
    pub timeline_edition: String,
    pub part: i64,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub part_count: i64,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BindingEvidence {
    pub timeline: String,
    pub file: String,
    pub revision: String,
    pub candidates: Vec<WholeBinding>,
}
impl BindingEvidence {
    fn whole(b: &WholeBinding) -> bool {
        b.start_ms.is_none() && b.end_ms.is_none() && b.part_count == 1
    }
    fn same_file(&self, b: &WholeBinding) -> bool {
        b.timeline == self.timeline && b.file == self.file && b.file_revision == self.revision
    }
    /// Preparation accepts any whole original version owned by the timeline's edition.
    pub fn is_current(&self) -> bool {
        self.candidates.iter().any(|b| {
            self.same_file(b)
                && Self::whole(b)
                && b.origin == "original"
                && b.file_edition == b.timeline_edition
        })
    }
    /// Downloads pin a particular version and its first (only) part; generated
    /// versions are permitted and their rendition is checked separately.
    pub fn matches_download(&self, version: &str) -> bool {
        self.candidates
            .iter()
            .any(|b| self.same_file(b) && Self::whole(b) && b.version == version && b.part == 1)
    }
}

/// Current observations for a preparation's publication. Adapters must observe
/// these in the same write transaction that acknowledges publication.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PreparationFacts {
    pub requester_authorized: bool,
    pub source_enabled: bool,
    pub source_in_scope: bool,
    pub binding: BindingEvidence,
    pub recipe_unchanged: bool,
}
impl PreparationFacts {
    pub fn admit(&self) -> Result<(), &'static str> {
        if !self.requester_authorized {
            return Err("preparation_authorization_changed");
        }
        if !self.source_enabled || !self.source_in_scope {
            return Err("preparation_source_unavailable");
        }
        if !self.binding.is_current() {
            return Err("preparation_timeline_changed");
        }
        if !self.recipe_unchanged {
            return Err("preparation_recipe_changed");
        }
        Ok(())
    }
}

pub struct RenditionFacts {
    pub expired: bool,
    pub phase: jobs::Phase,
    pub source_current: bool,
    pub binding: BindingEvidence,
    pub output_current: bool,
}
pub fn rendition_status(facts: &RenditionFacts) -> &'static str {
    if facts.expired {
        "expired"
    } else if matches!(facts.phase, jobs::Phase::Failed | jobs::Phase::Cancelled) {
        "failed"
    } else if !facts.source_current || !facts.binding.is_current() {
        "stale"
    } else if facts.phase == jobs::Phase::Completed {
        if facts.output_current {
            "ready"
        } else {
            "stale"
        }
    } else {
        "pending"
    }
}

pub fn finish_preparation(
    state: &State,
    result: &Completion,
    facts: Option<&PreparationFacts>,
) -> (State, Vec<Effect>) {
    let mut observed = result.clone();
    if facts.is_some_and(|f| f.admit().is_err()) {
        observed.validated_output = None;
    }
    finish(state, &observed)
}

#[cfg(test)]
mod preparation_tests {
    use super::*;
    fn test_binding(valid: bool) -> BindingEvidence {
        BindingEvidence {
            timeline: "t".into(),
            file: "f".into(),
            revision: "r".into(),
            candidates: if valid {
                vec![WholeBinding {
                    version: "v".into(),
                    timeline: "t".into(),
                    file: "f".into(),
                    file_revision: "r".into(),
                    origin: "original".into(),
                    file_edition: "e".into(),
                    timeline_edition: "e".into(),
                    part: 1,
                    start_ms: None,
                    end_ms: None,
                    part_count: 1,
                }]
            } else {
                vec![]
            },
        }
    }
    #[test]
    fn exact_binding_checks_all_identity_and_whole_file_facts() {
        let valid = test_binding(true);
        assert!(valid.is_current());
        for variant in 0..8 {
            let mut evidence = valid.clone();
            let candidate = &mut evidence.candidates[0];
            match variant {
                0 => candidate.timeline = "other".into(),
                1 => candidate.file = "other".into(),
                2 => candidate.file_revision = "stale".into(),
                3 => candidate.origin = "generated".into(),
                4 => candidate.file_edition = "other".into(),
                5 => candidate.start_ms = Some(0),
                6 => candidate.end_ms = Some(10),
                _ => candidate.part_count = 2,
            }
            assert!(!evidence.is_current(), "variant {variant}");
            evidence.candidates.push(valid.candidates[0].clone());
            assert!(evidence.is_current(), "a later valid candidate must count");
        }
    }

    #[test]
    fn a_kept_subtitle_is_explicit_and_required() {
        assert_eq!(kept_subtitle("off", false), Ok(()));
        assert_eq!(kept_subtitle("require", true), Ok(()));
        assert_eq!(
            kept_subtitle("off", true),
            Err(KeptSubtitleRefusal::PolicyConflict)
        );
        for (policy, explicit) in [("require", false), ("auto", false), ("auto", true)] {
            assert_eq!(
                kept_subtitle(policy, explicit),
                Err(KeptSubtitleRefusal::Unsupported)
            );
        }
    }

    #[test]
    fn renditions_carry_only_the_recorded_audio_and_subtitle() {
        let wanted = |audio, subtitle| Wanted {
            audio,
            audio_chosen: audio != Some(0),
            default_audio: Some(0),
            subtitle,
        };
        let encoded = |audio, subtitle| Carried::Encoded(Encoded { audio, subtitle });
        // No recorded audio is the recipe default (the first stream).
        assert!(carries(
            encoded(None, None),
            wanted(Some(0), WantedSubtitle::None)
        ));
        assert!(!carries(
            encoded(None, None),
            wanted(Some(1), WantedSubtitle::None)
        ));
        assert!(carries(
            encoded(Some(1), None),
            wanted(Some(1), WantedSubtitle::None)
        ));
        // The recorded subtitle must be exactly the selected one, and a
        // rendition with a subtitle does not serve a plan without one.
        assert!(carries(
            encoded(Some(1), Some(2)),
            wanted(Some(1), WantedSubtitle::Text(2))
        ));
        assert!(!carries(
            encoded(Some(1), Some(2)),
            wanted(Some(1), WantedSubtitle::Text(0))
        ));
        assert!(!carries(
            encoded(Some(1), Some(2)),
            wanted(Some(1), WantedSubtitle::None)
        ));
        assert!(!carries(
            encoded(Some(1), None),
            wanted(Some(1), WantedSubtitle::Text(2))
        ));
        // Sidecars and burned subtitles are never in a rendition.
        assert!(!carries(
            encoded(Some(0), None),
            wanted(Some(0), WantedSubtitle::Other)
        ));
        // Unrecorded renditions carry only unchosen defaults.
        assert!(carries(
            Carried::Unrecorded,
            wanted(Some(0), WantedSubtitle::None)
        ));
        assert!(!carries(
            Carried::Unrecorded,
            wanted(Some(1), WantedSubtitle::None)
        ));
        assert!(!carries(
            Carried::Unrecorded,
            wanted(Some(0), WantedSubtitle::Text(0))
        ));
        assert!(!carries(
            Carried::Unreadable,
            wanted(Some(0), WantedSubtitle::None)
        ));
    }

    #[test]
    fn every_publication_fact_is_required_and_attempts_and_cancellation_are_fenced() {
        for mask in 0..32 {
            let facts = PreparationFacts {
                requester_authorized: mask & 1 != 0,
                source_enabled: mask & 2 != 0,
                source_in_scope: mask & 4 != 0,
                binding: test_binding(mask & 8 != 0),
                recipe_unchanged: mask & 16 != 0,
            };
            for phase in [
                jobs::Phase::Running,
                jobs::Phase::Cancelling,
                jobs::Phase::Cancelled,
            ] {
                for attempt in [1, 2] {
                    let state = State {
                        job: Job { phase, attempt: 2 },
                        source_revision: "source".into(),
                        published_output: None,
                    };
                    let result = Completion {
                        attempt,
                        current_source_revision: Some("source".into()),
                        validated_output: Some("output".into()),
                    };
                    let (next, effects) = finish_preparation(&state, &result, Some(&facts));
                    let expected = mask == 31 && phase == jobs::Phase::Running && attempt == 2;
                    assert_eq!(
                        effects
                            .iter()
                            .filter(|e| matches!(e, Effect::Publish { .. }))
                            .count(),
                        usize::from(expected)
                    );
                    assert_eq!(
                        next.published_output.as_deref(),
                        expected.then_some("output")
                    );
                    let (duplicate, effects) = finish_preparation(&next, &result, Some(&facts));
                    assert_eq!(duplicate, next);
                    assert!(effects.is_empty());
                }
            }
        }
    }
    #[test]
    fn ready_requires_current_source_binding_and_published_output() {
        for bits in 0..8 {
            assert_eq!(
                rendition_status(&RenditionFacts {
                    expired: false,
                    phase: jobs::Phase::Completed,
                    source_current: bits & 1 != 0,
                    binding: test_binding(bits & 2 != 0),
                    output_current: bits & 4 != 0
                }),
                if bits == 7 { "ready" } else { "stale" }
            );
        }
    }
}

#[cfg(test)]
mod preparation_policy_tests {
    use super::*;
    #[test]
    fn recovery_facts_preserve_sources_and_release_only_proven_software_capacity() {
        for generated in [false, true] {
            for managed in [false, true] {
                for same in [false, true] {
                    assert_eq!(
                        restored_output_unavailable(
                            generated,
                            managed,
                            "old",
                            if same { "old" } else { "new" }
                        ),
                        generated && managed && !same
                    );
                }
            }
        }
        for phase in [
            crate::jobs::Phase::Queued,
            crate::jobs::Phase::Running,
            crate::jobs::Phase::Completed,
            crate::jobs::Phase::Failed,
        ] {
            for generated in [false, true] {
                for available in [false, true] {
                    assert_eq!(
                        restored_receipt_expired(phase, generated, available),
                        phase == crate::jobs::Phase::Completed && generated && !available
                    );
                }
            }
        }
        assert!(!recovered_hardware_required(Some("software")));
        for backend in [None, Some("videotoolbox"), Some("unknown")] {
            assert!(recovered_hardware_required(backend));
        }
        assert!(video_source_eligible(
            SourceContract::Legacy,
            "h264720p",
            None
        ));
        assert!(!video_source_eligible(
            SourceContract::Preparation,
            "h264720p",
            None
        ));
        assert!(!video_source_eligible(
            SourceContract::Legacy,
            "video_profile",
            None
        ));
    }
    #[test]
    fn profiles_backends_and_source_bounds_are_shared() {
        let profile = VideoProfileFacts {
            version: 1,
            max_width: 1280,
            max_height: 720,
            video_bitrate: 2_000_000,
            audio_bitrate: 128_000,
            frame_rate: Some((24, 1)),
        };
        assert!(profile.valid());
        assert!(!profile.admits_duration(f64::NAN));
        assert!(!profile.admits_duration(0.01));
        assert!(profile.admits_duration(0.5 / 24.0));
        for width in [0, 127, 129, 3841, 3842] {
            assert!(
                !VideoProfileFacts {
                    max_width: width,
                    ..profile.clone()
                }
                .valid()
            );
        }
        for rate in [(0, 1), (1, 0), (121, 1), (120001, 100000), (1, 2)] {
            assert!(
                !VideoProfileFacts {
                    frame_rate: Some(rate),
                    ..profile.clone()
                }
                .valid()
            );
        }
        for recipe in [
            "remux_mp4",
            "audio_aac",
            "h264720p",
            "video_profile",
            "unknown",
        ] {
            assert_eq!(
                backend_allowed(recipe, "software", false),
                recipe != "unknown"
            );
            assert_eq!(
                backend_allowed(recipe, "videotoolbox", true),
                matches!(recipe, "h264720p" | "video_profile")
            );
            assert!(!backend_allowed(recipe, "videotoolbox", false));
            for transfer in [None, Some("bt709"), Some("smpte2084"), Some("arib-std-b67")] {
                let expected = !matches!(recipe, "h264720p" | "video_profile")
                    || !matches!(transfer, Some("smpte2084" | "arib-std-b67"));
                assert_eq!(
                    video_source_eligible(
                        SourceContract::Preparation,
                        recipe,
                        Some((Some(1280), Some(720), transfer))
                    ),
                    expected
                );
            }
        }
    }
    #[test]
    fn recipe_identity_binds_all_encoder_inputs_and_preserves_legacy_receipts() {
        let base = "sha256:historical";
        assert_eq!(
            recipe_identity("remux_mp4", base, "software", None, false),
            Some(base.into())
        );
        let original = serde_json::json!({"version":1,"codec":"h264","max_width":1280,"max_height":720,"video_bitrate":2_000_000,"audio_bitrate":128_000,"frame_rate":null});
        let digest = recipe_identity(
            "video_profile",
            base,
            "software",
            Some(original.clone()),
            false,
        )
        .unwrap();
        for (field, value) in [
            ("codec", serde_json::json!("hevc")),
            ("max_width", serde_json::json!(1920)),
            ("max_height", serde_json::json!(1080)),
            ("video_bitrate", serde_json::json!(4_000_000)),
            ("audio_bitrate", serde_json::json!(192_000)),
            (
                "frame_rate",
                serde_json::json!({"numerator":24,"denominator":1}),
            ),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            assert_ne!(
                recipe_identity("video_profile", base, "software", Some(changed), false).unwrap(),
                digest
            );
        }
        assert_ne!(
            recipe_identity(
                "video_profile",
                base,
                "videotoolbox",
                Some(original.clone()),
                false
            )
            .unwrap(),
            digest
        );
        assert_ne!(
            recipe_identity(
                "video_profile",
                base,
                "software",
                Some(original.clone()),
                true
            )
            .unwrap(),
            digest
        );
        assert_ne!(
            recipe_identity(
                "video_profile",
                "new semantics",
                "software",
                Some(original),
                false
            )
            .unwrap(),
            digest
        );
        assert!(recipe_identity("video_profile", base, "software", None, false).is_none());
    }
}
