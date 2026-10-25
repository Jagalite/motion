//! Durable publication disposition for Motion-rendered native outputs.
//!
//! Reservation is committed with the exact command intent before dispatch. It
//! is the cancellation linearization point, not proof that native effects ran.
//! Adapters retain the reservation on absent/unknown receipts and never derive
//! a fresh dispatch or completion from that absence. Native receipt validation,
//! projection authority and the FK-safe application transaction remain adapters.
use crate::{
    jobs::Effect,
    processing::{self, Completion, PreparationFacts, State},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub catalog_id: String,
    pub incarnation: String,
    pub operation_id: String,
    pub payload_digest: String,
    pub output_file_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedReceipt {
    pub identity: Identity,
    /// Digest of the actual full native receipt, after native owner validation.
    pub receipt_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    identity: Identity,
    before: State,
    completion: Completion,
    facts: PreparationFacts,
    receipt: Option<CommittedReceipt>,
    acknowledged: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationDisposition {
    AlreadyCommitting,
    AlreadyCommitted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Receipt lookup only. This is deliberately not a dispatch permission.
    ReconcileOriginalReceipt,
    ProjectAndAcknowledge,
    Complete,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
impl Reservation {
    pub fn reserve(
        identity: Identity,
        before: State,
        completion: Completion,
        facts: PreparationFacts,
    ) -> Result<Self, &'static str> {
        let reservation = Self {
            identity,
            before,
            completion,
            facts,
            receipt: None,
            acknowledged: false,
        };
        reservation.validate()?;
        Ok(reservation)
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    fn proposed(&self) -> Result<State, &'static str> {
        self.facts.admit()?;
        let (next, effects) =
            processing::finish_preparation(&self.before, &self.completion, Some(&self.facts));
        if effects
            != [Effect::Publish {
                attempt: self.completion.attempt,
            }]
            || next.published_output.as_deref() != Some(self.identity.output_file_id.as_str())
        {
            return Err("publication_not_admitted");
        }
        Ok(next)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.identity.catalog_id.is_empty()
            || self.identity.incarnation.is_empty()
            || self.identity.operation_id.is_empty()
            || self.identity.output_file_id.is_empty()
            || !digest(&self.identity.payload_digest)
        {
            return Err("publication_identity_invalid");
        }
        self.proposed()?;
        if let Some(receipt) = &self.receipt {
            if receipt.identity != self.identity || !digest(&receipt.receipt_digest) {
                return Err("publication_receipt_mismatch");
            }
        } else if self.acknowledged {
            return Err("publication_receipt_required");
        }
        Ok(())
    }
    /// A late cancellation must return this truthful disposition; it cannot
    /// rewrite the attempt to cancelled while its reserved effect may commit.
    pub fn cancellation(&self) -> Result<CancellationDisposition, &'static str> {
        self.validate()?;
        Ok(if self.receipt.is_some() {
            CancellationDisposition::AlreadyCommitted
        } else {
            CancellationDisposition::AlreadyCommitting
        })
    }
    pub fn recovery(&self) -> Result<RecoveryAction, &'static str> {
        self.validate()?;
        Ok(if self.acknowledged {
            RecoveryAction::Complete
        } else if self.receipt.is_some() {
            RecoveryAction::ProjectAndAcknowledge
        } else {
            RecoveryAction::ReconcileOriginalReceipt
        })
    }
    /// Fresh authority may authorize only retry of this exact saved intent.
    /// Receipt absence is deliberately not an input or evidence of no effect.
    /// The adapter must retain the independently checked output descriptor and
    /// current source/authorization custody through native receipt-first dispatch.
    pub fn authorize_exact_retry(
        &self,
        identity: &Identity,
        current: &State,
        completion: &Completion,
        facts: &PreparationFacts,
    ) -> Result<(), &'static str> {
        self.validate()?;
        if self.receipt.is_some() || self.acknowledged {
            return Err("publication_receipt_already_committed");
        }
        if identity != &self.identity || current != &self.before || completion != &self.completion {
            return Err("publication_retry_identity_changed");
        }
        facts.admit()?;
        let (_, effects) = processing::finish_preparation(current, completion, Some(facts));
        if effects
            != [Effect::Publish {
                attempt: completion.attempt,
            }]
        {
            return Err("publication_retry_not_admitted");
        }
        Ok(())
    }
    pub fn record_receipt(&self, receipt: CommittedReceipt) -> Result<Self, &'static str> {
        self.validate()?;
        if receipt.identity != self.identity
            || !digest(&receipt.receipt_digest)
            || self.receipt.as_ref().is_some_and(|old| old != &receipt)
        {
            return Err("publication_receipt_mismatch");
        }
        let mut next = self.clone();
        next.receipt = Some(receipt);
        Ok(next)
    }
    /// Called only after exact native projection and application FK facts have
    /// been established in the same Motion writer that persists this result.
    /// No native publication effect is returned: that effect already committed.
    pub fn acknowledge(&self, current: &State) -> Result<(Self, State), &'static str> {
        self.validate()?;
        if self.receipt.is_none() {
            return Err("publication_receipt_required");
        }
        let completed = self.proposed()?;
        let expected = if self.acknowledged {
            &completed
        } else {
            &self.before
        };
        if current != expected {
            return Err("publication_attempt_changed");
        }
        let mut next = self.clone();
        next.acknowledged = true;
        Ok((next, completed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{Input, Job, Phase};
    fn before() -> State {
        State {
            job: Job {
                phase: Phase::Running,
                attempt: 3,
            },
            source_revision: "source".into(),
            published_output: None,
        }
    }
    fn identity() -> Identity {
        Identity {
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            operation_id: "operation".into(),
            payload_digest: "a".repeat(64),
            output_file_id: "output".into(),
        }
    }
    fn completion() -> Completion {
        Completion {
            attempt: 3,
            current_source_revision: Some("source".into()),
            validated_output: Some("output".into()),
        }
    }
    fn facts() -> PreparationFacts {
        PreparationFacts {
            requester_authorized: true,
            source_enabled: true,
            source_in_scope: true,
            exact_whole_timeline_binding: true,
            recipe_unchanged: true,
        }
    }
    fn reserved() -> Reservation {
        Reservation::reserve(identity(), before(), completion(), facts()).unwrap()
    }
    fn receipt() -> CommittedReceipt {
        CommittedReceipt {
            identity: identity(),
            receipt_digest: "b".repeat(64),
        }
    }
    #[test]
    fn cancellation_race_has_one_truthful_linearization() {
        let mut cancelled = before();
        cancelled.job = crate::jobs::transition(&cancelled.job, Input::Cancel).0;
        assert!(Reservation::reserve(identity(), cancelled, completion(), facts()).is_err());
        let pending = reserved();
        assert_eq!(
            pending.cancellation().unwrap(),
            CancellationDisposition::AlreadyCommitting
        );
        let recorded = pending.record_receipt(receipt()).unwrap();
        assert_eq!(
            recorded.cancellation().unwrap(),
            CancellationDisposition::AlreadyCommitted
        );
        assert_eq!(
            recorded
                .acknowledge(&before())
                .unwrap()
                .1
                .published_output
                .as_deref(),
            Some("output")
        );
    }
    #[test]
    fn missing_receipt_restart_never_dispatches_or_completes() {
        let restored: Reservation =
            serde_json::from_str(&serde_json::to_string(&reserved()).unwrap()).unwrap();
        for _ in 0..3 {
            assert_eq!(
                restored.recovery().unwrap(),
                RecoveryAction::ReconcileOriginalReceipt
            );
            assert_eq!(
                restored.acknowledge(&before()),
                Err("publication_receipt_required")
            );
        }
    }
    #[test]
    fn exact_receipt_and_ack_replay_without_second_publication() {
        let pending = reserved();
        let mut wrong = receipt();
        wrong.identity.payload_digest = "c".repeat(64);
        assert!(pending.record_receipt(wrong).is_err());
        let recorded = pending.record_receipt(receipt()).unwrap();
        assert_eq!(recorded.record_receipt(receipt()).unwrap(), recorded);
        assert_eq!(
            recorded.recovery().unwrap(),
            RecoveryAction::ProjectAndAcknowledge
        );
        let (done, completed) = recorded.acknowledge(&before()).unwrap();
        assert_eq!(completed.job.phase, Phase::Completed);
        assert_eq!(
            done.acknowledge(&completed).unwrap(),
            (done.clone(), completed)
        );
        assert_eq!(done.recovery().unwrap(), RecoveryAction::Complete);
        assert!(done.acknowledge(&before()).is_err());
        let mut superseded = before();
        superseded.job.attempt += 1;
        assert!(recorded.acknowledge(&superseded).is_err());
    }
    #[test]
    fn retry_needs_fresh_facts_and_exact_saved_identity_not_absence() {
        let reservation = reserved();
        reservation
            .authorize_exact_retry(&identity(), &before(), &completion(), &facts())
            .unwrap();
        let mut denied = facts();
        denied.requester_authorized = false;
        assert!(
            reservation
                .authorize_exact_retry(&identity(), &before(), &completion(), &denied)
                .is_err()
        );
        let mut changed = identity();
        changed.operation_id = "replacement".into();
        assert!(
            reservation
                .authorize_exact_retry(&changed, &before(), &completion(), &facts())
                .is_err()
        );
        let mut changed = before();
        changed.job.attempt += 1;
        assert!(
            reservation
                .authorize_exact_retry(&identity(), &changed, &completion(), &facts())
                .is_err()
        );
        let mut changed = completion();
        changed.validated_output = Some("different".into());
        assert!(
            reservation
                .authorize_exact_retry(&identity(), &before(), &changed, &facts())
                .is_err()
        );
        assert!(
            reservation
                .record_receipt(receipt())
                .unwrap()
                .authorize_exact_retry(&identity(), &before(), &completion(), &facts())
                .is_err()
        );
    }
}
