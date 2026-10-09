//! Stateless model of the identification workflow over the production
//! `matching` decisions, plus independent filename fixtures.
use playscale_core::matching::{
    Candidate, Decision, MatchError, MatchState, Parsed, Proposal, Status, decide, normalize_title,
    parse_name, propose, provider_identity_allowed, refresh,
};
use serde::{Deserialize, Serialize};
use stateless::{
    Check, Enumerate, Model, ModelCodec, ModelError, ModelMetadata, Transition, TransitionRef,
};

type Identity = (String, String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct State {
    proposal: Proposal,
    file_revision: String,
    match_state: MatchState,
    identity: Option<Identity>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Input {
    /// Re-run provider search returning candidate set `0`, `1` or both.
    Refresh(u8),
    Decide {
        stale: bool,
        decision: Decision,
    },
    /// The file is replaced in place.
    ReplaceFile,
    /// An automatic provider contribution asserts an identity.
    ProviderUpdate(u8),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Effect {
    Identified(Identity),
    ProviderApplied(Identity),
    ProviderConflict(Identity),
    Rejected(MatchError),
    Updated,
}

fn candidate(n: u8) -> Candidate {
    Candidate {
        id: format!("c{n}"),
        item_id: None,
        title: format!("Title {n}"),
        external_id: Some(identity(n)),
        reason_codes: vec!["title_year".into()],
    }
}
fn identity(n: u8) -> Identity {
    ("tmdb:movie".into(), n.to_string())
}
fn candidates(set: u8) -> Vec<Candidate> {
    match set {
        0 => vec![candidate(0)],
        1 => vec![candidate(1)],
        _ => vec![candidate(0), candidate(1)],
    }
}

struct Matching;
impl Model for Matching {
    type State = State;
    type Input = Input;
    type Output = Effect;
    fn metadata(&self) -> ModelMetadata {
        ModelMetadata {
            name: "playscale.matching".into(),
            model_version: 1,
            properties_version: 1,
            codec_version: 1,
            build: include_str!("../src/matching.rs").into(),
        }
    }
    fn initial_state(&self) -> Result<State, ModelError> {
        Ok(State {
            proposal: propose("p".into(), "f".into(), "r0".into(), candidates(2))
                .map_err(|e| ModelError::new(format!("{e:?}")))?,
            file_revision: "r0".into(),
            match_state: MatchState::Unmatched,
            identity: None,
        })
    }
    fn step(&self, s: &State, input: &Input) -> Result<Transition<State, Effect>, ModelError> {
        let mut next = s.clone();
        let effect = match input {
            Input::Refresh(set) => {
                match refresh(&s.proposal, Some(&s.file_revision), candidates(*set)) {
                    Ok(p) => {
                        next.proposal = p;
                        Effect::Updated
                    }
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::Decide { stale, decision } => {
                let expected = s.proposal.revision - u64::from(*stale);
                match decide(
                    &s.proposal,
                    expected,
                    Some(&s.file_revision),
                    decision.clone(),
                ) {
                    Ok((p, identification)) => {
                        next.proposal = p;
                        match identification.and_then(|i| i.candidate.external_id) {
                            Some(id) => {
                                next.identity = Some(id.clone());
                                next.match_state = MatchState::Manual;
                                Effect::Identified(id)
                            }
                            None => Effect::Updated,
                        }
                    }
                    Err(e) => Effect::Rejected(e),
                }
            }
            Input::ReplaceFile => {
                next.file_revision = format!("{}'", s.file_revision);
                // The adapter refreshes open proposals when a file changes.
                if let Ok(p) = refresh(&s.proposal, Some(&next.file_revision), vec![]) {
                    next.proposal = p;
                }
                Effect::Updated
            }
            Input::ProviderUpdate(n) => {
                let incoming = identity(*n);
                if provider_identity_allowed(s.match_state, s.identity.as_ref(), &incoming) {
                    next.identity = Some(incoming.clone());
                    if s.match_state != MatchState::Manual {
                        next.match_state = MatchState::Matched;
                    }
                    Effect::ProviderApplied(incoming)
                } else {
                    Effect::ProviderConflict(incoming)
                }
            }
        };
        Ok(Transition::accepted(next, vec![effect]))
    }
    fn check_state(&self, s: &State) -> Result<Vec<Check>, ModelError> {
        Ok(vec![
            check(
                "match.manual_has_identity",
                s.match_state != MatchState::Manual || s.identity.is_some(),
            ),
            check(
                "match.ambiguity_requires_review",
                !matches!(s.proposal.status, Status::Pending) || s.proposal.candidates.len() <= 1,
            ),
        ])
    }
    fn check_transition(
        &self,
        before: &State,
        input: &Input,
        next: &TransitionRef<'_, State, Effect>,
    ) -> Result<Vec<Check>, ModelError> {
        let after = next.state;
        let decided_before = matches!(before.proposal.status, Status::Accepted | Status::Rejected);
        let identified = matches!(next.outputs, [Effect::Identified(_)]);
        let valid_decision = match input {
            Input::Decide { stale, decision } => {
                !stale
                    && !decided_before
                    && before.proposal.status != Status::Stale
                    && match decision {
                        Decision::Accept { candidate_id } => before
                            .proposal
                            .candidates
                            .iter()
                            .any(|c| &c.id == candidate_id),
                        _ => true,
                    }
            }
            _ => false,
        };
        Ok(vec![
            check(
                "match.fix_match_survives_refresh",
                !decided_before || after.proposal == before.proposal,
            ),
            check(
                "match.decision_exactly_on_current_review",
                matches!(input, Input::Decide { .. })
                    .then(|| valid_decision != matches!(next.outputs, [Effect::Rejected(_)]))
                    .unwrap_or(true),
            ),
            check(
                "match.identification_exactly_once_per_accept",
                identified
                    == (valid_decision
                        && matches!(
                            input,
                            Input::Decide {
                                decision: Decision::Accept { .. },
                                ..
                            }
                        )),
            ),
            check(
                "match.manual_identity_pinned",
                before.match_state != MatchState::Manual
                    || identified
                    || after.identity == before.identity,
            ),
            check(
                "match.file_change_stales_open_proposal",
                !matches!(input, Input::ReplaceFile)
                    || decided_before
                    || after.proposal.status == Status::Stale,
            ),
            check(
                "match.revision_monotonic",
                after.proposal.revision >= before.proposal.revision
                    && (after.proposal == before.proposal
                        || after.proposal.revision == before.proposal.revision + 1),
            ),
        ])
    }
}
fn check(id: &'static str, ok: bool) -> Check {
    if ok {
        Check::passed(id)
    } else {
        Check::failed(id, "matching invariant violated")
    }
}
impl Enumerate for Matching {
    fn inputs(&self, s: &State) -> Result<Vec<Input>, ModelError> {
        let mut inputs = Vec::new();
        // Proposal revisions are bounded at 4 so the graph is finite.
        let open = s.proposal.revision < 4;
        for set in (0..3).filter(|_| open) {
            inputs.push(Input::Refresh(set));
        }
        for stale in [false, true].into_iter().filter(|_| open) {
            for decision in [
                Decision::Accept {
                    candidate_id: "c0".into(),
                },
                Decision::Accept {
                    candidate_id: "c1".into(),
                },
                Decision::Reject,
                Decision::Defer,
            ] {
                inputs.push(Input::Decide { stale, decision });
            }
        }
        if s.file_revision.len() < 4 {
            inputs.push(Input::ReplaceFile);
        }
        for n in 0..3 {
            inputs.push(Input::ProviderUpdate(n));
        }
        Ok(inputs)
    }
}
impl stateless::Generate for Matching {
    fn generate(&self, s: &State, rng: &mut stateless::Rng) -> Result<Option<Input>, ModelError> {
        let inputs = self.inputs(s)?;
        Ok(rng.index(inputs.len()).map(|i| inputs[i].clone()))
    }
}
impl ModelCodec for Matching {
    fn encode_state(&self, v: &State) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_state(&self, b: &[u8]) -> Result<State, ModelError> {
        decode(b)
    }
    fn encode_input(&self, v: &Input) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
    fn decode_input(&self, b: &[u8]) -> Result<Input, ModelError> {
        decode(b)
    }
    fn encode_output(&self, v: &Effect) -> Result<Vec<u8>, ModelError> {
        encode(v)
    }
}
fn encode<T: Serialize>(v: &T) -> Result<Vec<u8>, ModelError> {
    serde_json::to_vec(v).map_err(|e| ModelError::new(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(v: &[u8]) -> Result<T, ModelError> {
    serde_json::from_slice(v).map_err(|e| ModelError::new(e.to_string()))
}

#[test]
fn bounded_matching_graph_checks_production_decisions() {
    let report = stateless::explore::enumerate(&Matching, Default::default()).unwrap();
    assert!(report.failure.is_none(), "{:?}", report.failure);
    assert_eq!(report.skipped_checks, 0);
    assert_eq!(
        report.termination,
        stateless::explore::SearchTermination::GraphExhausted
    );
    println!(
        "Stateless: {} states, {} edges; 2 candidates, proposal revision <=4, <=3 file replacements, stale/current decisions, provider refresh",
        report.states, report.transitions
    );
}

#[test]
fn fix_match_survives_provider_refresh() {
    let mut state = Matching.initial_state().unwrap();
    for input in [
        Input::ProviderUpdate(0),
        Input::Decide {
            stale: false,
            decision: Decision::Accept {
                candidate_id: "c1".into(),
            },
        },
        Input::Refresh(0),
        Input::ProviderUpdate(0),
    ] {
        state = Matching.step(&state, &input).unwrap().state;
    }
    assert_eq!(state.identity, Some(identity(1)));
    assert_eq!(state.match_state, MatchState::Manual);
    assert_eq!(state.proposal.status, Status::Accepted);
    assert_eq!(state.proposal.candidates, candidates(2));
}

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
    normalize: Vec<(String, String)>,
}
#[derive(Deserialize)]
struct Case {
    stem: String,
    expected: Parsed,
}
#[test]
fn filename_golden_fixtures() {
    let f: Fixtures = serde_json::from_str(include_str!(
        "../../../qualification/catalog-reference/filename_golden.json"
    ))
    .unwrap();
    for case in f.cases {
        assert_eq!(parse_name(&case.stem), case.expected, "{}", case.stem);
    }
    for (input, expected) in f.normalize {
        assert_eq!(normalize_title(&input), expected, "{input}");
    }
}
