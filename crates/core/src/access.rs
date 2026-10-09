//! Principal, pairing, credential, idempotency and event-scope decisions.
//!
//! Adapters look up stored rows by token hash or ID, supply the current time,
//! and execute the returned decisions. Whether a credential authenticates,
//! whether a pairing may be approved or claimed, whether a retried request
//! replays, and whether an event stream must reset are decided here.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PAIRING_TTL_SECONDS: i64 = 600;
pub const PAIRING_POLL_SECONDS: i64 = 5;
pub const MAX_PENDING_PAIRINGS: usize = 16;
/// Device credentials expire unless renewed by an explicit new pairing.
pub const DEVICE_CREDENTIAL_TTL_SECONDS: i64 = 90 * 86_400;
pub const SESSION_TTL_SECONDS: i64 = 12 * 3_600;
pub const ACCESS_TTL_RANGE: std::ops::RangeInclusive<i64> = 60..=3_600;
pub const IDEMPOTENCY_RETENTION_SECONDS: i64 = 7 * 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "catalog:read")]
    CatalogRead,
    #[serde(rename = "catalog:write")]
    CatalogWrite,
    #[serde(rename = "sources:manage")]
    SourcesManage,
    #[serde(rename = "profiles:manage")]
    ProfilesManage,
    #[serde(rename = "viewing:write")]
    ViewingWrite,
    #[serde(rename = "playback:request")]
    PlaybackRequest,
    #[serde(rename = "processing:request")]
    ProcessingRequest,
    #[serde(rename = "downloads:manage")]
    DownloadsManage,
    #[serde(rename = "collections:write")]
    CollectionsWrite,
    #[serde(rename = "events:read")]
    EventsRead,
    #[serde(rename = "system:admin")]
    SystemAdmin,
}

impl Permission {
    pub const ALL: [Permission; 11] = [
        Permission::CatalogRead,
        Permission::CatalogWrite,
        Permission::SourcesManage,
        Permission::ProfilesManage,
        Permission::ViewingWrite,
        Permission::PlaybackRequest,
        Permission::ProcessingRequest,
        Permission::DownloadsManage,
        Permission::CollectionsWrite,
        Permission::EventsRead,
        Permission::SystemAdmin,
    ];
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Grant {
    pub profile_ids: BTreeSet<String>,
    pub permissions: BTreeSet<Permission>,
}

/// Resource restrictions beyond permissions. Rating and label restrictions are
/// active when `allow_unrated` is false or either list is non-empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Policy {
    pub library_ids: BTreeSet<String>,
    pub allow_unrated: bool,
    pub allowed_ratings: BTreeSet<String>,
    pub blocked_labels: BTreeSet<String>,
}

impl Default for Policy {
    /// A newly approved device sees no library until an administrator grants one.
    fn default() -> Self {
        Self {
            library_ids: BTreeSet::new(),
            allow_unrated: true,
            allowed_ratings: BTreeSet::new(),
            blocked_labels: BTreeSet::new(),
        }
    }
}

impl Policy {
    pub fn rating_restricted(&self) -> bool {
        !self.allow_unrated || !self.allowed_ratings.is_empty() || !self.blocked_labels.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Paired,
    TrustedPrivate,
    Integration,
}

/// Deployment setting. Household mode keeps the unauthenticated legacy v1
/// surface; profile restrictions are then not enforceable and are reported so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    TrustedHousehold,
    Restricted,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub device_id: Option<String>,
    pub mode: Mode,
    pub grant: Grant,
    pub policy: Policy,
    pub policy_revision: u64,
}

impl Principal {
    /// The holder of the local operator token file: an integration with every
    /// permission. Possession of the 0600 file, not loopback, establishes it.
    pub fn operator() -> Self {
        Self {
            id: "operator".into(),
            device_id: None,
            mode: Mode::Integration,
            grant: Grant {
                profile_ids: BTreeSet::new(),
                permissions: Permission::ALL.into_iter().collect(),
            },
            policy: Policy::default(),
            policy_revision: 0,
        }
    }
    pub fn is_admin(&self) -> bool {
        self.grant.permissions.contains(&Permission::SystemAdmin)
    }
    pub fn allows(&self, permission: Permission) -> bool {
        self.is_admin() || self.grant.permissions.contains(&permission)
    }
    pub fn may_use_profile(&self, profile_id: &str) -> bool {
        self.is_admin() || self.grant.profile_ids.contains(profile_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessError {
    Unauthenticated,
    CredentialRevoked,
    CredentialExpired,
    CredentialSuperseded,
    Forbidden(Permission),
    ParentCredentialRequired,
    PairingExpired,
    PairingPending,
    PairingDecided,
    UserCodeMismatch,
    UnknownProfile,
    InvalidTtl,
    StaleRevision,
    DeviceRevoked,
    IdempotencyMismatch,
    /// The acknowledged result was revoked since; a retry cannot restore it.
    ReplayUnavailable,
    InvalidCursor,
}

impl AccessError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "authentication_required",
            Self::CredentialRevoked => "credential_revoked",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialSuperseded => "credential_superseded",
            Self::Forbidden(_) => "permission_denied",
            Self::ParentCredentialRequired => "parent_credential_required",
            Self::PairingExpired => "pairing_expired",
            Self::PairingPending => "pairing_pending",
            Self::PairingDecided => "pairing_already_decided",
            Self::UserCodeMismatch => "user_code_mismatch",
            Self::UnknownProfile => "unknown_profile",
            Self::InvalidTtl => "invalid_ttl",
            Self::StaleRevision => "precondition_failed",
            Self::DeviceRevoked => "device_revoked",
            Self::IdempotencyMismatch => "idempotency_key_reused",
            Self::ReplayUnavailable => "idempotent_replay_unavailable",
            Self::InvalidCursor => "invalid_cursor",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PairingPhase {
    Pending,
    Approved {
        device_id: String,
    },
    /// Bound to the generation this pairing issued; a later issuance for the
    /// same device makes this pairing's acknowledgement unrecoverable.
    Claimed {
        device_id: String,
        generation: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pairing {
    pub id: String,
    pub user_code: String,
    pub expires_at: i64,
    pub phase: PairingPhase,
}

/// User codes are displayed grouped; comparison ignores case and separators.
pub fn normalize_user_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub fn admit_pairing(pending: usize) -> bool {
    pending < MAX_PENDING_PAIRINGS
}

/// Approval creates the device with an explicit grant. The device has no
/// usable credential until the pairing is claimed.
pub fn approve(
    pairing: &Pairing,
    now: i64,
    approver: &Principal,
    user_code: &str,
    grant: &Grant,
    known_profiles: &BTreeSet<String>,
) -> Result<(), AccessError> {
    if !approver.is_admin() {
        return Err(AccessError::Forbidden(Permission::SystemAdmin));
    }
    if now >= pairing.expires_at {
        return Err(AccessError::PairingExpired);
    }
    if pairing.phase != PairingPhase::Pending {
        return Err(AccessError::PairingDecided);
    }
    if normalize_user_code(user_code) != normalize_user_code(&pairing.user_code) {
        return Err(AccessError::UserCodeMismatch);
    }
    if !grant.profile_ids.is_subset(known_profiles) {
        return Err(AccessError::UnknownProfile);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub revision: u64,
    pub grant: Grant,
    pub policy: Policy,
    pub revoked: bool,
    /// Only credentials of this generation authenticate.
    pub generation: u64,
    pub credential_expires_at: Option<i64>,
}

impl Device {
    pub fn approved(id: String, grant: Grant) -> Self {
        Self {
            id,
            revision: 1,
            grant,
            policy: Policy::default(),
            revoked: false,
            generation: 0,
            credential_expires_at: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Issue {
    pub device_id: String,
    pub generation: u64,
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Claim {
    /// Issue generation `n + 1`, superseding every earlier device credential.
    Issue(Issue),
    /// A lost acknowledgement: return the credential already issued. The
    /// adapter re-derives the same secret; it never stores the plaintext.
    Replay(Issue),
}

/// `issued` is the current-generation device credential row, if it still exists.
pub fn claim(
    pairing: &Pairing,
    device: Option<&Device>,
    issued: Option<&Credential>,
    now: i64,
) -> Result<Claim, AccessError> {
    if now >= pairing.expires_at {
        return Err(AccessError::PairingExpired);
    }
    let (device_id, claimed) = match &pairing.phase {
        PairingPhase::Pending => return Err(AccessError::PairingPending),
        PairingPhase::Approved { device_id } => (device_id, None),
        PairingPhase::Claimed {
            device_id,
            generation,
        } => (device_id, Some(*generation)),
    };
    let device = device
        .filter(|d| &d.id == device_id && !d.revoked)
        .ok_or(AccessError::DeviceRevoked)?;
    let Some(generation) = claimed else {
        return Ok(Claim::Issue(Issue {
            device_id: device.id.clone(),
            generation: device.generation + 1,
            expires_at: now.saturating_add(DEVICE_CREDENTIAL_TTL_SECONDS),
        }));
    };
    match issued {
        Some(c)
            if c.kind == CredentialKind::Device
                && c.device_id == device.id
                && c.generation == generation
                && device.generation == generation
                && Some(c.expires_at) == device.credential_expires_at =>
        {
            Ok(Claim::Replay(Issue {
                device_id: device.id.clone(),
                generation: c.generation,
                expires_at: c.expires_at,
            }))
        }
        _ => Err(AccessError::ReplayUnavailable),
    }
}

/// Apply a fresh issue exactly once; a delayed or duplicated issue cannot
/// restore an older generation.
pub fn apply_issue(device: &Device, issue: &Issue) -> Result<Device, AccessError> {
    if issue.device_id != device.id || issue.generation != device.generation + 1 {
        return Err(AccessError::StaleRevision);
    }
    if device.revoked {
        return Err(AccessError::DeviceRevoked);
    }
    Ok(Device {
        generation: issue.generation,
        credential_expires_at: Some(issue.expires_at),
        ..device.clone()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Device,
    Access,
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Credential {
    pub device_id: String,
    pub kind: CredentialKind,
    pub generation: u64,
    pub expires_at: i64,
}

/// Every request re-derives the principal from the current device row, so a
/// revocation, rotation or policy change applies to the next request. A child
/// (access or session) credential additionally requires its parent device
/// credential row to still exist: removing the parent revokes its children.
pub fn authenticate(
    credential: &Credential,
    parent: Option<&Credential>,
    device: Option<&Device>,
    now: i64,
) -> Result<Principal, AccessError> {
    let device = device
        .filter(|d| d.id == credential.device_id && !d.revoked)
        .ok_or(AccessError::CredentialRevoked)?;
    if credential.generation != device.generation {
        return Err(AccessError::CredentialSuperseded);
    }
    let parent_live = match (credential.kind, parent) {
        (CredentialKind::Device, None) => true,
        (CredentialKind::Device, Some(_)) => false,
        (_, Some(p)) => {
            p.kind == CredentialKind::Device
                && p.device_id == device.id
                && p.generation == device.generation
                && credential.expires_at <= p.expires_at
        }
        (_, None) => false,
    };
    if !parent_live {
        return Err(AccessError::CredentialRevoked);
    }
    if now >= credential.expires_at || device.credential_expires_at.is_none_or(|e| now >= e) {
        return Err(AccessError::CredentialExpired);
    }
    Ok(Principal {
        id: device.id.clone(),
        device_id: Some(device.id.clone()),
        mode: Mode::Paired,
        grant: device.grant.clone(),
        policy: device.policy.clone(),
        policy_revision: device.revision,
    })
}

/// Derive a narrower credential. Only a device credential may derive; children
/// inherit its scope and never outlive it, so a child cannot extend itself.
pub fn derive(
    parent: &Credential,
    device: &Device,
    kind: CredentialKind,
    ttl_seconds: i64,
    now: i64,
) -> Result<Credential, AccessError> {
    if parent.kind != CredentialKind::Device {
        return Err(AccessError::ParentCredentialRequired);
    }
    authenticate(parent, None, Some(device), now)?;
    let range = match kind {
        CredentialKind::Device => return Err(AccessError::ParentCredentialRequired),
        CredentialKind::Access => ACCESS_TTL_RANGE,
        CredentialKind::Session => 60..=SESSION_TTL_SECONDS,
    };
    if !range.contains(&ttl_seconds) {
        return Err(AccessError::InvalidTtl);
    }
    Ok(Credential {
        device_id: device.id.clone(),
        kind,
        generation: device.generation,
        expires_at: now.saturating_add(ttl_seconds).min(parent.expires_at),
    })
}

/// `None` means the device is already revoked and the request is a no-op.
pub fn revoke(device: &Device, expected_revision: u64) -> Result<Option<Device>, AccessError> {
    if device.revision != expected_revision {
        return Err(AccessError::StaleRevision);
    }
    if device.revoked {
        return Ok(None);
    }
    Ok(Some(Device {
        revision: device.revision + 1,
        revoked: true,
        ..device.clone()
    }))
}

/// `None` means the policy is unchanged; no revision or event is produced.
pub fn replace_policy(
    device: &Device,
    expected_revision: u64,
    permissions: BTreeSet<Permission>,
    policy: Policy,
) -> Result<Option<Device>, AccessError> {
    if device.revision != expected_revision {
        return Err(AccessError::StaleRevision);
    }
    if device.revoked {
        return Err(AccessError::DeviceRevoked);
    }
    if device.grant.permissions == permissions && device.policy == policy {
        return Ok(None);
    }
    Ok(Some(Device {
        revision: device.revision + 1,
        grant: Grant {
            profile_ids: device.grant.profile_ids.clone(),
            permissions,
        },
        policy,
        ..device.clone()
    }))
}

/// Unauthenticated legacy routes remain only in household mode. Restricted
/// mode admits them for the operator alone, so v1 cannot bypass v2 policy.
pub fn legacy_allowed(mode: AccessMode, operator: bool) -> bool {
    operator || mode == AccessMode::TrustedHousehold
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    pub digest: String,
    pub expires_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Idempotent {
    Execute,
    Replay,
}

/// Records are scoped by principal, operation, target and key by the adapter,
/// which authenticates and authorizes the caller before consulting them.
pub fn idempotency(
    existing: Option<&IdempotencyRecord>,
    digest: &str,
    now: i64,
) -> Result<Idempotent, AccessError> {
    match existing {
        None => Ok(Idempotent::Execute),
        Some(r) if now >= r.expires_at => Ok(Idempotent::Execute),
        Some(r) if r.digest != digest => Err(AccessError::IdempotencyMismatch),
        Some(_) => Ok(Idempotent::Replay),
    }
}

/// Opaque to clients. Scoped to a restore epoch, the principal and its policy
/// revision, so a restored database, another principal or a policy change
/// cannot resume an old position.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventCursor {
    pub epoch: String,
    pub principal: String,
    pub policy_revision: u64,
    pub position: i64,
}

fn valid_id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl EventCursor {
    pub fn encode(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.epoch, self.principal, self.policy_revision, self.position
        )
    }
    pub fn parse(value: &str) -> Result<Self, AccessError> {
        let mut parts = value.split('.');
        let (Some(epoch), Some(principal), Some(policy), Some(position), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(AccessError::InvalidCursor);
        };
        let ids = valid_id(epoch) && valid_id(principal);
        match (ids, policy.parse(), position.parse()) {
            (true, Ok(policy_revision), Ok(position)) if position >= 0 => Ok(Self {
                epoch: epoch.into(),
                principal: principal.into(),
                policy_revision,
                position,
            }),
            _ => Err(AccessError::InvalidCursor),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetReason {
    InitialSnapshot,
    RestoreEpoch,
    PrincipalChanged,
    PolicyChanged,
    CursorExpired,
}

impl ResetReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialSnapshot => "initial_snapshot",
            Self::RestoreEpoch => "restore_epoch",
            Self::PrincipalChanged => "principal_changed",
            Self::PolicyChanged => "policy_changed",
            Self::CursorExpired => "cursor_expired",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Resume {
    Continue(i64),
    Reset { position: i64, reason: ResetReason },
}

pub fn resume(
    requested: Option<&EventCursor>,
    epoch: &str,
    principal: &Principal,
    oldest: i64,
    newest: i64,
) -> Resume {
    let reset = |reason| Resume::Reset {
        position: newest,
        reason,
    };
    match requested {
        None => reset(ResetReason::InitialSnapshot),
        Some(c) if c.epoch != epoch => reset(ResetReason::RestoreEpoch),
        Some(c) if c.principal != principal.id => reset(ResetReason::PrincipalChanged),
        Some(c) if c.policy_revision != principal.policy_revision => {
            reset(ResetReason::PolicyChanged)
        }
        Some(c) => match crate::events::reset_cursor(c.position, oldest, newest) {
            Some(_) => reset(ResetReason::CursorExpired),
            None => Resume::Continue(c.position),
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    CredentialInvalid,
    PermissionRevoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Recheck {
    Continue,
    Reset { policy_revision: u64 },
    Close(CloseReason),
}

/// Applied while streaming: revocation or loss of `events:read` ends the
/// stream; any other policy change resets it so the client rebuilds every
/// authorized view.
pub fn recheck(subscribed: u64, current: &Result<Principal, AccessError>) -> Recheck {
    match current {
        Err(_) => Recheck::Close(CloseReason::CredentialInvalid),
        Ok(p) if !p.allows(Permission::EventsRead) => {
            Recheck::Close(CloseReason::PermissionRevoked)
        }
        Ok(p) if p.policy_revision != subscribed => Recheck::Reset {
            policy_revision: p.policy_revision,
        },
        Ok(_) => Recheck::Continue,
    }
}

/// What an invalidation hint names, with the facts needed to authorize it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Resource {
    Library(String),
    /// Catalog-derived resources carry the libraries that contain them.
    Item {
        libraries: BTreeSet<String>,
    },
    Profile(String),
    Device(String),
    /// Administrative or unclassified: visible to administrators only.
    Administrative,
}

pub fn visible(principal: &Principal, resource: &Resource) -> bool {
    if principal.is_admin() {
        return true;
    }
    if !principal.allows(Permission::EventsRead) {
        return false;
    }
    let catalog = principal.allows(Permission::CatalogRead);
    match resource {
        Resource::Library(id) => catalog && principal.policy.library_ids.contains(id),
        // Rating/label evidence is not yet attached to hints; fail closed.
        Resource::Item { libraries } => {
            catalog
                && !principal.policy.rating_restricted()
                && libraries
                    .iter()
                    .any(|l| principal.policy.library_ids.contains(l))
        }
        Resource::Profile(id) => principal.may_use_profile(id),
        Resource::Device(id) => principal.device_id.as_deref() == Some(id),
        Resource::Administrative => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        let mut d = Device::approved(
            "d1".into(),
            Grant {
                profile_ids: ["p1".to_string()].into(),
                permissions: [Permission::CatalogRead, Permission::EventsRead].into(),
            },
        );
        d.generation = 1;
        d.credential_expires_at = Some(1_000);
        d
    }
    fn cred(kind: CredentialKind, expires_at: i64) -> Credential {
        Credential {
            device_id: "d1".into(),
            kind,
            generation: 1,
            expires_at,
        }
    }

    #[test]
    fn permissions_round_trip_contract_spelling() {
        for p in Permission::ALL {
            let s = serde_json::to_string(&p).unwrap();
            assert_eq!(serde_json::from_str::<Permission>(&s).unwrap(), p);
        }
        assert_eq!(
            serde_json::to_string(&Permission::SystemAdmin).unwrap(),
            "\"system:admin\""
        );
    }

    #[test]
    fn children_never_outlive_parent_and_cannot_derive() {
        let d = device();
        let parent = cred(CredentialKind::Device, 1_000);
        let child = derive(&parent, &d, CredentialKind::Access, 3_600, 900).unwrap();
        assert_eq!(child.expires_at, 1_000);
        assert_eq!(
            derive(&child, &d, CredentialKind::Access, 60, 900),
            Err(AccessError::ParentCredentialRequired)
        );
        for (kind, ttl) in [
            (CredentialKind::Access, 59),
            (CredentialKind::Access, 3_601),
            (CredentialKind::Session, -1),
            (CredentialKind::Session, SESSION_TTL_SECONDS + 1),
        ] {
            assert_eq!(
                derive(&parent, &d, kind, ttl, 900),
                Err(AccessError::InvalidTtl)
            );
        }
        assert_eq!(
            derive(&parent, &d, CredentialKind::Device, 60, 900),
            Err(AccessError::ParentCredentialRequired)
        );
        assert!(authenticate(&child, Some(&parent), Some(&d), 999).is_ok());
        assert_eq!(
            authenticate(&child, Some(&parent), Some(&d), 1_000),
            Err(AccessError::CredentialExpired)
        );
        // Removing the parent row revokes the child.
        assert_eq!(
            authenticate(&child, None, Some(&d), 900),
            Err(AccessError::CredentialRevoked)
        );
        assert_eq!(
            authenticate(&parent, Some(&parent), Some(&d), 900),
            Err(AccessError::CredentialRevoked)
        );
    }

    #[test]
    fn claims_issue_once_then_replay_and_stale_issues_are_rejected() {
        let mut d = Device::approved("d1".into(), Grant::default());
        let mut pairing = Pairing {
            id: "p".into(),
            user_code: "AB".into(),
            expires_at: 600,
            phase: PairingPhase::Approved {
                device_id: "d1".into(),
            },
        };
        let Ok(Claim::Issue(issue)) = claim(&pairing, Some(&d), None, 0) else {
            panic!("approved pairing issues");
        };
        let next = apply_issue(&d, &issue).unwrap();
        assert_eq!(apply_issue(&next, &issue), Err(AccessError::StaleRevision));
        d = next;
        pairing.phase = PairingPhase::Claimed {
            device_id: "d1".into(),
            generation: issue.generation,
        };
        let row = Credential {
            device_id: "d1".into(),
            kind: CredentialKind::Device,
            generation: issue.generation,
            expires_at: issue.expires_at,
        };
        assert_eq!(
            claim(&pairing, Some(&d), Some(&row), 1),
            Ok(Claim::Replay(issue))
        );
        assert_eq!(
            claim(&pairing, Some(&d), None, 1),
            Err(AccessError::ReplayUnavailable)
        );
        // A later issuance for the device makes this acknowledgement unrecoverable.
        let renewed = Device {
            generation: 2,
            ..d.clone()
        };
        let newer = Credential {
            generation: 2,
            ..row.clone()
        };
        assert_eq!(
            claim(&pairing, Some(&renewed), Some(&newer), 1),
            Err(AccessError::ReplayUnavailable)
        );
        assert_eq!(
            claim(&pairing, Some(&d), Some(&row), 600),
            Err(AccessError::PairingExpired)
        );
    }

    #[test]
    fn cursor_scope_resets() {
        let p = Principal {
            policy_revision: 2,
            ..Principal::operator()
        };
        let c = EventCursor {
            epoch: "e1".into(),
            principal: "operator".into(),
            policy_revision: 2,
            position: 5,
        };
        assert_eq!(EventCursor::parse(&c.encode()), Ok(c.clone()));
        assert_eq!(resume(Some(&c), "e1", &p, 1, 9), Resume::Continue(5));
        let other = Principal {
            id: "d1".into(),
            ..p.clone()
        };
        let changed = Principal {
            policy_revision: 3,
            ..p.clone()
        };
        for (epoch, principal, reason) in [
            ("e2", &p, ResetReason::RestoreEpoch),
            ("e1", &other, ResetReason::PrincipalChanged),
            ("e1", &changed, ResetReason::PolicyChanged),
        ] {
            assert_eq!(
                resume(Some(&c), epoch, principal, 1, 9),
                Resume::Reset {
                    position: 9,
                    reason
                }
            );
        }
        assert_eq!(
            resume(Some(&c), "e1", &p, 7, 9),
            Resume::Reset {
                position: 9,
                reason: ResetReason::CursorExpired
            }
        );
        for bad in [
            "",
            "e1.p.2",
            "e1.p.x.5",
            "e1.p.2.-1",
            "e.p.1.2.3",
            "e/1.p.2.3",
            "e1..2.3",
        ] {
            assert_eq!(EventCursor::parse(bad), Err(AccessError::InvalidCursor));
        }
    }

    #[test]
    fn streams_close_on_lost_permission() {
        let mut p = authenticate(
            &cred(CredentialKind::Device, 1_000),
            None,
            Some(&device()),
            0,
        )
        .unwrap();
        assert_eq!(recheck(1, &Ok(p.clone())), Recheck::Continue);
        p.grant.permissions.remove(&Permission::EventsRead);
        p.policy_revision = 2;
        assert_eq!(
            recheck(1, &Ok(p)),
            Recheck::Close(CloseReason::PermissionRevoked)
        );
        assert_eq!(
            recheck(1, &Err(AccessError::CredentialRevoked)),
            Recheck::Close(CloseReason::CredentialInvalid)
        );
    }

    #[test]
    fn visibility_is_scoped_and_fails_closed() {
        let mut p = authenticate(
            &cred(CredentialKind::Device, 1_000),
            None,
            Some(&device()),
            0,
        )
        .unwrap();
        let item = |l: &str| Resource::Item {
            libraries: [l.to_string()].into(),
        };
        assert!(!visible(&p, &Resource::Library("l1".into())));
        p.policy.library_ids.insert("l1".into());
        assert!(visible(&p, &Resource::Library("l1".into())));
        assert!(visible(&p, &item("l1")));
        assert!(!visible(&p, &item("l2")));
        assert!(!visible(
            &p,
            &Resource::Item {
                libraries: BTreeSet::new()
            }
        ));
        p.policy.allow_unrated = false;
        assert!(!visible(&p, &item("l1")));
        assert!(visible(&p, &Resource::Profile("p1".into())));
        assert!(!visible(&p, &Resource::Profile("p2".into())));
        assert!(visible(&p, &Resource::Device("d1".into())));
        assert!(!visible(&p, &Resource::Device("d2".into())));
        assert!(!visible(&p, &Resource::Administrative));
        assert!(visible(&Principal::operator(), &Resource::Administrative));
        p.grant.permissions.remove(&Permission::EventsRead);
        assert!(!visible(&p, &Resource::Profile("p1".into())));
    }

    #[test]
    fn idempotency_decisions() {
        let r = IdempotencyRecord {
            digest: "a".into(),
            expires_at: 10,
        };
        assert_eq!(idempotency(None, "a", 0), Ok(Idempotent::Execute));
        assert_eq!(idempotency(Some(&r), "a", 0), Ok(Idempotent::Replay));
        assert_eq!(
            idempotency(Some(&r), "b", 0),
            Err(AccessError::IdempotencyMismatch)
        );
        assert_eq!(idempotency(Some(&r), "b", 10), Ok(Idempotent::Execute));
    }

    #[test]
    fn legacy_routes_follow_access_mode() {
        assert!(legacy_allowed(AccessMode::TrustedHousehold, false));
        assert!(!legacy_allowed(AccessMode::Restricted, false));
        assert!(legacy_allowed(AccessMode::Restricted, true));
    }
}
