//! Logical catalog identity: works, editions, timelines, versions and file bindings.
//!
//! Adapters load a bounded snapshot of the affected aggregates, call a decision here,
//! and apply the returned change in one writer transaction. Nothing in this module
//! creates, copies or deletes original files: plans move associations only.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub type Id = String;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Work {
    pub id: Id,
    pub revision: u64,
    /// Normalized `(namespace, value)` external identities.
    pub external_ids: BTreeSet<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Equivalence {
    Verified,
    Declared,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Original,
    Generated,
}

/// A version-to-file association. Unknown interval ends are `None`, never zero.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Binding {
    pub file_id: Id,
    pub revision: String,
    pub part: u32,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Version {
    pub id: Id,
    pub origin: Origin,
    pub equivalence: Equivalence,
    pub bindings: Vec<Binding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timeline {
    pub id: Id,
    pub versions: Vec<Version>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edition {
    pub id: Id,
    pub label: String,
    pub timelines: Vec<Timeline>,
}

/// One work and its complete owned structure, as read inside the writer transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Aggregate {
    pub work: Work,
    pub editions: Vec<Edition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IdentityError {
    EmptySelection,
    DuplicateSelection(Id),
    TargetSelected,
    UnknownItem(Id),
    MissingExpectedRevision(Id),
    StaleRevision(Id),
    ExternalIdentityConflict(String),
    UnknownVersion(Id),
    SplitWouldEmptySource,
    InvalidTitle,
    UnknownEquivalenceRequiresNewTimeline,
    InvalidBindings(String),
    SharedFileNeedsKnownIntervals(Id),
    RevisionExhausted(Id),
    IncompatibleKinds(Id),
    SourceHasChildren(Id),
}

pub const MAX_SELECTION: usize = 100;

fn invalid(reason: &str) -> IdentityError {
    IdentityError::InvalidBindings(reason.into())
}

// ---------------------------------------------------------------------------
// Merge

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MergeRequest {
    pub sources: Vec<Id>,
    pub target: Id,
    /// Revisions the operator reviewed; every participant is required.
    pub expected: BTreeMap<Id, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MergePlan {
    pub target: Id,
    /// Every participant revision this plan was computed against.
    pub expected: BTreeMap<Id, u64>,
    /// Editions (with their timelines and versions intact) re-parented to the target.
    pub moved_editions: Vec<(Id, Id)>,
    /// Retired source works; each becomes an alias of the target.
    pub retired: Vec<Id>,
    /// Existing aliases re-pointed so every alias still resolves in one step.
    pub repointed_aliases: Vec<Id>,
    pub external_ids: BTreeSet<(String, String)>,
    pub target_revision: u64,
    pub warnings: Vec<String>,
}

/// Preview an explicit merge. Works merge; their cuts and timelines do not. Positions
/// and watch history stay keyed by the unchanged timeline IDs.
pub fn plan_merge(
    participants: &BTreeMap<Id, Aggregate>,
    aliases: &BTreeMap<Id, Id>,
    request: &MergeRequest,
) -> Result<MergePlan, IdentityError> {
    if request.sources.is_empty() {
        return Err(IdentityError::EmptySelection);
    }
    if request.sources.len() > MAX_SELECTION {
        return Err(invalid("too many merge sources"));
    }
    let mut seen = BTreeSet::new();
    for id in &request.sources {
        if id == &request.target {
            return Err(IdentityError::TargetSelected);
        }
        if !seen.insert(id) {
            return Err(IdentityError::DuplicateSelection(id.clone()));
        }
    }
    let mut expected = BTreeMap::new();
    for id in request.sources.iter().chain([&request.target]) {
        let aggregate = participants
            .get(id)
            .ok_or_else(|| IdentityError::UnknownItem(id.clone()))?;
        let reviewed = *request
            .expected
            .get(id)
            .ok_or_else(|| IdentityError::MissingExpectedRevision(id.clone()))?;
        if aggregate.work.revision != reviewed {
            return Err(IdentityError::StaleRevision(id.clone()));
        }
        expected.insert(id.clone(), reviewed);
    }
    let target = &participants[&request.target];
    let mut by_namespace: BTreeMap<&str, &str> = target
        .work
        .external_ids
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let mut moved_editions = Vec::new();
    let mut warnings = Vec::new();
    for id in &request.sources {
        let source = &participants[id];
        for (namespace, value) in &source.work.external_ids {
            match by_namespace.get(namespace.as_str()) {
                Some(existing) if *existing != value.as_str() => {
                    return Err(IdentityError::ExternalIdentityConflict(namespace.clone()));
                }
                _ => {
                    by_namespace.insert(namespace, value);
                }
            }
        }
        for edition in &source.editions {
            moved_editions.push((edition.id.clone(), id.clone()));
            if !edition.timelines.is_empty() {
                warnings.push(format!(
                    "edition {} keeps its own timelines and viewing history",
                    edition.id
                ));
            }
        }
    }
    let external_ids = by_namespace
        .into_iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    let retired: Vec<Id> = request.sources.clone();
    let retired_set: BTreeSet<&Id> = retired.iter().collect();
    let repointed_aliases = aliases
        .iter()
        .filter(|(_, to)| retired_set.contains(to))
        .map(|(from, _)| from.clone())
        .collect();
    Ok(MergePlan {
        target: request.target.clone(),
        expected,
        moved_editions,
        retired,
        repointed_aliases,
        external_ids,
        target_revision: target
            .work
            .revision
            .checked_add(1)
            .ok_or(IdentityError::RevisionExhausted(request.target.clone()))?,
        warnings,
    })
}

/// Hierarchy guard for merges: only works of one kind merge, and a retired work
/// must not own children that would be stranded behind its alias.
pub fn merge_structure(
    target_kind: &str,
    sources: &[(Id, String, usize)],
) -> Result<(), IdentityError> {
    for (id, kind, children) in sources {
        if kind != target_kind {
            return Err(IdentityError::IncompatibleKinds(id.clone()));
        }
        if *children > 0 {
            return Err(IdentityError::SourceHasChildren(id.clone()));
        }
    }
    Ok(())
}

/// Commit applies only the exact reviewed plan; any changed participant is a conflict.
pub fn commit_merge(
    reviewed: &MergePlan,
    current: &BTreeMap<Id, Aggregate>,
    aliases: &BTreeMap<Id, Id>,
) -> Result<MergePlan, IdentityError> {
    let request = MergeRequest {
        sources: reviewed.retired.clone(),
        target: reviewed.target.clone(),
        expected: reviewed.expected.clone(),
    };
    let fresh = plan_merge(current, aliases, &request)?;
    if &fresh != reviewed {
        // Same revisions but a different structure means the plan was not produced
        // from this catalog; never apply a partially matching proposal.
        return Err(IdentityError::StaleRevision(reviewed.target.clone()));
    }
    Ok(fresh)
}

/// Apply a merge plan to an alias map. Returned aliases always point at live works.
pub fn merged_aliases(aliases: &BTreeMap<Id, Id>, plan: &MergePlan) -> BTreeMap<Id, Id> {
    let retired: BTreeSet<&Id> = plan.retired.iter().collect();
    let mut next: BTreeMap<Id, Id> = aliases
        .iter()
        .map(|(from, to)| {
            let to = if retired.contains(to) {
                plan.target.clone()
            } else {
                to.clone()
            };
            (from.clone(), to)
        })
        .collect();
    for id in &plan.retired {
        next.insert(id.clone(), plan.target.clone());
    }
    next
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Resolved {
    Live(Id),
    Alias { from: Id, to: Id },
}

/// Resolve a possibly retired work ID. Bounded: cycles and dangling aliases are errors.
pub fn resolve(
    live: &BTreeSet<Id>,
    aliases: &BTreeMap<Id, Id>,
    id: &str,
) -> Result<Resolved, IdentityError> {
    if live.contains(id) {
        return Ok(Resolved::Live(id.into()));
    }
    let mut current = id;
    for _ in 0..=aliases.len() {
        match aliases.get(current) {
            Some(next) if live.contains(next) => {
                return Ok(Resolved::Alias {
                    from: id.into(),
                    to: next.clone(),
                });
            }
            Some(next) => current = next,
            None => break,
        }
    }
    Err(IdentityError::UnknownItem(id.into()))
}

// ---------------------------------------------------------------------------
// Split

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SplitRequest {
    pub item: Id,
    pub versions: Vec<Id>,
    pub new_title: String,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SplitMove {
    /// The whole edition, all timelines and versions, moves to the new work.
    Edition { edition: Id },
    /// Whole timelines move into a new edition (same label) on the new work.
    Timelines {
        from_edition: Id,
        new_edition: Id,
        timelines: Vec<Id>,
    },
    /// Some versions of a timeline move to a fresh timeline. Viewing history stays
    /// with the original timeline; no position is transferred.
    Versions {
        from_timeline: Id,
        new_edition: Id,
        new_timeline: Id,
        versions: Vec<Id>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SplitPlan {
    pub item: Id,
    pub expected_revision: u64,
    pub new_item: Id,
    pub new_title: String,
    pub moves: Vec<SplitMove>,
    pub warnings: Vec<String>,
}

/// Preview a split of selected versions into a new work. `new_id` supplies opaque IDs
/// so the decision stays deterministic under test.
pub fn plan_split(
    current: &Aggregate,
    request: &SplitRequest,
    new_id: &mut dyn FnMut() -> Id,
) -> Result<SplitPlan, IdentityError> {
    let title = request.new_title.trim();
    if title.is_empty() || title.len() > 500 {
        return Err(IdentityError::InvalidTitle);
    }
    if current.work.id != request.item {
        return Err(IdentityError::UnknownItem(request.item.clone()));
    }
    if current.work.revision != request.expected_revision {
        return Err(IdentityError::StaleRevision(request.item.clone()));
    }
    if request.expected_revision.checked_add(1).is_none() {
        return Err(IdentityError::RevisionExhausted(request.item.clone()));
    }
    if request.versions.is_empty() {
        return Err(IdentityError::EmptySelection);
    }
    if request.versions.len() > MAX_SELECTION {
        return Err(invalid("too many split versions"));
    }
    let mut selected = BTreeSet::new();
    for id in &request.versions {
        if !selected.insert(id.as_str()) {
            return Err(IdentityError::DuplicateSelection(id.clone()));
        }
    }
    let owned: BTreeSet<&str> = current
        .editions
        .iter()
        .flat_map(|e| &e.timelines)
        .flat_map(|t| &t.versions)
        .map(|v| v.id.as_str())
        .collect();
    if let Some(missing) = selected.iter().find(|v| !owned.contains(**v)) {
        return Err(IdentityError::UnknownVersion((*missing).into()));
    }
    if selected.len() == owned.len() {
        return Err(IdentityError::SplitWouldEmptySource);
    }
    let new_item = new_id();
    let mut moves = Vec::new();
    let mut warnings = Vec::new();
    for edition in &current.editions {
        let full = |t: &Timeline| {
            !t.versions.is_empty() && t.versions.iter().all(|v| selected.contains(v.id.as_str()))
        };
        let touched = |t: &Timeline| t.versions.iter().any(|v| selected.contains(v.id.as_str()));
        if !edition.timelines.iter().any(touched) {
            continue;
        }
        if edition.timelines.iter().all(&full) {
            moves.push(SplitMove::Edition {
                edition: edition.id.clone(),
            });
            continue;
        }
        let whole: Vec<Id> = edition
            .timelines
            .iter()
            .filter(|t| full(t))
            .map(|t| t.id.clone())
            .collect();
        let mut new_edition = None;
        if !whole.is_empty() {
            let id = new_id();
            new_edition = Some(id.clone());
            moves.push(SplitMove::Timelines {
                from_edition: edition.id.clone(),
                new_edition: id,
                timelines: whole,
            });
        }
        for timeline in edition.timelines.iter().filter(|t| touched(t) && !full(t)) {
            let edition_id = match &new_edition {
                Some(id) => id.clone(),
                None => {
                    let id = new_id();
                    new_edition = Some(id.clone());
                    id
                }
            };
            moves.push(SplitMove::Versions {
                from_timeline: timeline.id.clone(),
                new_edition: edition_id,
                new_timeline: new_id(),
                versions: timeline
                    .versions
                    .iter()
                    .filter(|v| selected.contains(v.id.as_str()))
                    .map(|v| v.id.clone())
                    .collect(),
            });
            warnings.push(format!(
                "viewing history remains on timeline {}; the new timeline starts unwatched",
                timeline.id
            ));
        }
    }
    Ok(SplitPlan {
        item: request.item.clone(),
        expected_revision: request.expected_revision,
        new_item,
        new_title: title.into(),
        moves,
        warnings,
    })
}

/// IDs in the order [`plan_split`] allocates them, so a reviewed plan can be
/// recomputed exactly at commit.
fn issued_ids(plan: &SplitPlan) -> Vec<Id> {
    let mut ids = vec![plan.new_item.clone()];
    let mut editions = BTreeSet::new();
    for step in &plan.moves {
        match step {
            SplitMove::Edition { .. } => {}
            SplitMove::Timelines { new_edition, .. } => {
                if editions.insert(new_edition) {
                    ids.push(new_edition.clone());
                }
            }
            SplitMove::Versions {
                new_edition,
                new_timeline,
                ..
            } => {
                if editions.insert(new_edition) {
                    ids.push(new_edition.clone());
                }
                ids.push(new_timeline.clone());
            }
        }
    }
    ids
}

/// Commit applies only the exact reviewed split against the current aggregate.
/// A stale, replayed or structurally different plan is a conflict, and the new
/// work ID must still be unused.
pub fn commit_split(
    reviewed: &SplitPlan,
    current: Option<&Aggregate>,
    new_item_taken: bool,
) -> Result<SplitPlan, IdentityError> {
    let current = current.ok_or_else(|| IdentityError::UnknownItem(reviewed.item.clone()))?;
    if new_item_taken {
        return Err(IdentityError::StaleRevision(reviewed.item.clone()));
    }
    let selected: Vec<Id> = reviewed
        .moves
        .iter()
        .flat_map(|step| match step {
            SplitMove::Edition { edition } => current
                .editions
                .iter()
                .filter(|e| &e.id == edition)
                .flat_map(|e| &e.timelines)
                .flat_map(|t| &t.versions)
                .map(|v| v.id.clone())
                .collect(),
            SplitMove::Timelines { timelines, .. } => current
                .editions
                .iter()
                .flat_map(|e| &e.timelines)
                .filter(|t| timelines.contains(&t.id))
                .flat_map(|t| &t.versions)
                .map(|v| v.id.clone())
                .collect(),
            SplitMove::Versions { versions, .. } => versions.clone(),
        })
        .collect();
    let mut issued = issued_ids(reviewed).into_iter();
    let mut ids = || issued.next().unwrap_or_default();
    let fresh = plan_split(
        current,
        &SplitRequest {
            item: reviewed.item.clone(),
            versions: selected,
            new_title: reviewed.new_title.clone(),
            expected_revision: reviewed.expected_revision,
        },
        &mut ids,
    )?;
    if &fresh != reviewed {
        return Err(IdentityError::StaleRevision(reviewed.item.clone()));
    }
    Ok(fresh)
}

/// Reference application of a merge to loaded aggregates. Adapters apply the same
/// moves in SQL; integration tests compare both results.
pub fn apply_merge(catalog: &mut BTreeMap<Id, Aggregate>, plan: &MergePlan) {
    let mut moved = Vec::new();
    for id in &plan.retired {
        if let Some(source) = catalog.remove(id) {
            moved.extend(source.editions);
        }
    }
    if let Some(target) = catalog.get_mut(&plan.target) {
        target.editions.extend(moved);
        target.work.external_ids = plan.external_ids.clone();
        target.work.revision = plan.target_revision;
    }
}

/// Reference application of a split; see [`apply_merge`].
pub fn apply_split(catalog: &mut BTreeMap<Id, Aggregate>, plan: &SplitPlan) {
    let Some(mut source) = catalog.remove(&plan.item) else {
        return;
    };
    let mut created: Vec<Edition> = Vec::new();
    fn edition<'a>(created: &'a mut Vec<Edition>, id: &Id, label: &str) -> &'a mut Edition {
        if let Some(i) = created.iter().position(|e| &e.id == id) {
            return &mut created[i];
        }
        created.push(Edition {
            id: id.clone(),
            label: label.into(),
            timelines: Vec::new(),
        });
        created.last_mut().unwrap()
    }
    for step in &plan.moves {
        match step {
            SplitMove::Edition { edition: id } => {
                if let Some(i) = source.editions.iter().position(|e| &e.id == id) {
                    created.push(source.editions.remove(i));
                }
            }
            SplitMove::Timelines {
                from_edition,
                new_edition,
                timelines,
            } => {
                let Some(from) = source.editions.iter_mut().find(|e| &e.id == from_edition) else {
                    continue;
                };
                let label = from.label.clone();
                let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut from.timelines)
                    .into_iter()
                    .partition(|t| timelines.contains(&t.id));
                from.timelines = kept;
                edition(&mut created, new_edition, &label)
                    .timelines
                    .extend(taken);
            }
            SplitMove::Versions {
                from_timeline,
                new_edition,
                new_timeline,
                versions,
            } => {
                let Some(from) = source
                    .editions
                    .iter_mut()
                    .find(|e| e.timelines.iter().any(|t| &t.id == from_timeline))
                else {
                    continue;
                };
                let label = from.label.clone();
                let timeline = from
                    .timelines
                    .iter_mut()
                    .find(|t| &t.id == from_timeline)
                    .unwrap();
                let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut timeline.versions)
                    .into_iter()
                    .partition(|v| versions.contains(&v.id));
                timeline.versions = kept;
                edition(&mut created, new_edition, &label)
                    .timelines
                    .push(Timeline {
                        id: new_timeline.clone(),
                        versions: taken,
                    });
            }
        }
    }
    // plan_split rejected exhaustion; the source revision always advances.
    source.work.revision = plan.expected_revision + 1;
    catalog.insert(
        plan.new_item.clone(),
        Aggregate {
            work: Work {
                id: plan.new_item.clone(),
                revision: 1,
                external_ids: BTreeSet::new(),
            },
            editions: created,
        },
    );
    catalog.insert(plan.item.clone(), source);
}

// ---------------------------------------------------------------------------
// Versions and bindings

/// A different encode may join an existing timeline only with explicit equivalence.
pub fn attach_version(timeline: &Timeline, equivalence: Equivalence) -> Result<(), IdentityError> {
    if equivalence == Equivalence::Unknown && !timeline.versions.is_empty() {
        Err(IdentityError::UnknownEquivalenceRequiresNewTimeline)
    } else {
        Ok(())
    }
}

/// Validate one version's bindings plus every other binding of the same files.
/// Multipart parts are consecutive from 1. A file shared by several timelines
/// (multi-episode) needs known, non-overlapping intervals on every binding.
pub fn validate_bindings(
    version: &[Binding],
    other_timeline_bindings: &[(Id, Binding)],
) -> Result<(), IdentityError> {
    if version.is_empty() || version.len() > 100 {
        return Err(invalid("1-100 file bindings required"));
    }
    let parts: BTreeSet<u32> = version.iter().map(|b| b.part).collect();
    if parts.len() != version.len()
        || parts
            .iter()
            .copied()
            .ne(1..=u32::try_from(version.len()).unwrap_or(u32::MAX))
    {
        return Err(invalid("parts must be unique and consecutive from 1"));
    }
    let mut files = BTreeSet::new();
    for binding in version {
        if !files.insert(binding.file_id.as_str()) {
            return Err(invalid("a file appears twice in one version"));
        }
        if let (Some(start), Some(end)) = (binding.start_ms, binding.end_ms)
            && start >= end
        {
            return Err(invalid("interval start must precede end"));
        }
    }
    for binding in version {
        let shared: Vec<&Binding> = other_timeline_bindings
            .iter()
            .filter(|(_, b)| b.file_id == binding.file_id)
            .map(|(_, b)| b)
            .collect();
        if shared.is_empty() {
            continue;
        }
        let known = |b: &Binding| b.start_ms.zip(b.end_ms);
        let Some((start, end)) = known(binding) else {
            return Err(IdentityError::SharedFileNeedsKnownIntervals(
                binding.file_id.clone(),
            ));
        };
        for other in shared {
            if other.revision != binding.revision {
                return Err(invalid("shared file bindings must pin the same revision"));
            }
            let Some((other_start, other_end)) = known(other) else {
                return Err(IdentityError::SharedFileNeedsKnownIntervals(
                    binding.file_id.clone(),
                ));
            };
            if start < other_end && other_start < end {
                return Err(invalid("shared file intervals overlap"));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy viewing attribution

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Attribution {
    /// Exactly one timeline could have produced the legacy position.
    Exact { timeline: Id },
    /// Several cuts were playable; preserve the record and warn. Never copy it.
    Ambiguous { timelines: Vec<Id> },
    /// No playable timeline remains; preserve the record for later correction.
    Orphaned,
}

/// Legacy progress was keyed by work. Attribute it only when one timeline holds
/// original media; otherwise keep it separately for user correction.
pub fn attribute_legacy_progress(timelines: &[(Id, usize)]) -> Attribution {
    let playable: Vec<Id> = timelines
        .iter()
        .filter(|(_, originals)| *originals > 0)
        .map(|(id, _)| id.clone())
        .collect();
    match playable.as_slice() {
        [] => Attribution::Orphaned,
        [one] => Attribution::Exact {
            timeline: one.clone(),
        },
        _ => Attribution::Ambiguous {
            timelines: playable,
        },
    }
}

/// Server-normalized external identity namespace. Movie and TV identities from the
/// same provider never collide, and arbitrary text cannot spoof a namespace.
pub fn external_namespace(provider: &str, kind: &str) -> Option<String> {
    let provider = provider.trim().to_ascii_lowercase();
    let valid = |s: &str| {
        !s.is_empty()
            && s.len() <= 32
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let kind = match kind {
        "movie" => "movie",
        "series" | "season" | "episode" => kind,
        _ => return None,
    };
    valid(&provider).then(|| format!("{provider}:{kind}"))
}
