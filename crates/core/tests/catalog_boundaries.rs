use playscale_core::identity::*;
use std::collections::BTreeSet;

fn split_fixture(shared_revision: bool) -> Aggregate {
    let version = |id: &str, revision: &str| Version {
        id: id.into(),
        origin: Origin::Original,
        equivalence: Equivalence::Declared,
        bindings: vec![Binding {
            file_id: id.into(),
            revision: revision.into(),
            part: 1,
            start_ms: None,
            end_ms: None,
        }],
    };
    Aggregate {
        work: Work {
            id: "work".into(),
            revision: 1,
            external_ids: BTreeSet::new(),
        },
        editions: vec![Edition {
            id: "edition".into(),
            label: "".into(),
            timelines: vec![Timeline {
                id: "timeline".into(),
                versions: vec![
                    version("move", "a"),
                    version("stay", if shared_revision { "a" } else { "b" }),
                ],
            }],
        }],
    }
}
#[test]
fn split_occurrences_pin_copy_ownership_and_reject_stale_inventory() {
    for shared in [false, true] {
        let current = split_fixture(shared);
        let mut n = 0;
        let plan = plan_split(
            &current,
            &SplitRequest {
                item: "work".into(),
                expected_revision: 1,
                versions: vec!["move".into()],
                new_title: "split".into(),
            },
            &mut || {
                n += 1;
                format!("new{n}")
            },
        )
        .unwrap();
        let inventory: Vec<_> = [
            ("move", "a"),
            ("stay", if shared { "a" } else { "b" }),
            ("copy", "a"),
        ]
        .into_iter()
        .map(|(id, revision)| SplitOccurrence {
            id: id.into(),
            edition: "edition".into(),
            revision: revision.into(),
        })
        .collect();
        let plan = plan_split_occurrences(&current, plan, inventory.clone());
        let moved: BTreeSet<_> = plan
            .occurrence_moves
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            moved,
            if shared {
                BTreeSet::from(["move"])
            } else {
                BTreeSet::from(["copy", "move"])
            }
        );
        assert_eq!(
            commit_split_occurrences(&plan, Some(&current), false, inventory.clone()).unwrap(),
            plan
        );
        // Content, membership and copy arrival/removal can change without a structural revision.
        for mutation in 0..4 {
            let mut observed = inventory.clone();
            match mutation {
                0 => observed[0].revision = "changed".into(),
                1 => observed[0].edition = "foreign".into(),
                2 => {
                    observed.pop();
                }
                _ => observed.push(SplitOccurrence {
                    id: "new-copy".into(),
                    edition: "edition".into(),
                    revision: "a".into(),
                }),
            }
            assert!(matches!(
                commit_split_occurrences(&plan, Some(&current), false, observed),
                Err(IdentityError::StaleRevision(_))
            ));
        }
        assert!(matches!(
            commit_split_occurrences(&plan, Some(&current), true, inventory),
            Err(IdentityError::StaleRevision(_))
        ));
    }
}

#[test]
fn relocation_requires_exact_scope_and_live_envelope() {
    use playscale_core::sources::*;
    for enabled in [false, true] {
        for bits in 0..16 {
            let files: Vec<_> = (0..4)
                .map(|i| (format!("f{i}"), format!("{i}"), bits & (1 << i) != 0))
                .collect();
            let exclusions = vec!["3".into()];
            let required: Vec<_> = files
                .iter()
                .filter(|(_, path, available)| {
                    relocation_file_required(enabled, *available, path, &exclusions)
                })
                .map(|(id, ..)| (id.clone(), "stamp".into()))
                .collect();
            let plan = RelocationPlan {
                source: "source".into(),
                expected_revision: 1,
                binding_revision: 1,
                root: "/new".into(),
                root_identity: "volume".into(),
                verified: required,
                out_of_scope: vec!["f3".into()],
                catalog: vec![],
            };
            assert!(relocation_complete(&plan, enabled, &files, &exclusions));
            for i in 0..plan.verified.len() {
                let mut incomplete = plan.clone();
                incomplete.verified.remove(i);
                assert!(!relocation_complete(
                    &incomplete,
                    enabled,
                    &files,
                    &exclusions
                ));
            }
            let mut wrong = plan.clone();
            wrong.verified.push(("f3".into(), "stamp".into()));
            assert!(!relocation_complete(&wrong, enabled, &files, &exclusions));
        }
    }
    assert_eq!(
        relocation_envelope(
            playscale_core::catalog::ReviewedAuthority {
                principal: "actor",
                history: "epoch",
                expires: 10
            },
            "source",
            "actor",
            "other",
            "epoch",
            9
        ),
        Err(RelocationEnvelopeError::ForeignSource)
    );
    assert_eq!(
        relocation_envelope(
            playscale_core::catalog::ReviewedAuthority {
                principal: "actor",
                history: "epoch",
                expires: 10
            },
            "source",
            "actor",
            "other",
            "epoch",
            10
        ),
        Err(RelocationEnvelopeError::Expired)
    );
    for (actor, source, now, valid) in [
        ("actor", "source", 9, true),
        ("actor", "source", 10, false),
        ("other", "source", 9, false),
        ("actor", "other", 9, false),
    ] {
        assert_eq!(
            relocation_envelope(
                playscale_core::catalog::ReviewedAuthority {
                    principal: "actor",
                    history: "epoch",
                    expires: 10
                },
                "source",
                actor,
                source,
                "epoch",
                now
            )
            .is_ok(),
            valid
        );
    }
}

#[test]
fn merge_contribution_target_and_source_order_are_explicit() {
    use playscale_core::catalog::*;
    let owner = |item: &str, keys: &[&str]| ContributionOwner {
        item: item.into(),
        metadata: keys.iter().map(|s| (*s).into()).collect(),
        artwork: vec![("poster".into(), "nfo".into())],
        nfo_origin: true,
    };
    for target_nfo in [false, true] {
        let target = owner("target", if target_nfo { &["nfo"] } else { &[] });
        let a = owner("a", &["nfo", "local"]);
        let b = owner("b", &["nfo", "provider"]);
        for sources in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
            let plan = merge_contributions(&target, &sources);
            assert_eq!(plan.len(), 2);
            assert_eq!(plan[0].nfo_origin, !target_nfo);
            assert!(!plan[1].nfo_origin);
            assert_eq!(
                plan.iter()
                    .flat_map(|p| &p.metadata)
                    .filter(|s| *s == "nfo")
                    .count(),
                usize::from(!target_nfo)
            );
            assert!(
                plan.iter().all(|p| p.artwork.is_empty()),
                "target artwork wins independently of metadata"
            );
            assert!(plan.iter().all(|p| p.to == "target"));
            assert_eq!(plan[0].from, sources[0].item);
        }
    }
}

#[test]
fn matching_history_preserves_decisions_and_one_open_identity() {
    use playscale_core::matching::*;
    let p = propose("old".into(), "file".into(), "r1".into(), vec![]).unwrap();
    let (decided, _) = decide(&p, 1, Some("r1"), Decision::Reject).unwrap();
    let (_, kept, fresh) = reconcile_history(
        std::slice::from_ref(&decided),
        "file",
        "r1",
        vec![],
        "unused".into(),
    )
    .unwrap();
    assert_eq!(kept, decided);
    assert!(!fresh);
    let (updates, new, fresh) = reconcile_history(
        std::slice::from_ref(&decided),
        "file",
        "r2",
        vec![],
        "new".into(),
    )
    .unwrap();
    assert!(updates.is_empty());
    assert!(fresh);
    assert_eq!(new.id, "new");
    assert_eq!(
        reopen_with_open(
            &decided,
            decided.revision,
            Some("r1"),
            std::slice::from_ref(&new.id)
        ),
        Err(MatchError::StaleProposal)
    );
    // Both two-proposal orderings and revision interleavings execute production
    // reconciliation; old decision receipt bytes never mutate.
    for history in [
        vec![decided.clone(), new.clone()],
        vec![new.clone(), decided.clone()],
    ] {
        for revision in ["r1", "r2", "r3"] {
            let (updates, selected, insert) =
                reconcile_history(&history, "file", revision, vec![], "third".into()).unwrap();
            assert!(updates.iter().all(|p| p.id != decided.id));
            let mut applied = history.clone();
            for update in updates {
                let id = update.id.clone();
                *applied.iter_mut().find(|p| p.id == id).unwrap() = update;
            }
            if insert {
                applied.push(selected);
            }
            assert!(applied.iter().filter(|p| proposal_open(p.status)).count() <= 1);
            assert_eq!(
                applied.iter().find(|p| p.id == decided.id).unwrap(),
                &decided
            );
        }
    }
}

#[test]
fn order_effects_are_exact_and_exhaustion_has_no_plan() {
    use std::collections::BTreeMap;
    let ancestors = vec!["owner".into()];
    let occupied = BTreeMap::from([(1, "timeline".into())]);
    let noop = order_placement(
        "timeline",
        7,
        &ancestors,
        "owner",
        "new",
        3,
        3,
        1,
        &occupied,
        Some(("new", 1, 3)),
    )
    .unwrap();
    assert!(!noop.changed);
    assert_eq!(noop.timeline_revision, 7);
    assert!(noop.group_revisions.is_empty());
    let moved = order_placement(
        "timeline",
        7,
        &ancestors,
        "owner",
        "new",
        3,
        3,
        2,
        &occupied,
        Some(("old", 1, 8)),
    )
    .unwrap();
    assert_eq!(moved.timeline_revision, 8);
    assert_eq!(
        moved.group_revisions,
        BTreeMap::from([("new".into(), 4), ("old".into(), 9)])
    );
    for (timeline_rev, group_rev, old_rev, expected) in [
        (i64::MAX as u64, 3, 8, 3),
        (7, i64::MAX as u64, 8, i64::MAX as u64),
        (7, 3, i64::MAX as u64, 3),
        (7, 3, 8, 2),
    ] {
        assert!(
            order_placement(
                "timeline",
                timeline_rev,
                &ancestors,
                "owner",
                "new",
                group_rev,
                expected,
                2,
                &occupied,
                Some(("old", 1, old_rev))
            )
            .is_err()
        );
    }
    assert!(
        invalidate_order("timeline", 7, "group", 3, &ancestors, "owner")
            .unwrap()
            .is_none()
    );
    let invalid = invalidate_order("timeline", 7, "group", 3, &ancestors, "foreign")
        .unwrap()
        .unwrap();
    assert_eq!(invalid.timeline_revision, 8);
    assert_eq!(invalid.group_revisions["group"], 4);
}

#[test]
fn receipt_projection_and_mutation_footprint_follow_current_owners() {
    use playscale_core::catalog::*;
    let saved = ReceiptProjection {
        libraries: vec!["visible".into(), "revoked".into()],
        parents: vec!["parent".into()],
        timeline: Some("timeline".into()),
        artwork: Some("art".into()),
    };
    for timeline in [Some("timeline".into()), Some("replacement".into()), None] {
        let visible = ReceiptProjection {
            libraries: vec!["visible".into()],
            parents: vec![],
            timeline: timeline.clone(),
            artwork: None,
        };
        let projected = project_receipt(saved.clone(), &visible);
        assert_eq!(projected.libraries, ["visible"]);
        assert!(projected.parents.is_empty());
        assert!(projected.artwork.is_none());
        assert_eq!(
            projected.timeline,
            if timeline.as_deref() == Some("timeline") {
                timeline
            } else {
                None
            }
        );
        assert_eq!(project_receipt(projected.clone(), &visible), projected);
    }
    let edges = vec![Edge {
        id: "edge".into(),
        source: "other".into(),
        target: "source".into(),
        kind: RelationshipKind::DerivedFrom,
    }];
    let footprint = merge_footprint(
        &["source".into()],
        "target",
        &edges,
        &[
            ("source".into(), "order-owner".into()),
            ("unmoved".into(), "unrelated".into()),
        ],
    );
    assert_eq!(
        footprint.owners,
        BTreeSet::from([
            "source".into(),
            "target".into(),
            "other".into(),
            "order-owner".into()
        ])
    );
    // Revocation of any affected owner must refuse the entire mutation.
    for revoked in &footprint.owners {
        let grants: BTreeSet<_> = footprint
            .owners
            .iter()
            .filter(|owner| *owner != revoked)
            .cloned()
            .collect();
        assert!(!footprint.owners.is_subset(&grants));
    }
}

#[test]
fn local_candidates_use_precedence_evidence_order_and_cap() {
    use playscale_core::matching::*;
    let parsed = parse_name("Film (2001)");
    let mut facts: Vec<_> = (0..55)
        .rev()
        .map(|i| LocalCandidateFacts {
            id: format!("{i:03}"),
            title: "Film".into(),
            years: vec![("z".into(), 1990), ("local".into(), 2001)],
        })
        .collect();
    facts.push(LocalCandidateFacts {
        id: "bad".into(),
        title: "Film".into(),
        years: vec![("local".into(), 1990), ("a".into(), 2001)],
    });
    let candidates = local_candidates(&parsed, facts);
    assert_eq!(candidates.len(), MAX_CANDIDATES);
    assert_eq!(candidates.first().unwrap().item_id.as_deref(), Some("000"));
    assert_eq!(candidates.last().unwrap().item_id.as_deref(), Some("049"));
    assert!(
        candidates
            .iter()
            .all(|c| c.reason_codes == ["title", "year"])
    );
}

#[test]
fn split_review_fences_pins_even_when_current_destination_would_be_identical() {
    let current = split_fixture(true);
    let mut n = 0;
    let plan = plan_split(
        &current,
        &SplitRequest {
            item: "work".into(),
            expected_revision: 1,
            versions: vec!["move".into()],
            new_title: "split".into(),
        },
        &mut || {
            n += 1;
            format!("new{n}")
        },
    )
    .unwrap();
    let inventory = vec![SplitOccurrence {
        id: "move".into(),
        edition: "edition".into(),
        revision: "a".into(),
    }];
    let plan = plan_split_occurrences(&current, plan, inventory.clone());
    let mut changed = current;
    changed.editions[0].timelines[0].versions[1].bindings[0].revision = "different".into();
    assert!(matches!(
        commit_split_occurrences(&plan, Some(&changed), false, inventory),
        Err(IdentityError::StaleRevision(_))
    ));
}

#[test]
fn catalog_command_preconditions_cover_stale_foreign_and_immutable_facts() {
    use playscale_core::catalog::*;
    for same_edition in [false, true] {
        for same_work in [false, true] {
            assert_eq!(
                reassignment_precondition(
                    "edition",
                    if same_edition { "edition" } else { "stale" },
                    "work",
                    if same_work { "work" } else { "foreign" }
                ),
                if !same_edition {
                    ReassignmentPrecondition::StaleEdition
                } else if !same_work {
                    ReassignmentPrecondition::ForeignWork
                } else {
                    ReassignmentPrecondition::Current
                }
            );
        }
    }
    let membership = BTreeSet::from(["a".into()]);
    for same_membership in [false, true] {
        for family in [None, Some("video"), Some("audio")] {
            let requested = if same_membership {
                membership.clone()
            } else {
                BTreeSet::new()
            };
            assert_eq!(
                item_precondition(&membership, &requested, "video", family),
                if !same_membership {
                    ItemPrecondition::DerivedMembership
                } else if family == Some("audio") {
                    ItemPrecondition::ImmutableFamily
                } else {
                    ItemPrecondition::Current
                }
            );
        }
    }
    let current = split_fixture(false);
    assert!(timeline_owner(&current, "edition").is_ok());
    assert!(timeline_owner(&current, "foreign").is_err());
    for parts in [
        vec![],
        vec![false],
        vec![true],
        vec![true, false],
        vec![true, true],
    ] {
        assert_eq!(
            generated_version(&parts),
            !parts.is_empty() && parts.iter().all(|p| *p)
        );
    }
    assert_eq!(default_timeline(&[]), None);
    assert_eq!(default_timeline(&["a".into()]), Some("a".into()));
    assert_eq!(default_timeline(&["a".into(), "b".into()]), None);
}

#[test]
fn reviewed_authority_rejects_missing_or_restored_history_without_adoption() {
    use playscale_core::catalog::*;
    for saved in ["history-a", "", "history-b"] {
        let authority = ReviewedAuthority {
            principal: "actor",
            history: saved,
            expires: 10,
        };
        assert_eq!(
            review_admission(authority, "actor", "history-a", 9).is_ok(),
            saved == "history-a"
        );
        assert_eq!(
            review_admission(authority, "actor", "history-a", 10),
            Err(ReviewAdmissionError::Expired)
        );
        assert_eq!(
            review_admission(authority, "other", "history-a", 9),
            Err(ReviewAdmissionError::ForeignActor)
        );
    }
}
