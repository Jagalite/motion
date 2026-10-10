//! Timeline-keyed viewing authority (v2): one viewing state per profile and
//! timeline, ordered sessions bound to a delivery, idempotent events, manual
//! overrides that advance a manual epoch, and sticky automatic completion.
//!
//! Positions are milliseconds on the logical timeline. A session's delivery
//! and generation say how the timeline was presented, not where the playhead
//! is, so they never decide whether progress is accepted; only the session's
//! authority, its manual epoch and the consecutive sequence do.
//!
//! The adapter loads the state, the session and any acknowledged event that
//! shares the incoming event's sequence or identity, and persists exactly the
//! returned records in one transaction.
use serde::{Deserialize, Serialize};

/// Ten years: the same bound the v1 session policy uses.
pub const MAX_POSITION_MS: u64 = 315_360_000_000;
/// Tolerance for container/encoder duration rounding.
pub const DURATION_SLACK_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Playing,
    Paused,
    Ended,
    Stopped,
    /// A newer session, or a manual override, took the authority.
    Superseded,
}
impl Status {
    /// Statuses a client may report in an event.
    pub fn reportable(self) -> bool {
        matches!(
            self,
            Status::Playing | Status::Paused | Status::Ended | Status::Stopped
        )
    }
    /// No further events are accepted.
    pub fn closed(self) -> bool {
        matches!(self, Status::Ended | Status::Stopped | Status::Superseded)
    }
}

/// The durable viewing state of one profile on one timeline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct View {
    pub revision: u64,
    pub manual_epoch: u64,
    pub position_ms: u64,
    /// Sticky: once the timeline was completed, only a manual override hides it.
    pub automatic_watched: bool,
    pub manual_watched: Option<bool>,
    /// The session whose events may advance this state.
    pub session: Option<String>,
}
impl View {
    pub fn watched(&self) -> bool {
        self.manual_watched.unwrap_or(self.automatic_watched)
    }
    /// Continue-watching admits positively progressed, effectively unwatched state.
    pub fn resumable(&self) -> bool {
        self.position_ms > 0 && !self.watched()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub revision: u64,
    pub delivery: String,
    /// The manual epoch of the state when the session was created.
    pub manual_epoch: u64,
    pub sequence: u64,
    pub position_ms: u64,
    pub status: Status,
    /// Logical duration of the bound delivery, when known.
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub sequence: u64,
    pub generation: u64,
    pub position_ms: u64,
    pub status: Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Error {
    /// The expected viewing or session revision is not the current one.
    RevisionConflict,
    /// The session no longer holds the authority for its profile and timeline.
    Superseded,
    /// The session ended or stopped.
    Closed,
    /// The sequence is not the next consecutive one.
    SequenceGap,
    /// The sequence was already used and its acknowledgement is unavailable.
    StaleSequence,
    /// A sequence or event identity was reused with different content.
    EventConflict,
    InvalidEvent,
    /// Outside the bound delivery's logical duration, or `ended` too early.
    InvalidPosition,
    Exhausted,
}

fn next(value: u64) -> Result<u64, Error> {
    value
        .checked_add(1)
        .filter(|v| *v < u64::MAX >> 11)
        .ok_or(Error::Exhausted)
}

/// Clamp a stored position into a (possibly shorter) delivery's duration.
fn clamp(position_ms: u64, duration_ms: Option<u64>) -> u64 {
    duration_ms.map_or(position_ms, |d| position_ms.min(d))
}

/// A newly created session and the state that now names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    pub view: View,
    pub session: Session,
    /// The previous authoritative session, now superseded.
    pub superseded: Option<String>,
}

/// Create the authoritative session `id` bound to `delivery`. Only the exact
/// current viewing revision may do so: a stale client never silently takes
/// over another device's session.
pub fn start(
    view: &View,
    expected_revision: u64,
    id: String,
    delivery: String,
    duration_ms: Option<u64>,
) -> Result<Started, Error> {
    if view.revision != expected_revision {
        return Err(Error::RevisionConflict);
    }
    let position_ms = clamp(view.position_ms, duration_ms);
    Ok(Started {
        view: View {
            revision: next(view.revision)?,
            session: Some(id.clone()),
            position_ms,
            ..view.clone()
        },
        session: Session {
            id,
            revision: 1,
            delivery,
            manual_epoch: view.manual_epoch,
            sequence: 0,
            position_ms,
            status: Status::Paused,
            duration_ms,
        },
        superseded: view.session.clone(),
    })
}

/// Mark a session whose authority was taken. Ended/stopped sessions keep
/// their final status; only open ones become superseded.
pub fn supersede(session: &Session) -> Result<Session, Error> {
    if session.status.closed() {
        return Ok(session.clone());
    }
    Ok(Session {
        status: Status::Superseded,
        revision: next(session.revision)?,
        ..session.clone()
    })
}

/// Whether `session` currently holds the authority over `view`.
pub fn authoritative(view: &View, session: &Session) -> bool {
    view.session.as_deref() == Some(session.id.as_str())
        && view.manual_epoch == session.manual_epoch
        && session.status != Status::Superseded
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// An exact retry of an acknowledged event: replay its acknowledgement.
    Duplicate,
    Accepted {
        view: View,
        session: Session,
    },
}

/// Apply one event. `prior` holds the acknowledged events of this session that
/// share the incoming event's sequence or identity (at most two). An exact
/// retry is acknowledged even after the session lost its authority, so a
/// durable client outbox can drain after a restart or a supersession.
pub fn record(
    view: &View,
    session: &Session,
    event: &Event,
    prior: &[Event],
    completion_percent: f64,
) -> Result<Recorded, Error> {
    if !event.status.reportable()
        || event.sequence == 0
        || event.generation == 0
        || event.position_ms > MAX_POSITION_MS
        || event.id.is_empty()
    {
        return Err(Error::InvalidEvent);
    }
    if let Some(p) = prior
        .iter()
        .find(|p| p.sequence == event.sequence || p.id == event.id)
    {
        return if p == event {
            Ok(Recorded::Duplicate)
        } else {
            Err(Error::EventConflict)
        };
    }
    if !authoritative(view, session) {
        return Err(Error::Superseded);
    }
    if session.status.closed() {
        return Err(Error::Closed);
    }
    if event.sequence <= session.sequence {
        return Err(Error::StaleSequence);
    }
    if event.sequence != session.sequence + 1 {
        return Err(Error::SequenceGap);
    }
    if let Some(d) = session.duration_ms
        && (event.position_ms > d.saturating_add(DURATION_SLACK_MS)
            || (event.status == Status::Ended
                && event.position_ms < d.saturating_sub(DURATION_SLACK_MS)))
    {
        return Err(Error::InvalidPosition);
    }
    let completed = event.status == Status::Ended
        || session.duration_ms.is_some_and(|d| {
            d > 0 && event.position_ms as f64 * 100.0 >= d as f64 * completion_percent
        });
    Ok(Recorded::Accepted {
        view: View {
            revision: next(view.revision)?,
            position_ms: event.position_ms,
            automatic_watched: view.automatic_watched || completed,
            ..view.clone()
        },
        session: Session {
            revision: next(session.revision)?,
            sequence: event.sequence,
            position_ms: event.position_ms,
            status: event.status,
            ..session.clone()
        },
    })
}

/// A manual watched override (or its removal). It advances the manual epoch,
/// so no session created before it can write progress afterwards, and
/// optionally clears the resume position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overridden {
    pub view: View,
    pub fenced: Option<String>,
}
pub fn override_watched(
    view: &View,
    expected_revision: u64,
    manual_watched: Option<bool>,
    reset_position: bool,
) -> Result<Overridden, Error> {
    if view.revision != expected_revision {
        return Err(Error::RevisionConflict);
    }
    Ok(Overridden {
        view: View {
            revision: next(view.revision)?,
            manual_epoch: next(view.manual_epoch)?,
            manual_watched,
            position_ms: if reset_position { 0 } else { view.position_ms },
            session: None,
            ..view.clone()
        },
        fenced: view.session.clone(),
    })
}

/// Rebind an authoritative, open session to another delivery of the same
/// profile and timeline (checked by the adapter through delivery ownership).
/// The session keeps its identity, sequence and position.
pub fn rebind(
    view: &View,
    session: &Session,
    expected_revision: u64,
    delivery: String,
    duration_ms: Option<u64>,
) -> Result<Session, Error> {
    if session.revision != expected_revision {
        return Err(Error::RevisionConflict);
    }
    if !authoritative(view, session) {
        return Err(Error::Superseded);
    }
    if session.status.closed() {
        return Err(Error::Closed);
    }
    Ok(Session {
        revision: next(session.revision)?,
        delivery,
        duration_ms,
        ..session.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, sequence: u64, position_ms: u64, status: Status) -> Event {
        Event {
            id: id.into(),
            sequence,
            generation: 1,
            position_ms,
            status,
        }
    }

    fn started() -> Started {
        start(&View::default(), 0, "s".into(), "d".into(), Some(100_000)).unwrap()
    }

    #[test]
    fn events_are_consecutive_idempotent_and_conflicts_are_explicit() {
        let Started { view, session, .. } = started();
        let e1 = event("e1", 1, 10_000, Status::Playing);
        let Recorded::Accepted { view, session } = record(&view, &session, &e1, &[], 90.0).unwrap()
        else {
            panic!()
        };
        assert_eq!((view.position_ms, session.sequence), (10_000, 1));
        assert_eq!(
            record(&view, &session, &e1, &[e1.clone()], 90.0),
            Ok(Recorded::Duplicate)
        );
        let changed = event("e1", 1, 11_000, Status::Playing);
        assert_eq!(
            record(&view, &session, &changed, &[e1.clone()], 90.0),
            Err(Error::EventConflict)
        );
        let reused = event("e1", 2, 11_000, Status::Playing);
        assert_eq!(
            record(&view, &session, &reused, &[e1.clone()], 90.0),
            Err(Error::EventConflict)
        );
        assert_eq!(
            record(
                &view,
                &session,
                &event("e3", 3, 1, Status::Paused),
                &[],
                90.0
            ),
            Err(Error::SequenceGap)
        );
        // A seek backwards is ordinary progress.
        let seek = event("e2", 2, 2_000, Status::Paused);
        assert!(matches!(
            record(&view, &session, &seek, &[], 90.0),
            Ok(Recorded::Accepted { .. })
        ));
    }

    #[test]
    fn completion_is_sticky_and_threshold_driven() {
        let Started { view, session, .. } = started();
        let near = event("e1", 1, 89_999, Status::Playing);
        let Recorded::Accepted { view, session } =
            record(&view, &session, &near, &[], 90.0).unwrap()
        else {
            panic!()
        };
        assert!(!view.watched());
        let at = event("e2", 2, 90_000, Status::Playing);
        let Recorded::Accepted { view, session } = record(&view, &session, &at, &[], 90.0).unwrap()
        else {
            panic!()
        };
        assert!(view.watched());
        let back = event("e3", 3, 1_000, Status::Paused);
        let Recorded::Accepted { view, .. } = record(&view, &session, &back, &[], 90.0).unwrap()
        else {
            panic!()
        };
        assert!(view.watched() && !view.resumable());
    }

    #[test]
    fn ended_must_be_at_the_end_and_positions_stay_within_duration() {
        let Started { view, session, .. } = started();
        for (position, status) in [(98_999, Status::Ended), (101_001, Status::Playing)] {
            assert_eq!(
                record(&view, &session, &event("e", 1, position, status), &[], 90.0),
                Err(Error::InvalidPosition)
            );
        }
        let Recorded::Accepted { session, view } = record(
            &view,
            &session,
            &event("e", 1, 100_500, Status::Ended),
            &[],
            90.0,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            record(
                &view,
                &session,
                &event("f", 2, 0, Status::Playing),
                &[],
                90.0
            ),
            Err(Error::Closed)
        );
    }

    #[test]
    fn manual_override_fences_earlier_sessions_but_retries_still_replay() {
        let Started { view, session, .. } = started();
        let e1 = event("e1", 1, 10_000, Status::Playing);
        let Recorded::Accepted { view, session } = record(&view, &session, &e1, &[], 90.0).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            override_watched(&view, view.revision - 1, Some(true), false),
            Err(Error::RevisionConflict)
        );
        let o = override_watched(&view, view.revision, Some(false), true).unwrap();
        assert_eq!(o.fenced.as_deref(), Some("s"));
        assert_eq!((o.view.manual_epoch, o.view.position_ms), (1, 0));
        let e2 = event("e2", 2, 20_000, Status::Playing);
        assert_eq!(
            record(&o.view, &session, &e2, &[], 90.0),
            Err(Error::Superseded)
        );
        assert_eq!(
            record(&o.view, &session, &e1, &[e1.clone()], 90.0),
            Ok(Recorded::Duplicate)
        );
        // A new session after the override starts in the new epoch.
        let again = start(&o.view, o.view.revision, "t".into(), "d".into(), None).unwrap();
        assert_eq!(again.session.manual_epoch, 1);
        assert!(authoritative(&again.view, &again.session));
    }

    #[test]
    fn a_new_session_needs_the_current_revision_and_supersedes_the_old_one() {
        let first = started();
        assert_eq!(
            start(&first.view, 0, "t".into(), "d2".into(), None),
            Err(Error::RevisionConflict)
        );
        let second = start(
            &first.view,
            first.view.revision,
            "t".into(),
            "d2".into(),
            Some(50_000),
        )
        .unwrap();
        assert_eq!(second.superseded.as_deref(), Some("s"));
        assert!(!authoritative(&second.view, &first.session));
        let old = supersede(&first.session).unwrap();
        assert_eq!(old.status, Status::Superseded);
        assert_eq!(
            rebind(&second.view, &old, old.revision, "d3".into(), None),
            Err(Error::Superseded)
        );
    }

    #[test]
    fn rebinding_keeps_identity_and_requires_the_current_session_revision() {
        let Started { view, session, .. } = started();
        assert_eq!(
            rebind(&view, &session, 7, "d2".into(), None),
            Err(Error::RevisionConflict)
        );
        let moved = rebind(&view, &session, 1, "d2".into(), Some(99_000)).unwrap();
        assert_eq!(
            (moved.id.as_str(), moved.delivery.as_str(), moved.revision),
            ("s", "d2", 2)
        );
        assert_eq!(moved.sequence, session.sequence);
    }

    #[test]
    fn starting_clamps_the_resume_position_into_the_delivery() {
        let view = View {
            position_ms: 120_000,
            ..View::default()
        };
        let s = start(&view, 0, "s".into(), "d".into(), Some(60_000)).unwrap();
        assert_eq!(
            (s.view.position_ms, s.session.position_ms),
            (60_000, 60_000)
        );
    }
}
