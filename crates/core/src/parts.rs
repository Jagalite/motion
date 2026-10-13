//! Version layouts: how one timeline maps onto the files a version binds.
//!
//! A version binds ordered parts. A single part may be clipped to an
//! interval of its file (several episodes in one file); several parts play
//! one after another (a film split across files). Split points are never
//! inferred: an interval needs explicit bounds within a probed file, and each
//! part of a multipart version needs a probed duration. Logical (timeline)
//! time starts at zero at the beginning of the interval or of the first part.
use serde::{Deserialize, Serialize};

/// One bound file of a version, as recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundPart {
    /// 1-based part number.
    pub part: u32,
    /// The bound file's probed duration.
    pub file_duration_ms: Option<u64>,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
}

/// An interval of a file, in file time: logical time `t` is file time
/// `start_ms + t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Clip {
    pub start_ms: u64,
    pub end_ms: u64,
}
impl Clip {
    pub fn duration_ms(self) -> u64 {
        self.end_ms - self.start_ms
    }
}

/// One part of a multipart version as a plan and a delivery pin it: the
/// exact file revision and its probed duration (which places the part on the
/// timeline).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PartPin {
    pub file: String,
    pub revision: String,
    pub duration_ms: u64,
    /// The zero-based audio stream of this part that carries the selected
    /// audio component (`components::per_file_tracks`); None plays no audio.
    /// Omitted when absent, so pins without it serialize as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<u32>,
    /// The zero-based subtitle stream of this part that carries the selected
    /// text subtitle component, delivered as WebVTT joined across the parts
    /// (`subtitles::join_parts`); None when no subtitle is selected. Omitted
    /// when absent, so pins without it serialize as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<u32>,
}

/// Whether two multipart layouts are the same timeline: the same files,
/// revisions and durations in the same order. The selected audio may differ
/// (a replan may switch tracks); the layout may not.
pub fn same_layout(a: &[PartPin], b: &[PartPin]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.file == b.file && a.revision == b.revision && a.duration_ms == b.duration_ms
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The whole single file.
    Whole,
    /// An interval of the single file.
    Interval(Clip),
    /// Several whole files in order, with their durations.
    Parts(Vec<u64>),
}

/// The layout of a version's bound parts, or why it cannot be played.
pub fn layout(parts: &[BoundPart]) -> Result<Layout, &'static str> {
    if parts.is_empty() {
        return Err("version_has_no_parts");
    }
    if parts
        .iter()
        .enumerate()
        .any(|(i, p)| p.part as usize != i + 1)
    {
        return Err("parts_not_contiguous");
    }
    if let [part] = parts {
        return match (part.start_ms, part.end_ms) {
            (None, None) => Ok(Layout::Whole),
            (start, end) => {
                // Bounds must lie within a file whose duration was observed.
                let duration = part.file_duration_ms.ok_or("interval_unverifiable")?;
                let start = start.unwrap_or(0);
                let end = end.unwrap_or(duration);
                if start < end && end <= duration {
                    Ok(Layout::Interval(Clip {
                        start_ms: start,
                        end_ms: end,
                    }))
                } else {
                    Err("interval_outside_file")
                }
            }
        };
    }
    if parts
        .iter()
        .any(|p| p.start_ms.is_some() || p.end_ms.is_some())
    {
        return Err("interval_in_multipart");
    }
    parts
        .iter()
        .map(|p| p.file_duration_ms.filter(|d| *d > 0))
        .collect::<Option<Vec<u64>>>()
        .map(Layout::Parts)
        .ok_or("part_duration_unknown")
}

impl Layout {
    /// Logical duration, given the single file's duration for `Whole`.
    pub fn duration_ms(&self, whole_ms: Option<u64>) -> Option<u64> {
        match self {
            Layout::Whole => whole_ms,
            Layout::Interval(clip) => Some(clip.duration_ms()),
            Layout::Parts(durations) => Some(durations.iter().sum()),
        }
    }
}

/// The part (0-based) and offset within it of logical time `logical_ms` in a
/// multipart layout. A boundary belongs to the part that starts there; the end
/// of the last part has no part.
pub fn locate(durations: &[u64], logical_ms: u64) -> Option<(usize, u64)> {
    let mut start = 0u64;
    for (i, d) in durations.iter().enumerate() {
        if logical_ms < start + d {
            return Some((i, logical_ms - start));
        }
        start += d;
    }
    None
}

/// Whether a subtitle component can be delivered across parts: it occurs
/// in every part (one codec per part, none missing) and each occurrence is
/// plain text, joined in timeline time (`subtitles::join_parts`). A burned
/// or styled occurrence in any part is not delivered across parts.
pub fn joinable_subtitle(codecs: &[Option<&str>]) -> bool {
    !codecs.is_empty()
        && codecs
            .iter()
            .all(|c| c.is_some_and(crate::delivery::sidecar_subtitle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subtitle_joins_across_parts_only_as_text_in_every_part() {
        assert!(joinable_subtitle(&[Some("mov_text"), Some("subrip")]));
        assert!(!joinable_subtitle(&[Some("mov_text"), None]));
        assert!(!joinable_subtitle(&[
            Some("mov_text"),
            Some("hdmv_pgs_subtitle")
        ]));
        assert!(!joinable_subtitle(&[Some("ass"), Some("ass")]));
        assert!(!joinable_subtitle(&[]));
    }
    fn part(part: u32, duration: Option<u64>, start: Option<u64>, end: Option<u64>) -> BoundPart {
        BoundPart {
            part,
            file_duration_ms: duration,
            start_ms: start,
            end_ms: end,
        }
    }

    #[test]
    fn single_parts_are_whole_or_explicit_intervals_within_the_file() {
        assert_eq!(layout(&[part(1, None, None, None)]), Ok(Layout::Whole));
        let clip = |s, e| {
            Ok(Layout::Interval(Clip {
                start_ms: s,
                end_ms: e,
            }))
        };
        assert_eq!(
            layout(&[part(1, Some(60_000), Some(10_000), Some(20_000))]),
            clip(10_000, 20_000)
        );
        // An open bound runs to the start or end of the probed file.
        assert_eq!(
            layout(&[part(1, Some(60_000), None, Some(20_000))]),
            clip(0, 20_000)
        );
        assert_eq!(
            layout(&[part(1, Some(60_000), Some(50_000), None)]),
            clip(50_000, 60_000)
        );
        // Never guessed: no probed duration, or bounds outside the file.
        assert_eq!(
            layout(&[part(1, None, Some(1), Some(2))]),
            Err("interval_unverifiable")
        );
        assert_eq!(
            layout(&[part(1, Some(60_000), Some(10_000), Some(70_000))]),
            Err("interval_outside_file")
        );
        assert_eq!(
            layout(&[part(1, Some(60_000), Some(60_000), None)]),
            Err("interval_outside_file")
        );
        assert_eq!(Layout::Whole.duration_ms(Some(5)), Some(5));
        assert_eq!(
            layout(&[part(1, Some(60_000), Some(10_000), Some(20_000))])
                .unwrap()
                .duration_ms(None),
            Some(10_000)
        );
    }

    #[test]
    fn multipart_needs_contiguous_whole_parts_with_known_durations() {
        let parts = [
            part(1, Some(1_000), None, None),
            part(2, Some(2_000), None, None),
        ];
        assert_eq!(layout(&parts), Ok(Layout::Parts(vec![1_000, 2_000])));
        assert_eq!(
            Layout::Parts(vec![1_000, 2_000]).duration_ms(None),
            Some(3_000)
        );
        assert_eq!(
            layout(&[
                part(1, Some(1_000), None, None),
                part(3, Some(2_000), None, None)
            ]),
            Err("parts_not_contiguous")
        );
        assert_eq!(
            layout(&[part(1, Some(1_000), None, None), part(2, None, None, None)]),
            Err("part_duration_unknown")
        );
        assert_eq!(
            layout(&[
                part(1, Some(1_000), None, None),
                part(2, Some(0), None, None)
            ]),
            Err("part_duration_unknown")
        );
        assert_eq!(
            layout(&[
                part(1, Some(1_000), Some(0), None),
                part(2, Some(2_000), None, None)
            ]),
            Err("interval_in_multipart")
        );
        assert_eq!(layout(&[]), Err("version_has_no_parts"));
    }

    #[test]
    fn logical_time_locates_its_part_and_boundaries_belong_to_the_next() {
        let durations = [1_000, 2_000, 500];
        assert_eq!(locate(&durations, 0), Some((0, 0)));
        assert_eq!(locate(&durations, 999), Some((0, 999)));
        assert_eq!(locate(&durations, 1_000), Some((1, 0)));
        assert_eq!(locate(&durations, 2_999), Some((1, 1_999)));
        assert_eq!(locate(&durations, 3_000), Some((2, 0)));
        assert_eq!(locate(&durations, 3_500), None);
    }

    #[test]
    fn layouts_compare_parts_not_track_selection() {
        let pin = |file: &str, audio| PartPin {
            file: file.into(),
            revision: "r".into(),
            duration_ms: 1_000,
            audio,
            subtitle: None,
        };
        assert!(same_layout(
            &[pin("a", Some(0)), pin("b", Some(1))],
            &[pin("a", Some(1)), pin("b", Some(0))]
        ));
        let subtitled = PartPin {
            subtitle: Some(2),
            ..pin("a", None)
        };
        assert!(same_layout(
            std::slice::from_ref(&subtitled),
            &[pin("a", None)]
        ));
        assert_eq!(
            serde_json::to_string(&subtitled).unwrap(),
            r#"{"file":"a","revision":"r","duration_ms":1000,"subtitle":2}"#
        );
        assert!(!same_layout(&[pin("a", None)], &[pin("b", None)]));
        assert!(!same_layout(
            &[pin("a", None)],
            &[pin("a", None), pin("b", None)]
        ));
        // Absent audio and subtitle serialize as before the field existed.
        assert_eq!(
            serde_json::to_string(&pin("a", None)).unwrap(),
            r#"{"file":"a","revision":"r","duration_ms":1000}"#
        );
    }
}
