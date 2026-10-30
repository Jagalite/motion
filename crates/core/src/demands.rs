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
use crate::jobs::{self, Job, Phase};
use serde::{Deserialize, Serialize};

pub type Id = String;

/// A source membership observed under the request's writer transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestMember {
    pub source: Id,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestEffect {
    Admit { source: Id },
    FailDisabled { source: Id },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestError {
    NoSources,
    UnknownSource(Id),
}

/// Select membership and decide the per-source demand outcome before any
/// request is persisted. Explicit selections preserve their order and repeats;
/// API-specific shape and size limits belong at the transport boundary.
pub fn initialize_request(
    members: &[RequestMember],
    selected: Option<&[Id]>,
) -> Result<Vec<RequestEffect>, RequestError> {
    let chosen: Vec<&RequestMember> = match selected {
        None => members.iter().collect(),
        Some(ids) => ids
            .iter()
            .map(|id| {
                members
                    .iter()
                    .find(|m| &m.source == id)
                    .ok_or_else(|| RequestError::UnknownSource(id.clone()))
            })
            .collect::<Result<_, _>>()?,
    };
    if chosen.is_empty() {
        return Err(RequestError::NoSources);
    }
    Ok(chosen
        .into_iter()
        .map(|m| {
            if m.enabled {
                RequestEffect::Admit {
                    source: m.source.clone(),
                }
            } else {
                RequestEffect::FailDisabled {
                    source: m.source.clone(),
                }
            }
        })
        .collect())
}

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

/// Ordered writes for recovery of one source. Apply the entire plan and
/// reconcile pending demands in the same reserved writer transaction. A failed
/// write rolls back all effects; no worker may start before recovery commits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryEffect {
    /// Keep the existing queued job's identity, attempt counter and barrier,
    /// while inheriting the interrupted request's mode and direct ownership.
    MergeQueued { job: Id, verify: bool, direct: bool },
    /// Set the interrupted job's phase and clear its started barrier. Its
    /// attempt counter and all other history remain unchanged.
    ResetInterrupted { job: Id, phase: Phase },
}

/// Recover attempts observed for one source under its writer boundary.
///
/// The snapshot has at most one running/cancelling job and one queued job
/// (enforced by the adapter's unique constraints); each Attempt phase agrees
/// with its Job. Only a retryable running job donates work to a queued
/// follow-up. Cancellation and attempt exhaustion retain their normal job
/// outcomes. Recovery itself never answers a demand: reconciliation uses the
/// resulting attempts and the unchanged pending demands via [`follow_up`].
pub fn recover(attempts: &[(Attempt, Job)]) -> Vec<RecoveryEffect> {
    let mut effects = Vec::new();
    for (attempt, job) in attempts {
        if !matches!(job.phase, Phase::Running | Phase::Cancelling) {
            continue;
        }
        let (mut recovered, _) = jobs::transition(job, jobs::Input::Recover);
        if recovered.phase == Phase::Queued
            && let Some((queued, _)) = attempts.iter().find(|(_, j)| j.phase == Phase::Queued)
        {
            effects.push(RecoveryEffect::MergeQueued {
                job: queued.job.clone(),
                verify: queued.verify || attempt.verify,
                direct: queued.direct || attempt.direct,
            });
            recovered = jobs::transition(&recovered, jobs::Input::Cancel).0;
        }
        effects.push(RecoveryEffect::ResetInterrupted {
            job: attempt.job.clone(),
            phase: recovered.phase,
        });
    }
    effects
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

/// Settled scan requests kept per library. History beyond this is pruned
/// when a new request is admitted; pending requests are always kept.
pub const RETAINED_SCANS_PER_LIBRARY: usize = 200;

/// Which of a library's scan requests to prune. `newest_first` lists every
/// request of one library in admission order, newest first, with its
/// per-source demand statuses. The request being admitted (`admitting`) and
/// every request still pending (by [`request_status`]) are never pruned and
/// do not count toward `keep`; of the other settled ones, the newest `keep`
/// stay.
pub fn prunable_requests<I: Clone + PartialEq>(
    newest_first: &[(I, Vec<DemandStatus>)],
    keep: usize,
    admitting: &I,
) -> Vec<I> {
    newest_first
        .iter()
        .filter(|(id, statuses)| {
            id != admitting && request_status(statuses) != DemandStatus::Pending
        })
        .skip(keep)
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod retention_tests {
    use super::{DemandStatus::*, prunable_requests};

    #[test]
    fn pending_requests_are_kept_and_settled_history_is_bounded() {
        let requests = vec![
            ("new", vec![Complete]),
            ("running", vec![Pending, Complete]),
            ("older", vec![Failed]),
            ("empty", vec![]),
            ("oldest", vec![Cancelled]),
            ("ancient", vec![Partial]),
        ];
        assert_eq!(
            prunable_requests(&requests, 2, &"none"),
            vec!["oldest", "ancient"]
        );
        assert_eq!(
            prunable_requests(&requests, 0, &"none"),
            vec!["new", "older", "oldest", "ancient"]
        );
        assert!(prunable_requests(&requests, 10, &"none").is_empty());
        // The request being admitted survives even when it is already settled
        // (all its sources disabled) and sorts below the kept history.
        assert_eq!(
            prunable_requests(&requests, 1, &"oldest"),
            vec!["older", "ancient"]
        );
        assert_eq!(
            prunable_requests(&requests, 0, &"ancient"),
            vec!["new", "older", "oldest"]
        );
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn interrupted(phase: Phase, counter: u32, verify: bool, direct: bool) -> (Attempt, Job) {
        (
            Attempt {
                job: "interrupted".into(),
                phase,
                verify,
                started_barrier: Some(7),
                direct,
            },
            Job {
                phase,
                attempt: counter,
            },
        )
    }

    #[test]
    fn recovery_preserves_the_follow_up_and_only_absorbs_retryable_work() {
        for phase in [Phase::Running, Phase::Cancelling] {
            for counter in [0, 1, u32::MAX - 1, u32::MAX] {
                for verify in [false, true] {
                    for direct in [false, true] {
                        for queued_verify in [false, true] {
                            for queued_direct in [false, true] {
                                let old = interrupted(phase, counter, verify, direct);
                                let queued = (
                                    Attempt {
                                        job: "follow-up".into(),
                                        phase: Phase::Queued,
                                        verify: queued_verify,
                                        direct: queued_direct,
                                        started_barrier: None,
                                    },
                                    Job {
                                        phase: Phase::Queued,
                                        attempt: 5,
                                    },
                                );
                                let expected = if phase == Phase::Running && counter < u32::MAX {
                                    vec![
                                        RecoveryEffect::MergeQueued {
                                            job: "follow-up".into(),
                                            verify: verify || queued_verify,
                                            direct: direct || queued_direct,
                                        },
                                        RecoveryEffect::ResetInterrupted {
                                            job: "interrupted".into(),
                                            phase: Phase::Cancelled,
                                        },
                                    ]
                                } else {
                                    vec![RecoveryEffect::ResetInterrupted {
                                        job: "interrupted".into(),
                                        phase: if phase == Phase::Cancelling {
                                            Phase::Cancelled
                                        } else {
                                            Phase::Failed
                                        },
                                    }]
                                };
                                assert_eq!(recover(&[old.clone(), queued.clone()]), expected);
                                assert_eq!(
                                    recover(&[queued, old]),
                                    expected,
                                    "input order must not choose the survivor"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn recovery_without_a_follow_up_keeps_the_interrupted_identity() {
        for phase in [Phase::Running, Phase::Cancelling] {
            for counter in [1, u32::MAX] {
                assert_eq!(
                    recover(&[interrupted(phase, counter, true, true)]),
                    vec![RecoveryEffect::ResetInterrupted {
                        job: "interrupted".into(),
                        phase: match (phase, counter) {
                            (Phase::Cancelling, _) => Phase::Cancelled,
                            (_, u32::MAX) => Phase::Failed,
                            _ => Phase::Queued,
                        },
                    },]
                );
            }
        }
    }

    #[test]
    fn queued_and_terminal_jobs_need_no_recovery_writes() {
        assert!(recover(&[]).is_empty());
        for phase in [
            Phase::Queued,
            Phase::Completed,
            Phase::Failed,
            Phase::Cancelled,
        ] {
            assert!(recover(&[interrupted(phase, 2, true, true)]).is_empty());
        }
    }
}

#[cfg(test)]
mod request_initialization_tests {
    use super::*;
    #[test]
    fn selection_and_disabled_sources_have_exact_ordered_outcomes() {
        let members = vec![
            RequestMember {
                source: "a".into(),
                enabled: true,
            },
            RequestMember {
                source: "b".into(),
                enabled: false,
            },
        ];
        assert_eq!(
            initialize_request(&members, None),
            Ok(vec![
                RequestEffect::Admit { source: "a".into() },
                RequestEffect::FailDisabled { source: "b".into() }
            ])
        );
        assert_eq!(
            initialize_request(&members, Some(&["b".into(), "a".into(), "b".into()])),
            Ok(vec![
                RequestEffect::FailDisabled { source: "b".into() },
                RequestEffect::Admit { source: "a".into() },
                RequestEffect::FailDisabled { source: "b".into() }
            ])
        );
        assert_eq!(
            initialize_request(&members, Some(&["a".into(), "missing".into()])),
            Err(RequestError::UnknownSource("missing".into()))
        );
        assert_eq!(
            initialize_request(&members, Some(&[])),
            Err(RequestError::NoSources)
        );
        assert_eq!(initialize_request(&[], None), Err(RequestError::NoSources));
    }
}
