//! Bounded interleavings of the production sign-in policies. The model applies
//! the exact core-returned transaction; it does not prove SQLite atomicity,
//! protocol verification, cookie handling, or mail delivery.
use std::hash::{Hash, Hasher};

use playscale_core::signin::{self as core, Current, Error, Phase, Purpose, Transaction};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

const EXPIRY: i64 = 600;
const RESTORE_EPOCH: u8 = 1;
const SERVER_EPOCH: u8 = 2;
const PROVIDER_REVISION: u8 = 4;
const USER_REVISION: u8 = 8;
const USER_IDENTITY: u8 = 16;
const IDENTITY_REVISION: u8 = 32;
const ACTOR_REVISION: u8 = 64;

fn transaction(purpose: Purpose) -> Transaction {
    Transaction {
        id: "transaction-1".into(),
        purpose,
        browser_hash: "browser-1".into(),
        epoch: "restore-1".into(),
        server_epoch: "server-1".into(),
        provider_revision: 1,
        user_id: Some("user-1".into()),
        user_revision: Some(1),
        identity_revision: Some(1),
        actor_revision: Some(1),
        expires_at: EXPIRY,
        phase: Phase::Pending,
        attempts: 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Availability {
    Enabled,
    RestoreHold,
    MethodDisabled,
    UserDisabled,
    IdentityDisabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    transaction: Transaction,
    changed: u8,
    availability: Availability,
    now: i64,
    reservations: u8,
    completions: u8,
}

impl Hash for State {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        serde_json::to_vec(&self.transaction).unwrap().hash(hasher);
        self.changed.hash(hasher);
        self.availability.hash(hasher);
        self.now.hash(hasher);
        self.reservations.hash(hasher);
        self.completions.hash(hasher);
    }
}

impl State {
    fn current(&self, matching_browser: bool) -> Current<'_> {
        Current {
            browser_hash: if matching_browser {
                "browser-1"
            } else {
                "browser-2"
            },
            epoch: if self.changed & RESTORE_EPOCH == 0 {
                "restore-1"
            } else {
                "restore-2"
            },
            server_epoch: if self.changed & SERVER_EPOCH == 0 {
                "server-1"
            } else {
                "server-2"
            },
            provider_revision: if self.changed & PROVIDER_REVISION == 0 {
                1
            } else {
                2
            },
            method_enabled: self.availability != Availability::MethodDisabled,
            restore_hold: self.availability == Availability::RestoreHold,
            user_id: Some(if self.changed & USER_IDENTITY == 0 {
                "user-1"
            } else {
                "user-2"
            }),
            user_revision: Some(if self.changed & USER_REVISION == 0 {
                1
            } else {
                2
            }),
            user_enabled: self.availability != Availability::UserDisabled,
            identity_revision: Some(if self.changed & IDENTITY_REVISION == 0 {
                1
            } else {
                2
            }),
            identity_owner: Some("user-1"),
            actor_revision: Some(if self.changed & ACTOR_REVISION == 0 {
                1
            } else {
                2
            }),
            identity_enabled: self.availability != Availability::IdentityDisabled,
            now: self.now,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Reserve { browser: bool },
    Redeem { browser: bool, matches: bool },
    Complete { browser: bool },
    Change(u8),
    Availability(Availability),
    At(i64),
    CrashAndRestart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Observed,
    Reserved {
        transaction_id: String,
    },
    CodeAttempt {
        attempt: u32,
        matched: bool,
    },
    Completed {
        transaction_id: String,
        purpose: Purpose,
        user_id: Option<String>,
    },
    Rejected(Error),
}

struct Signin {
    email: bool,
    purpose: Purpose,
}

/// Independent expected outcomes for the chosen facts; no production helper is
/// called here. Transition state itself is always produced by the real policy.
fn expected(s: &State, input: &Input) -> Effect {
    let browser = match input {
        Input::Reserve { browser }
        | Input::Redeem { browser, .. }
        | Input::Complete { browser } => *browser,
        _ => return Effect::Observed,
    };
    let rejected = if s.availability != Availability::Enabled
        && !(s.availability == Availability::IdentityDisabled
            && s.transaction.purpose == Purpose::Enroll)
    {
        Some(Error::Unavailable)
    } else if s.now >= EXPIRY {
        Some(Error::Expired)
    } else if !browser {
        Some(Error::BrowserMismatch)
    } else if s.changed != 0 {
        Some(Error::Stale)
    } else {
        None
    };
    if let Some(error) = rejected {
        return Effect::Rejected(error);
    }
    let tx = &s.transaction;
    match input {
        Input::Reserve { .. } if tx.phase == Phase::Pending => Effect::Reserved {
            transaction_id: tx.id.clone(),
        },
        Input::Redeem { matches, .. }
            if tx.phase == Phase::Pending && tx.attempts < core::CODE_ATTEMPTS =>
        {
            Effect::CodeAttempt {
                attempt: tx.attempts + 1,
                matched: *matches,
            }
        }
        Input::Redeem { .. }
            if tx.phase == Phase::Pending && tx.attempts >= core::CODE_ATTEMPTS =>
        {
            Effect::Rejected(Error::AttemptsExhausted)
        }
        Input::Complete { .. } if tx.phase == Phase::Exchanging => Effect::Completed {
            transaction_id: tx.id.clone(),
            purpose: tx.purpose,
            user_id: tx.user_id.clone(),
        },
        _ => Effect::Rejected(Error::AlreadyUsed),
    }
}

fn check(name: &'static str, passed: bool) -> Check {
    if passed {
        Check::passed(name)
    } else {
        Check::failed(name, "sign-in policy invariant")
    }
}

impl Model for Signin {
    type State = State;
    type Input = Input;
    type Output = Effect;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: format!(
                "motion.signin.{}.{:?}",
                if self.email { "email" } else { "oidc" },
                self.purpose
            ),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/signin.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            transaction: transaction(self.purpose),
            changed: 0,
            availability: Availability::Enabled,
            now: 0,
            reservations: 0,
            completions: 0,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let output = match input {
            Input::Reserve { browser } => match core::reserve(&s.transaction, &s.current(*browser))
            {
                Ok(tx) => {
                    next.transaction = tx;
                    next.reservations += 1;
                    Effect::Reserved {
                        transaction_id: s.transaction.id.clone(),
                    }
                }
                Err(error) => Effect::Rejected(error),
            },
            Input::Redeem { browser, matches } => {
                match core::redeem_code(&s.transaction, &s.current(*browser), *matches) {
                    Ok((tx, matched)) => {
                        let attempt = tx.attempts;
                        next.transaction = tx;
                        next.reservations += u8::from(matched);
                        Effect::CodeAttempt { attempt, matched }
                    }
                    Err(error) => Effect::Rejected(error),
                }
            }
            Input::Complete { browser } => {
                match core::complete(&s.transaction, &s.current(*browser)) {
                    Ok(tx) => {
                        next.transaction = tx;
                        next.completions += 1;
                        Effect::Completed {
                            transaction_id: s.transaction.id.clone(),
                            purpose: s.transaction.purpose,
                            user_id: s.transaction.user_id.clone(),
                        }
                    }
                    Err(error) => Effect::Rejected(error),
                }
            }
            Input::Change(bit) => {
                next.changed |= bit;
                Effect::Observed
            }
            Input::Availability(value) => {
                next.availability = *value;
                Effect::Observed
            }
            Input::At(now) => {
                next.now = *now;
                Effect::Observed
            }
            Input::CrashAndRestart => {
                next.changed |= SERVER_EPOCH;
                Effect::Observed
            }
        };
        Ok(Transition::accepted(next, vec![output]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            check(
                "signin.single_exchange_and_completion",
                s.reservations <= 1 && s.completions <= 1 && s.completions <= s.reservations,
            ),
            check(
                "signin.bounded_code_attempts",
                s.transaction.attempts <= core::CODE_ATTEMPTS,
            ),
            check(
                "signin.consumed_has_one_completion",
                (s.transaction.phase == Phase::Consumed) == (s.completions == 1),
            ),
            check(
                "signin.failed_code_is_exhausted",
                s.transaction.phase != Phase::Failed
                    || s.transaction.attempts == core::CODE_ATTEMPTS,
            ),
            check("signin.immutable_purpose_identity_bindings", {
                let initial = transaction(self.purpose);
                let tx = &s.transaction;
                tx.id == initial.id
                    && tx.purpose == initial.purpose
                    && tx.user_id == initial.user_id
                    && tx.user_revision == initial.user_revision
                    && tx.identity_revision == initial.identity_revision
                    && tx.actor_revision == initial.actor_revision
                    && tx.browser_hash == initial.browser_hash
                    && tx.epoch == initial.epoch
                    && tx.server_epoch == initial.server_epoch
                    && tx.provider_revision == initial.provider_revision
                    && tx.expires_at == initial.expires_at
            }),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        t: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let outcome = expected(before, input);
        let after = t.state;
        let unchanged = after == before;
        let phase = before.transaction.phase;
        let next_phase = after.transaction.phase;
        let expected_phase = match &outcome {
            Effect::Reserved { .. } => Phase::Exchanging,
            Effect::CodeAttempt { matched: true, .. } => Phase::Exchanging,
            Effect::CodeAttempt {
                matched: false,
                attempt,
            } if *attempt == core::CODE_ATTEMPTS => Phase::Failed,
            Effect::Completed { .. } => Phase::Consumed,
            _ => phase,
        };
        let completed_delta = u8::from(matches!(outcome, Effect::Completed { .. }));
        let reserved_delta = u8::from(matches!(
            outcome,
            Effect::Reserved { .. } | Effect::CodeAttempt { matched: true, .. }
        ));
        Ok(vec![
            check(
                "signin.exact_single_outcome",
                t.outputs == [outcome.clone()],
            ),
            check("signin.required_next_phase", next_phase == expected_phase),
            check(
                "signin.rejection_changes_nothing",
                !matches!(outcome, Effect::Rejected(_)) || unchanged,
            ),
            check(
                "signin.exact_exchange_completion_counts",
                after.completions == before.completions + completed_delta
                    && after.reservations == before.reservations + reserved_delta,
            ),
            check(
                "signin.exact_attempt_count",
                after.transaction.attempts
                    == before.transaction.attempts
                        + u32::from(matches!(outcome, Effect::CodeAttempt { .. })),
            ),
            check(
                "signin.never_reset_reserved_or_terminal_phase",
                phase == Phase::Pending || next_phase != Phase::Pending,
            ),
            check(
                "signin.crash_preserves_durable_transaction",
                !matches!(input, Input::CrashAndRestart)
                    || (before.transaction == after.transaction
                        && after.changed & SERVER_EPOCH != 0),
            ),
        ])
    }
}

impl Enumerate for Signin {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Complete { browser: true },
            Input::Complete { browser: false },
            Input::CrashAndRestart,
        ];
        for browser in [false, true] {
            if self.email {
                for matches in [false, true] {
                    inputs.push(Input::Redeem { browser, matches });
                }
            } else {
                inputs.push(Input::Reserve { browser });
            }
        }
        for bit in [
            RESTORE_EPOCH,
            SERVER_EPOCH,
            PROVIDER_REVISION,
            USER_REVISION,
            USER_IDENTITY,
            IDENTITY_REVISION,
            ACTOR_REVISION,
        ] {
            if s.changed & bit == 0 {
                inputs.push(Input::Change(bit));
            }
        }
        if s.availability == Availability::Enabled {
            for value in [
                Availability::RestoreHold,
                Availability::MethodDisabled,
                Availability::UserDisabled,
                Availability::IdentityDisabled,
            ] {
                inputs.push(Input::Availability(value));
            }
        }
        for now in [EXPIRY - 1, EXPIRY] {
            if now > s.now {
                inputs.push(Input::At(now));
            }
        }
        Ok(inputs)
    }
}

#[test]
fn bounded_signin_interleavings_execute_production_decisions() {
    for purpose in [
        Purpose::Login,
        Purpose::Link,
        Purpose::Test,
        Purpose::Reauth,
        Purpose::Enroll,
    ] {
        for email in [false, true] {
            let report = stateless::explore::enumerate(
                &Signin { email, purpose },
                stateless::explore::SearchConfig {
                    max_states: 50_000,
                    max_transitions: 1_000_000,
                    max_depth: 20,
                },
            )
            .unwrap();
            assert!(
                report.failure.is_none(),
                "purpose={purpose:?} email={email}: {:?}",
                report.failure
            );
            assert_eq!(report.skipped_checks, 0);
            assert_eq!(
                report.termination,
                stateless::explore::SearchTermination::GraphExhausted,
                "purpose={purpose:?} email={email}"
            );
            println!(
                "Signin {purpose:?} {}: {} states, {} transitions; one transaction, two browsers, restore/server epochs and provider/user/identity revision plus user identity and initiating-device revision changes, four availability failures, times 0/599/600, attempts 0..=5; no adapter atomicity claim",
                if email { "email" } else { "oidc" },
                report.states,
                report.transitions
            );
        }
    }
}

#[test]
fn code_attempt_bound_and_single_use_are_exact() {
    let model = Signin {
        email: true,
        purpose: Purpose::Login,
    };
    let state = model.initial_state().unwrap();
    let current = state.current(true);
    let mut tx = transaction(Purpose::Login);
    for attempt in 1..=core::CODE_ATTEMPTS {
        let (next, matched) = core::redeem_code(&tx, &current, false).unwrap();
        assert!(!matched);
        assert_eq!(next.attempts, attempt);
        assert_eq!(
            next.phase,
            if attempt == core::CODE_ATTEMPTS {
                Phase::Failed
            } else {
                Phase::Pending
            }
        );
        tx = next;
    }
    assert_eq!(
        core::redeem_code(&tx, &current, true),
        Err(Error::AlreadyUsed)
    );
    tx.phase = Phase::Pending;
    assert_eq!(
        core::redeem_code(&tx, &current, true),
        Err(Error::AttemptsExhausted)
    );
    tx.attempts = core::CODE_ATTEMPTS - 1;
    let (reserved, matched) = core::redeem_code(&tx, &current, true).unwrap();
    assert!(matched);
    assert_eq!(reserved.attempts, core::CODE_ATTEMPTS);
    let consumed = core::complete(&reserved, &current).unwrap();
    assert_eq!(core::complete(&consumed, &current), Err(Error::AlreadyUsed));
    assert_eq!(core::reserve(&reserved, &current), Err(Error::AlreadyUsed));
}

#[test]
fn current_user_binding_distinguishes_absent_and_same_revision_other_user() {
    let state = Signin {
        email: false,
        purpose: Purpose::Link,
    }
    .initial_state()
    .unwrap();
    let tx = &state.transaction;
    let mut current = state.current(true);
    current.user_id = Some("other-user");
    assert_eq!(core::reserve(tx, &current), Err(Error::Stale));
    current.user_id = None;
    assert_eq!(core::reserve(tx, &current), Err(Error::Stale));
    let mut anonymous = tx.clone();
    anonymous.user_id = None;
    anonymous.user_revision = None;
    anonymous.identity_revision = None;
    current.user_revision = None;
    current.identity_revision = None;
    current.identity_owner = None;
    assert_eq!(
        core::reserve(&anonymous, &current).unwrap().phase,
        Phase::Exchanging
    );
    current.user_id = Some("user-1");
    assert_eq!(core::reserve(&anonymous, &current), Err(Error::Stale));
}

#[test]
fn exact_identity_ownership_never_follows_contact_email_or_reassigns_owner() {
    assert_eq!(core::link_owner(None, "alice"), Ok(()));
    assert_eq!(core::link_owner(Some("alice"), "alice"), Ok(()));
    assert_eq!(
        core::link_owner(Some("alice"), "bob"),
        Err(Error::IdentityConflict)
    );
    assert_eq!(
        core::link_owner(Some("Alice"), "alice"),
        Err(Error::IdentityConflict)
    );
}

#[test]
fn family_requires_every_live_method_user_revision_and_epoch_fact() {
    for mask in 0u16..256 {
        let family = core::Family {
            revoked: mask & 1 != 0,
            epoch: "epoch-1",
            current_epoch: if mask & 2 == 0 { "epoch-1" } else { "epoch-2" },
            expires_at: 100,
            user_enabled: mask & 4 == 0,
            identity_enabled: mask & 8 == 0,
            identity_revision: 7,
            current_identity_revision: if mask & 16 == 0 { 7 } else { 8 },
            method_enabled: mask & 32 == 0,
            method_revision: 11,
            current_method_revision: if mask & 64 == 0 { 11 } else { 12 },
        };
        let now = if mask & 128 == 0 { 99 } else { 100 };
        assert_eq!(
            core::family_valid(&family, now),
            mask & 255 == 0,
            "mask={mask}"
        );
    }
}

#[test]
fn proof_binds_actor_action_target_epoch_approver_expiry_and_single_use() {
    for mask in 0u16..128 {
        let proof = core::Proof {
            actor: "actor-1",
            action: "provider.write",
            target: "provider-1",
            epoch: "epoch-1",
            expires_at: 100,
            approved_by: if mask & 1 == 0 {
                Some("approver-2")
            } else {
                None
            },
            consumed: mask & 2 != 0,
        };
        let valid = core::proof_valid(
            &proof,
            if mask & 4 == 0 { "actor-1" } else { "actor-2" },
            if mask & 8 == 0 {
                "provider.write"
            } else {
                "identity.unlink"
            },
            if mask & 16 == 0 {
                "provider-1"
            } else {
                "provider-2"
            },
            if mask & 32 == 0 { "epoch-1" } else { "epoch-2" },
            if mask & 64 == 0 { 99 } else { 100 },
        );
        assert_eq!(valid, mask & 127 == 0, "mask={mask}");
    }
    let proof = core::Proof {
        actor: "same",
        action: "a",
        target: "t",
        epoch: "e",
        expires_at: 10,
        approved_by: Some("same"),
        consumed: false,
    };
    assert!(!core::proof_valid(&proof, "same", "a", "t", "e", 0));
    for actor_is_approver in [false, true] {
        for admin in [false, true] {
            for same_user in [false, true] {
                assert_eq!(
                    core::can_approve_proof(
                        "a",
                        if actor_is_approver { "a" } else { "b" },
                        admin,
                        same_user
                    ),
                    !actor_is_approver && (admin || same_user)
                );
            }
        }
    }
}

#[test]
fn removing_last_method_needs_explicit_local_recovery() {
    assert_eq!(core::preserve_method(0, false), Err(Error::LastMethod));
    assert_eq!(core::preserve_method(0, true), Ok(()));
    for count in [1, 2, usize::MAX] {
        assert_eq!(core::preserve_method(count, false), Ok(()));
    }
}

#[test]
fn freshness_is_bounded_and_rejects_future_or_overflowing_observations() {
    for (authenticated_at, now, valid) in [
        (100, 100, true),
        (100, 100 + core::FRESH_SECONDS, true),
        (100, 101 + core::FRESH_SECONDS, false),
        (100, 99, false),
        (i64::MIN, i64::MAX, false),
        (i64::MAX, i64::MIN, false),
        (i64::MIN, i64::MIN + core::FRESH_SECONDS, true),
        (i64::MAX - core::FRESH_SECONDS, i64::MAX, true),
    ] {
        assert_eq!(
            core::fresh(authenticated_at, now),
            valid,
            "{authenticated_at}/{now}"
        );
    }
}

#[test]
fn email_policy_has_one_conservative_canonical_form_without_provider_aliases() {
    for (input, expected) in [
        (
            "  Alice.Example+tag@EXAMPLE.COM  ",
            "alice.example+tag@example.com",
        ),
        ("user_name@sub.example.com", "user_name@sub.example.com"),
        (
            "!#$%&'*+-/=?^_`{|}~@example.com",
            "!#$%&'*+-/=?^_`{|}~@example.com",
        ),
    ] {
        assert_eq!(core::canonical_email(input).as_deref(), Some(expected));
    }
    assert_ne!(
        core::canonical_email("a.b@example.com"),
        core::canonical_email("ab@example.com")
    );
    assert_ne!(
        core::canonical_email("a+tag@example.com"),
        core::canonical_email("a@example.com")
    );
    for invalid in [
        "",
        "a",
        "@example.com",
        "a@",
        "a@localhost",
        "a@@example.com",
        "a b@example.com",
        ".a@example.com",
        "a.@example.com",
        "a..b@example.com",
        "a@.example.com",
        "a@example..com",
        "a@-example.com",
        "a@example-.com",
        "á@example.com",
        "a@例.example",
        "\"a\"@example.com",
        "a@example.com\r\nBcc: b@example.com",
        "a\0@example.com",
    ] {
        assert_eq!(core::canonical_email(invalid), None, "{invalid:?}");
    }
    assert!(core::canonical_email(&format!("{}@example.com", "a".repeat(64))).is_some());
    assert!(core::canonical_email(&format!("{}@example.com", "a".repeat(65))).is_none());
    assert!(core::canonical_email(&format!("a@{}.com", "x".repeat(63))).is_some());
    assert!(core::canonical_email(&format!("a@{}.com", "x".repeat(64))).is_none());
    assert!(core::canonical_email(&format!("a@{}.example.com", "x".repeat(254))).is_none());
}

#[test]
fn return_target_is_same_server_path_without_encoded_or_header_redirects() {
    for valid in ["/", "/settings/access", "/settings/signin", "/library/abc"] {
        assert!(core::safe_return(valid), "{valid}");
    }
    for invalid in [
        "",
        "https://other.example/",
        "//other.example/",
        "/\\other.example",
        "/\t/other.example",
        "/\0other.example",
        "/\u{7f}other.example",
        "/%2f%2fother.example",
        "/api/v2/auth/session",
        "/signin?next=//other.example",
        "/settings/access#methods",
        "/\r\nLocation: https://other.example/",
    ] {
        assert!(!core::safe_return(invalid), "{invalid:?}");
    }
    assert!(core::safe_return(&format!("/{}", "a".repeat(511))));
    assert!(!core::safe_return(&format!("/{}", "a".repeat(512))));
}

#[test]
fn session_expiry_is_bounded_by_proof_time_not_delayed_issuance() {
    assert_eq!(
        core::session_expiry(1_000, 1_300),
        Some(1_000 + core::SESSION_MAX)
    );
    assert_eq!(
        core::session_expiry(1_030, 1_000),
        Some(1_000 + core::SESSION_MAX)
    );
    assert_eq!(core::session_expiry(1_000, 1_000 + core::SESSION_MAX), None);
    assert_eq!(core::session_expiry(i64::MAX, i64::MAX), None);
}
