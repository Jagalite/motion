//! Timeline viewing authority under competing sessions, manual overrides,
//! exact and conflicting retries, sequence gaps, rebinding to another delivery,
//! boundary positions and unknown durations. The step function only calls the
//! production decisions; the checks state the expected outcomes from a ghost
//! record of what happened, not from those decisions.
use playscale_core::timeline_viewing::{
    self as core, Error, Event, Recorded, Session, Started, Status, View,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

/// d0 has a known 100 s duration; d1's duration is unknown.
const DURATIONS: [Option<u64>; 2] = [Some(100_000), None];
/// Reset, past the 90 % threshold, the exact end, and past end + slack.
const POSITIONS: [u64; 4] = [0, 95_000, 100_000, 102_000];
const STATUSES: [Status; 4] = [
    Status::Playing,
    Status::Paused,
    Status::Ended,
    Status::Stopped,
];
const COMPLETION: f64 = 90.0;
const MAX_SESSIONS: usize = 2;
const MAX_EVENTS: usize = 2;

/// What happened, recorded independently of the production state.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Ghost {
    /// Overrides applied so far.
    overrides: u8,
    /// Per session: overrides at creation, accepted events, ended/stopped,
    /// current delivery index.
    sessions: Vec<(u8, u8, bool, u8)>,
    /// The last accepted progress position (None: none yet since a reset).
    position: u64,
    completed: bool,
    manual: Option<bool>,
    revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    view: View,
    sessions: Vec<Session>,
    /// Acknowledged events per session, in sequence order.
    receipts: Vec<Vec<Event>>,
    ghost: Ghost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Sequence {
    Next,
    Repeat,
    Gap,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    Start {
        current: bool,
        delivery: u8,
    },
    Event {
        session: u8,
        sequence: Sequence,
        /// Reuse the identity of the session's last acknowledged event.
        reuse_id: bool,
        position: u8,
        status: u8,
    },
    /// Resend acknowledged event `index` of `session` exactly.
    Retry {
        session: u8,
        index: u8,
    },
    Override {
        current: bool,
        manual: Option<bool>,
        reset: bool,
    },
    Rebind {
        session: u8,
        current: bool,
        delivery: u8,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Output {
    Started { superseded: Option<String> },
    Accepted,
    Duplicate,
    Overridden { fenced: Option<String> },
    Rebound,
    Refused(Error),
}

fn session_id(i: usize) -> String {
    format!("s{i}")
}

struct Viewing;
impl Model for Viewing {
    type State = State;
    type Input = Input;
    type Output = Output;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.timeline-viewing".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/timeline_viewing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            view: View::default(),
            sessions: vec![],
            receipts: vec![],
            ghost: Ghost {
                overrides: 0,
                sessions: vec![],
                position: 0,
                completed: false,
                manual: None,
                revision: 0,
            },
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Output>, ModelError> {
        let mut n = s.clone();
        let out = match input {
            Input::Start { current, delivery } => {
                let expected = if *current {
                    s.view.revision
                } else {
                    s.view.revision + 1
                };
                let id = session_id(s.sessions.len());
                match core::start(
                    &s.view,
                    expected,
                    id,
                    format!("d{delivery}"),
                    DURATIONS[*delivery as usize],
                ) {
                    Ok(Started {
                        view,
                        session,
                        superseded,
                    }) => {
                        if let Some(old) = &superseded {
                            let i = n.sessions.iter().position(|x| &x.id == old).unwrap();
                            n.sessions[i] = core::supersede(&n.sessions[i]).unwrap();
                        }
                        n.view = view;
                        n.sessions.push(session);
                        n.receipts.push(vec![]);
                        n.ghost.revision += 1;
                        n.ghost
                            .sessions
                            .push((s.ghost.overrides, 0, false, *delivery));
                        if let Some(d) = DURATIONS[*delivery as usize] {
                            n.ghost.position = n.ghost.position.min(d);
                        }
                        Output::Started { superseded }
                    }
                    Err(e) => Output::Refused(e),
                }
            }
            Input::Event {
                session,
                sequence,
                reuse_id,
                position,
                status,
            } => {
                let i = *session as usize;
                let receipts = &s.receipts[i];
                let last = s.sessions[i].sequence;
                let event = Event {
                    id: if *reuse_id {
                        receipts.last().unwrap().id.clone()
                    } else {
                        format!("e{i}-{}", receipts.len())
                    },
                    sequence: match sequence {
                        Sequence::Next => last + 1,
                        Sequence::Repeat => last,
                        Sequence::Gap => last + 2,
                    },
                    generation: 1,
                    position_ms: POSITIONS[*position as usize],
                    status: STATUSES[*status as usize],
                };
                apply(&mut n, i, &event)
            }
            Input::Retry { session, index } => {
                let i = *session as usize;
                let event = s.receipts[i][*index as usize].clone();
                apply(&mut n, i, &event)
            }
            Input::Override {
                current,
                manual,
                reset,
            } => {
                let expected = if *current {
                    s.view.revision
                } else {
                    s.view.revision + 1
                };
                match core::override_watched(&s.view, expected, *manual, *reset) {
                    Ok(o) => {
                        if let Some(old) = &o.fenced {
                            let i = n.sessions.iter().position(|x| &x.id == old).unwrap();
                            n.sessions[i] = core::supersede(&n.sessions[i]).unwrap();
                        }
                        n.view = o.view;
                        n.ghost.overrides += 1;
                        n.ghost.manual = *manual;
                        n.ghost.revision += 1;
                        if *reset {
                            n.ghost.position = 0;
                        }
                        Output::Overridden { fenced: o.fenced }
                    }
                    Err(e) => Output::Refused(e),
                }
            }
            Input::Rebind {
                session,
                current,
                delivery,
            } => {
                let i = *session as usize;
                let x = &s.sessions[i];
                let expected = if *current { x.revision } else { x.revision + 1 };
                match core::rebind(
                    &s.view,
                    x,
                    expected,
                    format!("d{delivery}"),
                    DURATIONS[*delivery as usize],
                ) {
                    Ok(next) => {
                        n.sessions[i] = next;
                        n.ghost.sessions[i].3 = *delivery;
                        Output::Rebound
                    }
                    Err(e) => Output::Refused(e),
                }
            }
        };
        Ok(Transition::accepted(n, vec![out]))
    }

    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let authorities = s
            .sessions
            .iter()
            .filter(|x| core::authoritative(&s.view, x))
            .count();
        let g = &s.ghost;
        Ok(vec![
            check("at_most_one_session_holds_the_authority", authorities <= 1),
            check(
                "the_view_names_only_the_newest_session_and_only_without_a_later_override",
                match &s.view.session {
                    None => g.sessions.last().is_none_or(|(at, ..)| *at < g.overrides),
                    Some(id) => {
                        *id == session_id(s.sessions.len() - 1)
                            && g.sessions.last().unwrap().0 == g.overrides
                    }
                },
            ),
            check(
                "position_is_the_last_accepted_progress",
                s.view.position_ms == g.position,
            ),
            check(
                "watched_is_the_manual_override_else_sticky_completion",
                s.view.watched() == g.manual.unwrap_or(g.completed)
                    && s.view.automatic_watched == g.completed,
            ),
            check(
                "viewing_revision_counts_accepted_changes",
                s.view.revision == g.revision,
            ),
            check(
                "manual_epoch_counts_overrides",
                s.view.manual_epoch == u64::from(g.overrides),
            ),
            check(
                "session_sequences_count_their_accepted_events",
                s.sessions.iter().zip(&g.sessions).zip(&s.receipts).all(
                    |((x, (_, accepted, ..)), r)| {
                        x.sequence == u64::from(*accepted) && r.len() == *accepted as usize
                    },
                ),
            ),
            check(
                "event_identities_are_unique_per_session",
                s.receipts.iter().all(|r| {
                    r.iter()
                        .map(|e| &e.id)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == r.len()
                }),
            ),
        ])
    }

    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        t: &TransitionRef<'_, State, Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let g = &s.ghost;
        // A session may write while it is the newest one, no override came
        // after it, and it has not ended or stopped.
        let may_write = |i: usize| {
            let (at, _, closed, _) = g.sessions[i];
            i + 1 == g.sessions.len() && at == g.overrides && !closed
        };
        let mut checks = vec![];
        match i {
            Input::Start { current, .. } => checks.push(check(
                "a_session_starts_exactly_from_the_current_revision_and_supersedes_the_holder",
                match &t.outputs[0] {
                    Output::Started { superseded } => {
                        *current
                            && *superseded
                                == s.sessions
                                    .last()
                                    .filter(|_| g.sessions.last().unwrap().0 == g.overrides)
                                    .map(|x| x.id.clone())
                    }
                    Output::Refused(Error::RevisionConflict) => !current,
                    _ => false,
                },
            )),
            Input::Event {
                session,
                sequence,
                reuse_id,
                position,
                status,
            } => {
                let i = *session as usize;
                let p = POSITIONS[*position as usize];
                let status = STATUSES[*status as usize];
                let valid = DURATIONS[g.sessions[i].3 as usize]
                    .is_none_or(|d| p <= d + 1_000 && (status != Status::Ended || p + 1_000 >= d));
                let last = s.receipts[i].last();
                // The same identity, sequence and content as the last receipt.
                let exact = *reuse_id
                    && *sequence == Sequence::Repeat
                    && last.is_some_and(|e| e.position_ms == p && e.status == status);
                let reused = *reuse_id || (*sequence == Sequence::Repeat && last.is_some());
                let expected = match () {
                    _ if *sequence == Sequence::Repeat && last.is_none() => {
                        // Sequence zero is never a valid event.
                        Output::Refused(Error::InvalidEvent)
                    }
                    _ if exact => Output::Duplicate,
                    _ if reused => Output::Refused(Error::EventConflict),
                    _ if !may_write(i) => Output::Refused(
                        if i + 1 == g.sessions.len() && g.sessions[i].0 == g.overrides {
                            Error::Closed
                        } else {
                            Error::Superseded
                        },
                    ),
                    _ if *sequence == Sequence::Gap => Output::Refused(Error::SequenceGap),
                    _ if !valid => Output::Refused(Error::InvalidPosition),
                    _ => Output::Accepted,
                };
                checks.push(check(
                    "events_are_accepted_exactly_when_fresh_authoritative_open_consecutive_and_in_range",
                    t.outputs[0] == expected,
                ));
                if expected == Output::Duplicate {
                    checks.push(check("a_duplicate_changes_nothing", t.state == s));
                }
            }
            Input::Retry { .. } => checks.push(check(
                "an_exact_retry_is_always_acknowledged_without_change",
                t.outputs[0] == Output::Duplicate && t.state == s,
            )),
            Input::Override { current, .. } => checks.push(check(
                "overrides_need_the_current_revision_and_fence_the_holder",
                match &t.outputs[0] {
                    Output::Overridden { fenced } => {
                        *current && *fenced == s.view.session && t.state.view.session.is_none()
                    }
                    Output::Refused(Error::RevisionConflict) => !current,
                    _ => false,
                },
            )),
            Input::Rebind {
                session, current, ..
            } => {
                let i = *session as usize;
                checks.push(check(
                    "rebinding_only_an_open_authoritative_session_at_its_revision_keeping_its_progress",
                    match &t.outputs[0] {
                        Output::Rebound => {
                            *current
                                && may_write(i)
                                && t.state.sessions[i].sequence == s.sessions[i].sequence
                                && t.state.view == s.view
                        }
                        Output::Refused(Error::RevisionConflict) => !current,
                        Output::Refused(_) => *current && !may_write(i),
                        _ => false,
                    },
                ));
            }
        }
        Ok(checks)
    }
}

/// Apply an event to session `i` and record its acknowledgement.
fn apply(n: &mut State, i: usize, event: &Event) -> Output {
    let prior: Vec<Event> = n.receipts[i]
        .iter()
        .filter(|p| p.sequence == event.sequence || p.id == event.id)
        .cloned()
        .collect();
    match core::record(&n.view, &n.sessions[i], event, &prior, COMPLETION) {
        Ok(Recorded::Duplicate) => Output::Duplicate,
        Ok(Recorded::Accepted { view, session }) => {
            n.view = view;
            n.sessions[i] = session;
            n.receipts[i].push(event.clone());
            let g = &mut n.ghost;
            g.revision += 1;
            g.position = event.position_ms;
            g.sessions[i].1 += 1;
            g.sessions[i].2 = matches!(event.status, Status::Ended | Status::Stopped);
            let duration = DURATIONS[g.sessions[i].3 as usize];
            g.completed |= event.status == Status::Ended
                || duration.is_some_and(|d| event.position_ms * 100 >= d * 90);
            Output::Accepted
        }
        Err(e) => Output::Refused(e),
    }
}

fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "timeline viewing contract violated")
    }
}

impl Enumerate for Viewing {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![];
        if s.sessions.len() < MAX_SESSIONS {
            for current in [true, false] {
                for delivery in 0..2 {
                    inputs.push(Input::Start { current, delivery });
                }
            }
        }
        for (i, receipts) in s.receipts.iter().enumerate() {
            let session = i as u8;
            if receipts.len() < MAX_EVENTS {
                for sequence in [Sequence::Next, Sequence::Repeat, Sequence::Gap] {
                    for reuse_id in [false, true] {
                        if reuse_id && receipts.is_empty() {
                            continue;
                        }
                        for position in 0..POSITIONS.len() as u8 {
                            for status in 0..STATUSES.len() as u8 {
                                inputs.push(Input::Event {
                                    session,
                                    sequence,
                                    reuse_id,
                                    position,
                                    status,
                                });
                            }
                        }
                    }
                }
            }
            for index in 0..receipts.len() as u8 {
                inputs.push(Input::Retry { session, index });
            }
            if s.ghost.sessions[i].3 == 0 {
                for current in [true, false] {
                    inputs.push(Input::Rebind {
                        session,
                        current,
                        delivery: 1,
                    });
                }
            }
        }
        if s.ghost.overrides < 1 {
            for current in [true, false] {
                for manual in [None, Some(true), Some(false)] {
                    for reset in [false, true] {
                        inputs.push(Input::Override {
                            current,
                            manual,
                            reset,
                        });
                    }
                }
            }
        }
        Ok(inputs)
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
fn timeline_viewing_authority_survives_supersession_overrides_retries_and_rebinding() {
    let report = stateless::explore::enumerate(
        &Viewing,
        stateless::explore::SearchConfig {
            max_states: 3_000_000,
            max_transitions: 200_000_000,
            max_depth: 100,
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
        "timeline-viewing model: {} states, {} transitions; two sessions on two deliveries \
         (known and unknown duration), two events each, one override, one rebind, exact \
         retries, reused identities, repeated and skipped sequences, boundary positions",
        report.states, report.transitions
    );
}
