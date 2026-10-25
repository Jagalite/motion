//! Native scan completion includes discovery publication, not inventory alone.
//! An unacknowledged cancellation signal is interruption, never durable cancel.
use crate::demands::{self, Attempt, Demand, DemandStatus};
use crate::jobs::Phase;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Failed,
    Interrupted,
}

pub fn finish(demand: &Demand, attempt: &Attempt, outcome: Outcome) -> (DemandStatus, Phase) {
    if demand.status == DemandStatus::Cancelled {
        return (DemandStatus::Cancelled, Phase::Cancelled);
    }
    if demand.status != DemandStatus::Pending {
        return (demand.status, attempt.phase);
    }
    if outcome == Outcome::Interrupted {
        return (DemandStatus::Pending, Phase::Queued);
    }
    let result = demands::resolve(
        attempt,
        match outcome {
            Outcome::Complete => Some(true),
            _ => None,
        },
        std::slice::from_ref(demand),
    );
    match result.first().map(|(_, status)| *status) {
        Some(DemandStatus::Complete) => (DemandStatus::Complete, Phase::Completed),
        Some(status) => (status, Phase::Failed),
        None => (DemandStatus::Pending, Phase::Queued),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Demand, Attempt) {
        (
            Demand {
                id: "d".into(),
                barrier: 4,
                verify: true,
                require_complete: true,
                status: DemandStatus::Pending,
            },
            Attempt {
                job: "a".into(),
                phase: Phase::Running,
                verify: true,
                started_barrier: Some(4),
                direct: false,
            },
        )
    }
    #[test]
    fn interruption_is_not_durable_cancellation() {
        let (mut d, a) = fixture();
        assert_eq!(
            finish(&d, &a, Outcome::Interrupted),
            (DemandStatus::Pending, Phase::Queued)
        );
        d.status = DemandStatus::Cancelled;
        assert_eq!(
            finish(&d, &a, Outcome::Complete),
            (DemandStatus::Cancelled, Phase::Cancelled)
        );
    }
    #[test]
    fn inventory_older_than_acceptance_cannot_finish_demand() {
        let (d, mut a) = fixture();
        a.started_barrier = Some(3);
        assert_eq!(
            finish(&d, &a, Outcome::Complete),
            (DemandStatus::Pending, Phase::Queued)
        );
        a.started_barrier = Some(4);
        assert_eq!(
            finish(&d, &a, Outcome::Complete),
            (DemandStatus::Complete, Phase::Completed)
        );
        assert_eq!(
            finish(&d, &a, Outcome::Failed),
            (DemandStatus::Failed, Phase::Failed)
        );
    }
}

/// Captured execution authority is narrower than the request's library scope.
/// An attempt cannot substitute another accepted source or a later retry.
pub const ACCEPTED_SERVICE_POLICY: &str = "accepted_bounded_service_v1";

/// Acceptance transfers only this fixed source demand to the service. Later
/// principal/library changes cannot widen it. Current permissions remain needed
/// for new requests, reads and cancellation; explicit durable cancel revokes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub policy_version: u32,
    pub request_id: String,
    pub demand_id: String,
    pub source_id: String,
    pub source_digest: String,
    pub attempt: u32,
}
pub fn admits_effect(
    accepted: &ExecutionIdentity,
    current: &ExecutionIdentity,
    status: DemandStatus,
    phase: Phase,
) -> bool {
    accepted == current
        && accepted.policy_version == 1
        && accepted.attempt > 0
        && !accepted.request_id.is_empty()
        && !accepted.demand_id.is_empty()
        && !accepted.source_id.is_empty()
        && !accepted.source_digest.is_empty()
        && status == DemandStatus::Pending
        && phase == Phase::Running
}
#[cfg(test)]
mod execution_tests {
    use super::*;
    #[test]
    fn cancellation_and_every_identity_change_revoke_effects() {
        let a = ExecutionIdentity {
            policy_version: 1,
            request_id: "r".into(),
            demand_id: "d".into(),
            source_id: "s".into(),
            source_digest: "proof".into(),
            attempt: 1,
        };
        assert!(admits_effect(&a, &a, DemandStatus::Pending, Phase::Running));
        for status in [
            DemandStatus::Cancelled,
            DemandStatus::Failed,
            DemandStatus::Complete,
        ] {
            assert!(!admits_effect(&a, &a, status, Phase::Running));
        }
        for phase in [Phase::Queued, Phase::Cancelling, Phase::Completed] {
            assert!(!admits_effect(&a, &a, DemandStatus::Pending, phase));
        }
        for field in 0..6 {
            let mut b = a.clone();
            match field {
                0 => b.request_id.push('x'),
                1 => b.demand_id.push('x'),
                2 => b.source_id.push('x'),
                3 => b.source_digest.push('x'),
                4 => b.attempt += 1,
                _ => b.policy_version += 1,
            };
            assert!(!admits_effect(
                &a,
                &b,
                DemandStatus::Pending,
                Phase::Running
            ));
        }
    }
}

/// An authenticated cancellation waiter temporarily inhibits new claims without
/// fabricating a durable cancellation. Caller loss releases only this inhibition.
pub fn admits_claim(status: DemandStatus, phase: Phase, cancellation_custody: bool) -> bool {
    status == DemandStatus::Pending && phase == Phase::Queued && !cancellation_custody
}

#[cfg(test)]
mod cancellation_custody_tests {
    use super::*;
    #[test]
    fn interruption_custody_and_durable_cancellation_are_distinct() {
        assert!(!admits_claim(DemandStatus::Pending, Phase::Queued, true));
        assert!(admits_claim(DemandStatus::Pending, Phase::Queued, false));
        assert!(!admits_claim(
            DemandStatus::Cancelled,
            Phase::Cancelled,
            false
        ));
        assert!(!admits_claim(DemandStatus::Pending, Phase::Running, false));
    }
}
