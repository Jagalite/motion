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
