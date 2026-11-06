use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};
fn check(name: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(name)
    } else {
        Check::failed(name, "catalog lifecycle invariant")
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}
macro_rules! codec {
    ($model:ty, $state:ty, $input:ty, $output:ty) => {
        impl ModelCodec for $model {
            fn encode_state(&self, v: &$state) -> Result<Vec<u8>, ModelError> {
                encode(v)
            }
            fn decode_state(&self, v: &[u8]) -> Result<$state, ModelError> {
                decode(v)
            }
            fn encode_input(&self, v: &$input) -> Result<Vec<u8>, ModelError> {
                encode(v)
            }
            fn decode_input(&self, v: &[u8]) -> Result<$input, ModelError> {
                decode(v)
            }
            fn encode_output(&self, v: &$output) -> Result<Vec<u8>, ModelError> {
                encode(v)
            }
        }
    };
}
mod source {
    use super::*;
    use playscale_core::sources::*;
    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct State {
        registered_identity: Option<bool>,
        observed_identity: bool,
        enabled: bool,
        members: bool,
        active: bool,
        files: bool,
        history: bool,
        excluded: bool,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Input {
        Register,
        Membership,
        Start,
        Finish,
        Fail,
        StorageChanged,
        ScopeChange,
        Remove,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Effect {
        Registered,
        Reused,
        RelocationRequired,
        Busy,
        Deleted,
        Disabled,
        ScopeChanged,
        Observed,
    }
    struct Source;
    impl Model for Source {
        type State = State;
        type Input = Input;
        type Output = Effect;
        fn metadata(&self) -> ModelMetadata {
            ModelMetadata {
                name: "catalog.source_lifecycle".into(),
                model_version: 1,
                properties_version: 1,
                codec_version: 1,
                build: include_str!("../src/sources.rs").into(),
            }
        }
        fn initial_state(&self) -> Result<State, ModelError> {
            Ok(State {
                registered_identity: None,
                observed_identity: false,
                enabled: false,
                members: false,
                active: false,
                files: false,
                history: false,
                excluded: false,
            })
        }
        fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
            let mut next = s.clone();
            let effect = match input {
                Input::Register => {
                    let identity = |value| if value { "volume-b" } else { "volume-a" };
                    match registration(
                        identity(s.observed_identity),
                        OwnershipRequirement::ExistingKind,
                        s.registered_identity.map(|value| ExistingRoot {
                            id: "source",
                            identity: identity(value),
                            managed: false,
                        }),
                    ) {
                        Registration::Create => {
                            next.registered_identity = Some(s.observed_identity);
                            next.enabled = true;
                            Effect::Registered
                        }
                        Registration::Reuse { id } => {
                            assert_eq!(id, "source");
                            Effect::Reused
                        }
                        Registration::RelocationRequired => Effect::RelocationRequired,
                        Registration::OwnershipConflict => {
                            unreachable!("existing-kind registration")
                        }
                    }
                }
                Input::Membership => {
                    next.members = !s.members;
                    Effect::Observed
                }
                Input::Start => {
                    next.active = true;
                    next.history = true;
                    Effect::Observed
                }
                Input::Finish => {
                    next.active = false;
                    next.files = true;
                    Effect::Observed
                }
                Input::Fail => {
                    next.active = false;
                    Effect::Observed
                }
                Input::StorageChanged => {
                    next.observed_identity = !s.observed_identity;
                    Effect::Observed
                }
                Input::ScopeChange => {
                    if source_scope_mutable(s.active) {
                        next.excluded = !s.excluded;
                        Effect::ScopeChanged
                    } else {
                        Effect::Busy
                    }
                }
                Input::Remove => match source_removal(s.members, s.active, s.files, s.history) {
                    Removal::Busy => Effect::Busy,
                    Removal::Delete => {
                        next.registered_identity = None;
                        next.enabled = false;
                        next.excluded = false;
                        Effect::Deleted
                    }
                    Removal::DisableAndWithdraw => {
                        next.enabled = false;
                        Effect::Disabled
                    }
                },
            };
            Ok(Transition::accepted(next, vec![effect]))
        }
        fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
            Ok(vec![
                check(
                    "retained_facts_keep_registration",
                    !(s.files || s.history || s.members || s.active)
                        || s.registered_identity.is_some(),
                ),
                check("active_requires_enabled", !s.active || s.enabled),
            ])
        }
        fn check_transition(
            &self,
            before: &State,
            _: &Input,
            next: &TransitionRef<'_, State, Effect>,
        ) -> Result<Vec<Check>, ModelError> {
            Ok(vec![
                check(
                    "refusal_and_reuse_are_noops",
                    !matches!(
                        next.outputs,
                        [Effect::Busy | Effect::Reused | Effect::RelocationRequired]
                    ) || next.state == before,
                ),
                check(
                    "deletion_never_discards_content_history_or_work",
                    !matches!(next.outputs, [Effect::Deleted])
                        || !(before.members || before.active || before.files || before.history),
                ),
                check(
                    "withdraw_retains_identity_and_history",
                    !matches!(next.outputs, [Effect::Disabled])
                        || (next.state.registered_identity == before.registered_identity
                            && next.state.files == before.files
                            && next.state.history == before.history
                            && !next.state.enabled),
                ),
                check(
                    "storage_change_does_not_rebind_registration",
                    next.state.registered_identity == before.registered_identity
                        || matches!(next.outputs, [Effect::Registered | Effect::Deleted]),
                ),
            ])
        }
    }
    impl Enumerate for Source {
        fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
            let mut out = vec![Input::Register, Input::StorageChanged];
            if s.registered_identity.is_some() {
                out.extend([Input::Membership, Input::ScopeChange, Input::Remove]);
            }
            if s.enabled && !s.active {
                out.push(Input::Start);
            }
            if s.active {
                out.extend([Input::Finish, Input::Fail]);
            }
            Ok(out)
        }
    }
    codec!(Source, State, Input, Effect);
    #[test]
    fn exhaustive_source_registration_work_removal_recovery_graph() {
        let report = stateless::explore::enumerate(&Source, Default::default()).unwrap();
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(report.skipped_checks, 0);
        assert_eq!(
            report.termination,
            stateless::explore::SearchTermination::GraphExhausted
        );
        println!(
            "source lifecycle: {} states / {} transitions; one root, two physical identities, membership/work/history/file/scope observations",
            report.states, report.transitions
        );
    }
}

mod matching_history {
    use super::*;
    use playscale_core::matching::*;
    #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
    struct State {
        revision: bool,
        history: Vec<Proposal>,
        receipts: Vec<Proposal>,
        steps: u8,
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Input {
        Propose,
        RevisionChanged,
        Reject(usize),
        Reopen(usize),
    }
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Effect {
        Observed,
        Proposal {
            updates: Vec<Proposal>,
            selected: Proposal,
            insert: bool,
        },
        Rejected,
        Decided(Proposal),
        Reopened {
            receipt: Proposal,
            next: Proposal,
        },
    }
    struct History;
    fn revision(value: bool) -> &'static str {
        if value { "r2" } else { "r1" }
    }
    impl Model for History {
        type State = State;
        type Input = Input;
        type Output = Effect;
        fn metadata(&self) -> ModelMetadata {
            ModelMetadata {
                name: "catalog.matching_history".into(),
                model_version: 1,
                properties_version: 1,
                codec_version: 1,
                build: include_str!("../src/matching.rs").into(),
            }
        }
        fn initial_state(&self) -> Result<State, ModelError> {
            Ok(State {
                revision: false,
                history: vec![],
                receipts: vec![],
                steps: 0,
            })
        }
        fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
            let mut next = s.clone();
            next.steps += 1;
            let effect = match input {
                Input::RevisionChanged => {
                    next.revision = !s.revision;
                    Effect::Observed
                }
                Input::Propose => {
                    let (updates, selected, insert) = reconcile_history(
                        &s.history,
                        "file",
                        revision(s.revision),
                        vec![],
                        format!("p{}", s.history.len()),
                    )
                    .unwrap();
                    for update in &updates {
                        *next.history.iter_mut().find(|p| p.id == update.id).unwrap() =
                            update.clone();
                    }
                    if insert {
                        next.history.push(selected.clone());
                    }
                    Effect::Proposal {
                        updates,
                        selected,
                        insert,
                    }
                }
                Input::Reject(index) => {
                    let proposal = &s.history[*index];
                    match decide(
                        proposal,
                        proposal.revision,
                        Some(revision(s.revision)),
                        Decision::Reject,
                    ) {
                        Ok((proposal, identification)) => {
                            assert!(identification.is_none());
                            next.history[*index] = proposal.clone();
                            Effect::Decided(proposal)
                        }
                        Err(_) => Effect::Rejected,
                    }
                }
                Input::Reopen(index) => {
                    let proposal = &s.history[*index];
                    let open: Vec<_> = s
                        .history
                        .iter()
                        .filter(|p| proposal_open(p.status))
                        .map(|p| p.id.clone())
                        .collect();
                    match reopen_with_open(
                        proposal,
                        proposal.revision,
                        Some(revision(s.revision)),
                        &open,
                    ) {
                        Ok(reopened) => {
                            next.receipts.push(proposal.clone());
                            next.history[*index] = reopened.clone();
                            Effect::Reopened {
                                receipt: proposal.clone(),
                                next: reopened,
                            }
                        }
                        Err(_) => Effect::Rejected,
                    }
                }
            };
            Ok(Transition::accepted(next, vec![effect]))
        }
        fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
            Ok(vec![
                check(
                    "at_most_one_open_proposal",
                    s.history.iter().filter(|p| proposal_open(p.status)).count() <= 1,
                ),
                check(
                    "receipts_preserve_decisions",
                    s.receipts.iter().all(|p| {
                        p.status == Status::Rejected && p.decision == Some(Decision::Reject)
                    }),
                ),
            ])
        }
        fn check_transition(
            &self,
            before: &State,
            _: &Input,
            after: &TransitionRef<'_, State, Effect>,
        ) -> Result<Vec<Check>, ModelError> {
            let after_state = after.state;
            let mut checks = vec![
                check(
                    "prior_receipts_never_change",
                    after_state.receipts.starts_with(&before.receipts),
                ),
                check(
                    "rejections_do_not_mutate_history",
                    !matches!(after.outputs, [Effect::Rejected])
                        || after_state.history == before.history,
                ),
            ];
            if let [
                Effect::Proposal {
                    updates,
                    selected,
                    insert,
                },
            ] = after.outputs
            {
                checks.push(check(
                    "refresh_never_rewrites_decision",
                    updates.iter().all(|p| {
                        before
                            .history
                            .iter()
                            .find(|old| old.id == p.id)
                            .is_some_and(|old| {
                                !matches!(old.status, Status::Accepted | Status::Rejected)
                            })
                    }),
                ));
                checks.push(check(
                    "exact_proposal_identity_and_count",
                    after_state.history.iter().any(|p| p == selected)
                        && after_state.history.len() == before.history.len() + usize::from(*insert),
                ));
            }
            if let [Effect::Reopened { receipt, next }] = after.outputs {
                checks.push(check(
                    "exact_reopen_receipt",
                    before.history.iter().any(|p| p == receipt)
                        && after_state.receipts.last() == Some(receipt)
                        && after_state.receipts.len() == before.receipts.len() + 1
                        && next.id == receipt.id
                        && next.revision == receipt.revision + 1
                        && next.decision.is_none(),
                ));
                checks.push(check(
                    "competing_open_prevents_reopen",
                    !before.history.iter().any(|p| proposal_open(p.status)),
                ));
            }
            Ok(checks)
        }
    }
    impl Enumerate for History {
        fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
            if s.steps == 7 {
                return Ok(vec![]);
            }
            let mut out = vec![Input::RevisionChanged];
            let (_, _, insert) = reconcile_history(
                &s.history,
                "file",
                revision(s.revision),
                vec![],
                format!("p{}", s.history.len()),
            )
            .unwrap();
            if !insert || s.history.len() < 2 {
                out.push(Input::Propose);
            }
            for i in 0..s.history.len() {
                out.extend([Input::Reject(i), Input::Reopen(i)]);
            }
            Ok(out)
        }
    }
    codec!(History, State, Input, Effect);
    #[test]
    fn bounded_two_proposal_history_and_reopen_graph() {
        let report = stateless::explore::enumerate(&History, Default::default()).unwrap();
        assert!(report.failure.is_none(), "{:?}", report.failure);
        assert_eq!(report.skipped_checks, 0);
        assert_eq!(
            report.termination,
            stateless::explore::SearchTermination::GraphExhausted
        );
        println!(
            "matching history: {} states / {} transitions; two proposals, two file revisions, <=7 actions, immutable reopen receipts",
            report.states, report.transitions
        );
    }
}
