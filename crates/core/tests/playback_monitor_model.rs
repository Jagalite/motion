//! Playback monitoring follows the production delivery reducer: which
//! playheads it accepted, which step ended playback and why, viewing rows in
//! order (read some time after the player writes them), stall episodes, and
//! history writes acknowledged only on commit.
use playscale_core::delivery::{self as core, Delivery, Input, Operation, Pin};
use playscale_core::playback_monitor::{
    self as monitor, EndReason, Observation, Persisted, STALL_MS, Snapshot, Viewing, ViewingRow,
    Watch,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    delivery: Delivery,
    watch: Watch,
    now: u64,
    /// Reasons the monitor recorded, in order.
    ended: Vec<EndReason>,
    persisted: Persisted,
    /// What the history row holds (the database, as committed).
    row: Option<Snapshot>,
    /// Viewing rows as the player wrote them, oldest first (the last two).
    written: Vec<(ViewingRow, Viewing)>,
    truth: Truth,
    steps: u8,
}

/// What really happened, kept by the model alone from the player's writes
/// (at the time they happened) and the reducer's accepted playheads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Truth {
    playing_since: Option<u64>,
    position: Option<u64>,
    advanced_at: Option<u64>,
    stalled: bool,
    stalls: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Step {
    /// The client reports a playhead (sequence, position).
    Observe(u64, u64),
    Fail,
    Close {
        stopping: bool,
    },
    /// Time passes; the worker ticks the reducer and the monitor.
    Wait(u64),
    /// The player records a status in its viewing session.
    Play(Viewing),
    /// The player starts a new viewing session, playing.
    Supersede,
    /// The monitor reads the newest written row, or the one before it.
    Read {
        stale: bool,
    },
    /// A history flush whose transaction commits or fails.
    Flush {
        commits: bool,
    },
}

struct Monitoring;

fn snapshot(w: &Watch) -> Snapshot {
    Snapshot {
        bytes_sent: 0,
        peak_bps: 0,
        watched_ms: w.watched_ms,
        stalls: w.stalls,
        position_ms: w.position_ms,
        last_activity_ms: w.observed_at.unwrap_or(0),
        status: format!("{:?}/{}", w.viewing, w.stalled),
        mode: "direct".into(),
    }
}

impl Monitoring {
    /// One reducer step and what the monitor takes from it, as the server's
    /// transition funnel does.
    fn reduce(&self, n: &mut State, input: Input, stopping: bool) {
        if let Ok((next, _)) = core::transition(&n.delivery, &input) {
            if let Some((generation, sequence, position_ms)) =
                monitor::accepted_playhead(&n.delivery, &next)
            {
                n.watch.observe(Observation::Playhead {
                    generation,
                    sequence,
                    position_ms,
                    now_ms: n.now,
                });
                if n.truth.position.is_some_and(|p| position_ms > p) {
                    n.truth.advanced_at = Some(n.now);
                }
                n.truth.position = Some(position_ms);
            }
            if let Some(reason) = monitor::end_reason(&n.delivery, &input, &next, stopping) {
                n.ended.push(reason);
            }
            n.delivery = next;
        }
    }

    fn write(&self, n: &mut State, id: &str, status: Viewing) {
        let revision = n
            .written
            .iter()
            .rev()
            .find(|(r, _)| r.id == id)
            .map_or(1, |(r, _)| r.revision + 1);
        n.written.push((
            ViewingRow {
                id: id.into(),
                revision,
                updated_ms: n.now,
            },
            status,
        ));
        if n.written.len() > 2 {
            n.written.remove(0);
        }
        let was_playing = n.truth.playing_since.is_some();
        match status {
            Viewing::Playing if !was_playing => n.truth.playing_since = Some(n.now),
            Viewing::Playing => {}
            _ => {
                n.truth.playing_since = None;
                n.truth.stalled = false;
            }
        }
    }
}

impl Model for Monitoring {
    type State = State;
    type Input = Step;
    type Output = ();
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.playback-monitor".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: [
                include_str!("../src/playback_monitor.rs"),
                include_str!("../src/delivery.rs"),
            ]
            .concat(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        let pin = Pin {
            source_file: "f".into(),
            source_revision: "r".into(),
            tracks: vec![],
            operation: Operation::Original,
            recipe_digest: None,
        };
        let (delivery, _) = Delivery::admit("t".into(), pin, 0, Some(1000), 0)
            .map_err(|e| ModelError::new(format!("{e:?}")))?;
        Ok(State {
            delivery,
            watch: Watch::new(Some(0)),
            now: 0,
            ended: vec![],
            persisted: Persisted::default(),
            row: None,
            written: vec![],
            truth: Truth::default(),
            steps: 0,
        })
    }
    fn step(&self, s: &State, step: &Step) -> Result<Transition<State, ()>, ModelError> {
        let mut n = s.clone();
        n.steps += 1;
        match step {
            Step::Observe(sequence, position_ms) => self.reduce(
                &mut n,
                Input::ObservePlayhead {
                    generation: 1,
                    sequence: *sequence,
                    position_ms: *position_ms,
                    now_ms: s.now,
                },
                false,
            ),
            Step::Fail => self.reduce(&mut n, Input::Failed { generation: 1 }, false),
            Step::Close { stopping } => self.reduce(&mut n, Input::Close, *stopping),
            Step::Wait(ms) => {
                n.now += ms;
                let now_ms = n.now;
                self.reduce(&mut n, Input::Tick { now_ms }, false);
                n.watch.observe(Observation::Tick { now_ms });
                let threshold = n
                    .truth
                    .advanced_at
                    .max(n.truth.playing_since)
                    .map(|at| at + STALL_MS);
                let stalled =
                    n.truth.playing_since.is_some() && threshold.is_some_and(|at| now_ms >= at);
                if stalled && !n.truth.stalled {
                    n.truth.stalls += 1;
                }
                n.truth.stalled = stalled;
            }
            Step::Play(status) => {
                let id = s
                    .written
                    .last()
                    .map_or("a".to_owned(), |(r, _)| r.id.clone());
                self.write(&mut n, &id, *status);
            }
            Step::Supersede => self.write(&mut n, "b", Viewing::Playing),
            Step::Read { stale } => {
                let index = n.written.len().checked_sub(if *stale { 2 } else { 1 });
                if let Some((row, status)) = index.and_then(|i| n.written.get(i)).cloned() {
                    n.watch.observe(Observation::Viewing {
                        row,
                        status,
                        position_ms: 0,
                        now_ms: s.now,
                    });
                }
            }
            Step::Flush { commits } => {
                if let Some(due) = n.persisted.due(&snapshot(&n.watch))
                    && *commits
                {
                    n.row = Some(due.snapshot.clone());
                    n.persisted.acknowledge(due.snapshot);
                }
            }
        }
        Ok(Transition::accepted(n, vec![]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let current = snapshot(&s.watch);
        // The monitor has read the player's newest row.
        let synced = s.written.last().map(|(r, _)| r) == s.watch.row.as_ref();
        let checks = [
            (
                "one_end_reason_per_delivery",
                s.ended.len() <= 1 && s.ended.is_empty() != s.delivery.status.fenced(),
            ),
            (
                "failure_is_never_relabelled",
                s.delivery.status != core::Status::Failed || s.ended == [EndReason::Error],
            ),
            (
                "nothing_due_means_the_row_is_current",
                s.persisted.due(&current).is_some() || s.row.as_ref() == Some(&current),
            ),
            (
                "stalled_only_while_playing",
                !s.watch.stalled || s.watch.viewing == Some(Viewing::Playing),
            ),
            (
                "no_false_stalls_once_rows_are_read",
                !synced
                    || (s.watch.stalls <= s.truth.stalls && (!s.watch.stalled || s.truth.stalled)),
            ),
        ];
        Ok(checks
            .into_iter()
            .map(|(id, ok)| {
                if ok {
                    Check::passed(id)
                } else {
                    Check::failed(id, format!("{s:?}"))
                }
            })
            .collect())
    }
    fn check_transition(
        &self,
        s: &State,
        step: &Step,
        t: &TransitionRef<'_, State, ()>,
    ) -> Result<Vec<Check>, ModelError> {
        let n = t.state;
        let accepted = n.delivery.observation != s.delivery.observation;
        let read_row = |stale: bool| {
            s.written
                .len()
                .checked_sub(if stale { 2 } else { 1 })
                .and_then(|i| s.written.get(i))
        };
        let checks = [
            (
                "playheads_follow_the_reducer",
                match step {
                    Step::Observe(sequence, position) if accepted => {
                        n.watch.playhead == Some((1, *sequence))
                            && n.watch.position_ms == Some(*position)
                    }
                    Step::Observe(..) => n.watch == s.watch,
                    _ => n.watch.playhead == s.watch.playhead,
                },
            ),
            (
                "viewing_rows_never_rewind_reducer_playheads",
                !matches!(step, Step::Read { .. })
                    || s.watch.playhead.is_none()
                    || n.watch.position_ms == s.watch.position_ms,
            ),
            (
                "stale_or_repeated_rows_change_nothing",
                match step {
                    Step::Read { stale } => read_row(*stale).is_none_or(|(row, _)| {
                        s.watch.row.as_ref().is_none_or(|seen| row.newer_than(seen))
                            || n.watch == s.watch
                    }),
                    _ => true,
                },
            ),
            (
                "stalls_counted_once_per_episode_and_retracted_only_by_an_earlier_pause",
                n.watch.stalls == s.watch.stalls
                    || (n.watch.stalls == s.watch.stalls + 1
                        && matches!(step, Step::Wait(_))
                        && !s.watch.stalled
                        && n.watch.stalled)
                    || (n.watch.stalls + 1 == s.watch.stalls
                        && s.watch.stalled
                        && match step {
                            Step::Read { stale } => {
                                read_row(*stale).is_some_and(|(row, status)| {
                                    *status != Viewing::Playing
                                        && s.watch
                                            .stalled_since
                                            .is_some_and(|at| row.updated_ms < at)
                                })
                            }
                            _ => false,
                        }),
            ),
            (
                "watched_time_only_grows_with_observed_play",
                n.watch.watched_ms >= s.watch.watched_ms
                    && (n.watch.watched_ms == s.watch.watched_ms
                        || matches!(step, Step::Observe(..) | Step::Read { .. })),
            ),
            (
                "reason_names_the_fencing_step",
                n.ended.len() == s.ended.len()
                    || match (step, n.ended.last()) {
                        (Step::Fail, Some(EndReason::Error)) => true,
                        (Step::Close { stopping: true }, Some(EndReason::ServerStopped)) => true,
                        (Step::Close { stopping: false }, Some(EndReason::Closed)) => true,
                        (Step::Wait(_), Some(EndReason::LeaseExpired)) => {
                            n.now >= s.delivery.lease_expires_ms
                        }
                        _ => false,
                    },
            ),
            (
                "failed_writes_change_nothing_durable",
                !matches!(step, Step::Flush { commits: false })
                    || (n.row == s.row && n.persisted == s.persisted),
            ),
        ];
        Ok(checks
            .into_iter()
            .map(|(id, ok)| {
                if ok {
                    Check::passed(id)
                } else {
                    Check::failed(id, format!("{step:?} from {s:?} to {n:?}"))
                }
            })
            .collect())
    }
}

const STEPS: u8 = 6;

impl Enumerate for Monitoring {
    fn inputs(&self, s: &State) -> Result<Vec<Step>, ModelError> {
        if s.steps >= STEPS {
            return Ok(vec![]);
        }
        let mut steps = vec![
            Step::Fail,
            Step::Close { stopping: false },
            Step::Close { stopping: true },
            Step::Wait(1_000),
            Step::Wait(STALL_MS - 1_000),
            Step::Play(Viewing::Playing),
            Step::Play(Viewing::Paused),
            Step::Supersede,
            Step::Read { stale: false },
            Step::Read { stale: true },
            Step::Flush { commits: true },
            Step::Flush { commits: false },
        ];
        for sequence in 1..=2 {
            for position in [0, 900] {
                steps.push(Step::Observe(sequence, position));
            }
        }
        Ok(steps)
    }
}

impl ModelCodec for Monitoring {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Step) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Step, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &()) -> Result<Vec<u8>, ModelError> {
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
fn monitoring_graph_follows_reducer_decisions_and_commits() {
    let report = stateless::explore::enumerate(
        &Monitoring,
        stateless::explore::SearchConfig {
            max_states: 5_000_000,
            max_transitions: 100_000_000,
            max_depth: STEPS as usize + 1,
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
        "Playback monitor: {} states, {} transitions; {STEPS} steps over playheads (sequences 1-2, positions 0/900), failure, close (stopping or not), waits of 1 s and 14 s, player rows (playing, paused, a superseding session) read late or stale, committed and failed flushes",
        report.states, report.transitions
    );
}
