//! Administrator playback monitoring: what a delivery's observations mean.
//!
//! The monitor never decides playback. It follows the delivery reducer's own
//! decisions (which playhead was accepted, which step fenced the delivery)
//! and turns them, with the player's viewing rows and the bytes sent, into
//! what an administrator sees and what the history row durably holds. Times
//! are wall-clock milliseconds supplied by the caller.

use serde::{Deserialize, Serialize};

use crate::delivery::{Delivery, Input, Status};

/// A playing session whose position has not moved for this long is stalled.
pub const STALL_MS: u64 = 15_000;
/// How long a finished delivery's history waits for responses still being
/// sent before it is written without them.
pub const DRAIN_MS: u64 = 30_000;
/// At shutdown the process allows the delivery worker five seconds: wait
/// this long for responses, then keep retrying failed writes for
/// `SHUTDOWN_RETRY_MS` before leaving the rest to startup recovery.
pub const SHUTDOWN_DRAIN_MS: u64 = 3_000;
pub const SHUTDOWN_RETRY_MS: u64 = 1_000;
/// Stalls remembered while no viewing row has been read; each takes at least
/// `STALL_MS`, and rows are read every few seconds.
const MAX_UNRESOLVED: usize = 16;
/// The largest position the API carries (a safe JSON integer).
pub const MAX_POSITION_MS: u64 = 9_007_199_254_740_991;

/// Why a delivery's playback ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Closed,
    LeaseExpired,
    Error,
    ServerStopped,
    Interrupted,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EndReason::Closed => "closed",
            EndReason::LeaseExpired => "lease_expired",
            EndReason::Error => "error",
            EndReason::ServerStopped => "server_stopped",
            EndReason::Interrupted => "interrupted",
        }
    }
}

/// Why a reducer step ended playback. Only the step that first fences the
/// delivery has a reason, so a later close cannot relabel a failure or an
/// expired lease. `stopping` is whether the server is shutting down.
pub fn end_reason(
    prior: &Delivery,
    input: &Input,
    next: &Delivery,
    stopping: bool,
) -> Option<EndReason> {
    if prior.status.fenced() || !next.status.fenced() {
        return None;
    }
    Some(match (next.status, input) {
        (Status::Failed, _) => EndReason::Error,
        (Status::Interrupted, _) => EndReason::Interrupted,
        (_, Input::Tick { now_ms }) if *now_ms >= prior.lease_expires_ms => EndReason::LeaseExpired,
        (_, Input::Close) if stopping => EndReason::ServerStopped,
        _ => EndReason::Closed,
    })
}

/// The ordered playhead observation (generation, sequence, position) a
/// reducer step accepted, if it accepted a new one. Rejected, stale and
/// duplicate reports leave the reducer's observation unchanged.
pub fn accepted_playhead(prior: &Delivery, next: &Delivery) -> Option<(u64, u64, u64)> {
    next.observation
        .filter(|_| next.observation != prior.observation)
}

/// A player's viewing status, as its viewing row records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Viewing {
    Playing,
    Paused,
    Ended,
    Stopped,
    Superseded,
}

impl Viewing {
    pub fn parse(status: &str) -> Option<Self> {
        Some(match status {
            "playing" => Viewing::Playing,
            "paused" => Viewing::Paused,
            "ended" => Viewing::Ended,
            "stopped" => Viewing::Stopped,
            "superseded" => Viewing::Superseded,
            _ => return None,
        })
    }
}

/// A viewing row as read: which session, its revision, and when it was
/// last written (to the second the database records).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViewingRow {
    pub id: String,
    pub revision: u64,
    pub updated_ms: u64,
}

impl ViewingRow {
    /// Whether this read is newer than `seen`: a later revision of the same
    /// session, or another session written no earlier (the delivery's
    /// authoritative session changed). Rereading a row is not new.
    pub fn newer_than(&self, seen: &ViewingRow) -> bool {
        if self.id == seen.id {
            self.revision > seen.revision
        } else {
            self.updated_ms >= seen.updated_ms
        }
    }
}

/// Something observed about a live delivery's playback.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Observation {
    /// A playhead the delivery reducer accepted, with its identity.
    Playhead {
        generation: u64,
        sequence: u64,
        position_ms: u64,
        now_ms: u64,
    },
    /// The delivery's authoritative viewing row.
    Viewing {
        row: ViewingRow,
        status: Viewing,
        position_ms: u64,
        now_ms: u64,
    },
    /// Time passed.
    Tick { now_ms: u64 },
}

/// One delivery's observed playback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Watch {
    /// The newest applied reducer playhead (generation, sequence).
    pub playhead: Option<(u64, u64)>,
    /// The newest applied viewing row and its status.
    pub row: Option<ViewingRow>,
    pub viewing: Option<Viewing>,
    pub position_ms: Option<u64>,
    pub start_position_ms: Option<u64>,
    /// When the position was last observed, and last moved forward.
    pub observed_at: Option<u64>,
    pub advanced_at: Option<u64>,
    /// When the current playing episode began.
    pub playing_since: Option<u64>,
    /// When each stall counted since the last viewing row was read began
    /// (its threshold crossed), oldest first: until a row confirms them, a
    /// pause written earlier and read late may show they never happened.
    pub unresolved: Vec<u64>,
    /// Forward play no faster than real time (with slack); seeks do not count.
    pub watched_ms: u64,
    pub stalled: bool,
    pub stalls: u64,
}

impl Watch {
    pub fn new(start_position_ms: Option<u64>) -> Self {
        Self {
            start_position_ms: start_position_ms.filter(|p| *p <= MAX_POSITION_MS),
            ..Default::default()
        }
    }

    /// Apply an observation; false when it is stale or already applied.
    pub fn observe(&mut self, observation: Observation) -> bool {
        match observation {
            Observation::Playhead {
                generation,
                sequence,
                position_ms,
                now_ms,
            } => {
                if position_ms > MAX_POSITION_MS
                    || self
                        .playhead
                        .is_some_and(|seen| (generation, sequence) <= seen)
                {
                    return false;
                }
                self.playhead = Some((generation, sequence));
                self.position(position_ms, now_ms);
            }
            Observation::Viewing {
                row,
                status,
                position_ms,
                now_ms,
            } => {
                if position_ms > MAX_POSITION_MS
                    || self.row.as_ref().is_some_and(|seen| !row.newer_than(seen))
                {
                    return false;
                }
                // Rows are read some seconds after they are written. If
                // writes were missed (a revision skipped, or another session),
                // any of them may have been a pause: nothing after the last
                // row read is certain. Otherwise only this row's own pause is
                // news. A stall that began after the cutoff never happened.
                let missed = self
                    .row
                    .as_ref()
                    .is_some_and(|seen| seen.id != row.id || row.revision != seen.revision + 1);
                let cutoff = if missed {
                    self.row.as_ref().map(|seen| seen.updated_ms)
                } else if status != Viewing::Playing {
                    Some(row.updated_ms)
                } else {
                    None
                };
                if let Some(cutoff) = cutoff {
                    let never = self
                        .unresolved
                        .iter()
                        .filter(|since| **since > cutoff)
                        .count() as u64;
                    if never > 0 {
                        self.stalls -= never;
                        self.stalled = false;
                    }
                }
                if status != Viewing::Playing {
                    self.playing_since = None;
                    self.stalled = false;
                } else if self.viewing != Some(Viewing::Playing) {
                    // A new playing episode: time paused is not a stall.
                    self.playing_since = Some(now_ms);
                } else if missed && !self.stalled {
                    // Playing again, perhaps after a pause we never read. A
                    // stall already confirmed continues until progress or a
                    // pause: restarting it here would count it twice.
                    self.playing_since = self.playing_since.max(Some(row.updated_ms));
                }
                // A playing row confirms the stalls that began before it was
                // written; a later pause may still undo the rest. Any other
                // row settles them all (those it undid are gone).
                if status == Viewing::Playing && !missed {
                    self.unresolved.retain(|since| *since > row.updated_ms);
                } else {
                    self.unresolved.clear();
                }
                self.row = Some(row);
                self.viewing = Some(status);
                // Reducer playheads are ordered and current; a viewing row may
                // lag them, so its position counts only without them.
                if self.playhead.is_none() {
                    self.position(position_ms, now_ms);
                }
            }
            Observation::Tick { now_ms } => {
                let threshold = self
                    .advanced_at
                    .max(self.playing_since)
                    .map(|at| at.saturating_add(STALL_MS));
                let stalled = self.viewing == Some(Viewing::Playing)
                    && threshold.is_some_and(|at| now_ms >= at);
                if stalled && !self.stalled {
                    self.stalls += 1;
                    if let Some(since) = threshold {
                        // Bounded: the oldest is taken as confirmed.
                        if self.unresolved.len() == MAX_UNRESOLVED {
                            self.unresolved.remove(0);
                        }
                        self.unresolved.push(since);
                    }
                }
                self.stalled = stalled;
            }
        }
        true
    }

    fn position(&mut self, position_ms: u64, now_ms: u64) {
        self.start_position_ms.get_or_insert(position_ms);
        if let (Some(last), Some(at)) = (self.position_ms, self.observed_at)
            && position_ms > last
        {
            let moved = position_ms - last;
            let elapsed = now_ms.saturating_sub(at);
            if moved <= elapsed.saturating_mul(3) / 2 + 2_000 {
                self.watched_ms = self.watched_ms.saturating_add(moved);
            }
            self.advanced_at = Some(now_ms);
        }
        self.position_ms = Some(position_ms);
        self.observed_at = Some(now_ms);
    }

    /// Whether the position moved forward within the stall window.
    pub fn advanced_recently(&self, now_ms: u64) -> bool {
        self.advanced_at
            .is_some_and(|at| now_ms.saturating_sub(at) < STALL_MS)
    }
}

/// What a history row holds for a session.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Snapshot {
    pub bytes_sent: u64,
    pub peak_bps: u64,
    pub watched_ms: u64,
    pub stalls: u64,
    pub position_ms: Option<u64>,
    pub last_activity_ms: u64,
    pub status: String,
    /// How the bytes are produced now; a replan can change it.
    pub mode: String,
}

/// What the history row is known to hold: only committed writes count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Persisted {
    written: Option<Snapshot>,
}

/// A write that brings a history row up to date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub snapshot: Snapshot,
    /// The session's status changed, so administrators are told.
    pub status_changed: bool,
}

impl Persisted {
    /// The write needed for `current`, if the row is behind it.
    pub fn due(&self, current: &Snapshot) -> Option<Due> {
        match &self.written {
            Some(written) if written == current => None,
            written => Some(Due {
                snapshot: current.clone(),
                status_changed: written.as_ref().is_none_or(|w| w.status != current.status),
            }),
        }
    }
    /// A write committed. A failed write is never acknowledged, so the
    /// next flush finds the row still behind.
    pub fn acknowledge(&mut self, written: Snapshot) {
        self.written = Some(written);
    }
}

/// Whether a finished delivery's history can be written: once no response
/// is still being sent, or the drain allowance (shorter while the server
/// stops) has passed.
pub fn settled(open_responses: usize, ended_ms: u64, now_ms: u64, stopping: bool) -> bool {
    let allowance = if stopping {
        SHUTDOWN_DRAIN_MS
    } else {
        DRAIN_MS
    };
    open_responses == 0 || now_ms >= ended_ms.saturating_add(allowance)
}

/// Whether the stopping worker keeps writing (and retrying) the history rows
/// still queued since it began to stop at `stopped_ms`.
pub fn keep_finishing(queued: usize, stopped_ms: u64, now_ms: u64) -> bool {
    queued > 0 && now_ms < stopped_ms.saturating_add(SHUTDOWN_DRAIN_MS + SHUTDOWN_RETRY_MS)
}

/// The delivery a direct file read is counted toward, among the reader's
/// live deliveries of that file as (id, source revision). Only deliveries of
/// the revision actually served qualify. The delivery the URL names is used
/// only if it is one of them; without a name only an unambiguous delivery is
/// chosen, never a guess between several.
pub fn attribute<'a>(
    named: Option<&str>,
    served_revision: &str,
    live: &[(&'a str, &str)],
) -> Option<&'a str> {
    let mut serving = live
        .iter()
        .filter(|(_, revision)| *revision == served_revision)
        .map(|(id, _)| *id);
    match named {
        Some(name) => serving.find(|id| *id == name),
        None => match (serving.next(), serving.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, revision: u64, updated_ms: u64) -> ViewingRow {
        ViewingRow {
            id: id.into(),
            revision,
            updated_ms,
        }
    }

    fn viewing(w: &mut Watch, row: ViewingRow, status: Viewing, now_ms: u64) -> bool {
        w.observe(Observation::Viewing {
            row,
            status,
            position_ms: 0,
            now_ms,
        })
    }

    #[test]
    fn stale_reads_never_rewind_newer_playheads() {
        let mut w = Watch::new(Some(0));
        assert!(w.observe(Observation::Playhead {
            generation: 1,
            sequence: 2,
            position_ms: 10_000,
            now_ms: 10_000
        }));
        assert!(!w.observe(Observation::Playhead {
            generation: 1,
            sequence: 1,
            position_ms: 5_000,
            now_ms: 11_000
        }));
        assert!(w.observe(Observation::Viewing {
            row: row("v", 3, 9_000),
            status: Viewing::Playing,
            position_ms: 5_000,
            now_ms: 12_000,
        }));
        assert_eq!(w.position_ms, Some(10_000));
        // Rereading the same row is not a new observation.
        assert!(!viewing(
            &mut w,
            row("v", 3, 9_000),
            Viewing::Playing,
            13_000
        ));
        // A superseding session written in the same second is.
        assert!(viewing(
            &mut w,
            row("a", 1, 9_000),
            Viewing::Playing,
            13_000
        ));
        assert!(!viewing(
            &mut w,
            row("v", 2, 8_000),
            Viewing::Paused,
            13_000
        ));
    }

    #[test]
    fn resuming_after_a_long_pause_is_not_a_stall() {
        let mut w = Watch::new(None);
        viewing(&mut w, row("v", 1, 0), Viewing::Paused, 0);
        viewing(&mut w, row("v", 2, 60_000), Viewing::Playing, 60_000);
        w.observe(Observation::Tick { now_ms: 60_001 });
        assert!(!w.stalled);
        w.observe(Observation::Tick {
            now_ms: 60_000 + STALL_MS,
        });
        w.observe(Observation::Tick {
            now_ms: 61_000 + STALL_MS,
        });
        assert!(w.stalled);
        assert_eq!(w.stalls, 1);
    }

    #[test]
    fn a_pause_read_late_retracts_the_stall_it_preceded() {
        let mut w = Watch::new(None);
        viewing(&mut w, row("v", 1, 0), Viewing::Playing, 0);
        // The player paused at 14 s; the row is read at 17 s, after a tick at
        // 15 s still saw it playing.
        w.observe(Observation::Tick { now_ms: STALL_MS });
        assert_eq!(w.stalls, 1);
        viewing(&mut w, row("v", 2, 14_000), Viewing::Paused, 17_000);
        assert_eq!((w.stalls, w.stalled), (0, false));
        // A pause recorded after the stall began leaves it counted.
        viewing(&mut w, row("v", 3, 20_000), Viewing::Playing, 20_000);
        w.observe(Observation::Tick {
            now_ms: 20_000 + STALL_MS,
        });
        viewing(&mut w, row("v", 4, 36_000), Viewing::Paused, 37_000);
        assert_eq!((w.stalls, w.stalled), (1, false));
    }

    #[test]
    fn a_pause_read_after_intervening_progress_still_retracts_the_stall() {
        let mut w = Watch::new(Some(0));
        let playhead = |w: &mut Watch, sequence, position_ms, now_ms| {
            w.observe(Observation::Playhead {
                generation: 1,
                sequence,
                position_ms,
                now_ms,
            })
        };
        viewing(&mut w, row("v", 1, 0), Viewing::Playing, 0);
        playhead(&mut w, 1, 0, 0);
        // Paused at 14 s (unread); a stall is counted at 15 s; a seek forward
        // while paused, a tick, then the pause is read.
        w.observe(Observation::Tick { now_ms: STALL_MS });
        assert_eq!(w.stalls, 1);
        playhead(&mut w, 2, 900, 16_000);
        w.observe(Observation::Tick { now_ms: 17_000 });
        assert!(!w.stalled);
        viewing(&mut w, row("v", 2, 14_000), Viewing::Paused, 18_000);
        assert_eq!(w.stalls, 0);
    }

    #[test]
    fn a_pause_and_resume_never_read_restarts_the_stall_clock() {
        let mut w = Watch::new(None);
        viewing(&mut w, row("v", 1, 0), Viewing::Playing, 0);
        // Paused at 0 and playing again at 1 s; only the second row is read.
        viewing(&mut w, row("v", 3, 1_000), Viewing::Playing, 2_000);
        w.observe(Observation::Tick { now_ms: STALL_MS });
        assert_eq!(w.stalls, 0, "playing only since 1 s");
        w.observe(Observation::Tick {
            now_ms: 1_000 + STALL_MS,
        });
        assert_eq!(w.stalls, 1);
        // A stall counted before a gap is read is not certain either.
        viewing(&mut w, row("v", 5, 20_000), Viewing::Playing, 21_000);
        assert_eq!((w.stalls, w.stalled), (0, false));
    }

    #[test]
    fn an_older_playing_row_confirms_only_earlier_stalls() {
        let mut w = Watch::new(None);
        viewing(&mut w, row("v", 1, 0), Viewing::Playing, 0);
        w.observe(Observation::Tick { now_ms: STALL_MS });
        assert_eq!(w.stalls, 1);
        // Read late: playing at 10 s, then paused at 14 s, before the stall.
        viewing(&mut w, row("v", 2, 10_000), Viewing::Playing, 16_000);
        assert_eq!(w.stalls, 1);
        viewing(&mut w, row("v", 3, 14_000), Viewing::Paused, 17_000);
        assert_eq!(w.stalls, 0);
    }

    #[test]
    fn positions_beyond_the_api_range_are_not_observed() {
        let mut w = Watch::new(Some(u64::MAX));
        assert_eq!(w.start_position_ms, None);
        assert!(!w.observe(Observation::Playhead {
            generation: 1,
            sequence: 1,
            position_ms: MAX_POSITION_MS + 1,
            now_ms: 0
        }));
        assert_eq!(w.position_ms, None);
    }

    #[test]
    fn only_committed_writes_are_acknowledged() {
        let snapshot = |position, mode: &str| Snapshot {
            bytes_sent: 1,
            peak_bps: 1,
            watched_ms: 0,
            stalls: 0,
            position_ms: Some(position),
            last_activity_ms: 1,
            status: "paused".into(),
            mode: mode.into(),
        };
        let mut p = Persisted::default();
        let first = p.due(&snapshot(5, "direct")).unwrap();
        assert!(first.status_changed);
        // The write failed: still due.
        assert!(p.due(&snapshot(5, "direct")).is_some());
        p.acknowledge(first.snapshot);
        assert!(p.due(&snapshot(5, "direct")).is_none());
        // A paused seek or a replan changes only the position or mode: still written.
        assert_eq!(
            p.due(&snapshot(1, "direct")).map(|d| d.status_changed),
            Some(false)
        );
        assert!(p.due(&snapshot(5, "transcode")).is_some());
    }

    #[test]
    fn attribution_never_guesses_and_matches_the_served_revision() {
        let live = [("a", "r1"), ("b", "r1"), ("c", "r0")];
        assert_eq!(attribute(Some("b"), "r1", &live), Some("b"));
        assert_eq!(attribute(Some("c"), "r1", &live), None, "another revision");
        assert_eq!(attribute(Some("z"), "r1", &live), None);
        assert_eq!(attribute(None, "r0", &live), Some("c"));
        assert_eq!(attribute(None, "r1", &live), None, "ambiguous");
        assert_eq!(attribute(None, "r1", &[]), None);
    }

    #[test]
    fn drains_until_responses_finish_or_the_allowance_passes() {
        assert!(settled(0, 10, 10, false));
        assert!(!settled(1, 10, 10 + DRAIN_MS - 1, false));
        assert!(settled(1, 10, 10 + DRAIN_MS, false));
        assert!(!settled(1, 10, 10 + SHUTDOWN_DRAIN_MS - 1, true));
        assert!(settled(1, 10, 10 + SHUTDOWN_DRAIN_MS, true));
        assert!(keep_finishing(1, 0, SHUTDOWN_DRAIN_MS));
        assert!(!keep_finishing(1, 0, SHUTDOWN_DRAIN_MS + SHUTDOWN_RETRY_MS));
        assert!(!keep_finishing(0, 0, 0));
    }
}
