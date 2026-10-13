//! Bounded caller-source reauthentication using the production core policy.
use playscale_core::access::{
    AccessError, Authentication, Credential, CredentialKind, Device, Grant, Mode, Permission,
    Policy, Principal, authenticate_caller,
};
use sha2::{Digest, Sha256};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    credential: bool,
    parent: bool,
    device: bool,
    revoked: bool,
    rotated: bool,
    downgraded: bool,
    now: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Operator,
    Device,
    Session,
    Ingress,
    WrongIngressDevice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    DeleteCredential,
    DeleteParent,
    DeleteDevice,
    Revoke,
    Rotate,
    Downgrade,
    At(i64),
    Reauthenticate(Source),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Changed,
    Authenticated(Result<Principal, AccessError>),
}

fn grant(downgraded: bool) -> Grant {
    Grant {
        profile_ids: ["p".into()].into(),
        permissions: if downgraded {
            [Permission::EventsRead].into()
        } else {
            [Permission::EventsRead, Permission::SystemAdmin].into()
        },
    }
}

fn observe(s: &State, source: Source) -> Result<Principal, AccessError> {
    let device = Device {
        id: "d".into(),
        revision: if s.downgraded { 2 } else { 1 },
        grant: grant(s.downgraded),
        policy: Policy::default(),
        revoked: s.revoked,
        generation: if s.rotated { 2 } else { 1 },
        credential_expires_at: Some(100),
    };
    let parent = Credential {
        device_id: "d".into(),
        kind: CredentialKind::Device,
        generation: 1,
        expires_at: 100,
    };
    let credential = Credential {
        kind: if matches!(source, Source::Session) {
            CredentialKind::Session
        } else {
            CredentialKind::Device
        },
        ..parent.clone()
    };
    let device = s.device.then_some(&device);
    let facts = match source {
        Source::Operator => Authentication::VerifiedLocalOperator,
        Source::Device | Source::Session => Authentication::Credential {
            credential: s.credential.then_some(&credential),
            parent: (s.parent && matches!(source, Source::Session)).then_some(&parent),
            device,
        },
        Source::Ingress | Source::WrongIngressDevice => Authentication::TrustedIngress {
            device_id: if matches!(source, Source::WrongIngressDevice) {
                "other"
            } else {
                "d"
            },
            device,
        },
    };
    authenticate_caller(facts, s.now)
}

// Independent outcome specification, including exact grants and rejection
// precedence. This does not call the production authentication helpers.
fn expected(s: &State, source: Source) -> Result<Principal, AccessError> {
    if matches!(source, Source::Operator) {
        return Ok(Principal::operator());
    }
    let ingress = matches!(source, Source::Ingress | Source::WrongIngressDevice);
    if !s.device
        || s.revoked
        || matches!(source, Source::WrongIngressDevice)
        || (!ingress && !s.credential)
    {
        return Err(AccessError::CredentialRevoked);
    }
    if !ingress {
        if s.rotated {
            return Err(AccessError::CredentialSuperseded);
        }
        if matches!(source, Source::Session) && !s.parent {
            return Err(AccessError::CredentialRevoked);
        }
        if s.now >= 100 {
            return Err(AccessError::CredentialExpired);
        }
    }
    Ok(Principal {
        id: "d".into(),
        device_id: Some("d".into()),
        mode: if ingress {
            Mode::TrustedPrivate
        } else {
            Mode::Paired
        },
        grant: grant(s.downgraded || ingress),
        policy: Policy::default(),
        policy_revision: if s.downgraded { 2 } else { 1 },
    })
}

struct Callers;
impl Model for Callers {
    type State = State;
    type Input = Input;
    type Output = Effect;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.caller_sources".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            // Fingerprint the complete source without embedding an unbounded string
            // in the trace header (Stateless defaults to 64 KiB per string).
            build: format!(
                "sha256:{:x}",
                Sha256::digest(include_bytes!("../src/access.rs"))
            ),
        }
    }

    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            credential: true,
            parent: true,
            device: true,
            revoked: false,
            rotated: false,
            downgraded: false,
            now: 0,
        })
    }

    fn step(&self, state: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = state.clone();
        let output = match input {
            Input::Reauthenticate(source) => Effect::Authenticated(observe(state, *source)),
            _ => {
                match input {
                    Input::DeleteCredential => next.credential = false,
                    Input::DeleteParent => next.parent = false,
                    Input::DeleteDevice => next.device = false,
                    Input::Revoke => next.revoked = true,
                    Input::Rotate => next.rotated = true,
                    Input::Downgrade => next.downgraded = true,
                    Input::At(now) => next.now = *now,
                    Input::Reauthenticate(_) => unreachable!(),
                }
                Effect::Changed
            }
        };
        Ok(Transition::accepted(next, vec![output]))
    }

    fn check_state(&self, state: &State) -> Result<Vec<Check>, ModelError> {
        // Check every source after every mutation, including terminal states.
        Ok([
            Source::Operator,
            Source::Device,
            Source::Session,
            Source::Ingress,
            Source::WrongIngressDevice,
        ]
        .into_iter()
        .map(|source| {
            if observe(state, source) == expected(state, source) {
                Check::passed("caller.current_source_facts")
            } else {
                Check::failed("caller.current_source_facts", "unexpected authority")
            }
        })
        .collect())
    }

    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let exact = match (input, next.outputs) {
            (Input::Reauthenticate(source), [Effect::Authenticated(result)]) => {
                next.state == before && *result == expected(before, *source)
            }
            (Input::Reauthenticate(_), _) => false,
            (_, [Effect::Changed]) => true,
            _ => false,
        };
        Ok(vec![if exact {
            Check::passed("caller.exact_current_authority_and_single_outcome")
        } else {
            Check::failed(
                "caller.exact_current_authority_and_single_outcome",
                "unexpected authority or effect",
            )
        }])
    }
}

impl Enumerate for Callers {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::DeleteCredential,
            Input::DeleteParent,
            Input::DeleteDevice,
            Input::Revoke,
            Input::Rotate,
            Input::Downgrade,
        ];
        for time in [99, 100] {
            if time > s.now {
                inputs.push(Input::At(time));
            }
        }
        inputs.extend(
            [
                Source::Operator,
                Source::Device,
                Source::Session,
                Source::Ingress,
                Source::WrongIngressDevice,
            ]
            .into_iter()
            .map(Input::Reauthenticate),
        );
        Ok(inputs)
    }
}

#[test]
fn caller_sources_never_infer_operator_from_missing_facts() {
    let report = stateless::explore::enumerate(
        &Callers,
        stateless::explore::SearchConfig {
            max_states: 1_000,
            max_transitions: 20_000,
            max_depth: 20,
        },
    )
    .unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    assert_eq!(report.states, 192);
    println!(
        "Caller model: {} states, {} transitions; one device and parent/session chain, five explicit sources, deletion/revoke/rotation/downgrade interleavings, time 0/99/100",
        report.states, report.transitions
    );
}
