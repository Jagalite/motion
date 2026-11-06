//! Preview withdrawal, failed removal and retry interleavings.
use playscale_core::{
    jobs::Phase,
    previews::{self, ArtifactIdentity, RetentionAction, RetentionObservation},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    row: bool,
    directory: bool,
    current: bool,
    pending: bool,
    staging: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Invalidate,
    Sweep {
        commit: bool,
    },
    Orphan,
    StaleIntent,
    Remove {
        removal_ok: bool,
        acknowledge: bool,
        matching_identity: bool,
    },
    Staging {
        live: bool,
        witness: bool,
        removal_ok: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    withdrew: bool,
    remove_directory: bool,
    remove_staging: bool,
}
struct Retention;
fn artifact() -> ArtifactIdentity {
    ArtifactIdentity {
        file: "f".into(),
        revision: "r".into(),
        layout_identity: "l".into(),
    }
}
impl Model for Retention {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.preview_retention".into(),
            model_version: 2,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/previews.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            row: true,
            directory: true,
            current: true,
            pending: false,
            staging: true,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let mut n = s.clone();
        let mut out = Output {
            withdrew: false,
            remove_directory: false,
            remove_staging: false,
        };
        match *i {
            Input::Invalidate => n.current = false,
            Input::StaleIntent => n.pending = true,
            Input::Sweep { commit } => {
                let observed = RetentionObservation {
                    artifact: artifact(),
                    current_layout_identity: if s.current { Some("l".into()) } else { None },
                    sheet_count: 1,
                    sheets_present: vec![s.directory],
                };
                if s.row
                    && matches!(
                        previews::retain_publication(&observed),
                        RetentionAction::Withdraw { .. }
                    )
                    && commit
                {
                    n.row = false;
                    out.withdrew = true;
                    n.pending = true;
                }
            }
            Input::Orphan => {
                if s.directory
                    && matches!(previews::orphan_cleanup("f", "r", s.row), previews::CleanupAction::RemoveOrphan { file, revision } if file == "f" && revision == "r")
                {
                    n.pending = true;
                }
            }
            Input::Remove {
                removal_ok,
                acknowledge,
                matching_identity,
            } => {
                let intent = previews::RemovalIntent {
                    directory: "f/r".into(),
                };
                if s.pending
                    && matches!(previews::plan_removal(&intent, s.row), previews::RemovalAction::Remove { directory } if directory == "f/r")
                {
                    out.remove_directory = true;
                    if removal_ok {
                        n.directory = false;
                    }
                    if acknowledge
                        && previews::acknowledge_removal(
                            &intent,
                            if matching_identity { "f/r" } else { "other/r" },
                            removal_ok,
                        )
                    {
                        n.pending = false;
                    }
                }
            }
            Input::Staging {
                live,
                witness,
                removal_ok,
            } => {
                let job = if live {
                    Some((Phase::Running, 1))
                } else {
                    Some((Phase::Completed, 1))
                };
                if s.staging
                    && matches!(previews::staging_cleanup("job", job, 1, witness), previews::CleanupAction::RemoveStaging { job, attempt } if job == "job" && attempt == 1)
                {
                    out.remove_staging = true;
                    if removal_ok {
                        n.staging = false;
                    }
                }
            }
        }
        Ok(Transition::accepted(n, vec![out]))
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
        let mut expected = s.clone();
        let mut out = Output {
            withdrew: false,
            remove_directory: false,
            remove_staging: false,
        };
        match *i {
            Input::Invalidate => expected.current = false,
            Input::StaleIntent => expected.pending = true,
            Input::Sweep { commit } => {
                if s.row && (!s.current || !s.directory) && commit {
                    expected.row = false;
                    out.withdrew = true;
                    expected.pending = true;
                }
            }
            Input::Orphan => {
                if s.directory && !s.row {
                    expected.pending = true;
                }
            }
            Input::Remove {
                removal_ok,
                acknowledge,
                matching_identity,
            } => {
                if s.pending && !s.row {
                    out.remove_directory = true;
                    if removal_ok {
                        expected.directory = false;
                    }
                    if removal_ok && acknowledge && matching_identity {
                        expected.pending = false;
                    }
                }
            }
            Input::Staging {
                live,
                witness,
                removal_ok,
            } => {
                if s.staging && !live && !witness {
                    out.remove_staging = true;
                    if removal_ok {
                        expected.staging = false;
                    }
                }
            }
        }
        let ok = n.state == &expected && n.outputs == [out];
        Ok(vec![if ok {
            Check::passed("preview.retention_effects")
        } else {
            Check::failed(
                "preview.retention_effects",
                "incorrect withdrawal or removal",
            )
        }])
    }
}
impl Enumerate for Retention {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![Input::Invalidate, Input::Orphan, Input::StaleIntent];
        for commit in [false, true] {
            v.push(Input::Sweep { commit });
        }
        for removal_ok in [false, true] {
            for acknowledge in [false, true] {
                for matching_identity in [false, true] {
                    v.push(Input::Remove {
                        removal_ok,
                        acknowledge,
                        matching_identity,
                    });
                }
            }
            for live in [false, true] {
                for witness in [false, true] {
                    v.push(Input::Staging {
                        live,
                        witness,
                        removal_ok,
                    });
                }
            }
        }
        Ok(v)
    }
}
impl ModelCodec for Retention {
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
fn bounded_withdrawal_retries_failed_cleanup() {
    let report = stateless::explore::enumerate(
        &Retention,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 4000,
            max_depth: 10,
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
        "Preview retention: {} states, {} transitions",
        report.states, report.transitions
    );
}
