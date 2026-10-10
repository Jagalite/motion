use playscale_core::delivery_admission::{self as core, Decision, Identity, Receipt};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    receipts: BTreeMap<String, Receipt>,
    started: Vec<String>,
    live: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Commit {
    Rollback,
    CommittedThenCrashed,
    Dispatch,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Input {
    Request {
        principal: u8,
        digest: u8,
        commit: Commit,
    },
    Retire,
    Restart,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Start(String),
    Ack(String),
    Conflict,
}
struct Admission;
impl Model for Admission {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.delivery-admission".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/delivery_admission.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            receipts: BTreeMap::new(),
            started: vec![],
            live: vec![],
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut n = s.clone();
        let mut output = vec![];
        match input {
            Input::Retire | Input::Restart => n.live.clear(),
            Input::Request {
                principal,
                digest,
                commit,
            } => {
                let identity = Identity {
                    principal: principal.to_string(),
                    key: "key".into(),
                    digest: digest.to_string(),
                };
                match core::decide(&identity, s.receipts.get(&identity.principal)) {
                    Ok(Decision::Create) if !matches!(commit, Commit::Rollback) => {
                        let id = format!("delivery-{}", s.receipts.len());
                        n.receipts.insert(
                            identity.principal.clone(),
                            Receipt {
                                identity,
                                delivery_id: id.clone(),
                            },
                        );
                        if *commit == Commit::Dispatch {
                            n.started.push(id.clone());
                            n.live.push(id.clone());
                            output.extend([Effect::Start(id.clone()), Effect::Ack(id)]);
                        }
                    }
                    Ok(Decision::Create) => {}
                    Ok(Decision::Replay { delivery_id }) => output.push(Effect::Ack(delivery_id)),
                    Err(_) => output.push(Effect::Conflict),
                }
            }
        }
        Ok(Transition::accepted(n, output))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "at_most_one_start_per_receipt",
            s.started
                .iter()
                .all(|id| s.started.iter().filter(|other| *other == id).count() == 1),
        )])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        t: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let mut checks = vec![check(
            "receipts_survive_retirement_and_restart",
            s.receipts
                .iter()
                .all(|(key, r)| t.state.receipts.get(key) == Some(r)),
        )];
        if let Input::Request {
            principal,
            digest,
            commit,
        } = i
        {
            let existing = s.receipts.get(&principal.to_string());
            let expected = if let Some(receipt) = existing {
                if receipt.identity.digest == digest.to_string() {
                    vec![Effect::Ack(receipt.delivery_id.clone())]
                } else {
                    vec![Effect::Conflict]
                }
            } else if *commit == Commit::Dispatch {
                let id = format!("delivery-{}", s.receipts.len());
                vec![Effect::Start(id.clone()), Effect::Ack(id)]
            } else {
                vec![]
            };
            checks.push(check(
                "exact_ack_conflict_and_start_effects",
                t.outputs == expected,
            ));
            checks.push(check(
                "rollback_and_replay_do_not_change_admission",
                (existing.is_none() && *commit != Commit::Rollback) || t.state == s,
            ));
        }
        Ok(checks)
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "delivery admission contract violated")
    }
}
impl Enumerate for Admission {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![Input::Retire, Input::Restart];
        for principal in 0..2 {
            for digest in 0..2 {
                for commit in [
                    Commit::Rollback,
                    Commit::CommittedThenCrashed,
                    Commit::Dispatch,
                ] {
                    inputs.push(Input::Request {
                        principal,
                        digest,
                        commit,
                    });
                }
            }
        }
        Ok(inputs)
    }
}
impl ModelCodec for Admission {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
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
fn admission_graph_covers_lost_ack_retirement_restart_conflict_and_rollback() {
    let report = stateless::explore::enumerate(&Admission, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Admission: {} states, {} transitions; two principals, two digests, rollback/crash-after-commit/dispatch, retirement and restart",
        report.states, report.transitions
    );
}
