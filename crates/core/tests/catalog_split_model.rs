use playscale_core::identity::*;
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    current: Aggregate,
    occurrences: Vec<SplitOccurrence>,
    reviewed: Option<SplitPlan>,
    committed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Preview,
    CopyChanged,
    PinChanged,
    Commit,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Observed,
    Previewed,
    Refused,
    Moved(Vec<SplitOccurrenceMove>),
}
struct Ownership;
impl Model for Ownership {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "catalog.split_occurrence_ownership".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/identity.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        let version = |id: &str, revision: &str| Version {
            id: id.into(),
            origin: Origin::Original,
            equivalence: Equivalence::Declared,
            bindings: vec![Binding {
                file_id: id.into(),
                revision: revision.into(),
                part: 1,
                start_ms: None,
                end_ms: None,
            }],
        };
        Ok(State {
            current: Aggregate {
                work: Work {
                    id: "work".into(),
                    revision: 1,
                    external_ids: BTreeSet::new(),
                },
                editions: vec![Edition {
                    id: "edition".into(),
                    label: "".into(),
                    timelines: vec![Timeline {
                        id: "timeline".into(),
                        versions: vec![version("move", "a"), version("stay", "a")],
                    }],
                }],
            },
            occurrences: [("move", "a"), ("stay", "a"), ("copy", "a")]
                .into_iter()
                .map(|(id, revision)| SplitOccurrence {
                    id: id.into(),
                    edition: "edition".into(),
                    revision: revision.into(),
                })
                .collect(),
            reviewed: None,
            committed: false,
        })
    }
    fn step(&self, before: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = before.clone();
        let effect = match input {
            Input::Preview => {
                let mut n = 0;
                let plan = plan_split(
                    &before.current,
                    &SplitRequest {
                        item: "work".into(),
                        expected_revision: 1,
                        versions: vec!["move".into()],
                        new_title: "split".into(),
                    },
                    &mut || {
                        n += 1;
                        format!("new{n}")
                    },
                )
                .unwrap();
                next.reviewed = Some(plan_split_occurrences(
                    &before.current,
                    plan,
                    before.occurrences.clone(),
                ));
                Effect::Previewed
            }
            Input::CopyChanged => {
                let revision = &mut next.occurrences[2].revision;
                *revision = if revision == "a" { "b" } else { "a" }.into();
                Effect::Observed
            }
            Input::PinChanged => {
                let revision =
                    &mut next.current.editions[0].timelines[0].versions[1].bindings[0].revision;
                *revision = if revision == "a" { "b" } else { "a" }.into();
                Effect::Observed
            }
            Input::Commit => match &before.reviewed {
                Some(plan) => match commit_split_occurrences(
                    plan,
                    Some(&before.current),
                    before.committed,
                    before.occurrences.clone(),
                ) {
                    Ok(plan) => {
                        for movement in &plan.occurrence_moves {
                            next.occurrences
                                .iter_mut()
                                .find(|f| f.id == movement.id)
                                .unwrap()
                                .edition = movement.to_edition.clone();
                        }
                        next.committed = true;
                        Effect::Moved(plan.occurrence_moves)
                    }
                    Err(_) => Effect::Refused,
                },
                None => Effect::Refused,
            },
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, state: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "retained_file_never_moves",
            state.occurrences[1].edition == "edition",
        )])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let mut checks = vec![check(
            "rejected_commit_has_no_effect",
            !matches!(next.outputs, [Effect::Refused]) || before == next.state,
        )];
        if let [Effect::Moved(moves)] = next.outputs {
            let pin = &before.current.editions[0].timelines[0].versions[1].bindings[0].revision;
            let copy_follows = pin != "a" && before.occurrences[2].revision == "a";
            let ids: BTreeSet<_> = moves.iter().map(|m| m.id.as_str()).collect();
            checks.push(check(
                "exact_occurrence_ownership",
                ids == if copy_follows {
                    BTreeSet::from(["copy", "move"])
                } else {
                    BTreeSet::from(["move"])
                },
            ));
            checks.push(check("commit_only_once", !before.committed));
        }
        if matches!(input, Input::Commit)
            && before.reviewed.as_ref().is_some_and(|p| {
                let mut observations = before.occurrences.clone();
                observations.sort();
                p.occurrences != observations
            })
        {
            checks.push(check(
                "stale_inventory_refused",
                matches!(next.outputs, [Effect::Refused]),
            ));
        }
        Ok(checks)
    }
}
fn check(name: &'static str, value: bool) -> Check {
    if value {
        Check::passed(name)
    } else {
        Check::failed(name, "ownership invariant")
    }
}
impl Enumerate for Ownership {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        Ok(if s.committed {
            vec![Input::Commit]
        } else {
            vec![
                Input::Preview,
                Input::CopyChanged,
                Input::PinChanged,
                Input::Commit,
            ]
        })
    }
}
impl ModelCodec for Ownership {
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
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
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
fn exhaustive_review_change_commit_ownership_graph() {
    let report = stateless::explore::enumerate(&Ownership, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "split ownership: {} states / {} transitions; 2 versions, 1 copy, 2 pin/revision values, preview/review-change/duplicate commit",
        report.states, report.transitions
    );
}
