//! Whole-resource metadata generations across source changes and rollback.
use playscale_core::metadata;
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    inputs: [u8; 3],
    generation: u64,
    history: u8,
    reviewed: Option<(u8, u64, [u8; 3], bool)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Change(usize, u8),
    Review,
    ReviewOtherItem,
    CommitReviewed(usize, u8),
    Rollback,
    Restore,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Changed(u64),
    Unchanged,
    Stale,
    Reviewed(u64),
    RolledBack,
    Restored,
}
struct Metadata;
impl Model for Metadata {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.metadata_generation".into(),
            model_version: 3,
            properties_version: 3,
            codec_version: 3,
            build: include_str!("../src/metadata.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            inputs: [0; 3],
            generation: 1,
            history: 0,
            reviewed: None,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match *i {
            Input::Review => {
                next.reviewed = Some((s.history, s.generation, s.inputs, false));
                Effect::Reviewed(s.generation)
            }
            Input::ReviewOtherItem => {
                next.reviewed = Some((s.history, s.generation, s.inputs, true));
                Effect::Reviewed(s.generation)
            }
            Input::Rollback => Effect::RolledBack,
            Input::Restore => {
                next.history = 1;
                next.generation = 1;
                next.inputs = [0; 3];
                Effect::Restored
            }
            Input::CommitReviewed(_, _)
                if s.reviewed
                    .is_none_or(|(history, generation, _, other_item)| {
                        !metadata::reviewed_item_revision(
                            &metadata::resource_revision(
                                "server",
                                &s.history.to_string(),
                                "item",
                                s.generation,
                            )
                            .unwrap(),
                            &metadata::resource_revision(
                                "server",
                                &history.to_string(),
                                if other_item { "other" } else { "item" },
                                generation,
                            )
                            .unwrap(),
                        )
                    }) =>
            {
                Effect::Stale
            }
            Input::Change(index, value) | Input::CommitReviewed(index, value) => {
                let changed = s.inputs[index] != value;
                next.generation = metadata::item_revision(s.generation, changed).unwrap();
                next.inputs[index] = value;
                if changed {
                    Effect::Changed(next.generation)
                } else {
                    Effect::Unchanged
                }
            }
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let no_reuse = s
            .reviewed
            .is_none_or(|(history, revision, inputs, other_item)| {
                inputs == s.inputs || other_item || history != s.history || revision != s.generation
            });
        Ok(vec![check(
            "metadata.changed_inputs_never_reuse_reviewed_generation",
            no_reuse,
        )])
    }
    fn check_transition(
        &self,
        b: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let changed = n.state.inputs != b.inputs;
        let exact =
            matches!(i, Input::Restore) || n.state.generation == b.generation + u64::from(changed);
        let must_refuse = matches!(i, Input::CommitReviewed(..))
            && b.reviewed
                .is_none_or(|(history, generation, _, other_item)| {
                    other_item || history != b.history || generation != b.generation
                });
        let refused = !must_refuse || (matches!(n.outputs, [Effect::Stale]) && n.state == b);
        let exact_effect = match *i {
            Input::Change(index, value) | Input::CommitReviewed(index, value) if !must_refuse => {
                let mut inputs = b.inputs;
                inputs[index] = value;
                let effect = if inputs == b.inputs {
                    Effect::Unchanged
                } else {
                    Effect::Changed(b.generation + 1)
                };
                n.state.inputs == inputs && n.outputs == [effect]
            }
            Input::Restore => {
                n.state.history == 1
                    && n.state.inputs == [0; 3]
                    && n.state.generation == 1
                    && n.outputs == [Effect::Restored]
            }
            _ => true,
        };
        Ok(vec![
            check("metadata.generation_advances_exactly_on_change", exact),
            check("metadata.stale_commit_is_noop", refused),
            check("metadata.exact_input_and_effect", exact_effect),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "metadata generation invariant")
    }
}
impl Enumerate for Metadata {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![Input::Review, Input::ReviewOtherItem, Input::Rollback];
        if s.history == 0 {
            inputs.push(Input::Restore);
        }
        if s.generation < 7 {
            for index in 0..3 {
                for value in 0..3 {
                    inputs.push(Input::Change(index, value));
                    inputs.push(Input::CommitReviewed(index, value));
                }
            }
        }
        Ok(inputs)
    }
}
impl ModelCodec for Metadata {
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
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
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
fn metadata_generations_do_not_repeat_after_withdrawal_or_recreation() {
    let report = stateless::explore::enumerate(
        &Metadata,
        stateless::explore::SearchConfig {
            max_states: 100000,
            max_transitions: 2000000,
            max_depth: 30,
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
        "Metadata generations: {} states, {} transitions; three inputs, values absent/A/B, generation <=7, one restore, cross-item/stale review and rollback",
        report.states, report.transitions
    );
}
