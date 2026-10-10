//! Selected interchange: `motion_export_v1` export/import of curation that is
//! not derivable from media (local metadata, manual collections, playlists).
//!
//! Exports identify works by verified content revisions and namespaced
//! external identities, never by server-local IDs. Import is preview then
//! commit: the plan records each match, its evidence and the revisions it was
//! computed against; commit applies only the reviewed plan. Ambiguous or
//! unresolved entries are reported, never guessed.
use crate::metadata::Contribution;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type Id = String;
pub const FORMAT: &str = "motion_export_v1";
pub const MAX_WORKS: usize = 50_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedWork {
    pub title: String,
    #[serde(default)]
    pub external_ids: Vec<(String, String)>,
    /// Verified content revisions (SHA-256) of the work's original files.
    #[serde(default)]
    pub content_revisions: Vec<String>,
    #[serde(default)]
    pub local: Option<Contribution>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedCollection {
    pub name: String,
    /// Indices into `works`.
    pub members: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedEntry {
    pub work: usize,
    /// Content revisions identifying the timeline within the work.
    pub content_revisions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportedPlaylist {
    pub name: String,
    pub entries: Vec<ExportedEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub format: String,
    pub works: Vec<ExportedWork>,
    #[serde(default)]
    pub collections: Vec<ExportedCollection>,
    #[serde(default)]
    pub playlists: Vec<ExportedPlaylist>,
}

/// What the importing server knows about one live work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkFacts {
    pub id: Id,
    pub external_ids: BTreeSet<(String, String)>,
    pub content_revisions: BTreeSet<String>,
    /// Revision of its `local` contribution (0 if none) and its document.
    pub local_revision: u64,
    pub local: Option<Contribution>,
    /// Timelines with the content revisions their versions pin.
    pub timelines: Vec<(Id, BTreeSet<String>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Content,
    ExternalIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Replace the work's local contribution, fenced on its revision.
    SetLocal {
        work: Id,
        expected_revision: u64,
        contribution: Contribution,
    },
    CreateCollection {
        name: String,
        members: Vec<Id>,
    },
    CreatePlaylist {
        name: String,
        timelines: Vec<Id>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPlan {
    pub profile: Id,
    /// Per exported work: the matched live work and the evidence used.
    pub matches: Vec<Option<(Id, Evidence)>>,
    pub actions: Vec<Action>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterchangeError {
    UnsupportedFormat,
    TooLarge,
    InvalidReference(usize),
    InvalidName,
    StalePlan,
}

/// Match one exported work: a unique live work sharing verified content
/// wins; otherwise a unique work sharing an external identity; otherwise
/// unresolved. Ambiguity is never broken arbitrarily.
fn match_work(work: &ExportedWork, facts: &[WorkFacts]) -> Result<Option<(Id, Evidence)>, String> {
    let by_content: BTreeSet<&Id> = facts
        .iter()
        .filter(|f| {
            work.content_revisions
                .iter()
                .any(|r| f.content_revisions.contains(r))
        })
        .map(|f| &f.id)
        .collect();
    match by_content.len() {
        1 => {
            return Ok(by_content
                .into_iter()
                .next()
                .map(|id| (id.clone(), Evidence::Content)));
        }
        n if n > 1 => return Err(format!("'{}' matches {n} works by content", work.title)),
        _ => {}
    }
    let by_identity: BTreeSet<&Id> = facts
        .iter()
        .filter(|f| work.external_ids.iter().any(|e| f.external_ids.contains(e)))
        .map(|f| &f.id)
        .collect();
    match by_identity.len() {
        0 => Ok(None),
        1 => Ok(by_identity
            .into_iter()
            .next()
            .map(|id| (id.clone(), Evidence::ExternalIdentity))),
        n => Err(format!("'{}' matches {n} works by identity", work.title)),
    }
}

/// Preview an import for one profile. `existing_names` are the profile's
/// collection and playlist names, which imports never overwrite.
pub fn plan_import(
    export: &Export,
    facts: &[WorkFacts],
    profile: &str,
    existing_names: &BTreeSet<String>,
) -> Result<ImportPlan, InterchangeError> {
    if export.format != FORMAT {
        return Err(InterchangeError::UnsupportedFormat);
    }
    if export.works.len() > MAX_WORKS {
        return Err(InterchangeError::TooLarge);
    }
    let mut warnings = Vec::new();
    let mut matches = Vec::with_capacity(export.works.len());
    for work in &export.works {
        match match_work(work, facts) {
            Ok(m) => {
                if m.is_none() {
                    warnings.push(format!("'{}' has no matching work", work.title));
                }
                matches.push(m);
            }
            Err(ambiguous) => {
                warnings.push(ambiguous);
                matches.push(None);
            }
        }
    }
    let facts_by_id: BTreeMap<&Id, &WorkFacts> = facts.iter().map(|f| (&f.id, f)).collect();
    let mut actions = Vec::new();
    let mut localized = BTreeSet::new();
    for (work, matched) in export.works.iter().zip(&matches) {
        let (Some(local), Some((id, _))) = (&work.local, matched) else {
            continue;
        };
        if !localized.insert(id.clone()) {
            warnings.push(format!(
                "'{}' maps to a work already receiving local metadata; skipped",
                work.title
            ));
            continue;
        }
        let current = facts_by_id[id];
        if current.local.as_ref() == Some(local) {
            continue;
        }
        actions.push(Action::SetLocal {
            work: id.clone(),
            expected_revision: current.local_revision,
            contribution: local.clone(),
        });
    }
    let mut names = existing_names.clone();
    let valid_name = |n: &str| !n.trim().is_empty() && n.len() <= 200;
    for collection in &export.collections {
        if !valid_name(&collection.name) {
            return Err(InterchangeError::InvalidName);
        }
        if !names.insert(collection.name.trim().to_string()) {
            warnings.push(format!(
                "collection '{}' already exists; skipped",
                collection.name
            ));
            continue;
        }
        let mut members = Vec::new();
        for &index in &collection.members {
            match matches.get(index) {
                None => return Err(InterchangeError::InvalidReference(index)),
                Some(Some((id, _))) if !members.contains(id) => members.push(id.clone()),
                Some(Some(_)) => {}
                Some(None) => warnings.push(format!(
                    "collection '{}' member '{}' unresolved",
                    collection.name, export.works[index].title
                )),
            }
        }
        actions.push(Action::CreateCollection {
            name: collection.name.trim().into(),
            members,
        });
    }
    for playlist in &export.playlists {
        if !valid_name(&playlist.name) {
            return Err(InterchangeError::InvalidName);
        }
        if !names.insert(playlist.name.trim().to_string()) {
            warnings.push(format!(
                "playlist '{}' already exists; skipped",
                playlist.name
            ));
            continue;
        }
        let mut timelines = Vec::new();
        for entry in &playlist.entries {
            let matched = matches
                .get(entry.work)
                .ok_or(InterchangeError::InvalidReference(entry.work))?;
            let Some((id, _)) = matched else {
                warnings.push(format!("playlist '{}' entry unresolved", playlist.name));
                continue;
            };
            // The timeline whose pinned content overlaps the entry's, if unique.
            let candidates: Vec<&Id> = facts_by_id[id]
                .timelines
                .iter()
                .filter(|(_, revisions)| {
                    entry
                        .content_revisions
                        .iter()
                        .any(|r| revisions.contains(r))
                })
                .map(|(t, _)| t)
                .collect();
            match candidates.as_slice() {
                [one] => timelines.push((*one).clone()),
                _ => warnings.push(format!(
                    "playlist '{}' entry has {} matching timelines; skipped",
                    playlist.name,
                    candidates.len()
                )),
            }
        }
        actions.push(Action::CreatePlaylist {
            name: playlist.name.trim().into(),
            timelines,
        });
    }
    Ok(ImportPlan {
        profile: profile.into(),
        matches,
        actions,
        warnings,
    })
}

/// Commit rechecks the reviewed plan against current facts: recomputing it
/// must yield exactly the same plan (same matches, revisions and actions).
pub fn commit_import(
    reviewed: &ImportPlan,
    export: &Export,
    facts: &[WorkFacts],
    existing_names: &BTreeSet<String>,
) -> Result<(), InterchangeError> {
    let fresh = plan_import(export, facts, &reviewed.profile, existing_names)?;
    if &fresh != reviewed {
        return Err(InterchangeError::StalePlan);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn facts(id: &str, revisions: &[&str], ids: &[(&str, &str)]) -> WorkFacts {
        WorkFacts {
            id: id.into(),
            external_ids: ids
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            content_revisions: revisions.iter().map(|r| r.to_string()).collect(),
            local_revision: 0,
            local: None,
            timelines: vec![(
                format!("{id}-tl"),
                revisions.iter().map(|r| r.to_string()).collect(),
            )],
        }
    }
    fn work(title: &str, revisions: &[&str], ids: &[(&str, &str)]) -> ExportedWork {
        ExportedWork {
            title: title.into(),
            external_ids: ids
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            content_revisions: revisions.iter().map(|r| r.to_string()).collect(),
            local: Some(Contribution::default()),
        }
    }
    #[test]
    fn content_beats_identity_and_ambiguity_is_reported() {
        let current = vec![
            facts("a", &["ra"], &[("tmdb", "1")]),
            facts("b", &["rb"], &[("tmdb", "2")]),
            facts("c", &["rc"], &[("tmdb", "2")]),
        ];
        let export = Export {
            format: FORMAT.into(),
            works: vec![
                work("A", &["ra"], &[("tmdb", "2")]),
                work("B", &[], &[("tmdb", "1")]),
                work("Dup", &[], &[("tmdb", "2")]),
                work("Gone", &["zz"], &[]),
            ],
            collections: vec![ExportedCollection {
                name: "Faves".into(),
                members: vec![0, 1, 3],
            }],
            playlists: vec![ExportedPlaylist {
                name: "Mix".into(),
                entries: vec![ExportedEntry {
                    work: 0,
                    content_revisions: vec!["ra".into()],
                }],
            }],
        };
        let plan = plan_import(&export, &current, "default", &BTreeSet::new()).unwrap();
        assert_eq!(plan.matches[0], Some(("a".into(), Evidence::Content)));
        assert_eq!(
            plan.matches[1],
            Some(("a".into(), Evidence::ExternalIdentity))
        );
        assert_eq!(plan.matches[2], None, "two works share the identity");
        assert_eq!(plan.matches[3], None);
        assert!(plan.actions.contains(&Action::CreateCollection {
            name: "Faves".into(),
            members: vec!["a".into()]
        }));
        assert!(plan.actions.contains(&Action::CreatePlaylist {
            name: "Mix".into(),
            timelines: vec!["a-tl".into()]
        }));
        // Dup ambiguous, Gone unmatched, B's duplicate local metadata, and the
        // unresolved collection member.
        assert_eq!(plan.warnings.len(), 4, "{:?}", plan.warnings);
        // Existing names are never overwritten; a changed catalog stales the plan.
        let existing: BTreeSet<String> = ["Faves".to_string()].into();
        assert!(
            plan_import(&export, &current, "default", &existing)
                .unwrap()
                .actions
                .iter()
                .all(|a| !matches!(a, Action::CreateCollection { .. }))
        );
        let mut changed = current.clone();
        changed[0].local_revision = 3;
        assert_eq!(
            commit_import(&plan, &export, &changed, &BTreeSet::new()),
            Err(InterchangeError::StalePlan)
        );
        assert_eq!(
            commit_import(&plan, &export, &current, &BTreeSet::new()),
            Ok(())
        );
        let bad = Export {
            format: "plex_export_v1".into(),
            ..export.clone()
        };
        assert_eq!(
            plan_import(&bad, &current, "default", &BTreeSet::new()),
            Err(InterchangeError::UnsupportedFormat)
        );
    }
}
