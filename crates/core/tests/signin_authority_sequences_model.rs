//! Sequential configuration evidence and action-proof ownership models.
//! Config exploration bounds durable revisions to four updates (enough for
//! save→test→enable→edit→retest); all inputs within that domain are exhausted.
use playscale_core::signin as core;
use serde::{Deserialize, Serialize};
use stateless::{Check, Enumerate, Model, ModelError, ModelMetadata, Transition, TransitionRef};
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct MethodState {
    revision: u8,
    enabled: bool,
    tested: bool,
    browser: bool,
    held: bool,
    family: bool,
    transaction: bool,
    mail: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum MethodInput {
    ConfigurationTest,
    BrowserTest,
    Hold(bool),
    Issue,
    Write {
        changed: bool,
        enable: bool,
        override_test: bool,
        commit: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MethodOutput {
    accepted: bool,
    update: bool,
    preserve: bool,
    revoke: bool,
    invalidate: bool,
    erase: bool,
    cancel_mail: bool,
}
struct Method {
    email: bool,
}
impl Model for Method {
    type State = MethodState;
    type Input = MethodInput;
    type Output = MethodOutput;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: format!("motion.signin_method_sequence.{}", self.email),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/signin.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<MethodState, ModelError> {
        Ok(MethodState {
            revision: 0,
            enabled: false,
            tested: false,
            browser: false,
            held: false,
            family: false,
            transaction: false,
            mail: false,
        })
    }
    fn step(
        &self,
        s: &MethodState,
        i: &MethodInput,
    ) -> Result<Transition<MethodState, MethodOutput>, ModelError> {
        let mut n = s.clone();
        let mut out = vec![];
        match i {
            MethodInput::ConfigurationTest => n.tested = true,
            MethodInput::BrowserTest => n.browser = true,
            MethodInput::Hold(v) => n.held = *v,
            MethodInput::Issue if s.enabled && !s.held => {
                n.family = true;
                n.transaction = true;
                n.mail = self.email;
            }
            MethodInput::Write {
                changed,
                enable,
                override_test,
                commit,
            } if s.revision < 4 => {
                let result = core::change_method(&core::MethodChange {
                    kind: if self.email {
                        core::MethodKind::Email
                    } else {
                        core::MethodKind::Provider
                    },
                    enabled: s.enabled,
                    requested_enabled: *enable,
                    configuration_changed: *changed,
                    configuration_tested: s.tested,
                    browser_tested: s.browser,
                    restore_hold: s.held,
                    apply_untested: *override_test,
                });
                match result {
                    Ok(e) => {
                        out.push(MethodOutput {
                            accepted: true,
                            update: e.update,
                            preserve: e.preserve_independent_method,
                            revoke: e.revoke_families,
                            invalidate: e.invalidate_transactions,
                            erase: e.erase_test_evidence,
                            cancel_mail: e.cancel_all_queued_mail,
                        });
                        if *commit && e.update {
                            n.revision += 1;
                            n.enabled = *enable;
                            if e.erase_test_evidence {
                                n.tested = false;
                                n.browser = false;
                            }
                            if e.revoke_families {
                                n.family = false;
                            }
                            if e.invalidate_transactions {
                                n.transaction = false;
                            }
                            if e.cancel_all_queued_mail {
                                n.mail = false;
                            }
                        }
                    }
                    Err(_) => out.push(MethodOutput {
                        accepted: false,
                        update: false,
                        preserve: false,
                        revoke: false,
                        invalidate: false,
                        erase: false,
                        cancel_mail: false,
                    }),
                }
            }
            _ => {}
        }
        Ok(Transition::accepted(n, out))
    }
    fn check_state(&self, s: &MethodState) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.revision <= 4 {
            Check::passed("method.bounded_revision")
        } else {
            Check::failed("method.bounded_revision", "revision overflow")
        }])
    }
    fn check_transition(
        &self,
        s: &MethodState,
        i: &MethodInput,
        n: &TransitionRef<'_, MethodState, MethodOutput>,
    ) -> Result<Vec<Check>, ModelError> {
        let MethodInput::Write {
            changed,
            enable,
            override_test,
            commit,
        } = i
        else {
            return Ok(vec![]);
        };
        if s.revision >= 4 {
            return Ok(vec![]);
        }
        let override_allowed = self.email && s.enabled && *changed && *override_test && !s.held;
        let accepted = !*enable
            || override_allowed
            || (!*changed && s.tested && !s.held && (self.email || s.browser));
        let update = accepted && (*changed || s.enabled != *enable);
        let o = &n.outputs[0];
        let effects = o.accepted == accepted
            && o.update == update
            && o.preserve == (update && s.enabled && !*enable)
            && o.revoke == update
            && o.invalidate == update
            && o.erase == (update && *changed)
            && o.cancel_mail == (update && self.email);
        let atomic = if !accepted || !*commit || !update {
            n.state == s
        } else {
            n.state.revision == s.revision + 1
                && n.state.enabled == *enable
                && !n.state.family
                && !n.state.transaction
                && (!self.email || !n.state.mail)
                && (!*changed || (!n.state.tested && !n.state.browser))
        };
        Ok(vec![
            if effects {
                Check::passed("method.exact_invalidation_effects")
            } else {
                Check::failed(
                    "method.exact_invalidation_effects",
                    "wrong dependent effects",
                )
            },
            if atomic {
                Check::passed("method.revision_evidence_and_commit_sequence")
            } else {
                Check::failed(
                    "method.revision_evidence_and_commit_sequence",
                    "stale evidence or partial commit",
                )
            },
        ])
    }
}
impl Enumerate for Method {
    fn inputs(&self, _: &MethodState) -> Result<Vec<MethodInput>, ModelError> {
        let mut v = vec![
            MethodInput::ConfigurationTest,
            MethodInput::BrowserTest,
            MethodInput::Hold(false),
            MethodInput::Hold(true),
            MethodInput::Issue,
        ];
        for bits in 0..16 {
            v.push(MethodInput::Write {
                changed: bits & 1 != 0,
                enable: bits & 2 != 0,
                override_test: bits & 4 != 0,
                commit: bits & 8 != 0,
            });
        }
        Ok(v)
    }
}
#[test]
fn configuration_revisions_tests_override_and_invalidation_sequences() {
    for email in [false, true] {
        let r = stateless::explore::enumerate(
            &Method { email },
            stateless::explore::SearchConfig {
                max_states: 3000,
                max_transitions: 100000,
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
            "method email={email}: {} states / {} transitions",
            r.states, r.transitions
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct ProofState {
    approved: bool,
    admin: bool,
    consumed: bool,
    rotated: bool,
    actor_revoked: bool,
    approver_revoked: bool,
    foreign: bool,
    expired: bool,
    epoch_changed: bool,
    uses: u8,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ProofInput {
    Approve {
        admin: bool,
        independent: bool,
        code: bool,
        commit: bool,
    },
    Rotate,
    RevokeActor,
    RevokeApprover,
    ChangeOwner,
    Expire,
    Restore,
    Consume {
        action_matches: bool,
        commit: bool,
    },
}
struct Proofs;
fn proof(s: &ProofState) -> core::Proof<'static> {
    core::Proof {
        actor: "actor",
        action: "identity.link",
        target: "email",
        epoch: "epoch",
        expires_at: 10,
        approved_by: s.approved.then_some("approver"),
        consumed: s.consumed,
    }
}
impl Model for Proofs {
    type State = ProofState;
    type Input = ProofInput;
    type Output = String;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "motion.signin_action_proof_sequence".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/signin.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<ProofState, ModelError> {
        Ok(ProofState {
            approved: false,
            admin: false,
            consumed: false,
            rotated: false,
            actor_revoked: false,
            approver_revoked: false,
            foreign: false,
            expired: false,
            epoch_changed: false,
            uses: 0,
        })
    }
    fn step(
        &self,
        s: &ProofState,
        i: &ProofInput,
    ) -> Result<Transition<ProofState, String>, ModelError> {
        let mut n = s.clone();
        let mut out = vec![];
        let epoch = if s.epoch_changed { "new" } else { "epoch" };
        let now = if s.expired { 10 } else { 1 };
        match i {
            ProofInput::Rotate => n.rotated = true,
            ProofInput::RevokeActor => n.actor_revoked = true,
            ProofInput::RevokeApprover => n.approver_revoked = true,
            ProofInput::ChangeOwner => n.foreign = true,
            ProofInput::Expire => n.expired = true,
            ProofInput::Restore => n.epoch_changed = true,
            ProofInput::Approve {
                admin,
                independent,
                code,
                commit,
            } => {
                if !s.approver_revoked
                    && !s.actor_revoked
                    && !s.rotated
                    && core::proof_pending(&proof(s), epoch, now)
                    && core::can_approve_proof(
                        "actor",
                        if *independent { "approver" } else { "actor" },
                        *admin,
                        !s.foreign,
                    )
                    && *code
                    && *commit
                {
                    n.approved = true;
                    n.admin = *admin;
                    out.push("approve:proof".into());
                }
            }
            ProofInput::Consume {
                action_matches,
                commit,
            } => {
                let authority = core::ProofAuthority {
                    recorded_actor_credential: Some("credential-0"),
                    current_actor_credential: Some(if s.rotated {
                        "credential-1"
                    } else {
                        "credential-0"
                    }),
                    actor_current: !s.actor_revoked,
                    approver_current: !s.approver_revoked,
                    approver_admin: s.admin,
                    actor_owner: Some(if s.foreign { "other" } else { "owner" }),
                    approver_owner: Some("owner"),
                };
                if core::proof_valid(
                    &proof(s),
                    "actor",
                    if *action_matches {
                        "identity.link"
                    } else {
                        "provider.test"
                    },
                    "email",
                    epoch,
                    now,
                ) && core::proof_authority_valid(&authority)
                    && *commit
                {
                    n.consumed = true;
                    n.uses += 1;
                    out.push("consume:proof:identity.link:email".into());
                }
            }
        }
        Ok(Transition::accepted(n, out))
    }
    fn check_state(&self, s: &ProofState) -> Result<Vec<Check>, ModelError> {
        Ok(vec![if s.uses <= 1 && s.consumed == (s.uses == 1) {
            Check::passed("proof.exactly_once_consumption")
        } else {
            Check::failed(
                "proof.exactly_once_consumption",
                "duplicate or unrecorded use",
            )
        }])
    }
    fn check_transition(
        &self,
        s: &ProofState,
        i: &ProofInput,
        n: &TransitionRef<'_, ProofState, String>,
    ) -> Result<Vec<Check>, ModelError> {
        let expected = matches!(
            i,
            ProofInput::Consume {
                action_matches: true,
                commit: true
            }
        ) && s.approved
            && !s.consumed
            && !s.rotated
            && !s.actor_revoked
            && !s.approver_revoked
            && !s.expired
            && !s.epoch_changed
            && (s.admin || !s.foreign);
        let expected_approval = match i {
            ProofInput::Approve {
                admin,
                independent,
                code,
                commit,
            } => {
                *independent
                    && *code
                    && *commit
                    && !s.approved
                    && !s.consumed
                    && !s.approver_revoked
                    && !s.actor_revoked
                    && !s.rotated
                    && !s.expired
                    && !s.epoch_changed
                    && (*admin || !s.foreign)
            }
            _ => false,
        };
        let approval_exact = n.outputs.iter().any(|o| o == "approve:proof") == expected_approval;
        let actual = n
            .outputs
            .iter()
            .any(|o| o == "consume:proof:identity.link:email");
        let atomic = !matches!(
            i,
            ProofInput::Consume { commit: false, .. } | ProofInput::Approve { commit: false, .. }
        ) || n.state == s;
        Ok(vec![
            if actual == expected && approval_exact {
                Check::passed("proof.current_actor_approver_owner_and_binding")
            } else {
                Check::failed(
                    "proof.current_actor_approver_owner_and_binding",
                    "stale authority consumed proof",
                )
            },
            if atomic {
                Check::passed("proof.failed_commit_preserves_state")
            } else {
                Check::failed("proof.failed_commit_preserves_state", "partial effect")
            },
        ])
    }
}
impl Enumerate for Proofs {
    fn inputs(&self, _: &ProofState) -> Result<Vec<ProofInput>, ModelError> {
        let mut v = vec![
            ProofInput::Rotate,
            ProofInput::RevokeActor,
            ProofInput::RevokeApprover,
            ProofInput::ChangeOwner,
            ProofInput::Expire,
            ProofInput::Restore,
        ];
        for bits in 0..16 {
            v.push(ProofInput::Approve {
                admin: bits & 1 != 0,
                independent: bits & 2 != 0,
                code: bits & 4 != 0,
                commit: bits & 8 != 0,
            })
        }
        for bits in 0..4 {
            v.push(ProofInput::Consume {
                action_matches: bits & 1 != 0,
                commit: bits & 2 != 0,
            })
        }
        Ok(v)
    }
}
#[test]
fn approved_proof_reobserves_rotation_revocation_ownership_and_restore() {
    let r = stateless::explore::enumerate(
        &Proofs,
        stateless::explore::SearchConfig {
            max_states: 2000,
            max_transitions: 100000,
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
        "action proof: {} states / {} transitions",
        r.states, r.transitions
    );
}
