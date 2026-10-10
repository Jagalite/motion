//! Identity reconciliation for one traversal attempt. Absence is inferred only
//! inside directories whose complete listing the attempt proved.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Existing {
    pub id: String,
    pub edition: String,
    pub path: String,
    pub revision: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed {
    pub path: String,
    pub revision: String,
}

/// Directory enumeration evidence. Paths are source-relative with `/`
/// separators; the root directory is `""`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Coverage {
    /// Every directory was listed completely and every listed file inspected.
    Complete,
    /// `complete` directories were listed with every media file inspected.
    /// `incomplete` directories were encountered but not proven: unreadable,
    /// unvisited, capped, a mount boundary, or holding a failed inspection.
    Listed {
        complete: BTreeSet<String>,
        incomplete: BTreeSet<String>,
    },
}
impl Coverage {
    /// A path's absence is proven by the nearest encountered ancestor: its own
    /// directory listed completely, or a completely listed ancestor in which the
    /// intermediate directory no longer appeared.
    pub fn covers(&self, path: &str) -> bool {
        let Self::Listed {
            complete,
            incomplete,
        } = self
        else {
            return true;
        };
        let mut dir = parent(path);
        loop {
            if incomplete.contains(dir) {
                return false;
            }
            if complete.contains(dir) {
                return true;
            }
            if dir.is_empty() {
                return false;
            }
            dir = parent(dir);
        }
    }
}
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Assignment {
    /// Same occurrence: unchanged path, or a proven unique move.
    Existing { id: String, edition: String },
    /// A new occurrence of verified content already held by exactly one
    /// edition: a backup copy, not a new work or edition.
    Copy { edition: String },
    /// Unknown content, or content held by several editions (ambiguous).
    New,
    /// Content no edition held before, already observed earlier in this same
    /// attempt at the given index: another occurrence of whatever that earlier
    /// observation was assigned to.
    CopyOf { observation: usize },
    /// The observation lies under a source exclusion and is not cataloged.
    OutOfScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Known occurrences proven absent by covered directory listings.
    pub unavailable: Vec<String>,
    /// One entry per observation.
    pub assignments: Vec<Assignment>,
}

/// Reconcile within a source's scope. Known files under an exclusion are out
/// of scope (returned separately, never as proven absences); observations
/// under an exclusion are assigned `OutOfScope`. Assignments stay aligned
/// with `found`.
pub fn reconcile_scoped(
    old: &[Existing],
    found: &[Observed],
    coverage: &Coverage,
    exclusions: &[String],
) -> (Plan, Vec<String>) {
    let out = |path: &str| crate::sources::excluded(path, exclusions);
    let excluded: Vec<String> = old
        .iter()
        .filter(|f| out(&f.path))
        .map(|f| f.id.clone())
        .collect();
    let in_scope_old: Vec<Existing> = old.iter().filter(|f| !out(&f.path)).cloned().collect();
    let positions: Vec<usize> = (0..found.len()).filter(|i| !out(&found[*i].path)).collect();
    let in_scope_found: Vec<Observed> = positions.iter().map(|i| found[*i].clone()).collect();
    let plan = reconcile_covered(&in_scope_old, &in_scope_found, coverage);
    // Excluded files prove neither absence nor a move, but their known
    // content identity still applies: a byte-identical included file is an
    // occurrence of that edition, not a new work.
    let mut holders: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for file in old {
        holders
            .entry(&file.revision)
            .or_default()
            .insert(&file.edition);
    }
    let mut assignments = vec![Assignment::OutOfScope; found.len()];
    for (k, assignment) in plan.assignments.into_iter().enumerate() {
        assignments[positions[k]] = match assignment {
            Assignment::CopyOf { observation } => Assignment::CopyOf {
                observation: positions[observation],
            },
            Assignment::New => match holders.get(found[positions[k]].revision.as_str()) {
                Some(editions) if editions.len() == 1 => Assignment::Copy {
                    edition: editions.iter().next().unwrap().to_string(),
                },
                _ => Assignment::New,
            },
            other => other,
        };
    }
    (
        Plan {
            unavailable: plan.unavailable,
            assignments,
        },
        excluded,
    )
}

pub fn reconcile(old: &[Existing], found: &[Observed]) -> Plan {
    reconcile_covered(old, found, &Coverage::Complete)
}

pub fn reconcile_covered(old: &[Existing], found: &[Observed], coverage: &Coverage) -> Plan {
    let observed: BTreeSet<_> = found.iter().map(|f| f.path.as_str()).collect();
    let by_path: BTreeMap<_, _> = old.iter().map(|f| (f.path.as_str(), f)).collect();
    let absent = |f: &&Existing| !observed.contains(f.path.as_str());
    // Previously known content not seen at its path, split by whether the
    // attempt proved the absence. An unproven one may still exist, so it makes
    // any move of the same content ambiguous.
    let mut missing: BTreeMap<&str, Vec<&Existing>> = BTreeMap::new();
    let mut unproven: BTreeSet<&str> = BTreeSet::new();
    for file in old.iter().filter(absent) {
        if coverage.covers(&file.path) {
            missing.entry(&file.revision).or_default().push(file);
        } else {
            unproven.insert(&file.revision);
        }
    }
    let mut counts = BTreeMap::new();
    for file in found {
        *counts.entry(file.revision.as_str()).or_insert(0usize) += 1;
    }
    let mut editions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for file in old {
        editions
            .entry(&file.revision)
            .or_default()
            .insert(&file.edition);
    }
    let mut first_seen: BTreeMap<&str, usize> = BTreeMap::new();
    let assignments = found
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let earlier = *first_seen.entry(file.revision.as_str()).or_insert(index);
            if let Some(f) = by_path.get(file.path.as_str()) {
                return Assignment::Existing {
                    id: f.id.clone(),
                    edition: f.edition.clone(),
                };
            }
            let revision = file.revision.as_str();
            if let Some([only]) = missing.get(revision).map(Vec::as_slice)
                && counts.get(revision) == Some(&1)
                && !unproven.contains(revision)
            {
                return Assignment::Existing {
                    id: only.id.clone(),
                    edition: only.edition.clone(),
                };
            }
            match editions.get(revision).map(|e| e.iter().collect::<Vec<_>>()) {
                Some(one) if one.len() == 1 => Assignment::Copy {
                    edition: one[0].to_string(),
                },
                // Known content in several editions is ambiguous: a new work.
                Some(_) => Assignment::New,
                None if earlier < index => Assignment::CopyOf {
                    observation: earlier,
                },
                None => Assignment::New,
            }
        })
        .collect();
    Plan {
        unavailable: old
            .iter()
            .filter(absent)
            .filter(|f| coverage.covers(&f.path))
            .map(|f| f.id.clone())
            .collect(),
        assignments,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Complete,
    Partial,
}
/// A traversal that could not list its root proves nothing and publishes
/// nothing; one that listed the root publishes what it covered.
pub fn outcome(root_listed: bool, incomplete_directories: usize) -> Option<Outcome> {
    match (root_listed, incomplete_directories) {
        (false, _) => None,
        (true, 0) => Some(Outcome::Complete),
        (true, _) => Some(Outcome::Partial),
    }
}

pub fn admit(existing_full_scan: Option<bool>, requested_full: bool) -> Result<bool, &'static str> {
    match existing_full_scan {
        Some(false) if requested_full => {
            Err("an incremental scan is already active; wait before requesting full verification")
        }
        Some(_) => Ok(false),
        None => Ok(true),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    pub attempt: u32,
    /// None means traversal or inspection failed; no disappearance is inferred.
    pub complete_inventory_roots: Option<(String, String)>,
    pub current_root: String,
    pub library_enabled: bool,
    /// Source binding revision when the attempt started, and now.
    pub binding_at_start: u64,
    pub binding_now: u64,
}
pub fn finish(
    job: &crate::jobs::Job,
    observed: &Completion,
) -> (crate::jobs::Job, Vec<crate::jobs::Effect>) {
    crate::jobs::transition(
        job,
        crate::jobs::Input::Finished {
            attempt: observed.attempt,
            success: observed.library_enabled
                && crate::sources::binding_current(observed.binding_at_start, observed.binding_now)
                && observed
                    .complete_inventory_roots
                    .as_ref()
                    .is_some_and(|(start, end)| start == &observed.current_root && end == start),
        },
    )
}
