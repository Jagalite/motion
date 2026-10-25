//! Bounded controls for Motion-owned native preparations. These observations
//! order application state only; they never authenticate native receipts or
//! authorize publication, worker termination, cleanup, or fresh dispatch.
use crate::{
    jobs::{self, Input, Job},
    processing::PreparationFacts,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publication {
    /// No command was reserved in any attempt. Planned bytes may still exist
    /// and remain owned by their existing cleanup/publication records.
    Unreserved,
    /// The original command may already have applied. Receipt absence is not
    /// a no-effect witness and does not authorize another native operation.
    Reserved,
    ReceiptRecorded,
    Acknowledged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Cancel,
    Retry {
        current_revision: u64,
        reviewed_revision: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    PublicationCommitting,
    PublicationRecorded,
    OutputRecorded,
    StaleRevision,
    NotRetryable,
}

/// Advisory availability of a NEW request, not authorization or replay policy.
/// A queued retry whose acknowledgement was lost can have both hints false;
/// the original key and reviewed revision must still reach exact replay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestHints {
    pub cancel_request_available: bool,
    pub retry_request_available: bool,
}

pub fn request_hints(
    job: &Job,
    output_recorded: bool,
    publication: Publication,
    native_preparation: bool,
    caller_may_control: bool,
) -> RequestHints {
    if !native_preparation || !caller_may_control {
        return RequestHints::default();
    }
    let changes =
        |control| decide(job, output_recorded, publication, control).is_ok_and(|next| next != *job);
    RequestHints {
        cancel_request_available: changes(Control::Cancel),
        // The future request still needs the revision actually reviewed by
        // its caller and fresh preparation/native/queue validation.
        retry_request_available: changes(Control::Retry {
            current_revision: 0,
            reviewed_revision: 0,
        }),
    }
}

/// The adapter serializes these facts with command reservation under the same
/// Motion writer. Even a historical acknowledged command prevents replacement.
/// Returned phase changes are cooperative signals to the existing worker. No
/// control increments an attempt, releases custody, or produces a Run/Publish.
pub fn decide(
    job: &Job,
    output_recorded: bool,
    publication: Publication,
    control: Control,
) -> Result<Job, Refusal> {
    match publication {
        Publication::Reserved => return Err(Refusal::PublicationCommitting),
        Publication::ReceiptRecorded | Publication::Acknowledged => {
            return Err(Refusal::PublicationRecorded);
        }
        Publication::Unreserved => {}
    }
    if output_recorded {
        return Err(Refusal::OutputRecorded);
    }
    let input = match control {
        Control::Cancel => Input::Cancel,
        Control::Retry {
            current_revision,
            reviewed_revision,
        } => {
            jobs::admit_retry(job, current_revision, reviewed_revision).map_err(
                |why| match why {
                    jobs::RetryRefusal::StaleRevision => Refusal::StaleRevision,
                    jobs::RetryRefusal::NotRetryable => Refusal::NotRetryable,
                },
            )?;
            Input::Retry
        }
    };
    Ok(jobs::transition(job, input).0)
}

/// A terminal phase is not enough to requeue work. The original preparation's
/// current requester/source/recipe facts and the ordinary queue bound still
/// apply. The native adapter independently authenticates the exact captured
/// version/occurrence; these facts cannot replace that native custody.
pub fn admit_retry_work(facts: &PreparationFacts, queued: i64) -> Result<(), &'static str> {
    facts.admit()?;
    if !crate::processing::queue_admits(queued) {
        return Err("processing_limit");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Phase;

    #[test]
    fn native_control_hints_offer_only_new_cohort_transitions() {
        for phase in [
            Phase::Queued,
            Phase::Running,
            Phase::Cancelling,
            Phase::Cancelled,
            Phase::Completed,
            Phase::Failed,
        ] {
            for attempt in [0, 1, u32::MAX] {
                let job = Job { phase, attempt };
                for publication in [
                    Publication::Unreserved,
                    Publication::Reserved,
                    Publication::ReceiptRecorded,
                    Publication::Acknowledged,
                ] {
                    for output in [false, true] {
                        let admitted = publication == Publication::Unreserved && !output;
                        let hints = request_hints(&job, output, publication, true, true);
                        assert_eq!(
                            hints.cancel_request_available,
                            admitted && matches!(phase, Phase::Queued | Phase::Running)
                        );
                        assert_eq!(hints.retry_request_available, admitted && job.retryable());
                        for (cohort, permission) in [(false, false), (false, true), (true, false)] {
                            assert_eq!(
                                request_hints(&job, output, publication, cohort, permission),
                                RequestHints::default()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn native_controls_never_replace_reserved_or_recorded_effects() {
        for phase in [
            Phase::Queued,
            Phase::Running,
            Phase::Cancelling,
            Phase::Cancelled,
            Phase::Failed,
            Phase::Completed,
        ] {
            for attempt in [0, 1, u32::MAX] {
                let job = Job { phase, attempt };
                for control in [
                    Control::Cancel,
                    Control::Retry {
                        current_revision: 5,
                        reviewed_revision: 5,
                    },
                ] {
                    for publication in [
                        Publication::Reserved,
                        Publication::ReceiptRecorded,
                        Publication::Acknowledged,
                    ] {
                        assert_eq!(
                            decide(&job, false, publication, control),
                            Err(if publication == Publication::Reserved {
                                Refusal::PublicationCommitting
                            } else {
                                Refusal::PublicationRecorded
                            })
                        );
                    }
                    assert_eq!(
                        decide(&job, true, Publication::Unreserved, control),
                        Err(Refusal::OutputRecorded)
                    );
                }
            }
        }
    }

    #[test]
    fn native_controls_signal_only_existing_worker_and_preserve_attempt() {
        for (phase, next) in [
            (Phase::Queued, Phase::Cancelled),
            (Phase::Running, Phase::Cancelling),
            (Phase::Cancelling, Phase::Cancelling),
            (Phase::Cancelled, Phase::Cancelled),
            (Phase::Failed, Phase::Failed),
            (Phase::Completed, Phase::Completed),
        ] {
            let job = Job { phase, attempt: 2 };
            assert_eq!(
                decide(&job, false, Publication::Unreserved, Control::Cancel).unwrap(),
                Job {
                    phase: next,
                    attempt: 2
                }
            );
            let retry = Control::Retry {
                current_revision: 5,
                reviewed_revision: 5,
            };
            if matches!(phase, Phase::Failed | Phase::Cancelled) {
                assert_eq!(
                    decide(&job, false, Publication::Unreserved, retry).unwrap(),
                    Job {
                        phase: Phase::Queued,
                        attempt: 2
                    }
                );
            } else {
                assert_eq!(
                    decide(&job, false, Publication::Unreserved, retry),
                    Err(Refusal::NotRetryable)
                );
            }
            assert_eq!(
                decide(
                    &job,
                    false,
                    Publication::Unreserved,
                    Control::Retry {
                        current_revision: 5,
                        reviewed_revision: 4
                    }
                ),
                Err(Refusal::StaleRevision)
            );
        }
        assert_eq!(
            decide(
                &Job {
                    phase: Phase::Cancelled,
                    attempt: u32::MAX
                },
                false,
                Publication::Unreserved,
                Control::Retry {
                    current_revision: 5,
                    reviewed_revision: 5
                }
            ),
            Err(Refusal::NotRetryable)
        );
    }

    #[test]
    fn native_retry_requires_each_preparation_fact_and_queue_capacity() {
        for mask in 0u8..32 {
            let facts = PreparationFacts {
                requester_authorized: mask & 1 != 0,
                source_enabled: mask & 2 != 0,
                source_in_scope: mask & 4 != 0,
                exact_whole_timeline_binding: mask & 8 != 0,
                recipe_unchanged: mask & 16 != 0,
            };
            assert_eq!(admit_retry_work(&facts, 0).is_ok(), mask == 31);
            assert!(admit_retry_work(&facts, i64::MAX).is_err());
            if mask == 31 {
                assert!(admit_retry_work(&facts, 99).is_ok());
                assert_eq!(admit_retry_work(&facts, 100), Err("processing_limit"));
            }
        }
    }

    #[test]
    fn native_control_and_reservation_order_has_no_cancelled_publication() {
        use crate::{
            processing::{Completion, State},
            processing_publication::{Identity, Reservation},
        };
        let before = State {
            job: Job {
                phase: Phase::Running,
                attempt: 4,
            },
            source_revision: "source".into(),
            published_output: None,
        };
        let identity = Identity {
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            operation_id: "operation".into(),
            payload_digest: "a".repeat(64),
            output_file_id: "output".into(),
        };
        let completion = Completion {
            attempt: 4,
            current_source_revision: Some("source".into()),
            validated_output: Some("output".into()),
        };
        let facts = PreparationFacts {
            requester_authorized: true,
            source_enabled: true,
            source_in_scope: true,
            exact_whole_timeline_binding: true,
            recipe_unchanged: true,
        };
        // Cancel wins the writer: the actual reservation policy refuses.
        let mut cancelled = before.clone();
        cancelled.job =
            decide(&before.job, false, Publication::Unreserved, Control::Cancel).unwrap();
        assert_eq!(cancelled.job.phase, Phase::Cancelling);
        assert!(
            Reservation::reserve(
                identity.clone(),
                cancelled,
                completion.clone(),
                facts.clone()
            )
            .is_err()
        );
        // Reservation wins the writer: neither cancel nor retry can rewrite
        // that attempt, even before native dispatch or any receipt exists.
        let reservation =
            Reservation::reserve(identity, before.clone(), completion, facts).unwrap();
        assert_eq!(
            reservation.recovery().unwrap(),
            crate::processing_publication::RecoveryAction::ReconcileOriginalReceipt
        );
        assert_eq!(
            decide(&before.job, false, Publication::Reserved, Control::Cancel),
            Err(Refusal::PublicationCommitting)
        );
        assert_eq!(
            decide(
                &before.job,
                false,
                Publication::Reserved,
                Control::Retry {
                    current_revision: 1,
                    reviewed_revision: 1
                }
            ),
            Err(Refusal::PublicationCommitting)
        );
    }
}
