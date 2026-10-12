//! Text subtitles for live (HLS) delivery.
//!
//! A selected text subtitle is converted once to WebVTT in source-file time.
//! A live generation publishes it as an HLS `SUBTITLES` rendition whose
//! segments mirror the generation's media segments. Each subtitle segment
//! carries the cues that overlap its media segment, in the generation's media
//! time, which is how the supported player (Shaka 5.2.11 through Demuxe)
//! places in-manifest WebVTT on a live presentation.

/// One WebVTT cue in source-file time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Cue settings after the timing (for example `align:start`).
    pub settings: String,
    /// The cue's payload lines (an identifier line is not kept).
    pub payload: String,
}

/// Parse the cues of a WebVTT document. None when a timing line cannot be
/// parsed: a subtitle is never served with guessed timing.
pub fn parse_webvtt(text: &str) -> Option<Vec<Cue>> {
    let normalized = text.replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block.lines().skip_while(|l| l.trim().is_empty()).collect();
        // The header and NOTE, STYLE and REGION blocks are not cues.
        if lines.first().is_some_and(|l| {
            ["WEBVTT", "NOTE", "STYLE", "REGION"]
                .iter()
                .any(|kind| l.starts_with(kind))
        }) {
            continue;
        }
        let Some(at) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let (start, rest) = lines[at].split_once("-->")?;
        let rest = rest.trim_start();
        let (end, settings) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let (start_ms, end_ms) = (parse_time(start.trim())?, parse_time(end)?);
        if end_ms <= start_ms {
            continue;
        }
        cues.push(Cue {
            start_ms,
            end_ms,
            settings: settings.trim().to_owned(),
            payload: lines[at + 1..].join("\n"),
        });
    }
    Some(cues)
}

/// The WebVTT segment for media time `[from_ms, to_ms)` of a generation:
/// cues overlapping it, moved from source-file time into media time by
/// subtracting `offset_ms` (the generation's media time origin plus any
/// interval start) and clipped at zero. A cue spanning several segments
/// appears in each, with its full timing.
pub fn webvtt_segment(cues: &[Cue], from_ms: u64, to_ms: u64, offset_ms: u64) -> String {
    let mut out = String::from("WEBVTT\n");
    for cue in cues {
        let (start, end) = (
            cue.start_ms.saturating_sub(offset_ms),
            cue.end_ms.saturating_sub(offset_ms),
        );
        if cue.end_ms <= offset_ms || end <= from_ms || start >= to_ms {
            continue;
        }
        out.push_str(&format!(
            "\n{} --> {}",
            format_time(start),
            format_time(end)
        ));
        if !cue.settings.is_empty() {
            out.push(' ');
            out.push_str(&cue.settings);
        }
        out.push('\n');
        out.push_str(&cue.payload);
        out.push('\n');
    }
    out
}

/// `[hh:]mm:ss.ttt` in milliseconds.
fn parse_time(s: &str) -> Option<u64> {
    let (clock, millis) = s.split_once('.')?;
    if millis.len() != 3 || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = clock.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [m, s] => ("0", *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    let number = |p: &str| -> Option<u64> {
        (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let (h, m, sec) = (number(h)?, number(m)?, number(sec)?);
    if m > 59 || sec > 59 {
        return None;
    }
    Some(((h * 60 + m) * 60 + sec) * 1000 + millis.parse::<u64>().ok()?)
}

fn format_time(ms: u64) -> String {
    let (s, ms) = (ms / 1000, ms % 1000);
    format!("{:02}:{:02}:{:02}.{ms:03}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VTT: &str = "WEBVTT\n\nNOTE comment --> not a cue? no timing\n\n1\n00:00:01.000 --> 00:00:03.000\nfirst\n\n\
        00:10.000 --> 00:14.500 align:start\nsecond\nline two\n\n01:00:00.000 --> 01:00:01.000\nlate\n";

    #[test]
    fn cues_parse_with_settings_and_payload() {
        let cues = parse_webvtt(VTT).unwrap();
        assert_eq!(cues.len(), 3);
        assert_eq!(
            (cues[0].start_ms, cues[0].end_ms, cues[0].payload.as_str()),
            (1_000, 3_000, "first")
        );
        assert_eq!(cues[1].settings, "align:start");
        assert_eq!(cues[1].payload, "second\nline two");
        assert_eq!(cues[2].start_ms, 3_600_000);
    }

    #[test]
    fn unparsable_timing_is_refused_not_guessed() {
        for bad in [
            "WEBVTT\n\n00:00:01 --> 00:00:02.000\nx\n",
            "WEBVTT\n\n00:61:01.000 --> 00:62:02.000\nx\n",
            "WEBVTT\n\n00:00:01.0000 --> 00:00:02.000\nx\n",
        ] {
            assert_eq!(parse_webvtt(bad), None, "{bad}");
        }
    }

    #[test]
    fn segments_carry_overlapping_cues_in_media_time() {
        let cues = parse_webvtt(VTT).unwrap();
        // A generation whose media time zero is file time 8 s.
        assert_eq!(
            webvtt_segment(&cues, 0, 4_000, 8_000),
            "WEBVTT\n\n00:00:02.000 --> 00:00:06.500 align:start\nsecond\nline two\n"
        );
        // The spanning cue appears again in the next segment.
        assert!(
            webvtt_segment(&cues, 4_000, 8_000, 8_000).contains("00:00:02.000 --> 00:00:06.500")
        );
        // Cues ending before the origin are dropped; an empty segment is valid.
        assert_eq!(webvtt_segment(&cues, 8_000, 12_000, 8_000), "WEBVTT\n");
        // Origin zero keeps source time; a cue clipped at the window start is kept.
        assert!(
            webvtt_segment(&cues, 2_000, 4_000, 0).contains("00:00:01.000 --> 00:00:03.000\nfirst")
        );
        // A cue starting before the origin is clipped to zero.
        assert!(
            webvtt_segment(&cues, 0, 4_000, 2_000).contains("00:00:00.000 --> 00:00:01.000\nfirst")
        );
    }
}
