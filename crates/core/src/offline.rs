//! Client-local event admission, never server viewing authority. Reconciliation
//! belongs to the server; this log preserves the causal facts it will require.
//!
//! The log is consecutive from `base + 1`, where `base` is the highest device
//! sequence the server acknowledged and the device then compacted away.
//! Compaction never renumbers or resets the device sequence.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheScope {
    pub server_id: String,
    pub principal_id: String,
    pub profile_id: String,
    pub device_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadIdentity {
    pub download_id: String,
    pub timeline_id: String,
    pub timeline_revision: String,
    pub source_revision: String,
    pub base_viewing_revision: String,
    pub base_manual_epoch: String,
    /// `server_id:restore_epoch` of the server history the base belongs to.
    /// Events cannot cross a restored database or a replaced server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewing_epoch: Option<String>,
}

/// A well-formed `server_id:restore_epoch` history identity.
pub fn viewing_epoch(value: &str) -> bool {
    (3..=257).contains(&value.len())
        && value
            .split_once(':')
            .is_some_and(|(server, epoch)| !server.is_empty() && !epoch.is_empty())
        && value.bytes().all(|b| b.is_ascii_graphic())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineEvent {
    pub scope: CacheScope,
    pub media: DownloadIdentity,
    pub event_id: String,
    pub device_sequence: String,
    pub position_ms: u64,
    pub status: String,
    /// The device clock when the observation was made. Informational and part
    /// of the event's identity; no clock decides authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_time: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Append,
    Duplicate,
    Conflict,
    Invalid,
    Full,
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn sequence(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 20
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

pub fn next_sequence(base: u64, history: &[OfflineEvent]) -> Option<u64> {
    match history.last() {
        Some(e) => sequence(&e.device_sequence)?.checked_add(1),
        None => base.checked_add(1),
    }
}

/// One server result for a sent event: whether it consumed (or had already
/// consumed, as an exact duplicate) that event's sequence. `sequence_gap` and
/// `event_conflict` rejections consume nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Acknowledgement {
    pub event_id: String,
    pub consumed: bool,
}

/// Whether a sync result (`outcome`, `code`) consumed its event's sequence.
/// Every outcome does except the rejections the server records nothing for.
pub fn consumed(outcome: &str, code: Option<&str>) -> bool {
    matches!(outcome, "applied" | "history_only" | "duplicate")
        || (outcome == "rejected"
            && !matches!(
                code,
                Some(
                    "sequence_gap"
                        | "event_conflict"
                        | "sequence_acknowledged"
                        | "acknowledgement_required"
                )
            ))
}

/// The highest sequence that may be compacted after a sync: the leading run
/// of retained events, in order, that the server confirmed by identity. The
/// server's sequence watermark alone is never enough: an event it refused as
/// a conflict sits below that watermark but holds different content.
pub fn acknowledged_through(
    base: u64,
    history: &[OfflineEvent],
    results: &[Acknowledgement],
    next_device_sequence: u64,
) -> u64 {
    let mut through = base;
    for (event, result) in history.iter().zip(results) {
        let Some(sequence) = sequence(&event.device_sequence) else {
            break;
        };
        if result.event_id != event.event_id || !result.consumed || sequence >= next_device_sequence
        {
            break;
        }
        through = sequence;
    }
    through
}

/// Drop the events the server acknowledged through `acknowledged_through`
/// (its next expected sequence minus one). Returns the new base and retained
/// events, or `None` when the acknowledgement does not fit this log: older
/// than the base (already compacted) is a no-op, newer than any event the
/// device wrote means the server's history is not this log's (for example a
/// restored server or another cache root) and must not erase anything.
pub fn compact(
    base: u64,
    history: &[OfflineEvent],
    acknowledged_through: u64,
) -> Option<(u64, Vec<OfflineEvent>)> {
    if acknowledged_through <= base {
        return Some((base, history.to_vec()));
    }
    let last = next_sequence(base, history)?.checked_sub(1)?;
    if acknowledged_through > last {
        return None;
    }
    let retained = history
        .iter()
        .filter(|e| sequence(&e.device_sequence).is_some_and(|s| s > acknowledged_through))
        .cloned()
        .collect();
    Some((acknowledged_through, retained))
}

/// Whether the device may stage `incoming` more bytes: the published and
/// staged total stays within the user's quota and the volume keeps `headroom`
/// free afterwards. A transfer that cannot finish is refused before it starts.
pub fn admits_transfer(
    used: u64,
    incoming: u64,
    quota: u64,
    available: u64,
    headroom: u64,
) -> bool {
    used.checked_add(incoming)
        .is_some_and(|total| total <= quota)
        && available.saturating_sub(headroom) >= incoming
}

pub fn admit(
    base: u64,
    history: &[OfflineEvent],
    event: &OfflineEvent,
    scope: &CacheScope,
    media: &DownloadIdentity,
    duration_ms: u64,
    limit: usize,
) -> Admission {
    if &event.scope != scope
        || &event.media != media
        || !identifier(&event.event_id)
        || event.position_ms > duration_ms
        || event.position_ms > 9_007_199_254_740_991
        || !matches!(
            event.status.as_str(),
            "playing" | "paused" | "ended" | "stopped"
        )
        || sequence(&event.device_sequence).is_none()
        || event.device_time.as_ref().is_some_and(|t| {
            t.is_empty() || t.len() > 64 || !t.bytes().all(|b| b.is_ascii_graphic())
        })
    {
        return Admission::Invalid;
    }
    if let Some(old) = history.iter().find(|e| e.event_id == event.event_id) {
        return if old == event {
            Admission::Duplicate
        } else {
            Admission::Conflict
        };
    }
    if sequence(&event.device_sequence) != next_sequence(base, history) {
        return Admission::Conflict;
    }
    if history.len() >= limit {
        return Admission::Full;
    }
    Admission::Append
}

/// The history (`viewing_epoch`) of a log's compaction point after
/// compacting through `base`: that of the event at `base` when compaction
/// advanced, otherwise unchanged. A restored server never saw that history.
pub fn compacted_epoch(
    previous: Option<&str>,
    history: &[OfflineEvent],
    old_base: u64,
    base: u64,
) -> Option<String> {
    if base == old_base {
        return previous.map(str::to_owned);
    }
    history
        .iter()
        .find(|e| sequence(&e.device_sequence) == Some(base))
        .and_then(|e| e.media.viewing_epoch.clone())
}

/// A device's request that a restored server continue its sequence after
/// the device's compaction point (`downloads::reanchor`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Reanchor {
    /// The history the compaction point belongs to.
    pub from_viewing_epoch: String,
    /// The compaction point: the device never sends a sequence at or below it.
    pub through: u64,
}

/// Whether a batch sent under `batch_epoch` must carry a re-anchor: the
/// device compacted through `base` in another history, which a restored
/// server never consumed, so its sequence would otherwise stop at a gap.
/// A compaction point of unknown history (logs from before this was
/// recorded) is never re-anchored.
pub fn reanchor(base: u64, base_epoch: Option<&str>, batch_epoch: &str) -> Option<Reanchor> {
    let from = base_epoch.filter(|e| *e != batch_epoch && base > 0)?;
    Some(Reanchor {
        from_viewing_epoch: from.to_owned(),
        through: base,
    })
}

/// A log after the person's choice for progress recorded against an
/// earlier history of a restored server (`discard`, `reapply`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub base: u64,
    pub base_epoch: Option<String>,
    pub events: Vec<OfflineEvent>,
    /// Re-applied events dropped because their download has no pin in the
    /// current history (it was deleted, or not re-pinned yet).
    pub dropped: usize,
}

/// How many leading retained events were recorded against another history
/// than `current`: they can never be sent to the restored server as they are.
pub fn foreign_events(history: &[OfflineEvent], current: &str) -> usize {
    history
        .iter()
        .take_while(|e| {
            e.media
                .viewing_epoch
                .as_deref()
                .is_some_and(|epoch| epoch != current)
        })
        .count()
}

/// Discard progress recorded against an earlier history: the leading foreign
/// events are compacted on the device (nothing is sent), so the restored
/// server keeps its own state; the next batch re-anchors past them.
pub fn discard(
    base: u64,
    base_epoch: Option<&str>,
    history: &[OfflineEvent],
    current: &str,
) -> Resolved {
    let foreign = foreign_events(history, current);
    let Some(last) = foreign.checked_sub(1).map(|i| &history[i]) else {
        return Resolved {
            base,
            base_epoch: base_epoch.map(str::to_owned),
            events: history.to_vec(),
            dropped: 0,
        };
    };
    Resolved {
        base: sequence(&last.device_sequence).unwrap_or(base),
        base_epoch: last.media.viewing_epoch.clone(),
        events: history[foreign..].to_vec(),
        dropped: 0,
    }
}

/// Re-apply progress recorded against an earlier history onto the restored
/// server. Each leading foreign event becomes a new event, in order, with the
/// same observation (position, status, device time) pinned to its
/// download's restored pin in `pins`: the viewing base *as restored*
/// (`restored_viewing_revision`), never the current one, so viewing written
/// on the restored server since then is newer and wins (`viewing_newer`).
/// New identities and the next sequences follow every retained event; the
/// foreign events are compacted and the next batch re-anchors past them.
/// Dropped: events whose download has no restored pin in `current`, and
/// events of a download for which the device already holds a newer,
/// current-history observation (re-applied after it, the older observation
/// would come last). Retained events are never renumbered.
pub fn reapply(
    base: u64,
    base_epoch: Option<&str>,
    history: &[OfflineEvent],
    pins: &[DownloadIdentity],
    current: &str,
) -> Option<Resolved> {
    let discarded = discard(base, base_epoch, history, current);
    let foreign = &history[..history.len() - discarded.events.len()];
    let mut events = discarded.events;
    let mut next = next_sequence(discarded.base, &events)?;
    let mut dropped = 0;
    // Downloads the device already observed in the current history.
    let newer: Vec<String> = events.iter().map(|e| e.media.timeline_id.clone()).collect();
    for event in foreign {
        let superseded = newer.contains(&event.media.timeline_id);
        let Some(pin) = pins.iter().filter(|_| !superseded).find(|p| {
            p.download_id == event.media.download_id
                && p.timeline_id == event.media.timeline_id
                && p.viewing_epoch.as_deref() == Some(current)
        }) else {
            dropped += 1;
            continue;
        };
        events.push(OfflineEvent {
            media: pin.clone(),
            event_id: format!("reapplied-{next}"),
            device_sequence: next.to_string(),
            ..event.clone()
        });
        next = next.checked_add(1)?;
    }
    Some(Resolved {
        events,
        dropped,
        ..discarded
    })
}

/// What the server answered when a download the person removed on this
/// device was deleted there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deletion {
    /// The server deleted the download.
    Deleted,
    /// The server no longer has it (deleted before, expired, or not visible).
    Absent,
    /// No answer, or a refusal: the server may still list it.
    Failed,
}

/// A download the person removed from this device stays removed. It is never
/// published (transferred or recovered) again while its removal is pending,
/// whatever the server still lists.
pub fn may_publish(download_id: &str, pending_removals: &[String]) -> bool {
    !pending_removals.iter().any(|id| id == download_id)
}

/// A pending removal is settled only once the server confirms the download
/// itself is gone and its copy is gone from this device; until then the
/// device keeps refusing to save it again and never presents it. The intent
/// is the only record suppressing a copy a failed removal left behind.
pub fn settles(outcome: Deletion, saved_locally: bool) -> bool {
    matches!(outcome, Deletion::Deleted | Deletion::Absent) && !saved_locally
}

/// Descriptive metadata saved with a download and shown offline. Optional
/// and bounded: it never blocks saving a title, and a cache entry stays
/// small.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_year: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
}

pub const MAX_DESCRIPTION_CHARS: usize = 4000;
pub const MAX_GENRES: usize = 10;
pub const MAX_GENRE_CHARS: usize = 64;

fn bounded_text(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| {
        value
            .chars()
            .take(max)
            .collect::<String>()
            .trim_end()
            .to_owned()
    })
}

/// What of a title's resolved metadata (`metadata::resolve` values) travels
/// offline: release year, description and genres, bounded (text is cut at
/// a character boundary, genres deduplicated in order). None when nothing
/// remains.
pub fn offline_metadata(
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Option<OfflineMetadata> {
    let mut genres: Vec<String> = Vec::new();
    for genre in values
        .get("genres")
        .and_then(|g| g.as_array())
        .into_iter()
        .flatten()
        .filter_map(|g| g.as_str())
        .filter_map(|g| bounded_text(g, MAX_GENRE_CHARS))
    {
        if genres.len() == MAX_GENRES {
            break;
        }
        if !genres.contains(&genre) {
            genres.push(genre);
        }
    }
    let metadata = OfflineMetadata {
        release_year: values
            .get("release_year")
            .and_then(|y| y.as_i64())
            .filter(|y| (1..=9999).contains(y))
            .map(|y| y as i32),
        description: values
            .get("description")
            .and_then(|d| d.as_str())
            .and_then(|d| bounded_text(d, MAX_DESCRIPTION_CHARS)),
        genres,
    };
    (metadata != OfflineMetadata::default()).then_some(metadata)
}

/// The metadata a device keeps from what it received: only within the
/// bounds `offline_metadata` produces and not empty. Anything else is
/// dropped rather than refusing the title it came with.
pub fn kept_metadata(metadata: Option<OfflineMetadata>) -> Option<OfflineMetadata> {
    let text_ok = |t: &str, max: usize| !t.trim().is_empty() && t.chars().count() <= max;
    metadata.filter(|m| {
        *m != OfflineMetadata::default()
            && m.release_year.is_none_or(|y| (1..=9999).contains(&y))
            && m.description
                .as_deref()
                .is_none_or(|d| text_ok(d, MAX_DESCRIPTION_CHARS))
            && m.genres.len() <= MAX_GENRES
            && m.genres.iter().all(|g| text_ok(g, MAX_GENRE_CHARS))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_metadata_is_bounded_and_what_the_device_keeps() {
        let values = serde_json::json!({
            "title": "Film",
            "release_year": 1999,
            "description": format!("  {}  ", "é".repeat(MAX_DESCRIPTION_CHARS + 5)),
            "genres": ["Drama", " drama ", "Drama", "", "x".repeat(70)],
            "cast": [{"name": "A"}],
        });
        let values: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_value(values).unwrap();
        let metadata = offline_metadata(&values).unwrap();
        assert_eq!(metadata.release_year, Some(1999));
        assert_eq!(
            metadata.description.as_deref().map(|d| d.chars().count()),
            Some(MAX_DESCRIPTION_CHARS)
        );
        assert_eq!(
            metadata.genres,
            ["Drama", "drama", &"x".repeat(MAX_GENRE_CHARS)]
        );
        // What the server produces is what the device keeps.
        assert_eq!(
            kept_metadata(Some(metadata.clone())),
            Some(metadata.clone())
        );
        // Nothing descriptive: no metadata at all.
        let bare = std::collections::BTreeMap::from([("title".into(), serde_json::json!("Film"))]);
        assert_eq!(offline_metadata(&bare), None);
        // Out of bounds, or empty, is dropped rather than kept.
        for bad in [
            OfflineMetadata::default(),
            OfflineMetadata {
                release_year: Some(0),
                ..Default::default()
            },
            OfflineMetadata {
                description: Some("x".repeat(MAX_DESCRIPTION_CHARS + 1)),
                ..Default::default()
            },
            OfflineMetadata {
                genres: vec!["g".into(); MAX_GENRES + 1],
                ..Default::default()
            },
            OfflineMetadata {
                genres: vec![" ".into()],
                ..Default::default()
            },
        ] {
            assert_eq!(kept_metadata(Some(bad)), None);
        }
    }

    fn restored_event(
        sequence: u64,
        download: &str,
        epoch: &str,
        position_ms: u64,
    ) -> OfflineEvent {
        OfflineEvent {
            scope: CacheScope {
                server_id: "s".into(),
                principal_id: "p".into(),
                profile_id: "f".into(),
                device_id: "d".into(),
            },
            media: DownloadIdentity {
                download_id: download.into(),
                timeline_id: format!("t-{download}"),
                timeline_revision: "1".into(),
                source_revision: "r".into(),
                base_viewing_revision: "4".into(),
                base_manual_epoch: "0".into(),
                viewing_epoch: Some(epoch.into()),
            },
            event_id: format!("e{sequence}"),
            device_sequence: sequence.to_string(),
            position_ms,
            status: "paused".into(),
            device_time: Some(format!("t{sequence}")),
        }
    }

    #[test]
    fn old_progress_cannot_follow_new_progress_from_another_download_of_the_same_timeline() {
        let old = restored_event(3, "a", "s:old", 10);
        let mut new = restored_event(4, "b", "s:new", 90);
        new.media.timeline_id = old.media.timeline_id.clone();
        let pin = DownloadIdentity {
            viewing_epoch: Some("s:new".into()),
            ..old.media.clone()
        };
        let result = reapply(2, Some("s:old"), &[old, new.clone()], &[pin], "s:new").unwrap();
        assert_eq!(result.events, vec![new]);
        assert_eq!(result.dropped, 1);
    }

    #[test]
    fn unknown_history_is_not_compacted_by_restore_choices() {
        let mut event = restored_event(3, "a", "s:old", 10);
        event.media.viewing_epoch = None;
        let result = discard(2, None, &[event.clone()], "s:new");
        assert_eq!(result.base, 2);
        assert_eq!(result.events, vec![event]);
    }

    #[test]
    fn an_earlier_observation_never_follows_a_newer_one_of_its_download() {
        // "c" was downloaded again after the restore and played: its newer,
        // current observation supersedes the earlier one.
        let log = vec![
            restored_event(3, "c", "s:old", 10),
            restored_event(4, "c", "s:new", 40),
        ];
        let pin = DownloadIdentity {
            viewing_epoch: Some("s:new".into()),
            ..restored_event(0, "c", "s:new", 0).media
        };
        let resolved = reapply(2, Some("s:old"), &log, &[pin], "s:new").unwrap();
        assert_eq!((resolved.base, resolved.dropped), (3, 1));
        assert_eq!(resolved.events, log[1..]);
    }

    #[test]
    fn progress_of_an_earlier_history_is_discarded_or_reapplied_by_choice() {
        let log = vec![
            restored_event(3, "a", "s:old", 10),
            restored_event(4, "b", "s:old", 20),
            restored_event(5, "a", "s:old", 30),
            // A download taken after the restore: already current.
            restored_event(6, "c", "s:new", 40),
        ];
        assert_eq!(foreign_events(&log, "s:new"), 3);
        let discarded = discard(2, Some("s:old"), &log, "s:new");
        assert_eq!(
            (
                discarded.base,
                discarded.base_epoch.as_deref(),
                discarded.events.len()
            ),
            (5, Some("s:old"), 1)
        );
        assert_eq!(
            reanchor(5, Some("s:old"), "s:new").map(|r| r.through),
            Some(5)
        );
        // Nothing foreign: unchanged.
        assert_eq!(
            discard(5, Some("s:old"), &log[3..], "s:new").events,
            log[3..]
        );
        let pin = |download: &str, epoch: &str| DownloadIdentity {
            base_viewing_revision: "9".into(),
            viewing_epoch: Some(epoch.into()),
            ..restored_event(0, download, epoch, 0).media
        };
        // "b" was not re-pinned to the current history: its event is dropped.
        let resolved = reapply(
            2,
            Some("s:old"),
            &log,
            &[pin("a", "s:new"), pin("b", "s:old")],
            "s:new",
        )
        .unwrap();
        assert_eq!((resolved.base, resolved.dropped), (5, 1));
        assert_eq!(
            resolved.events[0], log[3],
            "retained events are never renumbered"
        );
        let reapplied: Vec<_> = resolved.events[1..]
            .iter()
            .map(|e| {
                (
                    e.device_sequence.as_str(),
                    e.event_id.as_str(),
                    e.position_ms,
                    e.media.base_viewing_revision.as_str(),
                    e.media.viewing_epoch.as_deref(),
                    e.device_time.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            reapplied,
            [
                ("7", "reapplied-7", 10, "9", Some("s:new"), Some("t3")),
                ("8", "reapplied-8", 30, "9", Some("s:new"), Some("t5")),
            ]
        );
        // The result is a valid log for its own pins.
        for (i, e) in resolved.events.iter().enumerate() {
            assert_eq!(
                admit(
                    resolved.base,
                    &resolved.events[..i],
                    e,
                    &e.scope,
                    &e.media,
                    100,
                    10
                ),
                Admission::Append
            );
        }
    }

    #[test]
    fn a_compaction_point_of_another_history_is_reanchored() {
        let event = |sequence: u64, epoch: &str| OfflineEvent {
            scope: CacheScope {
                server_id: "s".into(),
                principal_id: "p".into(),
                profile_id: "f".into(),
                device_id: "d".into(),
            },
            media: DownloadIdentity {
                download_id: "x".into(),
                timeline_id: "t".into(),
                timeline_revision: "1".into(),
                source_revision: "r".into(),
                base_viewing_revision: "0".into(),
                base_manual_epoch: "0".into(),
                viewing_epoch: Some(epoch.into()),
            },
            event_id: format!("e{sequence}"),
            device_sequence: sequence.to_string(),
            position_ms: 1,
            status: "paused".into(),
            device_time: None,
        };
        let log = [event(3, "s:old"), event(4, "s:new")];
        assert_eq!(
            compacted_epoch(Some("s:x"), &log, 2, 2).as_deref(),
            Some("s:x")
        );
        assert_eq!(compacted_epoch(None, &log, 2, 3).as_deref(), Some("s:old"));
        assert_eq!(
            compacted_epoch(Some("s:old"), &log, 2, 4).as_deref(),
            Some("s:new")
        );
        assert_eq!(
            reanchor(3, Some("s:old"), "s:new"),
            Some(Reanchor {
                from_viewing_epoch: "s:old".into(),
                through: 3
            })
        );
        assert_eq!(reanchor(3, Some("s:new"), "s:new"), None, "same history");
        assert_eq!(reanchor(3, None, "s:new"), None, "unknown history");
        assert_eq!(
            reanchor(0, Some("s:old"), "s:new"),
            None,
            "nothing compacted"
        );
    }

    #[test]
    fn a_removed_download_is_never_saved_again_until_the_server_confirms() {
        let pending = vec!["d1".to_owned()];
        assert!(!may_publish("d1", &pending));
        assert!(may_publish("d2", &pending));
        assert!(may_publish("d1", &[]));
        assert!(settles(Deletion::Deleted, false));
        assert!(settles(Deletion::Absent, false));
        assert!(!settles(Deletion::Failed, false));
        // A copy still saved here keeps its intent, whatever the server says.
        assert!(!settles(Deletion::Deleted, true));
        assert!(!settles(Deletion::Absent, true));
    }
    fn event() -> OfflineEvent {
        OfflineEvent {
            scope: CacheScope {
                server_id: "s".into(),
                principal_id: "p".into(),
                profile_id: "p".into(),
                device_id: "d".into(),
            },
            media: DownloadIdentity {
                download_id: "d".into(),
                timeline_id: "t".into(),
                timeline_revision: "1".into(),
                source_revision: "r".into(),
                base_viewing_revision: "2".into(),
                base_manual_epoch: "3".into(),
                viewing_epoch: Some("server:restore".into()),
            },
            event_id: "e".into(),
            device_sequence: "1".into(),
            position_ms: 100,
            status: "playing".into(),
            device_time: Some("2026-10-11T10:00:00Z".into()),
        }
    }
    #[test]
    fn ordered_exact_retries_preserve_rewinds_and_causal_identity() {
        let first = event();
        let mut history = vec![first.clone()];
        assert_eq!(
            admit(0, &history, &first, &first.scope, &first.media, 200, 1),
            Admission::Duplicate
        );
        let mut next = first.clone();
        next.event_id = "next".into();
        next.device_sequence = "2".into();
        next.position_ms = 10;
        assert_eq!(
            admit(0, &history, &next, &first.scope, &first.media, 200, 2),
            Admission::Append
        );
        history.push(next.clone());
        next.position_ms = 20;
        assert_eq!(
            admit(0, &history, &next, &first.scope, &first.media, 200, 3),
            Admission::Conflict
        );
        next.media.base_manual_epoch = "4".into();
        assert_eq!(
            admit(0, &history, &next, &first.scope, &first.media, 200, 3),
            Admission::Invalid
        );
        next = first.clone();
        next.scope.profile_id = "other".into();
        assert_eq!(
            admit(0, &[], &next, &first.scope, &first.media, 200, 3),
            Admission::Invalid
        );
    }
    #[test]
    fn boundaries_fail_closed() {
        let first = event();
        let mut next = first.clone();
        for value in ["", "01", "-1", "18446744073709551616"] {
            next.device_sequence = value.into();
            assert_eq!(
                admit(0, &[], &next, &first.scope, &first.media, 200, 2),
                Admission::Invalid
            );
        }
        next = first.clone();
        next.device_sequence = "2".into();
        assert_eq!(
            admit(0, &[], &next, &first.scope, &first.media, 200, 2),
            Admission::Conflict
        );
        assert_eq!(
            admit(0, &[], &first, &first.scope, &first.media, 99, 2),
            Admission::Invalid
        );
        assert_eq!(
            admit(0, &[], &first, &first.scope, &first.media, 200, 0),
            Admission::Full
        );
        next.device_sequence = u64::MAX.to_string();
        assert_eq!(next_sequence(0, &[next]), None);
    }
    #[test]
    fn only_confirmed_leading_events_are_acknowledged() {
        let mut history = vec![];
        for n in 1..=3u64 {
            let mut e = event();
            e.event_id = format!("e{n}");
            e.device_sequence = n.to_string();
            history.push(e);
        }
        let ack = |id: &str, consumed| Acknowledgement {
            event_id: id.into(),
            consumed,
        };
        // A conflict at 3 stops there even though the watermark passed it.
        assert_eq!(
            acknowledged_through(
                0,
                &history,
                &[ack("e1", true), ack("e2", true), ack("e3", false)],
                4
            ),
            2
        );
        // A conflict at 1 acknowledges nothing.
        assert_eq!(
            acknowledged_through(0, &history, &[ack("e1", false), ack("e2", true)], 3),
            0
        );
        // Results that do not name the sent events acknowledge nothing.
        assert_eq!(acknowledged_through(0, &history, &[ack("x", true)], 2), 0);
        // Never beyond the server's watermark.
        assert_eq!(
            acknowledged_through(0, &history, &[ack("e1", true), ack("e2", true)], 2),
            1
        );
    }
    #[test]
    fn transfers_respect_quota_and_headroom() {
        assert!(admits_transfer(10, 5, 15, 105, 100));
        assert!(!admits_transfer(10, 6, 15, 1_000, 100));
        assert!(!admits_transfer(0, 5, 15, 104, 100));
        assert!(!admits_transfer(u64::MAX, 1, u64::MAX, u64::MAX, 0));
    }
    #[test]
    fn compaction_keeps_the_sequence_and_refuses_foreign_acknowledgements() {
        let mut history = vec![];
        for n in 1..=3u64 {
            let mut e = event();
            e.event_id = format!("e{n}");
            e.device_sequence = n.to_string();
            history.push(e);
        }
        let (base, kept) = compact(0, &history, 2).unwrap();
        assert_eq!((base, kept.len()), (2, 1));
        assert_eq!(next_sequence(base, &kept), Some(4));
        assert_eq!(compact(base, &kept, 1), Some((2, kept.clone())));
        assert_eq!(compact(base, &kept, 4), None);
        let (base, empty) = compact(base, &kept, 3).unwrap();
        assert_eq!(next_sequence(base, &empty), Some(4));
        let mut retry = history[0].clone();
        retry.device_sequence = "3".into();
        assert_eq!(
            admit(base, &empty, &retry, &retry.scope, &retry.media, 200, 5),
            Admission::Conflict
        );
        let mut fresh = retry.clone();
        fresh.event_id = "e4".into();
        fresh.device_sequence = "4".into();
        assert_eq!(
            admit(base, &empty, &fresh, &fresh.scope, &fresh.media, 200, 5),
            Admission::Append
        );
    }

    #[test]
    fn only_refusals_the_server_records_nothing_for_are_unconsumed() {
        for code in [
            "sequence_gap",
            "event_conflict",
            "sequence_acknowledged",
            "acknowledgement_required",
        ] {
            assert!(!consumed("rejected", Some(code)), "{code}");
        }
        for code in ["not_found", "invalid_event", "invalid_position"] {
            assert!(consumed("rejected", Some(code)), "{code}");
        }
        assert!(consumed("duplicate", Some("applied")));
        assert!(consumed("history_only", Some("viewing_newer")));
    }
}
