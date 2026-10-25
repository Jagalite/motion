use super::*;
use serde_json::json;

fn hash() -> String {
    "a".repeat(64)
}
fn head(id: &str) -> Head {
    Head {
        id: id.into(),
        revision: 1,
    }
}
fn year(source: &str, value: Option<i64>) -> ReleaseYearEvidence {
    ReleaseYearEvidence {
        source: source.into(),
        revision: 1,
        document_digest: hash(),
        release_year: value,
    }
}
fn work(id: &str, title: &str) -> LocalWork {
    LocalWork {
        work: head(id),
        state: WorkState::Live,
        title: title.into(),
        metadata_digest: hash(),
        years_complete: Completeness::Complete,
        years: vec![],
    }
}
fn observation() -> Observation {
    let exact = ExactFile {
        id: "native-file".into(),
        revision: "exact-v1".into(),
    };
    Observation {
        contract_version: CONTRACT_VERSION,
        fence: Fence {
            identity: CatalogIdentity {
                catalog_id: "catalog".into(),
                incarnation: "incarnation".into(),
                profile: "default".into(),
            },
            generation: 10,
        },
        file: FileObservation {
            file_id: "file".into(),
            file_revision: "wire-v1".into(),
            original_native: exact.clone(),
            observed_native: Some(exact),
            relative_path: "movies/Movie.2020.1080p.mkv".into(),
            origin: FileOrigin::Original,
            presence: Presence::Present,
            source: SourceEvidence {
                source_id: "source".into(),
                mapping_digest: hash(),
                binding_digest: hash(),
                policy_digest: hash(),
                occurrence_digest: hash(),
            },
            ownership_complete: Completeness::Complete,
            owner_paths: vec![OwnerPath {
                work: head("own-work"),
                work_state: WorkState::Live,
                context: head("context"),
                timeline: head("timeline"),
                version: head("version"),
                association: head("association"),
                binding: head("binding"),
                work_closure_digest: hash(),
                context_closure_digest: hash(),
            }],
            memberships_complete: Completeness::Complete,
            file_libraries: vec![FileLibrary {
                library: head("library"),
                definition_digest: hash(),
                scope_digest: hash(),
            }],
        },
        corpus: CandidateCorpus {
            completeness: Completeness::Complete,
            selection_digest: hash(),
            works: vec![work("target", "Movie")],
        },
    }
}
fn request(observed: &Observation, current: Option<&Workflow>) -> CreateRequest {
    CreateRequest {
        new_proposal_id: if current.is_none() {
            "proposal"
        } else {
            "next-proposal"
        }
        .into(),
        expected: current.map(workflow_head),
        file_id: observed.file.file_id.clone(),
        file_revision: observed.file.file_revision.clone(),
        library_id: "library".into(),
    }
}
fn create(observed: &Observation) -> Workflow {
    plan_create(None, observed, &CatalogScope::All, &request(observed, None))
        .unwrap()
        .result
}
fn decision(current: &Workflow, choice: Decision) -> DecisionRequest {
    DecisionRequest {
        expected: workflow_head(current),
        decision: choice,
    }
}
fn decision_observation(observed: &Observation) -> DecisionObservation {
    DecisionObservation {
        contract_version: observed.contract_version,
        fence: observed.fence.clone(),
        file: observed.file.clone(),
    }
}
fn decide(current: &Workflow, observed: &Observation, choice: Decision) -> Result<WorkflowPlan> {
    plan_decide(
        current,
        &decision_observation(observed),
        &CatalogScope::All,
        &decision(current, choice),
    )
}
fn refresh(current: &Workflow, observed: &Observation) -> Result<WorkflowPlan> {
    plan_create(
        Some(current),
        observed,
        &CatalogScope::All,
        &request(observed, Some(current)),
    )
}

#[test]
fn fresh_proposal_is_local_evidence_not_acceptance() {
    let observed = observation();
    let plan = plan_create(
        None,
        &observed,
        &CatalogScope::All,
        &request(&observed, None),
    )
    .unwrap();
    assert_eq!(plan.writes, vec![plan.result.clone()]);
    assert_eq!(plan.result.proposal.status, Status::Pending);
    assert_eq!(plan.result.proposal.revision, 1);
    assert_eq!(plan.result.origin, plan.result.reviewed);
    assert_eq!(plan.result.proposal.candidates[0].id, "local:target");
    assert_eq!(plan.result.proposal.candidates[0].external_id, None);
    assert_eq!(
        decide(
            &plan.result,
            &observed,
            Decision::Accept {
                candidate_id: "local:target".into()
            }
        ),
        Err(Refusal::AcceptUnavailable)
    );
}

#[test]
fn title_and_year_match_legacy_rules_and_preserve_display_text() {
    let mut observed = observation();
    observed.file.relative_path = "Movie.Name.2020.1080p.mkv".into();
    observed.corpus.works = vec![
        work("a", "Movie Name"),
        work("b", "Movie Name"),
        work("c", "Movie Name"),
        work("d", "Different"),
    ];
    observed.corpus.works[0].years = vec![year("local", Some(2020))];
    observed.corpus.works[1].years = vec![year("local", Some(2019))];
    let state = create(&observed);
    assert_eq!(state.proposal.status, Status::Review);
    assert_eq!(
        state
            .proposal
            .candidates
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["local:a", "local:c"]
    );
    assert_eq!(state.proposal.candidates[0].title, "Movie Name");
    assert_eq!(state.proposal.candidates[0].reason_codes, ["title", "year"]);
    assert_eq!(state.proposal.candidates[1].reason_codes, ["title"]);
}

#[test]
fn release_year_uses_local_then_source_order_not_resolved_precedence() {
    let mut observed = observation();
    observed.corpus.works[0].years = vec![
        year("aaa", Some(1999)),
        year("local", Some(2020)),
        year("zzz", Some(2021)),
    ];
    let state = create(&observed);
    assert_eq!(
        state.reviewed.candidates[0]
            .selected_year
            .as_ref()
            .unwrap()
            .source,
        "local"
    );
    observed.corpus.works[0].years[1].release_year = None;
    observed.corpus.works[0].years[2].release_year = Some(2020);
    assert!(
        create(&observed).proposal.candidates.is_empty(),
        "aaa's mismatching year wins; zzz must not replace it"
    );
    observed.corpus.works[0].years[0].release_year = Some(2020);
    assert_eq!(
        create(&observed).reviewed.candidates[0]
            .selected_year
            .as_ref()
            .unwrap()
            .source,
        "aaa"
    );
}

#[test]
fn own_work_is_excluded_and_corpus_order_is_explicit() {
    let mut observed = observation();
    observed.corpus.works.insert(0, work("own-work", "Movie"));
    assert_eq!(create(&observed).proposal.candidates.len(), 1);
    observed.corpus.works.swap(0, 1);
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::InvalidObservation)
    );
}

#[test]
fn fifty_candidates_fit_but_fifty_one_are_never_truncated() {
    let mut observed = observation();
    observed.corpus.works = (0..50)
        .map(|i| work(&format!("candidate-{i:03}"), "Movie"))
        .collect();
    assert_eq!(create(&observed).proposal.candidates.len(), 50);
    observed.corpus.works.push(work("candidate-050", "Movie"));
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::CandidateOverflow)
    );
    assert_eq!(MAX_CORPUS_ROWS, 10_000);
}

#[test]
fn missing_parsed_title_yields_empty_candidates() {
    let mut observed = observation();
    observed.file.relative_path = "1080p.mkv".into();
    let state = create(&observed);
    assert!(state.proposal.candidates.is_empty());
    assert_eq!(state.proposal.status, Status::Pending);
}

#[test]
fn unrelated_generation_and_nonmatching_corpus_changes_are_noops() {
    let mut observed = observation();
    observed.corpus.works.insert(0, work("other", "Unrelated"));
    let state = create(&observed);
    observed.fence.generation += 1;
    observed.corpus.selection_digest = "b".repeat(64);
    observed.corpus.works[0].work.revision += 1;
    observed.corpus.works[0].title = "Still unrelated".into();
    observed.corpus.works[0].metadata_digest = "c".repeat(64);
    let plan = refresh(&state, &observed).unwrap();
    assert!(plan.writes.is_empty());
    assert_eq!(plan.result, state);
    assert_eq!(plan.fence.generation, observed.fence.generation);
}

#[test]
fn changed_review_heads_advance_only_open_review_and_keep_origin() {
    let mut observed = observation();
    let state = create(&observed);
    observed.file.owner_paths[0].context.revision += 1;
    observed.file.owner_paths[0].context_closure_digest = "b".repeat(64);
    observed.fence.generation += 1;
    assert!(decide(&state, &observed, Decision::Reject).is_ok());
    let next = refresh(&state, &observed).unwrap().result;
    assert_eq!(next.proposal.revision, state.proposal.revision + 1);
    assert_eq!(next.origin, state.origin);
    assert_ne!(next.reviewed.digest, state.reviewed.digest);
    let old = decision(&state, Decision::Reject);
    assert_eq!(
        plan_decide(
            &next,
            &decision_observation(&observed),
            &CatalogScope::All,
            &old
        ),
        Err(Refusal::StaleProposal)
    );
}

#[test]
fn deferred_status_and_decision_survive_candidate_refresh() {
    let mut observed = observation();
    let state = create(&observed);
    let deferred = decide(&state, &observed, Decision::Defer).unwrap().result;
    observed.corpus.works.push(work("target-2", "Movie"));
    let next = refresh(&deferred, &observed).unwrap().result;
    assert_eq!(next.proposal.status, Status::Deferred);
    assert_eq!(next.proposal.decision, Some(Decision::Defer));
    assert_eq!(next.proposal.candidates.len(), 2);
    assert_eq!(next.proposal.revision, 3);
    assert_eq!(next.origin, state.origin);
}

#[test]
fn rejected_and_preexisting_accepted_proposals_are_preserved() {
    let mut observed = observation();
    let state = create(&observed);
    let rejected = decide(&state, &observed, Decision::Reject).unwrap().result;
    let mut accepted = state;
    accepted.proposal.revision += 1;
    accepted.proposal.status = Status::Accepted;
    accepted.proposal.decision = Some(Decision::Accept {
        candidate_id: "local:target".into(),
    });
    observed.file.owner_paths[0].context.revision += 1;
    observed.corpus.works.push(work("target-2", "Movie"));
    for decided in [rejected, accepted] {
        let plan = refresh(&decided, &observed).unwrap();
        assert_eq!(plan.result, decided);
        assert!(plan.writes.is_empty());
    }
}

#[test]
fn replacement_revision_stales_open_proposal_before_raising_new() {
    let mut observed = observation();
    let state = create(&observed);
    observed.file.file_revision = "wire-v2".into();
    observed.file.original_native.revision = "exact-v2".into();
    observed.file.observed_native = Some(observed.file.original_native.clone());
    observed.fence.generation += 1;
    let plan = refresh(&state, &observed).unwrap();
    assert_eq!(plan.writes.len(), 2);
    assert_eq!(plan.writes[0].proposal.id, state.proposal.id);
    assert_eq!(plan.writes[0].proposal.status, Status::Stale);
    assert_eq!(plan.writes[0].origin, state.origin);
    assert_eq!(plan.writes[1].proposal.id, "next-proposal");
    assert_eq!(plan.result.proposal.revision, 1);
    let mut recycled = request(&observed, Some(&state));
    recycled.new_proposal_id = state.proposal.id.clone();
    assert_eq!(
        plan_create(Some(&state), &observed, &CatalogScope::All, &recycled),
        Err(Refusal::InvalidInput)
    );
}

#[test]
fn pending_intent_fences_refresh_decision_and_changed_file_recovery() {
    let mut observed = observation();
    let mut state = create(&observed);
    state.pending = Some(PendingIntent {
        operation_id: "pending-command".into(),
        envelope_digest: hash(),
        proposal_revision: state.proposal.revision,
        evidence_digest: state.reviewed.digest.clone(),
        decision: Decision::Accept {
            candidate_id: "local:target".into(),
        },
    });
    let saved = state.clone();
    assert_eq!(refresh(&state, &observed), Err(Refusal::RecoveryRequired));
    for choice in [
        Decision::Reject,
        Decision::Defer,
        Decision::Accept {
            candidate_id: "local:target".into(),
        },
    ] {
        assert_eq!(
            decide(&state, &observed, choice),
            Err(Refusal::RecoveryRequired)
        );
    }
    observed.file.file_revision = "wire-v2".into();
    observed.file.original_native.revision = "exact-v2".into();
    observed.file.observed_native = Some(observed.file.original_native.clone());
    assert_eq!(refresh(&state, &observed), Err(Refusal::RecoveryRequired));
    assert_eq!(
        state, saved,
        "no planner manufactures a recovery acknowledgment"
    );
    state.pending.as_mut().unwrap().proposal_revision += 1;
    assert_eq!(refresh(&state, &observed), Err(Refusal::InvalidObservation));
}

#[test]
fn proposal_and_file_fences_are_independent() {
    let mut observed = observation();
    let state = create(&observed);
    let mut stale = decision(&state, Decision::Reject);
    stale.expected.revision += 1;
    assert_eq!(
        plan_decide(
            &state,
            &decision_observation(&observed),
            &CatalogScope::All,
            &stale
        ),
        Err(Refusal::StaleProposal)
    );
    let mut create_request = request(&observed, Some(&state));
    create_request.expected = None;
    assert_eq!(
        plan_create(Some(&state), &observed, &CatalogScope::All, &create_request),
        Err(Refusal::StaleProposal)
    );
    observed.file.file_revision = "different-wire".into();
    assert_eq!(
        decide(&state, &observed, Decision::Reject),
        Err(Refusal::FileChanged)
    );
}

#[test]
fn same_wire_revision_cannot_be_rebound_to_different_native_content() {
    let mut observed = observation();
    let state = create(&observed);
    observed.file.original_native.revision = "different-native-content".into();
    observed.file.observed_native = Some(observed.file.original_native.clone());
    assert_eq!(refresh(&state, &observed), Err(Refusal::FileChanged));
    assert_eq!(
        decide(&state, &observed, Decision::Reject),
        Err(Refusal::FileChanged)
    );
}

#[test]
fn file_curation_requires_all_file_libraries_not_all_work_libraries() {
    let mut observed = observation();
    observed.file.file_libraries.push(FileLibrary {
        library: head("shared"),
        definition_digest: hash(),
        scope_digest: hash(),
    });
    let only_one = CatalogScope::Libraries(["library".into()].into());
    assert_eq!(
        plan_create(None, &observed, &only_one, &request(&observed, None)),
        Err(Refusal::OutsideFileScope)
    );
    let both = CatalogScope::Libraries(["library".into(), "shared".into()].into());
    let plan = plan_create(None, &observed, &both, &request(&observed, None)).unwrap();
    // Work closure may contain other versions in other libraries. Its digest
    // is evidence, not an expansion of file-level curation permission.
    assert!(
        plan_decide(
            &plan.result,
            &decision_observation(&observed),
            &both,
            &decision(&plan.result, Decision::Reject)
        )
        .is_ok()
    );
    assert_eq!(
        plan_decide(
            &plan.result,
            &decision_observation(&observed),
            &only_one,
            &decision(&plan.result, Decision::Reject)
        ),
        Err(Refusal::OutsideFileScope)
    );
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::Nothing,
            &request(&observed, None)
        ),
        Err(Refusal::OutsideFileScope)
    );
}

#[test]
fn incomplete_observations_never_mean_empty_complete_evidence() {
    for part in 0..4 {
        let mut observed = observation();
        match part {
            0 => observed.file.ownership_complete = Completeness::Incomplete,
            1 => observed.file.memberships_complete = Completeness::Incomplete,
            2 => observed.corpus.completeness = Completeness::Incomplete,
            _ => observed.corpus.works[0].years_complete = Completeness::Incomplete,
        }
        assert_eq!(
            plan_create(
                None,
                &observed,
                &CatalogScope::All,
                &request(&observed, None)
            ),
            Err(Refusal::IncompleteObservation)
        );
    }
}

#[test]
fn ambiguous_generated_missing_and_retired_observations_refuse() {
    for count in [0, 2] {
        let mut observed = observation();
        observed.file.owner_paths = vec![observed.file.owner_paths[0].clone(); count];
        assert_eq!(
            plan_create(
                None,
                &observed,
                &CatalogScope::All,
                &request(&observed, None)
            ),
            Err(Refusal::AmbiguousOwnership)
        );
    }
    for state in [Presence::Missing, Presence::Unknown] {
        let mut observed = observation();
        observed.file.presence = state;
        assert_eq!(
            plan_create(
                None,
                &observed,
                &CatalogScope::All,
                &request(&observed, None)
            ),
            Err(Refusal::FileUnavailable)
        );
    }
    let mut observed = observation();
    observed.file.origin = FileOrigin::Generated;
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::GeneratedFile)
    );
    let mut observed = observation();
    observed.file.owner_paths[0].work_state = WorkState::Retired;
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::RetiredWork)
    );
    let mut observed = observation();
    observed.file.observed_native = None;
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::FileChanged)
    );
}

#[test]
fn identity_regression_bad_digests_and_duplicate_year_sources_refuse() {
    let original = observation();
    let state = create(&original);
    for kind in 0..4 {
        let mut observed = original.clone();
        match kind {
            0 => observed.fence.identity.catalog_id.push('x'),
            1 => observed.fence.identity.incarnation.push('x'),
            2 => observed.fence.identity.profile.push('x'),
            _ => observed.fence.generation -= 1,
        }
        assert_eq!(refresh(&state, &observed), Err(Refusal::StaleObservation));
    }
    let mut observed = original.clone();
    observed.file.source.occurrence_digest = "A".repeat(64);
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::InvalidObservation)
    );
    let mut observed = original;
    observed.corpus.works[0].years = vec![year("local", Some(2020)), year("local", Some(2020))];
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::InvalidObservation)
    );
}

#[test]
fn immutable_evidence_digest_detects_changed_retained_snapshot() {
    let observed = observation();
    let mut state = create(&observed);
    state.origin.file.source.binding_digest = "b".repeat(64);
    assert_eq!(refresh(&state, &observed), Err(Refusal::InvalidObservation));
}

#[test]
fn reject_and_defer_need_no_candidate_corpus_or_unchanged_work_heads() {
    let observed = observation();
    let state = create(&observed);
    let mut current = decision_observation(&observed);
    current.fence.generation += 1;
    current.file.owner_paths[0].work.revision += 1;
    current.file.owner_paths[0].context.revision += 1;
    current.file.owner_paths[0].work_closure_digest = "b".repeat(64);
    // No corpus exists in this API, even if current candidate discovery would
    // be incomplete, huge, or find that the old candidate title/year changed.
    for choice in [Decision::Reject, Decision::Defer] {
        let plan = plan_decide(
            &state,
            &current,
            &CatalogScope::All,
            &decision(&state, choice),
        )
        .unwrap();
        assert_eq!(plan.result.proposal.candidates, state.proposal.candidates);
        assert_eq!(plan.result.reviewed, state.reviewed);
        assert_eq!(plan.result.origin, state.origin);
        assert_eq!(plan.result.proposal.revision, state.proposal.revision + 1);
    }
    // A different CURRENT owner path can also authorize rejecting evidence;
    // this produces no identification or ownership change in either work.
    current.file.owner_paths[0].work = head("new-current-owner");
    assert!(
        plan_decide(
            &state,
            &current,
            &CatalogScope::All,
            &decision(&state, Decision::Reject)
        )
        .is_ok()
    );
}

#[test]
fn persisted_decision_records_its_generation_without_rewriting_evidence() {
    let observed = observation();
    let state = create(&observed);
    let mut current = decision_observation(&observed);
    current.fence.generation += 10;
    let deferred = plan_decide(
        &state,
        &current,
        &CatalogScope::All,
        &decision(&state, Decision::Defer),
    )
    .unwrap()
    .result;
    assert_eq!(deferred.reviewed_generation, current.fence.generation);
    assert_eq!(deferred.reviewed, state.reviewed);
    current.fence.generation -= 1;
    assert_eq!(
        plan_decide(
            &deferred,
            &current,
            &CatalogScope::All,
            &decision(&deferred, Decision::Reject),
        ),
        Err(Refusal::StaleObservation)
    );
}

#[test]
fn rehashed_persisted_candidate_corruption_is_still_refused() {
    let mut observed = observation();
    observed.corpus.works[0].years = vec![year("local", Some(2020))];
    let state = create(&observed);
    for malformed in 0..9 {
        let mut bad = state.clone();
        let candidate = &mut bad.reviewed.candidates[0];
        match malformed {
            0 => candidate.selected_year.as_mut().unwrap().source.clear(),
            1 => candidate.selected_year.as_mut().unwrap().revision = 0,
            2 => candidate.selected_year.as_mut().unwrap().document_digest = "A".repeat(64),
            3 => candidate.selected_year.as_mut().unwrap().release_year = None,
            4 => candidate.selected_year.as_mut().unwrap().release_year = Some(1999),
            5 => candidate.candidate.title = "Unrelated title".into(),
            6 => candidate.candidate.reason_codes = vec!["guessed".into()],
            7 => candidate.candidate.external_id = Some(("provider".into(), "42".into())),
            _ => candidate.candidate.id = "different-candidate".into(),
        }
        bad.proposal.candidates = bad
            .reviewed
            .candidates
            .iter()
            .map(|c| c.candidate.clone())
            .collect();
        bad.reviewed.digest = evidence_digest(&bad.reviewed).unwrap();
        assert!(
            decide(&bad, &observed, Decision::Reject).is_err(),
            "malformed persisted case {malformed}"
        );
    }
}

#[test]
fn malformed_persisted_status_revision_and_pending_candidate_refuse() {
    let observed = observation();
    let state = create(&observed);
    let mut bad = state.clone();
    bad.proposal.status = Status::Review;
    assert_eq!(
        decide(&bad, &observed, Decision::Reject),
        Err(Refusal::InvalidObservation)
    );
    for revision in [0, i64::MAX as u64 + 1] {
        let mut bad = state.clone();
        bad.proposal.revision = revision;
        assert_eq!(
            decide(&bad, &observed, Decision::Reject),
            Err(Refusal::InvalidObservation)
        );
    }
    let mut exhausted = state.clone();
    exhausted.proposal.revision = i64::MAX as u64;
    assert_eq!(
        decide(&exhausted, &observed, Decision::Reject),
        Err(Refusal::Matching(MatchError::RevisionExhausted))
    );
    let mut bad = state;
    bad.pending = Some(PendingIntent {
        operation_id: "pending".into(),
        envelope_digest: hash(),
        proposal_revision: bad.proposal.revision,
        evidence_digest: bad.reviewed.digest.clone(),
        decision: Decision::Accept {
            candidate_id: "not-reviewed".into(),
        },
    });
    assert_eq!(
        decide(&bad, &observed, Decision::Reject),
        Err(Refusal::InvalidObservation)
    );
}

#[test]
fn stale_deferred_history_is_valid_but_never_redecided() {
    let observed = observation();
    let state = create(&observed);
    let deferred = decide(&state, &observed, Decision::Defer).unwrap().result;
    let mut stale = deferred;
    stale.proposal =
        matching::refresh(&stale.proposal, None, stale.proposal.candidates.clone()).unwrap();
    assert_eq!(stale.proposal.status, Status::Stale);
    assert_eq!(stale.proposal.decision, Some(Decision::Defer));
    assert_eq!(
        decide(&stale, &observed, Decision::Reject),
        Err(Refusal::Matching(MatchError::StaleProposal))
    );
}

#[test]
fn exact_byte_node_depth_and_work_limits_are_independent() {
    assert_ne!(
        Budget::default().charge(&"x".repeat(MAX_BYTES - 2)),
        Err(Refusal::ByteBudget)
    );
    assert_eq!(
        Budget::default().charge(&"x".repeat(MAX_BYTES - 1)),
        Err(Refusal::ByteBudget)
    );
    assert_ne!(
        Budget::default().charge(&vec![0u8; MAX_NODES - 1]),
        Err(Refusal::NodeBudget)
    );
    assert_eq!(
        Budget::default().charge(&vec![0u8; MAX_NODES]),
        Err(Refusal::NodeBudget)
    );
    let mut nested = json!(null);
    for _ in 0..MAX_DEPTH {
        nested = json!([nested]);
    }
    assert!(Budget::default().charge(&nested).is_ok());
    assert_eq!(
        Budget::default().charge(&json!([nested])),
        Err(Refusal::DepthBudget)
    );
    let mut budget = Budget::default();
    assert_eq!(budget.work(MAX_WORK), Ok(()));
    assert_eq!(budget.work(1), Err(Refusal::WorkBudget));
}

#[test]
fn planner_precharges_large_input_and_bounds_candidate_computation() {
    let mut observed = observation();
    observed.file.relative_path = "x".repeat(MAX_BYTES);
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::ByteBudget)
    );
    let mut observed = observation();
    observed.corpus.works = (0..100)
        .map(|i| work(&format!("candidate-{i:03}"), &"x".repeat(100)))
        .collect();
    assert_eq!(
        plan_create(
            None,
            &observed,
            &CatalogScope::All,
            &request(&observed, None)
        ),
        Err(Refusal::WorkBudget)
    );
}

#[test]
fn serde_contract_has_no_missing_original_file_or_unknown_outer_fields() {
    let observed = observation();
    let state = create(&observed);
    assert_eq!(
        serde_json::from_value::<Workflow>(serde_json::to_value(&state).unwrap()).unwrap(),
        state
    );
    let mut raw = serde_json::to_value(&observed).unwrap();
    raw["file"]
        .as_object_mut()
        .unwrap()
        .remove("original_native");
    assert!(serde_json::from_value::<Observation>(raw).is_err());
    let mut raw = serde_json::to_value(&observed).unwrap();
    raw["grant"] = json!(true);
    assert!(serde_json::from_value::<Observation>(raw).is_err());
}
