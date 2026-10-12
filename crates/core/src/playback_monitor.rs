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

/// The identity of a viewing row as read: (updated at, id, revision). A
/// later read of the same row is not a new observation.
pub type RowIdentity = (u64, String, u64);

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
    /// The delivery's newest viewing row.
    Viewing {
        row: RowIdentity,
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
    pub row: Option<RowIdentity>,
    pub viewing: Option<Viewing>,
    pub position_ms: Option<u64>,
    pub start_position_ms: Option<u64>,
    /// When the position was last observed, and last moved forward.
    pub observed_at: Option<u64>,
    pub advanced_at: Option<u64>,
    /// When the current playing episode began.
    pub playing_since: Option<u64>,
    /// Forward play no faster than real time (with slack); seeks do not count.
    pub watched_ms: u64,
    pub stalled: bool,
    pub stalls: u64,
}

impl Watch {
    pub fn new(start_position_ms: Option<u64>) -> Self {
        Self {
            start_position_ms,
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
                if self
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
                if self.row.as_ref().is_some_and(|seen| row <= *seen) {
                    return false;
                }
                self.row = Some(row);
                if status != Viewing::Playing {
                    self.playing_since = None;
                    self.stalled = false;
                } else if self.viewing != Some(Viewing::Playing) {
                    // A new playing episode: time paused is not a stall.
                    self.playing_since = Some(now_ms);
                }
                self.viewing = Some(status);
                // Reducer playheads are ordered and current; a viewing row may
                // lag them, so its position counts only without them.
                if self.playhead.is_none() {
                    self.position(position_ms, now_ms);
                }
            }
            Observation::Tick { now_ms } => {
                let since = self.advanced_at.max(self.playing_since);
                let stalled = self.viewing == Some(Viewing::Playing)
                    && since.is_some_and(|at| now_ms.saturating_sub(at) >= STALL_MS);
                if stalled && !self.stalled {
                    self.stalls += 1;
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
/// is still being sent, or the drain allowance has passed.
pub fn settled(open_responses: usize, ended_ms: u64, now_ms: u64) -> bool {
    open_responses == 0 || now_ms >= ended_ms.saturating_add(DRAIN_MS)
}

/// The delivery a direct file read is counted toward, among the reader's
/// live deliveries of that file. The delivery the URL names is used only if
/// it is one of them; without a name only an unambiguous delivery is chosen,
/// never a guess between several.
pub fn attribute<'a>(named: Option<&str>, live: &[&'a str]) -> Option<&'a str> {
    match named {
        Some(name) => live.iter().copied().find(|id| *id == name),
        None => match live {
            [only] => Some(only),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: u64, revision: u64) -> RowIdentity {
        (at, "v".into(), revision)
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
        w.observe(Observation::Viewing {
            row: row(9_000, 3),
            status: Viewing::Playing,
            position_ms: 5_000,
            now_ms: 12_000,
        });
        assert_eq!(w.position_ms, Some(10_000));
        // Reading the same row again is not a new observation.
        assert!(!w.observe(Observation::Viewing {
            row: row(9_000, 3),
            status: Viewing::Playing,
            position_ms: 5_000,
            now_ms: 13_000,
        }));
    }

    #[test]
    fn resuming_after_a_long_pause_is_not_a_stall() {
        let mut w = Watch::new(None);
        w.observe(Observation::Viewing {
            row: row(0, 1),
            status: Viewing::Paused,
            position_ms: 0,
            now_ms: 0,
        });
        w.observe(Observation::Viewing {
            row: row(60_000, 2),
            status: Viewing::Playing,
            position_ms: 0,
            now_ms: 60_000,
        });
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
    fn only_committed_writes_are_acknowledged() {
        let snapshot = |position| Snapshot {
            bytes_sent: 1,
            peak_bps: 1,
            watched_ms: 0,
            stalls: 0,
            position_ms: Some(position),
            last_activity_ms: 1,
            status: "paused".into(),
        };
        let mut p = Persisted::default();
        let first = p.due(&snapshot(5)).unwrap();
        assert!(first.status_changed);
        // The write failed: still due.
        assert!(p.due(&snapshot(5)).is_some());
        p.acknowledge(first.snapshot);
        assert!(p.due(&snapshot(5)).is_none());
        // A paused seek changes only the position: still written.
        assert_eq!(p.due(&snapshot(1)).map(|d| d.status_changed), Some(false));
    }

    #[test]
    fn attribution_never_guesses() {
        assert_eq!(attribute(Some("b"), &["a", "b"]), Some("b"));
        assert_eq!(attribute(Some("c"), &["a", "b"]), None);
        assert_eq!(attribute(Some("c"), &["c2"]), None);
        assert_eq!(attribute(None, &["a"]), Some("a"));
        assert_eq!(attribute(None, &["a", "b"]), None);
        assert_eq!(attribute(None, &[]), None);
    }

    #[test]
    fn drains_until_responses_finish_or_the_allowance_passes() {
        assert!(settled(0, 10, 10));
        assert!(!settled(1, 10, 10 + DRAIN_MS - 1));
        assert!(settled(1, 10, 10 + DRAIN_MS));
    }
}
