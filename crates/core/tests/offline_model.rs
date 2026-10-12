use playscale_core::offline::{self, Admission, CacheScope, DownloadIdentity, OfflineEvent};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Generate, Model, ModelCodec, ModelError, ModelMetadata, Transition,
    TransitionRef,
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Forward,
    Rewind,
    Duplicate,
    AlterDuplicate,
    Gap,
    ForeignProfile,
    StaleManifest,
    Restart,
    /// The server acknowledged every retained event.
    AcknowledgeAll,
    /// The server acknowledged only the oldest retained event.
    AcknowledgeOne,
    /// An acknowledgement beyond anything this log wrote (restored server).
    ForeignAcknowledge,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Log {
    base: u64,
    events: Vec<OfflineEvent>,
}
impl Log {
    fn next(&self) -> u64 {
        self.base + self.events.len() as u64 + 1
    }
}
struct Offline;
fn scope() -> CacheScope {
    CacheScope {
        server_id: "server".into(),
        principal_id: "principal".into(),
        profile_id: "profile".into(),
        device_id: "device".into(),
    }
}
fn media() -> DownloadIdentity {
    DownloadIdentity {
        download_id: "download".into(),
        timeline_id: "timeline".into(),
        timeline_revision: "1".into(),
        source_revision: "source".into(),
        base_viewing_revision: "2".into(),
        base_manual_epoch: "3".into(),
        viewing_epoch: Some("server:restore".into()),
    }
}
fn event(log: &Log, i: &Input) -> OfflineEvent {
    let s = &log.events;
    let mut e = OfflineEvent {
        scope: scope(),
        media: media(),
        event_id: format!("event{}", log.next()),
        device_sequence: log.next().to_string(),
        position_ms: if matches!(i, Input::Rewind) { 0 } else { 100 },
        status: "paused".into(),
        device_time: None,
    };
    match i {
        Input::Duplicate | Input::AlterDuplicate => {
            if let Some(last) = s.last() {
                e = last.clone();
            }
            if matches!(i, Input::AlterDuplicate) {
                e.position_ms = if e.position_ms == 50 { 0 } else { 50 };
            }
        }
        Input::Gap => e.device_sequence = (log.next() + 1).to_string(),
        Input::ForeignProfile => e.scope.profile_id = "other".into(),
        Input::StaleManifest => e.media.base_manual_epoch = "0".into(),
        _ => {}
    }
    e
}
fn acknowledgement(s: &Log, i: &Input) -> Option<u64> {
    match i {
        Input::AcknowledgeAll => Some(s.next() - 1),
        Input::AcknowledgeOne => Some(s.base + u64::from(!s.events.is_empty())),
        Input::ForeignAcknowledge => Some(s.next()),
        _ => None,
    }
}
fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(name)
    } else {
        Check::failed(name, "offline event obligation violated")
    }
}
impl Model for Offline {
    type State = Log;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.offline-client-log".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 1,
            build: include_str!("../src/offline.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<Self::State, ModelError> {
        Ok(Log {
            base: 0,
            events: vec![],
        })
    }
    fn check_state(&self, s: &Self::State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "offline.bounded_consecutive_log_after_base",
            s.events.len() <= 4
                && s.events
                    .iter()
                    .enumerate()
                    .all(|(i, e)| e.device_sequence == (s.base + i as u64 + 1).to_string()),
        )])
    }
    fn step(
        &self,
        s: &Self::State,
        i: &Input,
    ) -> Result<Transition<Self::State, bool>, ModelError> {
        let mut next = s.clone();
        if matches!(i, Input::Restart) {
            return Ok(Transition::accepted(next, vec![]));
        }
        if let Some(through) = acknowledgement(s, i) {
            let compacted = offline::compact(s.base, &s.events, through);
            if let Some((base, events)) = &compacted {
                next = Log {
                    base: *base,
                    events: events.clone(),
                };
            }
            return Ok(Transition::accepted(next, vec![compacted.is_some()]));
        }
        let e = event(s, i);
        let decision = offline::admit(s.base, &s.events, &e, &scope(), &media(), 100, 4);
        if decision == Admission::Append {
            next.events.push(e);
        }
        Ok(Transition::accepted(
            next,
            vec![matches!(decision, Admission::Append | Admission::Duplicate)],
        ))
    }
    fn check_transition(
        &self,
        s: &Self::State,
        i: &Input,
        n: &TransitionRef<'_, Self::State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let append = match i {
            Input::Forward | Input::Rewind => s.events.len() < 4,
            Input::Duplicate | Input::AlterDuplicate => s.events.is_empty(),
            _ => false,
        };
        let mut expected = s.clone();
        let accept = match i {
            Input::AcknowledgeAll => {
                expected = Log {
                    base: s.next() - 1,
                    events: vec![],
                };
                true
            }
            Input::AcknowledgeOne => {
                if !s.events.is_empty() {
                    expected.base += 1;
                    expected.events.remove(0);
                }
                true
            }
            Input::ForeignAcknowledge => false,
            _ => append || matches!(i, Input::Duplicate) && !s.events.is_empty(),
        };
        if append {
            expected.events.push(event(s, i));
        }
        Ok(vec![
            check(
                "offline.compaction_never_renumbers",
                n.state.next() >= s.next() && n.state.base >= s.base,
            ),
            check("offline.exact_event_and_count", n.state == &expected),
            check(
                "offline.required_acceptance_and_rejection",
                if matches!(i, Input::Restart) {
                    n.outputs.is_empty()
                } else {
                    n.outputs == [accept]
                },
            ),
            check(
                "offline.identity_and_causality",
                n.state
                    .events
                    .iter()
                    .all(|e| e.scope == scope() && e.media == media()),
            ),
        ])
    }
}
impl Enumerate for Offline {
    fn inputs(&self, _: &Self::State) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Forward,
            Input::Rewind,
            Input::Duplicate,
            Input::AlterDuplicate,
            Input::Gap,
            Input::ForeignProfile,
            Input::StaleManifest,
            Input::Restart,
            Input::AcknowledgeAll,
            Input::AcknowledgeOne,
            Input::ForeignAcknowledge,
        ])
    }
}
impl Generate for Offline {
    fn generate(
        &self,
        s: &Self::State,
        rng: &mut stateless::Rng,
    ) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
impl ModelCodec for Offline {
    fn encode_state(&self, v: &Self::State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<Self::State, ModelError> {
        decode(v)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
        decode(v)
    }
    fn encode_output(&self, v: &bool) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}
#[test]
fn offline_log_bounded_fault_interleavings() {
    let report = stateless::explore::fuzz(
        &Offline,
        stateless::explore::FuzzConfig {
            seed: 20261010,
            cases: 1000,
            max_steps: 50,
            max_transitions: 50000,
            mutation_percent: 50,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    println!(
        "Offline log: {} cases, {} transitions; 4 retained records; forward/rewind, exact/altered duplicate, sequence gap, foreign scope, stale epoch, restart, full/partial/foreign acknowledgement compaction; four transition properties",
        report.cases, report.transitions
    );
}
#[test]
fn checks_detect_lost_valid_events() {
    let s = Offline.initial_state().unwrap();
    let mutation = Transition::accepted(s.clone(), vec![false]);
    assert!(
        Offline
            .check_transition(&s, &Input::Forward, &mutation.as_ref())
            .unwrap()
            .iter()
            .any(|c| matches!(c.status, stateless::CheckStatus::Failed(_)))
    );
}
#[test]
fn checks_detect_renumbering_and_foreign_erasure() {
    let mut s = Offline.initial_state().unwrap();
    s.events.push(event(&s, &Input::Forward));
    let reset = Transition::accepted(Offline.initial_state().unwrap(), vec![true]);
    let erased = Transition::accepted(
        Log {
            base: 1,
            events: vec![],
        },
        vec![true],
    );
    for (input, mutation) in [
        (Input::AcknowledgeAll, reset),
        (Input::ForeignAcknowledge, erased),
    ] {
        assert!(
            Offline
                .check_transition(&s, &input, &mutation.as_ref())
                .unwrap()
                .iter()
                .any(|c| matches!(c.status, stateless::CheckStatus::Failed(_)))
        );
    }
}
