use playscale_core::{
    jobs::{self, Effect, Job, Phase},
    processing::{self, Completion},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    durable: processing::State,
    source: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    Control(jobs::Input),
    Source(Option<String>),
    Complete {
        attempt: u32,
        valid: bool,
        commit: bool,
    },
}
struct Processing;
impl Model for Processing {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.processing-publication".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: concat!(
                include_str!("../src/processing.rs"),
                "\n",
                include_str!("../src/jobs.rs")
            )
            .into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            durable: processing::State {
                job: Job::default(),
                source_revision: "original".into(),
                published_output: None,
            },
            source: Some("original".into()),
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effects = match i {
            Input::Control(input) => {
                let (job, effects) = jobs::transition(&s.durable.job, *input);
                next.durable.job = job;
                effects
            }
            Input::Source(source) => {
                next.source = source.clone();
                vec![]
            }
            Input::Complete {
                attempt,
                valid,
                commit,
            } => {
                let (durable, effects) = processing::finish(
                    &s.durable,
                    &Completion {
                        attempt: *attempt,
                        current_source_revision: s.source.clone(),
                        validated_output: valid.then(|| "output".into()),
                    },
                );
                // Adapter contract: the complete publication unit commits or rolls back.
                if *commit {
                    next.durable = durable;
                    effects
                } else {
                    vec![]
                }
            }
        };
        Ok(Transition::accepted(next, effects))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "publication_and_completion_atomic",
            (s.durable.job.phase == Phase::Completed) == s.durable.published_output.is_some(),
        )])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let publishes: Vec<_> = n
            .outputs
            .iter()
            .filter(|e| matches!(e, Effect::Publish { .. }))
            .collect();
        let allowed = matches!(i, Input::Complete { attempt, valid:true, commit:true } if *attempt == s.durable.job.attempt)
            && s.durable.job.phase == Phase::Running
            && s.source.as_deref() == Some("original")
            && s.durable.published_output.is_none();
        let noop = matches!(i, Input::Complete {attempt, commit, ..} if !commit || *attempt != s.durable.job.attempt || s.durable.published_output.is_some());
        Ok(vec![
            check(
                "publish_exactly_once_with_current_identity",
                if allowed {
                    publishes
                        == vec![&Effect::Publish {
                            attempt: s.durable.job.attempt,
                        }]
                } else {
                    publishes.is_empty()
                },
            ),
            check(
                "stale_duplicate_rollback_noop",
                !noop || (n.state == s && n.outputs.is_empty()),
            ),
            check(
                "publication_never_disappears",
                s.durable.published_output.is_none()
                    || n.state.durable.published_output == s.durable.published_output,
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "publication contract violated")
    }
}
impl Enumerate for Processing {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Control(jobs::Input::Cancel),
            Input::Control(jobs::Input::Retry),
            Input::Control(jobs::Input::Recover),
        ];
        if s.durable.job.attempt < 3 {
            inputs.push(Input::Control(jobs::Input::Start));
        }
        for source in [None, Some("original".into()), Some("replaced".into())] {
            inputs.push(Input::Source(source));
        }
        for attempt in 0..=4 {
            for valid in [false, true] {
                for commit in [false, true] {
                    inputs.push(Input::Complete {
                        attempt,
                        valid,
                        commit,
                    });
                }
            }
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Processing {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Processing {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
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
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(b).map_err(|e| ModelError::new(e.to_string()))
}
#[test]
fn processing_graph_covers_source_changes_cancellation_duplicate_and_rollback() {
    let report = stateless::explore::enumerate(&Processing, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Processing model: {} states, {} transitions; attempts <=3, three source states, valid/invalid output, commit/rollback",
        report.states, report.transitions
    );
}
#[test]
fn processing_seeded_fuzz_and_replay() {
    let report = stateless::explore::fuzz(
        &Processing,
        stateless::explore::FuzzConfig {
            seed: 20261007,
            cases: 1000,
            max_steps: 100,
            max_transitions: 100000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    println!(
        "Processing fuzz: {} cases, {} transitions",
        report.cases, report.transitions
    );
    let trace = stateless::execution::record(
        &Processing,
        vec![
            Input::Control(jobs::Input::Start),
            Input::Source(Some("replaced".into())),
            Input::Complete {
                attempt: 1,
                valid: true,
                commit: true,
            },
            Input::Control(jobs::Input::Retry),
            Input::Control(jobs::Input::Start),
            Input::Source(Some("original".into())),
            Input::Complete {
                attempt: 1,
                valid: true,
                commit: true,
            },
            Input::Complete {
                attempt: 2,
                valid: true,
                commit: false,
            },
            Input::Complete {
                attempt: 2,
                valid: true,
                commit: true,
            },
            Input::Complete {
                attempt: 2,
                valid: true,
                commit: true,
            },
        ],
        Default::default(),
        100,
    )
    .unwrap();
    assert_eq!(
        stateless::execution::replay(&Processing, &trace, Default::default())
            .unwrap()
            .outcome,
        stateless::execution::ReplayOutcome::Exact
    );
}
