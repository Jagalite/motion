//! Bounded ordered playhead observations execute the production reducer.
use playscale_core::delivery::{self as core, Delivery, Effect, Error, Input, Operation, Pin};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    delivery: Delivery,
    steps: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Output {
    error: Option<Error>,
    effects: Vec<Effect>,
}
struct Observations;
impl Model for Observations {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.delivery-observations".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/delivery.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        let pin = Pin {
            source_file: "f".into(),
            source_revision: "r".into(),
            tracks: vec![],
            operation: Operation::Original,
            recipe_digest: None,
            clip: None,
            parts: vec![],
            max_bitrate_bps: None,
        };
        let (delivery, _) = Delivery::admit("t".into(), pin, 0, Some(1000), 0).unwrap();
        Ok(State { delivery, steps: 0 })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Output>, ModelError> {
        let (delivery, output) = match core::transition(&s.delivery, i) {
            Ok((d, effects)) => (
                d,
                Output {
                    error: None,
                    effects,
                },
            ),
            Err(e) => (
                s.delivery.clone(),
                Output {
                    error: Some(e),
                    effects: vec![],
                },
            ),
        };
        Ok(Transition::accepted(
            State {
                delivery,
                steps: s.steps + 1,
            },
            vec![output],
        ))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let valid = s
            .delivery
            .observation
            .is_none_or(|(g, sequence, position)| {
                sequence > 0
                    && (s.delivery.active != Some(g) || s.delivery.playhead == Some((g, position)))
            });
        Ok(vec![if valid {
            Check::passed("active_playhead_matches_latest_ordered_observation")
        } else {
            Check::failed(
                "active_playhead_matches_latest_ordered_observation",
                "playhead identity mismatch",
            )
        }])
    }
    fn check_transition(
        &self,
        s: &State,
        input: &Input,
        t: &TransitionRef<'_, State, Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let a = &s.delivery;
        let b = &t.state.delivery;
        let mut ok = true;
        if let Input::ObservePlayhead {
            generation,
            sequence,
            position_ms,
            now_ms,
        } = input
        {
            let old = a.observation.filter(|(g, _, _)| g == generation);
            let error = if a.status.fenced() || *now_ms >= a.lease_expires_ms {
                Some(Error::DeliveryClosed)
            } else if a.active != Some(*generation) {
                Some(Error::GenerationConflict)
            } else if *sequence == 0 || *position_ms > 2000 {
                Some(Error::InvalidPosition)
            } else if old.is_some_and(|(_, s, p)| s == *sequence && p != *position_ms) {
                Some(Error::GenerationConflict)
            } else {
                None
            };
            ok &= t.outputs[0].error == error;
            let fresh = error.is_none() && old.is_none_or(|(_, s, _)| *sequence > s);
            if fresh {
                ok &= b.playhead == Some((*generation, *position_ms));
                ok &= b.observation == Some((*generation, *sequence, *position_ms));
                ok &= b.lease_expires_ms == a.lease_expires_ms.max(now_ms + core::LEASE_MS);
            } else {
                ok &= a == b;
            }
            // Byte delivery has no encoder effects; retries/rejections never do.
            ok &= t.outputs[0].effects.is_empty();
        }
        Ok(vec![if ok {
            Check::passed("ordered_generation_fenced_observations_and_exact_effects")
        } else {
            Check::failed(
                "ordered_generation_fenced_observations_and_exact_effects",
                "observation contract",
            )
        }])
    }
}
impl Enumerate for Observations {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        if s.steps == 4 {
            return Ok(vec![]);
        }
        let mut result = vec![
            Input::Close,
            Input::Interrupt,
            Input::ObservePlayhead {
                generation: 1,
                sequence: 2,
                position_ms: 0,
                now_ms: s.delivery.lease_expires_ms,
            },
        ];
        if s.delivery.last_generation == 1 {
            result.push(Input::Change {
                expected_generation: 1,
                position_ms: 0,
                replan: None,
                overlap: true,
                now_ms: u64::from(s.steps) + 1,
            });
        }
        if s.delivery.pending == Some(2) {
            result.push(Input::Activate {
                generation: 2,
                expected_active: Some(1),
                now_ms: u64::from(s.steps) + 1,
            });
        }
        for generation in [1, 2] {
            for sequence in [0, 1, 2] {
                for position_ms in [0, 1000, 2001] {
                    result.push(Input::ObservePlayhead {
                        generation,
                        sequence,
                        position_ms,
                        now_ms: u64::from(s.steps) + 1,
                    });
                }
            }
        }
        Ok(result)
    }
}
impl ModelCodec for Observations {
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
    fn encode_output(&self, v: &Output) -> Result<Vec<u8>, ModelError> {
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
fn ordered_playhead_observations() {
    let report = stateless::explore::enumerate(
        &Observations,
        stateless::explore::SearchConfig {
            max_states: 100_000,
            max_transitions: 1_000_000,
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
        "ordered observations: {} states, {} transitions; 4 events, 2 generations with activation, 3 sequences, 3 positions, lease expiry, close/restart",
        report.states, report.transitions
    );
}

#[test]
fn watched_aggregation_preserves_independent_cuts() {
    use playscale_core::timeline_viewing::work_watched;
    assert!(!work_watched([]));
    for a in [false, true] {
        for b in [false, true] {
            for ma in [None, Some(false), Some(true)] {
                for mb in [None, Some(false), Some(true)] {
                    assert_eq!(
                        work_watched([(a, ma), (b, mb)]),
                        ma.unwrap_or(a) || mb.unwrap_or(b)
                    );
                }
            }
        }
    }
}

#[test]
fn revoked_missing_and_narrowed_owners_cannot_continue() {
    use playscale_core::{
        access::{Device, FileFacts, Grant, Permission, Policy},
        playback_session::{Owner, may_continue},
    };
    let owner = Owner {
        principal: "d".into(),
        profile: "p".into(),
        timeline: "t".into(),
        file: "f".into(),
        file_revision: "r".into(),
    };
    let file = FileFacts {
        library_ids: ["lib".into()].into(),
        revision: "r".into(),
    };
    for revoked in [false, true] {
        for permission in [false, true] {
            for profile in [false, true] {
                for library in [false, true] {
                    let device = Device {
                        id: "d".into(),
                        revision: 1,
                        generation: 1,
                        revoked,
                        credential_expires_at: None,
                        grant: Grant {
                            profile_ids: if profile {
                                ["p".into()].into()
                            } else {
                                ["other".into()].into()
                            },
                            permissions: if permission {
                                [Permission::CatalogRead, Permission::PlaybackRequest].into()
                            } else {
                                [Permission::CatalogRead].into()
                            },
                        },
                        policy: Policy {
                            library_ids: if library {
                                ["lib".into()].into()
                            } else {
                                ["other".into()].into()
                            },
                            ..Policy::default()
                        },
                    };
                    assert_eq!(
                        may_continue(&owner, Some(&device), Some(&file)),
                        !revoked && permission && profile && library
                    );
                }
            }
        }
    }
    assert!(!may_continue(&owner, None, Some(&file)));
}
