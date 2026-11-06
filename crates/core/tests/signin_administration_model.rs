//! Bounded administrative lifecycle uses production decisions for rate budgets,
//! terminal test receipts and consume-before-issuance bootstrap semantics.
use playscale_core::{access, signin};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    attempts: u32,
    confirmed: bool,
    now: i64,
    window: Option<(i64, i64)>,
    proof: bool,
    reservations: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    Confirm(bool),
    At(i64),
    Rate,
    Proof(bool),
    PersistenceFailed,
}
struct Administration;
impl Model for Administration {
    type State = State;
    type Input = Input;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.signin.administration".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: format!(
                "{}{}",
                include_str!("../src/signin.rs"),
                include_str!("../src/access.rs")
            ),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            attempts: 0,
            confirmed: false,
            now: 0,
            window: None,
            proof: true,
            reservations: 0,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, String>, ModelError> {
        let mut n = s.clone();
        let output = match i {
            Input::Confirm(matches) => match signin::confirm_mail_test(
                s.confirmed,
                !s.confirmed,
                s.attempts,
                10,
                s.now,
                *matches,
            ) {
                signin::TestConfirmationDecision::Replay => "replay",
                signin::TestConfirmationDecision::Reject => "reject",
                signin::TestConfirmationDecision::Record {
                    attempts,
                    confirmed,
                } => {
                    n.attempts = attempts;
                    n.confirmed = confirmed;
                    "record"
                }
            },
            Input::At(now) => {
                n.now = *now;
                "clock"
            }
            Input::Rate => match signin::reserve_rate_window(s.window, s.now, 2, 10) {
                signin::RateAdmission::Reserve { start, attempts } => {
                    n.window = Some((start, attempts));
                    "reserve"
                }
                signin::RateAdmission::Limited { .. } => "limited",
            },
            Input::Proof(matches) => match access::admit_bootstrap(
                s.proof.then_some("hash"),
                if *matches { "hash" } else { "wrong" },
                s.now as u64,
                10,
            ) {
                access::BootstrapAdmission::Keep => "keep",
                access::BootstrapAdmission::Expire => {
                    n.proof = false;
                    "expired"
                }
                access::BootstrapAdmission::ReserveAndBurn => {
                    n.proof = false;
                    n.reservations += 1;
                    "burn"
                }
            },
            Input::PersistenceFailed => "rollback_no_proof_reissue",
        };
        Ok(Transition::accepted(n, vec![output.into()]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            if s.attempts <= 5 && s.reservations <= 1 && s.window.is_none_or(|(_, n)| n <= 2) {
                Check::passed("administration.bounds")
            } else {
                Check::failed("administration.bounds", "budget or proof reused")
            },
        ])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match i {
            Input::Confirm(matches) => {
                if s.confirmed {
                    "replay"
                } else if s.now >= 10 || s.attempts >= 5 {
                    "reject"
                } else {
                    if *matches && !n.state.confirmed {
                        return Ok(vec![Check::failed(
                            "receipt",
                            "matching code not confirmed",
                        )]);
                    }
                    "record"
                }
            }
            Input::At(_) => "clock",
            Input::Rate => {
                if s.window
                    .is_some_and(|(at, count)| s.now - at < 10 && count >= 2)
                {
                    "limited"
                } else {
                    "reserve"
                }
            }
            Input::Proof(matches) => {
                if !s.proof {
                    "keep"
                } else if s.now >= 10 {
                    "expired"
                } else if *matches {
                    "burn"
                } else {
                    "keep"
                }
            }
            Input::PersistenceFailed => "rollback_no_proof_reissue",
        };
        let exact = n.outputs == [expected.to_owned()]
            && (!s.confirmed || n.state.confirmed)
            && (!matches!(i, Input::PersistenceFailed) || n.state == s);
        Ok(vec![if exact {
            Check::passed("administration.exact_outcomes")
        } else {
            Check::failed(
                "administration.exact_outcomes",
                "terminal replay or reservation changed",
            )
        }])
    }
}
impl Enumerate for Administration {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![
            Input::Confirm(false),
            Input::Confirm(true),
            Input::Rate,
            Input::Proof(false),
            Input::Proof(true),
            Input::PersistenceFailed,
        ];
        for now in [9, 10, 20] {
            if now > s.now {
                inputs.push(Input::At(now));
            }
        }
        Ok(inputs)
    }
}
#[test]
fn bounded_administrative_sequences() {
    let r = stateless::explore::enumerate(
        &Administration,
        stateless::explore::SearchConfig {
            max_states: 20000,
            max_transitions: 200000,
            max_depth: 25,
        },
    )
    .unwrap();
    assert!(r.failure.is_none(), "{:?}", r.failure);
    assert_eq!(r.skipped_checks, 0);
    assert_eq!(
        r.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "administration: {} states, {} transitions",
        r.states, r.transitions
    );
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RequestState {
    pending: bool,
    revision: u64,
    now: i64,
    current: bool,
    linked: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum RequestInput {
    Approve { revision: u64, recovery: bool },
    Deny(u64),
    Expire,
    MethodChanged,
    Clock,
}
struct Requests;
impl Model for Requests {
    type State = RequestState;
    type Input = RequestInput;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.signin.request".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/signin.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<Self::State, ModelError> {
        Ok(RequestState {
            pending: true,
            revision: 1,
            now: 0,
            current: true,
            linked: 0,
        })
    }
    fn step(
        &self,
        s: &Self::State,
        i: &Self::Input,
    ) -> Result<Transition<Self::State, String>, ModelError> {
        let mut n = s.clone();
        let action = match i {
            RequestInput::Approve { revision, recovery } => {
                Some((*revision, signin::RequestAction::Approve, *recovery))
            }
            RequestInput::Deny(revision) => Some((*revision, signin::RequestAction::Deny, false)),
            RequestInput::Expire => Some((s.revision, signin::RequestAction::Expire, false)),
            RequestInput::MethodChanged => {
                n.current = false;
                None
            }
            RequestInput::Clock => {
                n.now = 10;
                None
            }
        };
        let mut output = "observe";
        if let Some((expected, action, recovery)) = action {
            let result = signin::decide_request(
                &signin::RequestObservation {
                    pending: s.pending,
                    revision: s.revision,
                    expires_at: 10,
                    method_current: s.current,
                },
                expected,
                action,
                recovery,
                s.now,
            );
            output = "refuse";
            if let Ok(revision) = result {
                n.pending = false;
                n.revision = revision;
                if action == signin::RequestAction::Approve {
                    n.linked += 1;
                }
                output = "commit";
            }
        }
        Ok(Transition::accepted(n, vec![output.into()]))
    }
    fn check_state(&self, state: &RequestState) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            if state.linked <= 1 && (state.linked == 0 || !state.pending) {
                Check::passed("request.one_terminal_effect")
            } else {
                Check::failed(
                    "request.one_terminal_effect",
                    "duplicate or nonterminal link",
                )
            },
        ])
    }
    fn check_transition(
        &self,
        s: &Self::State,
        i: &Self::Input,
        n: &TransitionRef<'_, Self::State, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let safe = n.state.linked <= 1
            && (s.pending || n.state.linked == s.linked)
            && (n.state.linked == s.linked
                || (s.current
                    && s.now < 10
                    && matches!(
                        i,
                        RequestInput::Approve {
                            revision: 1,
                            recovery: true
                        }
                    )))
            && (n.outputs != ["refuse"] || n.state == s);
        Ok(vec![if safe {
            Check::passed("request.exact_single_link")
        } else {
            Check::failed(
                "request.exact_single_link",
                "stale request changed identity",
            )
        }])
    }
}
impl Enumerate for Requests {
    fn inputs(&self, _: &RequestState) -> Result<Vec<RequestInput>, ModelError> {
        Ok(vec![
            RequestInput::Approve {
                revision: 1,
                recovery: true,
            },
            RequestInput::Approve {
                revision: 1,
                recovery: false,
            },
            RequestInput::Approve {
                revision: 2,
                recovery: true,
            },
            RequestInput::Deny(1),
            RequestInput::Deny(2),
            RequestInput::Expire,
            RequestInput::Clock,
            RequestInput::MethodChanged,
        ])
    }
}
#[test]
fn competing_request_decisions_are_single_effect() {
    let r = stateless::explore::enumerate(
        &Requests,
        stateless::explore::SearchConfig {
            max_states: 1000,
            max_transitions: 10000,
            max_depth: 20,
        },
    )
    .unwrap();
    assert!(r.failure.is_none(), "{:?}", r.failure);
    assert_eq!(r.skipped_checks, 0);
    assert_eq!(
        r.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "requests: {} states, {} transitions",
        r.states, r.transitions
    );
}
