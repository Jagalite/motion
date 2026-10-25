//! Ordering of independently authenticated native identity transitions.
//!
//! This module authenticates no database row, receipt, image, or grant. Adapters
//! must verify those commitments before using its result for admission. In
//! particular, a pending transition is recovery work, not a current identity.
use crate::catalog_backend::Identity;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    Restore,
    Native41Bootstrap,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionStatus {
    Pending,
    Acknowledged,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    pub id: String,
    pub predecessor: Option<String>,
    pub from: Identity,
    pub to: Identity,
    pub kind: TransitionKind,
    pub status: TransitionStatus,
}
#[derive(Debug, Eq, PartialEq)]
pub enum Resolution<'a> {
    Current {
        identity: &'a Identity,
        ordered: Vec<&'a Transition>,
    },
    Pending {
        transition: &'a Transition,
        acknowledged: Vec<&'a Transition>,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidIdentity,
    InvalidTransition,
    DuplicateId,
    AmbiguousPredecessor,
    DisconnectedHistory,
    PendingHasSuccessor,
    ReusedIncarnation,
    LimitExceeded,
}

/// Resolve a complete inventory, regardless of storage order. The immutable
/// original binding (after its separately verified 39→40 acknowledgment) is
/// the only root. Neither disconnected rows nor a second root are ignored.
pub fn resolve<'a>(
    original: &'a Identity,
    transitions: &'a [Transition],
) -> Result<Resolution<'a>, Error> {
    original.validate().map_err(|_| Error::InvalidIdentity)?;
    if original.schema_version != 40 {
        return Err(Error::InvalidIdentity);
    }
    if transitions.len() > 33 {
        return Err(Error::LimitExceeded);
    }
    let mut ids = BTreeSet::new();
    let mut successors = BTreeMap::new();
    for transition in transitions {
        if transition.id.is_empty() {
            return Err(Error::InvalidTransition);
        }
        if !ids.insert(transition.id.as_str()) {
            return Err(Error::DuplicateId);
        }
        if successors
            .insert(transition.predecessor.as_deref(), transition)
            .is_some()
        {
            return Err(Error::AmbiguousPredecessor);
        }
    }
    let mut identity = original;
    let mut predecessor = None;
    let mut ordered = Vec::with_capacity(transitions.len());
    let mut incarnations = BTreeSet::from([original.incarnation.as_str()]);
    let mut restores = 0;
    while let Some(transition) = successors.remove(&predecessor) {
        transition
            .from
            .validate()
            .map_err(|_| Error::InvalidIdentity)?;
        transition
            .to
            .validate()
            .map_err(|_| Error::InvalidIdentity)?;
        if &transition.from != identity || transition.to.catalog_id != original.catalog_id {
            return Err(Error::InvalidTransition);
        }
        match transition.kind {
            TransitionKind::Restore => {
                restores += 1;
                if restores > 32 {
                    return Err(Error::LimitExceeded);
                }
                if transition.to.schema_version != identity.schema_version {
                    return Err(Error::InvalidTransition);
                }
                if !incarnations.insert(transition.to.incarnation.as_str()) {
                    return Err(Error::ReusedIncarnation);
                }
            }
            TransitionKind::Native41Bootstrap => {
                if identity.schema_version != 40
                    || transition.to.schema_version != 41
                    || transition.to.incarnation != identity.incarnation
                {
                    return Err(Error::InvalidTransition);
                }
            }
        }
        if transition.status == TransitionStatus::Pending {
            if successors.contains_key(&Some(transition.id.as_str())) {
                return Err(Error::PendingHasSuccessor);
            }
            if !successors.is_empty() {
                return Err(Error::DisconnectedHistory);
            }
            return Ok(Resolution::Pending {
                transition,
                acknowledged: ordered,
            });
        }
        predecessor = Some(transition.id.as_str());
        identity = &transition.to;
        ordered.push(transition);
    }
    if !successors.is_empty() {
        return Err(Error::DisconnectedHistory);
    }
    Ok(Resolution::Current { identity, ordered })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(incarnation: &str, schema_version: u32) -> Identity {
        Identity {
            catalog_id: "catalog".into(),
            incarnation: incarnation.into(),
            schema_version,
        }
    }
    fn step(
        id: &str,
        predecessor: Option<&str>,
        from: Identity,
        to: Identity,
        kind: TransitionKind,
    ) -> Transition {
        Transition {
            id: id.into(),
            predecessor: predecessor.map(str::to_owned),
            from,
            to,
            kind,
            status: TransitionStatus::Acknowledged,
        }
    }
    #[test]
    fn restore_and_bootstrap_order_is_not_fixed() {
        let original = identity("a", 40);
        for restore_first in [false, true] {
            let middle = identity(
                if restore_first { "b" } else { "a" },
                if restore_first { 40 } else { 41 },
            );
            let first = step(
                "first",
                None,
                original.clone(),
                middle.clone(),
                if restore_first {
                    TransitionKind::Restore
                } else {
                    TransitionKind::Native41Bootstrap
                },
            );
            let second = step(
                "second",
                Some("first"),
                middle,
                identity("b", 41),
                if restore_first {
                    TransitionKind::Native41Bootstrap
                } else {
                    TransitionKind::Restore
                },
            );
            let rows = [second, first];
            let Resolution::Current { identity, ordered } = resolve(&original, &rows).unwrap()
            else {
                panic!("pending")
            };
            assert_eq!(identity, &self::identity("b", 41));
            assert_eq!(
                ordered.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
                ["first", "second"]
            );
        }
    }
    #[test]
    fn pending_is_never_current_and_cannot_have_successors() {
        let original = identity("a", 40);
        let mut pending = step(
            "pending",
            None,
            original.clone(),
            identity("b", 40),
            TransitionKind::Restore,
        );
        pending.status = TransitionStatus::Pending;
        assert!(matches!(
            resolve(&original, &[pending.clone()]),
            Ok(Resolution::Pending { .. })
        ));
        let next = step(
            "next",
            Some("pending"),
            pending.to.clone(),
            identity("b", 41),
            TransitionKind::Native41Bootstrap,
        );
        assert_eq!(
            resolve(&original, &[pending, next]),
            Err(Error::PendingHasSuccessor)
        );
    }
    #[test]
    fn complete_inventory_rejects_gaps_cycles_forks_and_duplicate_ids() {
        let original = identity("a", 40);
        let first = step(
            "first",
            None,
            original.clone(),
            identity("b", 40),
            TransitionKind::Restore,
        );
        for predecessor in [Some("missing"), Some("first")] {
            let mut gap = first.clone();
            gap.predecessor = predecessor.map(str::to_owned);
            assert_eq!(resolve(&original, &[gap]), Err(Error::DisconnectedHistory));
        }
        assert_eq!(
            resolve(&original, &[first.clone(), first.clone()]),
            Err(Error::DuplicateId)
        );
        let mut fork = first.clone();
        fork.id = "fork".into();
        assert_eq!(
            resolve(&original, &[first, fork]),
            Err(Error::AmbiguousPredecessor)
        );
    }
    #[test]
    fn exact_identity_and_incarnation_history_are_preserved() {
        let original = identity("a", 40);
        let first = step(
            "first",
            None,
            original.clone(),
            identity("b", 40),
            TransitionKind::Restore,
        );
        let reused = step(
            "second",
            Some("first"),
            first.to.clone(),
            original.clone(),
            TransitionKind::Restore,
        );
        assert_eq!(
            resolve(&original, &[first.clone(), reused]),
            Err(Error::ReusedIncarnation)
        );
        for alteration in 0..4 {
            let mut bad = first.clone();
            match alteration {
                0 => bad.from.incarnation = "foreign".into(),
                1 => bad.to.catalog_id = "foreign".into(),
                2 => bad.to.schema_version = 41,
                _ => bad.to.incarnation.clear(),
            }
            assert!(resolve(&original, &[bad]).is_err());
        }
    }
    #[test]
    fn complete_restore_bound_and_empty_history() {
        let original = identity("0", 40);
        assert_eq!(
            resolve(&original, &[]),
            Ok(Resolution::Current {
                identity: &original,
                ordered: vec![]
            })
        );
        let mut rows: Vec<Transition> = Vec::new();
        for index in 0..32 {
            rows.push(step(
                &format!("r{index}"),
                if index == 0 {
                    None
                } else {
                    Some(rows.last().unwrap().id.as_str())
                },
                identity(&index.to_string(), 40),
                identity(&(index + 1).to_string(), 40),
                TransitionKind::Restore,
            ));
        }
        assert!(matches!(
            resolve(&original, &rows),
            Ok(Resolution::Current { .. })
        ));
        rows.push(step(
            "bootstrap",
            Some("r31"),
            identity("32", 40),
            identity("32", 41),
            TransitionKind::Native41Bootstrap,
        ));
        assert!(resolve(&original, &rows).is_ok());
        rows.push(step(
            "extra",
            Some("bootstrap"),
            identity("32", 41),
            identity("33", 41),
            TransitionKind::Restore,
        ));
        assert_eq!(resolve(&original, &rows), Err(Error::LimitExceeded));
    }
}
