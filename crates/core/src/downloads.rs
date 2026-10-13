//! Server authority for device-owned offline downloads and the reconciliation
//! of offline viewing events into the canonical timeline viewing state.
//!
//! The server authorizes one exact timeline revision and source file revision
//! per download; the device owns transfer, verification, publication, quotas
//! and removal. Reconciliation consumes each device sequence exactly once and
//! applies an offline event only while no newer authority (another session,
//! another device, a manual override or a timeline change) intervened since
//! the state the download was based on. Everything else is retained as
//! history only, never silently rebased onto newer state.
//!
//! The adapter observes the facts named here in one write transaction and
//! persists exactly the returned records.
use crate::timeline_viewing::{DURATION_SLACK_MS, MAX_POSITION_MS, Status, View};
use serde::{Deserialize, Serialize};

/// Active (not cancelled) downloads one device may hold for one principal.
/// Matches the device cache's manifest bound.
pub const MAX_ACTIVE_PER_DEVICE: usize = 500;

/// The largest single media file the device cache publishes and verifies.
pub const MAX_MEDIA_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Original containers the offline player can present from one complete file.
pub const SUPPORTED_CONTENT_TYPES: [&str; 5] = [
    "video/mp4",
    "video/webm",
    "audio/mp4",
    "audio/mpeg",
    "audio/ogg",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitlePolicy {
    Off,
    Auto,
    Require,
}

/// The requested track selection. The supported slice downloads the complete
/// original file, so pins and required subtitles are refused explicitly
/// rather than silently delivering another selection.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tracks {
    pub audio_component_id: Option<String>,
    pub subtitle_component_id: Option<String>,
    pub audio_track_id: Option<String>,
    pub subtitle_track_id: Option<String>,
    pub subtitle_policy: SubtitlePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Request {
    pub version_id: Option<String>,
    pub recipe_id: Option<String>,
    pub tracks: Tracks,
}

/// One version binding of the requested timeline, observed by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Candidate {
    pub version_id: String,
    pub file_id: String,
    /// Current content revision (the file's SHA-256).
    pub file_revision: String,
    /// The binding still pins the file's current revision.
    pub bound_current: bool,
    pub original: bool,
    pub parts: u32,
    /// The binding presents an interval of its file rather than all of it.
    pub interval: bool,
    pub available: bool,
    /// Inside the principal's catalog scope.
    pub readable: bool,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    /// Timeline duration, or the file's probed duration when the timeline has none.
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Choice {
    pub version_id: String,
    pub file_id: String,
    pub file_revision: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
}

/// Why a download cannot be authorized. Every refusal is explicit; none
/// substitutes another version, component or rendition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Refusal {
    /// No readable, available original binds the timeline (or the version).
    NotFound,
    /// Preparing a rendition for download is not implemented.
    PreparationUnavailable,
    /// Track pins or required subtitles cannot be honoured by a whole-file copy.
    TrackSelectionUnavailable,
    /// Only multipart or interval bindings exist.
    MultipartUnavailable,
    /// Only containers the offline player cannot present exist.
    ContainerUnavailable,
    /// The duration needed to validate offline progress is unknown.
    DurationUnknown,
    /// Larger than the device cache can verify and publish (`MAX_MEDIA_BYTES`).
    TooLarge,
    /// The device already holds the maximum number of active downloads.
    Quota,
}

/// Choose the exact original file a download pins. Candidates are in stable
/// version order; the first one satisfying every rule wins. Unsupported
/// candidates refuse with the most specific reason found among readable ones.
pub fn choose(
    request: &Request,
    candidates: &[Candidate],
    active: usize,
) -> Result<Choice, Refusal> {
    if request.recipe_id.is_some() {
        return Err(Refusal::PreparationUnavailable);
    }
    let t = &request.tracks;
    if t.audio_component_id.is_some()
        || t.subtitle_component_id.is_some()
        || t.audio_track_id.is_some()
        || t.subtitle_track_id.is_some()
        || t.subtitle_policy == SubtitlePolicy::Require
    {
        return Err(Refusal::TrackSelectionUnavailable);
    }
    let mut refusal = Refusal::NotFound;
    let readable = candidates.iter().filter(|c| {
        c.readable
            && c.available
            && c.original
            && c.bound_current
            && request
                .version_id
                .as_ref()
                .is_none_or(|v| v == &c.version_id)
    });
    for c in readable {
        if c.parts != 1 || c.interval {
            refusal = refusal.max_specific(Refusal::MultipartUnavailable);
            continue;
        }
        let Some(content_type) = c
            .content_type
            .as_deref()
            .filter(|t| SUPPORTED_CONTENT_TYPES.contains(t))
        else {
            refusal = refusal.max_specific(Refusal::ContainerUnavailable);
            continue;
        };
        let Some(duration_ms) = c.duration_ms.filter(|d| *d > 0 && *d <= MAX_POSITION_MS) else {
            refusal = refusal.max_specific(Refusal::DurationUnknown);
            continue;
        };
        if c.size_bytes == 0 {
            refusal = refusal.max_specific(Refusal::ContainerUnavailable);
            continue;
        }
        if c.size_bytes > MAX_MEDIA_BYTES {
            refusal = refusal.max_specific(Refusal::TooLarge);
            continue;
        }
        if active >= MAX_ACTIVE_PER_DEVICE {
            return Err(Refusal::Quota);
        }
        return Ok(Choice {
            version_id: c.version_id.clone(),
            file_id: c.file_id.clone(),
            file_revision: c.file_revision.clone(),
            content_type: content_type.into(),
            size_bytes: c.size_bytes,
            duration_ms,
        });
    }
    Err(refusal)
}

impl Refusal {
    /// The more specific of two refusals: the one later in the admission order.
    fn max_specific(self, other: Refusal) -> Refusal {
        let rank = |r: Refusal| match r {
            Refusal::NotFound => 0,
            Refusal::MultipartUnavailable => 1,
            Refusal::ContainerUnavailable => 2,
            Refusal::DurationUnknown => 3,
            Refusal::TooLarge => 4,
            _ => 5,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// The durable server demand. Expiry is derived, never stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stored {
    Ready,
    Cancelled,
}

/// The state a download reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ready,
    Cancelled,
    /// The pinned timeline or file revision is no longer the current one,
    /// or the principal can no longer read it.
    Expired,
}

/// What the download pinned when it was authorized.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pin {
    pub timeline_revision: u64,
    pub version_id: String,
    pub file_id: String,
    pub file_revision: String,
}

/// Current facts about the pinned source, observed by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Current {
    /// None when the timeline no longer exists.
    pub timeline_revision: Option<u64>,
    /// The version still binds the pinned file as its only, whole part.
    pub bound: bool,
    /// None when the file is unavailable.
    pub file_revision: Option<String>,
    /// The principal may still read the timeline and the file's bytes.
    pub readable: bool,
}

pub fn state(stored: Stored, pin: &Pin, current: &Current) -> State {
    match stored {
        Stored::Cancelled => State::Cancelled,
        Stored::Ready
            if current.readable
                && current.timeline_revision == Some(pin.timeline_revision)
                && current.bound
                && current.file_revision.as_deref() == Some(pin.file_revision.as_str()) =>
        {
            State::Ready
        }
        Stored::Ready => State::Expired,
    }
}

/// Cancelling is idempotent; the device separately removes its local bytes.
/// Returns whether the stored demand changed.
pub fn cancel(stored: Stored) -> (Stored, bool) {
    (Stored::Cancelled, stored != Stored::Cancelled)
}

/// Byte grants and manifests are issued only for a current, ready demand.
pub fn transferable(state: State) -> bool {
    state == State::Ready
}

// ------------------------------------------------------------ reconciliation

/// One offline viewing observation as submitted by the device.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OfflineEvent {
    pub id: String,
    pub sequence: u64,
    /// The download whose copy was played; its pin supplies the effective
    /// duration when the timeline has no curated one.
    pub download_id: String,
    pub timeline_id: String,
    pub timeline_revision: u64,
    pub base_viewing_revision: u64,
    pub base_manual_epoch: u64,
    pub position_ms: u64,
    pub status: Status,
    /// Opaque device clock observation; part of the event identity.
    pub device_time: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    HistoryOnly,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// The profile or timeline is unknown or no longer readable.
    NotFound,
    InvalidEvent,
    InvalidPosition,
    /// The timeline changed after the download pinned it.
    TimelineChanged,
    /// A manual watched change happened after the download's base.
    ManualOverrideNewer,
    /// Another session or device advanced viewing after the download's base.
    ViewingNewer,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NotFound => "not_found",
            Code::InvalidEvent => "invalid_event",
            Code::InvalidPosition => "invalid_position",
            Code::TimelineChanged => "timeline_changed",
            Code::ManualOverrideNewer => "manual_override_newer",
            Code::ViewingNewer => "viewing_newer",
        }
    }
}

/// An acknowledged event of this device and profile, kept for exact retries.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Recorded {
    pub event: OfflineEvent,
    pub outcome: Outcome,
    pub code: Option<Code>,
}

/// The causal chain of this device's applied offline progress on one
/// profile and timeline: which base it started from and which viewing
/// revision its last applied event produced. A later event of the same
/// base may continue only while that revision is still the current one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Lineage {
    pub base_viewing_revision: u64,
    pub base_manual_epoch: u64,
    pub produced_revision: u64,
}

/// Current facts about the event's target, observed by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Target {
    pub timeline_revision: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// An exact retry: report the stored outcome; nothing changes.
    Duplicate(Recorded),
    /// The sequence or event ID was used with different content. Not consumed.
    Conflict,
    /// Not the next consecutive sequence. Not consumed.
    SequenceGap,
    /// The sequence is at or below the retention floor: the device reported
    /// that it compacted its log through it, so this is a stale copy (for
    /// example a restored device backup). Not consumed.
    Acknowledged,
    /// The server already retains `Retention::limit` consumed events the
    /// device has not acknowledged. Not consumed; the device must report its
    /// compaction before more events are accepted.
    RetentionFull,
    /// The sequence is consumed and `recorded` is stored with it.
    Consumed {
        recorded: Recorded,
        /// The new viewing state, when applied.
        view: Option<View>,
        /// The open session that lost its authority to the applied event.
        fenced: Option<String>,
        lineage: Option<Lineage>,
    },
}

/// At most this many consumed events are retained per device and profile
/// before the device acknowledges its compaction.
pub const MAX_RETAINED_EVENTS: u64 = 10_000;

/// Server retention of consumed offline events for one device and profile.
/// Every event at or below `pruned_through` has been deleted; every consumed
/// event above it is retained for exact-retry deduplication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Retention {
    pub pruned_through: u64,
    pub limit: u64,
}

impl Retention {
    pub fn server(pruned_through: u64) -> Self {
        Self {
            pruned_through,
            limit: MAX_RETAINED_EVENTS,
        }
    }
}

/// The device reported a compaction beyond what the server consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcknowledgementAhead;

/// The retention floor after a sync request. `acknowledged` is the device's
/// own compaction point: it never resends an event at or below it, so the
/// server may delete those records without weakening exact-retry
/// deduplication. The floor never moves backward, and never past what the
/// server consumed (`next_sequence - 1`); a report beyond that is refused.
/// Without a report (an older agent) nothing is pruned.
pub fn retain(
    next_sequence: u64,
    retention: Retention,
    acknowledged: Option<u64>,
) -> Result<Retention, AcknowledgementAhead> {
    let Some(acknowledged) = acknowledged else {
        return Ok(retention);
    };
    if acknowledged >= next_sequence.max(1) {
        return Err(AcknowledgementAhead);
    }
    Ok(Retention {
        pruned_through: retention.pruned_through.max(acknowledged),
        ..retention
    })
}

/// Why a re-anchor was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReanchorRefusal {
    /// Not a `server_id:restore_epoch` identity, or an unsafe sequence.
    Invalid,
    /// Another server's history: a replaced server never continues it.
    OtherServer,
    /// The current history itself: its sequence is never skipped.
    SameHistory,
}

/// Continue a device's sequence on a restored server. The device compacted
/// its log through `through` in `from` (an earlier history of this server)
/// and never sends those sequences again; the restored server, which never
/// consumed them, moves its next sequence past them and retains nothing at
/// or below. Only the device's own sequence space moves, and only forward;
/// when the server already is past `through` (a retried request, or a
/// backup that already held later sequences) only the floor moves.
/// Refused for the current history or another server's.
pub fn reanchor(
    current: (&str, &str),
    from: &str,
    through: u64,
    next_sequence: u64,
    retention: Retention,
) -> Result<(u64, Retention), ReanchorRefusal> {
    let (server, restore) = from
        .split_once(':')
        .filter(|_| crate::offline::viewing_epoch(from) && through < 1 << 53)
        .ok_or(ReanchorRefusal::Invalid)?;
    if server != current.0 {
        return Err(ReanchorRefusal::OtherServer);
    }
    if restore == current.1 {
        return Err(ReanchorRefusal::SameHistory);
    }
    // The device never resends a sequence at or below `through`, so nothing
    // there is retained, whether or not the sequence moves.
    let retention = Retention {
        pruned_through: retention.pruned_through.max(through),
        ..retention
    };
    Ok((next_sequence.max(through + 1), retention))
}

/// Decide one offline event under the server's retention floor. A recorded
/// event above the floor is still an exact duplicate or a conflict; an
/// unrecorded sequence at or below it is a stale copy and is refused; a new
/// event is refused while the retained, unacknowledged records fill the limit.
/// Otherwise see [`decide`].
#[allow(clippy::too_many_arguments)]
pub fn reconcile(
    retention: Retention,
    next_sequence: u64,
    prior: &[Recorded],
    event: &OfflineEvent,
    target: Option<&Target>,
    view: &View,
    lineage: Option<&Lineage>,
    completion_percent: f64,
) -> Decision {
    let recorded = prior
        .iter()
        .any(|p| p.event.sequence == event.sequence || p.event.id == event.id);
    if !recorded {
        if event.sequence <= retention.pruned_through {
            return Decision::Acknowledged;
        }
        let retained = next_sequence.saturating_sub(1 + retention.pruned_through);
        if event.sequence == next_sequence && retained >= retention.limit {
            return Decision::RetentionFull;
        }
    }
    decide(
        next_sequence,
        prior,
        event,
        target,
        view,
        lineage,
        completion_percent,
    )
}

/// Decide one offline event. `next_sequence` is the next unconsumed device
/// sequence for this device and profile, `prior` any recorded event sharing
/// the incoming sequence or identity, `target` the timeline when the profile
/// is usable and the timeline readable now, and `lineage` this device's
/// chain on the profile and timeline.
fn decide(
    next_sequence: u64,
    prior: &[Recorded],
    event: &OfflineEvent,
    target: Option<&Target>,
    view: &View,
    lineage: Option<&Lineage>,
    completion_percent: f64,
) -> Decision {
    if let Some(p) = prior
        .iter()
        .find(|p| p.event.sequence == event.sequence || p.event.id == event.id)
    {
        return if &p.event == event {
            Decision::Duplicate(p.clone())
        } else {
            Decision::Conflict
        };
    }
    if event.sequence != next_sequence {
        return Decision::SequenceGap;
    }
    let consumed = |outcome, code| Decision::Consumed {
        recorded: Recorded {
            event: event.clone(),
            outcome,
            code,
        },
        view: None,
        fenced: None,
        lineage: None,
    };
    if !event.status.reportable() || event.id.is_empty() || event.position_ms > MAX_POSITION_MS {
        return consumed(Outcome::Rejected, Some(Code::InvalidEvent));
    }
    let Some(target) = target else {
        return consumed(Outcome::Rejected, Some(Code::NotFound));
    };
    if let Some(d) = target.duration_ms
        && (event.position_ms > d.saturating_add(DURATION_SLACK_MS)
            || (event.status == Status::Ended
                && event.position_ms < d.saturating_sub(DURATION_SLACK_MS)))
    {
        return consumed(Outcome::Rejected, Some(Code::InvalidPosition));
    }
    if target.timeline_revision != event.timeline_revision {
        return consumed(Outcome::HistoryOnly, Some(Code::TimelineChanged));
    }
    if view.manual_epoch != event.base_manual_epoch {
        return consumed(Outcome::HistoryOnly, Some(Code::ManualOverrideNewer));
    }
    let continues = lineage.is_some_and(|l| {
        l.base_viewing_revision == event.base_viewing_revision
            && l.base_manual_epoch == event.base_manual_epoch
            && l.produced_revision == view.revision
    });
    if view.revision != event.base_viewing_revision && !continues {
        return consumed(Outcome::HistoryOnly, Some(Code::ViewingNewer));
    }
    let Some(revision) = view.revision.checked_add(1).filter(|v| *v < u64::MAX >> 11) else {
        return consumed(Outcome::Rejected, Some(Code::InvalidEvent));
    };
    let completed = event.status == Status::Ended
        || target.duration_ms.is_some_and(|d| {
            d > 0 && event.position_ms as f64 * 100.0 >= d as f64 * completion_percent
        });
    let next = View {
        revision,
        position_ms: event.position_ms,
        automatic_watched: view.automatic_watched || completed,
        session: None,
        ..view.clone()
    };
    Decision::Consumed {
        recorded: Recorded {
            event: event.clone(),
            outcome: Outcome::Applied,
            code: None,
        },
        lineage: Some(Lineage {
            base_viewing_revision: event.base_viewing_revision,
            base_manual_epoch: event.base_manual_epoch,
            produced_revision: next.revision,
        }),
        fenced: view.session.clone(),
        view: Some(next),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> Candidate {
        Candidate {
            version_id: "v1".into(),
            file_id: "f1".into(),
            file_revision: "r1".into(),
            bound_current: true,
            original: true,
            parts: 1,
            interval: false,
            available: true,
            readable: true,
            content_type: Some("video/mp4".into()),
            size_bytes: 10,
            duration_ms: Some(100_000),
        }
    }
    fn request() -> Request {
        Request {
            version_id: None,
            recipe_id: None,
            tracks: Tracks {
                audio_component_id: None,
                subtitle_component_id: None,
                audio_track_id: None,
                subtitle_track_id: None,
                subtitle_policy: SubtitlePolicy::Auto,
            },
        }
    }

    #[test]
    fn choice_is_exact_and_refusals_are_specific() {
        let c = candidate();
        assert_eq!(
            choose(&request(), std::slice::from_ref(&c), 0)
                .unwrap()
                .file_id,
            "f1"
        );
        let mut r = request();
        r.recipe_id = Some("x".into());
        assert_eq!(
            choose(&r, std::slice::from_ref(&c), 0),
            Err(Refusal::PreparationUnavailable)
        );
        let mut r = request();
        r.tracks.subtitle_policy = SubtitlePolicy::Require;
        assert_eq!(
            choose(&r, std::slice::from_ref(&c), 0),
            Err(Refusal::TrackSelectionUnavailable)
        );
        let mut r = request();
        r.version_id = Some("v2".into());
        assert_eq!(
            choose(&r, std::slice::from_ref(&c), 0),
            Err(Refusal::NotFound)
        );
        let mut multi = c.clone();
        multi.parts = 2;
        let mut mkv = c.clone();
        mkv.version_id = "v2".into();
        mkv.content_type = Some("video/x-matroska".into());
        assert_eq!(
            choose(&request(), &[multi.clone(), mkv.clone()], 0),
            Err(Refusal::ContainerUnavailable)
        );
        assert_eq!(
            choose(&request(), &[multi, mkv, c.clone()], 0)
                .unwrap()
                .version_id,
            "v1"
        );
        let mut hidden = c.clone();
        hidden.readable = false;
        hidden.content_type = None;
        assert_eq!(choose(&request(), &[hidden], 0), Err(Refusal::NotFound));
        let mut unknown = c.clone();
        unknown.duration_ms = None;
        assert_eq!(
            choose(&request(), &[unknown], 0),
            Err(Refusal::DurationUnknown)
        );
        let mut huge = c.clone();
        huge.size_bytes = MAX_MEDIA_BYTES + 1;
        assert_eq!(choose(&request(), &[huge], 0), Err(Refusal::TooLarge));
        assert_eq!(
            choose(&request(), &[c], MAX_ACTIVE_PER_DEVICE),
            Err(Refusal::Quota)
        );
    }

    #[test]
    fn expiry_is_derived_from_current_revisions() {
        let pin = Pin {
            timeline_revision: 2,
            version_id: "v".into(),
            file_id: "f".into(),
            file_revision: "r".into(),
        };
        let mut current = Current {
            timeline_revision: Some(2),
            bound: true,
            file_revision: Some("r".into()),
            readable: true,
        };
        assert_eq!(state(Stored::Ready, &pin, &current), State::Ready);
        current.readable = false;
        assert_eq!(state(Stored::Ready, &pin, &current), State::Expired);
        current.readable = true;
        assert_eq!(state(Stored::Cancelled, &pin, &current), State::Cancelled);
        current.file_revision = Some("changed".into());
        assert_eq!(state(Stored::Ready, &pin, &current), State::Expired);
        current.file_revision = Some("r".into());
        current.timeline_revision = Some(3);
        assert_eq!(state(Stored::Ready, &pin, &current), State::Expired);
        assert_eq!(cancel(Stored::Ready), (Stored::Cancelled, true));
        assert_eq!(cancel(Stored::Cancelled), (Stored::Cancelled, false));
    }

    fn offline(sequence: u64, position_ms: u64) -> OfflineEvent {
        OfflineEvent {
            id: format!("e{sequence}"),
            sequence,
            download_id: "d".into(),
            timeline_id: "t".into(),
            timeline_revision: 1,
            base_viewing_revision: 4,
            base_manual_epoch: 0,
            position_ms,
            status: Status::Paused,
            device_time: "2026-10-11T00:00:00Z".into(),
        }
    }
    const TARGET: Target = Target {
        timeline_revision: 1,
        duration_ms: Some(100_000),
    };

    #[test]
    fn offline_progress_chains_without_overwriting_newer_authority() {
        let view = View {
            revision: 4,
            position_ms: 1_000,
            session: Some("online".into()),
            ..View::default()
        };
        let Decision::Consumed {
            recorded,
            view: Some(next),
            fenced,
            lineage: Some(lineage),
        } = decide(1, &[], &offline(1, 5_000), Some(&TARGET), &view, None, 90.0)
        else {
            panic!()
        };
        assert_eq!(recorded.outcome, Outcome::Applied);
        assert_eq!(
            (next.revision, next.position_ms, next.session.as_deref()),
            (5, 5_000, None)
        );
        assert_eq!(fenced.as_deref(), Some("online"));
        // The chain continues from the revision it produced.
        let Decision::Consumed {
            view: Some(after),
            lineage: Some(l2),
            ..
        } = decide(
            2,
            &[],
            &offline(2, 95_000),
            Some(&TARGET),
            &next,
            Some(&lineage),
            90.0,
        )
        else {
            panic!()
        };
        assert!(after.watched());
        assert_eq!(l2.produced_revision, 6);
        // A newer online revision breaks the chain: history only.
        let mut newer = after.clone();
        newer.revision = 7;
        let Decision::Consumed { recorded, view, .. } = decide(
            3,
            &[],
            &offline(3, 2_000),
            Some(&TARGET),
            &newer,
            Some(&l2),
            90.0,
        ) else {
            panic!()
        };
        assert_eq!(
            (recorded.outcome, recorded.code, view),
            (Outcome::HistoryOnly, Some(Code::ViewingNewer), None)
        );
        // A manual override after the base wins even at the base revision.
        let manual = View {
            manual_epoch: 1,
            ..view_at(4)
        };
        let Decision::Consumed { recorded, .. } = decide(
            1,
            &[],
            &offline(1, 2_000),
            Some(&TARGET),
            &manual,
            None,
            90.0,
        ) else {
            panic!()
        };
        assert_eq!(recorded.code, Some(Code::ManualOverrideNewer));
    }

    fn view_at(revision: u64) -> View {
        View {
            revision,
            ..View::default()
        }
    }

    #[test]
    fn sequences_are_consumed_once_and_retries_are_exact() {
        let view = view_at(4);
        let Decision::Consumed { recorded, .. } =
            decide(1, &[], &offline(1, 10), Some(&TARGET), &view, None, 90.0)
        else {
            panic!()
        };
        assert_eq!(
            decide(
                2,
                std::slice::from_ref(&recorded),
                &offline(1, 10),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::Duplicate(recorded.clone())
        );
        assert_eq!(
            decide(
                2,
                std::slice::from_ref(&recorded),
                &offline(1, 11),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::Conflict
        );
        assert_eq!(
            decide(2, &[], &offline(3, 10), Some(&TARGET), &view, None, 90.0),
            Decision::SequenceGap
        );
        // Unusable targets and invalid positions still consume the sequence.
        for (target, position, code) in [
            (None, 10, Code::NotFound),
            (Some(&TARGET), 200_000, Code::InvalidPosition),
        ] {
            let Decision::Consumed { recorded, view, .. } = decide(
                2,
                &[],
                &offline(2, position),
                target,
                &view_at(4),
                None,
                90.0,
            ) else {
                panic!()
            };
            assert_eq!(
                (recorded.outcome, recorded.code, view),
                (Outcome::Rejected, Some(code), None)
            );
        }
        let changed = Target {
            timeline_revision: 2,
            ..TARGET
        };
        let Decision::Consumed { recorded, .. } = decide(
            2,
            &[],
            &offline(2, 10),
            Some(&changed),
            &view_at(4),
            None,
            90.0,
        ) else {
            panic!()
        };
        assert_eq!(recorded.code, Some(Code::TimelineChanged));
    }

    #[test]
    fn a_restored_server_continues_the_devices_sequence_only_when_asked_from_another_history() {
        let retention = Retention::server(0);
        let current = ("server", "r2");
        // The device compacted through 7 under r1; the restored server is at 3.
        assert_eq!(
            reanchor(current, "server:r1", 7, 3, retention),
            Ok((8, Retention::server(7)))
        );
        // A retried request after the move changes nothing.
        assert_eq!(
            reanchor(current, "server:r1", 7, 8, Retention::server(7)),
            Ok((8, Retention::server(7)))
        );
        // A backup already past `through`: the sequence stays, the floor moves.
        assert_eq!(
            reanchor(current, "server:r1", 10_000, 10_001, retention),
            Ok((10_001, Retention::server(10_000)))
        );
        assert_eq!(
            reanchor(current, "server:r2", 7, 3, retention),
            Err(ReanchorRefusal::SameHistory)
        );
        assert_eq!(
            reanchor(current, "other:r1", 7, 3, retention),
            Err(ReanchorRefusal::OtherServer)
        );
        for bad in ["server", ":r1", "server:"] {
            assert_eq!(
                reanchor(current, bad, 7, 3, retention),
                Err(ReanchorRefusal::Invalid)
            );
        }
        assert_eq!(
            reanchor(current, "server:r1", 1 << 53, 3, retention),
            Err(ReanchorRefusal::Invalid)
        );
    }

    #[test]
    fn retention_follows_the_devices_acknowledged_compaction() {
        let floor = Retention::server(3);
        // No report: an older agent; nothing is pruned.
        assert_eq!(retain(10, floor, None), Ok(floor));
        // Never backward, never past what was consumed.
        assert_eq!(retain(10, floor, Some(1)), Ok(floor));
        assert_eq!(retain(10, floor, Some(9)).unwrap().pruned_through, 9);
        assert_eq!(retain(10, floor, Some(10)), Err(AcknowledgementAhead));
        assert_eq!(
            retain(1, Retention::server(0), Some(0)),
            Ok(Retention::server(0))
        );
        assert_eq!(
            retain(1, Retention::server(0), Some(1)),
            Err(AcknowledgementAhead)
        );
    }

    #[test]
    fn pruned_sequences_refuse_stale_copies_and_the_limit_bounds_retention() {
        let view = view_at(4);
        let tight = Retention {
            pruned_through: 2,
            limit: 2,
        };
        // A stale copy of a pruned event consumes nothing.
        assert_eq!(
            reconcile(
                tight,
                5,
                &[],
                &offline(2, 10),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::Acknowledged
        );
        // 3 and 4 are retained and unacknowledged: the next new event waits.
        assert_eq!(
            reconcile(
                tight,
                5,
                &[],
                &offline(5, 10),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::RetentionFull
        );
        // An exact retry of a retained event is still a duplicate.
        let Decision::Consumed { recorded, .. } =
            decide(4, &[], &offline(4, 10), Some(&TARGET), &view, None, 90.0)
        else {
            panic!()
        };
        assert_eq!(
            reconcile(
                tight,
                5,
                std::slice::from_ref(&recorded),
                &offline(4, 10),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::Duplicate(recorded)
        );
        // Below the limit the next event is decided normally; a gap is a gap.
        let room = Retention { limit: 3, ..tight };
        assert!(matches!(
            reconcile(
                room,
                5,
                &[],
                &offline(5, 10),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::Consumed { .. }
        ));
        assert_eq!(
            reconcile(
                room,
                5,
                &[],
                &offline(7, 10),
                Some(&TARGET),
                &view,
                None,
                90.0
            ),
            Decision::SequenceGap
        );
    }
}
