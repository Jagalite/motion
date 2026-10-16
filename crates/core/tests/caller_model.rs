//! Bounded caller-source reauthentication using the production core policy.
use playscale_core::access::{
    AccessError, AccessMode, Authentication, Credential, CredentialKind, Device, Grant,
    HouseholdScope, Mode, Permission, Policy, Principal, Recheck, authenticate_caller, recheck,
};
use sha2::{Digest, Sha256};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    credential: bool,
    parent: bool,
    device: bool,
    revoked: bool,
    rotated: bool,
    downgraded: bool,
    now: i64,
    /// Deployment mode, as chosen at the last (re)start.
    household: bool,
    library: bool,
    profile: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Operator,
    Device,
    Session,
    Ingress,
    WrongIngressDevice,
    /// No bearer token and no session cookie.
    Anonymous,
    /// A credential was presented but did not resolve (malformed, unknown).
    RejectedCredential,
}

const SOURCES: [Source; 7] = [
    Source::Operator,
    Source::Device,
    Source::Session,
    Source::Ingress,
    Source::WrongIngressDevice,
    Source::Anonymous,
    Source::RejectedCredential,
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    DeleteCredential,
    DeleteParent,
    DeleteDevice,
    Revoke,
    Rotate,
    Downgrade,
    At(i64),
    Restart { household: bool },
    AddLibrary,
    RemoveLibrary,
    AddProfile,
    RemoveProfile,
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
    let library_ids: BTreeSet<String> = s.library.then(|| "l".to_string()).into_iter().collect();
    let profile_ids: BTreeSet<String> = s.profile.then(|| "p".to_string()).into_iter().collect();
    let mode = if s.household {
        AccessMode::TrustedHousehold
    } else {
        AccessMode::Restricted
    };
    let facts = match source {
        Source::Anonymous | Source::RejectedCredential => Authentication::Household {
            mode,
            presented: matches!(source, Source::RejectedCredential),
            scope: HouseholdScope {
                library_ids: &library_ids,
                profile_ids: &profile_ids,
            },
        },
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
    if matches!(source, Source::Anonymous | Source::RejectedCredential) {
        // Only a credential-less request in a household deployment, and then
        // exactly the viewer grant over what exists now; never administration.
        if !s.household || matches!(source, Source::RejectedCredential) {
            return Err(AccessError::Unauthenticated);
        }
        return Ok(Principal {
            id: "household".into(),
            device_id: None,
            mode: Mode::Household,
            grant: Grant {
                profile_ids: s.profile.then(|| "p".to_string()).into_iter().collect(),
                permissions: [
                    Permission::CatalogRead,
                    Permission::PlaybackRequest,
                    Permission::ViewingWrite,
                    Permission::EventsRead,
                ]
                .into(),
            },
            policy: Policy {
                library_ids: s.library.then(|| "l".to_string()).into_iter().collect(),
                ..Policy::default()
            },
            // A scope digest; compared by `scoped` and the stream property below.
            policy_revision: 0,
        });
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

/// Household revisions are scope digests: specified by how they change
/// (checked per transition), not by value.
fn scoped(result: Result<Principal, AccessError>) -> Result<Principal, AccessError> {
    result.map(|mut p| {
        if p.mode == Mode::Household {
            p.policy_revision = 0;
        }
        p
    })
}

fn scope_change(input: &Input) -> bool {
    matches!(
        input,
        Input::AddLibrary | Input::RemoveLibrary | Input::AddProfile | Input::RemoveProfile
    )
}

struct Callers;
impl Model for Callers {
    type State = State;
    type Input = Input;
    type Output = Effect;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.caller_sources".into(),
            model_version: 2,
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
            household: false,
            library: true,
            profile: true,
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
                    Input::Restart { household } => next.household = *household,
                    Input::AddLibrary => next.library = true,
                    Input::RemoveLibrary => next.library = false,
                    Input::AddProfile => next.profile = true,
                    Input::RemoveProfile => next.profile = false,
                    Input::Reauthenticate(_) => unreachable!(),
                }
                Effect::Changed
            }
        };
        Ok(Transition::accepted(next, vec![output]))
    }

    fn check_state(&self, state: &State) -> Result<Vec<Check>, ModelError> {
        // Check every source after every mutation, including terminal states.
        let mut checks: Vec<Check> = SOURCES
            .into_iter()
            .map(|source| {
                if scoped(observe(state, source)) == expected(state, source) {
                    Check::passed("caller.current_source_facts")
                } else {
                    Check::failed("caller.current_source_facts", "unexpected authority")
                }
            })
            .collect();
        // Stated directly as well: the household viewer is never an
        // administrator and never outlives a restricted restart.
        let household = observe(state, Source::Anonymous);
        checks.push(
            if household.as_ref().is_ok_and(|p| p.is_admin())
                || (!state.household && household.is_ok())
            {
                Check::failed(
                    "caller.household_viewer_bounded",
                    "household authority escaped",
                )
            } else {
                Check::passed("caller.household_viewer_bounded")
            },
        );
        Ok(checks)
    }

    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let exact = match (input, next.outputs) {
            (Input::Reauthenticate(source), [Effect::Authenticated(result)]) => {
                next.state == before && scoped(result.clone()) == expected(before, *source)
            }
            (Input::Reauthenticate(_), _) => false,
            (_, [Effect::Changed]) => true,
            _ => false,
        };
        // An event stream opened by the household viewer before this input
        // resets exactly when its scope changed (production `recheck`).
        let stream = match (
            observe(before, Source::Anonymous),
            observe(next.state, Source::Anonymous),
        ) {
            (Ok(subscribed), current @ Ok(_)) => {
                let reset = matches!(
                    recheck(subscribed.policy_revision, &current),
                    Recheck::Reset { .. }
                );
                reset == scope_change(input)
            }
            // Restricted on either side: no household stream to keep.
            _ => true,
        };
        Ok(vec![
            if exact {
                Check::passed("caller.exact_current_authority_and_single_outcome")
            } else {
                Check::failed(
                    "caller.exact_current_authority_and_single_outcome",
                    "unexpected authority or effect",
                )
            },
            if stream {
                Check::passed("caller.household_stream_resets_on_scope_change")
            } else {
                Check::failed(
                    "caller.household_stream_resets_on_scope_change",
                    "household scope change not reflected in its revision",
                )
            },
        ])
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
            Input::Restart {
                household: !s.household,
            },
            if s.library {
                Input::RemoveLibrary
            } else {
                Input::AddLibrary
            },
            if s.profile {
                Input::RemoveProfile
            } else {
                Input::AddProfile
            },
        ];
        for time in [99, 100] {
            if time > s.now {
                inputs.push(Input::At(time));
            }
        }
        inputs.extend(SOURCES.into_iter().map(Input::Reauthenticate));
        Ok(inputs)
    }
}

#[test]
fn caller_sources_never_infer_operator_from_missing_facts() {
    let report = stateless::explore::enumerate(
        &Callers,
        stateless::explore::SearchConfig {
            max_states: 5_000,
            max_transitions: 200_000,
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
    assert_eq!(report.states, 1_536);
    println!(
        "Caller model: {} states, {} transitions; one device and parent/session chain, seven explicit sources (incl. anonymous and rejected-credential household fallback), deletion/revoke/rotation/downgrade, restricted/household restarts, library/profile add/remove interleavings, time 0/99/100",
        report.states, report.transitions
    );
}
