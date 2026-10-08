//! Identity reconciliation for a complete, successfully inspected inventory.
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub unavailable: Vec<String>,
    /// One entry per observation; None requests a new identity.
    pub identities: Vec<Option<(String, String)>>,
}
pub fn reconcile(old: &[Existing], found: &[Observed]) -> Plan {
    let observed: BTreeSet<_> = found.iter().map(|f| f.path.as_str()).collect();
    let by_path: BTreeMap<_, _> = old.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut missing: BTreeMap<&str, Vec<&Existing>> = BTreeMap::new();
    for file in old.iter().filter(|f| !observed.contains(f.path.as_str())) {
        missing.entry(&file.revision).or_default().push(file);
    }
    let mut counts = BTreeMap::new();
    for file in found {
        *counts.entry(file.revision.as_str()).or_insert(0usize) += 1;
    }
    let identities = found
        .iter()
        .map(|file| {
            by_path
                .get(file.path.as_str())
                .copied()
                .or_else(|| {
                    let candidates = missing.get(file.revision.as_str())?;
                    (candidates.len() == 1 && counts.get(file.revision.as_str()) == Some(&1))
                        .then_some(candidates[0])
                })
                .map(|f| (f.id.clone(), f.edition.clone()))
        })
        .collect();
    Plan {
        unavailable: old
            .iter()
            .filter(|f| !observed.contains(f.path.as_str()))
            .map(|f| f.id.clone())
            .collect(),
        identities,
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
                && observed
                    .complete_inventory_roots
                    .as_ref()
                    .is_some_and(|(start, end)| start == &observed.current_root && end == start),
        },
    )
}
