//! Bounded preview publication identity, fencing and transaction rollback.
use playscale_core::{
    jobs::{Job, Phase},
    previews::{self, ArtifactIdentity, PublicationAction, Sheet},
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
    published: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    attempt: u32,
    authorized: bool,
    identity_current: bool,
    sheets_valid: bool,
    stuck: bool,
    rollback: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    action: Option<PublicationAction>,
    cleanup: bool,
}
struct Preview(Phase, u8);
fn artifact(layout: &str) -> ArtifactIdentity {
    ArtifactIdentity {
        file: "f".into(),
        revision: "r".into(),
        layout_identity: layout.into(),
    }
}
impl Model for Preview {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.preview_publication".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/previews.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            phase: self.0,
            attempt: 1,
            error: Some("prior".into()),
            published: self.1,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let job = Job {
            phase: s.phase,
            attempt: s.attempt,
        };
        let layout = previews::layout(10_000, 160, 90).unwrap();
        let sheet = Sheet {
            width: layout.tile_width * layout.columns,
            height: layout.tile_height * layout.rows,
            bytes: if i.sheets_valid { 10 } else { 0 },
        };
        let expected = artifact("new");
        let previous = match s.published {
            1 => Some(artifact("old")),
            2 => Some(expected.clone()),
            _ => None,
        };
        let plan = previews::plan_publication(
            &job,
            i.attempt,
            "job",
            &expected,
            Some(if i.identity_current { "new" } else { "old" }),
            Some((&layout, &[sheet], true)),
            i.authorized,
            previous.as_ref(),
            i.stuck,
        );
        let mut next = s.clone();
        if !i.rollback {
            if let Some(j) = &plan.job {
                next.phase = j.phase;
                next.attempt = j.attempt;
                next.error = plan.error.clone();
            }
            if matches!(plan.action, Some(PublicationAction::Publish { .. })) {
                next.published = 2;
            }
        }
        Ok(Transition::accepted(
            next,
            vec![Output {
                action: if i.rollback { None } else { plan.action },
                cleanup: !i.rollback && plan.remove_staging,
            }],
        ))
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
        let valid = matching
            && s.phase == Phase::Running
            && i.authorized
            && i.identity_current
            && i.sheets_valid;
        let layout = previews::layout(10_000, 160, 90).unwrap();
        let art = artifact("new");
        let action = if i.rollback || !valid {
            None
        } else if s.published == 2 {
            Some(PublicationAction::Reuse { artifact: art })
        } else {
            Some(PublicationAction::Publish {
                job: "job".into(),
                attempt: i.attempt,
                artifact: art,
                layout,
            })
        };
        let phase = if i.rollback || !matching {
            s.phase
        } else if s.phase == Phase::Cancelling {
            Phase::Cancelled
        } else if valid {
            Phase::Completed
        } else {
            Phase::Failed
        };
        let error = if i.rollback || !matching {
            s.error.clone()
        } else if phase == Phase::Failed {
            Some("preview_failed".into())
        } else {
            None
        };
        let published = if !i.rollback && valid && s.published != 2 {
            2
        } else {
            s.published
        };
        let ok = n.state.phase == phase
            && n.state.attempt == s.attempt
            && n.state.error == error
            && n.state.published == published
            && n.outputs
                == [Output {
                    action,
                    cleanup: !i.rollback && !i.stuck,
                }];
        Ok(vec![if ok {
            Check::passed("preview.publication_identity")
        } else {
            Check::failed("preview.publication_identity", "incorrect publication plan")
        }])
    }
}
impl Enumerate for Preview {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![];
        for attempt in 0..=2 {
            for authorized in [false, true] {
                for identity_current in [false, true] {
                    for sheets_valid in [false, true] {
                        for stuck in [false, true] {
                            for rollback in [false, true] {
                                v.push(Input {
                                    attempt,
                                    authorized,
                                    identity_current,
                                    sheets_valid,
                                    stuck,
                                    rollback,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(v)
    }
}
impl ModelCodec for Preview {
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
fn bounded_publication_fences_attempts_and_exact_artifacts() {
    for phase in [Phase::Running, Phase::Cancelling, Phase::Queued] {
        for prior in 0..=2 {
            let report = stateless::explore::enumerate(
                &Preview(phase, prior),
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
                "Preview {phase:?} prior={prior}: {} states, {} transitions",
                report.states, report.transitions
            );
        }
    }
}
