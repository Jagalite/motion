//! Scan demands and the physical attempts that satisfy them.
//!
//! A demand belongs to a requester and captures a freshness barrier: the
//! source's monotonic scan barrier at request time (requests and filesystem
//! change hints advance it). An attempt records the barrier when its traversal
//! starts. An attempt satisfies a demand only if it started at or after the
//! demand's barrier and its mode is at least as strong, so a scan never
//! reports an older observation as fresh. One attempt may satisfy many
//! demands; cancelling one demand never cancels an attempt another live
//! demand still needs. Per source there is at most one running attempt and at
//! most one queued follow-up.
use crate::jobs::Phase;
use serde::{Deserialize, Serialize};

pub type Id = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemandStatus {
    Pending,
    Complete,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Demand {
    pub id: Id,
    pub barrier: u64,
    pub verify: bool,
    pub require_complete: bool,
    pub status: DemandStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Attempt {
    pub job: Id,
    pub phase: Phase,
    pub verify: bool,
    /// Barrier observed when traversal started; `None` while queued.
    pub started_barrier: Option<u64>,
    /// Admitted by a requester outside the demand ledger (v1 API, schedule,
    /// configured root). No demand cancellation may stop it.
    pub direct: bool,
}

fn active(a: &Attempt) -> bool {
    matches!(a.phase, Phase::Queued | Phase::Running | Phase::Cancelling)
}

/// Can this attempt (still) satisfy this demand?
pub fn can_satisfy(attempt: &Attempt, demand: &Demand) -> bool {
    let mode = attempt.verify || !demand.verify;
    match (attempt.phase, attempt.started_barrier) {
        // A queued attempt has not started; it will observe a newer barrier.
        (Phase::Queued, _) => mode,
        (Phase::Running, Some(start)) | (Phase::Completed, Some(start)) => {
            mode && start >= demand.barrier
        }
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admission {
    /// An active attempt will satisfy the demand.
    Join { job: Id },
    /// The queued attempt is incremental; upgrade it to verify and join.
    UpgradeQueued { job: Id },
    /// No attempt can satisfy it; enqueue a new one (follow-up if one runs).
    Enqueue,
}

pub fn admit(demand: &Demand, attempts: &[Attempt]) -> Admission {
    if let Some(a) = attempts
        .iter()
        .filter(|a| active(a))
        .find(|a| can_satisfy(a, demand))
    {
        return Admission::Join { job: a.job.clone() };
    }
    match attempts.iter().find(|a| a.phase == Phase::Queued) {
        Some(queued) => Admission::UpgradeQueued {
            job: queued.job.clone(),
        },
        None => Admission::Enqueue,
    }
}

/// Outcome of a finished attempt for the demands it can satisfy. `outcome`
/// is `Some(true)` complete, `Some(false)` partial, `None` failed/cancelled.
/// Demands it cannot satisfy stay pending for a follow-up.
pub fn resolve(
    attempt: &Attempt,
    outcome: Option<bool>,
    demands: &[Demand],
) -> Vec<(Id, DemandStatus)> {
    let finished = Attempt {
        phase: Phase::Completed,
        ..attempt.clone()
    };
    demands
        .iter()
        .filter(|d| d.status == DemandStatus::Pending)
        .filter(|d| match outcome {
            // A failed attempt answers every demand it was serving.
            None => {
                attempt.started_barrier.is_some_and(|s| s >= d.barrier)
                    && (attempt.verify || !d.verify)
            }
            Some(_) => can_satisfy(&finished, d),
        })
        .map(|d| {
            let status = match outcome {
                Some(true) => DemandStatus::Complete,
                // Partial coverage is reported as such; a demand that requires
                // completeness can be retried explicitly.
                Some(false) => DemandStatus::Partial,
                None => DemandStatus::Failed,
            };
            (d.id.clone(), status)
        })
        .collect()
}

/// Cancel one demand. The attempt it relied on is cancelled only if no other
/// pending demand can still be satisfied by it.
pub fn cancel(demand: &Demand, others: &[Demand], attempts: &[Attempt]) -> (DemandStatus, Vec<Id>) {
    if demand.status != DemandStatus::Pending {
        return (demand.status, vec![]);
    }
    let still_needed = |a: &Attempt| {
        others
            .iter()
            .filter(|d| d.id != demand.id && d.status == DemandStatus::Pending)
            .any(|d| can_satisfy(a, d))
    };
    let stop = attempts
        .iter()
        .filter(|a| active(a) && a.phase != Phase::Cancelling && !a.direct)
        .filter(|a| can_satisfy(a, demand) && !still_needed(a))
        .map(|a| a.job.clone())
        .collect();
    (DemandStatus::Cancelled, stop)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowUp {
    Nothing,
    Enqueue { verify: bool },
    UpgradeQueued { job: Id },
}

/// After admission, an attempt finishing, or recovery: pending demands that
/// no active attempt can satisfy need a new attempt strong enough for all of
/// them, or the queued attempt upgraded to verify.
pub fn follow_up(demands: &[Demand], attempts: &[Attempt]) -> FollowUp {
    let unserved: Vec<&Demand> = demands
        .iter()
        .filter(|d| d.status == DemandStatus::Pending)
        .filter(|d| {
            !attempts
                .iter()
                .filter(|a| active(a))
                .any(|a| can_satisfy(a, d))
        })
        .collect();
    if unserved.is_empty() {
        return FollowUp::Nothing;
    }
    match attempts.iter().find(|a| a.phase == Phase::Queued) {
        Some(queued) => FollowUp::UpgradeQueued {
            job: queued.job.clone(),
        },
        None => FollowUp::Enqueue {
            verify: unserved.iter().any(|d| d.verify),
        },
    }
}

/// Status of a request from its per-source demands: pending while any is;
/// otherwise cancelled only if all were cancelled; failed if any failed;
/// partial if any is partial; else complete.
pub fn request_status(statuses: &[DemandStatus]) -> DemandStatus {
    use DemandStatus::*;
    if statuses.contains(&Pending) {
        Pending
    } else if !statuses.is_empty() && statuses.iter().all(|s| *s == Cancelled) {
        Cancelled
    } else if statuses.contains(&Failed) {
        Failed
    } else if statuses.contains(&Partial) {
        Partial
    } else if statuses.is_empty() {
        Pending
    } else {
        Complete
    }
}
