use playscale_core::delivery::{self as core, Delivery, Error, Input, Operation, Pin};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Change {
    basis: u64,
    track: u8,
}
struct Selections;
fn pin(track: u8) -> Pin {
    Pin {
        source_file: "f".into(),
        source_revision: "r".into(),
        tracks: vec![track.to_string()],
        operation: Operation::Original,
        recipe_digest: None,
        clip: None,
        parts: vec![],
        max_bitrate_bps: None,
        adaptive: false,
        tone_map: None,
    }
}
impl Model for Selections {
    type State = Delivery;
    type Input = Change;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.partial_selection_basis".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/delivery.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<Delivery, ModelError> {
        Ok(Delivery::admit("t".into(), pin(0), 0, None, 0).unwrap().0)
    }
    fn step(&self, s: &Delivery, i: &Change) -> Result<Transition<Delivery, bool>, ModelError> {
        let command = Input::Change {
            expected_generation: 1,
            expected_selection: Some(i.basis),
            position_ms: 0,
            replan: Some(("t".into(), Box::new(pin(i.track)))),
            overlap: true,
            now_ms: 0,
        };
        match core::transition(s, &command) {
            Ok((next, _)) => Ok(Transition::accepted(next, vec![true])),
            Err(Error::GenerationConflict) => Ok(Transition::accepted(s.clone(), vec![false])),
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    fn check_state(&self, _: &Delivery) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        s: &Delivery,
        i: &Change,
        n: &TransitionRef<'_, Delivery, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let newest = s.pending.unwrap_or(1);
        let valid = i.basis == newest;
        let correct = n.outputs[0] == valid
            && if valid {
                n.state.pending.is_some_and(|g| {
                    n.state.generations[&g].pin.tracks == vec![i.track.to_string()]
                })
            } else {
                n.state == s
            };
        Ok(vec![if correct {
            Check::passed("stale_partial_cannot_overwrite")
        } else {
            Check::failed(
                "stale_partial_cannot_overwrite",
                "wrong state or acknowledgement",
            )
        }])
    }
}
impl Enumerate for Selections {
    fn inputs(&self, s: &Delivery) -> Result<Vec<Change>, ModelError> {
        Ok(if s.last_generation >= 4 {
            vec![]
        } else {
            (0..=4)
                .flat_map(|basis| (0..2).map(move |track| Change { basis, track }))
                .collect()
        })
    }
}
#[test]
fn concurrent_partial_changes_require_exact_pending_identity_even_for_equal_pins() {
    let r = stateless::explore::enumerate(
        &Selections,
        stateless::explore::SearchConfig {
            max_states: 1000,
            max_transitions: 10000,
            max_depth: 8,
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
        "selection basis: {} states / {} transitions",
        r.states, r.transitions
    );
}

#[test]
fn boxed_replan_preserves_the_serialized_command_contract() {
    let command = Input::Change {
        expected_generation: 1,
        expected_selection: Some(2),
        position_ms: 10,
        replan: Some(("t".into(), Box::new(pin(1)))),
        overlap: true,
        now_ms: 20,
    };
    let document = serde_json::to_value(&command).unwrap();
    assert_eq!(
        document["Change"]["replan"],
        serde_json::json!(["t", pin(1)])
    );
    assert_eq!(serde_json::from_value::<Input>(document).unwrap(), command);
}
