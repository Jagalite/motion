//! Subtitle timing and selection for delivery.
//!
//! A selected text subtitle is converted once to WebVTT in timeline time (its
//! cues keep source time, which is timeline time). Each delivery generation
//! presents media whose time zero is `media_time_origin_ms` on the timeline,
//! so the WebVTT served with a generation is shifted into that media time.

/// Shift WebVTT cues from timeline time into a generation's media time:
/// subtract `origin_ms`. A cue that ends at or before the origin is dropped
/// and one that starts before it is clipped to zero. None when a cue timing
/// line cannot be parsed. With origin zero the text is returned unchanged.
pub fn shift_webvtt(text: &str, origin_ms: u64) -> Option<String> {
    if origin_ms == 0 {
        return Some(text.to_owned());
    }
    let mut out = String::with_capacity(text.len());
    // Blocks are separated by blank lines; a cue block has a timing line.
    let normalized = text.replace("\r\n", "\n");
    let mut blocks = normalized.split("\n\n").peekable();
    let mut first = true;
    while let Some(block) = blocks.next() {
        if block.trim().is_empty() {
            continue;
        }
        let lines: Vec<&str> = block.lines().collect();
        let timing = lines.iter().position(|l| l.contains("-->"));
        let block = match timing {
            None => block.to_owned(),
            Some(at) => {
                let (start, rest) = lines[at].split_once("-->")?;
                let rest = rest.trim_start();
                let (end, settings) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let start = parse_time(start.trim())?;
                let end = parse_time(end)?;
                if end <= origin_ms {
                    continue;
                }
                let mut shifted = lines.clone();
                let timing = format!(
                    "{} --> {}{}{}",
                    format_time(start.saturating_sub(origin_ms)),
                    format_time(end - origin_ms),
                    if settings.is_empty() { "" } else { " " },
                    settings
                );
                shifted[at] = &timing;
                shifted.join("\n")
            }
        };
        if !first {
            out.push_str("\n\n");
        }
        first = false;
        out.push_str(&block);
    }
    out.push('\n');
    Some(out)
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

    const VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nfirst\n\n\
        1\n00:01.000 --> 01:05.000 align:start\nlong\nsecond line\n\n\
        01:00:00.000 --> 01:00:01.250\nlate\n";

    #[test]
    fn origin_zero_is_unchanged() {
        assert_eq!(shift_webvtt(VTT, 0).unwrap(), VTT);
    }

    #[test]
    fn cues_move_into_generation_media_time() {
        let shifted = shift_webvtt(VTT, 2_000).unwrap();
        assert_eq!(
            shifted,
            "WEBVTT\n\n00:00:00.000 --> 00:00:00.500\nfirst\n\n\
             1\n00:00:00.000 --> 00:01:03.000 align:start\nlong\nsecond line\n\n\
             00:59:58.000 --> 00:59:59.250\nlate\n"
        );
        // Ended before the origin: dropped, other blocks kept.
        let shifted = shift_webvtt(VTT, 65_000).unwrap();
        assert_eq!(shifted, "WEBVTT\n\n00:58:55.000 --> 00:58:56.250\nlate\n");
        assert!(
            shift_webvtt(
                "WEBVTT\r\n\r\n00:00:05.000 --> 00:00:06.000\r\nx\r\n",
                1_000
            )
            .unwrap()
            .contains("00:00:04.000 --> 00:00:05.000")
        );
    }

    #[test]
    fn unparsable_timing_is_refused_not_guessed() {
        for bad in [
            "WEBVTT\n\n00:00:01 --> 00:00:02.000\nx\n",
            "WEBVTT\n\n00:61:01.000 --> 00:62:02.000\nx\n",
            "WEBVTT\n\n00:00:01.0000 --> 00:00:02.000\nx\n",
        ] {
            assert_eq!(shift_webvtt(bad, 1_000), None, "{bad}");
        }
    }
}
