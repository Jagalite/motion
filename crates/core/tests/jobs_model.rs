use playscale_core::jobs::{Effect, Input, Job, Phase, transition};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

struct Jobs;
impl stateless::Generate for Jobs {
    fn generate(&self, state: &Job, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(state)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i]))
    }
}
impl Model for Jobs {
    type State = Job;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.jobs".into(),
            model_version: 1,
            properties_version: 3,
            codec_version: 1,
            build: format!(
                "{}:{}",
                env!("CARGO_PKG_VERSION"),
                include_str!("../src/jobs.rs")
            ),
        }
    }
    fn initial_state(&self) -> Result<Job, ModelError> {
        Ok(Job::default())
    }
    fn step(&self, s: &Job, i: &Input) -> Result<Transition<Job, Effect>, ModelError> {
        let (state, outputs) = transition(s, *i);
        Ok(Transition::accepted(state, outputs))
    }
    fn check_state(&self, s: &Job) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "active_attempt_exists",
            !matches!(
                s.phase,
                Phase::Running | Phase::Cancelling | Phase::Completed
            ) || s.attempt > 0,
        )])
    }
    fn check_transition(
        &self,
        before: &Job,
        input: &Input,
        next: &TransitionRef<'_, Job, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let stale = matches!(input,Input::Finished{attempt,..} if *attempt!=before.attempt);
        let publishes = next
            .outputs
            .iter()
            .any(|e| matches!(e, Effect::Publish { .. }));
        let should_publish = before.phase == Phase::Running
            && matches!(input,Input::Finished{attempt,success:true} if *attempt==before.attempt);
        let expected_effect = match (before.phase, input) {
            (Phase::Queued, Input::Start) if before.attempt < u32::MAX => Some(Effect::Run {
                attempt: before.attempt + 1,
            }),
            (Phase::Running, Input::Cancel) => Some(Effect::Stop {
                attempt: before.attempt,
            }),
            _ if should_publish => Some(Effect::Publish {
                attempt: before.attempt,
            }),
            _ => None,
        };
        Ok(vec![
            check(
                "exact_effect_identity_and_count",
                next.outputs == expected_effect.into_iter().collect::<Vec<_>>(),
            ),
            check(
                "recovery_outcome",
                *input != Input::Recover
                    || next.state.phase
                        == match before.phase {
                            Phase::Running if before.attempt == u32::MAX => Phase::Failed,
                            Phase::Running => Phase::Queued,
                            Phase::Cancelling => Phase::Cancelled,
                            phase => phase,
                        },
            ),
            check(
                "stale_completion_noop",
                !stale || (next.state == before && next.outputs.is_empty()),
            ),
            check(
                "publication_exactly_on_current_success",
                publishes == should_publish,
            ),
            check(
                "cancel_never_publishes",
                before.phase != Phase::Cancelling || !publishes,
            ),
            check(
                "attempt_never_decreases",
                next.state.attempt >= before.attempt,
            ),
            check(
                "only_start_increments_attempt",
                next.state.attempt == before.attempt
                    || (before.phase == Phase::Queued
                        && *input == Input::Start
                        && next.state.attempt == before.attempt + 1),
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "job invariant violated")
    }
}
impl Enumerate for Jobs {
    fn inputs(&self, state: &Job) -> Result<Vec<Input>, ModelError> {
        // Finite domain: three attempts, every phase control, stale/current/future completions.
        let mut inputs = vec![Input::Cancel, Input::Recover, Input::Retry];
        if state.attempt < 3 {
            inputs.push(Input::Start);
        }
        for attempt in 0..=4 {
            for success in [false, true] {
                inputs.push(Input::Finished { attempt, success });
            }
        }
        Ok(inputs)
    }
}
impl ModelCodec for Jobs {
    fn encode_state(&self, v: &Job) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<Job, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}
fn encode<T: serde::Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}

#[test]
fn bounded_job_graph_checks_production_reducer() {
    let report = stateless::explore::enumerate(&Jobs, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    assert!(report.transitions > 100);
    println!(
        "Stateless: {} states, {} edges; attempts <=3, completion identities 0..4; only declared properties checked",
        report.states, report.transitions
    );
}

#[test]
fn cancellation_retry_stale_completion_replays_exactly() {
    let inputs = vec![
        Input::Start,
        Input::Cancel,
        Input::Finished {
            attempt: 1,
            success: true,
        },
        Input::Retry,
        Input::Start,
        Input::Finished {
            attempt: 1,
            success: true,
        },
        Input::Finished {
            attempt: 2,
            success: true,
        },
    ];
    let trace = stateless::execution::record(&Jobs, inputs, Default::default(), 100).unwrap();
    let replay = stateless::execution::replay(&Jobs, &trace, Default::default()).unwrap();
    assert_eq!(replay.outcome, stateless::execution::ReplayOutcome::Exact);
    assert_eq!(replay.steps_verified, 7);
}

#[test]
fn seeded_job_sequences_check_order_dependent_properties() {
    let report = stateless::explore::fuzz(
        &Jobs,
        stateless::explore::FuzzConfig {
            seed: 20261006,
            cases: 1000,
            max_steps: 100,
            max_transitions: 100000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none());
    assert_eq!(report.skipped_checks, 0);
    assert!(matches!(
        report.termination,
        stateless::explore::FuzzTermination::CasesCompleted
            | stateless::explore::FuzzTermination::TransitionLimit
    ));
    assert!(report.transitions > 10000);
    println!(
        "Stateless fuzz: {} cases, {} transitions, {:?}",
        report.cases, report.transitions, report.termination
    );
}

/// Reviewed retries: a client reviews the job at its current revision and
/// later submits a retry conditional on that revision, while workers and
/// other clients keep changing the job. The adapter bumps the revision on
/// every change; admission is the production `admit_retry` followed by the
/// production reducer.
mod reviewed {
    use super::*;
    use playscale_core::jobs::{RetryRefusal, admit_retry};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct State {
        pub job: Job,
        pub revision: u64,
        /// The revision the client last reviewed.
        pub reviewed: Option<u64>,
        /// Retries admitted since that review.
        pub admitted: u8,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum Event {
        Review,
        /// A worker or another client drives the job.
        Job(Input),
        RetryReviewed,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Outcome {
        Admitted,
        Refused(RetryRefusal),
    }

    pub struct ReviewedJobs;

    const MAX_REVISION: u64 = 9;

    impl stateless::Generate for ReviewedJobs {
        fn generate(
            &self,
            state: &State,
            rng: &mut stateless::Rng,
        ) -> Result<Option<Event>, ModelError> {
            let inputs = self.inputs(state)?;
            Ok(rng.index(inputs.len()).map(|i| inputs[i]))
        }
    }

    impl Model for ReviewedJobs {
        type State = State;
        type Input = Event;
        type Output = Outcome;
        fn metadata(&self) -> ModelMetadata {
            ModelMetadata {
                name: "playscale.jobs.reviewed_retry".into(),
                model_version: 1,
                properties_version: 1,
                codec_version: 1,
                build: format!(
                    "{}:{}",
                    env!("CARGO_PKG_VERSION"),
                    include_str!("../src/jobs.rs")
                ),
            }
        }
        fn initial_state(&self) -> Result<State, ModelError> {
            Ok(State {
                job: Job::default(),
                revision: 1,
                reviewed: None,
                admitted: 0,
            })
        }
        fn step(&self, s: &State, e: &Event) -> Result<Transition<State, Outcome>, ModelError> {
            let mut next = s.clone();
            let mut outputs = vec![];
            match *e {
                Event::Review => {
                    next.reviewed = Some(s.revision);
                    next.admitted = 0;
                }
                Event::Job(input) => {
                    let (job, _) = transition(&s.job, input);
                    if job != s.job {
                        next.revision += 1;
                    }
                    next.job = job;
                }
                Event::RetryReviewed => {
                    let Some(reviewed) = s.reviewed else {
                        return Ok(Transition::accepted(next, outputs));
                    };
                    match admit_retry(&s.job, s.revision, reviewed) {
                        Ok(()) => {
                            let (job, _) = transition(&s.job, Input::Retry);
                            if job != s.job {
                                next.revision += 1;
                            }
                            next.job = job;
                            next.admitted += 1;
                            outputs.push(Outcome::Admitted);
                        }
                        Err(refusal) => outputs.push(Outcome::Refused(refusal)),
                    }
                }
            }
            Ok(Transition::accepted(next, outputs))
        }
        fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
            Ok(vec![check("one_retry_per_review", s.admitted <= 1)])
        }
        fn check_transition(
            &self,
            before: &State,
            event: &Event,
            next: &TransitionRef<'_, State, Outcome>,
        ) -> Result<Vec<Check>, ModelError> {
            let admitted = next.outputs.contains(&Outcome::Admitted);
            let at_review = before.reviewed == Some(before.revision);
            Ok(vec![
                check(
                    "retry_admitted_only_at_reviewed_revision",
                    !admitted || at_review,
                ),
                check(
                    "admission_exactly_when_current_and_retryable",
                    *event != Event::RetryReviewed
                        || before.reviewed.is_none()
                        || admitted == (at_review && before.job.retryable()),
                ),
                check(
                    "stale_review_is_refused_without_change",
                    *event != Event::RetryReviewed
                        || at_review
                        || before.reviewed.is_none()
                        || (next.state.job == before.job
                            && next.outputs == [Outcome::Refused(RetryRefusal::StaleRevision)]),
                ),
                check(
                    "admitted_retry_queues_and_advances_revision",
                    !admitted
                        || (next.state.job.phase == Phase::Queued
                            && next.state.revision == before.revision + 1
                            && next.state.job.attempt == before.job.attempt),
                ),
            ])
        }
    }

    impl Enumerate for ReviewedJobs {
        fn inputs(&self, s: &State) -> Result<Vec<Event>, ModelError> {
            // Bounded: job changes up to MAX_REVISION (an admitted retry can
            // reach one more), attempts up to 2,
            // current and stale completion identities.
            let mut inputs = vec![Event::Review, Event::RetryReviewed];
            if s.revision < MAX_REVISION {
                inputs.push(Event::Job(Input::Cancel));
                inputs.push(Event::Job(Input::Recover));
                inputs.push(Event::Job(Input::Retry));
                if s.job.attempt < 2 {
                    inputs.push(Event::Job(Input::Start));
                }
                for attempt in 0..=2 {
                    for success in [false, true] {
                        inputs.push(Event::Job(Input::Finished { attempt, success }));
                    }
                }
            }
            Ok(inputs)
        }
    }

    impl ModelCodec for ReviewedJobs {
        fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
            encode(v)
        }
        fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
            decode(b)
        }
        fn encode_input(&self, v: &Event) -> Result<Vec<u8>, ModelError> {
            encode(v)
        }
        fn decode_input(&self, b: &[u8]) -> Result<Event, ModelError> {
            decode(b)
        }
        fn encode_output(&self, v: &Outcome) -> Result<Vec<u8>, ModelError> {
            encode(v)
        }
    }
}

#[test]
fn bounded_reviewed_retry_graph_admits_one_retry_per_review() {
    let report =
        stateless::explore::enumerate(&reviewed::ReviewedJobs, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    assert!(report.transitions > 1000);
    println!(
        "Stateless reviewed retry: {} states, {} edges; revisions <=10 (job changes stop at 9), attempts <=2, completion identities 0..2",
        report.states, report.transitions
    );
}

#[test]
fn stale_review_after_another_clients_retry_is_refused() {
    use reviewed::Event;
    // Fail, review, another client retries and the attempt fails again; the
    // old review must not admit a second retry.
    let inputs = vec![
        Event::Job(Input::Start),
        Event::Job(Input::Finished {
            attempt: 1,
            success: false,
        }),
        Event::Review,
        Event::Job(Input::Retry),
        Event::Job(Input::Start),
        Event::Job(Input::Finished {
            attempt: 2,
            success: false,
        }),
        Event::RetryReviewed,
    ];
    let trace =
        stateless::execution::record(&reviewed::ReviewedJobs, inputs, Default::default(), 100)
            .unwrap();
    let replay =
        stateless::execution::replay(&reviewed::ReviewedJobs, &trace, Default::default()).unwrap();
    assert_eq!(replay.outcome, stateless::execution::ReplayOutcome::Exact);
    assert_eq!(
        playscale_core::jobs::admit_retry(
            &Job {
                phase: Phase::Failed,
                attempt: 2
            },
            5,
            3
        ),
        Err(playscale_core::jobs::RetryRefusal::StaleRevision)
    );
}
