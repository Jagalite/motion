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
    /// modified, renamed or removed. `container` is the adapter's observation
    /// that it is (or may be) a directory: it is a directory now, the event
    /// named a folder, or it was renamed or removed with an unknown kind. A
    /// directory rename can arrive without events for its unchanged children.
    Path { path: String, container: bool },
    /// Events may have been lost; the whole source is suspect.
    Lost,
}

/// Whether a hint can affect the catalog. Excluded paths are outside the
/// source's scope. Directories (whatever their name) and media, subtitle
/// sidecar and NFO files are relevant; hidden entries such as `.DS_Store` and
/// editor/partial-download temporaries are not.
pub fn relevant(hint: &Hint, exclusions: &[String]) -> bool {
    let (path, container) = match hint {
        Hint::Lost => return true,
        Hint::Path { path, container } => (path, *container),
    };
    if path.is_empty() || crate::sources::excluded(path, exclusions) {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.starts_with('.') {
        return false;
    }
    if container {
        return true;
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

/// Whether a watched root must be watched again: its identity (volume and
/// inode) changed or it could not be observed. A watch on a deleted and
/// recreated root (inotify) or a remounted volume no longer reports changes
/// even though the registered path is unchanged. `None` means unobservable.
pub fn rewatch(watched_identity: Option<&str>, current_identity: Option<&str>) -> bool {
    match (watched_identity, current_identity) {
        (Some(watched), Some(current)) => watched != current,
        _ => true,
    }
}

/// Which roots are watched, and which sources are owed a catch-up scan
/// because their watch was lost (identity changed or root unobservable).
/// The obligation survives failed re-registration and is discharged only by
/// a successful one, or by the source no longer being wanted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    watched: BTreeMap<Id, (String, Option<String>)>,
    owed: std::collections::BTreeSet<Id>,
}

/// A registration effect for the adapter to execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Unwatch { id: Id, root: String },
    Watch { id: Id, root: String },
}

impl Registry {
    /// Reconcile with the wanted sources (`id -> (root, current identity)`).
    pub fn plan(&mut self, wanted: &BTreeMap<Id, (String, Option<String>)>) -> Vec<Step> {
        let mut steps = Vec::new();
        let current: Vec<Id> = self.watched.keys().cloned().collect();
        for id in current {
            let (root, identity) = self.watched[&id].clone();
            match wanted.get(&id) {
                Some((want_root, want_identity)) if *want_root == root => {
                    if rewatch(identity.as_deref(), want_identity.as_deref()) {
                        self.watched.remove(&id);
                        self.owed.insert(id.clone());
                        steps.push(Step::Unwatch { id, root });
                    }
                }
                // Removed, disabled or relocated: nothing is owed for it.
                _ => {
                    self.watched.remove(&id);
                    self.owed.remove(&id);
                    steps.push(Step::Unwatch { id, root });
                }
            }
        }
        self.owed.retain(|id| wanted.contains_key(id));
        for (id, (root, _)) in wanted {
            if !self.watched.contains_key(id) {
                steps.push(Step::Watch {
                    id: id.clone(),
                    root: root.clone(),
                });
            }
        }
        steps
    }

    /// Record the outcome of a `Watch` step. Returns true when the source is
    /// owed a catch-up scan that should now be requested.
    pub fn registered(&mut self, id: &str, root: &str, identity: Option<String>, ok: bool) -> bool {
        if !ok {
            return false;
        }
        self.watched
            .insert(id.to_string(), (root.to_string(), identity));
        self.owed.remove(id)
    }

    pub fn owed(&self) -> impl Iterator<Item = &Id> {
        self.owed.iter()
    }
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
        let path = |p: &str| Hint::Path {
            path: p.into(),
            container: false,
        };
        let dir = |p: &str| Hint::Path {
            path: p.into(),
            container: true,
        };
        assert!(
            relevant(&dir("Movies/Movie.2025"), &exclusions),
            "dotted directory"
        );
        assert!(!relevant(&dir("Extras/Movie.2025"), &exclusions));
        assert!(!relevant(&dir("Movies/.Trash"), &exclusions));
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
    fn roots_are_rewatched_when_their_identity_changes_or_is_unknown() {
        assert!(!rewatch(Some("1:2"), Some("1:2")));
        assert!(rewatch(Some("1:2"), Some("1:3")), "recreated root");
        assert!(rewatch(Some("1:2"), None), "root missing");
        assert!(rewatch(None, Some("1:2")), "never observed");
    }

    fn wanted(entries: &[(&str, &str, Option<&str>)]) -> BTreeMap<Id, (String, Option<String>)> {
        entries
            .iter()
            .map(|(id, root, identity)| {
                (
                    id.to_string(),
                    (root.to_string(), identity.map(str::to_string)),
                )
            })
            .collect()
    }

    #[test]
    fn a_lost_watch_owes_a_catch_up_until_registration_succeeds() {
        let mut registry = Registry::default();
        let steps = registry.plan(&wanted(&[("s", "/m", Some("1:1"))]));
        assert_eq!(
            steps,
            [Step::Watch {
                id: "s".into(),
                root: "/m".into()
            }]
        );
        assert!(
            !registry.registered("s", "/m", Some("1:1".into()), true),
            "first watch owes nothing"
        );
        // Unchanged: no steps.
        assert!(
            registry
                .plan(&wanted(&[("s", "/m", Some("1:1"))]))
                .is_empty()
        );
        // Disk unplugged: unwatch, try again, registration fails.
        let steps = registry.plan(&wanted(&[("s", "/m", None)]));
        assert_eq!(steps.len(), 2);
        assert!(!registry.registered("s", "/m", None, false));
        assert!(registry.owed().any(|id| id == "s"));
        // Still missing on the next pass: still owed, retried.
        let steps = registry.plan(&wanted(&[("s", "/m", None)]));
        assert_eq!(
            steps,
            [Step::Watch {
                id: "s".into(),
                root: "/m".into()
            }]
        );
        assert!(!registry.registered("s", "/m", None, false));
        // Disk back (new identity): registration succeeds and pays the debt once.
        registry.plan(&wanted(&[("s", "/m", Some("2:9"))]));
        assert!(registry.registered("s", "/m", Some("2:9".into()), true));
        assert!(registry.owed().next().is_none());
        assert!(
            registry
                .plan(&wanted(&[("s", "/m", Some("2:9"))]))
                .is_empty()
        );
    }

    #[test]
    fn removed_or_relocated_sources_owe_nothing() {
        let mut registry = Registry::default();
        registry.plan(&wanted(&[("s", "/m", Some("1:1"))]));
        registry.registered("s", "/m", Some("1:1".into()), true);
        registry.plan(&wanted(&[("s", "/m", None)]));
        registry.registered("s", "/m", None, false);
        // Disabled while owed.
        assert!(registry.plan(&wanted(&[])).is_empty());
        assert!(registry.owed().next().is_none());
        // Relocated: unwatch old root, watch new, nothing owed.
        registry.plan(&wanted(&[("s", "/m", Some("1:1"))]));
        registry.registered("s", "/m", Some("1:1".into()), true);
        let steps = registry.plan(&wanted(&[("s", "/n", Some("3:3"))]));
        assert_eq!(
            steps,
            [
                Step::Unwatch {
                    id: "s".into(),
                    root: "/m".into()
                },
                Step::Watch {
                    id: "s".into(),
                    root: "/n".into()
                }
            ]
        );
        assert!(!registry.registered("s", "/n", Some("3:3".into()), true));
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
