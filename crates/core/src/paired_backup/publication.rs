//! Publication decisions only. Observations carry no filesystem/export authority.
//! A linked name is visible before directory fsync. Neither cancellation nor a
//! failed/unknown syscall can turn possible visibility back into clean absence.
use super::{Artifact, digest};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub parent_device: u64,
    pub parent_inode: u64,
    pub file_device: u64,
    pub file_inode: u64,
    pub name: String,
    pub artifact: Artifact,
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
impl Evidence {
    pub fn valid(&self) -> bool {
        self.parent_inode != 0
            && self.file_inode != 0
            && self.parent_device == self.file_device
            && valid_name(&self.name)
            && self.artifact.bytes > 0
            && digest(&self.artifact.sha256)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    Ready,
    LinkAttempted,
    LinkedUnconfirmed,
    Durable,
    Abandoned,
    Collision,
    OutcomeUnknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    evidence: Evidence,
    phase: Phase,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    BeginLink,
    Cancel,
    LinkSucceeded,
    DestinationAlreadyExists,
    LinkOutcomeUnknown,
    DirectorySynced,
    DirectorySyncFailed,
    /// Fresh owner observed the exact name/inode/bytes. This alone does not
    /// prove durability; a successful directory sync must still follow.
    ReconciledExact(Evidence),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    LinkCreateOnly,
    SyncDirectory,
    None,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    InvalidEvidence,
    InvalidPhase,
    Mismatch,
}
impl Publication {
    pub fn prepared(evidence: Evidence) -> Result<Self, Rejection> {
        if !evidence.valid() {
            return Err(Rejection::InvalidEvidence);
        }
        Ok(Self {
            evidence,
            phase: Phase::Ready,
        })
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn evidence(&self) -> &Evidence {
        &self.evidence
    }
    pub fn apply(&mut self, input: Input) -> Result<Effect, Rejection> {
        use Phase::*;
        let (phase, effect) = match (self.phase, input) {
            (Ready, Input::BeginLink) => (LinkAttempted, Effect::LinkCreateOnly),
            (Ready, Input::Cancel) => (Abandoned, Effect::None),
            (LinkAttempted, Input::LinkSucceeded) => (LinkedUnconfirmed, Effect::SyncDirectory),
            (LinkAttempted, Input::DestinationAlreadyExists) => (Collision, Effect::None),
            (LinkAttempted, Input::LinkOutcomeUnknown) => (OutcomeUnknown, Effect::None),
            (LinkAttempted, Input::Cancel) => (OutcomeUnknown, Effect::None),
            (LinkedUnconfirmed, Input::DirectorySynced) => (Durable, Effect::None),
            (LinkedUnconfirmed, Input::DirectorySyncFailed) => (LinkedUnconfirmed, Effect::None),
            (LinkedUnconfirmed | OutcomeUnknown, Input::ReconciledExact(actual)) => {
                if actual != self.evidence {
                    return Err(Rejection::Mismatch);
                }
                (LinkedUnconfirmed, Effect::SyncDirectory)
            }
            (
                LinkedUnconfirmed | OutcomeUnknown | Durable | Abandoned | Collision,
                Input::Cancel,
            ) => (self.phase, Effect::None),
            _ => return Err(Rejection::InvalidPhase),
        };
        self.phase = phase;
        Ok(effect)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn evidence() -> Evidence {
        Evidence {
            parent_device: 1,
            parent_inode: 2,
            file_device: 1,
            file_inode: 3,
            name: "pair-1.bundle".into(),
            artifact: Artifact {
                bytes: 9,
                sha256: "a".repeat(64),
            },
        }
    }
    #[test]
    fn visibility_precedes_durability_and_cancel_never_erases_it() {
        let mut p = Publication::prepared(evidence()).unwrap();
        assert_eq!(p.apply(Input::BeginLink), Ok(Effect::LinkCreateOnly));
        assert_eq!(p.apply(Input::LinkSucceeded), Ok(Effect::SyncDirectory));
        assert_eq!(p.phase(), Phase::LinkedUnconfirmed);
        p.apply(Input::DirectorySyncFailed).unwrap();
        p.apply(Input::Cancel).unwrap();
        assert_eq!(p.phase(), Phase::LinkedUnconfirmed);
        assert!(p.apply(Input::BeginLink).is_err());
        p.apply(Input::DirectorySynced).unwrap();
        p.apply(Input::Cancel).unwrap();
        assert_eq!(p.phase(), Phase::Durable);
    }
    #[test]
    fn unknown_outcome_requires_exact_reconciliation_not_absence_or_retry() {
        let mut p = Publication::prepared(evidence()).unwrap();
        p.apply(Input::BeginLink).unwrap();
        p.apply(Input::LinkOutcomeUnknown).unwrap();
        assert!(p.apply(Input::BeginLink).is_err());
        assert!(p.apply(Input::DirectorySynced).is_err());
        let mut wrong = evidence();
        wrong.file_inode += 1;
        assert_eq!(
            p.apply(Input::ReconciledExact(wrong)),
            Err(Rejection::Mismatch)
        );
        assert_eq!(p.phase(), Phase::OutcomeUnknown);
        assert_eq!(
            p.apply(Input::ReconciledExact(evidence())),
            Ok(Effect::SyncDirectory)
        );
        p.apply(Input::DirectorySynced).unwrap();
        assert_eq!(p.phase(), Phase::Durable);
    }
    #[test]
    fn collision_and_prelink_cancel_never_issue_second_link() {
        for cancel in [true, false] {
            let mut p = Publication::prepared(evidence()).unwrap();
            if cancel {
                p.apply(Input::Cancel).unwrap();
            } else {
                p.apply(Input::BeginLink).unwrap();
                p.apply(Input::DestinationAlreadyExists).unwrap();
            }
            assert!(p.apply(Input::BeginLink).is_err());
            assert!(p.apply(Input::DirectorySynced).is_err());
            assert_eq!(p.evidence(), &evidence());
        }
    }
    #[test]
    fn complete_finite_phase_graph_never_relinks_or_loses_visibility_evidence() {
        use std::collections::{BTreeSet, VecDeque};
        let initial = Publication::prepared(evidence()).unwrap();
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([(initial, 0u8)]);
        while let Some((state, links)) = queue.pop_front() {
            if !seen.insert((serde_json::to_string(&state).unwrap(), links)) {
                continue;
            }
            let mut wrong = evidence();
            wrong.artifact.sha256 = "b".repeat(64);
            for input in [
                Input::BeginLink,
                Input::Cancel,
                Input::LinkSucceeded,
                Input::DestinationAlreadyExists,
                Input::LinkOutcomeUnknown,
                Input::DirectorySynced,
                Input::DirectorySyncFailed,
                Input::ReconciledExact(evidence()),
                Input::ReconciledExact(wrong),
            ] {
                let mut next = state.clone();
                let transition = next.apply(input.clone());
                let next_links = links + u8::from(transition == Ok(Effect::LinkCreateOnly));
                assert!(next_links <= 1);
                assert_eq!(next.evidence(), &evidence());
                if transition.is_err() {
                    assert_eq!(next, state);
                    continue;
                }
                if next.phase() == Phase::Durable && state.phase() != Phase::Durable {
                    assert_eq!(state.phase(), Phase::LinkedUnconfirmed);
                    assert_eq!(input, Input::DirectorySynced);
                }
                if matches!(
                    state.phase(),
                    Phase::LinkAttempted
                        | Phase::LinkedUnconfirmed
                        | Phase::OutcomeUnknown
                        | Phase::Durable
                ) {
                    assert!(!matches!(next.phase(), Phase::Ready | Phase::Abandoned));
                }
                queue.push_back((next, next_links));
            }
        }
        assert_eq!(seen.len(), 7, "all finite publication phases explored");
    }
    #[test]
    fn invalid_target_and_identity_are_rejected_before_effect() {
        for name in ["", ".", "..", "../pair", "a/b", "a\\0b"] {
            let mut e = evidence();
            e.name = name.into();
            assert!(Publication::prepared(e).is_err());
        }
        let mut e = evidence();
        e.file_device = 9;
        assert!(Publication::prepared(e).is_err());
    }
}
