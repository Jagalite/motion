//! Exact-root identity and ownership decisions shared by registration adapters.
use playscale_core::sources::{self, ExistingRoot, OwnershipRequirement, Registration};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    existing: Option<(bool, bool)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    identity: bool,
    strict: bool,
    managed: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Created,
    Reused(String),
    RelocationRequired,
    OwnershipConflict,
}
struct Sources;
impl Model for Sources {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.source_registration".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/sources.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State { existing: None })
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(Vec::new())
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let observed = s.existing.map(|(identity, managed)| ExistingRoot {
            id: "source-1",
            identity: if identity { "disk-a" } else { "disk-b" },
            managed,
        });
        let requirement = if i.strict {
            OwnershipRequirement::Exact(i.managed)
        } else {
            OwnershipRequirement::ExistingKind
        };
        let effect = match sources::registration(
            if i.identity { "disk-a" } else { "disk-b" },
            requirement,
            observed,
        ) {
            Registration::Create => {
                next.existing = Some((i.identity, i.managed));
                Effect::Created
            }
            Registration::Reuse { id } => Effect::Reused(id),
            Registration::RelocationRequired => Effect::RelocationRequired,
            Registration::OwnershipConflict => Effect::OwnershipConflict,
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_transition(
        &self,
        b: &State,
        i: &Input,
        n: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match b.existing {
            None => Effect::Created,
            Some((identity, _)) if identity != i.identity => Effect::RelocationRequired,
            Some((_, managed)) if i.strict && managed != i.managed => Effect::OwnershipConflict,
            Some(_) => Effect::Reused("source-1".into()),
        };
        let stable = b.existing.is_none() || n.state == b;
        Ok(vec![if stable && n.outputs == [expected] {
            Check::passed("source.exact_identity_and_ownership")
        } else {
            Check::failed(
                "source.exact_identity_and_ownership",
                "registration changed the wrong root or returned the wrong decision",
            )
        }])
    }
}
impl Enumerate for Sources {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok([false, true]
            .into_iter()
            .flat_map(|identity| {
                [false, true].into_iter().flat_map(move |strict| {
                    [false, true].into_iter().map(move |managed| Input {
                        identity,
                        strict,
                        managed,
                    })
                })
            })
            .collect())
    }
}
impl ModelCodec for Sources {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        dec(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        dec(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
        enc(v)
    }
}
fn enc<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn dec<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(b).map_err(|e| ModelError::new(e.to_string()))
}
#[test]
fn exact_root_registration_interleavings() {
    let report = stateless::explore::enumerate(
        &Sources,
        stateless::explore::SearchConfig {
            max_states: 100,
            max_transitions: 1000,
            max_depth: 10,
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
        "Source registration: {} states, {} transitions; two identities and ownership kinds",
        report.states, report.transitions
    );
}
