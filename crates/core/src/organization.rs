//! Organization: typed saved filters, collections, playlists and play queues.
//!
//! Membership and ordering never confer access; adapters still apply the
//! viewer's policy when reading members. Filters are a typed allowlist, never
//! SQL text or executable expressions.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type Id = String;

pub const MAX_TERMS: usize = 32;
pub const MAX_MEMBERS: usize = 1000;
pub const MAX_NAME: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Kind,
    Title,
    Year,
    Genre,
    PersonId,
    Tag,
    Availability,
    Watched,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    Eq,
    Contains,
    Gte,
    Lte,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Boolean(bool),
    Integer(i64),
    Text(String),
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Term {
    pub field: Field,
    pub operator: Operator,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrganizationError {
    InvalidName,
    TooManyTerms,
    InvalidTerm(usize),
    ManualCollectionHasFilter,
    SmartCollectionNeedsFilter,
    SmartCollectionHasMembers,
    TooManyMembers,
    DuplicateMember(Id),
    StaleRevision,
    RevisionExhausted,
    UnknownEntry(Id),
    /// A replacement names a different owning profile than the stored one.
    OwnerChanged,
    /// The saved filter still defines a smart collection.
    FilterInUse,
}

pub fn valid_name(name: &str) -> Result<(), OrganizationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || name.len() > MAX_NAME {
        Err(OrganizationError::InvalidName)
    } else {
        Ok(())
    }
}

const KINDS: &[&str] = &["movie", "series", "season", "episode", "unclassified"];

/// Field/operator/value compatibility for the allowlisted conjunction.
pub fn validate_filter(terms: &[Term]) -> Result<(), OrganizationError> {
    if terms.len() > MAX_TERMS {
        return Err(OrganizationError::TooManyTerms);
    }
    for (index, term) in terms.iter().enumerate() {
        let text = |max: usize| match &term.value {
            Value::Text(t) => !t.trim().is_empty() && t.len() <= max,
            _ => false,
        };
        use Field::*;
        use Operator::*;
        let ok = match (term.field, term.operator) {
            (Kind, Eq) => matches!(&term.value, Value::Text(k) if KINDS.contains(&k.as_str())),
            (Title | Genre | Tag, Eq | Contains) => text(256),
            (PersonId, Eq) => text(256),
            (Year, Eq | Gte | Lte) => {
                matches!(term.value, Value::Integer(y) if (1..=9999).contains(&y))
            }
            (Availability | Watched, Eq) => matches!(term.value, Value::Boolean(_)),
            _ => false,
        };
        if !ok {
            return Err(OrganizationError::InvalidTerm(index));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Manual,
    Smart,
}

/// Manual collections hold explicit unordered members and no filter; smart
/// collections hold a filter and no stored members.
pub fn validate_collection(
    name: &str,
    kind: CollectionKind,
    members: &[Id],
    filter: Option<&Id>,
) -> Result<(), OrganizationError> {
    valid_name(name)?;
    match (kind, filter) {
        (CollectionKind::Manual, Some(_)) => {
            return Err(OrganizationError::ManualCollectionHasFilter);
        }
        (CollectionKind::Smart, None) => return Err(OrganizationError::SmartCollectionNeedsFilter),
        (CollectionKind::Smart, Some(_)) if !members.is_empty() => {
            return Err(OrganizationError::SmartCollectionHasMembers);
        }
        _ => {}
    }
    unique(members.iter())
}

fn unique<'a>(ids: impl ExactSizeIterator<Item = &'a Id>) -> Result<(), OrganizationError> {
    if ids.len() > MAX_MEMBERS {
        return Err(OrganizationError::TooManyMembers);
    }
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(OrganizationError::DuplicateMember(id.clone()));
        }
    }
    Ok(())
}

/// Optimistic concurrency for every organization aggregate.
pub fn advance(current: u64, expected: u64) -> Result<u64, OrganizationError> {
    if current != expected {
        Err(OrganizationError::StaleRevision)
    } else {
        current
            .checked_add(1)
            .ok_or(OrganizationError::RevisionExhausted)
    }
}

/// Removal of an aggregate under its revision check. A saved filter that still
/// defines `dependants` smart collections cannot be removed.
pub fn removable(current: u64, expected: u64, dependants: usize) -> Result<(), OrganizationError> {
    if current != expected {
        Err(OrganizationError::StaleRevision)
    } else if dependants > 0 {
        Err(OrganizationError::FilterInUse)
    } else {
        Ok(())
    }
}

/// Every aggregate keeps the profile that created it; a replacement cannot
/// move it to another profile.
pub fn same_owner(stored: &str, requested: &str) -> Result<(), OrganizationError> {
    if stored == requested {
        Ok(())
    } else {
        Err(OrganizationError::OwnerChanged)
    }
}

/// A replacement written by a viewer whose catalog policy hides some stored
/// members keeps those members: membership is shared by every viewer of the
/// profile, and a member a viewer cannot see is neither disclosed to nor
/// removable by that viewer. `hidden` holds the stored members the writer
/// cannot see; the requested members follow, then the hidden ones in stored
/// order.
pub fn keep_hidden_members(requested: Vec<Id>, stored: &[Id], hidden: &BTreeSet<Id>) -> Vec<Id> {
    let mut out = requested;
    for id in stored {
        if hidden.contains(id) && !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// The ordered counterpart of [`keep_hidden_members`] for playlist and queue
/// entries. `hidden` holds the entry IDs the writer cannot see. Each hidden
/// entry stays directly after the nearest preceding stored entry that the
/// writer could see and kept (or at the front when there is none), so the
/// writer's reordering of what it sees never moves what it cannot. A requested
/// entry ID equal to a hidden entry's is a duplicate.
pub fn keep_hidden_entries(
    requested: Vec<Entry>,
    stored: &[Entry],
    hidden: &BTreeSet<Id>,
) -> Result<Vec<Entry>, OrganizationError> {
    if let Some(clash) = requested.iter().find(|e| hidden.contains(&e.entry_id)) {
        return Err(OrganizationError::DuplicateMember(clash.entry_id.clone()));
    }
    let kept: BTreeSet<&Id> = requested.iter().map(|e| &e.entry_id).collect();
    let mut front = Vec::new();
    let mut after: std::collections::BTreeMap<&Id, Vec<Entry>> = Default::default();
    let mut anchor: Option<&Id> = None;
    for entry in stored {
        if hidden.contains(&entry.entry_id) {
            match anchor {
                Some(a) => after.entry(a).or_default().push(entry.clone()),
                None => front.push(entry.clone()),
            }
        } else if kept.contains(&entry.entry_id) {
            anchor = Some(&entry.entry_id);
        }
    }
    let mut out = front;
    for entry in requested {
        let tail = after.remove(&entry.entry_id);
        out.push(entry);
        out.extend(tail.into_iter().flatten());
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    pub entry_id: Id,
    pub timeline_id: Id,
}

/// Playlists and queues keep explicit entry IDs so the same timeline may
/// appear twice and edits address one occurrence.
pub fn validate_entries(entries: &[Entry]) -> Result<(), OrganizationError> {
    unique(
        entries
            .iter()
            .map(|e| &e.entry_id)
            .collect::<Vec<_>>()
            .into_iter(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    Off,
    One,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Queue {
    pub revision: u64,
    pub entries: Vec<Entry>,
    pub current: Option<Id>,
    pub repeat: Repeat,
    pub shuffle_seed: Option<u64>,
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Playback order: entry order, or a reproducible Fisher-Yates permutation of
/// the entry IDs for a seed. Same entries and seed give the same order on every
/// server and client.
pub fn play_order(queue: &Queue) -> Vec<Id> {
    let mut order: Vec<Id> = queue.entries.iter().map(|e| e.entry_id.clone()).collect();
    if let Some(seed) = queue.shuffle_seed {
        let mut state = seed;
        for i in (1..order.len()).rev() {
            let j = (splitmix64(&mut state) % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }
    }
    order
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Next,
    Previous,
    /// Playback of the current entry ended on its own; `repeat: one` replays it.
    Ended,
}

/// Move within the play order. An explicit next/previous always moves; only a
/// natural end honours `repeat: one`. `Off` stops at either end.
pub fn step(queue: &Queue, step: Step) -> Option<Id> {
    let order = play_order(queue);
    if order.is_empty() {
        return None;
    }
    let Some(position) = queue
        .current
        .as_ref()
        .and_then(|c| order.iter().position(|e| e == c))
    else {
        return match step {
            Step::Previous => order.last().cloned(),
            _ => order.first().cloned(),
        };
    };
    if step == Step::Ended && queue.repeat == Repeat::One {
        return Some(order[position].clone());
    }
    let wrap = queue.repeat != Repeat::Off;
    let target = match step {
        Step::Next | Step::Ended => {
            if position + 1 < order.len() {
                Some(position + 1)
            } else if wrap {
                Some(0)
            } else {
                None
            }
        }
        Step::Previous => {
            if position > 0 {
                Some(position - 1)
            } else if wrap {
                Some(order.len() - 1)
            } else {
                None
            }
        }
    };
    target.map(|i| order[i].clone())
}

/// Apply a revision-checked navigation step.
pub fn advance_queue(
    queue: &Queue,
    expected_revision: u64,
    movement: Step,
) -> Result<Queue, OrganizationError> {
    let revision = advance(queue.revision, expected_revision)?;
    Ok(Queue {
        revision,
        current: step(queue, movement),
        ..queue.clone()
    })
}

/// Replace entries/repeat/shuffle under a revision check. The current entry is
/// kept if it survives; otherwise playback continues at the first surviving
/// entry that followed it in the previous play order, never an arbitrary one.
pub fn edit_queue(
    queue: &Queue,
    expected_revision: u64,
    entries: Vec<Entry>,
    repeat: Repeat,
    shuffle_seed: Option<u64>,
) -> Result<Queue, OrganizationError> {
    validate_entries(&entries)?;
    let revision = advance(queue.revision, expected_revision)?;
    let survivors: BTreeSet<&Id> = entries.iter().map(|e| &e.entry_id).collect();
    let current = queue.current.as_ref().and_then(|current| {
        if survivors.contains(current) {
            return Some(current.clone());
        }
        let old = play_order(queue);
        let at = old.iter().position(|e| e == current)?;
        old[at + 1..]
            .iter()
            .find(|e| survivors.contains(e))
            .cloned()
    });
    Ok(Queue {
        revision,
        entries,
        current,
        repeat,
        shuffle_seed,
    })
}

/// Jump to a specific entry; it must be in the queue.
pub fn select(
    queue: &Queue,
    expected_revision: u64,
    entry: &Id,
) -> Result<Queue, OrganizationError> {
    if !queue.entries.iter().any(|e| &e.entry_id == entry) {
        return Err(OrganizationError::UnknownEntry(entry.clone()));
    }
    let revision = advance(queue.revision, expected_revision)?;
    Ok(Queue {
        revision,
        current: Some(entry.clone()),
        ..queue.clone()
    })
}

/// What a filter may observe about one work for one profile, supplied by the
/// adapter from effective metadata, file availability and viewing state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    pub kind: String,
    pub title: String,
    pub year: Option<i64>,
    pub genres: Vec<String>,
    pub person_ids: Vec<String>,
    pub tags: Vec<String>,
    pub available: bool,
    pub watched: bool,
}

/// Evaluate a validated conjunction. Text comparisons use the deterministic
/// normalized form, so display punctuation and case never change membership.
pub fn matches(terms: &[Term], facts: &Facts) -> bool {
    use crate::matching::normalize_title as norm;
    let text_match = |candidates: &[&str], op: Operator, wanted: &str| {
        let wanted = norm(wanted);
        candidates.iter().any(|c| match op {
            Operator::Eq => norm(c) == wanted,
            Operator::Contains => norm(c).contains(&wanted),
            _ => false,
        })
    };
    terms.iter().all(|term| match (&term.field, &term.value) {
        (Field::Kind, Value::Text(k)) => &facts.kind == k,
        (Field::Title, Value::Text(t)) => text_match(&[&facts.title], term.operator, t),
        (Field::Genre, Value::Text(t)) => text_match(
            &facts.genres.iter().map(String::as_str).collect::<Vec<_>>(),
            term.operator,
            t,
        ),
        (Field::Tag, Value::Text(t)) => text_match(
            &facts.tags.iter().map(String::as_str).collect::<Vec<_>>(),
            term.operator,
            t,
        ),
        (Field::PersonId, Value::Text(id)) => facts.person_ids.iter().any(|p| p == id),
        (Field::Year, Value::Integer(y)) => facts.year.is_some_and(|year| match term.operator {
            Operator::Eq => year == *y,
            Operator::Gte => year >= *y,
            Operator::Lte => year <= *y,
            Operator::Contains => false,
        }),
        (Field::Availability, Value::Boolean(b)) => facts.available == *b,
        (Field::Watched, Value::Boolean(b)) => facts.watched == *b,
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str) -> Entry {
        Entry {
            entry_id: id.into(),
            timeline_id: format!("t-{id}"),
        }
    }
    fn ids(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.entry_id.as_str()).collect()
    }
    fn set(ids: &[&str]) -> BTreeSet<Id> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn removal_checks_revision_then_dependants() {
        assert_eq!(removable(3, 3, 0), Ok(()));
        assert_eq!(removable(3, 2, 0), Err(OrganizationError::StaleRevision));
        assert_eq!(removable(3, 2, 1), Err(OrganizationError::StaleRevision));
        assert_eq!(removable(3, 3, 1), Err(OrganizationError::FilterInUse));
        assert_eq!(removable(u64::MAX, u64::MAX, 0), Ok(()));
    }

    #[test]
    fn owner_is_immutable() {
        assert_eq!(same_owner("p", "p"), Ok(()));
        assert_eq!(same_owner("p", "q"), Err(OrganizationError::OwnerChanged));
    }

    #[test]
    fn hidden_members_survive_a_restricted_replacement() {
        let stored: Vec<Id> = ["a", "h", "b"].map(String::from).to_vec();
        let kept = keep_hidden_members(vec!["b".into(), "c".into()], &stored, &set(&["h"]));
        assert_eq!(kept, ["b", "c", "h"]);
        // Nothing hidden: the request is the result.
        assert_eq!(
            keep_hidden_members(vec!["x".into()], &stored, &BTreeSet::new()),
            ["x"]
        );
    }

    #[test]
    fn hidden_entries_keep_their_anchor() {
        let stored = [e("h0"), e("a"), e("h1"), e("h2"), e("b"), e("c"), e("h3")];
        let hidden = set(&["h0", "h1", "h2", "h3"]);
        // Reorder visible entries: hidden ones follow their anchors.
        let out = keep_hidden_entries(vec![e("c"), e("a"), e("b")], &stored, &hidden).unwrap();
        assert_eq!(ids(&out), ["h0", "c", "h3", "a", "h1", "h2", "b"]);
        // Removing an anchor reattaches to the previous kept visible entry.
        let out = keep_hidden_entries(vec![e("b"), e("c")], &stored, &hidden).unwrap();
        assert_eq!(ids(&out), ["h0", "h1", "h2", "b", "c", "h3"]);
        // A requested ID may not reuse a hidden entry's ID.
        assert_eq!(
            keep_hidden_entries(vec![e("h1")], &stored, &hidden),
            Err(OrganizationError::DuplicateMember("h1".into()))
        );
    }

    /// Exhaustive over 4 stored entries, every hidden mask and every ordered
    /// selection of the visible ones: the result is the request with exactly
    /// the hidden entries inserted once each, the request's relative order is
    /// preserved, and an order-preserving request leaves the stored order.
    #[test]
    fn hidden_entries_exhaustive() {
        let all = ["a", "b", "c", "d"];
        let stored: Vec<Entry> = all.iter().map(|i| e(i)).collect();
        fn arrangements(pool: &[&'static str]) -> Vec<Vec<&'static str>> {
            let mut out = vec![vec![]];
            for (i, x) in pool.iter().enumerate() {
                let mut rest = pool.to_vec();
                rest.remove(i);
                for mut tail in arrangements(&rest) {
                    tail.insert(0, x);
                    out.push(tail);
                }
            }
            out.sort();
            out.dedup();
            out
        }
        for mask in 0u8..16 {
            let hidden: BTreeSet<Id> = all
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, s)| s.to_string())
                .collect();
            let visible: Vec<&str> = all
                .iter()
                .copied()
                .filter(|s| !hidden.contains(*s))
                .collect();
            for request in arrangements(&visible) {
                let requested: Vec<Entry> = request.iter().map(|i| e(i)).collect();
                let out = keep_hidden_entries(requested, &stored, &hidden).unwrap();
                let out_ids = ids(&out);
                let seen_visible: Vec<&str> = out_ids
                    .iter()
                    .copied()
                    .filter(|s| !hidden.contains(*s))
                    .collect();
                let seen_hidden: Vec<&str> = out_ids
                    .iter()
                    .copied()
                    .filter(|s| hidden.contains(*s))
                    .collect();
                let mut sorted_hidden = seen_hidden.clone();
                sorted_hidden.sort();
                let stored_hidden: Vec<&str> = all
                    .iter()
                    .copied()
                    .filter(|s| hidden.contains(*s))
                    .collect();
                assert_eq!(seen_visible, request, "mask {mask}");
                assert_eq!(sorted_hidden, stored_hidden, "mask {mask} {request:?}");
                assert!(validate_entries(&out).is_ok());
                // A request that keeps the stored order changes nothing else.
                if request.windows(2).all(|w| w[0] < w[1]) {
                    let expected: Vec<&str> = all
                        .iter()
                        .copied()
                        .filter(|s| hidden.contains(*s) || request.contains(s))
                        .collect();
                    assert_eq!(out_ids, expected, "mask {mask} {request:?}");
                }
            }
        }
    }
}
