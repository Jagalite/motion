//! User ownership and administrator decisions. Adapters supply durable snapshots
//! under a reserved writer transaction; these policies perform no I/O or time reads.
use crate::access::{
    self, AccessError, Device, Grant, Pairing, PairingPhase, Permission, Policy, Principal,
};
use serde::{Deserialize, Serialize};

pub const INVITATION_TTL_MAX: i64 = 86_400;
pub const REDEMPTION_WINDOW: i64 = 60;
pub const REDEMPTION_LIMIT: u32 = 30;
pub const MAX_BLOCKED_LABELS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub disabled: bool,
    pub grant: Grant,
    pub policy: Policy,
}

pub fn deny(pairing: &Pairing, approver: &Principal, now: i64) -> Result<(), AccessError> {
    if !approver.is_admin() {
        return Err(AccessError::Forbidden(Permission::SystemAdmin));
    }
    if now >= pairing.expires_at {
        return Err(AccessError::PairingExpired);
    }
    if pairing.phase != PairingPhase::Pending {
        return Err(AccessError::PairingDecided);
    }
    Ok(())
}

/// Administration is an explicit unrestricted ceiling. Ordinary grants must
/// satisfy every dimension, including the special empty-ratings = any rule.
pub fn within(grant: &Grant, policy: &Policy, user: &User) -> bool {
    if user.disabled {
        return false;
    }
    if user.grant.permissions.contains(&Permission::SystemAdmin) {
        return true;
    }
    grant.permissions.is_subset(&user.grant.permissions)
        && grant.profile_ids.is_subset(&user.grant.profile_ids)
        && policy.library_ids.is_subset(&user.policy.library_ids)
        && (!policy.allow_unrated || user.policy.allow_unrated)
        && policy
            .rating_eligibility()
            .within(&user.policy.rating_eligibility())
        && user.policy.blocked_labels.is_subset(&policy.blocked_labels)
}

pub fn authorize_grant(
    user: &User,
    expected: u64,
    grant: &Grant,
    policy: &Policy,
) -> Result<(), AccessError> {
    if user.revision != expected {
        return Err(AccessError::StaleRevision);
    }
    if user.disabled {
        return Err(AccessError::UserDisabled);
    }
    if !within(grant, policy, user) {
        return Err(AccessError::GrantExceeded);
    }
    Ok(())
}

/// Materialize a user's narrower ceiling into a device. Widening a user never
/// silently widens an existing device's approval. Every user edit fences old
/// policy-bound sessions/deliveries, even if the effective grant stays equal.
pub fn constrain(device: &Device, user: &User) -> Result<Device, AccessError> {
    let mut next = device.clone();
    next.revision = match access::revise(device.revision, device.revision, true)? {
        access::Revised::Next(n) => n,
        _ => unreachable!(),
    };
    if user.disabled {
        next.revoked = true;
    }
    if !user.grant.permissions.contains(&Permission::SystemAdmin) {
        if device.grant.permissions.contains(&Permission::SystemAdmin) {
            next.grant = user.grant.clone();
            next.policy = user.policy.clone();
        } else {
            next.grant.permissions = device
                .grant
                .permissions
                .intersection(&user.grant.permissions)
                .copied()
                .collect();
            next.grant.profile_ids = device
                .grant
                .profile_ids
                .intersection(&user.grant.profile_ids)
                .cloned()
                .collect();
            next.policy.library_ids = device
                .policy
                .library_ids
                .intersection(&user.policy.library_ids)
                .cloned()
                .collect();
            next.policy.allow_unrated &= user.policy.allow_unrated;
            next.policy
                .blocked_labels
                .extend(user.policy.blocked_labels.iter().cloned());
            next.policy.set_rating_eligibility(
                device
                    .policy
                    .rating_eligibility()
                    .intersection(&user.policy.rating_eligibility()),
            );
        }
    }
    // Repeated ceilings accumulate restrictions. Reject overflow atomically;
    // truncating this union would silently widen an existing device's access.
    if next.policy.blocked_labels.len() > MAX_BLOCKED_LABELS {
        return Err(AccessError::PolicyLimitExceeded);
    }
    Ok(next)
}

/// Linking is an authority-preserving enrollment, including re-enrollment of
/// a disabled identity. The observed initiating device and any existing method
/// are both ceilings; possession of a user's identity is never a wider grant.
pub fn link_ceiling(
    user: &User,
    initiating: &Device,
    existing: Option<&Device>,
) -> Result<Device, AccessError> {
    let bounded = constrain(initiating, user)?;
    if bounded.revoked {
        return Err(AccessError::UserDisabled);
    }
    match existing {
        None => Ok(bounded),
        Some(existing) => constrain(
            existing,
            &User {
                grant: bounded.grant,
                policy: bounded.policy,
                ..user.clone()
            },
        ),
    }
}

pub fn adds_administrator(previous: &Device, next: &Device) -> bool {
    !previous
        .grant
        .permissions
        .contains(&Permission::SystemAdmin)
        && next.grant.permissions.contains(&Permission::SystemAdmin)
}

/// Unassigned legacy devices retain their exact device authority. An assigned
/// device requires the named current owner; a missing owner never means legacy.
pub fn owner_allows(device: &Device, owner_id: Option<&str>, owner: Option<&User>) -> bool {
    match (owner_id, owner) {
        (None, None) => true,
        (Some(id), Some(user)) if id == user.id => within(&device.grant, &device.policy, user),
        _ => false,
    }
}

/// Assign an unassigned device explicitly. Its current approval is intersected
/// with the selected user's ceiling; assignment never widens or revives it.
/// Already assigned devices require a separate recovery/transfer operation.
pub fn assign_owner(
    device: &Device,
    current_owner: Option<&str>,
    expected_device_revision: u64,
    user: &User,
    expected_user_revision: u64,
) -> Result<Device, AccessError> {
    access::revise(device.revision, expected_device_revision, false)?;
    if current_owner.is_some() || user.revision != expected_user_revision {
        return Err(AccessError::StaleRevision);
    }
    if device.revoked {
        return Err(AccessError::DeviceRevoked);
    }
    if user.disabled {
        return Err(AccessError::UserDisabled);
    }
    constrain(device, user)
}

pub fn usable_admin(device: &Device, credential_present: bool, now: i64) -> bool {
    !device.revoked
        && credential_present
        && device
            .credential_expires_at
            .is_some_and(|expiry| now < expiry)
        && device.grant.permissions.contains(&Permission::SystemAdmin)
}

pub fn preserve_admin(before: usize, after: usize) -> Result<(), AccessError> {
    if before > 0 && after == 0 {
        Err(AccessError::LastAdministrator)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvitationPhase {
    Unused,
    Claimed {
        claim_hash: String,
        device_id: String,
        generation: u64,
    },
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Invitation {
    pub expires_at: i64,
    pub user_id: String,
    pub user_revision: u64,
    pub grant: Grant,
    pub policy: Policy,
    pub phase: InvitationPhase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redemption {
    Issue,
    Replay,
}

/// Issuer authorization and revision are reobserved inside the same transaction.
/// Revocation/expiry/user edits invalidate both first claims and lost-ack retries.
pub fn redeem(
    invite: &Invitation,
    user: &User,
    issuer_current: bool,
    claim_hash: &str,
    issued: Option<(&Device, &access::Credential)>,
    now: i64,
) -> Result<Redemption, AccessError> {
    if now >= invite.expires_at || !issuer_current || invite.user_id != user.id {
        return Err(AccessError::InvitationUnavailable);
    }
    authorize_grant(user, invite.user_revision, &invite.grant, &invite.policy)
        .map_err(|_| AccessError::InvitationUnavailable)?;
    match &invite.phase {
        InvitationPhase::Unused => Ok(Redemption::Issue),
        InvitationPhase::Claimed {
            claim_hash: expected,
            device_id,
            generation,
        } if expected == claim_hash => {
            let (device, credential) = issued.ok_or(AccessError::InvitationUnavailable)?;
            if &device.id != device_id
                || device.generation != *generation
                || credential.kind != access::CredentialKind::Device
                || access::authenticate(credential, None, Some(device), now).is_err()
            {
                return Err(AccessError::InvitationUnavailable);
            }
            Ok(Redemption::Replay)
        }
        _ => Err(AccessError::InvitationUnavailable),
    }
}

/// Durable global admission bounds unknown-code guesses as well as valid codes.
/// Adapters commit the returned counter even when redemption fails.
pub fn redemption_attempt(start: i64, count: u32, now: i64) -> Result<(i64, u32), i64> {
    if now >= start.saturating_add(REDEMPTION_WINDOW) {
        return Ok((now, 1));
    }
    if count >= REDEMPTION_LIMIT {
        return Err(start
            .saturating_add(REDEMPTION_WINDOW)
            .saturating_sub(now)
            .max(1));
    }
    Ok((start, count + 1))
}

/// Claim and revoke compete on the same durable phase. Once claimed, an
/// administrator must revoke the issued device, not relabel its invitation.
pub fn revoke_invitation(
    invitation: &Invitation,
    revision: u64,
    expected: u64,
) -> Result<access::Revised, AccessError> {
    if matches!(invitation.phase, InvitationPhase::Claimed { .. }) {
        return Err(AccessError::InvitationUnavailable);
    }
    access::revise(
        revision,
        expected,
        invitation.phase != InvitationPhase::Revoked,
    )
}
