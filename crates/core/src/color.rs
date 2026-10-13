//! HDR-to-SDR conversion policy. The adapter supplies the source's probed
//! colour facts and which FFmpeg filters the configured binary has; this
//! decides whether a conversion may tone-map the picture to SDR BT.709 and
//! with which explicit input properties. Unknown or unsupported inputs are
//! refused rather than guessed: retagging HDR pixels as SDR is never a route.
use serde::{Deserialize, Serialize};

/// Probed colour facts of one video stream, as ffprobe names them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorFacts<'a> {
    pub transfer: Option<&'a str>,
    pub primaries: Option<&'a str>,
    /// The matrix coefficients (`color_space`).
    pub matrix: Option<&'a str>,
    pub range: Option<&'a str>,
    /// Mastering display maximum luminance, in nits.
    pub mastering_peak_nits: Option<u32>,
    /// Content light level MaxCLL, in nits.
    pub max_cll_nits: Option<u32>,
    /// Dolby Vision configuration: profile and base-layer signal compatibility.
    pub dolby_vision: Option<(u8, u8)>,
}

/// Which of the filters Motion's policies depend on the configured FFmpeg
/// has (observed, never assumed).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Filters {
    pub zscale: bool,
    pub tonemap: bool,
    /// libass text and styled subtitle rendering (`subtitles`).
    #[serde(default)]
    pub subtitles: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    /// SMPTE ST 2084 (HDR10 and Dolby Vision HDR10-compatible base layers).
    Pq,
    /// ARIB STD-B67 hybrid log-gamma.
    Hlg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    Limited,
    Full,
}

/// The explicit input properties and peak of one tone-mapped conversion.
/// Carried in the delivery pin, so execution never re-derives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToneMap {
    pub transfer: Transfer,
    pub range: Range,
    /// Source peak luminance used for the tone curve, in nits.
    pub peak_nits: u32,
    /// The peak was not in the source metadata; `DEFAULT_PEAK_NITS` was used.
    pub peak_assumed: bool,
}

/// Peak assumed for PQ sources without mastering or content-light metadata,
/// and the nominal display peak for HLG. FFmpeg's own PQ default (100 nits)
/// would clip every highlight.
pub const DEFAULT_PEAK_NITS: u32 = 1_000;
/// Peaks outside this range are treated as malformed metadata.
const PEAK_RANGE: std::ops::RangeInclusive<u32> = 100..=10_000;

/// Why an HDR source cannot be converted to SDR here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The configured FFmpeg lacks `zscale` or `tonemap`.
    ToolsUnavailable,
    /// Dolby Vision without an HDR10- or HLG-compatible base layer
    /// (for example profile 5's IPTPQc2), which these filters cannot decode.
    DolbyVisionUnsupported,
    /// Primaries, matrix or range are missing or not BT.2020 non-constant
    /// luminance: the conversion's input properties would be guesses.
    ColorUnknown,
}

/// Whether the transfer is HDR (PQ or HLG).
pub fn hdr(transfer: Option<&str>) -> Option<Transfer> {
    match transfer {
        Some("smpte2084") => Some(Transfer::Pq),
        Some("arib-std-b67") => Some(Transfer::Hlg),
        _ => None,
    }
}

/// The tone map an SDR conversion of this HDR source uses. `Ok(None)` for an
/// SDR source (no tone map needed).
pub fn tone_map(facts: &ColorFacts<'_>, filters: Filters) -> Result<Option<ToneMap>, Refusal> {
    let Some(transfer) = hdr(facts.transfer) else {
        return Ok(None);
    };
    if let Some((profile, compatibility)) = facts.dolby_vision {
        // Profile 8.1 and 7 carry an HDR10 base layer (compatibility 1, or 6
        // for UHD Blu-ray profile 7), 8.4 an HLG one (4): those base layers are
        // ordinary PQ/HLG pictures. Every other configuration needs a Dolby
        // Vision decoder.
        let base_layer = match (profile, compatibility) {
            (7 | 8, 1) | (7, 6) => Some(Transfer::Pq),
            (8, 4) => Some(Transfer::Hlg),
            _ => None,
        };
        if base_layer != Some(transfer) {
            return Err(Refusal::DolbyVisionUnsupported);
        }
    }
    if facts.primaries != Some("bt2020") || facts.matrix != Some("bt2020nc") {
        return Err(Refusal::ColorUnknown);
    }
    let range = match facts.range {
        Some("tv") => Range::Limited,
        Some("pc") => Range::Full,
        _ => return Err(Refusal::ColorUnknown),
    };
    if !(filters.zscale && filters.tonemap) {
        return Err(Refusal::ToolsUnavailable);
    }
    let measured = match transfer {
        // MaxCLL describes the content itself; the mastering peak bounds it.
        Transfer::Pq => facts
            .max_cll_nits
            .filter(|n| PEAK_RANGE.contains(n))
            .or(facts.mastering_peak_nits.filter(|n| PEAK_RANGE.contains(n))),
        // HLG is scene-referred: it is displayed for a nominal peak.
        Transfer::Hlg => None,
    };
    Ok(Some(ToneMap {
        transfer,
        range,
        peak_nits: measured.unwrap_or(DEFAULT_PEAK_NITS),
        peak_assumed: transfer == Transfer::Pq && measured.is_none(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: Filters = Filters {
        zscale: true,
        tonemap: true,
        subtitles: false,
    };

    fn hdr10() -> ColorFacts<'static> {
        ColorFacts {
            transfer: Some("smpte2084"),
            primaries: Some("bt2020"),
            matrix: Some("bt2020nc"),
            range: Some("tv"),
            ..Default::default()
        }
    }

    #[test]
    fn sdr_needs_no_tone_map_and_hdr10_uses_explicit_properties() {
        let sdr = ColorFacts {
            transfer: Some("bt709"),
            ..Default::default()
        };
        assert_eq!(tone_map(&sdr, Filters::default()), Ok(None));
        assert_eq!(
            tone_map(&ColorFacts::default(), Filters::default()),
            Ok(None)
        );
        assert_eq!(
            tone_map(&hdr10(), TOOLS),
            Ok(Some(ToneMap {
                transfer: Transfer::Pq,
                range: Range::Limited,
                peak_nits: DEFAULT_PEAK_NITS,
                peak_assumed: true,
            }))
        );
    }

    #[test]
    fn the_peak_comes_from_content_light_then_mastering_metadata() {
        let mut facts = hdr10();
        facts.mastering_peak_nits = Some(4_000);
        assert_eq!(tone_map(&facts, TOOLS).unwrap().unwrap().peak_nits, 4_000);
        facts.max_cll_nits = Some(1_500);
        let chosen = tone_map(&facts, TOOLS).unwrap().unwrap();
        assert_eq!((chosen.peak_nits, chosen.peak_assumed), (1_500, false));
        // Malformed metadata is ignored, not trusted.
        facts.max_cll_nits = Some(0);
        facts.mastering_peak_nits = Some(50_000);
        assert!(tone_map(&facts, TOOLS).unwrap().unwrap().peak_assumed);
        let hlg = ColorFacts {
            transfer: Some("arib-std-b67"),
            max_cll_nits: Some(400),
            ..hdr10()
        };
        let chosen = tone_map(&hlg, TOOLS).unwrap().unwrap();
        assert_eq!(
            (chosen.transfer, chosen.peak_nits, chosen.peak_assumed),
            (Transfer::Hlg, DEFAULT_PEAK_NITS, false)
        );
    }

    #[test]
    fn unknown_inputs_unsupported_dolby_vision_and_missing_tools_are_refused() {
        for change in [
            |f: &mut ColorFacts<'static>| f.primaries = None,
            |f: &mut ColorFacts<'static>| f.primaries = Some("bt709"),
            |f: &mut ColorFacts<'static>| f.matrix = Some("bt2020c"),
            |f: &mut ColorFacts<'static>| f.range = None,
            |f: &mut ColorFacts<'static>| f.range = Some("unknown"),
        ] {
            let mut facts = hdr10();
            change(&mut facts);
            assert_eq!(tone_map(&facts, TOOLS), Err(Refusal::ColorUnknown));
        }
        let mut dv = hdr10();
        for (config, expected) in [
            ((5, 0), Err(Refusal::DolbyVisionUnsupported)),
            ((8, 2), Err(Refusal::DolbyVisionUnsupported)),
            // An HLG-compatible base layer under a PQ transfer tag is inconsistent.
            ((8, 4), Err(Refusal::DolbyVisionUnsupported)),
            ((8, 1), Ok(())),
            ((7, 1), Ok(())),
            // UHD Blu-ray profile 7: an HDR10-compatible base layer.
            ((7, 6), Ok(())),
            ((8, 6), Err(Refusal::DolbyVisionUnsupported)),
        ] {
            dv.dolby_vision = Some(config);
            assert_eq!(tone_map(&dv, TOOLS).map(|_| ()), expected, "{config:?}");
        }
        for tools in [
            Filters::default(),
            Filters {
                zscale: true,
                tonemap: false,
                subtitles: true,
            },
            Filters {
                zscale: false,
                tonemap: true,
                subtitles: true,
            },
        ] {
            assert_eq!(tone_map(&hdr10(), tools), Err(Refusal::ToolsUnavailable));
        }
    }
}
