use playscale_core::viewing::{self, Session};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Generate, Model, ModelCodec, ModelError, ModelMetadata, Transition,
    TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct State {
    view: viewing::State,
    session: Session,
    id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Start(String),
    Override(Option<bool>),
    Event {
        id: String,
        sequence: i64,
        ended: bool,
    },
    Legacy,
}
// All positions in this model are finite (0, 10, 100); NaN is not in its domain.
impl Eq for State {}
struct Viewing;
impl Model for Viewing {
    type State = State;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.viewing".into(),
            model_version: 1,
            properties_version: 2,
            codec_version: 1,
            build: concat!(
                include_str!("../src/viewing.rs"),
                "\n",
                include_str!("../src/revision.rs"),
                "\n",
                include_str!("../src/renditions.rs")
            )
            .into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            view: viewing::State {
                revision: 0,
                session_id: None,
                position: 0.0,
                automatic_watched: false,
                manual_watched: None,
            },
            session: Session {
                sequence: 0,
                position: 0.0,
                status: "paused".into(),
            },
            id: String::new(),
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, bool>, ModelError> {
        let mut next = s.clone();
        let accepted = match i {
            Input::Start(id) => match s
                .view
                .start_change(s.view.revision, id.clone(), Some(100.0))
            {
                Ok(view) => {
                    next.view = view.view;
                    next.id = id.clone();
                    next.session = Session {
                        sequence: 0,
                        position: next.view.position,
                        status: "paused".into(),
                    };
                    true
                }
                Err(_) => false,
            },
            Input::Override(watched) => match s.view.override_change(s.view.revision, *watched) {
                Ok(view) => {
                    next.view = view.view;
                    if let Some((_, status)) = view.close_session {
                        next.session.status = status.into();
                    }
                    true
                }
                Err(_) => false,
            },
            Input::Event {
                id,
                sequence,
                ended,
            } => {
                let input = Session {
                    sequence: *sequence,
                    position: if *ended { 100.0 } else { 0.0 },
                    status: if *ended { "ended" } else { "playing" }.into(),
                };
                match viewing::event(&s.view, id, &s.session, &input, Some(100.0)) {
                    Ok(Some((session, view))) => {
                        next.session = session;
                        next.view = view;
                        true
                    }
                    Ok(None) => true,
                    Err(_) => false,
                }
            }
            Input::Legacy => match s.view.legacy(10.0) {
                Ok(view) => {
                    next.view = view;
                    true
                }
                Err(_) => false,
            },
        };
        Ok(Transition::accepted(next, vec![accepted]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "finite_positions",
            s.view.position.is_finite() && s.session.position.is_finite(),
        )])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let forbidden = matches!(i, Input::Legacy) && s.view.session_id.is_some()
            || matches!(i,Input::Event{id,..} if s.view.session_id.as_deref()!=Some(id.as_str()));
        // Independent obligations include required acceptance. Safety-only checks
        // would also pass an implementation that rejects every valid update.
        let (must_accept, changes) = match i {
            Input::Start(_) | Input::Override(_) => (true, true),
            Input::Legacy => (s.view.session_id.is_none(), s.view.session_id.is_none()),
            Input::Event {
                id,
                sequence,
                ended,
            } => {
                let same = *sequence == s.session.sequence
                    && s.session.status == if *ended { "ended" } else { "playing" }
                    && s.session.position == if *ended { 100.0 } else { 0.0 };
                let open = matches!(s.session.status.as_str(), "paused" | "playing");
                let allowed = s.view.session_id.as_deref() == Some(id.as_str())
                    && *sequence > 0
                    && (same || (open && *sequence > s.session.sequence));
                (allowed, allowed && !same)
            }
        };
        let intended = match i {
            Input::Start(id) => {
                n.state.view.session_id.as_ref() == Some(id)
                    && n.state.session.sequence == 0
                    && n.state.session.status == "paused"
                    && n.state.view.manual_watched == s.view.manual_watched
            }
            Input::Override(watched) => {
                n.state.view.manual_watched == *watched
                    && n.state.view.session_id == s.view.session_id
                    && (s.view.session_id.is_none() || n.state.session.status == "invalidated")
            }
            Input::Legacy => !changes || n.state.view.position == 10.0,
            Input::Event {
                sequence, ended, ..
            } => {
                !changes
                    || (n.state.session.sequence == *sequence
                        && n.state.view.position == if *ended { 100.0 } else { 0.0 }
                        && n.state.view.automatic_watched == (s.view.automatic_watched || *ended)
                        && n.state.view.manual_watched == s.view.manual_watched)
            }
        };
        Ok(vec![
            check("valid_events_must_be_accepted", n.outputs == [must_accept]),
            check("accepted_events_apply_intended_change", intended),
            check(
                "exact_revision_change",
                n.state.view.revision == s.view.revision + i64::from(changes),
            ),
            check(
                "retries_and_rejections_preserve_all_state",
                changes || n.state == s,
            ),
            check(
                "rejected_events_are_noops",
                n.outputs != [false] || n.state == s,
            ),
            check(
                "ownership_and_legacy_barrier",
                !forbidden || n.outputs == [false],
            ),
            check(
                "automatic_completion_is_sticky",
                !s.view.automatic_watched || n.state.view.automatic_watched,
            ),
            check(
                "revision_is_monotonic_bounded_step",
                n.state.view.revision >= s.view.revision
                    && n.state.view.revision <= s.view.revision + 1,
            ),
            check(
                "manual_override_has_precedence",
                n.state.view.watched()
                    == n.state
                        .view
                        .manual_watched
                        .unwrap_or(n.state.view.automatic_watched),
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "viewing rule violated")
    }
}
impl Enumerate for Viewing {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Legacy,
            Input::Override(None),
            Input::Override(Some(false)),
            Input::Override(Some(true)),
        ];
        for id in ["first", "second"] {
            inputs.push(Input::Start(id.into()));
            for sequence in 0..=3 {
                for ended in [false, true] {
                    inputs.push(Input::Event {
                        id: id.into(),
                        sequence,
                        ended,
                    });
                }
            }
        }
        Ok(inputs)
    }
}
impl Generate for Viewing {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Viewing {
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
    fn encode_output(&self, v: &bool) -> Result<Vec<u8>, ModelError> {
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
fn viewing_sequences_cover_authority_completion_override_and_legacy_barrier() {
    let report = stateless::explore::fuzz(
        &Viewing,
        stateless::explore::FuzzConfig {
            seed: 20261007,
            cases: 1000,
            max_steps: 100,
            max_transitions: 100000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    println!(
        "Viewing fuzz: {} cases, {} transitions; two session identities, event sequences 0..3",
        report.cases, report.transitions
    );
}

#[test]
fn properties_detect_rejected_valid_updates_and_lost_completion() {
    let initial = Viewing.initial_state().unwrap();
    let start = Input::Start("first".into());
    let rejected = Transition::accepted(initial.clone(), vec![false]);
    let fails = |state: &State, input: &Input, next: &Transition<State, bool>| {
        assert!(
            Viewing
                .check_transition(state, input, &next.as_ref())
                .unwrap()
                .iter()
                .any(|c| matches!(c.status, stateless::CheckStatus::Failed(_)))
        );
    };
    fails(&initial, &start, &rejected);
    let active = Viewing.step(&initial, &start).unwrap().state;
    let ended = Input::Event {
        id: "first".into(),
        sequence: 1,
        ended: true,
    };
    fails(
        &active,
        &ended,
        &Transition::accepted(active.clone(), vec![false]),
    );
    let mut lost = Viewing.step(&active, &ended).unwrap();
    lost.state.view.automatic_watched = false;
    fails(&active, &ended, &lost);
}
