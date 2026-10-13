//! Bounded interleavings execute the production administrator policies.
use playscale_core::{
    access::{self as access, *},
    access_admin::{self as admin, *},
};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    user: User,
    invite: Invitation,
    device: Option<Device>,
    credential: Option<Credential>,
    admins: [Device; 2],
    issuer: bool,
    issued: u8,
    now: i64,
    pairing: Pairing,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Claim(bool),
    RevokeInvite,
    EditUser(bool),
    LoseIssuer,
    RevokeDevice,
    RevokeAdmin(usize, bool),
    EditAdmin(usize, bool),
    Expire,
    Deny,
    Approve,
}
struct Access;
fn grant() -> Grant {
    Grant {
        profile_ids: ["p".into()].into(),
        permissions: [Permission::CatalogRead].into(),
    }
}
impl Model for Access {
    type State = State;
    type Input = Input;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.admin-access".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/access_admin.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        let user = User {
            id: "u".into(),
            name: "User".into(),
            revision: 1,
            disabled: false,
            grant: grant(),
            policy: Policy::default(),
        };
        let make = |id: &str| Device {
            generation: 1,
            credential_expires_at: Some(1000),
            ..Device::approved(
                id.into(),
                Grant {
                    profile_ids: Default::default(),
                    permissions: [Permission::SystemAdmin].into(),
                },
            )
        };
        Ok(State {
            invite: Invitation {
                expires_at: 60,
                user_id: user.id.clone(),
                user_revision: 1,
                grant: user.grant.clone(),
                policy: user.policy.clone(),
                phase: InvitationPhase::Unused,
            },
            user,
            device: None,
            credential: None,
            admins: [make("a"), make("b")],
            issuer: true,
            issued: 0,
            now: 0,
            pairing: Pairing {
                id: "p".into(),
                user_code: "CODE".into(),
                expires_at: 60,
                phase: PairingPhase::Pending,
            },
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, String>, ModelError> {
        let mut n = s.clone();
        let mut output = "unchanged";
        match input {
            Input::Claim(same) => {
                let hash = if *same { "claim" } else { "other" };
                match admin::redeem(
                    &s.invite,
                    &s.user,
                    s.issuer,
                    hash,
                    s.device.as_ref().zip(s.credential.as_ref()),
                    s.now,
                ) {
                    Ok(Redemption::Issue) => {
                        let device = Device::approved("new".into(), s.invite.grant.clone());
                        let issue = Issue {
                            device_id: "new".into(),
                            generation: 1,
                            expires_at: 1000,
                        };
                        n.device = Some(apply_issue(&device, &issue).unwrap());
                        n.credential = Some(Credential {
                            device_id: "new".into(),
                            generation: 1,
                            expires_at: 1000,
                            kind: CredentialKind::Device,
                        });
                        n.invite.phase = InvitationPhase::Claimed {
                            claim_hash: hash.into(),
                            device_id: "new".into(),
                            generation: 1,
                        };
                        n.issued += 1;
                        output = "issue";
                    }
                    Ok(Redemption::Replay) => output = "replay",
                    Err(_) => output = "rejected",
                }
            }
            Input::RevokeInvite => {
                if let Ok(Revised::Next(_)) = admin::revoke_invitation(&s.invite, 1, 1) {
                    n.invite.phase = InvitationPhase::Revoked;
                    output = "revoked"
                }
            }
            Input::EditUser(disable) => {
                if s.user.revision == 1 {
                    n.user.revision = 2;
                    n.user.disabled = *disable;
                    n.user.grant.permissions.clear();
                    if let Some(d) = &s.device {
                        n.device = Some(admin::constrain(d, &n.user).unwrap());
                        if *disable {
                            n.credential = None
                        }
                    };
                    output = "edited"
                }
            }
            Input::LoseIssuer => {
                n.issuer = false;
                output = "issuer_lost"
            }
            Input::RevokeDevice => {
                if let Some(d) = &s.device {
                    if let Some(next) = revoke(d, d.revision).unwrap() {
                        n.device = Some(next);
                        n.credential = None;
                        output = "device_revoked"
                    }
                }
            }
            Input::RevokeAdmin(index, stale) => {
                let old = &s.admins[*index];
                if let Ok(Some(next)) = revoke(old, old.revision - u64::from(*stale)) {
                    let before = s
                        .admins
                        .iter()
                        .filter(|d| admin::usable_admin(d, true, s.now))
                        .count();
                    let after = s
                        .admins
                        .iter()
                        .enumerate()
                        .filter(|(i, d)| {
                            admin::usable_admin(if i == index { &next } else { d }, true, s.now)
                        })
                        .count();
                    if admin::preserve_admin(before, after).is_ok() {
                        n.admins[*index] = next;
                        output = "admin_revoked"
                    } else {
                        output = "last_admin"
                    }
                } else {
                    output = "rejected"
                }
            }
            Input::EditAdmin(index, disabled) => {
                let old = &s.admins[*index];
                if old.revision == 1 {
                    let mut user = s.user.clone();
                    user.disabled = *disabled;
                    let next = admin::constrain(old, &user).unwrap();
                    let before = s
                        .admins
                        .iter()
                        .filter(|d| admin::usable_admin(d, true, s.now))
                        .count();
                    let after = s
                        .admins
                        .iter()
                        .enumerate()
                        .filter(|(i, d)| {
                            admin::usable_admin(if i == index { &next } else { d }, true, s.now)
                        })
                        .count();
                    if admin::preserve_admin(before, after).is_ok() {
                        n.admins[*index] = next;
                        output = "admin_edited";
                    } else {
                        output = "last_admin";
                    }
                }
            }
            Input::Expire => {
                n.now = 60;
                output = "expired"
            }
            Input::Deny => {
                if admin::deny(&s.pairing, &Principal::operator(), s.now).is_ok() {
                    n.pairing.phase = PairingPhase::Denied;
                    output = "denied"
                }
            }
            Input::Approve => {
                if access::approve(
                    &s.pairing,
                    s.now,
                    &Principal::operator(),
                    "CODE",
                    &grant(),
                    &["p".into()].into(),
                )
                .is_ok()
                {
                    n.pairing.phase = PairingPhase::Approved {
                        device_id: "paired".into(),
                    };
                    output = "approved"
                }
            }
        }
        Ok(Transition::accepted(n, vec![output.into()]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            check("one_credential_issuance", s.issued <= 1),
            check(
                "last_usable_admin",
                s.admins.iter().any(|d| admin::usable_admin(d, true, s.now)),
            ),
            check(
                "device_within_user_ceiling",
                s.device
                    .as_ref()
                    .is_none_or(|d| d.revoked || admin::within(&d.grant, &d.policy, &s.user)),
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
        // Independent oracle: state facts predict the required successful and
        // refused outcomes, so an implementation that always rejects fails.
        let expected = match input {
            Input::Claim(same) => {
                let live = s.now < 60 && s.issuer && s.user.revision == 1 && !s.user.disabled;
                if !live {
                    "rejected"
                } else {
                    match &s.invite.phase {
                        InvitationPhase::Unused => "issue",
                        InvitationPhase::Claimed {
                            claim_hash,
                            device_id,
                            generation,
                        } if claim_hash == if *same { "claim" } else { "other" } => {
                            match s.device.as_ref().zip(s.credential.as_ref()) {
                                Some((d, c))
                                    if !d.revoked
                                        && &d.id == device_id
                                        && d.generation == *generation
                                        && c.generation == *generation
                                        && c.device_id == *device_id
                                        && c.expires_at > s.now =>
                                {
                                    "replay"
                                }
                                _ => "rejected",
                            }
                        }
                        _ => "rejected",
                    }
                }
            }
            Input::Deny => {
                if s.now < 60 && s.pairing.phase == PairingPhase::Pending {
                    "denied"
                } else {
                    "unchanged"
                }
            }
            Input::Approve => {
                if s.now < 60 && s.pairing.phase == PairingPhase::Pending {
                    "approved"
                } else {
                    "unchanged"
                }
            }
            Input::RevokeInvite => {
                if s.invite.phase == InvitationPhase::Unused {
                    "revoked"
                } else {
                    "unchanged"
                }
            }
            Input::RevokeAdmin(i, stale) => {
                if *stale || s.admins[*i].revoked {
                    "rejected"
                } else if !s.admins[1 - *i].revoked
                    && s.admins[1 - *i]
                        .grant
                        .permissions
                        .contains(&Permission::SystemAdmin)
                {
                    "admin_revoked"
                } else if s.admins[*i]
                    .grant
                    .permissions
                    .contains(&Permission::SystemAdmin)
                {
                    "last_admin"
                } else {
                    "admin_revoked"
                }
            }
            Input::EditAdmin(i, _) => {
                if s.admins[*i].revision != 1 {
                    "unchanged"
                } else if !s.admins[1 - *i].revoked
                    && s.admins[1 - *i]
                        .grant
                        .permissions
                        .contains(&Permission::SystemAdmin)
                {
                    "admin_edited"
                } else {
                    "last_admin"
                }
            }
            Input::EditUser(_) => {
                if s.user.revision == 1 {
                    "edited"
                } else {
                    "unchanged"
                }
            }
            Input::LoseIssuer => "issuer_lost",
            Input::Expire => "expired",
            Input::RevokeDevice => {
                if s.device.as_ref().is_some_and(|d| !d.revoked) {
                    "device_revoked"
                } else {
                    "unchanged"
                }
            }
        };
        Ok(vec![
            check("required_action_outcome", effect == expected),
            check(
                "exact_issue_identity",
                effect != "issue"
                    || (n.issued == s.issued + 1
                        && n.device.as_ref().is_some_and(|d| {
                            d.id == "new" && d.generation == 1 && d.grant == s.invite.grant
                        })
                        && n.credential
                            .as_ref()
                            .is_some_and(|c| c.device_id == "new" && c.generation == 1)),
            ),
            check(
                "required_pairing_decision",
                match expected {
                    "denied" => n.pairing.phase == PairingPhase::Denied,
                    "approved" => {
                        n.pairing.phase
                            == PairingPhase::Approved {
                                device_id: "paired".into(),
                            }
                    }
                    _ => true,
                },
            ),
            check("exact_effect_count", t.outputs.len() == 1),
            check(
                "unavailable_invite_never_issues",
                !(s.now >= 60
                    || !s.issuer
                    || s.user.revision != 1
                    || s.invite.phase == InvitationPhase::Revoked)
                    || n.issued == s.issued,
            ),
            check(
                "replay_keeps_exact_identity",
                effect != "replay"
                    || (n.device == s.device
                        && n.credential == s.credential
                        && n.issued == s.issued),
            ),
            check(
                "claim_is_bound_to_recipient",
                !matches!(input,Input::Claim(same) if matches!(&s.invite.phase,InvitationPhase::Claimed{claim_hash,..} if claim_hash != if *same {"claim"} else {"other"}))
                    || effect == "rejected",
            ),
            check(
                "pairing_decision_is_final",
                s.pairing.phase == PairingPhase::Pending || n.pairing == s.pairing,
            ),
            check(
                "stale_admin_action_has_no_effect",
                !matches!(input, Input::RevokeAdmin(_, true)) || n.admins == s.admins,
            ),
        ])
    }
}
fn check(id: &'static str, passed: bool) -> Check {
    if passed {
        Check::passed(id)
    } else {
        Check::failed(id, "administrator access invariant")
    }
}
impl Enumerate for Access {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Claim(true),
            Input::Claim(false),
            Input::RevokeInvite,
            Input::EditUser(false),
            Input::EditUser(true),
            Input::LoseIssuer,
            Input::RevokeDevice,
            Input::RevokeAdmin(0, false),
            Input::RevokeAdmin(1, false),
            Input::RevokeAdmin(0, true),
            Input::RevokeAdmin(1, true),
            Input::EditAdmin(0, false),
            Input::EditAdmin(1, false),
            Input::EditAdmin(0, true),
            Input::EditAdmin(1, true),
            Input::Expire,
            Input::Deny,
            Input::Approve,
        ])
    }
}
#[test]
fn bounded_admin_interleavings() {
    let report = stateless::explore::enumerate(
        &Access,
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
        "Admin access: {} states, {} transitions; two live administrators, one pairing, one invitation/two claim identities, one target user revision change, issue/replay/revoke/expiry/issuer loss",
        report.states, report.transitions
    );
}
#[test]
fn grant_rating_intersection_and_attempt_boundaries() {
    let mut user = Access.initial_state().unwrap().user;
    user.policy.library_ids = ["lib".into()].into();
    user.policy.allowed_ratings = ["G".into()].into();
    let mut d = Device::approved("d".into(), grant());
    d.policy.library_ids = ["lib".into()].into();
    d.policy.allowed_ratings = ["R".into()].into();
    let n = admin::constrain(&d, &user).unwrap();
    assert!(n.policy.library_ids.is_empty());
    assert_eq!(admin::redemption_attempt(0, 30, 59), Err(1));
    assert_eq!(admin::redemption_attempt(0, 30, 60), Ok((60, 1)));
    assert!(!admin::usable_admin(&d, true, 0));
    let mut a = Access.initial_state().unwrap().admins[0].clone();
    assert!(!admin::usable_admin(&a, false, 0));
    assert!(!admin::usable_admin(&a, true, 1000));
    a.revoked = true;
    assert!(!admin::usable_admin(&a, true, 0));
}

impl stateless::ModelCodec for Access {
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

#[test]
fn every_grant_dimension_and_admin_credential_boundary_is_enforced() {
    let mut u = Access.initial_state().unwrap().user;
    u.policy.library_ids = ["lib".into()].into();
    u.policy.allowed_ratings = ["G".into()].into();
    u.policy.blocked_labels = ["private".into()].into();
    u.policy.allow_unrated = false;
    assert_eq!(admin::authorize_grant(&u, 1, &u.grant, &u.policy), Ok(()));
    assert_eq!(
        admin::authorize_grant(&u, 2, &u.grant, &u.policy),
        Err(AccessError::StaleRevision)
    );
    for dimension in 0..6 {
        let mut g = u.grant.clone();
        let mut p = u.policy.clone();
        match dimension {
            0 => {
                g.permissions.insert(Permission::SystemAdmin);
            }
            1 => {
                g.profile_ids.insert("other".into());
            }
            2 => {
                p.library_ids.insert("other".into());
            }
            3 => {
                p.allowed_ratings.clear();
            }
            4 => {
                p.blocked_labels.clear();
            }
            _ => {
                p.allow_unrated = true;
            }
        }
        assert_eq!(
            admin::authorize_grant(&u, 1, &g, &p),
            Err(AccessError::GrantExceeded),
            "dimension {dimension}"
        );
    }
    u.disabled = true;
    assert_eq!(
        admin::authorize_grant(&u, 1, &u.grant, &u.policy),
        Err(AccessError::UserDisabled)
    );
    for present in [false, true] {
        for revoked in [false, true] {
            for expiry in [None, Some(0), Some(1)] {
                let mut a = Access.initial_state().unwrap().admins[0].clone();
                a.revoked = revoked;
                a.credential_expires_at = expiry;
                assert_eq!(
                    admin::usable_admin(&a, present, 0),
                    present && !revoked && expiry == Some(1)
                );
            }
        }
    }
    assert_eq!(
        admin::preserve_admin(1, 0),
        Err(AccessError::LastAdministrator)
    );
    assert_eq!(admin::preserve_admin(2, 1), Ok(()));
    assert_eq!(admin::preserve_admin(0, 0), Ok(()));
}
