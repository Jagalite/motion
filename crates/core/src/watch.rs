//! Filesystem change hints. A watcher event is never evidence of what changed:
//! it only makes a source due for a scan, whose coverage and freshness rules
//! (`scan`, `demands`) decide what is published. This module decides which
//! observed paths matter and when a burst of hints becomes one scan request.
//!
//! Debounce: a source is due once it has been quiet for `quiet_ms` since its
//! last hint, or `max_delay_ms` after its first pending hint, whichever comes
//! first, so a continuously changing tree is still rescanned. Lost events
//! (watcher overflow or error) are hints for the whole source.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Debounce {
    pub quiet_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for Debounce {
    fn default() -> Self {
        Self {
            quiet_ms: 5_000,
            max_delay_ms: 60_000,
        }
    }
}

/// Pending hints for one source: when the first and the latest arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub first_ms: u64,
    pub last_ms: u64,
}

/// What the adapter observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hint {
    /// A path (relative to the source root, `/`-separated) was created,
    /// modified, renamed or removed.
    Path(String),
    /// Events may have been lost; the whole source is suspect.
    Lost,
}

/// Whether a hint can affect the catalog. Excluded paths are outside the
/// source's scope. Only media, subtitle sidecars, NFO files and directories
/// (no extension, or a directory rename) are relevant; hidden files such as
/// `.DS_Store` and editor/partial-download temporaries are not.
pub fn relevant(hint: &Hint, exclusions: &[String]) -> bool {
    let path = match hint {
        Hint::Lost => return true,
        Hint::Path(path) => path,
    };
    if path.is_empty() || crate::sources::excluded(path, exclusions) {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.starts_with('.') {
        return false;
    }
    match name.rsplit_once('.') {
        // No extension: most likely a directory (or an extensionless file,
        // which the scanner ignores; a spare scan is harmless).
        None => true,
        Some((_, extension)) => {
            let extension = extension.to_ascii_lowercase();
            MEDIA.contains(&extension.as_str()) || SIDECARS.contains(&extension.as_str())
        }
    }
}

const MEDIA: [&str; 14] = [
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "ts", "m2ts", "mp3", "m4a", "flac", "ogg", "wav",
    "opus",
];
const SIDECARS: [&str; 5] = ["srt", "vtt", "ass", "ssa", "nfo"];

/// Record a relevant hint for `source` at `now_ms`.
pub fn record(pending: &mut BTreeMap<Id, Pending>, source: &str, now_ms: u64) {
    pending
        .entry(source.to_string())
        .and_modify(|p| p.last_ms = p.last_ms.max(now_ms))
        .or_insert(Pending {
            first_ms: now_ms,
            last_ms: now_ms,
        });
}

/// Sources due for a scan at `now_ms`; they are removed from `pending`.
pub fn take_due(pending: &mut BTreeMap<Id, Pending>, now_ms: u64, policy: Debounce) -> Vec<Id> {
    let due: Vec<Id> = pending
        .iter()
        .filter(|(_, p)| {
            now_ms.saturating_sub(p.last_ms) >= policy.quiet_ms
                || now_ms.saturating_sub(p.first_ms) >= policy.max_delay_ms
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in &due {
        pending.remove(id);
    }
    due
}

/// Drop pending hints for sources no longer watched (removed, disabled or
/// relocated); their next scan comes from whoever re-enables them.
pub fn retain_watched(pending: &mut BTreeMap<Id, Pending>, watched: &[Id]) {
    pending.retain(|id, _| watched.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relevance_follows_scope_and_catalog_file_kinds() {
        let exclusions = vec!["Extras".to_string()];
        let path = |p: &str| Hint::Path(p.into());
        assert!(relevant(&path("Movies/Film.MKV"), &exclusions));
        assert!(relevant(&path("Movies/Film.en.srt"), &exclusions));
        assert!(relevant(&path("Movies/movie.nfo"), &exclusions));
        assert!(relevant(&path("Movies/New Folder"), &exclusions));
        assert!(relevant(&Hint::Lost, &exclusions));
        assert!(!relevant(&path("Extras/clip.mkv"), &exclusions));
        assert!(!relevant(&path("Movies/.DS_Store"), &exclusions));
        assert!(!relevant(&path("Movies/Film.mkv.part"), &exclusions));
        assert!(!relevant(&path("Movies/cover.jpg"), &exclusions));
        assert!(!relevant(&path(""), &exclusions));
    }

    #[test]
    fn bursts_coalesce_and_continuous_change_is_bounded() {
        let policy = Debounce {
            quiet_ms: 10,
            max_delay_ms: 100,
        };
        let mut pending = BTreeMap::new();
        record(&mut pending, "a", 0);
        record(&mut pending, "a", 5);
        assert!(
            take_due(&mut pending, 14, policy).is_empty(),
            "not yet quiet"
        );
        assert_eq!(take_due(&mut pending, 15, policy), ["a"]);
        assert!(pending.is_empty(), "one request per burst");
        // A hint every 5ms never goes quiet, but is due by max_delay.
        let mut now = 200;
        record(&mut pending, "b", now);
        let mut fired = None;
        while fired.is_none() {
            now += 5;
            record(&mut pending, "b", now);
            if !take_due(&mut pending, now, policy).is_empty() {
                fired = Some(now);
            }
        }
        assert_eq!(fired, Some(300));
    }

    /// Exhaustive over small event schedules: every hint is followed by a
    /// request no later than `max_delay` after it was first pending, and no
    /// request is made without a pending hint.
    #[test]
    fn every_hint_is_served_within_the_bound_and_none_are_invented() {
        let policy = Debounce {
            quiet_ms: 3,
            max_delay_ms: 7,
        };
        // Each schedule: a hint (or not) at each of 12 ticks.
        for mask in 0u32..(1 << 12) {
            let mut pending = BTreeMap::new();
            let mut oldest_unserved: Option<u64> = None;
            for now in 0..40u64 {
                let hinted = now < 12 && mask & (1 << now) != 0;
                if hinted {
                    record(&mut pending, "s", now);
                    oldest_unserved.get_or_insert(now);
                }
                let due = take_due(&mut pending, now, policy);
                if due.is_empty() {
                    if let Some(first) = oldest_unserved {
                        assert!(
                            now - first < policy.max_delay_ms,
                            "mask {mask:b} late at {now}"
                        );
                    }
                } else {
                    assert!(
                        oldest_unserved.is_some(),
                        "mask {mask:b} invented a request at {now}"
                    );
                    oldest_unserved = None;
                }
            }
            assert!(
                oldest_unserved.is_none(),
                "mask {mask:b} left a hint unserved"
            );
        }
    }
}
