//! Bounded source ownership and normalized tag outcomes.
use playscale_core::metadata::{self, Contribution, ContributionValidationError};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    prior: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    source: u8,
    excluded: bool,
    tag: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    result: String,
}
struct Validation;
fn source(n: u8) -> &'static str {
    ["local", "provider", "scan", "Bad"][n as usize]
}
fn tag(n: u8) -> &'static str {
    ["  DRAMA   Fun ", "  ", "ok"][n as usize]
}
impl Model for Validation {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.metadata_validation".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/metadata.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State { prior: 0 })
    }
    fn step(&self, _: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let mut c = Contribution::default();
        c.tags.insert(tag(i.tag).into());
        if i.excluded {
            c.excluded_tags.insert("  OLD ".into());
        }
        let result = match metadata::normalize_source_contribution(source(i.source), &c) {
            Ok(c) => format!(
                "ok:{}:{}",
                c.tags.iter().next().unwrap(),
                c.excluded_tags
                    .iter()
                    .next()
                    .map(String::as_str)
                    .unwrap_or("")
            ),
            Err(ContributionValidationError::InvalidSource) => "source".into(),
            Err(ContributionValidationError::BoundsOrExclusions) => "exclusions".into(),
            Err(ContributionValidationError::Tag) => "tag".into(),
            Err(e) => format!("unexpected:{e:?}"),
        };
        Ok(Transition::accepted(
            State {
                prior: u8::from(result.starts_with("ok:")),
            },
            vec![Output { result }],
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
        let expected = if i.source >= 2 {
            "source".to_owned()
        } else if i.source != 0 && i.excluded {
            "exclusions".to_owned()
        } else if i.tag == 1 {
            "tag".to_owned()
        } else {
            format!(
                "ok:{}:{}",
                if i.tag == 0 { "drama fun" } else { "ok" },
                if i.excluded { "old" } else { "" }
            )
        };
        let ok = n.state.prior == u8::from(expected.starts_with("ok:"))
            && n.outputs == [Output { result: expected }];
        Ok(vec![if ok {
            Check::passed("metadata.source_validation")
        } else {
            Check::failed("metadata.source_validation", "invalid source policy")
        }])
    }
}
impl Enumerate for Validation {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![];
        for source in 0..4 {
            for excluded in [false, true] {
                for tag in 0..3 {
                    v.push(Input {
                        source,
                        excluded,
                        tag,
                    });
                }
            }
        }
        Ok(v)
    }
}
impl ModelCodec for Validation {
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
fn bounded_source_validation() {
    let report = stateless::explore::enumerate(
        &Validation,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 1000,
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
        "Metadata validation: {} states, {} transitions",
        report.states, report.transitions
    );
}
