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
    UnknownLibrary,
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
            Self::UnknownLibrary => "unknown_library",
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

/// At most one claim per poll interval per pairing. `Err` carries the
/// seconds to wait before the next claim.
pub fn claim_poll(last_claim_at: Option<i64>, now: i64) -> Result<(), i64> {
    match last_claim_at.map(|last| PAIRING_POLL_SECONDS - (now - last)) {
        Some(wait) if wait > 0 => Err(wait),
        _ => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Revised {
    Unchanged,
    Next(u64),
}

/// A conditional replacement of a simple revisioned resource (profiles): the
/// strong precondition must name the current revision; an identical value is
/// not a new revision.
pub fn revise(current: u64, expected: u64, changed: bool) -> Result<Revised, AccessError> {
    if current != expected {
        return Err(AccessError::StaleRevision);
    }
    if !changed {
        return Ok(Revised::Unchanged);
    }
    current
        .checked_add(1)
        .map(Revised::Next)
        .ok_or(AccessError::StaleRevision)
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

/// A request through the trusted private-ingress listener whose verified
/// login is mapped to a device. It carries that device's grant and policy but
/// can never act as an administrator, and fails closed if the device is
/// missing or revoked.
pub fn authenticate_ingress(device: Option<&Device>) -> Result<Principal, AccessError> {
    let device = device
        .filter(|d| !d.revoked)
        .ok_or(AccessError::CredentialRevoked)?;
    let mut grant = device.grant.clone();
    grant.permissions.remove(&Permission::SystemAdmin);
    Ok(Principal {
        id: device.id.clone(),
        device_id: Some(device.id.clone()),
        mode: Mode::TrustedPrivate,
        grant,
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
/// `known_libraries` are the existing libraries among those the policy names.
pub fn replace_policy(
    device: &Device,
    expected_revision: u64,
    permissions: BTreeSet<Permission>,
    policy: Policy,
    known_libraries: &BTreeSet<String>,
) -> Result<Option<Device>, AccessError> {
    if device.revision != expected_revision {
        return Err(AccessError::StaleRevision);
    }
    if device.revoked {
        return Err(AccessError::DeviceRevoked);
    }
    if !policy.library_ids.is_subset(known_libraries) {
        return Err(AccessError::UnknownLibrary);
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

/// Unauthenticated legacy routes remain only in household mode. v1 responses
/// are not library-scoped, so restricted mode admits them only for an
/// administrator (whose v2 scope is the whole catalog): v1 can never return
/// more than v2 would.
pub fn legacy_allowed(mode: AccessMode, admin: bool) -> bool {
    admin || mode == AccessMode::TrustedHousehold
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
    /// A hint whose authorization facts are gone (e.g. a deleted item).
    ScopeUnknown,
}

impl ResetReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialSnapshot => "initial_snapshot",
            Self::RestoreEpoch => "restore_epoch",
            Self::PrincipalChanged => "principal_changed",
            Self::PolicyChanged => "policy_changed",
            Self::CursorExpired => "cursor_expired",
            Self::ScopeUnknown => "scope_unknown",
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
    /// Catalog-derived resources carry the libraries that contain them now
    /// and, for file mutations, the library the mutated file was in.
    Item {
        libraries: BTreeSet<String>,
        prior: BTreeSet<String>,
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
        Resource::Item { libraries, .. } => {
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

/// The catalog a principal may read. Every catalog, file, artwork, media and
/// search adapter constrains its query with this before counting, filtering
/// or paginating, so restricted rows are never disclosed indirectly.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CatalogScope {
    All,
    Libraries(BTreeSet<String>),
    Nothing,
}

impl CatalogScope {
    /// Whether a resource contained in `libraries` is readable.
    pub fn admits<'a>(&self, mut libraries: impl Iterator<Item = &'a String>) -> bool {
        match self {
            Self::All => true,
            Self::Libraries(allowed) => libraries.any(|l| allowed.contains(l)),
            Self::Nothing => false,
        }
    }
    pub fn library(&self, library: &str) -> bool {
        match self {
            Self::All => true,
            Self::Libraries(allowed) => allowed.contains(library),
            Self::Nothing => false,
        }
    }
}

/// Ratings and labels are not yet recorded as catalog evidence, so a
/// rating- or label-restricted principal reads nothing until they are
/// (missing evidence is unknown, and unknown is denied).
pub fn catalog_scope(principal: &Principal) -> CatalogScope {
    if principal.is_admin() {
        CatalogScope::All
    } else if !principal.allows(Permission::CatalogRead) || principal.policy.rating_restricted() {
        CatalogScope::Nothing
    } else {
        CatalogScope::Libraries(principal.policy.library_ids.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Disclosure {
    Deliver,
    Withhold,
    /// The hint may concern a resource the principal could see, but the facts
    /// to decide are gone; the client must discard and rebuild its views.
    Reset,
}

/// How to treat one hint for a principal. A catalog hint the principal can
/// no longer see, but which left a library it can see (its file moved or was
/// removed), or which has no membership left at all (deleted item), cannot
/// be named; the principal gets a reset that names nothing, so its cached
/// views of that library are rebuilt.
pub fn disclose(principal: &Principal, resource: &Resource) -> Disclosure {
    if visible(principal, resource) {
        return Disclosure::Deliver;
    }
    let catalog = principal.allows(Permission::EventsRead)
        && principal.allows(Permission::CatalogRead)
        && !principal.policy.rating_restricted();
    match resource {
        Resource::Item { libraries, prior }
            if catalog
                && (prior
                    .iter()
                    .any(|l| principal.policy.library_ids.contains(l))
                    || (libraries.is_empty() && !principal.policy.library_ids.is_empty())) =>
        {
            Disclosure::Reset
        }
        _ => Disclosure::Withhold,
    }
}

/// Purpose of a content ticket; each purpose needs its own permission and a
/// metadata grant is never a byte grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketPurpose {
    Playback,
    Download,
    Cast,
}

/// Current facts about a file, observed by the adapter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileFacts {
    pub library_id: String,
    pub revision: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TicketError {
    /// The principal lacks the purpose's permission.
    Forbidden(Permission),
    /// Missing, invisible, or a resource family this server cannot serve.
    NotFound,
    /// The file revision differs from the one requested or pinned.
    SourceChanged,
    /// Casting needs a separately approved receiver policy.
    CastUnavailable,
    InvalidTtl,
    /// Expired, revoked, issued for another resource, or its principal is
    /// no longer valid.
    TicketInvalid,
}

fn purpose_permission(purpose: TicketPurpose) -> Result<Permission, TicketError> {
    match purpose {
        TicketPurpose::Playback => Ok(Permission::PlaybackRequest),
        TicketPurpose::Download => Ok(Permission::DownloadsManage),
        TicketPurpose::Cast => Err(TicketError::CastUnavailable),
    }
}

/// Grant a ticket for one exact file revision. `facts` is `None` when the
/// file does not exist (or the resource family has no service). The ticket
/// never outlives the credential that requested it.
pub fn grant_file_ticket(
    principal: &Principal,
    purpose: TicketPurpose,
    requested_revision: &str,
    facts: Option<&FileFacts>,
    ttl_seconds: i64,
    parent_expires_at: Option<i64>,
    now: i64,
) -> Result<i64, TicketError> {
    if !(1..=3_600).contains(&ttl_seconds) {
        return Err(TicketError::InvalidTtl);
    }
    let permission = purpose_permission(purpose)?;
    if !principal.allows(permission) {
        return Err(TicketError::Forbidden(permission));
    }
    let facts = facts
        .filter(|f| catalog_scope(principal).library(&f.library_id))
        .ok_or(TicketError::NotFound)?;
    if facts.revision != requested_revision {
        return Err(TicketError::SourceChanged);
    }
    let expires = now.saturating_add(ttl_seconds);
    Ok(parent_expires_at.map_or(expires, |p| expires.min(p)))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileTicket {
    pub file_id: String,
    pub revision: String,
    pub purpose: TicketPurpose,
    pub expires_at: i64,
    pub revoked: bool,
}

/// Admit a media request presenting a ticket. The issuing principal is
/// re-derived for every request (`principal` is its current authentication
/// result), so revocation, policy narrowing and permission loss apply
/// immediately; the ticket only ever narrows that principal's access.
pub fn admit_file_ticket(
    ticket: &FileTicket,
    file_id: &str,
    requested_revision: Option<&str>,
    principal: &Result<Principal, AccessError>,
    facts: Option<&FileFacts>,
    now: i64,
) -> Result<(), TicketError> {
    if ticket.revoked || now >= ticket.expires_at || ticket.file_id != file_id {
        return Err(TicketError::TicketInvalid);
    }
    let principal = principal.as_ref().map_err(|_| TicketError::TicketInvalid)?;
    let permission = purpose_permission(ticket.purpose)?;
    if !principal.allows(permission) {
        return Err(TicketError::Forbidden(permission));
    }
    let facts = facts
        .filter(|f| catalog_scope(principal).library(&f.library_id))
        .ok_or(TicketError::NotFound)?;
    if facts.revision != ticket.revision || requested_revision.is_some_and(|r| r != ticket.revision)
    {
        return Err(TicketError::SourceChanged);
    }
    Ok(())
}

/// Byte access without a ticket: a bearer or cookie principal needs a byte
/// permission (playback or download) and the file inside its catalog scope.
pub fn admit_file_bytes(
    principal: &Principal,
    facts: Option<&FileFacts>,
) -> Result<(), TicketError> {
    if !principal.allows(Permission::PlaybackRequest)
        && !principal.allows(Permission::DownloadsManage)
    {
        return Err(TicketError::Forbidden(Permission::PlaybackRequest));
    }
    facts
        .filter(|f| catalog_scope(principal).library(&f.library_id))
        .map(|_| ())
        .ok_or(TicketError::NotFound)
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
            prior: BTreeSet::new(),
        };
        assert!(!visible(&p, &Resource::Library("l1".into())));
        p.policy.library_ids.insert("l1".into());
        assert!(visible(&p, &Resource::Library("l1".into())));
        assert!(visible(&p, &item("l1")));
        assert!(!visible(&p, &item("l2")));
        assert!(!visible(
            &p,
            &Resource::Item {
                libraries: BTreeSet::new(),
                prior: BTreeSet::new(),
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

    /// Exhaustive over a small domain, against an independent statement of
    /// the rule: deliver iff visible now; otherwise reset iff the hint may
    /// have left the principal's view; never name a resource it cannot see.
    #[test]
    fn disclosure_matches_specification_exhaustively() {
        let libs = ["a", "b"];
        let subsets: Vec<BTreeSet<String>> = (0..4)
            .map(|mask| {
                libs.iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, l)| l.to_string())
                    .collect()
            })
            .collect();
        let mut cases = 0;
        for allowed in &subsets {
            for now in &subsets {
                for prior in &subsets {
                    for (catalog, events, unrated) in [
                        (true, true, true),
                        (false, true, true),
                        (true, false, true),
                        (true, true, false),
                    ] {
                        let mut p = authenticate(
                            &cred(CredentialKind::Device, 1_000),
                            None,
                            Some(&device()),
                            0,
                        )
                        .unwrap();
                        p.grant.permissions.clear();
                        if catalog {
                            p.grant.permissions.insert(Permission::CatalogRead);
                        }
                        if events {
                            p.grant.permissions.insert(Permission::EventsRead);
                        }
                        p.policy.library_ids = allowed.clone();
                        p.policy.allow_unrated = unrated;
                        let r = Resource::Item {
                            libraries: now.clone(),
                            prior: prior.clone(),
                        };
                        let can = catalog && events && unrated;
                        let sees_now = can && now.iter().any(|l| allowed.contains(l));
                        let left_view = can
                            && (prior.iter().any(|l| allowed.contains(l))
                                || (now.is_empty() && !allowed.is_empty()));
                        let expected = if sees_now {
                            Disclosure::Deliver
                        } else if left_view {
                            Disclosure::Reset
                        } else {
                            Disclosure::Withhold
                        };
                        assert_eq!(
                            disclose(&p, &r),
                            expected,
                            "{allowed:?} {now:?} {prior:?} {catalog} {events} {unrated}"
                        );
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 256);
        let orphan = Resource::Item {
            libraries: BTreeSet::new(),
            prior: BTreeSet::new(),
        };
        assert_eq!(
            disclose(&Principal::operator(), &orphan),
            Disclosure::Deliver
        );
    }

    #[test]
    fn claim_polls_and_conditional_revisions() {
        assert_eq!(claim_poll(None, 100), Ok(()));
        assert_eq!(claim_poll(Some(98), 100), Err(3));
        assert_eq!(claim_poll(Some(95), 100), Ok(()));
        assert_eq!(revise(2, 1, true), Err(AccessError::StaleRevision));
        assert_eq!(revise(2, 2, false), Ok(Revised::Unchanged));
        assert_eq!(revise(2, 2, true), Ok(Revised::Next(3)));
        assert_eq!(
            revise(u64::MAX, u64::MAX, true),
            Err(AccessError::StaleRevision)
        );
        let d = device();
        let lib =
            |ids: &[&str]| -> BTreeSet<String> { ids.iter().map(|s| s.to_string()).collect() };
        let policy = Policy {
            library_ids: lib(&["l1", "l2"]),
            ..Policy::default()
        };
        assert_eq!(
            replace_policy(
                &d,
                1,
                d.grant.permissions.clone(),
                policy.clone(),
                &lib(&["l1"])
            ),
            Err(AccessError::UnknownLibrary)
        );
        assert!(
            replace_policy(
                &d,
                1,
                d.grant.permissions.clone(),
                policy,
                &lib(&["l1", "l2"])
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn catalog_scope_fails_closed() {
        let mut p = authenticate(
            &cred(CredentialKind::Device, 1_000),
            None,
            Some(&device()),
            0,
        )
        .unwrap();
        let libs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(catalog_scope(&p), CatalogScope::Libraries(BTreeSet::new()));
        assert!(!catalog_scope(&p).admits(libs(&["l1"]).iter()));
        p.policy.library_ids.insert("l1".into());
        assert!(catalog_scope(&p).admits(libs(&["l2", "l1"]).iter()));
        assert!(!catalog_scope(&p).admits(libs(&[]).iter()));
        p.policy.blocked_labels.insert("horror".into());
        assert_eq!(catalog_scope(&p), CatalogScope::Nothing);
        p.policy.blocked_labels.clear();
        p.grant.permissions.remove(&Permission::CatalogRead);
        assert_eq!(catalog_scope(&p), CatalogScope::Nothing);
        assert_eq!(catalog_scope(&Principal::operator()), CatalogScope::All);
    }

    #[test]
    fn file_tickets_pin_revision_purpose_and_principal() {
        let mut p = authenticate(
            &cred(CredentialKind::Device, 1_000),
            None,
            Some(&device()),
            0,
        )
        .unwrap();
        p.grant.permissions.insert(Permission::PlaybackRequest);
        p.policy.library_ids.insert("l1".into());
        let facts = FileFacts {
            library_id: "l1".into(),
            revision: "r1".into(),
        };
        let hidden = FileFacts {
            library_id: "l2".into(),
            ..facts.clone()
        };
        let grant = |p: &Principal, purpose, rev: &str, f: Option<&FileFacts>, ttl| {
            grant_file_ticket(p, purpose, rev, f, ttl, Some(1_000), 900)
        };
        assert_eq!(
            grant(&p, TicketPurpose::Playback, "r1", Some(&facts), 3_600),
            Ok(1_000)
        );
        assert_eq!(
            grant(&p, TicketPurpose::Playback, "r1", Some(&facts), 60),
            Ok(960)
        );
        assert_eq!(
            grant(&p, TicketPurpose::Download, "r1", Some(&facts), 60),
            Err(TicketError::Forbidden(Permission::DownloadsManage))
        );
        assert_eq!(
            grant(&p, TicketPurpose::Cast, "r1", Some(&facts), 60),
            Err(TicketError::CastUnavailable)
        );
        assert_eq!(
            grant(&p, TicketPurpose::Playback, "r1", Some(&hidden), 60),
            Err(TicketError::NotFound)
        );
        assert_eq!(
            grant(&p, TicketPurpose::Playback, "r1", None, 60),
            Err(TicketError::NotFound)
        );
        assert_eq!(
            grant(&p, TicketPurpose::Playback, "r0", Some(&facts), 60),
            Err(TicketError::SourceChanged)
        );
        for ttl in [0, 3_601] {
            assert_eq!(
                grant(&p, TicketPurpose::Playback, "r1", Some(&facts), ttl),
                Err(TicketError::InvalidTtl)
            );
        }

        let ticket = FileTicket {
            file_id: "f1".into(),
            revision: "r1".into(),
            purpose: TicketPurpose::Playback,
            expires_at: 1_000,
            revoked: false,
        };
        let ok = Ok(p.clone());
        assert_eq!(
            admit_file_ticket(&ticket, "f1", Some("r1"), &ok, Some(&facts), 999),
            Ok(())
        );
        assert_eq!(
            admit_file_ticket(&ticket, "f1", None, &ok, Some(&facts), 1_000),
            Err(TicketError::TicketInvalid)
        );
        assert_eq!(
            admit_file_ticket(&ticket, "f2", None, &ok, Some(&facts), 0),
            Err(TicketError::TicketInvalid)
        );
        assert_eq!(
            admit_file_ticket(
                &ticket,
                "f1",
                None,
                &Err(AccessError::CredentialRevoked),
                Some(&facts),
                0
            ),
            Err(TicketError::TicketInvalid)
        );
        let changed = FileFacts {
            revision: "r2".into(),
            ..facts.clone()
        };
        assert_eq!(
            admit_file_ticket(&ticket, "f1", None, &ok, Some(&changed), 0),
            Err(TicketError::SourceChanged)
        );
        assert_eq!(
            admit_file_ticket(&ticket, "f1", Some("r2"), &ok, Some(&facts), 0),
            Err(TicketError::SourceChanged)
        );
        // The issuing principal lost the library or the permission since.
        assert_eq!(
            admit_file_ticket(&ticket, "f1", None, &ok, Some(&hidden), 0),
            Err(TicketError::NotFound)
        );
        let mut narrowed = p.clone();
        narrowed
            .grant
            .permissions
            .remove(&Permission::PlaybackRequest);
        assert_eq!(
            admit_file_ticket(&ticket, "f1", None, &Ok(narrowed.clone()), Some(&facts), 0),
            Err(TicketError::Forbidden(Permission::PlaybackRequest))
        );
        let revoked = FileTicket {
            revoked: true,
            ..ticket
        };
        assert_eq!(
            admit_file_ticket(&revoked, "f1", None, &ok, Some(&facts), 0),
            Err(TicketError::TicketInvalid)
        );

        assert_eq!(admit_file_bytes(&p, Some(&facts)), Ok(()));
        assert_eq!(
            admit_file_bytes(&p, Some(&hidden)),
            Err(TicketError::NotFound)
        );
        assert_eq!(
            admit_file_bytes(&narrowed, Some(&facts)),
            Err(TicketError::Forbidden(Permission::PlaybackRequest))
        );
    }

    #[test]
    fn ingress_identity_is_never_administrative() {
        let mut d = device();
        d.grant.permissions.insert(Permission::SystemAdmin);
        let p = authenticate_ingress(Some(&d)).unwrap();
        assert!(!p.is_admin() && p.mode == Mode::TrustedPrivate);
        assert!(p.allows(Permission::CatalogRead));
        d.revoked = true;
        assert_eq!(
            authenticate_ingress(Some(&d)),
            Err(AccessError::CredentialRevoked)
        );
        assert_eq!(
            authenticate_ingress(None),
            Err(AccessError::CredentialRevoked)
        );
    }

    #[test]
    fn legacy_routes_follow_access_mode() {
        assert!(legacy_allowed(AccessMode::TrustedHousehold, false));
        assert!(!legacy_allowed(AccessMode::Restricted, false));
        assert!(legacy_allowed(AccessMode::Restricted, true));
    }
}
