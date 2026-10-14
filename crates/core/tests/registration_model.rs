//! Bounded interleavings of the production registration decisions with two
//! concurrent registrations for one address, policy changes, invitation
//! revocation/expiry/consumption, concurrent linking by another account and an
//! administrator-bearing default grant. Effects applied here mirror the
//! adapter's single reserved-writer transaction; this model does not prove
//! SQLite atomicity, which the live API tests exercise.
use std::collections::BTreeSet;

use playscale_core::access::{Grant, Permission, Policy as AccessPolicy};
use playscale_core::access_admin::{Invitation, InvitationPhase, User};
use playscale_core::registration::{
    self as core, Admission, Current, CurrentInvitation, Error, Intent, InvitationRef, Outcome,
    Policy,
};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

const INVITATION_EXPIRES: i64 = 100;
const CAPACITY: u8 = 2;

fn grant(admin: bool) -> Grant {
    Grant {
        profile_ids: BTreeSet::from(["everyone".to_owned()]),
        permissions: if admin {
            BTreeSet::from([Permission::CatalogRead, Permission::SystemAdmin])
        } else {
            BTreeSet::from([Permission::CatalogRead])
        },
    }
}

fn invitee() -> User {
    User {
        id: "invitee".into(),
        name: "Invitee".into(),
        revision: 1,
        disabled: false,
        grant: grant(false),
        policy: AccessPolicy::default(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tx {
    Verify(Intent),
    Notice,
    Silent,
    Closed,
    Done(Result<&'static str, Error>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    policy: Policy,
    policy_revision: u64,
    admin_default: bool,
    invitation: Option<(Invitation, u64)>,
    issuer_current: bool,
    now: i64,
    owner: Option<String>,
    users_created: u8,
    pending: u8,
    txs: [Option<Tx>; 2],
    activations: u8,
}

impl State {
    fn registrable(&self) -> bool {
        self.invitation.as_ref().is_some_and(|(invitation, _)| {
            core::invitation_registrable(invitation, &invitee(), self.issuer_current, self.now)
        })
    }
    fn current(&self) -> Current<'_> {
        Current {
            policy: self.policy,
            policy_revision: self.policy_revision,
            address_owned: self.owner.is_some(),
            invitation: self
                .invitation
                .as_ref()
                .map(|(_, revision)| CurrentInvitation {
                    id: "invitation-1",
                    revision: *revision,
                    registrable: self.registrable(),
                }),
            pending_request: self.pending > 0,
            default_grant_valid: core::default_grant_allowed(&grant(self.admin_default)),
            capacity: self.users_created < CAPACITY,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Start(usize),
    Complete(usize),
    SetPolicy(Policy),
    Invite,
    RevokeInvitation,
    IssuerRevoked,
    OtherAccountLinks,
    AdminDefault,
    At(i64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Observed,
    Admitted(Tx),
    Completed(Outcome),
    Rejected(Error),
    AlreadyUsed,
    NoCode,
}

struct Registration {
    initial_policy: Policy,
}

fn check(name: &'static str, passed: bool) -> Check {
    if passed {
        Check::passed(name)
    } else {
        Check::failed(name, "registration invariant")
    }
}

impl Model for Registration {
    type State = State;
    type Input = Input;
    type Output = Effect;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: format!("motion.registration.{:?}", self.initial_policy),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/registration.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            policy: self.initial_policy,
            policy_revision: 1,
            admin_default: false,
            invitation: None,
            issuer_current: true,
            now: 0,
            owner: None,
            users_created: 0,
            pending: 0,
            txs: [None, None],
            activations: 0,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match input {
            Input::Start(i) => {
                let invitation =
                    s.registrable()
                        .then(|| s.invitation.as_ref().unwrap())
                        .map(|(_, revision)| InvitationRef {
                            id: "invitation-1".into(),
                            revision: *revision,
                        });
                let tx =
                    match core::admit(s.policy, s.policy_revision, s.owner.is_some(), invitation) {
                        Ok(Admission::Verify(intent)) => Tx::Verify(intent),
                        Ok(Admission::Notice) => Tx::Notice,
                        Ok(Admission::Silent) => Tx::Silent,
                        Err(_) => Tx::Closed,
                    };
                next.txs[*i] = Some(tx.clone());
                Effect::Admitted(tx)
            }
            Input::Complete(i) => match &s.txs[*i] {
                Some(Tx::Verify(intent)) => {
                    let decision = core::decide(intent, &s.current());
                    match decision {
                        Ok(outcome) => {
                            match outcome {
                                Outcome::Activated => {
                                    next.users_created += 1;
                                    next.activations += 1;
                                    next.owner = Some(format!("new-{}", next.users_created));
                                }
                                Outcome::InvitationAccepted => {
                                    let (invitation, revision) = next.invitation.as_mut().unwrap();
                                    invitation.phase = InvitationPhase::Registered {
                                        identity_id: "identity-1".into(),
                                    };
                                    *revision += 1;
                                    next.owner = Some("invitee".into());
                                }
                                Outcome::PendingApproval { existing } => {
                                    next.pending += u8::from(!existing);
                                }
                            }
                            next.txs[*i] = Some(Tx::Done(Ok(outcome.result())));
                            Effect::Completed(outcome)
                        }
                        Err(error) => {
                            // Verified proof is consumed even when policy refuses it.
                            next.txs[*i] = Some(Tx::Done(Err(error)));
                            Effect::Rejected(error)
                        }
                    }
                }
                Some(Tx::Done(_)) => Effect::AlreadyUsed,
                _ => Effect::NoCode,
            },
            Input::SetPolicy(policy) => {
                next.policy = *policy;
                next.policy_revision += 1;
                Effect::Observed
            }
            Input::Invite => {
                let mut user = invitee();
                user.revision = 1;
                next.invitation = Some((
                    Invitation {
                        expires_at: INVITATION_EXPIRES,
                        user_id: user.id,
                        user_revision: 1,
                        grant: grant(false),
                        policy: AccessPolicy::default(),
                        phase: InvitationPhase::Unused,
                    },
                    1,
                ));
                Effect::Observed
            }
            Input::RevokeInvitation => {
                let (invitation, revision) = next.invitation.as_mut().unwrap();
                if invitation.phase == InvitationPhase::Unused {
                    invitation.phase = InvitationPhase::Revoked;
                    *revision += 1;
                }
                Effect::Observed
            }
            Input::IssuerRevoked => {
                next.issuer_current = false;
                Effect::Observed
            }
            Input::OtherAccountLinks => {
                next.owner = Some("other".into());
                Effect::Observed
            }
            Input::AdminDefault => {
                next.admin_default = true;
                Effect::Observed
            }
            Input::At(now) => {
                next.now = *now;
                Effect::Observed
            }
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let registered = s
            .invitation
            .as_ref()
            .is_some_and(|(i, _)| matches!(i.phase, InvitationPhase::Registered { .. }));
        Ok(vec![
            check(
                "registration.one_activation_per_address",
                s.activations <= 1,
            ),
            check("registration.one_pending_request", s.pending <= 1),
            check(
                "registration.invitation_owner",
                !registered || s.owner.as_deref() == Some("invitee"),
            ),
            check("registration.capacity", s.users_created <= CAPACITY),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        t: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = t.state;
        let effect = &t.outputs[0];
        let intent = match input {
            Input::Complete(i) => match &before.txs[*i] {
                Some(Tx::Verify(intent)) => Some(intent),
                _ => None,
            },
            _ => None,
        };
        let completed = matches!(effect, Effect::Completed(_));
        Ok(vec![
            check(
                "registration.owner_never_reassigned",
                before.owner.is_none() || after.owner == before.owner,
            ),
            check(
                "registration.completion_requires_unchanged_open_policy",
                !completed
                    || intent.is_some_and(|i| {
                        i.policy == before.policy
                            && i.policy_revision == before.policy_revision
                            && before.policy != Policy::Closed
                    }),
            ),
            check(
                "registration.never_claims_owned_address",
                !completed || before.owner.is_none(),
            ),
            check(
                "registration.activation_never_grants_admin",
                *effect != Effect::Completed(Outcome::Activated)
                    || (before.policy == Policy::Open && !before.admin_default),
            ),
            check(
                "registration.invitation_completion_is_exact",
                *effect != Effect::Completed(Outcome::InvitationAccepted)
                    || (before.registrable()
                        && intent.and_then(|i| i.invitation.as_ref()).is_some_and(|r| {
                            Some(r.revision) == before.invitation.as_ref().map(|(_, rev)| *rev)
                        })),
            ),
            check(
                "registration.invite_only_requires_invitation",
                !matches!(effect, Effect::Admitted(Tx::Verify(i)) if i.policy == Policy::InviteOnly && i.invitation.is_none()),
            ),
            check(
                "registration.owned_address_gets_notice_only",
                !matches!(input, Input::Start(_))
                    || before.owner.is_none()
                    || before.policy == Policy::Closed
                    || *effect == Effect::Admitted(Tx::Notice),
            ),
            check(
                "registration.consumed_proof_is_stable",
                !matches!(input, Input::Complete(i) if matches!(before.txs[*i], Some(Tx::Done(_))))
                    || (*effect == Effect::AlreadyUsed && after == before),
            ),
        ])
    }
}

impl Enumerate for Registration {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = Vec::new();
        for i in 0..2 {
            inputs.push(if s.txs[i].is_none() {
                Input::Start(i)
            } else {
                Input::Complete(i)
            });
        }
        if s.policy_revision < 3 {
            for policy in [
                Policy::Closed,
                Policy::ApprovalRequired,
                Policy::InviteOnly,
                Policy::Open,
            ] {
                if policy != s.policy {
                    inputs.push(Input::SetPolicy(policy));
                }
            }
        }
        match &s.invitation {
            None => inputs.push(Input::Invite),
            Some((invitation, _)) if invitation.phase == InvitationPhase::Unused => {
                inputs.push(Input::RevokeInvitation)
            }
            _ => {}
        }
        if s.issuer_current && s.invitation.is_some() {
            inputs.push(Input::IssuerRevoked);
        }
        if s.owner.is_none() {
            inputs.push(Input::OtherAccountLinks);
        }
        if !s.admin_default {
            inputs.push(Input::AdminDefault);
        }
        if s.now < INVITATION_EXPIRES {
            inputs.push(Input::At(INVITATION_EXPIRES));
        }
        Ok(inputs)
    }
}

#[test]
fn bounded_registration_interleavings_execute_production_decisions() {
    for initial_policy in [
        Policy::Closed,
        Policy::ApprovalRequired,
        Policy::InviteOnly,
        Policy::Open,
    ] {
        let report = stateless::explore::enumerate(
            &Registration { initial_policy },
            stateless::explore::SearchConfig {
                max_states: 400_000,
                max_transitions: 8_000_000,
                max_depth: 30,
            },
        )
        .unwrap();
        assert!(
            report.failure.is_none(),
            "{initial_policy:?}: {:?}",
            report.failure
        );
        assert_eq!(report.skipped_checks, 0);
        assert_eq!(
            report.termination,
            stateless::explore::SearchTermination::GraphExhausted,
            "{initial_policy:?}"
        );
        println!(
            "Registration from {initial_policy:?}: {} states, {} transitions; two concurrent registrations for one address, up to two policy changes, one invitation (revoke/expiry/issuer loss/consumption), concurrent linking, administrator default grant; no SQLite atomicity claim",
            report.states, report.transitions
        );
    }
}

#[test]
fn policy_and_invitation_revisions_are_exact_fences() {
    let intent = Intent {
        policy: Policy::InviteOnly,
        policy_revision: 4,
        invitation: Some(InvitationRef {
            id: "i".into(),
            revision: 2,
        }),
    };
    let current = |policy_revision, id, revision, registrable| Current {
        policy: Policy::InviteOnly,
        policy_revision,
        address_owned: false,
        invitation: Some(CurrentInvitation {
            id,
            revision,
            registrable,
        }),
        pending_request: false,
        default_grant_valid: true,
        capacity: true,
    };
    assert_eq!(
        core::decide(&intent, &current(4, "i", 2, true)),
        Ok(Outcome::InvitationAccepted)
    );
    assert_eq!(
        core::decide(&intent, &current(5, "i", 2, true)),
        Err(Error::PolicyChanged)
    );
    for (id, revision, registrable) in [("j", 2, true), ("i", 3, true), ("i", 2, false)] {
        assert_eq!(
            core::decide(&intent, &current(4, id, revision, registrable)),
            Err(Error::InvitationUnavailable)
        );
    }
    assert!(!core::default_grant_allowed(&grant(true)));
    assert!(core::default_grant_allowed(&grant(false)));
    assert_eq!(
        core::admit(Policy::Closed, 1, false, None),
        Err(Error::Closed)
    );
    assert_eq!(
        core::admit(Policy::Open, 1, true, None),
        Ok(Admission::Notice)
    );
    assert_eq!(
        core::admit(Policy::InviteOnly, 1, false, None),
        Ok(Admission::Silent)
    );
}
