//! Bounded first-run owner setup interleavings execute the production setup
//! and pairing-user policies. The adapter's writer transaction makes each
//! setup attempt atomic, so an interrupted attempt is modelled as no effect.
use playscale_core::{
    access::*,
    access_admin::{self as admin, *},
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

const MAX_USERS: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    users: Vec<User>,
    owner: Option<String>,
    /// Owner ids returned to setup requests, in order.
    returned: Vec<String>,
    /// An existing unassigned device; setup never assigns it.
    legacy_device_owner: Option<String>,
    /// The user each suggested pairing approval selected.
    suggested: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    /// Setup by the server-local operator or an administrator; request 0 and
    /// 1 are concurrent or retried requests serialized by the writer lock.
    Setup(u8),
    /// A non-administrator presents a setup request.
    SetupWithoutAdmin,
    /// The setup transaction is interrupted before commit and rolls back.
    Interrupted,
    CreateUser,
    Disable(usize),
    Enable(usize),
    /// Approve a pairing for the suggested user when exactly one is eligible.
    ApproveSuggested,
}
type SetupFn =
    fn(&Principal, bool, usize, String, Option<String>) -> Result<OwnerSetup, AccessError>;
type PickFn = for<'a> fn(&'a [User]) -> PairingUser<'a>;

/// The model executes these policies; production uses the real ones and
/// `mutant_policies_are_detected` checks that broken rules are caught.
struct Setup {
    setup: SetupFn,
    pick: PickFn,
}
const PRODUCTION: Setup = Setup {
    setup: admin::setup_owner,
    pick: admin::pairing_user,
};

fn principal(admin: bool) -> Principal {
    if admin {
        return Principal::operator();
    }
    Principal {
        id: "viewer".into(),
        device_id: Some("viewer".into()),
        mode: Mode::Paired,
        grant: Grant {
            profile_ids: Default::default(),
            permissions: [Permission::CatalogRead].into(),
        },
        policy: Policy::default(),
        policy_revision: 0,
    }
}

impl Model for Setup {
    type State = State;
    type Input = Input;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.owner-setup".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/access_admin.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            users: vec![],
            owner: None,
            returned: vec![],
            legacy_device_owner: None,
            suggested: vec![],
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, String>, ModelError> {
        let mut n = s.clone();
        let output: String = match input {
            Input::Setup(request) => {
                let id = format!("owner-{request}-{}", s.users.len());
                match (self.setup)(&principal(true), s.owner.is_some(), s.users.len(), id, None) {
                    Ok(OwnerSetup::Create(user)) => {
                        n.owner = Some(user.id.clone());
                        n.returned.push(user.id.clone());
                        n.users.push(user);
                        "created".into()
                    }
                    Ok(OwnerSetup::Completed) => {
                        n.returned.push(s.owner.clone().expect("recorded owner"));
                        "completed".into()
                    }
                    Err(AccessError::OwnerSetupUnavailable) => "unavailable".into(),
                    Err(e) => e.code().into(),
                }
            }
            Input::SetupWithoutAdmin => match (self.setup)(
                &principal(false),
                s.owner.is_some(),
                s.users.len(),
                "intruder".into(),
                None,
            ) {
                Err(AccessError::Forbidden(_)) => "forbidden".into(),
                other => format!("{other:?}"),
            },
            Input::Interrupted => "rolled_back".into(),
            Input::CreateUser => {
                n.users.push(User {
                    id: format!("user-{}", s.users.len()),
                    name: "Member".into(),
                    revision: 1,
                    disabled: false,
                    grant: Grant {
                        profile_ids: Default::default(),
                        permissions: [Permission::CatalogRead].into(),
                    },
                    policy: Policy::default(),
                });
                "user_created".into()
            }
            Input::Disable(i) => {
                n.users[*i].disabled = true;
                n.users[*i].revision += 1;
                "disabled".into()
            }
            Input::Enable(i) => {
                n.users[*i].disabled = false;
                n.users[*i].revision += 1;
                "enabled".into()
            }
            Input::ApproveSuggested => match (self.pick)(&s.users) {
                PairingUser::Only(user) => {
                    n.suggested.push(user.id.clone());
                    "approved".into()
                }
                PairingUser::Choose => "choose".into(),
                PairingUser::NoEligibleUser => "no_user".into(),
            },
        };
        Ok(Transition::accepted(n, vec![output]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let owners: Vec<_> = s
            .users
            .iter()
            .filter(|u| Some(&u.id) == s.owner.as_ref())
            .collect();
        Ok(vec![
            check(
                "recorded_owner_is_one_existing_user",
                s.owner.is_none() || owners.len() == 1,
            ),
            check(
                "every_setup_response_names_the_recorded_owner",
                s.returned.iter().all(|id| Some(id) == s.owner.as_ref()),
            ),
            check(
                "unassigned_device_is_never_inferred",
                s.legacy_device_owner.is_none(),
            ),
            check(
                "owner_is_the_first_user",
                s.owner.is_none() || s.users.first().map(|u| &u.id) == s.owner.as_ref(),
            ),
        ])
    }
    fn check_transition(
        &self,
        s: &State,
        input: &Input,
        t: &TransitionRef<'_, State, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let effect = t.outputs.first().map(String::as_str).unwrap_or_default();
        let n = t.state;
        let eligible: Vec<&User> = s.users.iter().filter(|u| !u.disabled).collect();
        // Independent oracle over the observed facts.
        let expected = match input {
            Input::Setup(_) if s.owner.is_some() => "completed",
            Input::Setup(_) if s.users.is_empty() => "created",
            Input::Setup(_) => "unavailable",
            Input::SetupWithoutAdmin => "forbidden",
            Input::Interrupted => "rolled_back",
            Input::CreateUser => "user_created",
            Input::Disable(_) => "disabled",
            Input::Enable(_) => "enabled",
            Input::ApproveSuggested => match eligible.len() {
                0 => "no_user",
                1 => "approved",
                _ => "choose",
            },
        };
        let created = n.users.get(s.users.len());
        Ok(vec![
            check("required_outcome", effect == expected),
            check("exact_effect_count", t.outputs.len() == 1),
            check(
                "recorded_owner_is_never_replaced",
                s.owner.is_none() || n.owner == s.owner,
            ),
            check(
                "setup_never_changes_existing_users",
                !matches!(input, Input::Setup(_) | Input::SetupWithoutAdmin)
                    || n.users[..s.users.len()] == s.users[..],
            ),
            check(
                "setup_creates_at_most_one_enabled_administrator",
                effect != "created"
                    || (n.users.len() == s.users.len() + 1
                        && created.is_some_and(|u| {
                            !u.disabled
                                && u.name == OWNER_DEFAULT_NAME
                                && u.grant.permissions.contains(&Permission::SystemAdmin)
                                && u.revision == 1
                        })),
            ),
            check(
                "refused_or_failed_setup_has_no_effect",
                !matches!(effect, "unavailable" | "forbidden" | "rolled_back")
                    || (n.users == s.users && n.owner == s.owner),
            ),
            check(
                "suggestion_is_the_single_eligible_user",
                effect != "approved"
                    || (eligible.len() == 1 && n.suggested.last() == Some(&eligible[0].id)),
            ),
            check(
                "multiple_or_no_users_never_select_automatically",
                effect == "approved" || n.suggested == s.suggested,
            ),
        ])
    }
}
fn check(id: &'static str, passed: bool) -> Check {
    if passed {
        Check::passed(id)
    } else {
        Check::failed(id, "owner setup invariant")
    }
}
impl Enumerate for Setup {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Setup(0),
            Input::Setup(1),
            Input::SetupWithoutAdmin,
            Input::Interrupted,
            Input::ApproveSuggested,
        ];
        if s.users.len() < MAX_USERS {
            inputs.push(Input::CreateUser);
        }
        for (i, u) in s.users.iter().enumerate() {
            // Two toggles per user bound revisions and keep the graph finite.
            if u.revision < 3 {
                inputs.push(if u.disabled {
                    Input::Enable(i)
                } else {
                    Input::Disable(i)
                });
            }
        }
        // Repeated identical responses carry no new facts; bound the log.
        if s.returned.len() >= 3 || s.suggested.len() >= 2 {
            inputs.retain(|i| !matches!(i, Input::Setup(_) | Input::ApproveSuggested));
        }
        Ok(inputs)
    }
}
#[test]
fn bounded_owner_setup_interleavings() {
    let report = stateless::explore::enumerate(
        &PRODUCTION,
        stateless::explore::SearchConfig {
            max_states: 200_000,
            max_transitions: 3_000_000,
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
        "Owner setup: {} states, {} transitions; up to {MAX_USERS} users with two enable/disable toggles each, two concurrent/retried setup requests, interrupted setup, non-administrator setup, suggested pairing approval",
        report.states, report.transitions
    );
}

#[test]
fn setup_boundaries_and_pairing_suggestions() {
    assert_eq!(admin::setup_state(false, 0), SetupState::Required);
    assert_eq!(admin::setup_state(false, 1), SetupState::ExistingUsers);
    assert_eq!(admin::setup_state(true, 0), SetupState::Complete);
    assert_eq!(admin::setup_state(true, 5), SetupState::Complete);
    let Ok(OwnerSetup::Create(owner)) =
        admin::setup_owner(&principal(true), false, 0, "o".into(), Some("Ada".into()))
    else {
        panic!("setup must create the first owner");
    };
    assert_eq!(owner.name, "Ada");
    assert_eq!(owner.grant.permissions.len(), Permission::ALL.len());
    assert!(admin::within(&owner.grant, &owner.policy, &owner));
    assert_eq!(
        admin::setup_owner(&principal(true), false, 1, "o".into(), None),
        Err(AccessError::OwnerSetupUnavailable)
    );
    assert_eq!(
        admin::setup_owner(&principal(false), false, 0, "o".into(), None),
        Err(AccessError::Forbidden(Permission::SystemAdmin))
    );
    let mut disabled = owner.clone();
    disabled.id = "d".into();
    disabled.disabled = true;
    assert_eq!(admin::pairing_user(&[]), PairingUser::NoEligibleUser);
    assert_eq!(
        admin::pairing_user(std::slice::from_ref(&disabled)),
        PairingUser::NoEligibleUser
    );
    assert_eq!(
        admin::pairing_user(&[disabled.clone(), owner.clone()]),
        PairingUser::Only(&owner)
    );
    let mut second = owner.clone();
    second.id = "s".into();
    assert_eq!(admin::pairing_user(&[owner, second]), PairingUser::Choose);
}

impl stateless::ModelCodec for Setup {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, v: &[u8]) -> Result<State, ModelError> {
        decode(v)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, v: &[u8]) -> Result<Input, ModelError> {
        decode(v)
    }
    fn encode_output(&self, v: &String) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}

fn explore(model: &Setup) -> stateless::explore::SearchReport<Input> {
    stateless::explore::enumerate(
        model,
        stateless::explore::SearchConfig {
            max_states: 200_000,
            max_transitions: 3_000_000,
            max_depth: 40,
        },
    )
    .unwrap()
}

/// Each mutant breaks one rule the model claims to check; all must fail.
#[test]
fn mutant_policies_are_detected() {
    let mutants: [(&str, Setup); 6] = [
        (
            "adopts existing users",
            Setup {
                setup: |p, recorded, _, id, name| admin::setup_owner(p, recorded, 0, id, name),
                ..PRODUCTION
            },
        ),
        (
            "replaces the recorded owner",
            Setup {
                setup: |p, _, _, id, name| admin::setup_owner(p, false, 0, id, name),
                ..PRODUCTION
            },
        ),
        (
            "owner without administration",
            Setup {
                setup: |p, recorded, users, id, name| {
                    admin::setup_owner(p, recorded, users, id, name).map(|d| match d {
                        OwnerSetup::Create(mut u) => {
                            u.grant.permissions.remove(&Permission::SystemAdmin);
                            OwnerSetup::Create(u)
                        }
                        other => other,
                    })
                },
                ..PRODUCTION
            },
        ),
        (
            "skips authority",
            Setup {
                setup: |_, recorded, users, id, name| {
                    admin::setup_owner(&Principal::operator(), recorded, users, id, name)
                },
                ..PRODUCTION
            },
        ),
        (
            "suggests among several users",
            Setup {
                pick: |users| match users.iter().find(|u| !u.disabled) {
                    Some(u) => PairingUser::Only(u),
                    None => PairingUser::NoEligibleUser,
                },
                ..PRODUCTION
            },
        ),
        (
            "suggests a disabled user",
            Setup {
                pick: |users| match users {
                    [only] => PairingUser::Only(only),
                    [] => PairingUser::NoEligibleUser,
                    _ => admin::pairing_user(users),
                },
                ..PRODUCTION
            },
        ),
    ];
    for (name, mutant) in &mutants {
        let report = explore(mutant);
        assert!(report.failure.is_some(), "mutant not detected: {name}");
    }
}
