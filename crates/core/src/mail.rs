//! Durable transactional-mail delivery decisions shared by the SMTP relay and
//! native direct delivery. Adapters perform DNS, sockets, TLS, signing, time
//! observations and SQLite effects; they supply those observations here.
//!
//! External delivery is at-least-once at best. A message keeps one identity
//! across retries, an attempt is reserved durably before any network work, and
//! a lost reply after the final DATA terminator is recorded as ambiguous rather
//! than as either success or refusal. Cancellation cannot recall sent bytes.
use serde::{Deserialize, Serialize};

/// Attempts per message, including attempts recovered after a crash.
pub const MAX_ATTEMPTS: u32 = 6;
/// A reserved attempt is presumed abandoned after this interval. It must exceed
/// the adapter's complete per-attempt deadline, including every failover host.
pub const LEASE_SECONDS: i64 = 600;
/// Terminal delivery records (without payloads) remain visible this long.
pub const RETENTION_SECONDS: i64 = 7 * 86_400;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Relay,
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    SigninCode,
    RegistrationCode,
    Invitation,
    AccountNotice,
    Test,
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SigninCode => "signin_code",
            Self::RegistrationCode => "registration_code",
            Self::Invitation => "invitation",
            Self::AccountNotice => "account_notice",
            Self::Test => "test",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::SigninCode,
            Self::RegistrationCode,
            Self::Invitation,
            Self::AccountNotice,
            Self::Test,
        ]
        .into_iter()
        .find(|purpose| purpose.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Waiting for its first attempt or, when `attempts > 0`, a retry.
    Queued,
    /// An attempt is reserved under `lease`.
    Sending,
    /// The recipient server (direct) or relay accepted the final DATA.
    Accepted,
    /// Permanently refused, or the retry budget ended without ambiguity.
    Failed,
    /// The retry budget ended after an attempt whose acceptance is unknown.
    Unknown,
    /// The deadline passed before acceptance.
    Expired,
    /// Owner, configuration, restore epoch or enablement no longer admit it.
    Cancelled,
}

impl Phase {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Sending)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sending => "sending",
            Self::Accepted => "accepted",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Sending,
            Self::Accepted,
            Self::Failed,
            Self::Unknown,
            Self::Expired,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|phase| phase.as_str() == value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Lease {
    pub attempt: u32,
    pub token: String,
    pub until: i64,
}

/// Where an attempt stopped. Diagnostics only; the outcome kind decides policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Dns,
    Route,
    Connect,
    Greeting,
    Ehlo,
    /// Transient TLS failure (for example a 454 reply or handshake timeout).
    Tls,
    /// Definite incompatibility with the TLS policy: STARTTLS not offered,
    /// certificate invalid for the exchanger, or plaintext injection.
    TlsPolicy,
    Auth,
    MailFrom,
    RcptTo,
    Data,
    Message,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Route => "route",
            Self::Connect => "connect",
            Self::Greeting => "greeting",
            Self::Ehlo => "ehlo",
            Self::Tls => "tls",
            Self::TlsPolicy => "tls_policy",
            Self::Auth => "auth",
            Self::MailFrom => "mail_from",
            Self::RcptTo => "rcpt_to",
            Self::Data => "data",
            Self::Message => "message",
        }
    }
}

/// One adapter observation for one reserved attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Observation {
    /// A 2xx reply to the final DATA terminator.
    Accepted,
    /// Refused before the message could have been accepted; retry may succeed.
    Temporary(Stage),
    /// Refused before acceptance; retrying the same message cannot help.
    Permanent(Stage),
    /// The final DATA terminator may have been received but no reply arrived.
    Ambiguous,
    /// The pre-content recheck withdrew eligibility; nothing secret was sent.
    Withdrawn,
}

/// Every durable fact deciding whether a message may still be attempted.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub purpose: Purpose,
    pub owner: String,
    pub mode: Mode,
    pub config_revision: u64,
    /// Signing-key identity for direct mode; part of the configuration fence.
    pub key_id: Option<String>,
    pub epoch: String,
    pub expires_at: i64,
    pub phase: Phase,
    pub attempts: u32,
    pub lease: Option<Lease>,
    pub next_attempt: i64,
    /// Some earlier attempt may have been accepted; a retry can duplicate mail.
    pub ambiguous: bool,
}

/// Current observations supplied under the adapter's reserved writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Current<'a> {
    pub now: i64,
    pub enabled: bool,
    pub mode: Mode,
    pub config_revision: u64,
    pub key_id: Option<&'a str>,
    pub epoch: &'a str,
    pub restore_hold: bool,
    /// The owning challenge, invitation or test still wants this message.
    pub owner_live: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Persist the reserved attempt, commit, then perform network work.
    Attempt(Message),
    /// Persist a terminal state and erase the payload.
    Finish(Message),
    /// Nothing to do yet.
    Wait,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The observation is for an attempt that no longer holds the lease.
    StaleAttempt,
}

/// Retry delay after `attempt` (1-based) did not end in acceptance.
pub fn backoff(attempt: u32) -> i64 {
    match attempt {
        0 | 1 => 30,
        2 => 60,
        3 => 120,
        4 => 300,
        _ => 600,
    }
}

fn invalidated(m: &Message, c: &Current<'_>) -> Option<Phase> {
    if c.now >= m.expires_at {
        return Some(Phase::Expired);
    }
    if !c.enabled
        || c.restore_hold
        || !c.owner_live
        || c.mode != m.mode
        || c.config_revision != m.config_revision
        || c.key_id != m.key_id.as_deref()
        || c.epoch != m.epoch
    {
        return Some(Phase::Cancelled);
    }
    None
}

fn finish(m: &Message, phase: Phase) -> Message {
    Message {
        phase,
        lease: None,
        ..m.clone()
    }
}

fn exhausted(m: &Message) -> Phase {
    if m.ambiguous {
        Phase::Unknown
    } else {
        Phase::Failed
    }
}

/// Decide the next durable step. A Sending message whose lease expired was
/// interrupted at an unknown point, so it is treated as ambiguous: the next
/// attempt may produce duplicate mail, and that risk is recorded.
pub fn claim(m: &Message, c: &Current<'_>, token: &str) -> Decision {
    if m.phase.terminal() {
        return Decision::Wait;
    }
    let mut next = m.clone();
    if m.phase == Phase::Sending {
        match &m.lease {
            Some(lease) if c.now < lease.until => return Decision::Wait,
            _ => {
                next.phase = Phase::Queued;
                next.lease = None;
                next.ambiguous = true;
                next.next_attempt = next.next_attempt.min(c.now);
            }
        }
    }
    if let Some(phase) = invalidated(&next, c) {
        return Decision::Finish(finish(&next, phase));
    }
    if next.attempts >= MAX_ATTEMPTS {
        return Decision::Finish(finish(&next, exhausted(&next)));
    }
    if c.now < next.next_attempt {
        return if next == *m {
            Decision::Wait
        } else {
            // Persist the recovered ambiguity even while waiting.
            Decision::Finish(next)
        };
    }
    let attempt = next.attempts + 1;
    Decision::Attempt(Message {
        phase: Phase::Sending,
        attempts: attempt,
        lease: Some(Lease {
            attempt,
            token: token.to_owned(),
            until: c.now.saturating_add(LEASE_SECONDS),
        }),
        ..next
    })
}

/// Recheck immediately before transmitting secret content (the DATA command).
pub fn may_transmit(m: &Message, token: &str, c: &Current<'_>) -> bool {
    m.phase == Phase::Sending
        && m.lease
            .as_ref()
            .is_some_and(|lease| lease.token == token && c.now < lease.until)
        && invalidated(m, c).is_none()
}

/// Apply the adapter's observation for the attempt holding `token`.
/// Acceptance is recorded even if the message became invalid meanwhile: the
/// bytes were already sent and that fact must not be hidden.
pub fn observe(
    m: &Message,
    token: &str,
    observation: Observation,
    c: &Current<'_>,
) -> Result<Message, Error> {
    if m.phase != Phase::Sending || m.lease.as_ref().is_none_or(|lease| lease.token != token) {
        return Err(Error::StaleAttempt);
    }
    let mut next = Message {
        phase: Phase::Queued,
        lease: None,
        ..m.clone()
    };
    match observation {
        Observation::Accepted => return Ok(finish(&next, Phase::Accepted)),
        Observation::Permanent(_) => return Ok(finish(&next, Phase::Failed)),
        Observation::Ambiguous => next.ambiguous = true,
        Observation::Temporary(_) | Observation::Withdrawn => {}
    }
    if let Some(phase) = invalidated(&next, c) {
        return Ok(finish(&next, phase));
    }
    if next.attempts >= MAX_ATTEMPTS {
        return Ok(finish(&next, exhausted(&next)));
    }
    next.next_attempt = c.now.saturating_add(backoff(next.attempts));
    if next.next_attempt >= next.expires_at {
        return Ok(finish(&next, Phase::Expired));
    }
    Ok(next)
}

/// Explicit cancellation, for example after challenge completion or an
/// administrator configuration change. A queued message ends now. An attempt
/// already in flight keeps its lease: its observation (or lease expiry) then
/// meets the invalidated facts, so a late acceptance is still recorded.
pub fn cancel(m: &Message) -> Message {
    if m.phase == Phase::Queued {
        finish(m, Phase::Cancelled)
    } else {
        m.clone()
    }
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relay => "relay",
            Self::Direct => "direct",
        }
    }
}

/// Classify an SMTP reply code observed at `stage` before the final DATA
/// terminator. 4xx and malformed replies are temporary; 5xx are permanent.
pub fn classify_reply(stage: Stage, code: u16) -> Observation {
    if (500..600).contains(&code) {
        Observation::Permanent(stage)
    } else {
        Observation::Temporary(stage)
    }
}

/// Combine per-route outcomes of one attempt, in the order tried. The first
/// acceptance or ambiguity ends the attempt. A permanent refusal from a mail
/// exchanger is authoritative (RFC 5321 §4.2.1); transport/TLS failures at one
/// route allow the next. When every route definitely failed the TLS policy, the
/// destination cannot satisfy it and retrying cannot help; transient TLS
/// failures stay temporary.
pub fn combine_routes(outcomes: &[Observation]) -> Observation {
    let mut tls_only = !outcomes.is_empty();
    for outcome in outcomes {
        match outcome {
            Observation::Accepted
            | Observation::Ambiguous
            | Observation::Permanent(_)
            | Observation::Withdrawn => return *outcome,
            Observation::Temporary(Stage::TlsPolicy) => {}
            Observation::Temporary(_) => tls_only = false,
        }
    }
    match outcomes.last() {
        None => Observation::Temporary(Stage::Route),
        Some(_) if tls_only => Observation::Permanent(Stage::TlsPolicy),
        Some(last) => *last,
    }
}

/// True when a route failure allows trying the next route in the same attempt.
pub fn try_next_route(outcome: Observation) -> bool {
    matches!(outcome, Observation::Temporary(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Message {
        Message {
            id: "m".into(),
            purpose: Purpose::SigninCode,
            owner: "t".into(),
            mode: Mode::Direct,
            config_revision: 3,
            key_id: Some("k1".into()),
            epoch: "e".into(),
            expires_at: 1_000,
            phase: Phase::Queued,
            attempts: 0,
            lease: None,
            next_attempt: 0,
            ambiguous: false,
        }
    }
    fn current(now: i64) -> Current<'static> {
        Current {
            now,
            enabled: true,
            mode: Mode::Direct,
            config_revision: 3,
            key_id: Some("k1"),
            epoch: "e",
            restore_hold: false,
            owner_live: true,
        }
    }

    #[test]
    fn route_combination_prefers_authoritative_and_ambiguous_outcomes() {
        use Observation::*;
        assert_eq!(combine_routes(&[]), Temporary(Stage::Route));
        assert_eq!(
            combine_routes(&[Temporary(Stage::Connect), Accepted]),
            Accepted
        );
        assert_eq!(
            combine_routes(&[Temporary(Stage::TlsPolicy), Temporary(Stage::TlsPolicy)]),
            Permanent(Stage::TlsPolicy)
        );
        assert_eq!(
            combine_routes(&[Temporary(Stage::Tls)]),
            Temporary(Stage::Tls)
        );
        assert_eq!(
            combine_routes(&[Temporary(Stage::TlsPolicy), Temporary(Stage::Connect)]),
            Temporary(Stage::Connect)
        );
        assert_eq!(
            combine_routes(&[Permanent(Stage::RcptTo), Accepted]),
            Permanent(Stage::RcptTo)
        );
        assert_eq!(combine_routes(&[Ambiguous, Accepted]), Ambiguous);
        assert!(try_next_route(Temporary(Stage::Greeting)));
        assert!(!try_next_route(Permanent(Stage::MailFrom)));
        assert!(!try_next_route(Ambiguous));
        assert_eq!(classify_reply(Stage::RcptTo, 450), Temporary(Stage::RcptTo));
        assert_eq!(classify_reply(Stage::RcptTo, 550), Permanent(Stage::RcptTo));
        assert_eq!(classify_reply(Stage::Data, 999), Temporary(Stage::Data));
    }

    #[test]
    fn expired_lease_recovers_as_ambiguous_and_waits_for_backoff() {
        let token = "a";
        let Decision::Attempt(sending) = claim(&message(), &current(0), token) else {
            panic!()
        };
        assert_eq!(claim(&sending, &current(10), "b"), Decision::Wait);
        let Decision::Attempt(retry) = claim(&sending, &current(LEASE_SECONDS), "b") else {
            panic!()
        };
        assert!(retry.ambiguous);
        assert_eq!(retry.attempts, 2);
        assert_eq!(
            observe(&sending, token, Observation::Accepted, &current(1)),
            Ok(Message {
                phase: Phase::Accepted,
                lease: None,
                ..sending.clone()
            })
        );
        assert_eq!(
            observe(&retry, token, Observation::Accepted, &current(1)),
            Err(Error::StaleAttempt)
        );
    }
}
