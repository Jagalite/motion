use playscale_core::execution_deadline::{Deadline, Expired};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    deadline: Deadline,
    now: u64,
    advanced: bool,
    largest_observation: u64,
}
struct Liveness {
    expected: Option<u64>,
}
impl Liveness {
    fn hard_expiry(&self, now: u64) -> Option<Expired> {
        if now >= 8 {
            Some(Expired::Total)
        } else if self.expected.is_some_and(|limit| now >= limit) {
            Some(Expired::ExpectedDuration)
        } else {
            None
        }
    }
}
impl Model for Liveness {
    type State = State;
    type Input = u64;
    type Output = Expired;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: format!("playscale.execution-deadline.{:?}", self.expected),
            model_version: 2,
            properties_version: 2,
            codec_version: 2,
            build: include_str!("../src/execution_deadline.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            deadline: Deadline::new(8, Some(3), Some(2)).with_expected_duration(self.expected),
            now: 0,
            advanced: false,
            largest_observation: 0,
        })
    }
    fn step(&self, s: &State, position: &u64) -> Result<Transition<State, Expired>, ModelError> {
        let mut n = s.clone();
        n.now += 1;
        n.deadline.observe(n.now, *position);
        n.advanced |= *position > 0 && n.now < 3;
        n.largest_observation = n.largest_observation.max(*position);
        let output = n.deadline.expired(n.now).into_iter().collect();
        Ok(Transition::accepted(n, output))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            if s.now < 8 || s.deadline.expired(s.now) == Some(Expired::Total) {
                Check::passed("total_expiry_is_durable")
            } else {
                Check::failed("total_expiry_is_durable", "expired execution became live")
            },
        ])
    }
    fn check_transition(
        &self,
        s: &State,
        position: &u64,
        t: &TransitionRef<'_, State, Expired>,
    ) -> Result<Vec<Check>, ModelError> {
        let n = t.state;
        let old_expired = s.deadline.expired(s.now);
        let advances_in_time =
            *position > s.largest_observation && s.deadline.expired(n.now).is_none();
        let checks = [
            (
                "non_advancement_does_not_renew",
                *position > s.largest_observation
                    || n.deadline.expired(n.now) == s.deadline.expired(n.now),
            ),
            (
                "advancement_grants_exact_stall_window",
                !advances_in_time
                    || (n.deadline.expired(n.now + 1) == self.hard_expiry(n.now + 1)
                        && n.deadline.expired(n.now + 2)
                            == Some(self.hard_expiry(n.now + 2).unwrap_or(Expired::NoProgress))),
            ),
            (
                "expired_never_revives",
                old_expired.is_none() || !t.outputs.is_empty(),
            ),
            (
                "total_is_absolute",
                n.now < 8 || t.outputs == [Expired::Total],
            ),
            (
                "zero_cannot_start",
                s.advanced || *position > 0 || n.now < 3 || !t.outputs.is_empty(),
            ),
            (
                "expected_duration_is_absolute",
                self.hard_expiry(n.now)
                    .is_none_or(|reason| t.outputs == [reason]),
            ),
            ("one_expiry_reason", t.outputs.len() <= 1),
        ];
        Ok(checks
            .into_iter()
            .map(|(id, ok)| {
                if ok {
                    Check::passed(id)
                } else {
                    Check::failed(id, "execution liveness violated")
                }
            })
            .collect())
    }
}
impl Enumerate for Liveness {
    fn inputs(&self, s: &State) -> Result<Vec<u64>, ModelError> {
        Ok(if s.now < 9 {
            vec![0, 1, 2, 3, u64::MAX]
        } else {
            vec![]
        })
    }
}
impl ModelCodec for Liveness {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &u64) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<u64, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Expired) -> Result<Vec<u8>, ModelError> {
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
fn liveness_graph_covers_stalls_duplicates_regressions_and_expiry() {
    for expected in [
        None,
        Some(0),
        Some(1),
        Some(4),
        Some(8),
        Some(9),
        Some(u64::MAX),
    ] {
        let report =
            stateless::explore::enumerate(&Liveness { expected }, Default::default()).unwrap();
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(report.skipped_checks, 0);
        assert_eq!(
            report.termination,
            stateless::explore::SearchTermination::GraphExhausted
        );
        println!(
            "Liveness expected={expected:?}: {} states, {} transitions; elapsed 0..9, positions 0/1/2/3/u64::MAX",
            report.states, report.transitions
        );
    }
}
