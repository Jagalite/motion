//! Playback selection is a pure decision; planning never starts processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Original,
    Convert,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Unknown,
    Supported,
    Unsupported,
}
#[derive(Debug)]
pub struct Candidate {
    pub original: bool,
    pub available: bool,
    pub support: Support,
    pub matches_recipe: bool,
    pub within_budget: bool,
    pub can_process: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Play(usize),
    Prepare(usize),
    Blocked(&'static str),
}

/// `selected` pins an explicit version. It cannot silently fall back to another.
/// Candidates are ordered by the caller; originals and renditions retain that order.
pub fn decide(mode: Mode, candidates: &[Candidate], selected: Option<usize>) -> Decision {
    let allowed = |c: &Candidate| {
        c.available
            && c.support != Support::Unsupported
            && c.within_budget
            && match mode {
                Mode::Auto => true,
                Mode::Original => c.original,
                Mode::Convert => !c.original && c.matches_recipe,
            }
    };
    if let Some(index) = selected {
        return match candidates.get(index) {
            Some(c) if allowed(c) => Decision::Play(index),
            _ => Decision::Blocked("selected_version_unavailable"),
        };
    }
    // Original delivery takes precedence in Auto. Supported candidates take precedence
    // over unknown ones within each tier, without pretending an unknown is supported.
    for original in [true, false] {
        for support in [Support::Supported, Support::Unknown] {
            if let Some(index) = candidates
                .iter()
                .position(|c| c.original == original && c.support == support && allowed(c))
            {
                return Decision::Play(index);
            }
        }
    }
    if mode == Mode::Original {
        return Decision::Blocked("original_unavailable_or_unsupported");
    }
    // Re-encoding the same recipe does not resolve a known failure or budget
    // mismatch in its current output. Require another recipe/version instead.
    if candidates
        .iter()
        .any(|c| !c.original && c.available && c.matches_recipe)
    {
        return Decision::Blocked("prepared_version_unusable");
    }
    if let Some(index) = candidates
        .iter()
        .position(|c| c.original && c.available && c.can_process)
    {
        Decision::Prepare(index)
    } else {
        Decision::Blocked("no_processable_source")
    }
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

/// Browser capability interpretation from observed streams and advertised codecs.
pub fn browser_support(
    video: Option<&str>,
    audio: Option<&str>,
    hdr: bool,
    require_sdr: bool,
    hdr_supported: bool,
    container: Option<&str>,
    video_codecs: &[String],
    audio_codecs: &[String],
) -> Support {
    let Some(video) = video else {
        return Support::Unknown;
    };
    if hdr && (require_sdr || !hdr_supported) || never_browser(video, audio) {
        return Support::Unsupported;
    }
    let listed =
        |tag: Option<&str>, list: &[String]| tag.is_some_and(|t| list.iter().any(|c| c == t));
    if listed(video_tag(video), video_codecs)
        && audio.is_none_or(|a| listed(audio_tag(a), audio_codecs))
        && matches!(container, Some("mp4" | "m4v" | "mov" | "webm"))
    {
        Support::Supported
    } else {
        Support::Unknown
    }
}

pub fn legacy_processable(
    duration: Option<f64>,
    profile: Option<&crate::processing::VideoProfileFacts>,
    video: Option<(Option<u32>, Option<u32>, Option<&str>)>,
) -> bool {
    crate::processing::video_source_eligible(
        crate::processing::SourceContract::Legacy,
        if profile.is_some() {
            "video_profile"
        } else {
            "h264720p"
        },
        video,
    ) && duration
        .is_some_and(|d| d.is_finite() && d > 0.0 && profile.is_none_or(|p| p.admits_duration(d)))
}

pub fn within_average_budget(bytes: i64, duration: Option<f64>, limit: u64) -> bool {
    duration.is_some_and(|seconds| {
        seconds.is_finite()
            && seconds > 0.0
            && bytes >= 0
            && bytes as f64 * 8.0 / seconds <= limit as f64
    })
}

/// Completed local-job provenance, distinct from an imported recipe label.
pub struct LegacyProvenance<'a> {
    pub registration_source: &'a str,
    pub external_id: &'a str,
    pub file: &'a str,
    pub file_revision: &'a str,
    pub source_file: &'a str,
    pub source_revision: &'a str,
    pub job_id: &'a str,
    pub job_source_file: &'a str,
    pub job_source_revision: &'a str,
    pub phase: &'a str,
    pub expired: bool,
}
pub fn local_output_contract(
    p: &LegacyProvenance<'_>,
    file: &str,
    revision: &str,
    source: &str,
    source_revision: &str,
) -> bool {
    p.registration_source == "playscale"
        && p.external_id == p.job_id
        && p.phase == "completed"
        && !p.expired
        && p.file == file
        && p.file_revision == revision
        && p.source_file == source
        && p.source_revision == source_revision
        && p.job_source_file == source
        && p.job_source_revision == source_revision
}
pub fn legacy_recipe_matches(
    recipe: Option<&str>,
    profile: Option<&serde_json::Value>,
    wanted_recipe: &str,
    wanted_profile: Option<&serde_json::Value>,
) -> bool {
    recipe == Some(wanted_recipe) && profile == wanted_profile
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(original: bool) -> Candidate {
        Candidate {
            original,
            available: true,
            support: Support::Unknown,
            matches_recipe: true,
            within_budget: true,
            can_process: original,
        }
    }
    #[test]
    fn strict_original_never_uses_rendition_or_prepares() {
        let mut c = [candidate(true), candidate(false)];
        c[0].support = Support::Unsupported;
        assert!(matches!(
            decide(Mode::Original, &c, None),
            Decision::Blocked(_)
        ));
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(1));
        c[1].available = false;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Prepare(0));
    }
    #[test]
    fn conversion_requires_matching_current_output() {
        let mut c = [candidate(true), candidate(false)];
        assert_eq!(decide(Mode::Convert, &c, None), Decision::Play(1));
        c[1].matches_recipe = false;
        assert_eq!(decide(Mode::Convert, &c, None), Decision::Prepare(0));
        c[0].available = false;
        assert_eq!(
            decide(Mode::Convert, &c, None),
            Decision::Blocked("no_processable_source")
        );
    }
    #[test]
    fn explicit_version_and_budget_never_silently_fallback() {
        let mut c = [candidate(true), candidate(false)];
        c[0].within_budget = false;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(1));
        assert!(matches!(
            decide(Mode::Auto, &c, Some(0)),
            Decision::Blocked(_)
        ));
        assert!(matches!(
            decide(Mode::Original, &c, Some(1)),
            Decision::Blocked(_)
        ));
    }
    #[test]
    fn unusable_current_recipe_does_not_propose_identical_processing() {
        let mut c = [candidate(true), candidate(false)];
        c[1].within_budget = false;
        assert_eq!(
            decide(Mode::Convert, &c, None),
            Decision::Blocked("prepared_version_unusable")
        );
        c[1].within_budget = true;
        c[1].support = Support::Unsupported;
        assert_eq!(
            decide(Mode::Convert, &c, None),
            Decision::Blocked("prepared_version_unusable")
        );
        c[1].available = false;
        assert_eq!(decide(Mode::Convert, &c, None), Decision::Prepare(0));
    }
    #[test]
    fn unknown_is_attemptable_but_supported_wins_within_tier() {
        let mut c = [candidate(true), candidate(true), candidate(false)];
        c[1].support = Support::Supported;
        c[2].support = Support::Supported;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(1));
        c[1].available = false;
        assert_eq!(decide(Mode::Auto, &c, None), Decision::Play(0));
    }
}
