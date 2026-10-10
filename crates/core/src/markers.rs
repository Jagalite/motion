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
    pub kind: Kind,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub label: Option<String>,
    pub provenance: Provenance,
    /// Timeline revision the marker was validated against.
    pub timeline_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerError {
    StaleTimeline,
    InvalidInterval,
    OutsideTimeline,
    InvalidLabel,
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
    if label.is_some_and(|l| l.trim().is_empty() || l.len() > 200) {
        return Err(MarkerError::InvalidLabel);
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
/// (earliest start, then ID, on ties); for chapters, the complete set from
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
                a.start_ms,
                &a.id,
            )
                .cmp(&(
                    std::cmp::Reverse(b.provenance.precedence()),
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
    fn m(id: &str, kind: Kind, start: u64, end: Option<u64>, p: Provenance) -> Marker {
        Marker {
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
}
