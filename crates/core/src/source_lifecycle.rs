//! Explicit source withdrawal, distinct from scan enablement and physical loss.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Active,
    Withdrawn,
    Tombstoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleProof {
    pub source_id: String,
    pub epoch: i64,
    pub catalog_id: String,
    pub incarnation: String,
    pub mapping_digest: String,
    pub source_revision: i64,
    pub binding_revision: i64,
    pub state: State,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WithdrawalFacts {
    pub logical_memberships: u64,
    pub active_demands: u64,
    pub active_processing: u64,
    pub reserved_publications: u64,
    pub pending_discovery_effects: u64,
    pub native_ownership_unresolved: bool,
    pub private_generated_source: bool,
    pub retained_files: u64,
    pub historical_demands: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    RevisionConflict,
    NotFound,
    Busy,
    PrivateSource,
    Exhausted,
}

pub fn withdraw(
    current: &LifecycleProof,
    expected_revision: i64,
    facts: WithdrawalFacts,
) -> Result<LifecycleProof, Refusal> {
    if current.state == State::Tombstoned {
        return Err(Refusal::NotFound);
    }
    if current.source_revision != expected_revision {
        return Err(Refusal::RevisionConflict);
    }
    if facts.private_generated_source {
        return Err(Refusal::PrivateSource);
    }
    if facts.logical_memberships != 0
        || facts.active_demands != 0
        || facts.active_processing != 0
        || facts.reserved_publications != 0
        || facts.pending_discovery_effects != 0
        || facts.native_ownership_unresolved
    {
        return Err(Refusal::Busy);
    }
    let mut next = current.clone();
    next.epoch = next.epoch.checked_add(1).ok_or(Refusal::Exhausted)?;
    next.source_revision = next
        .source_revision
        .checked_add(1)
        .ok_or(Refusal::Exhausted)?;
    next.state = if facts.retained_files == 0 && facts.historical_demands == 0 {
        State::Tombstoned
    } else {
        State::Withdrawn
    };
    Ok(next)
}

/// Decision inputs, not native receipt or filesystem authority. The retained
/// adapter must independently establish the committed effect and current owner
/// inventory before applying this decision in the same Motion writer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenewalFacts {
    pub active_demands: u64,
    pub active_processing: u64,
    pub reserved_publications: u64,
    pub pending_discovery_effects: u64,
    pub native_ownership_unresolved: bool,
    pub private_generated_source: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenewalRefusal {
    PredecessorConflict,
    Inactive,
    PrivateSource,
    Busy,
    MappingUnchanged,
    Exhausted,
}

/// A distinct active-to-active transition. Compare the entire saved predecessor,
/// not merely a public revision: old custody and generated scope tokens must
/// cease to match even when a caller supplies a stale mapping at a newer epoch.
/// This does not admit a command or authenticate the supplied mapping digest.
pub fn renew_mapping(
    current: &LifecycleProof,
    expected: &LifecycleProof,
    new_mapping_digest: &str,
    facts: RenewalFacts,
) -> Result<LifecycleProof, RenewalRefusal> {
    if current != expected {
        return Err(RenewalRefusal::PredecessorConflict);
    }
    if current.state != State::Active {
        return Err(RenewalRefusal::Inactive);
    }
    if facts.private_generated_source {
        return Err(RenewalRefusal::PrivateSource);
    }
    if facts.active_demands != 0
        || facts.active_processing != 0
        || facts.reserved_publications != 0
        || facts.pending_discovery_effects != 0
        || facts.native_ownership_unresolved
    {
        return Err(RenewalRefusal::Busy);
    }
    if new_mapping_digest == current.mapping_digest {
        return Err(RenewalRefusal::MappingUnchanged);
    }
    let mut next = current.clone();
    next.epoch = next.epoch.checked_add(1).ok_or(RenewalRefusal::Exhausted)?;
    next.source_revision = next
        .source_revision
        .checked_add(1)
        .ok_or(RenewalRefusal::Exhausted)?;
    next.binding_revision = next
        .binding_revision
        .checked_add(1)
        .ok_or(RenewalRefusal::Exhausted)?;
    next.mapping_digest = new_mapping_digest.to_owned();
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn active() -> LifecycleProof {
        LifecycleProof {
            source_id: "source".into(),
            epoch: 0,
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            mapping_digest: "a".repeat(64),
            source_revision: 19,
            binding_revision: 7,
            state: State::Active,
        }
    }
    #[test]
    fn withdrawal_preserves_identity_and_history() {
        let current = active();
        let next = withdraw(
            &current,
            19,
            WithdrawalFacts {
                retained_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(next.state, State::Withdrawn);
        assert_eq!(next.source_revision, 20);
        assert_eq!(next.binding_revision, 7);
        assert_eq!(next.mapping_digest, current.mapping_digest);
        assert_eq!(
            withdraw(&next, 19, WithdrawalFacts::default()),
            Err(Refusal::RevisionConflict)
        );
        assert_eq!(
            withdraw(
                &next,
                20,
                WithdrawalFacts {
                    historical_demands: 1,
                    ..Default::default()
                }
            )
            .unwrap()
            .state,
            State::Withdrawn
        );
    }
    #[test]
    fn every_actual_owner_blocks_withdrawal() {
        for facts in [
            WithdrawalFacts {
                logical_memberships: 1,
                ..Default::default()
            },
            WithdrawalFacts {
                active_demands: 1,
                ..Default::default()
            },
            WithdrawalFacts {
                active_processing: 1,
                ..Default::default()
            },
            WithdrawalFacts {
                reserved_publications: 1,
                ..Default::default()
            },
            WithdrawalFacts {
                pending_discovery_effects: 1,
                ..Default::default()
            },
            WithdrawalFacts {
                native_ownership_unresolved: true,
                ..Default::default()
            },
        ] {
            assert_eq!(withdraw(&active(), 19, facts), Err(Refusal::Busy));
        }
        let dead = withdraw(&active(), 19, WithdrawalFacts::default()).unwrap();
        assert_eq!(dead.state, State::Tombstoned);
        assert_eq!(
            withdraw(&dead, 20, WithdrawalFacts::default()),
            Err(Refusal::NotFound)
        );
    }
    #[test]
    fn relocation_renewal_invalidates_full_predecessor_without_reactivation() {
        let first = active();
        let second =
            renew_mapping(&first, &first, &"b".repeat(64), RenewalFacts::default()).unwrap();
        assert_eq!(
            (
                second.epoch,
                second.source_revision,
                second.binding_revision
            ),
            (1, 20, 8)
        );
        assert_eq!(second.state, State::Active);
        assert_eq!(second.source_id, first.source_id);
        let third =
            renew_mapping(&second, &second, &"c".repeat(64), RenewalFacts::default()).unwrap();
        assert_eq!(
            (third.epoch, third.source_revision, third.binding_revision),
            (2, 21, 9)
        );
        assert_eq!(
            renew_mapping(&third, &first, &"b".repeat(64), RenewalFacts::default()),
            Err(RenewalRefusal::PredecessorConflict)
        );
        let withdrawn = withdraw(
            &second,
            20,
            WithdrawalFacts {
                retained_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            renew_mapping(
                &withdrawn,
                &second,
                &"c".repeat(64),
                RenewalFacts::default()
            ),
            Err(RenewalRefusal::PredecessorConflict)
        );
        assert_eq!(
            renew_mapping(
                &withdrawn,
                &withdrawn,
                &"c".repeat(64),
                RenewalFacts::default()
            ),
            Err(RenewalRefusal::Inactive)
        );
        assert_eq!(
            withdraw(&second, first.source_revision, WithdrawalFacts::default()),
            Err(Refusal::RevisionConflict)
        );
    }
    #[test]
    fn relocation_renewal_checks_each_owner_and_revision_overflow() {
        let first = active();
        for facts in [
            RenewalFacts {
                active_demands: 1,
                ..Default::default()
            },
            RenewalFacts {
                active_processing: 1,
                ..Default::default()
            },
            RenewalFacts {
                reserved_publications: 1,
                ..Default::default()
            },
            RenewalFacts {
                pending_discovery_effects: 1,
                ..Default::default()
            },
            RenewalFacts {
                native_ownership_unresolved: true,
                ..Default::default()
            },
        ] {
            assert_eq!(
                renew_mapping(&first, &first, &"b".repeat(64), facts),
                Err(RenewalRefusal::Busy)
            );
        }
        for field in 0..3 {
            let mut exhausted = first.clone();
            match field {
                0 => exhausted.epoch = i64::MAX,
                1 => exhausted.source_revision = i64::MAX,
                _ => exhausted.binding_revision = i64::MAX,
            }
            assert_eq!(
                renew_mapping(
                    &exhausted,
                    &exhausted,
                    &"b".repeat(64),
                    RenewalFacts::default()
                ),
                Err(RenewalRefusal::Exhausted)
            );
        }
        assert_eq!(
            renew_mapping(
                &first,
                &first,
                &first.mapping_digest,
                RenewalFacts::default()
            ),
            Err(RenewalRefusal::MappingUnchanged)
        );
    }
}
