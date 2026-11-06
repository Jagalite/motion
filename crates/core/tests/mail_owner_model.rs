//! Exhaustive bounded typed-owner admission; production mail policy is used
//! directly. SQL observations and transport execution remain adapter tests.
use playscale_core::mail::{self as core, Message, Mode, OwnerObservation, Phase, Purpose};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State(bool);
#[derive(Clone, Debug, PartialEq, Eq)]
struct Input {
    purpose: u8,
    owner: u8,
    flags: u8,
    expires: i64,
}
struct Owners;
fn purpose(n: u8) -> Purpose {
    [
        Purpose::SigninCode,
        Purpose::RegistrationCode,
        Purpose::Invitation,
        Purpose::Test,
        Purpose::AccountNotice,
    ][n as usize]
}
impl Model for Owners {
    type State = State;
    type Input = Input;
    type Output = bool;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.mail.owners".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/mail.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State(false))
    }
    fn step(&self, _: &State, i: &Input) -> Result<Transition<State, bool>, ModelError> {
        let m = Message {
            id: "m".into(),
            purpose: purpose(i.purpose),
            owner: "o".into(),
            mode: Mode::Relay,
            config_revision: 1,
            key_id: None,
            epoch: "current".into(),
            expires_at: 20,
            phase: Phase::Queued,
            attempts: 0,
            lease: None,
            next_attempt: 0,
            ambiguous: false,
        };
        let owner = match i.owner {
            0 => OwnerObservation::Missing,
            1 => OwnerObservation::Challenge {
                purpose: if i.flags & 8 != 0 {
                    playscale_core::signin::Purpose::Register
                } else {
                    playscale_core::signin::Purpose::Login
                },
                pending: i.flags & 1 != 0,
                server_epoch: if i.flags & 2 != 0 { "server" } else { "old" },
                epoch: if i.flags & 4 != 0 { "current" } else { "old" },
            },
            2 => OwnerObservation::Invitation {
                revoked: i.flags & 1 != 0,
                device_claimed: i.flags & 2 != 0,
                identity_claimed: i.flags & 4 != 0,
                expires_at: i.expires,
            },
            3 => OwnerObservation::Test {
                expires_at: i.expires,
            },
            _ => OwnerObservation::AccountNotice,
        };
        let admitted = core::owner_eligible(&m, owner, "server", 10);
        Ok(Transition::accepted(State(admitted), vec![admitted]))
    }
    fn check_state(&self, _: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![])
    }
    fn check_transition(
        &self,
        _: &State,
        i: &Input,
        n: &TransitionRef<'_, State, bool>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = match (i.purpose, i.owner) {
            (0 | 1, 1) => i.flags & 7 == 7 && ((i.purpose == 1) == (i.flags & 8 != 0)),
            (2, 2) => i.flags & 7 == 0 && i.expires > 10,
            (3, 3) => i.expires > 10,
            (4, 4) => true,
            _ => false,
        };
        Ok(vec![if n.state.0 == expected && n.outputs == [expected] {
            Check::passed("mail.exact_owner_admission")
        } else {
            Check::failed(
                "mail.exact_owner_admission",
                "purpose/history/redemption/deadline mismatch",
            )
        }])
    }
}
impl Enumerate for Owners {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = vec![];
        for purpose in 0..5 {
            for owner in 0..5 {
                for flags in 0..16 {
                    for expires in [9, 10, 11] {
                        inputs.push(Input {
                            purpose,
                            owner,
                            flags,
                            expires,
                        });
                    }
                }
            }
        }
        Ok(inputs)
    }
}
#[test]
fn bounded_typed_owners() {
    let r = stateless::explore::enumerate(
        &Owners,
        stateless::explore::SearchConfig {
            max_states: 4,
            // Two admitted/refused states × 1,200 purpose/owner/fact inputs.
            max_transitions: 3000,
            max_depth: 3,
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
        "mail owners: {} states, {} transitions",
        r.states, r.transitions
    );
}
