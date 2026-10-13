//! Device transfer reports against the production policy
//! (`downloads::accept_transfer`).
//!
//! The download's device sends reports about its copy with its own
//! increasing sequence; the network delivers them in any order, any number
//! of times. The server cancels the download once (a new revision); the
//! device may learn the new revision and report its copy removed. The
//! oracle checks what the server stores against what the device sent.
use playscale_core::downloads::{
    TransferDecision, TransferRefusal, TransferReport, TransferState, accept_transfer,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

const SIZE: u64 = 100;
const MAX_REPORTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Kind {
    Partial,
    Verified,
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    /// The device sends a report of this kind (under the revision it knows).
    Report(Kind),
    /// The network delivers the n-th sent report (again, possibly).
    Deliver(usize),
    /// The server cancels the download: a new revision; it has ended.
    Cancel,
    /// The device reads the download again and learns its revision.
    Refresh,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct World {
    // Device.
    sent: Vec<TransferReport>,
    known_revision: u64,
    // Server.
    revision: u64,
    ended: bool,
    stored: Option<TransferReport>,
}

fn report(w: &World, kind: Kind) -> TransferReport {
    let (state, bytes_received) = match kind {
        Kind::Partial => (TransferState::Transferring, SIZE / 2),
        Kind::Verified => (TransferState::VerifiedLocal, SIZE),
        Kind::Removed => (TransferState::Removed, 0),
    };
    TransferReport {
        sequence: w.sent.len() as u64 + 1,
        download_revision: w.known_revision,
        state,
        bytes_received,
    }
}

type Output = Result<TransferDecision, TransferRefusal>;

struct Reports;

fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(name)
    } else {
        Check::failed(name, "transfer report obligation violated")
    }
}

impl Model for Reports {
    type State = World;
    type Input = Input;
    type Output = Output;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.download-transfer-reports".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/downloads.rs").into(),
        }
    }

    fn initial_state(&self) -> Result<World, ModelError> {
        Ok(World {
            sent: vec![],
            known_revision: 1,
            revision: 1,
            ended: false,
            stored: None,
        })
    }

    fn check_state(&self, w: &World) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            check(
                "reports.stored_only_what_the_device_sent",
                w.stored.is_none_or(|s| w.sent.contains(&s)),
            ),
            check(
                "reports.verified_only_when_complete",
                w.stored.is_none_or(|s| {
                    s.state != TransferState::VerifiedLocal || s.bytes_received == SIZE
                }),
            ),
        ])
    }

    fn step(&self, w: &World, input: &Input) -> Result<Transition<World, Output>, ModelError> {
        let mut n = w.clone();
        let mut out = vec![];
        match *input {
            Input::Report(kind) if w.sent.len() < MAX_REPORTS => n.sent.push(report(w, kind)),
            Input::Deliver(i) if i < w.sent.len() => {
                let incoming = w.sent[i];
                let decided =
                    accept_transfer(w.stored.as_ref(), &incoming, w.revision, SIZE, w.ended);
                if decided == Ok(TransferDecision::Record) {
                    n.stored = Some(incoming);
                }
                out.push(decided);
            }
            Input::Cancel if !w.ended => {
                n.ended = true;
                n.revision += 1;
            }
            Input::Refresh => n.known_revision = w.revision,
            _ => {}
        }
        Ok(Transition::accepted(n, out))
    }

    fn check_transition(
        &self,
        w: &World,
        input: &Input,
        t: &TransitionRef<'_, World, Output>,
    ) -> Result<Vec<Check>, ModelError> {
        let n = t.state;
        let mut checks = vec![check(
            "reports.never_move_backward",
            match (w.stored, n.stored) {
                (Some(before), Some(after)) => after.sequence >= before.sequence,
                (Some(_), None) => false,
                _ => true,
            },
        )];
        if let Input::Deliver(i) = *input
            && i < w.sent.len()
        {
            let incoming = w.sent[i];
            // Oracle: the outcome the device's order and the server's facts require.
            let expected = if incoming.download_revision != w.revision {
                Err(TransferRefusal::DownloadChanged)
            } else if w.stored.is_some_and(|s| s.sequence == incoming.sequence) {
                Ok(TransferDecision::Unchanged)
            } else if w.stored.is_some_and(|s| s.sequence > incoming.sequence) {
                Err(TransferRefusal::Stale)
            } else if w.ended && incoming.state != TransferState::Removed {
                Err(TransferRefusal::Ended)
            } else {
                Ok(TransferDecision::Record)
            };
            checks.push(check(
                "reports.required_outcome",
                t.outputs == [expected].as_slice(),
            ));
            checks.push(check(
                "reports.recorded_only_about_the_current_revision",
                n.stored == w.stored || n.stored.is_some_and(|s| s.download_revision == w.revision),
            ));
        }
        Ok(checks)
    }
}

impl Enumerate for Reports {
    fn inputs(&self, _: &World) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Report(Kind::Partial),
            Input::Report(Kind::Verified),
            Input::Report(Kind::Removed),
            Input::Cancel,
            Input::Refresh,
        ];
        inputs.extend((0..MAX_REPORTS).map(Input::Deliver));
        Ok(inputs)
    }
}

fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
impl ModelCodec for Reports {
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
    fn encode_output(&self, v: &Output) -> Result<Vec<u8>, ModelError> {
        encode(&format!("{v:?}"))
    }
}

#[test]
fn transfer_reports_exhaust_reordered_and_duplicated_delivery() {
    let report = stateless::explore::enumerate(
        &Reports,
        stateless::explore::SearchConfig {
            max_states: 1_000_000,
            max_transitions: 50_000_000,
            max_depth: 40,
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
        "Transfer reports: {} states, {} transitions; {MAX_REPORTS} reports (partial, verified, removed), delivery in any order and repeated, 1 cancellation, revision refresh",
        report.states, report.transitions
    );
}

#[test]
fn checks_detect_a_server_that_accepts_reports_out_of_order() {
    let mut w = Reports.initial_state().unwrap();
    for input in [
        Input::Report(Kind::Partial),
        Input::Report(Kind::Verified),
        Input::Deliver(1),
    ] {
        w = Reports.step(&w, &input).unwrap().state;
    }
    // A policy that let the earlier partial report replace the verified one.
    let mut bad = w.clone();
    bad.stored = Some(w.sent[0]);
    let mutation = Transition::accepted(bad, vec![Ok(TransferDecision::Record)]);
    assert!(
        Reports
            .check_transition(&w, &Input::Deliver(0), &mutation.as_ref())
            .unwrap()
            .iter()
            .any(|c| matches!(c.status, stateless::CheckStatus::Failed(_)))
    );
}
