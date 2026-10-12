//! Offline download reconciliation against the production core policies.
//!
//! One device downloads a timeline, records offline progress into its local
//! log (`offline::admit`), synchronizes in order (`downloads::reconcile`),
//! loses responses, retries, compacts acknowledged events (`offline::compact`)
//! and rebases onto newer server state, interleaved with online progress,
//! manual overrides and timeline edits on the server. The oracle does not
//! re-run the policy: it tracks whether any other authority touched the
//! timeline since the download's base and derives the required outcome.
use playscale_core::{
    downloads::{self, Code, Decision, Lineage, OfflineEvent, Outcome, Recorded, Target},
    offline::{self, Admission, CacheScope, DownloadIdentity},
    timeline_viewing::{self, Status, View},
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

const DURATION: u64 = 100_000;
const MAX_SEQUENCES: u64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    /// Take a download pinned to the current viewing and timeline state.
    Download,
    /// Offline observations: partial, completing, beyond the timeline.
    Play(Position),
    /// Send every retained event; the device receives the response.
    Sync,
    /// The server commits the batch but the response is lost.
    SyncLost,
    /// Resend the oldest recorded event with altered content.
    AlteredRetry,
    /// Skip a sequence.
    Gap,
    /// After a complete sync, re-read the manifest base.
    Rebase,
    /// Another session advanced viewing on the server.
    OnlineProgress,
    ManualOverride,
    TimelineEdit,
    /// The server database is restored from a backup taken at the start; its
    /// history identity (restore epoch) changes.
    Restore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Position {
    Partial,
    Complete,
    Beyond,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct World {
    view: View,
    timeline_revision: u64,
    // Server reconciliation state for this device and profile.
    next_sequence: u64,
    recorded: Vec<Recorded>,
    lineage: Option<Lineage>,
    // Device state.
    download: Option<DownloadIdentity>,
    base: u64,
    log: Vec<offline::OfflineEvent>,
    // Oracle facts since the download's base.
    other_viewing: bool,
    other_manual: bool,
    online: u8,
    manual: u8,
    edits: u8,
    /// The server's restore epoch and how many restores happened.
    epoch: u8,
    restores: u8,
}

fn scope() -> CacheScope {
    CacheScope {
        server_id: "server".into(),
        principal_id: "device".into(),
        profile_id: "profile".into(),
        device_id: "device".into(),
    }
}

fn wire(e: &offline::OfflineEvent) -> OfflineEvent {
    OfflineEvent {
        id: e.event_id.clone(),
        sequence: e.device_sequence.parse().unwrap(),
        download_id: e.media.download_id.clone(),
        timeline_id: e.media.timeline_id.clone(),
        timeline_revision: e.media.timeline_revision.parse().unwrap(),
        base_viewing_revision: e.media.base_viewing_revision.parse().unwrap(),
        base_manual_epoch: e.media.base_manual_epoch.parse().unwrap(),
        position_ms: e.position_ms,
        status: match e.status.as_str() {
            "ended" => Status::Ended,
            _ => Status::Paused,
        },
        device_time: format!("t{}", e.device_sequence),
    }
}

fn position(p: Position) -> (u64, &'static str) {
    match p {
        Position::Partial => (40_000, "paused"),
        Position::Complete => (DURATION, "ended"),
        Position::Beyond => (DURATION * 2, "paused"),
    }
}

fn identity(w: &World) -> DownloadIdentity {
    DownloadIdentity {
        download_id: "download".into(),
        timeline_id: "timeline".into(),
        timeline_revision: w.timeline_revision.to_string(),
        source_revision: "source".into(),
        base_viewing_revision: w.view.revision.to_string(),
        base_manual_epoch: w.view.manual_epoch.to_string(),
        viewing_epoch: Some(history(w)),
    }
}

fn history(w: &World) -> String {
    format!("server:{}", w.epoch)
}

/// The history identity a batch is sent under: that of its events' base.
fn header(log: &[offline::OfflineEvent]) -> Option<String> {
    log.first().and_then(|e| e.media.viewing_epoch.clone())
}

/// Server side of one batch: each event decided in order against the state
/// left by the previous one, exactly as the adapter's transaction does.
fn sync(
    w: &mut World,
    events: &[OfflineEvent],
    header: Option<&str>,
) -> Vec<Option<(Outcome, Option<Code>)>> {
    // The adapter checks the history identity first, inside the transaction.
    let current = history(w);
    let accepted = header
        .and_then(|h| h.split_once(':'))
        .is_some_and(|expected| {
            timeline_viewing::accepts_history_identity(expected, current.split_once(':').unwrap())
        });
    if !accepted {
        return vec![None; events.len()];
    }
    let mut results = vec![];
    for event in events {
        let prior: Vec<Recorded> = w
            .recorded
            .iter()
            .filter(|r| r.event.sequence == event.sequence || r.event.id == event.id)
            .cloned()
            .collect();
        let target = Target {
            timeline_revision: w.timeline_revision,
            duration_ms: Some(DURATION),
        };
        match downloads::reconcile(
            w.next_sequence,
            &prior,
            event,
            Some(&target),
            &w.view,
            w.lineage.as_ref(),
            90.0,
        ) {
            Decision::Duplicate(r) => results.push(Some((r.outcome, r.code))),
            Decision::Conflict | Decision::SequenceGap => results.push(None),
            Decision::Consumed {
                recorded,
                view,
                lineage,
                ..
            } => {
                w.next_sequence += 1;
                results.push(Some((recorded.outcome, recorded.code)));
                w.recorded.push(recorded);
                if let Some(v) = view {
                    w.view = v;
                }
                if lineage.is_some() {
                    w.lineage = lineage;
                }
            }
        }
    }
    results
}

fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(name)
    } else {
        Check::failed(name, "offline reconciliation obligation violated")
    }
}

struct Downloads;

impl Model for Downloads {
    type State = World;
    type Input = Input;
    type Output = Option<(Outcome, Option<Code>)>;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.offline-download-reconciliation".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 1,
            build: concat!(
                include_str!("../src/downloads.rs"),
                include_str!("../src/offline.rs")
            )
            .into(),
        }
    }

    fn initial_state(&self) -> Result<World, ModelError> {
        Ok(World {
            view: View::default(),
            timeline_revision: 1,
            next_sequence: 1,
            recorded: vec![],
            lineage: None,
            download: None,
            base: 0,
            log: vec![],
            other_viewing: false,
            other_manual: false,
            online: 0,
            manual: 0,
            edits: 0,
            epoch: 0,
            restores: 0,
        })
    }

    fn check_state(&self, w: &World) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            check(
                "reconcile.each_sequence_recorded_once_in_order",
                w.recorded
                    .iter()
                    .enumerate()
                    .all(|(i, r)| r.event.sequence == i as u64 + 1)
                    && w.next_sequence == w.recorded.len() as u64 + 1,
            ),
            check(
                "reconcile.device_never_compacts_unrecorded_events",
                // A restore rewinds the server below what the device already
                // compacted against the previous history.
                w.restores > 0 || w.base < w.next_sequence,
            ),
        ])
    }

    fn step(
        &self,
        w: &World,
        input: &Input,
    ) -> Result<Transition<World, Self::Output>, ModelError> {
        let mut n = w.clone();
        let mut out = vec![];
        let written = w.base + w.log.len() as u64;
        match *input {
            Input::Download if w.download.is_none() => {
                n.download = Some(identity(w));
                n.other_viewing = false;
                n.other_manual = false;
            }
            Input::Play(p) if written < MAX_SEQUENCES => {
                if let Some(media) = &w.download {
                    let (position_ms, status) = position(p);
                    let next = offline::next_sequence(w.base, &w.log).unwrap();
                    let event = offline::OfflineEvent {
                        scope: scope(),
                        media: media.clone(),
                        event_id: format!("e{next}"),
                        device_sequence: next.to_string(),
                        position_ms,
                        status: status.into(),
                        device_time: None,
                    };
                    let admitted =
                        offline::admit(w.base, &w.log, &event, &scope(), media, DURATION * 2, 8);
                    if admitted == Admission::Append {
                        n.log.push(event);
                    }
                }
            }
            Input::Sync | Input::SyncLost => {
                let events: Vec<_> = w.log.iter().map(wire).collect();
                out = sync(&mut n, &events, header(&w.log).as_deref());
                if *input == Input::Sync {
                    // The device compacts only what the results confirm.
                    let acks: Vec<_> = w
                        .log
                        .iter()
                        .zip(&out)
                        .map(|(e, r)| offline::Acknowledgement {
                            event_id: e.event_id.clone(),
                            consumed: r.is_some(),
                        })
                        .collect();
                    let through =
                        offline::acknowledged_through(w.base, &w.log, &acks, n.next_sequence);
                    if let Some((base, log)) = offline::compact(w.base, &w.log, through) {
                        n.base = base;
                        n.log = log;
                    }
                }
            }
            Input::AlteredRetry => {
                if let Some(first) = w.recorded.first() {
                    let mut altered = first.event.clone();
                    altered.position_ms += 1;
                    out = sync(&mut n, &[altered], Some(&history(w)));
                }
            }
            Input::Gap => {
                if let Some(media) = &w.download {
                    let mut e = wire(&offline::OfflineEvent {
                        scope: scope(),
                        media: media.clone(),
                        event_id: "gap".into(),
                        device_sequence: (w.next_sequence + 1).to_string(),
                        position_ms: 1,
                        status: "paused".into(),
                        device_time: None,
                    });
                    e.device_time = "gap".into();
                    out = sync(&mut n, &[e], Some(&history(w)));
                }
            }
            Input::Rebase if w.download.is_some() && w.log.is_empty() => {
                let mut media = identity(w);
                media.timeline_revision = w.download.as_ref().unwrap().timeline_revision.clone();
                n.download = Some(media);
                n.other_viewing = false;
                n.other_manual = false;
            }
            Input::OnlineProgress if w.online < 2 => {
                n.online += 1;
                n.view.revision += 1;
                n.view.position_ms = 5_000;
                n.other_viewing = true;
            }
            Input::ManualOverride if w.manual < 1 => {
                n.manual += 1;
                n.view.revision += 1;
                n.view.manual_epoch += 1;
                n.view.manual_watched = Some(false);
                n.view.position_ms = 0;
                n.other_manual = true;
            }
            Input::TimelineEdit if w.edits < 1 => {
                n.edits += 1;
                n.timeline_revision += 1;
            }
            Input::Restore if w.restores < 1 => {
                // Back to the empty backup; the device keeps everything.
                n.view = View::default();
                n.timeline_revision = 1;
                n.next_sequence = 1;
                n.recorded = vec![];
                n.lineage = None;
                n.epoch += 1;
                n.restores += 1;
            }
            _ => {}
        }
        Ok(Transition::accepted(n, out))
    }

    fn check_transition(
        &self,
        w: &World,
        input: &Input,
        t: &TransitionRef<'_, World, Self::Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let n = t.state;
        let mut checks = vec![
            check(
                "reconcile.manual_state_is_never_overwritten",
                !matches!(
                    input,
                    Input::Sync | Input::SyncLost | Input::AlteredRetry | Input::Gap
                ) || (n.view.manual_epoch == w.view.manual_epoch
                    && n.view.manual_watched == w.view.manual_watched),
            ),
            check(
                "reconcile.viewing_revision_monotonic",
                // Within one server history; a restore rewinds it by definition.
                *input == Input::Restore || n.view.revision >= w.view.revision,
            ),
            check(
                "device.sequence_never_renumbered",
                n.base + n.log.len() as u64 >= w.base + w.log.len() as u64 && n.base >= w.base,
            ),
        ];
        match input {
            Input::Sync | Input::SyncLost => {
                // Oracle: the required outcome of each retained event.
                let pin = w
                    .download
                    .as_ref()
                    .map(|d| d.timeline_revision.parse::<u64>().unwrap());
                let mut expected = vec![];
                let same_history = header(&w.log) == Some(history(w));
                let mut next = w.next_sequence;
                for e in &w.log {
                    let sequence: u64 = e.device_sequence.parse().unwrap();
                    if !same_history {
                        // Another server history: nothing is consumed or applied.
                        expected.push(None);
                    } else if let Some(r) = w.recorded.iter().find(|r| r.event.sequence == sequence)
                    {
                        expected.push(Some((r.outcome, r.code)));
                    } else if sequence != next {
                        expected.push(None);
                    } else if e.position_ms > DURATION + 1_000 {
                        next += 1;
                        expected.push(Some((Outcome::Rejected, Some(Code::InvalidPosition))));
                    } else if pin != Some(w.timeline_revision) {
                        next += 1;
                        expected.push(Some((Outcome::HistoryOnly, Some(Code::TimelineChanged))));
                    } else if w.other_manual {
                        next += 1;
                        expected.push(Some((
                            Outcome::HistoryOnly,
                            Some(Code::ManualOverrideNewer),
                        )));
                    } else if w.other_viewing {
                        next += 1;
                        expected.push(Some((Outcome::HistoryOnly, Some(Code::ViewingNewer))));
                    } else {
                        next += 1;
                        expected.push(Some((Outcome::Applied, None)));
                    }
                }
                let last_applied = w
                    .log
                    .iter()
                    .zip(&expected)
                    .rev()
                    .find(|(e, x)| {
                        **x == Some((Outcome::Applied, None))
                            && !w.recorded.iter().any(|r| r.event.id == e.event_id)
                    })
                    .map(|(e, _)| e.position_ms);
                checks.push(check("reconcile.required_outcomes", t.outputs == expected));
                checks.push(check(
                    "reconcile.applied_progress_is_the_last_applied_event",
                    n.view.position_ms == last_applied.unwrap_or(w.view.position_ms)
                        && n.view.automatic_watched
                            == (w.view.automatic_watched
                                || w.log.iter().zip(&expected).any(|(e, x)| {
                                    *x == Some((Outcome::Applied, None))
                                        && e.position_ms >= DURATION * 9 / 10
                                })),
                ));
                checks.push(check(
                    "device.compacts_exactly_acknowledged_events",
                    if *input == Input::Sync {
                        let confirmed = expected.iter().take_while(|x| x.is_some()).count();
                        n.log == w.log[confirmed..] && n.base == w.base + confirmed as u64
                    } else {
                        n.log == w.log && n.base == w.base
                    },
                ));
            }
            Input::AlteredRetry | Input::Gap => checks.push(check(
                "reconcile.conflicts_and_gaps_change_nothing",
                n == w && t.outputs.iter().all(Option::is_none),
            )),
            _ => {}
        }
        Ok(checks)
    }
}

impl Enumerate for Downloads {
    fn inputs(&self, _: &World) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Download,
            Input::Play(Position::Partial),
            Input::Play(Position::Complete),
            Input::Play(Position::Beyond),
            Input::Sync,
            Input::SyncLost,
            Input::AlteredRetry,
            Input::Gap,
            Input::Rebase,
            Input::OnlineProgress,
            Input::ManualOverride,
            Input::TimelineEdit,
            Input::Restore,
        ])
    }
}

fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
impl ModelCodec for Downloads {
    fn encode_state(&self, v: &World) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<World, ModelError> {
        decode(v)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
        decode(v)
    }
    fn encode_output(&self, v: &Self::Output) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}

#[test]
fn offline_reconciliation_exhausts_bounded_interleavings() {
    let report = stateless::explore::enumerate(
        &Downloads,
        stateless::explore::SearchConfig {
            max_states: 12_000_000,
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
        "Offline reconciliation: {} states, {} transitions; {MAX_SEQUENCES} device sequences, 2 online writes, 1 manual override, 1 timeline edit, 1 restore; download/rebase, partial/complete/beyond, sync, lost response, altered retry, gap",
        report.states, report.transitions
    );
}

#[test]
fn checks_detect_overwriting_newer_authority() {
    let mut w = Downloads.initial_state().unwrap();
    w = Downloads.step(&w, &Input::Download).unwrap().state;
    w = Downloads
        .step(&w, &Input::Play(Position::Partial))
        .unwrap()
        .state;
    w = Downloads.step(&w, &Input::OnlineProgress).unwrap().state;
    // A policy that applied stale offline progress over the newer session.
    let mut bad = w.clone();
    bad.view.position_ms = 40_000;
    bad.view.revision += 1;
    let mutation = Transition::accepted(bad, vec![Some((Outcome::Applied, None))]);
    assert!(
        Downloads
            .check_transition(&w, &Input::Sync, &mutation.as_ref())
            .unwrap()
            .iter()
            .any(|c| matches!(c.status, stateless::CheckStatus::Failed(_)))
    );
}
