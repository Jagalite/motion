//! Timeline markers (chapters, intro, credits, recap). Markers belong to a
//! timeline and are fenced on its revision; positions are logical timeline
//! milliseconds. The effective view applies provenance precedence.
use serde::{Deserialize, Serialize};

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Chapter,
    Intro,
    Credits,
    Recap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Embedded,
    Manual,
    Detected,
    Imported,
}

impl Provenance {
    /// Higher wins: an operator's marker beats container chapters, which beat
    /// imported and then automatically detected ones.
    pub fn precedence(self) -> u8 {
        match self {
            Provenance::Manual => 3,
            Provenance::Embedded => 2,
            Provenance::Imported => 1,
            Provenance::Detected => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Id,
    /// Durable creation order supplied by the transactional adapter; zero for legacy facts.
    #[serde(default)]
    pub creation_order: u64,
    pub kind: Kind,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub label: Option<String>,
    pub provenance: Provenance,
    /// Timeline revision the marker was validated against.
    pub timeline_revision: u64,
}

/// A chapter as the container records it (probed from the file), in file
/// time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileChapter {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub title: Option<String>,
}

/// At most this many chapters of a file become markers.
pub const MAX_EMBEDDED_CHAPTERS: usize = 200;

/// The embedded chapter markers of a timeline whose whole, single file
/// records `chapters`, at the timeline's current revision (they are file
/// facts, so they follow every revision of the timeline that still binds
/// the file). Chapters outside the timeline, out of order or duplicated at
/// a start are dropped rather than guessed; labels are trimmed and bounded.
/// `id_prefix` names the file and its revision, so the identities change
/// with the file's content.
pub fn embedded_chapters(
    chapters: &[FileChapter],
    duration_ms: Option<u64>,
    timeline_revision: u64,
    id_prefix: &str,
) -> Vec<Marker> {
    let mut starts: Vec<u64> = Vec::new();
    let mut markers = Vec::new();
    for chapter in chapters {
        if markers.len() == MAX_EMBEDDED_CHAPTERS {
            break;
        }
        let inside =
            duration_ms.is_none_or(|d| chapter.start_ms < d) && chapter.start_ms <= MAX_POSITION_MS;
        if !inside || starts.contains(&chapter.start_ms) {
            continue;
        }
        let label = chapter
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(200).collect::<String>());
        starts.push(chapter.start_ms);
        markers.push(Marker {
            creation_order: 0,
            id: format!("{id_prefix}-{}", markers.len()),
            kind: Kind::Chapter,
            start_ms: chapter.start_ms,
            end_ms: None,
            label,
            provenance: Provenance::Embedded,
            timeline_revision,
        });
    }
    markers.sort_by_key(|m| m.start_ms);
    markers
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerError {
    StaleTimeline,
    InvalidInterval,
    OutsideTimeline,
    InvalidLabel,
    /// The timeline already holds `MAX_MARKERS` markers.
    TooManyMarkers,
}

/// Markers one timeline may hold, of any provenance. Reads and the effective
/// view always see all of them; creation stops here.
pub const MAX_MARKERS: usize = 1_000;

/// Positions beyond this are outside any timeline (the playback bound).
const MAX_POSITION_MS: u64 = crate::timeline_viewing::MAX_POSITION_MS;

/// Whether another marker may be added to a timeline holding `existing`.
pub fn admits_another(existing: usize) -> Result<(), MarkerError> {
    if existing >= MAX_MARKERS {
        Err(MarkerError::TooManyMarkers)
    } else {
        Ok(())
    }
}

/// Validate a marker against the reviewed timeline revision and duration.
/// Intro/credits/recap need a known end; chapters may leave it open.
pub fn validate(
    kind: Kind,
    start_ms: u64,
    end_ms: Option<u64>,
    label: Option<&str>,
    expected_timeline_revision: u64,
    timeline_revision: u64,
    duration_ms: Option<u64>,
) -> Result<(), MarkerError> {
    if expected_timeline_revision != timeline_revision {
        return Err(MarkerError::StaleTimeline);
    }
    if label.is_some_and(|l| l.trim().is_empty() || l.chars().count() > 200) {
        return Err(MarkerError::InvalidLabel);
    }
    // Even without a known duration, positions stay within playback's bound
    // (and safely inside JSON and SQLite integers).
    if start_ms > MAX_POSITION_MS || end_ms.is_some_and(|e| e > MAX_POSITION_MS) {
        return Err(MarkerError::OutsideTimeline);
    }
    match (kind, end_ms) {
        (Kind::Chapter, None) => {}
        (_, Some(end)) if end > start_ms => {}
        _ => return Err(MarkerError::InvalidInterval),
    }
    if let Some(duration) = duration_ms
        && (start_ms >= duration || end_ms.is_some_and(|e| e > duration))
    {
        return Err(MarkerError::OutsideTimeline);
    }
    Ok(())
}

/// Effective markers for the timeline's current revision: markers validated
/// against an older revision are not effective (they stay listed). Then for
/// intro, credits and recap the single marker of the highest provenance
/// (latest manual creation order, then earliest start and ID on ties); for chapters, the complete set from
/// the highest provenance present, in start order.
pub fn effective(markers: &[Marker], timeline_revision: u64) -> Vec<Marker> {
    let current: Vec<Marker> = markers
        .iter()
        .filter(|m| m.timeline_revision == timeline_revision)
        .cloned()
        .collect();
    let markers = current.as_slice();
    let mut out = Vec::new();
    for kind in [Kind::Intro, Kind::Recap, Kind::Credits] {
        if let Some(best) = markers.iter().filter(|m| m.kind == kind).min_by(|a, b| {
            (
                std::cmp::Reverse(a.provenance.precedence()),
                std::cmp::Reverse(if a.provenance == Provenance::Manual {
                    a.creation_order
                } else {
                    0
                }),
                a.start_ms,
                &a.id,
            )
                .cmp(&(
                    std::cmp::Reverse(b.provenance.precedence()),
                    std::cmp::Reverse(if b.provenance == Provenance::Manual {
                        b.creation_order
                    } else {
                        0
                    }),
                    b.start_ms,
                    &b.id,
                ))
        }) {
            out.push(best.clone());
        }
    }
    let chapters: Vec<&Marker> = markers.iter().filter(|m| m.kind == Kind::Chapter).collect();
    if let Some(top) = chapters.iter().map(|m| m.provenance.precedence()).max() {
        let mut chosen: Vec<Marker> = chapters
            .into_iter()
            .filter(|m| m.provenance.precedence() == top)
            .cloned()
            .collect();
        chosen.sort_by(|a, b| (a.start_ms, &a.id).cmp(&(b.start_ms, &b.id)));
        out.extend(chosen);
    }
    out.sort_by(|a, b| (a.start_ms, a.kind, &a.id).cmp(&(b.start_ms, b.kind, &b.id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_chapters_become_embedded_markers_of_the_current_revision() {
        let chapter = |start_ms, title: Option<&str>| FileChapter {
            start_ms,
            end_ms: None,
            title: title.map(str::to_owned),
        };
        let markers = embedded_chapters(
            &[
                chapter(60_000, Some(" Second ")),
                chapter(0, Some("First")),
                chapter(60_000, Some("duplicate start")),
                chapter(120_000, Some("   ")),
                chapter(900_000, Some("beyond the end")),
            ],
            Some(600_000),
            7,
            "embedded-f1",
        );
        assert_eq!(
            markers
                .iter()
                .map(|m| (
                    m.start_ms,
                    m.label.as_deref(),
                    m.provenance,
                    m.timeline_revision
                ))
                .collect::<Vec<_>>(),
            [
                (0, Some("First"), Provenance::Embedded, 7),
                (60_000, Some("Second"), Provenance::Embedded, 7),
                (120_000, None, Provenance::Embedded, 7),
            ]
        );
        // A manual chapter wins over all of them in the effective view.
        let mut all = markers.clone();
        all.push(Marker {
            creation_order: 0,
            id: "manual".into(),
            kind: Kind::Chapter,
            start_ms: 30_000,
            end_ms: None,
            label: Some("Mine".into()),
            provenance: Provenance::Manual,
            timeline_revision: 7,
        });
        assert_eq!(
            effective(&all, 7)
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["manual"]
        );
        // Bounded.
        let many: Vec<_> = (0..300).map(|n| chapter(n * 1000, None)).collect();
        assert_eq!(
            embedded_chapters(&many, None, 1, "x").len(),
            MAX_EMBEDDED_CHAPTERS
        );
    }
    fn m(id: &str, kind: Kind, start: u64, end: Option<u64>, p: Provenance) -> Marker {
        Marker {
            creation_order: 0,
            id: id.into(),
            kind,
            start_ms: start,
            end_ms: end,
            label: None,
            provenance: p,
            timeline_revision: 1,
        }
    }
    #[test]
    fn validation_and_effective_precedence() {
        assert_eq!(
            validate(Kind::Intro, 0, Some(90_000), None, 3, 3, Some(3_600_000)),
            Ok(())
        );
        assert_eq!(
            validate(Kind::Intro, 0, None, None, 3, 3, None),
            Err(MarkerError::InvalidInterval)
        );
        assert_eq!(
            validate(Kind::Chapter, 10, None, Some("Act 2"), 3, 3, None),
            Ok(())
        );
        assert_eq!(
            validate(Kind::Credits, 5, Some(4), None, 3, 3, None),
            Err(MarkerError::InvalidInterval)
        );
        assert_eq!(
            validate(Kind::Credits, 10, Some(20), None, 2, 3, None),
            Err(MarkerError::StaleTimeline)
        );
        assert_eq!(
            validate(Kind::Credits, 10, Some(200), None, 3, 3, Some(100)),
            Err(MarkerError::OutsideTimeline)
        );
        assert_eq!(
            validate(Kind::Chapter, 0, None, Some(" "), 3, 3, None),
            Err(MarkerError::InvalidLabel)
        );
        let markers = vec![
            m("d", Kind::Intro, 5_000, Some(60_000), Provenance::Detected),
            m("x", Kind::Intro, 4_000, Some(61_000), Provenance::Manual),
            m("e1", Kind::Chapter, 0, None, Provenance::Embedded),
            m("e2", Kind::Chapter, 600_000, None, Provenance::Embedded),
            m("i1", Kind::Chapter, 300_000, None, Provenance::Imported),
            m(
                "c",
                Kind::Credits,
                3_000_000,
                Some(3_100_000),
                Provenance::Detected,
            ),
        ];
        let ids: Vec<String> = effective(&markers, 1).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["e1", "x", "e2", "c"]);
        // After the timeline changes, old markers are not effective.
        let mut stale = markers.clone();
        stale.push(Marker {
            creation_order: 0,
            timeline_revision: 2,
            ..m(
                "new",
                Kind::Intro,
                1_000,
                Some(30_000),
                Provenance::Detected,
            )
        });
        let ids: Vec<String> = effective(&stale, 2).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["new"]);
    }

    #[test]
    fn positions_and_counts_are_bounded_without_a_known_duration() {
        let huge = super::MAX_POSITION_MS + 1;
        assert_eq!(
            validate(Kind::Chapter, huge, None, None, 1, 1, None),
            Err(MarkerError::OutsideTimeline)
        );
        assert_eq!(
            validate(Kind::Intro, 0, Some(huge), None, 1, 1, None),
            Err(MarkerError::OutsideTimeline)
        );
        assert_eq!(validate(Kind::Chapter, 5, None, None, 1, 1, None), Ok(()));
        assert_eq!(admits_another(MAX_MARKERS - 1), Ok(()));
        assert_eq!(
            admits_another(MAX_MARKERS),
            Err(MarkerError::TooManyMarkers)
        );
    }
    #[test]
    fn newest_manual_correction_wins_independent_of_input_or_start_order() {
        for kind in [Kind::Intro, Kind::Recap, Kind::Credits] {
            let mut old = m("old", kind, 100, Some(200), Provenance::Manual);
            old.creation_order = 1;
            let mut corrected = m("new", kind, 150, Some(250), Provenance::Manual);
            corrected.creation_order = 2;
            for rows in [
                vec![old.clone(), corrected.clone()],
                vec![corrected.clone(), old.clone()],
            ] {
                assert_eq!(effective(&rows, 1), vec![corrected.clone()]);
            }
            corrected.timeline_revision = 2;
            assert_eq!(effective(&[old.clone(), corrected], 1), vec![old]);
        }
        assert_eq!(
            validate(Kind::Chapter, 0, None, Some(&"章".repeat(200)), 1, 1, None),
            Ok(())
        );
        assert_eq!(
            validate(Kind::Chapter, 0, None, Some(&"章".repeat(201)), 1, 1, None),
            Err(MarkerError::InvalidLabel)
        );
    }
}
