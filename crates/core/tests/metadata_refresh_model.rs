//! Bounded refresh completion, identity fencing, acknowledgments and stale jobs.
use playscale_core::{
    jobs::{Job, Phase},
    metadata::{self, CurrentRefreshBinding, RefreshBinding, RefreshResult},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    phase: Phase,
    attempt: u32,
    error: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    attempt: u32,
    authorized: bool,
    available: bool,
    current_kind: u8,
    usable: bool,
    root_matches: bool,
    publication: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    committed: bool,
}
struct Refresh;
fn binding() -> RefreshBinding {
    RefreshBinding {
        work: "work".into(),
        file: "f".into(),
        relative: "film".into(),
        root: "root".into(),
        root_identity: "disk".into(),
        binding_revision: 1,
        file_revision: "v1".into(),
    }
}
impl Model for Refresh {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.metadata_refresh".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/metadata.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            phase: Phase::Running,
            attempt: 1,
            error: Some("prior".into()),
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let job = Job {
            phase: s.phase,
            attempt: s.attempt,
        };
        let mut result = RefreshResult::Unavailable;
        if metadata::refresh_may_publish(&job, i.attempt, i.authorized, i.available) {
            let original = binding();
            let mut current = original.clone();
            if i.current_kind == 1 {
                current.file_revision = "v2".into();
            }
            if i.current_kind == 2 {
                current.work = "other".into();
            }
            let current = CurrentRefreshBinding {
                binding: current,
                enabled: i.current_kind != 3,
                available: true,
            };
            let root_matches = metadata::refresh_root_matches(
                &original.root_identity,
                if i.root_matches {
                    Some("disk")
                } else {
                    Some("other")
                },
            );
            let accepted = metadata::refresh_observation_current(
                &original,
                Some(&current),
                i.usable && root_matches,
            );
            let observed = vec!["f".into()];
            let usable = if i.usable && root_matches {
                observed.clone()
            } else {
                vec![]
            };
            let published = if accepted && i.publication == 0 {
                observed.clone()
            } else if i.publication == 1 {
                vec!["other".into()]
            } else {
                vec![]
            };
            result = RefreshResult::Acknowledged {
                observed,
                usable,
                published,
            };
        }
        let completion = metadata::finish_refresh(&job, i.attempt, i.authorized, &result);
        let (next, committed) = match completion {
            Some(c) => (
                State {
                    phase: c.job.phase,
                    attempt: c.job.attempt,
                    error: c.error,
                },
                c.commit_publication,
            ),
            None => (s.clone(), false),
        };
        Ok(Transition::accepted(next, vec![Output { committed }]))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let matching =
            s.attempt == i.attempt && matches!(s.phase, Phase::Running | Phase::Cancelling);
        let committed = s.phase == Phase::Running
            && matching
            && i.authorized
            && i.available
            && i.current_kind == 0
            && i.usable
            && i.root_matches
            && i.publication == 0;
        let expected_phase = if !matching {
            s.phase
        } else if s.phase == Phase::Cancelling {
            Phase::Cancelled
        } else if committed {
            Phase::Completed
        } else {
            Phase::Failed
        };
        let expected_error = if !matching {
            s.error.clone()
        } else if committed {
            None
        } else {
            Some("refresh_unavailable_or_unauthorized".into())
        };
        let ok = n.state.phase == expected_phase
            && n.state.attempt == s.attempt
            && n.state.error == expected_error
            && n.outputs == [Output { committed }];
        Ok(vec![if ok {
            Check::passed("refresh.identity_and_completion")
        } else {
            Check::failed("refresh.identity_and_completion", "wrong refresh decision")
        }])
    }
}
impl Enumerate for Refresh {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![];
        for attempt in 0..=2 {
            for authorized in [false, true] {
                for available in [false, true] {
                    for current_kind in 0..4 {
                        for usable in [false, true] {
                            for root_matches in [false, true] {
                                for publication in 0..3 {
                                    v.push(Input {
                                        attempt,
                                        authorized,
                                        available,
                                        current_kind,
                                        usable,
                                        root_matches,
                                        publication,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(v)
    }
}
impl ModelCodec for Refresh {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        dec(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        dec(b)
    }
    fn encode_output(&self, v: &Output) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
}
fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn dec<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(b).map_err(|e| ModelError::new(e.to_string()))
}
#[test]
fn bounded_refresh_fences_identity_and_stale_completions() {
    for phase in [Phase::Running, Phase::Cancelling, Phase::Queued] {
        struct Initial(Phase);
        // Seed distinct phases through an adapter around the same model.
        impl Model for Initial {
            type State = State;
            type Input = Input;
            type Output = Output;
            fn metadata(&self) -> ModelMetadata {
                Refresh.metadata()
            }
            fn initial_state(&self) -> Result<State, ModelError> {
                Ok(State {
                    phase: self.0,
                    attempt: 1,
                    error: Some("prior".into()),
                })
            }
            fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
                Refresh.step(s, i)
            }
            fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
                Refresh.check_state(s)
            }
            fn check_transition(
                &self,
                s: &State,
                i: &Input,
                n: &TransitionRef<'_, State, Output>,
            ) -> Result<Vec<Check>, ModelError> {
                Refresh.check_transition(s, i, n)
            }
        }
        impl Enumerate for Initial {
            fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
                Refresh.inputs(s)
            }
        }
        let report = stateless::explore::enumerate(
            &Initial(phase),
            stateless::explore::SearchConfig {
                max_states: 100,
                max_transitions: 5000,
                max_depth: 5,
            },
        )
        .unwrap();
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(report.skipped_checks, 0);
        assert_eq!(
            report.termination,
            stateless::explore::SearchTermination::GraphExhausted
        );
        println!(
            "Refresh {phase:?}: {} states, {} transitions",
            report.states, report.transitions
        );
    }
}
