//! Email registration policy. Verification proves control of an address; this
//! policy decides what access follows. It never attaches a registration to an
//! existing account because an address matches, never grants administrator
//! authority implicitly, and rechecks the captured policy revision, invitation
//! revision and address ownership when the proof is consumed.
use serde::{Deserialize, Serialize};

use crate::access::{Grant, Permission, Policy as AccessPolicy};
use crate::access_admin::{self, Invitation, InvitationPhase, User};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// No public registration. Existing deployments keep this default.
    #[default]
    Closed,
    /// Verification creates a pending request; an administrator decides access.
    ApprovalRequired,
    /// Only an unexpired, unrevoked invitation bound to the address admits.
    InviteOnly,
    /// Verification activates a new user with the configured default grant.
    Open,
}

/// The facts captured with the challenge. Completion must observe the same
/// policy revision and, for invitations, the same invitation revision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Intent {
    pub policy: Policy,
    pub policy_revision: u64,
    pub invitation: Option<InvitationRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InvitationRef {
    pub id: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Send a purpose-bound verification code.
    Verify(Intent),
    /// The address already belongs to an identity: send a notice without a
    /// code, so the public response stays identical and no account is touched.
    Notice,
    /// Send nothing. The public response stays identical.
    Silent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// A new pending request, or the existing one for this address.
    PendingApproval { existing: bool },
    /// The invitation's user receives a new email identity bounded by the
    /// invitation grant. No other account is affected.
    InvitationAccepted,
    /// A new user is created with the configured default grant.
    Activated,
}

impl Outcome {
    pub fn result(self) -> &'static str {
        match self {
            Self::PendingApproval { .. } => "pending_approval",
            Self::InvitationAccepted => "invitation_accepted",
            Self::Activated => "account_active",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    Closed,
    /// Policy or its revision changed after the challenge was issued.
    PolicyChanged,
    /// The address gained an owner meanwhile (concurrent registration/link).
    AddressUnavailable,
    InvitationUnavailable,
    /// The open-registration default grant is invalid or exceeds the ceiling.
    GrantInvalid,
    /// A configured bound on new accounts or requests is reached.
    Capacity,
}

impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Closed => "registration_closed",
            Self::PolicyChanged => "registration_changed",
            Self::AddressUnavailable | Self::InvitationUnavailable => "registration_unavailable",
            Self::GrantInvalid => "registration_misconfigured",
            Self::Capacity => "registration_capacity",
        }
    }
}

/// An open-registration default may never include administrator authority.
pub fn default_grant_allowed(grant: &Grant) -> bool {
    !grant.permissions.contains(&Permission::SystemAdmin)
}

/// The default user ceiling for open registration. Users are created with it
/// directly; identities and devices then intersect it as usual.
pub fn open_user(id: String, name: String, grant: &Grant, policy: &AccessPolicy) -> User {
    User {
        id,
        name,
        revision: 1,
        disabled: false,
        grant: grant.clone(),
        policy: policy.clone(),
    }
}

/// An email-bound invitation may complete registration only if it is still
/// unused, unexpired, issued by a current administrator and within its user's
/// current ceiling at the recorded user revision.
pub fn invitation_registrable(
    invitation: &Invitation,
    user: &User,
    issuer_current: bool,
    now: i64,
) -> bool {
    invitation.phase == InvitationPhase::Unused
        && now < invitation.expires_at
        && issuer_current
        && invitation.user_id == user.id
        && access_admin::authorize_grant(
            user,
            invitation.user_revision,
            &invitation.grant,
            &invitation.policy,
        )
        .is_ok()
}

/// Decide what a registration request may start. `invitation` is an available
/// email-bound invitation for exactly this canonical address, if any.
pub fn admit(
    policy: Policy,
    policy_revision: u64,
    address_owned: bool,
    invitation: Option<InvitationRef>,
) -> Result<Admission, Error> {
    if policy == Policy::Closed {
        return Err(Error::Closed);
    }
    if address_owned {
        return Ok(Admission::Notice);
    }
    let intent = |invitation| {
        Admission::Verify(Intent {
            policy,
            policy_revision,
            invitation,
        })
    };
    Ok(match (policy, invitation) {
        // An explicit administrator invitation takes precedence in every
        // non-closed policy; its grant was chosen for this address.
        (_, Some(invitation)) => intent(Some(invitation)),
        (Policy::InviteOnly, None) => Admission::Silent,
        (Policy::ApprovalRequired | Policy::Open, None) => intent(None),
        (Policy::Closed, None) => unreachable!(),
    })
}

/// Facts observed under the reserved writer when consuming verified proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Current<'a> {
    pub policy: Policy,
    pub policy_revision: u64,
    /// Any identity, enabled or not, already using this canonical address.
    pub address_owned: bool,
    /// The captured invitation as currently stored, if it still exists.
    pub invitation: Option<CurrentInvitation<'a>>,
    pub pending_request: bool,
    pub default_grant_valid: bool,
    pub capacity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentInvitation<'a> {
    pub id: &'a str,
    pub revision: u64,
    pub registrable: bool,
}

pub fn decide(intent: &Intent, current: &Current<'_>) -> Result<Outcome, Error> {
    if current.policy != intent.policy || current.policy_revision != intent.policy_revision {
        return Err(Error::PolicyChanged);
    }
    if current.policy == Policy::Closed {
        return Err(Error::Closed);
    }
    if current.address_owned {
        return Err(Error::AddressUnavailable);
    }
    if let Some(expected) = &intent.invitation {
        return match &current.invitation {
            Some(now)
                if now.id == expected.id
                    && now.revision == expected.revision
                    && now.registrable =>
            {
                Ok(Outcome::InvitationAccepted)
            }
            _ => Err(Error::InvitationUnavailable),
        };
    }
    match current.policy {
        Policy::ApprovalRequired => {
            if !current.pending_request && !current.capacity {
                return Err(Error::Capacity);
            }
            Ok(Outcome::PendingApproval {
                existing: current.pending_request,
            })
        }
        Policy::Open => {
            if !current.default_grant_valid {
                return Err(Error::GrantInvalid);
            }
            if !current.capacity {
                return Err(Error::Capacity);
            }
            Ok(Outcome::Activated)
        }
        Policy::InviteOnly => Err(Error::InvitationUnavailable),
        Policy::Closed => Err(Error::Closed),
    }
}
