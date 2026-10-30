use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    pub values: BTreeMap<String, Value>,
    pub tags: BTreeSet<String>,
    /// Only local contributions may suppress imported tags.
    pub excluded_tags: BTreeSet<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub values: BTreeMap<String, Value>,
    pub field_sources: BTreeMap<String, Vec<String>>,
    pub tags: BTreeSet<String>,
    pub tag_sources: BTreeMap<String, Vec<String>>,
    /// Disagreeing providers without a local override are exposed, never silently ranked.
    pub conflicts: BTreeMap<String, BTreeMap<String, Value>>,
}
pub fn resolve(documents: &BTreeMap<String, Contribution>) -> Resolved {
    let mut result = Resolved::default();
    let local = documents.get("local");
    let fields: BTreeSet<_> = documents
        .values()
        .flat_map(|d| d.values.keys().cloned())
        .collect();
    for field in fields {
        if let Some(value) = local.and_then(|d| d.values.get(&field)) {
            result.values.insert(field.clone(), value.clone());
            result.field_sources.insert(field, vec!["local".into()]);
            continue;
        }
        let candidates: BTreeMap<_, _> = documents
            .iter()
            .filter(|(source, _)| source.as_str() != "scan" && source.as_str() != "local")
            .filter_map(|(source, d)| d.values.get(&field).map(|v| (source.clone(), v.clone())))
            .collect();
        if let Some(value) = candidates.values().next() {
            if candidates.values().all(|v| v == value) {
                result.values.insert(field.clone(), value.clone());
                result
                    .field_sources
                    .insert(field, candidates.keys().cloned().collect());
            } else {
                result.conflicts.insert(field, candidates);
            }
        } else if let Some(value) = documents.get("scan").and_then(|d| d.values.get(&field)) {
            result.values.insert(field.clone(), value.clone());
            result.field_sources.insert(field, vec!["scan".into()]);
        }
    }
    for (source, document) in documents {
        for tag in &document.tags {
            if local.is_some_and(|d| d.excluded_tags.contains(tag)) {
                continue;
            }
            result.tags.insert(tag.clone());
            result
                .tag_sources
                .entry(tag.clone())
                .or_default()
                .push(source.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provenance_override_conflict_and_tag_removal() {
        let mut docs = BTreeMap::new();
        let doc = |title: &str, tags: &[&str]| Contribution {
            values: BTreeMap::from([("title".into(), Value::String(title.into()))]),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        docs.insert("scan".into(), doc("Filename", &[]));
        docs.insert("catabolic".into(), doc("Imported", &["favorite", "drama"]));
        docs.insert("local".into(), doc("My title", &["favorite"]));
        assert_eq!(resolve(&docs).values["title"], "My title");
        docs.insert("catabolic".into(), doc("Refreshed", &["drama"]));
        assert!(resolve(&docs).tags.contains("favorite"));
        docs.get_mut("local")
            .unwrap()
            .excluded_tags
            .insert("drama".into());
        assert!(!resolve(&docs).tags.contains("drama"));
        docs.get_mut("local").unwrap().values.clear();
        assert_eq!(resolve(&docs).values["title"], "Refreshed");
        docs.insert("other".into(), doc("Disagreement", &[]));
        assert!(resolve(&docs).conflicts.contains_key("title"));
        assert!(!resolve(&docs).values.contains_key("title"));
    }
}

/// Shared conventional fields; extension fields retain arbitrary non-null JSON.
pub fn valid_field(key: &str, value: &Value) -> bool {
    fn text(value: &Value, max: usize) -> bool {
        value
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= max)
    }
    match key {
        "title" => text(value, 500),
        "description" => value.as_str().is_some_and(|s| s.len() <= 8000),
        "release_year" => value.as_i64().is_some_and(|y| (1..=9999).contains(&y)),
        "release_date" => value.as_str().is_some_and(|s| {
            let bytes = s.as_bytes();
            if bytes.len() != 10
                || bytes[4] != b'-'
                || bytes[7] != b'-'
                || bytes
                    .iter()
                    .enumerate()
                    .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
            {
                return false;
            }
            let year = s[..4].parse::<u32>().unwrap();
            let month = s[5..7].parse::<usize>().unwrap();
            let day = s[8..].parse::<u32>().unwrap();
            let leap =
                year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
            let days = [
                31,
                if leap { 29 } else { 28 },
                31,
                30,
                31,
                30,
                31,
                31,
                30,
                31,
                30,
                31,
            ];
            year > 0 && (1..=12).contains(&month) && day > 0 && day <= days[month - 1]
        }),
        "cast" => value.as_array().is_some_and(|cast| {
            cast.len() <= 100
                && cast.iter().all(|person| {
                    person.as_object().is_some_and(|p| {
                        p.get("name").is_some_and(|v| text(v, 200))
                            && p.get("role").is_none_or(|v| text(v, 200))
                            && p.keys().all(|k| k == "name" || k == "role")
                    })
                })
        }),
        _ => !value.is_null(),
    }
}
#[cfg(test)]
mod field_tests {
    use super::*;
    #[test]
    fn conventional_fields_reject_invalid_dates_and_shapes() {
        assert!(valid_field(
            "release_date",
            &serde_json::json!("2024-02-29")
        ));
        for date in [
            "2025-02-29",
            "2024-13-01",
            "0000-01-01",
            "2024-00-01",
            "2024-01-00",
            "💥-01-01",
        ] {
            assert!(!valid_field("release_date", &serde_json::json!(date)));
        }
        assert!(!valid_field("release_year", &serde_json::json!(2024.5)));
        assert!(!valid_field("cast", &serde_json::json!([{"role":"Host"}])));
        assert!(valid_field(
            "cast",
            &serde_json::json!([{"name":"Actor","role":"Host"}])
        ));
        assert!(valid_field(
            "custom-provider-field",
            &serde_json::json!({"x":1})
        ));
    }
}

/// Source identifiers accepted for user-supplied metadata contributions.
pub fn valid_source(source: &str) -> bool {
    source != "scan"
        && !source.is_empty()
        && source.len() <= 64
        && source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContributionValidationError {
    InvalidSource,
    BoundsOrExclusions,
    FieldNameOrNull,
    ConventionalField,
    Tag,
}

/// Normalize a contribution using one source-aware rule for API writes and
/// imported local data. `scan` belongs to scanner-owned projection evidence.
pub fn normalize_source_contribution(
    source: &str,
    c: &Contribution,
) -> Result<Contribution, ContributionValidationError> {
    use ContributionValidationError::*;
    if !valid_source(source) {
        return Err(InvalidSource);
    }
    if c.values.len() > 100
        || c.tags.len() > 100
        || c.excluded_tags.len() > 100
        || (source != "local" && !c.excluded_tags.is_empty())
    {
        return Err(BoundsOrExclusions);
    }
    for (key, value) in &c.values {
        if key.is_empty() || key.len() > 100 || value.is_null() {
            return Err(FieldNameOrNull);
        }
        if !valid_field(key, value) {
            return Err(ConventionalField);
        }
    }
    let tags =
        |values: &BTreeSet<String>| -> Result<BTreeSet<String>, ContributionValidationError> {
            values
                .iter()
                .map(|tag| {
                    let tag = tag
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .to_lowercase();
                    if tag.is_empty() || tag.len() > 100 {
                        Err(Tag)
                    } else {
                        Ok(tag)
                    }
                })
                .collect()
        };
    Ok(Contribution {
        values: c.values.clone(),
        tags: tags(&c.tags)?,
        excluded_tags: tags(&c.excluded_tags)?,
    })
}

/// Normalize local interchange data, retaining its historical warning wording.
pub fn normalize_contribution(c: &Contribution) -> Result<Contribution, &'static str> {
    normalize_source_contribution("local", c).map_err(|e| match e {
        ContributionValidationError::BoundsOrExclusions => "too many fields or tags",
        ContributionValidationError::FieldNameOrNull
        | ContributionValidationError::ConventionalField => "invalid metadata field",
        ContributionValidationError::Tag => "tags must contain 1-100 bytes",
        ContributionValidationError::InvalidSource => unreachable!("local is a valid source"),
    })
}

/// Advance the durable whole-resource generation after one acknowledged input
/// change. The stored generation is positive and bounded by SQLite's signed
/// integer range; exhaustion fails closed. Deletion and recreation count as
/// separate changes, and the adapter retains generations as tombstones.
pub fn item_revision(generation: u64, changed: bool) -> Option<u64> {
    if generation == 0 || generation > i64::MAX as u64 {
        return None;
    }
    let next = if changed {
        generation.checked_add(1)?
    } else {
        generation
    };
    (next <= i64::MAX as u64).then_some(next)
}

/// A whole-metadata validator binds the item and database history. A restore
/// may rewind the generation, so another binding or history is stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRevision {
    pub binding: String,
    pub generation: u64,
}

/// Derive a validator binding from distinct observed identity facts. Tuple
/// encoding stays unambiguous even when a fact contains a delimiter.
pub fn resource_revision(
    server: &str,
    epoch: &str,
    item: &str,
    generation: u64,
) -> Option<ResourceRevision> {
    use sha2::{Digest, Sha256};
    let generation = item_revision(generation, false)?;
    let identity = serde_json::to_vec(&(server, epoch, item)).ok()?;
    Some(ResourceRevision {
        binding: format!("{:x}", Sha256::digest(identity)),
        generation,
    })
}

pub fn reviewed_item_revision(current: &ResourceRevision, reviewed: &ResourceRevision) -> bool {
    current == reviewed && item_revision(current.generation, false).is_some()
}

#[cfg(test)]
mod revision_tests {
    use super::{ResourceRevision, item_revision, resource_revision, reviewed_item_revision};
    #[test]
    fn item_revision_advances_only_for_changes_and_fails_closed() {
        assert_eq!(item_revision(1, false), Some(1));
        assert_eq!(item_revision(1, true), Some(2));
        assert_eq!(item_revision(2, true), Some(3));
        assert_eq!(item_revision(0, false), None);
        assert_eq!(item_revision(i64::MAX as u64, false), Some(i64::MAX as u64));
        assert_eq!(item_revision(i64::MAX as u64, true), None);
    }

    #[test]
    fn same_generation_in_another_history_is_stale() {
        let before = ResourceRevision {
            binding: "old".into(),
            generation: 2,
        };
        let restored = ResourceRevision {
            binding: "new".into(),
            generation: 2,
        };
        assert!(reviewed_item_revision(&before, &before));
        assert!(!reviewed_item_revision(&restored, &before));
        let item = resource_revision("server", "epoch", "item", 2).unwrap();
        assert_ne!(
            item,
            resource_revision("server", "epoch", "other", 2).unwrap()
        );
        assert_ne!(
            item,
            resource_revision("server", "restored", "item", 2).unwrap()
        );
        assert_ne!(
            resource_revision("a:b", "c", "item", 2),
            resource_revision("a", "b:c", "item", 2)
        );
    }
}

/// A physical root observation must match the registered identity on both
/// sides of the read. A missing identity is not evidence of a match.
pub fn refresh_root_matches(expected: &str, observed: Option<&str>) -> bool {
    observed == Some(expected)
}

/// Facts pinning one sidecar observation to the file and registered source
/// binding from which it was read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshBinding {
    pub work: String,
    pub file: String,
    pub relative: String,
    pub root: String,
    pub root_identity: String,
    pub binding_revision: i64,
    pub file_revision: String,
}

/// The current row carries source enablement, work membership and availability
/// so the core can reject observations whose authority has changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentRefreshBinding {
    pub binding: RefreshBinding,
    pub enabled: bool,
    pub available: bool,
}

pub fn refresh_observation_current(
    observed: &RefreshBinding,
    current: Option<&CurrentRefreshBinding>,
    usable: bool,
) -> bool {
    usable
        && current.is_some_and(|current| {
            current.enabled && current.available && &current.binding == observed
        })
}

/// Decide completion from exact observation and publication identities. The
/// adapter supplies acknowledgments only for effects actually applied in its
/// provisional transaction; rejection requires rolling that transaction back.
pub fn refresh_acknowledged(
    observed: &[String],
    usable: &[String],
    published: &[String],
    authorized: bool,
) -> bool {
    authorized && !observed.is_empty() && observed == usable && usable == published
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefreshResult {
    Unavailable,
    Acknowledged {
        observed: Vec<String>,
        usable: Vec<String>,
        published: Vec<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshCompletion {
    pub job: crate::jobs::Job,
    pub commit_publication: bool,
    pub error: Option<String>,
}

/// Publication is provisional until the final decision; current authorization,
/// the worker attempt and the availability of observations gate its start.
pub fn refresh_may_publish(
    job: &crate::jobs::Job,
    attempt: u32,
    authorized: bool,
    observation_available: bool,
) -> bool {
    job.executing(attempt) && authorized && observation_available
}

/// Derive the durable job state, error and savepoint decision from the facts.
/// An obsolete worker leaves both phase and ancillary error untouched. A
/// matching cancelling attempt still completes cancellation.
pub fn finish_refresh(
    job: &crate::jobs::Job,
    attempt: u32,
    authorized: bool,
    result: &RefreshResult,
) -> Option<RefreshCompletion> {
    if job.attempt != attempt
        || !matches!(
            job.phase,
            crate::jobs::Phase::Running | crate::jobs::Phase::Cancelling
        )
    {
        return None;
    }
    let success = job.executing(attempt)
        && match result {
            RefreshResult::Unavailable => false,
            RefreshResult::Acknowledged {
                observed,
                usable,
                published,
            } => refresh_acknowledged(observed, usable, published, authorized),
        };
    let next = crate::jobs::transition(job, crate::jobs::Input::Finished { attempt, success }).0;
    Some(RefreshCompletion {
        job: next,
        commit_publication: success,
        error: (!success).then(|| "refresh_unavailable_or_unauthorized".to_owned()),
    })
}

/// A bounded refresh is complete only when every requested observation was
/// usable and still current at publication. Zero observations proves nothing.
pub fn refresh_complete(
    observed: usize,
    unusable: usize,
    current: usize,
    authorized: bool,
) -> bool {
    authorized && observed > 0 && unusable == 0 && current == observed
}

#[cfg(test)]
mod refresh_tests {
    #[test]
    fn refresh_requires_complete_current_authorized_observations() {
        for observed in 0..=3 {
            for unusable in 0..=3 {
                for current in 0..=3 {
                    for authorized in [false, true] {
                        assert_eq!(
                            super::refresh_complete(observed, unusable, current, authorized),
                            authorized && observed > 0 && unusable == 0 && current == observed
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod source_validation_tests {
    use super::*;
    #[test]
    fn source_ownership_and_normalization_are_shared() {
        let mut c = Contribution::default();
        c.tags.insert("  Drama   COMEDY  ".into());
        c.excluded_tags.insert("  OLD  ".into());
        let local = normalize_source_contribution("local", &c).unwrap();
        assert_eq!(local.tags, BTreeSet::from(["drama comedy".into()]));
        assert_eq!(local.excluded_tags, BTreeSet::from(["old".into()]));
        assert_eq!(normalize_contribution(&c).unwrap(), local);
        assert_eq!(
            normalize_source_contribution("provider", &c),
            Err(ContributionValidationError::BoundsOrExclusions)
        );
        assert_eq!(
            normalize_source_contribution("scan", &c),
            Err(ContributionValidationError::InvalidSource)
        );
        for name in ["", "Upper", "under_score", "bad space"] {
            assert!(!valid_source(name));
        }
        assert!(valid_source("provider-42"));
        c.excluded_tags.clear();
        assert_eq!(
            normalize_source_contribution("provider", &c).unwrap().tags,
            local.tags
        );
        c.values.insert("title".into(), serde_json::Value::Null);
        assert_eq!(
            normalize_source_contribution("local", &c),
            Err(ContributionValidationError::FieldNameOrNull)
        );
        c.values
            .insert("release_year".into(), serde_json::json!("wrong"));
        c.values.remove("title");
        assert_eq!(
            normalize_source_contribution("local", &c),
            Err(ContributionValidationError::ConventionalField)
        );
    }
}

#[cfg(test)]
mod refresh_identity_tests {
    use super::*;
    use crate::jobs::{Job, Phase};
    #[test]
    fn binding_and_acknowledgments_fence_refresh_completion() {
        let observed = RefreshBinding {
            work: "work".into(),
            file: "f".into(),
            relative: "film.mkv".into(),
            root: "root".into(),
            root_identity: "volume".into(),
            binding_revision: 1,
            file_revision: "v1".into(),
        };
        let mut current = CurrentRefreshBinding {
            binding: observed.clone(),
            enabled: true,
            available: true,
        };
        assert!(refresh_observation_current(&observed, Some(&current), true));
        current.binding.file_revision = "v2".into();
        assert!(!refresh_observation_current(
            &observed,
            Some(&current),
            true
        ));
        current.binding = observed.clone();
        current.available = false;
        assert!(!refresh_observation_current(
            &observed,
            Some(&current),
            true
        ));
        assert!(!refresh_observation_current(&observed, None, true));
        assert!(!refresh_observation_current(
            &observed,
            Some(&current),
            false
        ));
        let ids = vec!["a".into(), "b".into()];
        assert!(refresh_acknowledged(&ids, &ids, &ids, true));
        assert!(!refresh_acknowledged(
            &ids,
            &ids,
            &["b".into(), "a".into()],
            true
        ));
        assert!(!refresh_acknowledged(
            &ids,
            &["a".into()],
            &["a".into()],
            true
        ));
        assert!(!refresh_acknowledged(&[], &[], &[], true));
        assert!(!refresh_acknowledged(&ids, &ids, &ids, false));
        let running = Job {
            phase: Phase::Running,
            attempt: 2,
        };
        assert_eq!(
            finish_refresh(&running, 1, false, &RefreshResult::Unavailable),
            None
        );
        assert_eq!(
            finish_refresh(
                &running,
                2,
                true,
                &RefreshResult::Acknowledged {
                    observed: ids.clone(),
                    usable: ids.clone(),
                    published: ids.clone()
                }
            )
            .unwrap()
            .job
            .phase,
            Phase::Completed
        );
        assert_eq!(
            finish_refresh(
                &Job {
                    phase: Phase::Cancelling,
                    attempt: 2
                },
                2,
                false,
                &RefreshResult::Unavailable
            )
            .unwrap()
            .job
            .phase,
            Phase::Cancelled
        );
        assert_eq!(
            finish_refresh(
                &Job {
                    phase: Phase::Queued,
                    attempt: 2
                },
                2,
                false,
                &RefreshResult::Unavailable
            ),
            None
        );
    }
}
