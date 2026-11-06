//! Bounded exact whole-file preparation binding decisions.
use playscale_core::processing::{BindingEvidence, WholeBinding};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    accepted: bool,
    download: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    mask: u8,
    second_valid: bool,
    version_matches: bool,
    first_part: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    accepted: bool,
    download: bool,
}
struct Bindings;
fn candidate(mask: u8) -> WholeBinding {
    WholeBinding {
        version: "v".into(),
        timeline: if mask & 1 != 0 { "t" } else { "other" }.into(),
        file: if mask & 2 != 0 { "f" } else { "other" }.into(),
        file_revision: if mask & 4 != 0 { "r" } else { "old" }.into(),
        origin: if mask & 8 != 0 {
            "original"
        } else {
            "generated"
        }
        .into(),
        file_edition: if mask & 16 != 0 { "e" } else { "other" }.into(),
        timeline_edition: "e".into(),
        part: 1,
        start_ms: if mask & 32 != 0 { None } else { Some(0) },
        end_ms: if mask & 64 != 0 { None } else { Some(10) },
        part_count: if mask & 128 != 0 { 1 } else { 2 },
    }
}
impl Model for Bindings {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.preparation_binding".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/processing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            accepted: false,
            download: false,
        })
    }
    fn step(&self, _: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let mut first = candidate(i.mask);
        if !i.version_matches {
            first.version = "other".into();
        }
        if !i.first_part {
            first.part = 2;
        }
        let mut candidates = vec![first];
        if i.second_valid {
            candidates.push(candidate(255));
        }
        let evidence = BindingEvidence {
            timeline: "t".into(),
            file: "f".into(),
            revision: "r".into(),
            candidates,
        };
        let accepted = evidence.is_current();
        let download = evidence.matches_download("v");
        Ok(Transition::accepted(
            State { accepted, download },
            vec![Output { accepted, download }],
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
        let expected = i.mask == 255 || i.second_valid;
        let download = i.second_valid || (i.mask & 231 == 231 && i.version_matches && i.first_part);
        Ok(vec![if n.state.accepted == expected
            && n.state.download == download
            && n.outputs
                == [Output {
                    accepted: expected,
                    download,
                }] {
            Check::passed("binding.exact_whole_file")
        } else {
            Check::failed("binding.exact_whole_file", "incorrect binding")
        }])
    }
}
impl Enumerate for Bindings {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut values = Vec::new();
        for mask in 0..=255 {
            for second_valid in [false, true] {
                for version_matches in [false, true] {
                    for first_part in [false, true] {
                        values.push(Input {
                            mask,
                            second_valid,
                            version_matches,
                            first_part,
                        });
                    }
                }
            }
        }
        Ok(values)
    }
}
impl ModelCodec for Bindings {
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
fn bounded_whole_binding_uses_exact_facts() {
    let report = stateless::explore::enumerate(
        &Bindings,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 20000,
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
        "Preparation binding: {} states, {} transitions",
        report.states, report.transitions
    );
}
