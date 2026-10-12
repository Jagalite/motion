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

#[cfg(test)]
mod tests {
    use super::*;
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
}
