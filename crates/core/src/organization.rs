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
