//! Playback monitoring follows the production delivery reducer: which
//! playheads it accepted, which step ended playback and why, viewing rows in
//! order, stall episodes, and history writes acknowledged only on commit.
use playscale_core::delivery::{self as core, Delivery, Input, Operation, Pin};
use playscale_core::playback_monitor::{
    self as monitor, EndReason, Observation, Persisted, STALL_MS, Snapshot, Viewing, Watch,
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
    /// Ghost, kept by the model alone: the newest row revision applied and
    /// when viewing last entered playing.
    ghost_revision: u64,
    resumed_at: Option<u64>,
    steps: u8,
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
    /// The newest viewing row as read (revision, status, position).
    Row(u64, Viewing, u64),
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
            }
            if let Some(reason) = monitor::end_reason(&n.delivery, &input, &next, stopping) {
                n.ended.push(reason);
            }
            n.delivery = next;
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
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
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
            ghost_revision: 0,
            resumed_at: None,
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
            }
            Step::Row(revision, status, position_ms) => {
                if *revision > s.ghost_revision {
                    n.ghost_revision = *revision;
                    n.resumed_at = match status {
                        Viewing::Playing => s.resumed_at.or(Some(s.now)),
                        _ => None,
                    };
                }
                n.watch.observe(Observation::Viewing {
                    row: (*revision, "v".into(), *revision),
                    status: *status,
                    position_ms: *position_ms,
                    now_ms: s.now,
                });
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
                "stall_needs_a_full_window_of_playing",
                !s.watch.stalled || s.resumed_at.is_some_and(|at| s.now - at >= STALL_MS),
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
                !matches!(step, Step::Row(..))
                    || s.watch.playhead.is_none()
                    || n.watch.position_ms == s.watch.position_ms,
            ),
            (
                "stale_or_repeated_rows_change_nothing",
                match step {
                    Step::Row(revision, ..) => {
                        s.watch.row.as_ref().is_none_or(|(r, ..)| revision > r)
                            || n.watch == s.watch
                    }
                    _ => true,
                },
            ),
            (
                "stalls_counted_once_per_episode",
                n.watch.stalls == s.watch.stalls
                    || (n.watch.stalls == s.watch.stalls + 1
                        && !s.watch.stalled
                        && n.watch.stalled),
            ),
            (
                "watched_time_only_grows_with_observed_play",
                n.watch.watched_ms >= s.watch.watched_ms
                    && (n.watch.watched_ms == s.watch.watched_ms
                        || matches!(step, Step::Observe(..) | Step::Row(..))),
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

impl Enumerate for Monitoring {
    fn inputs(&self, s: &State) -> Result<Vec<Step>, ModelError> {
        if s.steps >= 5 {
            return Ok(vec![]);
        }
        let mut steps = vec![
            Step::Fail,
            Step::Close { stopping: false },
            Step::Close { stopping: true },
            Step::Wait(1_000),
            Step::Wait(STALL_MS),
            Step::Flush { commits: true },
            Step::Flush { commits: false },
        ];
        for sequence in 1..=2 {
            for position in [0, 400, 900] {
                steps.push(Step::Observe(sequence, position));
            }
        }
        for revision in 1..=2 {
            for status in [Viewing::Playing, Viewing::Paused] {
                for position in [0, 900] {
                    steps.push(Step::Row(revision, status, position));
                }
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
    let report = stateless::explore::enumerate(&Monitoring, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Playback monitor: {} states, {} transitions; 5 steps over playheads (sequences 1-2, positions 0/400/900), failure, close (stopping or not), waits of 1 s and the stall window, viewing rows (revisions 1-2, playing/paused), committed and failed flushes",
        report.states, report.transitions
    );
}
