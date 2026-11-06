//! Issuance through completion: recipient changes, failed persistence and repeat
//! submissions use the production recipient, transaction and effect policies.
use playscale_core::{signin, signin_flows as flow};
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    enabled: bool,
    foreign: bool,
    user_enabled: bool,
    transaction: Option<String>,
    challenge: bool,
    sessions: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    Enabled(bool),
    Foreign(bool),
    UserEnabled(bool),
    Issue { commit: bool },
    Complete { commit: bool },
}
struct Flows;
fn seed() -> signin::Transaction {
    signin::Transaction {
        id: "tx".into(),
        purpose: signin::Purpose::Reauth,
        browser_hash: "browser".into(),
        epoch: "e".into(),
        server_epoch: "s".into(),
        provider_revision: 4,
        user_id: None,
        user_revision: None,
        identity_revision: None,
        actor_revision: Some(3),
        expires_at: 600,
        phase: signin::Phase::Pending,
        attempts: 0,
        registration: None,
    }
}
impl Model for Flows {
    type State = State;
    type Input = Input;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.signin_issuance_completion".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: concat!(
                include_str!("../src/signin_flows.rs"),
                include_str!("../src/signin.rs")
            )
            .into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            enabled: true,
            foreign: false,
            user_enabled: true,
            transaction: None,
            challenge: false,
            sessions: 0,
        })
    }
    fn step(&self, s: &State, i: &Input) -> Result<Transition<State, String>, ModelError> {
        let mut n = s.clone();
        let mut out = vec![];
        match i {
            Input::Enabled(v) => n.enabled = *v,
            Input::Foreign(v) => n.foreign = *v,
            Input::UserEnabled(v) => n.user_enabled = *v,
            Input::Issue { commit } if s.transaction.is_none() => {
                let user = flow::UserBinding {
                    id: "owner".into(),
                    revision: 2,
                };
                let identity = flow::RecipientIdentity {
                    id: "identity".into(),
                    revision: 7,
                    user: flow::UserBinding {
                        id: if s.foreign { "foreign" } else { "owner" }.into(),
                        revision: 2,
                    },
                    enabled: s.enabled,
                    user_enabled: s.user_enabled,
                };
                let recipient = flow::email_recipient(
                    signin::Purpose::Reauth,
                    true,
                    Some(&user),
                    Some(&identity),
                );
                let tx = flow::bind_recipient(seed(), &recipient);
                if *commit {
                    n.challenge = matches!(recipient, flow::Recipient::IssueChallenge { .. });
                    n.transaction = Some(serde_json::to_string(&tx).unwrap());
                    if n.challenge {
                        out.push("challenge:tx:identity".into());
                    }
                }
            }
            Input::Complete { commit } => {
                if let Some(raw) = &s.transaction {
                    let tx: signin::Transaction = serde_json::from_str(raw).unwrap();
                    let current = signin::Current {
                        browser_hash: "browser",
                        epoch: "e",
                        server_epoch: "s",
                        provider_revision: 4,
                        method_enabled: true,
                        restore_hold: false,
                        user_id: tx.user_id.as_deref(),
                        user_revision: tx.user_revision,
                        user_enabled: s.user_enabled,
                        identity_revision: tx.identity_revision,
                        identity_enabled: s.enabled,
                        identity_owner: Some(if s.foreign { "foreign" } else { "owner" }),
                        actor_revision: Some(3),
                        now: 1,
                    };
                    if let Ok((reserved, true)) = signin::redeem_code(&tx, &current, s.challenge)
                        && let Ok(completed) = signin::complete(&reserved, &current)
                        && let Ok(flow::Completion::Session { identity }) = flow::completion(
                            flow::Protocol::Email,
                            completed.purpose,
                            completed.user_id.as_deref(),
                            Some(("identity", if s.foreign { "foreign" } else { "owner" })),
                        )
                        && *commit
                    {
                        n.transaction = Some(serde_json::to_string(&completed).unwrap());
                        n.sessions += 1;
                        out.push(format!("session:{identity}"));
                    }
                }
            }
            _ => {}
        }
        Ok(Transition::accepted(n, out))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.sessions <= 1 {
            Check::passed("signin.at_most_one_family")
        } else {
            Check::failed("signin.at_most_one_family", "duplicate issuance")
        }])
    }
    fn check_transition(
        &self,
        s: &State,
        i: &Input,
        n: &TransitionRef<'_, State, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let allowed = s.enabled && s.user_enabled && !s.foreign;
        let expected_issue =
            matches!(i, Input::Issue { commit: true }) && s.transaction.is_none() && allowed;
        let pending = s.transaction.as_ref().is_some_and(|raw| {
            serde_json::from_str::<signin::Transaction>(raw)
                .unwrap()
                .phase
                == signin::Phase::Pending
        });
        let expected_session =
            matches!(i, Input::Complete { commit: true }) && s.challenge && pending && allowed;
        Ok(vec![
            if n.outputs.iter().any(|s| s == "challenge:tx:identity") == expected_issue
                && n.outputs.iter().any(|s| s == "session:identity") == expected_session
            {
                Check::passed("signin.exact_issuance_and_completion_effects")
            } else {
                Check::failed(
                    "signin.exact_issuance_and_completion_effects",
                    "wrong challenge or session effect",
                )
            },
            if !matches!(
                i,
                Input::Issue { commit: false } | Input::Complete { commit: false }
            ) || n.state == s
            {
                Check::passed("signin.failed_persistence_is_atomic")
            } else {
                Check::failed(
                    "signin.failed_persistence_is_atomic",
                    "state changed before commit",
                )
            },
        ])
    }
}
impl Enumerate for Flows {
    fn inputs(&self, _: &State) -> Result<Vec<Input>, ModelError> {
        Ok(vec![
            Input::Enabled(false),
            Input::Enabled(true),
            Input::Foreign(false),
            Input::Foreign(true),
            Input::UserEnabled(false),
            Input::UserEnabled(true),
            Input::Issue { commit: false },
            Input::Issue { commit: true },
            Input::Complete { commit: false },
            Input::Complete { commit: true },
        ])
    }
}
#[test]
fn issuance_and_completion_exhaust_disabled_foreign_and_retry_interleavings() {
    let r = stateless::explore::enumerate(
        &Flows,
        stateless::explore::SearchConfig {
            max_states: 500,
            max_transitions: 10000,
            max_depth: 12,
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
        "sign-in flows: {} states / {} transitions",
        r.states, r.transitions
    );
}

// Separate protocol/effect model covers OIDC Test, Link, Login approval/session,
// Reauth and email registration receipt replay. Protocol failure after durable
// reservation is terminal, and replay never emits another completion effect.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct OutcomeState {
    phase: u8,
    effects: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum OutcomeInput {
    Reserve { commit: bool },
    Finish { identity: u8, commit: bool },
    Failure { denied: bool, commit: bool },
    Replay { same_browser: bool },
}
struct Outcomes {
    case: u8,
}
impl Outcomes {
    fn transaction(&self, s: &OutcomeState) -> signin::Transaction {
        let mut t = seed();
        t.purpose = match self.case {
            0 => signin::Purpose::Login,
            1 => signin::Purpose::Link,
            2 => signin::Purpose::Test,
            3 => signin::Purpose::Reauth,
            _ => signin::Purpose::Register,
        };
        t.phase = match s.phase {
            0 => signin::Phase::Pending,
            1 => signin::Phase::Exchanging,
            2 => signin::Phase::Consumed,
            _ => signin::Phase::Failed,
        };
        t.actor_revision = None;
        if matches!(self.case, 1 | 3) {
            t.user_id = Some("owner".into());
            t.user_revision = Some(2);
        }
        if self.case == 4 {
            t.registration = Some(playscale_core::registration::Intent {
                policy: playscale_core::registration::Policy::ApprovalRequired,
                policy_revision: 1,
                invitation: None,
            });
        }
        t
    }
    fn expected(&self, identity: u8) -> Option<&'static str> {
        match self.case {
            0 => Some(if identity == 0 {
                "pending_approval"
            } else {
                "session:identity"
            }),
            1 if identity != 2 => Some("link:identity"),
            2 => Some("test:provider"),
            3 if identity == 1 => Some("session:identity"),
            4 => Some("registration:policy"),
            _ => None,
        }
    }
}
impl Model for Outcomes {
    type State = OutcomeState;
    type Input = OutcomeInput;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: format!("motion.signin_protocol_effects.{}", self.case),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: concat!(
                include_str!("../src/signin_flows.rs"),
                include_str!("../src/signin.rs")
            )
            .into(),
        }
    }
    fn initial_state(&self) -> Result<OutcomeState, ModelError> {
        Ok(OutcomeState {
            phase: 0,
            effects: 0,
        })
    }
    fn step(
        &self,
        s: &OutcomeState,
        i: &OutcomeInput,
    ) -> Result<Transition<OutcomeState, String>, ModelError> {
        let mut n = s.clone();
        let mut out = vec![];
        let tx = self.transaction(s);
        let current = signin::Current {
            browser_hash: "browser",
            epoch: "e",
            server_epoch: "s",
            provider_revision: 4,
            method_enabled: true,
            restore_hold: false,
            user_id: tx.user_id.as_deref(),
            user_revision: tx.user_revision,
            user_enabled: true,
            identity_revision: None,
            identity_enabled: true,
            identity_owner: None,
            actor_revision: None,
            now: 1,
        };
        match i {
            OutcomeInput::Reserve { commit } => {
                if signin::reserve(&tx, &current).is_ok() && *commit {
                    n.phase = 1;
                }
            }
            OutcomeInput::Finish { identity, commit } => {
                if let Ok(consumed) = signin::complete(&tx, &current) {
                    let identified = match identity {
                        0 => None,
                        1 => Some(("identity", "owner")),
                        _ => Some(("identity", "foreign")),
                    };
                    if let Ok(effect) = flow::completion(
                        if self.case == 4 {
                            flow::Protocol::Email
                        } else {
                            flow::Protocol::Oidc
                        },
                        consumed.purpose,
                        consumed.user_id.as_deref(),
                        identified,
                    ) {
                        let effect = match effect {
                            flow::Completion::Test => Some("test:provider".into()),
                            flow::Completion::Link => {
                                if signin::link_owner(identified.map(|i| i.1), "owner").is_ok() {
                                    Some("link:identity".into())
                                } else {
                                    None
                                }
                            }
                            flow::Completion::Session { identity } => {
                                Some(format!("session:{identity}"))
                            }
                            flow::Completion::PendingApproval => Some("pending_approval".into()),
                            flow::Completion::Registration => Some("registration:policy".into()),
                        };
                        if let Some(effect) = effect
                            && *commit
                        {
                            n.phase = 2;
                            n.effects += 1;
                            out.push(effect);
                        }
                    }
                }
            }
            OutcomeInput::Failure { denied, commit } if s.phase == 1 => {
                let (failed, result) = flow::protocol_failure(tx, *denied);
                if *commit {
                    assert_eq!(failed.phase, signin::Phase::Failed);
                    n.phase = 3;
                    out.push(result.into());
                }
            }
            OutcomeInput::Replay { same_browser }
                if flow::consumed_replay(
                    &tx,
                    Some(if *same_browser { "browser" } else { "other" }),
                ) =>
            {
                out.push("receipt:no-session".into());
            }
            _ => {}
        }
        Ok(Transition::accepted(n, out))
    }
    fn check_state(&self, s: &OutcomeState) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            if s.effects <= 1 && (s.phase == 2) == (s.effects == 1) {
                Check::passed("protocol.single_completion_effect")
            } else {
                Check::failed("protocol.single_completion_effect", "duplicate completion")
            },
        ])
    }
    fn check_transition(
        &self,
        s: &OutcomeState,
        i: &OutcomeInput,
        n: &TransitionRef<'_, OutcomeState, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected: Vec<String> = match i {
            OutcomeInput::Finish {
                identity,
                commit: true,
            } if s.phase == 1 => self
                .expected(*identity)
                .map(str::to_owned)
                .into_iter()
                .collect(),
            OutcomeInput::Failure {
                denied,
                commit: true,
            } if s.phase == 1 => vec![if *denied { "denied" } else { "failed" }.into()],
            OutcomeInput::Replay { same_browser: true } if s.phase == 2 && self.case == 4 => {
                vec!["receipt:no-session".into()]
            }
            _ => vec![],
        };
        let atomic = !matches!(
            i,
            OutcomeInput::Reserve { commit: false }
                | OutcomeInput::Finish { commit: false, .. }
                | OutcomeInput::Failure { commit: false, .. }
        ) || n.state == s;
        Ok(vec![
            if n.outputs == expected {
                Check::passed("protocol.exact_effect_kind_identity_and_replay")
            } else {
                Check::failed(
                    "protocol.exact_effect_kind_identity_and_replay",
                    format!("expected {expected:?}, got {:?}", n.outputs),
                )
            },
            if atomic {
                Check::passed("protocol.failed_commit_keeps_reservation")
            } else {
                Check::failed("protocol.failed_commit_keeps_reservation", "partial commit")
            },
        ])
    }
}
impl Enumerate for Outcomes {
    fn inputs(&self, _: &OutcomeState) -> Result<Vec<OutcomeInput>, ModelError> {
        let mut v = vec![
            OutcomeInput::Reserve { commit: false },
            OutcomeInput::Reserve { commit: true },
            OutcomeInput::Replay {
                same_browser: false,
            },
            OutcomeInput::Replay { same_browser: true },
        ];
        for commit in [false, true] {
            for identity in 0..3 {
                v.push(OutcomeInput::Finish { identity, commit });
            }
            for denied in [false, true] {
                v.push(OutcomeInput::Failure { denied, commit });
            }
        }
        Ok(v)
    }
}
#[test]
fn oidc_effects_failure_and_registration_replay_are_single_use() {
    for case in 0..5 {
        let r = stateless::explore::enumerate(
            &Outcomes { case },
            stateless::explore::SearchConfig {
                max_states: 20,
                max_transitions: 500,
                max_depth: 6,
            },
        )
        .unwrap();
        assert!(r.failure.is_none(), "case {case}: {:?}", r.failure);
        assert_eq!(r.skipped_checks, 0);
        assert_eq!(
            r.termination,
            stateless::explore::SearchTermination::GraphExhausted
        );
        println!(
            "protocol case={case}: {} states / {} transitions",
            r.states, r.transitions
        );
    }
}
