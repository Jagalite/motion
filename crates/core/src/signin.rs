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
