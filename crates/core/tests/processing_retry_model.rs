//! Bounded processing retry interleavings using the production admission rule.
use playscale_core::{
    jobs::{Job, Phase},
    processing,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    jobs: [Job; 2],
    background: u8,
    revisions: [u8; 2],
    reviewed: [u8; 2],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Retry(usize),
    Review(usize),
    Release,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Retried(usize),
    Refused(usize, String),
}
struct Retries;
impl Model for Retries {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.processing_retry".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/processing.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            jobs: [
                Job {
                    phase: Phase::Failed,
                    attempt: 1,
                },
                Job {
                    phase: Phase::Cancelled,
                    attempt: 2,
                },
            ],
            background: 99,
            revisions: [1, 1],
            reviewed: [1, 1],
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let mut out = vec![];
        match *i {
            Input::Review(n) => next.reviewed[n] = next.revisions[n],
            Input::Release => next.background = next.background.saturating_sub(1),
            Input::Retry(n) => {
                let active = i64::from(s.background)
                    + s.jobs.iter().filter(|j| j.phase == Phase::Queued).count() as i64;
                match processing::retry(
                    &s.jobs[n],
                    Some((s.revisions[n].into(), s.reviewed[n].into())),
                    active,
                ) {
                    Ok(job) => {
                        next.jobs[n] = job;
                        next.revisions[n] += 1;
                        out.push(Effect::Retried(n));
                    }
                    Err(e) => out.push(Effect::Refused(n, format!("{e:?}"))),
                }
            }
        }
        Ok(Transition::accepted(next, out))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let active =
            usize::from(s.background) + s.jobs.iter().filter(|j| j.phase == Phase::Queued).count();
        Ok(vec![check("processing.active_at_most_100", active <= 100)])
    }
    fn check_transition(
        &self,
        b: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let valid = if let Input::Retry(index) = *i {
            let active = usize::from(b.background)
                + b.jobs.iter().filter(|j| j.phase == Phase::Queued).count();
            let reason = if b.revisions[index] != b.reviewed[index] {
                Some("StaleRevision")
            } else if !b.jobs[index].retryable() {
                Some("NotRetryable")
            } else if active >= 100 {
                Some("QueueFull")
            } else {
                None
            };
            match reason {
                Some(reason) => {
                    n.state == b && n.outputs == [Effect::Refused(index, reason.into())]
                }
                None => {
                    n.state.jobs[index].phase == Phase::Queued
                        && n.state.jobs[index].attempt == b.jobs[index].attempt
                        && n.outputs == [Effect::Retried(index)]
                }
            }
        } else {
            true
        };
        Ok(vec![check("processing.retry_exact_outcome", valid)])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "retry invariant violated")
    }
}
impl Enumerate for Retries {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut v = vec![
            Input::Retry(0),
            Input::Retry(1),
            Input::Review(0),
            Input::Review(1),
        ];
        if s.background > 98 {
            v.push(Input::Release);
        }
        Ok(v)
    }
}
impl ModelCodec for Retries {
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
fn bounded_retries_keep_capacity_and_exact_outcomes() {
    let report = stateless::explore::enumerate(
        &Retries,
        stateless::explore::SearchConfig {
            max_states: 10000,
            max_transitions: 100000,
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
        "Processing retry: {} states, {} edges; two jobs, background 98..99, reviewed/current revisions",
        report.states, report.transitions
    );
}
