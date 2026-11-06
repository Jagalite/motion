//! Bounded interleavings of the production outbox delivery policy with two
//! workers, crash/lease expiry, recheck before DATA, every observation kind and
//! owner/configuration/epoch invalidation. The model applies exactly the
//! message returned by core; it does not prove SQLite atomicity or SMTP/DNS
//! conformance, which the adapter and API tests exercise separately.
use playscale_core::mail::{
    self as core, Current, Decision, Error, Message, Mode, Observation, PayloadAvailability, Phase,
    Purpose, Stage,
};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};

const EXPIRES: i64 = 2 * core::LEASE_SECONDS;

fn message() -> Message {
    Message {
        id: "message-1".into(),
        purpose: Purpose::RegistrationCode,
        owner: "transaction-1".into(),
        mode: Mode::Direct,
        config_revision: 1,
        key_id: Some("key-1".into()),
        epoch: "epoch-1".into(),
        expires_at: EXPIRES,
        phase: Phase::Queued,
        attempts: 0,
        lease: None,
        next_attempt: 0,
        ambiguous: false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Worker {
    /// Lease token held by this worker's in-flight attempt.
    token: Option<String>,
    /// The worker passed the pre-content recheck and sent DATA content.
    transmitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    message: Message,
    workers: [Worker; 2],
    now: i64,
    owner_live: bool,
    config_changed: bool,
    epoch_changed: bool,
    disabled: bool,
    /// Successful claims (durable attempt reservations).
    claims: u32,
    /// Content transmissions after a successful recheck.
    transmissions: u32,
    /// Transmissions performed while the message was invalid. Must stay 0.
    invalid_transmissions: u32,
    payload: bool,
}

impl State {
    fn current(&self) -> Current<'_> {
        Current {
            now: self.now,
            enabled: !self.disabled,
            mode: Mode::Direct,
            config_revision: if self.config_changed { 2 } else { 1 },
            key_id: Some("key-1"),
            epoch: if self.epoch_changed {
                "epoch-2"
            } else {
                "epoch-1"
            },
            restore_hold: false,
            owner_live: self.owner_live,
        }
    }
    fn valid(&self) -> bool {
        self.now < EXPIRES
            && self.owner_live
            && !self.config_changed
            && !self.epoch_changed
            && !self.disabled
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Claim(usize),
    Transmit(usize),
    Observe(usize, Observation),
    Crash(usize),
    OwnerCompleted,
    ConfigChanged,
    EpochChanged,
    Disabled,
    LosePayload,
    At(i64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Observed,
    Attempt { attempt: u32 },
    Finished(Phase),
    Waiting,
    Transmitted,
    TransmitRefused,
    Recorded(Phase),
    Stale,
}

struct Delivery;

fn check(name: &'static str, passed: bool) -> Check {
    if passed {
        Check::passed(name)
    } else {
        Check::failed(name, "mail delivery invariant")
    }
}

impl Model for Delivery {
    type State = State;
    type Input = Input;
    type Output = Effect;

    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.mail.delivery".into(),
            model_version: 2,
            properties_version: 2,
            codec_version: 1,
            build: include_str!("../src/mail.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        let idle = Worker {
            token: None,
            transmitted: false,
        };
        Ok(State {
            message: message(),
            workers: [idle.clone(), idle],
            now: 0,
            owner_live: true,
            config_changed: false,
            epoch_changed: false,
            disabled: false,
            claims: 0,
            transmissions: 0,
            invalid_transmissions: 0,
            payload: true,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match input {
            Input::Claim(w) => {
                let token = format!("w{w}-{}", s.claims + 1);
                let availability = if s.payload {
                    PayloadAvailability::Available
                } else {
                    PayloadAvailability::Unavailable
                };
                match core::claim_with_payload(&s.message, &s.current(), &token, availability) {
                    Decision::Attempt(m) => {
                        let attempt = m.attempts;
                        next.message = m;
                        next.claims += 1;
                        next.workers[*w] = Worker {
                            token: Some(token),
                            transmitted: false,
                        };
                        Effect::Attempt { attempt }
                    }
                    Decision::Finish(m) => {
                        let phase = m.phase;
                        next.message = m;
                        Effect::Finished(phase)
                    }
                    Decision::PayloadUnavailable(m) => {
                        let phase = m.phase;
                        next.message = m;
                        Effect::Finished(phase)
                    }
                    Decision::Wait => Effect::Waiting,
                }
            }
            Input::Transmit(w) => {
                let token = s.workers[*w].token.as_deref().unwrap_or_default();
                if core::may_transmit(&s.message, token, &s.current()) {
                    next.workers[*w].transmitted = true;
                    next.transmissions += 1;
                    next.invalid_transmissions += u32::from(!s.valid());
                    Effect::Transmitted
                } else {
                    Effect::TransmitRefused
                }
            }
            Input::Observe(w, observation) => {
                let token = s.workers[*w].token.clone().unwrap_or_default();
                next.workers[*w] = Worker {
                    token: None,
                    transmitted: false,
                };
                match core::observe(&s.message, &token, *observation, &s.current()) {
                    Ok(m) => {
                        let phase = m.phase;
                        next.message = m;
                        Effect::Recorded(phase)
                    }
                    Err(Error::StaleAttempt) => Effect::Stale,
                }
            }
            Input::Crash(w) => {
                next.workers[*w] = Worker {
                    token: None,
                    transmitted: false,
                };
                Effect::Observed
            }
            Input::OwnerCompleted => {
                next.owner_live = false;
                next.message = core::cancel(&s.message);
                Effect::Observed
            }
            Input::ConfigChanged => {
                next.config_changed = true;
                next.message = core::cancel(&s.message);
                Effect::Observed
            }
            Input::EpochChanged => {
                next.epoch_changed = true;
                Effect::Observed
            }
            Input::Disabled => {
                next.disabled = true;
                Effect::Observed
            }
            Input::LosePayload => {
                next.payload = false;
                Effect::Observed
            }
            Input::At(now) => {
                next.now = *now;
                Effect::Observed
            }
        };
        // The adapter erases secret payloads in the same write as a terminal phase.
        if next.message.phase.terminal() {
            next.payload = false;
        }
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        let m = &s.message;
        Ok(vec![
            check("mail.bounded_attempts", m.attempts <= core::MAX_ATTEMPTS),
            check("mail.attempts_match_reservations", m.attempts == s.claims),
            check(
                "mail.lease_iff_sending",
                (m.phase == Phase::Sending) == m.lease.is_some(),
            ),
            check(
                "mail.no_transmission_while_invalid",
                s.invalid_transmissions == 0,
            ),
            check(
                "mail.accepted_requires_transmission",
                m.phase != Phase::Accepted || s.transmissions > 0,
            ),
            check(
                "mail.unknown_only_after_ambiguity",
                m.phase != Phase::Unknown || m.ambiguous,
            ),
            check(
                "mail.terminal_erases_payload",
                !m.phase.terminal() || !s.payload,
            ),
            check(
                "mail.immutable_identity",
                m.id == "message-1"
                    && m.owner == "transaction-1"
                    && m.config_revision == 1
                    && m.epoch == "epoch-1"
                    && m.expires_at == EXPIRES,
            ),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        t: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let (b, a) = (&before.message, &t.state.message);
        let effect = &t.outputs[0];
        let lease_holder = |w: usize| {
            before.workers[w].token.is_some()
                && b.phase == Phase::Sending
                && b.lease.as_ref().map(|l| &l.token) == before.workers[w].token.as_ref()
        };
        let expected_record = match input {
            Input::Observe(w, observation) if lease_holder(*w) => Some(match observation {
                Observation::Accepted => Phase::Accepted,
                Observation::Permanent(_) => Phase::Failed,
                _ if !before.valid() => {
                    if before.now >= EXPIRES {
                        Phase::Expired
                    } else {
                        Phase::Cancelled
                    }
                }
                _ if b.attempts >= core::MAX_ATTEMPTS => {
                    if b.ambiguous || *observation == Observation::Ambiguous {
                        Phase::Unknown
                    } else {
                        Phase::Failed
                    }
                }
                _ if before.now + core::backoff(b.attempts) >= EXPIRES => Phase::Expired,
                _ => Phase::Queued,
            }),
            _ => None,
        };
        Ok(vec![
            check(
                "mail.terminal_is_absorbing",
                !b.phase.terminal() || a.phase == b.phase,
            ),
            check(
                "mail.no_new_attempt_when_invalid",
                !matches!(effect, Effect::Attempt { .. }) || before.valid(),
            ),
            check(
                "mail.stale_observation_changes_nothing",
                *effect != Effect::Stale || a == b,
            ),
            check(
                "mail.only_lease_holder_records",
                !matches!(input, Input::Observe(w, _) if !lease_holder(*w))
                    || *effect == Effect::Stale,
            ),
            check(
                "mail.exact_recorded_phase",
                expected_record.is_none_or(|phase| *effect == Effect::Recorded(phase)),
            ),
            check(
                "mail.ambiguity_is_never_forgotten",
                !b.ambiguous || a.ambiguous,
            ),
            check(
                "mail.unavailable_payload_preserves_recovery_uncertainty",
                !matches!(input, Input::Claim(_))
                    || before.payload
                    || !matches!(effect, Effect::Finished(Phase::Failed | Phase::Unknown))
                    || (a.attempts == b.attempts
                        && a.lease.is_none()
                        && (!matches!(b.phase, Phase::Sending) || a.ambiguous)),
            ),
            check(
                "mail.retry_waits_for_backoff",
                !matches!(expected_record, Some(Phase::Queued))
                    || a.next_attempt == before.now + core::backoff(b.attempts),
            ),
        ])
    }
}

impl Enumerate for Delivery {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = Vec::new();
        for w in 0..2 {
            let worker = &s.workers[w];
            if worker.token.is_none() {
                inputs.push(Input::Claim(w));
                continue;
            }
            inputs.push(Input::Crash(w));
            if worker.transmitted {
                for observation in [
                    Observation::Accepted,
                    Observation::Ambiguous,
                    Observation::Permanent(Stage::Message),
                    Observation::Temporary(Stage::Message),
                ] {
                    inputs.push(Input::Observe(w, observation));
                }
            } else {
                inputs.push(Input::Transmit(w));
                for observation in [
                    Observation::Temporary(Stage::Connect),
                    Observation::Permanent(Stage::RcptTo),
                    Observation::Withdrawn,
                ] {
                    inputs.push(Input::Observe(w, observation));
                }
            }
        }
        if s.owner_live {
            inputs.push(Input::OwnerCompleted);
        }
        if !s.config_changed {
            inputs.push(Input::ConfigChanged);
        }
        if !s.epoch_changed {
            inputs.push(Input::EpochChanged);
        }
        if !s.disabled {
            inputs.push(Input::Disabled);
        }
        if s.payload && !s.message.phase.terminal() {
            inputs.push(Input::LosePayload);
        }
        // Lease expiry, backoff boundaries and the message deadline.
        for now in [
            30,
            core::LEASE_SECONDS,
            core::LEASE_SECONDS + 30,
            EXPIRES - 1,
            EXPIRES,
        ] {
            if now > s.now {
                inputs.push(Input::At(now));
            }
        }
        Ok(inputs)
    }
}

#[test]
fn bounded_delivery_interleavings_execute_production_decisions() {
    let report = stateless::explore::enumerate(
        &Delivery,
        stateless::explore::SearchConfig {
            max_states: 400_000,
            max_transitions: 8_000_000,
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
        "Mail delivery: {} states, {} transitions; one message, two workers with crash, recheck-before-DATA, accepted/ambiguous/temporary/permanent/withdrawn observations, owner completion, configuration/epoch/enablement invalidation, times 0/30/lease/lease+30/expiry-1/expiry, attempts 0..={}; no SQLite or SMTP conformance claim",
        report.states,
        report.transitions,
        core::MAX_ATTEMPTS
    );
}

#[test]
fn crash_after_data_is_retried_with_recorded_ambiguity_and_same_identity() {
    let s = Delivery.initial_state().unwrap();
    let Decision::Attempt(sending) = core::claim(&s.message, &s.current(), "a") else {
        panic!()
    };
    assert!(core::may_transmit(&sending, "a", &s.current()));
    let mut later = s.clone();
    later.now = core::LEASE_SECONDS;
    let Decision::Attempt(retry) = core::claim(&sending, &later.current(), "b") else {
        panic!()
    };
    assert!(retry.ambiguous);
    assert_eq!(retry.id, sending.id);
    assert!(!core::may_transmit(&retry, "a", &later.current()));
    assert_eq!(
        core::observe(&retry, "a", Observation::Accepted, &later.current()),
        Err(Error::StaleAttempt)
    );
    let mut exhausted = retry.clone();
    exhausted.attempts = core::MAX_ATTEMPTS;
    assert_eq!(
        core::observe(
            &exhausted,
            "b",
            Observation::Temporary(Stage::Connect),
            &later.current()
        )
        .unwrap()
        .phase,
        Phase::Unknown
    );
}
