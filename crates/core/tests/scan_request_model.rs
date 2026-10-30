//! Bounded request membership and per-source outcome decisions.
use playscale_core::demands::{self, RequestEffect, RequestMember};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    previous: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    members: u8,
    enabled: u8,
    selection: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    decisions: Vec<String>,
}
struct Requests;
fn ids(selection: u8) -> Option<Vec<String>> {
    match selection {
        0 => None,
        1 => Some(vec![]),
        2 => Some(vec!["a".into()]),
        3 => Some(vec!["b".into(), "a".into()]),
        4 => Some(vec!["a".into(), "x".into()]),
        _ => Some(vec!["b".into(), "b".into()]),
    }
}
impl Model for Requests {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.scan_request".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/demands.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State { previous: 0 })
    }
    fn step(&self, _: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let members: Vec<_> = [(1, "a"), (2, "b")]
            .into_iter()
            .filter(|(bit, _)| i.members & bit != 0)
            .map(|(bit, id)| RequestMember {
                source: id.into(),
                enabled: i.enabled & bit != 0,
            })
            .collect();
        let selected = ids(i.selection);
        let result = demands::initialize_request(&members, selected.as_deref());
        let decisions = match result {
            Ok(effects) => effects
                .into_iter()
                .map(|e| match e {
                    RequestEffect::Admit { source } => format!("admit:{source}"),
                    RequestEffect::FailDisabled { source } => format!("disabled:{source}"),
                })
                .collect(),
            Err(e) => vec![format!("error:{e:?}")],
        };
        Ok(Transition::accepted(
            State {
                previous: decisions.len() as u8,
            },
            vec![Output { decisions }],
        ))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let selected = ids(i.selection).unwrap_or_else(|| {
            [(1, "a"), (2, "b")]
                .into_iter()
                .filter(|(bit, _)| i.members & bit != 0)
                .map(|(_, id)| id.into())
                .collect()
        });
        let mut expected = Vec::new();
        if selected.is_empty() {
            expected.push("error:NoSources".into());
        } else {
            for id in &selected {
                let bit = match id.as_str() {
                    "a" => 1,
                    "b" => 2,
                    _ => 0,
                };
                if bit == 0 || i.members & bit == 0 {
                    expected = vec![format!("error:UnknownSource({id:?})")];
                    break;
                }
                expected.push(format!(
                    "{}:{id}",
                    if i.enabled & bit != 0 {
                        "admit"
                    } else {
                        "disabled"
                    }
                ));
            }
        }
        let ok = n.state.previous as usize == expected.len()
            && n.outputs
                == [Output {
                    decisions: expected,
                }];
        Ok(vec![if ok {
            Check::passed("request.exact_outcomes")
        } else {
            Check::failed("request.exact_outcomes", "wrong request plan")
        }])
    }
}
impl Enumerate for Requests {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![];
        for members in 0..4 {
            for enabled in 0..4 {
                for selection in 0..6 {
                    v.push(Input {
                        members,
                        enabled,
                        selection,
                    });
                }
            }
        }
        Ok(v)
    }
}
impl ModelCodec for Requests {
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
fn bounded_request_initialization_preserves_exact_outcomes() {
    let report = stateless::explore::enumerate(
        &Requests,
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
        "Scan requests: {} states, {} transitions",
        report.states, report.transitions
    );
}
