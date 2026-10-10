//! Client-local event admission, never server viewing authority. Reconciliation
//! belongs to the server; this log preserves the causal facts it will require.
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

pub fn next_sequence(history: &[OfflineEvent]) -> Option<u64> {
    history
        .last()
        .map_or(Some(1), |e| sequence(&e.device_sequence)?.checked_add(1))
}

pub fn admit(
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
    if sequence(&event.device_sequence) != next_sequence(history) {
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
        }
    }
    #[test]
    fn ordered_exact_retries_preserve_rewinds_and_causal_identity() {
        let first = event();
        let mut history = vec![first.clone()];
        assert_eq!(
            admit(&history, &first, &first.scope, &first.media, 200, 1),
            Admission::Duplicate
        );
        let mut next = first.clone();
        next.event_id = "next".into();
        next.device_sequence = "2".into();
        next.position_ms = 10;
        assert_eq!(
            admit(&history, &next, &first.scope, &first.media, 200, 2),
            Admission::Append
        );
        history.push(next.clone());
        next.position_ms = 20;
        assert_eq!(
            admit(&history, &next, &first.scope, &first.media, 200, 3),
            Admission::Conflict
        );
        next.media.base_manual_epoch = "4".into();
        assert_eq!(
            admit(&history, &next, &first.scope, &first.media, 200, 3),
            Admission::Invalid
        );
        next = first.clone();
        next.scope.profile_id = "other".into();
        assert_eq!(
            admit(&[], &next, &first.scope, &first.media, 200, 3),
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
                admit(&[], &next, &first.scope, &first.media, 200, 2),
                Admission::Invalid
            );
        }
        next = first.clone();
        next.device_sequence = "2".into();
        assert_eq!(
            admit(&[], &next, &first.scope, &first.media, 200, 2),
            Admission::Conflict
        );
        assert_eq!(
            admit(&[], &first, &first.scope, &first.media, 99, 2),
            Admission::Invalid
        );
        assert_eq!(
            admit(&[], &first, &first.scope, &first.media, 200, 0),
            Admission::Full
        );
        next.device_sequence = u64::MAX.to_string();
        assert_eq!(next_sequence(&[next]), None);
    }
}
