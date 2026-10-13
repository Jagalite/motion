//! The real per-artifact policy, with at most three simultaneous readers.
//! RAII identity and blocking filesystem lifetime are checked by adapter tests.
use playscale_core::maintenance::{ArtifactUse as State, ArtifactUseInput as Input};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

struct Leases;
impl Model for Leases {
    type State = State;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.artifact-leases".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/maintenance.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State::default())
    }
    fn step(&self, state: &State, input: &Input) -> Result<Transition<State, bool>, ModelError> {
        let next = state.transition(*input);
        Ok(Transition::accepted(
            next.unwrap_or(*state),
            vec![next.is_some()],
        ))
    }
    fn check_state(&self, state: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![check(
            "reader_bound",
            !matches!(state, State::Readers(n) if *n > 3),
        )])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match (*before, *input) {
            (State::Readers(n), Input::Pin) => Some(State::Readers(n + 1)),
            (State::Readers(n), Input::ReleasePin) if n > 0 => Some(State::Readers(n - 1)),
            (State::Readers(0), Input::ClaimCleanup) => Some(State::Cleaning),
            (State::Cleaning, Input::ReleaseCleanup) => Some(State::Readers(0)),
            _ => None,
        };
        Ok(vec![
            check(
                "exact_admission_and_release",
                next.state == &expected.unwrap_or(*before) && next.outputs == [expected.is_some()],
            ),
            check(
                "cleanup_excludes_readers",
                !matches!(before, State::Readers(n) if *n > 0) || *next.state != State::Cleaning,
            ),
            check(
                "cleaning_refuses_readers",
                *before != State::Cleaning
                    || *input != Input::Pin
                    || (next.state == before && next.outputs == [false]),
            ),
            check(
                "refusal_preserves_ownership",
                expected.is_some() || next.state == before,
            ),
        ])
    }
}
fn check(name: &'static str, value: bool) -> Check {
    if value {
        Check::passed(name)
    } else {
        Check::failed(name, "artifact ownership invariant")
    }
}
impl Enumerate for Leases {
    fn inputs(&self, state: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::ReleasePin,
            Input::ClaimCleanup,
            Input::ReleaseCleanup,
        ];
        if *state != State::Readers(3) {
            inputs.push(Input::Pin);
        }
        Ok(inputs)
    }
}
impl ModelCodec for Leases {
    fn encode_state(&self, value: &State) -> Result<Vec<u8>, ModelError> {
        encode(value)
    }
    fn decode_state(&self, bytes: &[u8]) -> Result<State, ModelError> {
        decode(bytes)
    }
    fn encode_input(&self, value: &Input) -> Result<Vec<u8>, ModelError> {
        encode(value)
    }
    fn decode_input(&self, bytes: &[u8]) -> Result<Input, ModelError> {
        decode(bytes)
    }
    fn encode_output(&self, value: &bool) -> Result<Vec<u8>, ModelError> {
        encode(value)
    }
}
fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(value).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(bytes).map_err(|e| ModelError::new(e.to_string()))
}
#[test]
fn bounded_readers_and_cleanup_explore_production_policy() {
    let report = stateless::explore::enumerate(&Leases, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Artifact leases: {} states, {} transitions; readers 0..3, four ownership properties",
        report.states, report.transitions
    );
    assert_eq!(State::Readers(u32::MAX).transition(Input::Pin), None);
    assert_eq!(State::Readers(0).transition(Input::ReleasePin), None);
}
