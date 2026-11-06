//! Catalog relationships are explicit; filenames never infer identity.
use serde::{Deserialize, Serialize};
pub fn valid_relationship(kind: &str, parent_kind: Option<&str>, number: Option<i64>) -> bool {
    match kind {
        "movie" | "series" | "unclassified" => parent_kind.is_none() && number.is_none(),
        "season" => {
            parent_kind == Some("series") && number.is_some_and(|n| (0..=100_000).contains(&n))
        }
        "episode" => {
            parent_kind == Some("season") && number.is_some_and(|n| (0..=100_000).contains(&n))
        }
        _ => false,
    }
}

pub fn validate_structure(
    kind: &str,
    parent_kind: Option<&str>,
    number: Option<i64>,
    self_parent: bool,
    children: &[String],
    editions: i64,
) -> Result<(), &'static str> {
    if self_parent {
        return Err("An item cannot parent itself");
    }
    if !valid_relationship(kind, parent_kind, number) {
        return Err(
            "Expected series > season > episode, with nonnegative season/episode numbers; root types have no parent or number",
        );
    }
    if children.iter().any(|child| {
        !matches!(
            (kind, child.as_str()),
            ("series", "season") | ("season", "episode")
        )
    }) {
        return Err("Reclassification would invalidate existing children");
    }
    if editions > 0 && !can_own_editions(kind) {
        return Err("Series and seasons cannot own editions");
    }
    Ok(())
}
pub fn can_own_editions(kind: &str) -> bool {
    !matches!(kind, "series" | "season")
}

pub fn library_idle(active_jobs: i64) -> Result<(), &'static str> {
    if active_jobs > 0 {
        Err("library_busy")
    } else {
        Ok(())
    }
}
pub fn removable_profile(id: &str) -> bool {
    id != "default"
}

/// Revalidate the exact catalog inspected before changing a library root.
pub fn relocation<T: PartialEq>(
    verified: &[T],
    current: &[T],
    other_root_owners: i64,
) -> Result<(), &'static str> {
    if verified != current {
        Err("catalog_changed")
    } else if other_root_owners > 0 {
        Err("root_registered")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hierarchy_is_strict_and_specials_are_zero_numbered() {
        assert!(valid_relationship("season", Some("series"), Some(0)));
        assert!(valid_relationship("episode", Some("season"), Some(0)));
        for parent in ["movie", "episode", "unclassified"] {
            assert!(!valid_relationship("season", Some(parent), Some(1)));
            assert!(!valid_relationship("episode", Some(parent), Some(1)));
        }
        assert!(!valid_relationship("movie", Some("series"), None));
        assert!(!valid_relationship("episode", Some("season"), Some(-1)));
        assert!(!valid_relationship("series", None, Some(1)));
    }
}

/// Exact target-wins contribution transfers, in reviewed retired-source order.
/// Each key is `(role, source)`; metadata uses the empty role. An NFO origin
/// follows only its moved metadata contribution, never an existing target NFO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContributionOwner {
    pub item: String,
    pub metadata: Vec<String>,
    pub artwork: Vec<(String, String)>,
    pub nfo_origin: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContributionTransfer {
    pub from: String,
    pub to: String,
    pub metadata: Vec<String>,
    pub artwork: Vec<(String, String)>,
    pub nfo_origin: bool,
}
pub fn merge_contributions(
    target: &ContributionOwner,
    sources: &[ContributionOwner],
) -> Vec<ContributionTransfer> {
    let mut metadata: std::collections::BTreeSet<_> = target.metadata.iter().cloned().collect();
    let mut artwork: std::collections::BTreeSet<_> = target.artwork.iter().cloned().collect();
    sources
        .iter()
        .map(|source| {
            let moved: Vec<_> = source
                .metadata
                .iter()
                .filter(|key| metadata.insert((*key).clone()))
                .cloned()
                .collect();
            ContributionTransfer {
                from: source.item.clone(),
                to: target.item.clone(),
                nfo_origin: source.nfo_origin && moved.iter().any(|key| key == "nfo"),
                metadata: moved,
                artwork: source
                    .artwork
                    .iter()
                    .filter(|key| artwork.insert((*key).clone()))
                    .cloned()
                    .collect(),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReassignmentPrecondition {
    Current,
    StaleEdition,
    ForeignWork,
}
pub fn reassignment_precondition(
    current_edition: &str,
    expected_edition: &str,
    current_work: &str,
    target_work: &str,
) -> ReassignmentPrecondition {
    if current_edition != expected_edition {
        ReassignmentPrecondition::StaleEdition
    } else if current_work != target_work {
        ReassignmentPrecondition::ForeignWork
    } else {
        ReassignmentPrecondition::Current
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemPrecondition {
    Current,
    DerivedMembership,
    ImmutableFamily,
}
pub fn item_precondition(
    current: &std::collections::BTreeSet<String>,
    requested: &std::collections::BTreeSet<String>,
    family: &str,
    requested_family: Option<&str>,
) -> ItemPrecondition {
    if current != requested {
        ItemPrecondition::DerivedMembership
    } else if requested_family.is_some_and(|f| f != family) {
        ItemPrecondition::ImmutableFamily
    } else {
        ItemPrecondition::Current
    }
}

/// Historical identities are disclosed only while their exact current
/// projection remains visible. Historical scalar values are not refreshed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptProjection {
    pub libraries: Vec<String>,
    pub parents: Vec<String>,
    pub timeline: Option<String>,
    pub artwork: Option<String>,
}
pub fn project_receipt(
    mut historical: ReceiptProjection,
    visible: &ReceiptProjection,
) -> ReceiptProjection {
    historical
        .libraries
        .retain(|id| visible.libraries.contains(id));
    historical.parents.retain(|id| visible.parents.contains(id));
    if historical.timeline != visible.timeline {
        historical.timeline = None;
    }
    if historical.artwork != visible.artwork {
        historical.artwork = None;
    }
    historical
}

pub fn file_availability(total: usize, available: usize) -> &'static str {
    match (total, available) {
        (0, _) => "unknown",
        (n, a) if a == n => "available",
        (_, 0) => "unavailable",
        _ => "degraded",
    }
}
pub fn split_root_kind(kind: &str) -> bool {
    matches!(kind, "movie" | "unclassified")
}

pub fn default_timeline(visible: &[String]) -> Option<String> {
    match visible {
        [only] => Some(only.clone()),
        _ => None,
    }
}
pub fn generated_version(parts: &[bool]) -> bool {
    !parts.is_empty() && parts.iter().all(|g| *g)
}
/// SQL may materialize visibility with EXISTS/NOT EXISTS, but must match this
/// predicate: a multipart version is indivisible, including unavailable parts.
pub fn version_visible(administrator: bool, part_visibility: &[bool]) -> bool {
    administrator || (!part_visibility.is_empty() && part_visibility.iter().all(|visible| *visible))
}

/// A reviewed proposal is not authority across identities, deadlines or a
/// restore. Missing historical bindings fail closed rather than being adopted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReviewedAuthority<'a> {
    pub principal: &'a str,
    pub history: &'a str,
    pub expires: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAdmissionError {
    ForeignActor,
    Expired,
    HistoryChanged,
}
pub fn review_admission(
    reviewed: ReviewedAuthority<'_>,
    principal: &str,
    history: &str,
    now: i64,
) -> Result<(), ReviewAdmissionError> {
    if reviewed.principal != principal {
        Err(ReviewAdmissionError::ForeignActor)
    } else if now >= reviewed.expires {
        Err(ReviewAdmissionError::Expired)
    } else if reviewed.history.is_empty() || reviewed.history != history {
        Err(ReviewAdmissionError::HistoryChanged)
    } else {
        Ok(())
    }
}
