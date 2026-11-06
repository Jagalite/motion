//! Deterministic sign-in, freshness and method-lineage decisions. Protocol
//! verification, secret material, time observations and durable transactions
//! belong to adapters; successful delivery is never authentication evidence.
use serde::{Deserialize, Serialize};

pub const TRANSACTION_TTL: i64 = 600;
pub const SESSION_MAX: i64 = 12 * 3600;
pub const FRESH_SECONDS: i64 = 300;
pub const CODE_ATTEMPTS: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Login,
    Link,
    Test,
    Reauth,
    Enroll,
    /// Anonymous email registration; the outcome is decided by `registration`.
    Register,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Pending,
    Exchanging,
    Consumed,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub purpose: Purpose,
    pub browser_hash: String,
    pub epoch: String,
    pub server_epoch: String,
    pub provider_revision: u64,
    pub user_id: Option<String>,
    pub user_revision: Option<u64>,
    pub identity_revision: Option<u64>,
    pub actor_revision: Option<u64>,
    pub expires_at: i64,
    pub phase: Phase,
    pub attempts: u32,
    /// Registration facts captured at issue; absent for every other purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration: Option<crate::registration::Intent>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    Expired,
    BrowserMismatch,
    AlreadyUsed,
    Stale,
    InvalidCode,
    AttemptsExhausted,
    RecentProofRequired,
    IdentityConflict,
    LastMethod,
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "signin_unavailable",
            Self::Expired => "signin_expired",
            Self::BrowserMismatch => "signin_browser_mismatch",
            Self::AlreadyUsed => "signin_already_used",
            Self::Stale => "signin_stale",
            Self::InvalidCode => "invalid_code",
            Self::AttemptsExhausted => "code_attempts_exhausted",
            Self::RecentProofRequired => "recent_auth_required",
            Self::IdentityConflict => "identity_already_linked",
            Self::LastMethod => "last_signin_method",
        }
    }
}
/// Durable admission-window decision. One observation of time is used for
/// expiry, reservation and Retry-After; the adapter commits successful budget
/// reservations independently from the protected request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateAdmission {
    Reserve { start: i64, attempts: i64 },
    Limited { retry_after: u64 },
}
pub fn reserve_rate_window(
    previous: Option<(i64, i64)>,
    now: i64,
    maximum: i64,
    window: i64,
) -> RateAdmission {
    if maximum <= 0 || window <= 0 {
        return RateAdmission::Limited { retry_after: 1 };
    }
    let (start, attempts) = previous
        .filter(|(start, _)| now.saturating_sub(*start) < window)
        .unwrap_or((now, 0));
    if attempts < 0 || attempts >= maximum {
        return RateAdmission::Limited {
            retry_after: start.saturating_add(window).saturating_sub(now).max(1) as u64,
        };
    }
    RateAdmission::Reserve {
        start,
        attempts: attempts + 1,
    }
}

/// A confirmed mail-test receipt remains replayable even when confirmation
/// happened on the final attempt or its challenge later expires. It remains
/// bound to its recorded configuration revision; replay grants no new receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestConfirmationDecision {
    Replay,
    Reject,
    Record { attempts: u32, confirmed: bool },
}
pub fn confirm_mail_test(
    confirmed: bool,
    has_code: bool,
    attempts: u32,
    expires_at: i64,
    now: i64,
    code_matches: bool,
) -> TestConfirmationDecision {
    if confirmed {
        return TestConfirmationDecision::Replay;
    }
    if !has_code || now >= expires_at || attempts >= CODE_ATTEMPTS {
        return TestConfirmationDecision::Reject;
    }
    TestConfirmationDecision::Record {
        attempts: attempts + 1,
        confirmed: code_matches,
    }
}

/// Method changes invalidate unfinished work, not historical successful or
/// failed receipts. Secrets are erased in every phase by the adapter.
pub fn invalidate_method_phase(phase: Phase) -> Phase {
    match phase {
        Phase::Pending | Phase::Exchanging => Phase::Failed,
        terminal => terminal,
    }
}

pub struct Current<'a> {
    pub browser_hash: &'a str,
    pub epoch: &'a str,
    pub server_epoch: &'a str,
    pub provider_revision: u64,
    pub method_enabled: bool,
    pub restore_hold: bool,
    pub user_id: Option<&'a str>,
    pub user_revision: Option<u64>,
    pub user_enabled: bool,
    pub identity_revision: Option<u64>,
    pub identity_enabled: bool,
    pub identity_owner: Option<&'a str>,
    pub actor_revision: Option<u64>,
    pub now: i64,
}
pub fn validate(tx: &Transaction, current: &Current<'_>) -> Result<(), Error> {
    // A registration carries its policy intent and never a pre-bound user;
    // any other purpose carrying registration facts is malformed state.
    if (tx.purpose == Purpose::Register)
        != (tx.registration.is_some() && tx.user_id.is_none() && tx.identity_revision.is_none())
    {
        return Err(Error::Stale);
    }
    if current.restore_hold
        || !current.method_enabled
        || !current.user_enabled
        || (!current.identity_enabled
            && !(tx.purpose == Purpose::Enroll
                && tx.user_id.is_some()
                && tx.user_id.as_deref() == current.identity_owner))
    {
        return Err(Error::Unavailable);
    }
    if current.now >= tx.expires_at {
        return Err(Error::Expired);
    }
    if tx.browser_hash != current.browser_hash {
        return Err(Error::BrowserMismatch);
    }
    if tx.epoch != current.epoch
        || tx.server_epoch != current.server_epoch
        || tx.provider_revision != current.provider_revision
        || tx.user_id.as_deref() != current.user_id
        || tx.user_revision != current.user_revision
        || tx.identity_revision != current.identity_revision
        || tx.actor_revision != current.actor_revision
        || (tx.identity_revision.is_some() && tx.user_id.as_deref() != current.identity_owner)
    {
        return Err(Error::Stale);
    }
    Ok(())
}
/// Commit Exchanging before any external code exchange. Exchanging is never
/// recoverable to Pending after a crash; a new transaction is required.
pub fn reserve(tx: &Transaction, current: &Current<'_>) -> Result<Transaction, Error> {
    validate(tx, current)?;
    if tx.phase != Phase::Pending {
        return Err(Error::AlreadyUsed);
    }
    Ok(Transaction {
        phase: Phase::Exchanging,
        ..tx.clone()
    })
}
pub fn complete(tx: &Transaction, current: &Current<'_>) -> Result<Transaction, Error> {
    validate(tx, current)?;
    if tx.phase != Phase::Exchanging {
        return Err(Error::AlreadyUsed);
    }
    Ok(Transaction {
        phase: Phase::Consumed,
        ..tx.clone()
    })
}
/// The adapter commits the returned attempt count even on a wrong code. The
/// equality fact is supplied only after keyed-hash comparison in the adapter.
pub fn redeem_code(
    tx: &Transaction,
    current: &Current<'_>,
    code_matches: bool,
) -> Result<(Transaction, bool), Error> {
    validate(tx, current)?;
    if tx.phase != Phase::Pending {
        return Err(Error::AlreadyUsed);
    }
    if tx.attempts >= CODE_ATTEMPTS {
        return Err(Error::AttemptsExhausted);
    }
    let attempts = tx.attempts + 1;
    Ok((
        Transaction {
            attempts,
            phase: if code_matches {
                Phase::Exchanging
            } else if attempts == CODE_ATTEMPTS {
                Phase::Failed
            } else {
                Phase::Pending
            },
            ..tx.clone()
        },
        code_matches,
    ))
}
pub fn link_owner(existing: Option<&str>, target: &str) -> Result<(), Error> {
    if existing.is_some_and(|owner| owner != target) {
        Err(Error::IdentityConflict)
    } else {
        Ok(())
    }
}
pub struct Family<'a> {
    pub revoked: bool,
    pub epoch: &'a str,
    pub current_epoch: &'a str,
    pub expires_at: i64,
    pub user_enabled: bool,
    pub identity_enabled: bool,
    pub identity_revision: u64,
    pub current_identity_revision: u64,
    pub method_enabled: bool,
    pub method_revision: u64,
    pub current_method_revision: u64,
}
pub fn family_valid(f: &Family<'_>, now: i64) -> bool {
    !f.revoked
        && f.user_enabled
        && f.identity_enabled
        && f.method_enabled
        && now < f.expires_at
        && f.epoch == f.current_epoch
        && f.identity_revision == f.current_identity_revision
        && f.method_revision == f.current_method_revision
}
pub struct Proof<'a> {
    pub actor: &'a str,
    pub action: &'a str,
    pub target: &'a str,
    pub epoch: &'a str,
    pub expires_at: i64,
    pub approved_by: Option<&'a str>,
    pub consumed: bool,
}
pub fn proof_valid(
    p: &Proof<'_>,
    actor: &str,
    action: &str,
    target: &str,
    epoch: &str,
    now: i64,
) -> bool {
    !p.consumed
        && p.actor == actor
        && p.action == action
        && p.target == target
        && p.epoch == epoch
        && now < p.expires_at
        && p.approved_by.is_some_and(|other| other != actor)
}
/// Local issuance cannot extend the maximum interval since verified proof.
/// A tolerated provider clock lead must not lengthen the local session.
pub fn session_expiry(authenticated_at: i64, now: i64) -> Option<i64> {
    authenticated_at
        .min(now)
        .checked_add(SESSION_MAX)
        .filter(|expiry| *expiry > now)
}
pub fn fresh(authenticated_at: i64, now: i64) -> bool {
    now.checked_sub(authenticated_at)
        .is_some_and(|elapsed| (0..=FRESH_SECONDS).contains(&elapsed))
}
/// Approval is independent of the initiating device and comes either from an
/// administrator or from another device already owned by the same local user.
pub fn can_approve_proof(
    actor: &str,
    approver: &str,
    approver_admin: bool,
    same_user: bool,
) -> bool {
    actor != approver && (approver_admin || same_user)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryCredential<'a> {
    pub owner: Option<&'a str>,
    pub revoked: bool,
    pub kind_is_device: bool,
    pub generation: u64,
    pub device_generation: u64,
    pub expires_at: i64,
    pub has_browser_family: bool,
}
pub fn independent_credential(c: &RecoveryCredential<'_>, owner: &str, now: i64) -> bool {
    c.owner == Some(owner)
        && !c.revoked
        && c.kind_is_device
        && c.generation > 0
        && c.generation == c.device_generation
        && now < c.expires_at
        && !c.has_browser_family
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityMethod {
    Oidc,
    Email,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryIdentity<'a> {
    pub id: &'a str,
    pub owner: &'a str,
    pub provider: Option<&'a str>,
    pub enabled: bool,
    pub kind: IdentityMethod,
    pub provider_enabled: bool,
    pub email_enabled: bool,
}
pub fn independent_identity(
    i: &RecoveryIdentity<'_>,
    owner: &str,
    excluded_identity: Option<&str>,
    excluded_provider: Option<&str>,
    exclude_email: bool,
) -> bool {
    i.owner == owner
        && i.enabled
        && excluded_identity != Some(i.id)
        && !(excluded_provider.is_some() && excluded_provider == i.provider)
        && match i.kind {
            IdentityMethod::Oidc => i.provider.is_some() && i.provider_enabled,
            IdentityMethod::Email => !exclude_email && i.email_enabled,
            IdentityMethod::Unknown => false,
        }
}

/// Removing the last available method requires an explicit local recovery
/// override. A browser's administrator grant is not a recovery override.
pub fn preserve_method(remaining: usize, local_recovery: bool) -> Result<(), Error> {
    if remaining > 0 || local_recovery {
        Ok(())
    } else {
        Err(Error::LastMethod)
    }
}
pub fn safe_return(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.chars().any(char::is_control)
        && !path.contains(['\\', '\r', '\n', '#', '?'])
        && !path.contains('%')
        && path.len() <= 512
        && !path.starts_with("/api/")
}
/// Conservative shared address policy: ASCII dot-atom mailbox, case folded,
/// no provider-specific dot/plus equivalence. Applied before lookup and storage.
pub fn canonical_email(address: &str) -> Option<String> {
    let address = address.trim();
    if !address.is_ascii() || address.len() > 254 {
        return None;
    }
    let (local, domain) = address.split_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        || !domain.contains('.')
        || domain.split('.').any(|p| {
            p.is_empty()
                || p.len() > 63
                || p.starts_with('-')
                || p.ends_with('-')
                || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    Some(address.to_ascii_lowercase())
}

#[cfg(test)]
mod administrative_tests {
    use super::*;
    #[test]
    fn independent_recovery_uses_current_owned_credential_and_method_facts() {
        for revoked in [false, true] {
            for family in [false, true] {
                for generation in [0, 1, 2] {
                    for expires_at in [9, 10, 11] {
                        for owner in [None, Some("a"), Some("b")] {
                            let c = RecoveryCredential {
                                owner,
                                revoked,
                                kind_is_device: true,
                                generation,
                                device_generation: 1,
                                expires_at,
                                has_browser_family: family,
                            };
                            assert_eq!(
                                independent_credential(&c, "a", 10),
                                owner == Some("a")
                                    && !revoked
                                    && !family
                                    && generation == 1
                                    && expires_at > 10
                            );
                        }
                    }
                }
            }
        }
        for kind in [
            IdentityMethod::Oidc,
            IdentityMethod::Email,
            IdentityMethod::Unknown,
        ] {
            for enabled in [false, true] {
                for provider_enabled in [false, true] {
                    for email_enabled in [false, true] {
                        for exclude_email in [false, true] {
                            let i = RecoveryIdentity {
                                id: "i",
                                owner: "a",
                                provider: Some("p"),
                                enabled,
                                kind,
                                provider_enabled,
                                email_enabled,
                            };
                            let expected = enabled
                                && match kind {
                                    IdentityMethod::Oidc => provider_enabled,
                                    IdentityMethod::Email => email_enabled && !exclude_email,
                                    _ => false,
                                };
                            assert_eq!(
                                independent_identity(&i, "a", None, None, exclude_email),
                                expected
                            );
                            assert!(!independent_identity(&i, "b", None, None, exclude_email));
                            assert!(!independent_identity(
                                &i,
                                "a",
                                Some("i"),
                                None,
                                exclude_email
                            ));
                            assert!(!independent_identity(
                                &i,
                                "a",
                                None,
                                Some("p"),
                                exclude_email
                            ));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rate_window_boundaries_and_clock_rollback() {
        assert_eq!(
            reserve_rate_window(None, 10, 2, 10),
            RateAdmission::Reserve {
                start: 10,
                attempts: 1
            }
        );
        assert_eq!(
            reserve_rate_window(Some((10, 1)), 19, 2, 10),
            RateAdmission::Reserve {
                start: 10,
                attempts: 2
            }
        );
        assert_eq!(
            reserve_rate_window(Some((10, 2)), 19, 2, 10),
            RateAdmission::Limited { retry_after: 1 }
        );
        assert_eq!(
            reserve_rate_window(Some((10, 2)), 20, 2, 10),
            RateAdmission::Reserve {
                start: 20,
                attempts: 1
            }
        );
        assert_eq!(
            reserve_rate_window(Some((10, 2)), 9, 2, 10),
            RateAdmission::Limited { retry_after: 11 }
        );
        assert!(matches!(
            reserve_rate_window(Some((10, -1)), 11, 2, 10),
            RateAdmission::Limited { .. }
        ));
    }
    #[test]
    fn confirmed_receipt_is_terminal_and_configuration_neutral() {
        assert_eq!(
            confirm_mail_test(false, true, 4, 20, 10, true),
            TestConfirmationDecision::Record {
                attempts: 5,
                confirmed: true
            }
        );
        for now in [10, 20, 100] {
            assert_eq!(
                confirm_mail_test(true, false, 5, 20, now, false),
                TestConfirmationDecision::Replay
            );
        }
        assert_eq!(
            confirm_mail_test(false, true, 5, 20, 10, true),
            TestConfirmationDecision::Reject
        );
        assert_eq!(
            confirm_mail_test(false, false, 0, 20, 10, false),
            TestConfirmationDecision::Reject
        );
    }
    #[test]
    fn method_invalidation_preserves_terminal_receipts() {
        for phase in [
            Phase::Pending,
            Phase::Exchanging,
            Phase::Consumed,
            Phase::Failed,
        ] {
            assert_eq!(
                invalidate_method_phase(phase),
                if matches!(phase, Phase::Pending | Phase::Exchanging) {
                    Phase::Failed
                } else {
                    phase
                }
            );
        }
    }
}

/// An administrator reviews a request against current method and user facts.
/// The returned next revision is written atomically with the identity link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestAction {
    Approve,
    Deny,
    Expire,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    StaleRevision,
    Expired,
    MethodChanged,
    RecoveryRequired,
    RevisionExhausted,
}
pub struct RequestObservation {
    pub pending: bool,
    pub revision: u64,
    pub expires_at: i64,
    pub method_current: bool,
}
pub fn decide_request(
    request: &RequestObservation,
    expected: u64,
    action: RequestAction,
    recovery: bool,
    now: i64,
) -> Result<u64, RequestError> {
    if request.revision != expected {
        return Err(RequestError::StaleRevision);
    }
    if !request.pending
        || (action != RequestAction::Expire && now >= request.expires_at)
        || (action == RequestAction::Expire && now < request.expires_at)
    {
        return Err(RequestError::Expired);
    }
    if action == RequestAction::Approve {
        if !recovery {
            return Err(RequestError::RecoveryRequired);
        }
        if !request.method_current {
            return Err(RequestError::MethodChanged);
        }
    }
    request
        .revision
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or(RequestError::RevisionExhausted)
}

/// Credential hashes are opaque identities observed by the adapter, not secrets
/// interpreted by core. Currency and ownership must be reobserved at use time.
pub struct ProofAuthority<'a> {
    pub recorded_actor_credential: Option<&'a str>,
    pub current_actor_credential: Option<&'a str>,
    pub actor_current: bool,
    pub approver_current: bool,
    pub approver_admin: bool,
    pub actor_owner: Option<&'a str>,
    pub approver_owner: Option<&'a str>,
}
pub fn proof_authority_valid(authority: &ProofAuthority<'_>) -> bool {
    authority.actor_current
        && authority.approver_current
        && authority.recorded_actor_credential == authority.current_actor_credential
        && (authority.approver_admin
            || authority
                .actor_owner
                .is_some_and(|owner| Some(owner) == authority.approver_owner))
}
pub fn proof_pending(p: &Proof<'_>, epoch: &str, now: i64) -> bool {
    !p.consumed && p.approved_by.is_none() && p.epoch == epoch && now < p.expires_at
}
pub fn proof_status(p: &Proof<'_>, epoch: &str, now: i64) -> &'static str {
    if p.consumed {
        "consumed"
    } else if p.epoch != epoch {
        "invalidated"
    } else if now >= p.expires_at {
        "expired"
    } else if p.approved_by.is_some() {
        "approved"
    } else {
        "pending"
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodKind {
    Provider,
    Email,
}
pub struct MethodChange {
    pub kind: MethodKind,
    pub enabled: bool,
    pub requested_enabled: bool,
    pub configuration_changed: bool,
    pub configuration_tested: bool,
    pub browser_tested: bool,
    pub restore_hold: bool,
    pub apply_untested: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodEffects {
    pub update: bool,
    pub preserve_independent_method: bool,
    pub revoke_families: bool,
    pub invalidate_transactions: bool,
    pub erase_test_evidence: bool,
    pub cancel_all_queued_mail: bool,
}
pub fn change_method(change: &MethodChange) -> Result<MethodEffects, Error> {
    let override_test = change.kind == MethodKind::Email
        && change.enabled
        && change.configuration_changed
        && change.apply_untested
        && !change.restore_hold;
    if change.requested_enabled
        && !override_test
        && (change.configuration_changed
            || !change.configuration_tested
            || change.restore_hold
            || (change.kind == MethodKind::Provider && !change.browser_tested))
    {
        return Err(Error::Unavailable);
    }
    let update = change.configuration_changed || change.enabled != change.requested_enabled;
    Ok(MethodEffects {
        update,
        preserve_independent_method: update && change.enabled && !change.requested_enabled,
        revoke_families: update,
        invalidate_transactions: update,
        erase_test_evidence: update && change.configuration_changed,
        cancel_all_queued_mail: update && change.kind == MethodKind::Email,
    })
}
pub fn issuer_edit_allowed(changed: bool, linked_identities: bool, duplicate: bool) -> bool {
    !changed || (!linked_identities && !duplicate)
}
pub fn provider_capacity(count: i64) -> bool {
    (0..32).contains(&count)
}

#[cfg(test)]
mod authority_transition_tests {
    use super::*;
    #[test]
    fn configuration_effects_follow_exact_current_evidence() {
        for kind in [MethodKind::Provider, MethodKind::Email] {
            for bits in 0..256 {
                let bit = |n| bits & (1 << n) != 0;
                let c = MethodChange {
                    kind,
                    enabled: bit(0),
                    requested_enabled: bit(1),
                    configuration_changed: bit(2),
                    configuration_tested: bit(3),
                    browser_tested: bit(4),
                    restore_hold: bit(5),
                    apply_untested: bit(6),
                };
                let result = change_method(&c);
                let override_allowed = kind == MethodKind::Email
                    && c.enabled
                    && c.configuration_changed
                    && c.apply_untested
                    && !c.restore_hold;
                let allowed = !c.requested_enabled
                    || override_allowed
                    || (!c.configuration_changed
                        && c.configuration_tested
                        && !c.restore_hold
                        && (kind == MethodKind::Email || c.browser_tested));
                assert_eq!(result.is_ok(), allowed);
                if let Ok(e) = result {
                    let update = c.configuration_changed || c.enabled != c.requested_enabled;
                    assert_eq!(e.update, update);
                    assert_eq!(e.revoke_families, update);
                    assert_eq!(e.invalidate_transactions, update);
                    assert_eq!(e.erase_test_evidence, update && c.configuration_changed);
                    assert_eq!(
                        e.cancel_all_queued_mail,
                        update && kind == MethodKind::Email
                    );
                    assert_eq!(
                        e.preserve_independent_method,
                        update && c.enabled && !c.requested_enabled
                    );
                }
            }
        }
    }
    #[test]
    fn approval_authority_is_reobserved_at_consumption() {
        for bits in 0..64 {
            let bit = |n| bits & (1 << n) != 0;
            let a = ProofAuthority {
                recorded_actor_credential: Some("old"),
                current_actor_credential: Some(if bit(0) { "old" } else { "rotated" }),
                actor_current: bit(1),
                approver_current: bit(2),
                approver_admin: bit(3),
                actor_owner: bit(4).then_some("owner"),
                approver_owner: Some(if bit(5) { "owner" } else { "other" }),
            };
            assert_eq!(
                proof_authority_valid(&a),
                bit(0) && bit(1) && bit(2) && (bit(3) || (bit(4) && bit(5)))
            );
        }
    }
    #[test]
    fn terminal_pending_request_never_relinks() {
        for action in [
            RequestAction::Approve,
            RequestAction::Deny,
            RequestAction::Expire,
        ] {
            let r = RequestObservation {
                pending: false,
                revision: 2,
                expires_at: 10,
                method_current: true,
            };
            assert_eq!(
                decide_request(&r, 2, action, true, 20),
                Err(RequestError::Expired)
            );
        }
        let r = RequestObservation {
            pending: true,
            revision: i64::MAX as u64,
            expires_at: 10,
            method_current: true,
        };
        assert_eq!(
            decide_request(&r, r.revision, RequestAction::Approve, true, 0),
            Err(RequestError::RevisionExhausted)
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogoutEffect<'a> {
    RevokeFamily { family: &'a str, device: &'a str },
    DeleteCredential { hash: &'a str },
}
/// Explicit logout is not a privileged removal of someone else's last method;
/// it may always end the caller's own browser family.
pub fn logout_effect<'a>(
    family: Option<&'a str>,
    device: &'a str,
    hash: &'a str,
) -> LogoutEffect<'a> {
    match family {
        Some(family) => LogoutEffect::RevokeFamily { family, device },
        None => LogoutEffect::DeleteCredential { hash },
    }
}
pub fn revoke_family_device(
    device: &crate::access::Device,
) -> Result<crate::access::Device, crate::access::AccessError> {
    let mut next = device.clone();
    next.revoked = true;
    next.revision = device
        .revision
        .checked_add(1)
        .filter(|r| *r <= i64::MAX as u64)
        .ok_or(crate::access::AccessError::StaleRevision)?;
    Ok(next)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalidation {
    MethodChanged,
    RegistrationChanged,
}
pub fn invalidated_phase(phase: Phase, reason: Invalidation) -> Phase {
    match reason {
        Invalidation::MethodChanged => invalidate_method_phase(phase),
        // Registration completion holds the writer through exchanging and its
        // effects; a later configuration edit only cancels pending challenges.
        Invalidation::RegistrationChanged if phase == Phase::Pending => Phase::Failed,
        Invalidation::RegistrationChanged => phase,
    }
}
