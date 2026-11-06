//! Stored queue receipts do not retain authority after visibility changes.
use playscale_core::organization::{self as core, Entry, EntryDisclosure, Reference};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    visible: u8,
    current: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Visibility(u8),
    Current(u8),
}
struct Disclosure;
fn entries() -> Vec<Entry> {
    [("a", "one"), ("b", "two"), ("c", "one")]
        .into_iter()
        .map(|(entry_id, timeline_id)| Entry {
            entry_id: entry_id.into(),
            timeline_id: timeline_id.into(),
        })
        .collect()
}
fn current(n: u8) -> Option<&'static str> {
    match n {
        0 => None,
        1 => Some("a"),
        2 => Some("b"),
        3 => Some("c"),
        _ => Some("removed"),
    }
}
impl Model for Disclosure {
    type State = State;
    type Input = Input;
    type Output = EntryDisclosure;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.organization_current_disclosure".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/organization.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            visible: 3,
            current: 1,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, EntryDisclosure>, ModelError> {
        let mut next = s.clone();
        match i {
            Input::Visibility(mask) => next.visible = *mask,
            Input::Current(id) => next.current = *id,
        }
        let entries = entries();
        let hidden: BTreeSet<_> = entries
            .iter()
            .filter(|e| next.visible & if e.timeline_id == "one" { 1 } else { 2 } == 0)
            .map(|e| e.entry_id.clone())
            .collect();
        let disclosure = core::disclose_entries(&entries, current(next.current), &hidden);
        Ok(Transition::accepted(next, vec![disclosure]))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &State,
        _: &Input,
        n: &TransitionRef<'_, State, EntryDisclosure>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected_ids: Vec<&str> = match n.state.visible {
            0 => vec![],
            1 => vec!["a", "c"],
            2 => vec!["b"],
            _ => vec!["a", "b", "c"],
        };
        let output = &n.outputs[0];
        let ids: Vec<_> = output.entries.iter().map(|e| e.entry_id.as_str()).collect();
        let expected_current = current(n.state.current).filter(|id| expected_ids.contains(id));
        Ok(vec![
            if ids == expected_ids && output.current.as_deref() == expected_current {
                Check::passed("disclosure.current_visibility_exact_occurrence")
            } else {
                Check::failed(
                    "disclosure.current_visibility_exact_occurrence",
                    "hidden or substituted occurrence",
                )
            },
        ])
    }
}
impl Enumerate for Disclosure {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok((0..4)
            .map(Input::Visibility)
            .chain((0..5).map(Input::Current))
            .collect())
    }
}
#[test]
fn changed_visibility_redacts_historical_occurrences_and_current_pointer() {
    let report = stateless::explore::enumerate(
        &Disclosure,
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
        "organization disclosure: {} states / {} transitions",
        report.states, report.transitions
    );
}
#[test]
fn reference_admission_uses_liveness_and_exact_filter_owner() {
    for (reference, expected) in [
        (Reference::Missing, false),
        (Reference::Work { retired: false }, true),
        (Reference::Work { retired: true }, false),
        (Reference::Timeline, true),
        (
            Reference::SavedFilter {
                profile: "owner".into(),
            },
            true,
        ),
        (
            Reference::SavedFilter {
                profile: "other".into(),
            },
            false,
        ),
    ] {
        assert_eq!(core::reference_admitted("owner", &reference), expected);
    }
}
