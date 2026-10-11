//! Same production import/retraction decisions under duplicate repair, manual
//! reset, session authority and rollback. One timeline, one canonical action.
use playscale_core::{
    identity::Attribution,
    timeline_viewing::{self as core, View},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    view: Option<View>,
    changed: bool,
    session: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Repair,
    RollbackRepair,
    Manual,
    Session,
}
struct Repair {
    exact: bool,
    backfill: bool,
}
fn legacy() -> View {
    View {
        position_ms: 20000,
        ..View::default()
    }
}
impl Repair {
    fn attribution(&self) -> Attribution {
        if self.exact {
            Attribution::Exact {
                timeline: "t".into(),
            }
        } else {
            Attribution::Orphaned
        }
    }
    fn repair(&self, s: &State) -> State {
        let mut next = s.clone();
        if let Some(view) = &s.view {
            if core::retract_unattributed_legacy(&self.attribution(), view, &legacy(), s.session) {
                next.view = None;
            }
        }
        if let Some(view) =
            core::import_legacy(&self.attribution(), next.view.as_ref(), 20000, false, None)
        {
            next.view = Some(view);
        }
        next
    }
}
impl Model for Repair {
    type State = State;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.legacy-import".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/timeline_viewing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            view: self.backfill.then(legacy),
            changed: false,
            session: false,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, bool>, ModelError> {
        let mut n = s.clone();
        match i {
            Input::Repair => n = self.repair(s),
            Input::RollbackRepair => {
                let _uncommitted = self.repair(s);
            }
            Input::Manual => {
                n.view = Some(View {
                    revision: 1,
                    manual_epoch: 1,
                    manual_watched: Some(false),
                    ..View::default()
                });
                n.changed = true;
            }
            Input::Session => {
                n.view = Some(View {
                    revision: 1,
                    session: Some("session".into()),
                    ..View::default()
                });
                n.changed = true;
                n.session = true;
            }
        }
        Ok(Transition::accepted(n, vec![true]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let canonical = !s.changed || s.view.as_ref().is_some_and(|v| v.revision > 0);
        Ok(vec![if canonical {
            Check::passed("canonical_action_retains_revision")
        } else {
            Check::failed(
                "canonical_action_retains_revision",
                "canonical authority was lost",
            )
        }])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        t: &TransitionRef<'_, State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let ok = match i {
            Input::RollbackRepair => t.state == s,
            Input::Repair => {
                if s.changed {
                    t.state == s
                } else if self.exact {
                    t.state.view == Some(legacy())
                } else {
                    t.state.view.is_none()
                }
            }
            _ => true,
        };
        Ok(vec![if ok {
            Check::passed("repair_preserves_authority_or_imports_only_exact")
        } else {
            Check::failed(
                "repair_preserves_authority_or_imports_only_exact",
                "invalid publication or lost canonical action",
            )
        }])
    }
}
impl Enumerate for Repair {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![Input::Repair, Input::RollbackRepair];
        if !s.changed {
            v.extend([Input::Manual, Input::Session]);
        }
        Ok(v)
    }
}
impl ModelCodec for Repair {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<State, ModelError> {
        decode(v)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
        decode(v)
    }
    fn encode_output(&self, v: &bool) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
#[test]
fn legacy_repair_interleavings() {
    for exact in [false, true] {
        for backfill in [false, true] {
            let r = stateless::explore::enumerate(
                &Repair { exact, backfill },
                stateless::explore::SearchConfig {
                    max_states: 100,
                    max_transitions: 1000,
                    max_depth: 20,
                },
            )
            .unwrap();
            assert!(r.failure.is_none(), "{:?}", r.failure);
            assert_eq!(r.skipped_checks, 0);
            assert_eq!(
                r.termination,
                stateless::explore::SearchTermination::GraphExhausted
            );
            println!(
                "exact={exact} backfill={backfill}: {} states, {} transitions",
                r.states, r.transitions
            );
        }
    }
}
