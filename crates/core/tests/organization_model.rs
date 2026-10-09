//! Stateless model of play-queue navigation and edits over the production
//! `organization` decisions, plus filter/collection validation cases.
use playscale_core::organization::{
    CollectionKind, Entry, Field, Operator, OrganizationError, Queue, Repeat, Step, Term, Value,
    advance_queue, edit_queue, play_order, select, step, validate_collection, validate_filter,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::BTreeSet;

const ALL: [&str; 4] = ["a", "b", "c", "d"];

fn entries(mask: u8) -> Vec<Entry> {
    ALL.iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, id)| Entry {
            entry_id: id.to_string(),
            timeline_id: format!("t-{id}"),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Move {
        step: Step,
        stale: bool,
    },
    Edit {
        mask: u8,
        repeat: Repeat,
        seed: Option<u64>,
    },
    Select(String),
}

struct Queues;
impl Model for Queues {
    type State = Queue;
    type Input = Input;
    type Output = Result<Option<String>, OrganizationError>;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.queue".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/organization.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<Queue, ModelError> {
        Ok(Queue {
            revision: 1,
            entries: entries(0b0111),
            current: None,
            repeat: Repeat::Off,
            shuffle_seed: None,
        })
    }
    fn step(
        &self,
        q: &Queue,
        input: &Input,
    ) -> Result<Transition<Queue, Self::Output>, ModelError> {
        let result = match input {
            Input::Move { step, stale } => advance_queue(q, q.revision - u64::from(*stale), *step),
            Input::Edit { mask, repeat, seed } => {
                edit_queue(q, q.revision, entries(*mask), *repeat, *seed)
            }
            Input::Select(id) => select(q, q.revision, id),
        };
        Ok(match result {
            Ok(next) => {
                let current = next.current.clone();
                Transition::accepted(next, vec![Ok(current)])
            }
            Err(e) => Transition::accepted(q.clone(), vec![Err(e)]),
        })
    }
    fn check_state(&self, q: &Queue) -> Result<Vec<Check>, ModelError> {
        let order = play_order(q);
        let ids: BTreeSet<&String> = q.entries.iter().map(|e| &e.entry_id).collect();
        Ok(vec![
            check(
                "queue.current_is_member",
                q.current.as_ref().is_none_or(|c| ids.contains(c)),
            ),
            check(
                "queue.order_is_permutation",
                order.len() == q.entries.len() && order.iter().collect::<BTreeSet<_>>() == ids,
            ),
            check("queue.order_reproducible", order == play_order(&q.clone())),
        ])
    }
    fn check_transition(
        &self,
        before: &Queue,
        input: &Input,
        next: &TransitionRef<'_, Queue, Self::Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let order = play_order(before);
        let position = before
            .current
            .as_ref()
            .and_then(|c| order.iter().position(|e| e == c));
        let accepted = matches!(next.outputs, [Ok(_)]);
        let expected_move = match input {
            Input::Move { stale: true, .. } => !accepted && after == before,
            Input::Move { step: s, .. } => {
                let expected = match (position, s, before.repeat) {
                    (_, _, _) if order.is_empty() => None,
                    (Some(p), Step::Ended, Repeat::One) => Some(order[p].clone()),
                    (None, Step::Previous, _) => order.last().cloned(),
                    (None, _, _) => order.first().cloned(),
                    (Some(p), Step::Next | Step::Ended, r) => {
                        if p + 1 < order.len() {
                            Some(order[p + 1].clone())
                        } else if r == Repeat::Off {
                            None
                        } else {
                            Some(order[0].clone())
                        }
                    }
                    (Some(p), Step::Previous, r) => {
                        if p > 0 {
                            Some(order[p - 1].clone())
                        } else if r == Repeat::Off {
                            None
                        } else {
                            order.last().cloned()
                        }
                    }
                };
                accepted
                    && after.current == expected
                    && after.entries == before.entries
                    && after.revision == before.revision + 1
            }
            Input::Edit { .. } => {
                let kept = before
                    .current
                    .as_ref()
                    .filter(|c| after.entries.iter().any(|e| &e.entry_id == *c));
                accepted
                    && (kept.is_none() || after.current.as_ref() == kept)
                    // A replacement current entry must have followed the old one.
                    && (kept.is_some()
                        || after.current.as_ref().is_none_or(|c| {
                            let at = order.iter().position(|e| e == c);
                            at.zip(position).is_some_and(|(a, p)| a > p)
                        }))
            }
            Input::Select(id) => {
                let member = before.entries.iter().any(|e| &e.entry_id == id);
                accepted == member && (!member || after.current.as_ref() == Some(id))
            }
        };
        Ok(vec![
            check("queue.exact_transition", expected_move),
            check(
                "queue.rejection_changes_nothing",
                accepted || after == before,
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "queue invariant violated")
    }
}
impl Enumerate for Queues {
    fn inputs(&self, q: &Queue) -> Result<Vec<Input>, ModelError> {
        // Revisions are bounded at 6 so the graph is finite.
        if q.revision >= 6 {
            return Ok(vec![]);
        }
        let mut inputs = Vec::new();
        for step in [Step::Next, Step::Previous, Step::Ended] {
            for stale in [false, true] {
                inputs.push(Input::Move { step, stale });
            }
        }
        for mask in [0b0000, 0b0011, 0b0110, 0b1111] {
            for repeat in [Repeat::Off, Repeat::One, Repeat::All] {
                for seed in [None, Some(7)] {
                    inputs.push(Input::Edit { mask, repeat, seed });
                }
            }
        }
        for id in ALL {
            inputs.push(Input::Select(id.into()));
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Queues {
    fn generate(&self, s: &Queue, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Queues {
    fn encode_state(&self, v: &Queue) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<Queue, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Self::Output) -> Result<Vec<u8>, ModelError> {
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
fn bounded_queue_graph_checks_production_navigation() {
    let report = stateless::explore::enumerate(&Queues, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Stateless: {} states, {} edges; 4 entries, revision <=6, repeat off/one/all, seed none/7",
        report.states, report.transitions
    );
}

#[test]
fn shuffle_is_a_reproducible_permutation() {
    let queue = |seed| Queue {
        revision: 1,
        entries: entries(0b1111),
        current: None,
        repeat: Repeat::All,
        shuffle_seed: seed,
    };
    let shuffled = play_order(&queue(Some(42)));
    assert_eq!(shuffled, play_order(&queue(Some(42))));
    assert_ne!(shuffled, play_order(&queue(None)));
    // Pinned so clients and servers agree across releases.
    assert_eq!(shuffled, ["c", "a", "d", "b"]);
    let mut q = queue(Some(42));
    q.current = Some("b".into());
    assert_eq!(
        step(&q, Step::Next).as_deref(),
        Some("c"),
        "wraps in play order"
    );
}

#[test]
fn filters_are_typed_and_collections_exclusive() {
    let term = |field, operator, value| Term {
        field,
        operator,
        value,
    };
    assert_eq!(
        validate_filter(&[
            term(Field::Kind, Operator::Eq, Value::Text("movie".into())),
            term(Field::Year, Operator::Gte, Value::Integer(1990)),
            term(
                Field::Title,
                Operator::Contains,
                Value::Text("matrix".into())
            ),
            term(Field::Watched, Operator::Eq, Value::Boolean(false)),
        ]),
        Ok(())
    );
    for bad in [
        term(
            Field::Kind,
            Operator::Eq,
            Value::Text("'; DROP TABLE items".into()),
        ),
        term(Field::Year, Operator::Contains, Value::Integer(1990)),
        term(Field::Year, Operator::Eq, Value::Text("1990".into())),
        term(Field::Watched, Operator::Gte, Value::Boolean(true)),
        term(Field::Title, Operator::Eq, Value::Text("  ".into())),
    ] {
        assert_eq!(
            validate_filter(std::slice::from_ref(&bad)),
            Err(OrganizationError::InvalidTerm(0)),
            "{bad:?}"
        );
    }
    let many = vec![term(Field::Watched, Operator::Eq, Value::Boolean(true)); 33];
    assert_eq!(validate_filter(&many), Err(OrganizationError::TooManyTerms));
    let filter = "f".to_string();
    assert_eq!(
        validate_collection("Manual", CollectionKind::Manual, &[], Some(&filter)),
        Err(OrganizationError::ManualCollectionHasFilter)
    );
    assert_eq!(
        validate_collection("Smart", CollectionKind::Smart, &["x".into()], Some(&filter)),
        Err(OrganizationError::SmartCollectionHasMembers)
    );
    assert_eq!(
        validate_collection("Smart", CollectionKind::Smart, &[], None),
        Err(OrganizationError::SmartCollectionNeedsFilter)
    );
    assert_eq!(
        validate_collection(
            "Dup",
            CollectionKind::Manual,
            &["x".into(), "x".into()],
            None
        ),
        Err(OrganizationError::DuplicateMember("x".into()))
    );
    // Wire shape: a term is exactly {field, operator, value}.
    let parsed: Term =
        serde_json::from_str(r#"{"field":"year","operator":"lte","value":2000}"#).unwrap();
    assert_eq!(
        parsed,
        term(Field::Year, Operator::Lte, Value::Integer(2000))
    );
    assert!(serde_json::from_str::<Term>(r#"{"field":"sql","operator":"eq","value":1}"#).is_err());
}
