//! Bounded admission interleavings: source/layout invalidation, revocation,
//! equivalent publication/cancellation and capacity changes between requests.
use playscale_core::previews::{Admission, AdmissionFacts};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
struct Previews;
fn facts(state: u8) -> AdmissionFacts {
    AdmissionFacts {
        source_current: state & 1 != 0,
        requester_authorized: state & 2 != 0,
        layout_valid: state & 4 != 0,
        equivalent_work: state & 8 != 0,
        active_jobs: if state & 16 != 0 { 100 } else { 99 },
        capacity: 100,
    }
}
impl Model for Previews {
    type State = u8;
    type Input = u8;
    type Output = u8;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.preview-admission".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/previews.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<u8, ModelError> {
        Ok(7)
    }
    fn step(&self, s: &u8, i: &u8) -> Result<Transition<u8, u8>, ModelError> {
        if *i == 0 {
            let enqueue = facts(*s).decide() == Admission::Enqueue;
            Ok(Transition::accepted(
                if enqueue { s | 8 } else { *s },
                if enqueue { vec![1] } else { vec![] },
            ))
        } else {
            Ok(Transition::accepted(s ^ i, vec![]))
        }
    }
    fn check_state(&self, _: &u8) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        s: &u8,
        i: &u8,
        t: &TransitionRef<'_, u8, u8>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = *i == 0 && s & 7 == 7 && s & 24 == 0;
        let valid = t.outputs == if expected { vec![1] } else { vec![] }
            && (*i != 0 || *t.state == if expected { s | 8 } else { *s });
        Ok(vec![if valid {
            Check::passed("one_enqueue_only_when_current_authorized_unique_and_capacity")
        } else {
            Check::failed(
                "one_enqueue_only_when_current_authorized_unique_and_capacity",
                "wrong enqueue count or state",
            )
        }])
    }
}
impl Enumerate for Previews {
    fn inputs(&self, _: &u8) -> Result<Vec<u8>, ModelError> {
        Ok(vec![0, 1, 2, 4, 8, 16])
    }
}
impl ModelCodec for Previews {
    fn encode_state(&self, v: &u8) -> Result<Vec<u8>, ModelError> {
        Ok(vec![*v])
    }
    fn decode_state(&self, v: &[u8]) -> Result<u8, ModelError> {
        v.first()
            .copied()
            .ok_or_else(|| ModelError::new("empty state"))
    }
    fn encode_input(&self, v: &u8) -> Result<Vec<u8>, ModelError> {
        Ok(vec![*v])
    }
    fn decode_input(&self, v: &[u8]) -> Result<u8, ModelError> {
        self.decode_state(v)
    }
    fn encode_output(&self, v: &u8) -> Result<Vec<u8>, ModelError> {
        Ok(vec![*v])
    }
}
#[test]
fn admission_interleavings_use_production_policy() {
    let report = stateless::explore::enumerate(&Previews, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
}
