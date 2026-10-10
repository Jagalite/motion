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
    }
}
fn event(s: &[OfflineEvent], i: &Input) -> OfflineEvent {
    let mut e = OfflineEvent {
        scope: scope(),
        media: media(),
        event_id: format!("event{}", s.len() + 1),
        device_sequence: (s.len() + 1).to_string(),
        position_ms: if matches!(i, Input::Rewind) { 0 } else { 100 },
        status: "paused".into(),
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
        Input::Gap => e.device_sequence = (s.len() + 2).to_string(),
        Input::ForeignProfile => e.scope.profile_id = "other".into(),
        Input::StaleManifest => e.media.base_manual_epoch = "0".into(),
        _ => {}
    }
    e
}
fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(name)
    } else {
        Check::failed(name, "offline event obligation violated")
    }
}
impl Model for Offline {
    type State = Vec<OfflineEvent>;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.offline-client-log".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/offline.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<Self::State, ModelError> {
        Ok(vec![])
    }
    fn check_state(&self, s: &Self::State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "offline.bounded_consecutive_log",
            s.len() <= 4
                && s.iter()
                    .enumerate()
                    .all(|(i, e)| e.device_sequence == (i + 1).to_string()),
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
        let e = event(s, i);
        let decision = offline::admit(s, &e, &scope(), &media(), 100, 4);
        if decision == Admission::Append {
            next.push(e);
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
            Input::Forward | Input::Rewind => s.len() < 4,
            Input::Duplicate | Input::AlterDuplicate => s.is_empty(),
            _ => false,
        };
        let accept = append || matches!(i, Input::Duplicate) && !s.is_empty();
        let mut expected = s.clone();
        if append {
            expected.push(event(s, i));
        }
        Ok(vec![
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
        "Offline log: {} cases, {} transitions; 4 records; forward/rewind, exact/altered duplicate, sequence gap, foreign scope, stale epoch, restart; three transition properties",
        report.cases, report.transitions
    );
}
#[test]
fn checks_detect_lost_valid_events() {
    let s = vec![];
    let mutation = Transition::accepted(s.clone(), vec![false]);
    assert!(
        Offline
            .check_transition(&s, &Input::Forward, &mutation.as_ref())
            .unwrap()
            .iter()
            .any(|c| matches!(c.status, stateless::CheckStatus::Failed(_)))
    );
}
