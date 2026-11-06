//! Bounded probe choices over cached stream facts and chapter observations.
use playscale_core::scan::{self, ProbeDecision};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    last: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    enabled: bool,
    supported: bool,
    full: bool,
    fingerprint_matches: bool,
    kind: u8,
    version: u32,
    chapters: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    choice: u8,
}
struct Reuse;
fn kind(i: u8) -> &'static str {
    ["audio", "video", "attachment", "subtitle"][i as usize]
}
impl Model for Reuse {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.probe_reuse".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/scan.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State { last: 0 })
    }
    fn step(&self, _: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let choice = if !scan::incremental_cache_enabled(i.enabled, i.supported, i.full) {
            0
        } else {
            match scan::probe_decision(
                "a",
                if i.fingerprint_matches { "a" } else { "b" },
                [(kind(i.kind), i.version)],
                i.chapters,
            ) {
                ProbeDecision::InspectFully => 0,
                ProbeDecision::ProbeChapters => 1,
                ProbeDecision::Reuse => 2,
            }
        };
        Ok(Transition::accepted(
            State { last: choice },
            vec![Output { choice }],
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
        let expected = if !i.enabled
            || !i.supported
            || i.full
            || !i.fingerprint_matches
            || (i.version < 2 && (i.kind == 1 || i.kind == 2))
        {
            0
        } else if !i.chapters {
            1
        } else {
            2
        };
        let ok = n.state.last == expected && n.outputs == [Output { choice: expected }];
        Ok(vec![if ok {
            Check::passed("probe.exact_decision")
        } else {
            Check::failed("probe.exact_decision", "wrong probe choice")
        }])
    }
}
impl Enumerate for Reuse {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![];
        for enabled in [false, true] {
            for supported in [false, true] {
                for full in [false, true] {
                    for fingerprint_matches in [false, true] {
                        for kind in 0..4 {
                            for version in 0..=2 {
                                for chapters in [false, true] {
                                    inputs.push(Input {
                                        enabled,
                                        supported,
                                        full,
                                        fingerprint_matches,
                                        kind,
                                        version,
                                        chapters,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(inputs)
    }
}
impl ModelCodec for Reuse {
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
fn bounded_probe_choices_match_evidence() {
    let report = stateless::explore::enumerate(
        &Reuse,
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
        "Probe reuse: {} states, {} transitions",
        report.states, report.transitions
    );
}
