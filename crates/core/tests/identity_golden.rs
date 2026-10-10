//! Independent golden fixtures for catalog identity decisions.
use playscale_core::identity::{
    Aggregate, Attribution, Binding, Equivalence, Id, IdentityError, MergePlan, MergeRequest,
    SplitPlan, SplitRequest, Timeline, attach_version, attribute_legacy_progress,
    external_namespace, plan_merge, plan_split, validate_bindings,
};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome<T> {
    Ok(T),
    Err(IdentityError),
}
#[derive(Deserialize)]
struct MergeCase {
    name: String,
    request: MergeRequest,
    aliases: BTreeMap<Id, Id>,
    expected: Outcome<MergePlan>,
}
#[derive(Deserialize)]
struct SplitCase {
    name: String,
    item: Id,
    request: SplitRequest,
    expected: Outcome<SplitPlan>,
}
#[derive(Deserialize)]
struct ProgressCase {
    name: String,
    timelines: Vec<(Id, usize)>,
    expected: Attribution,
}
#[derive(Deserialize)]
struct Fixtures {
    works: BTreeMap<Id, Aggregate>,
    merge: Vec<MergeCase>,
    split: Vec<SplitCase>,
    legacy_progress: Vec<ProgressCase>,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!(
        "../../../qualification/catalog-reference/identity_golden.json"
    ))
    .unwrap()
}

#[test]
fn merge_fixtures() {
    let f = fixtures();
    for case in f.merge {
        let actual = plan_merge(&f.works, &case.aliases, &case.request);
        match case.expected {
            Outcome::Ok(plan) => assert_eq!(actual, Ok(plan), "{}", case.name),
            Outcome::Err(error) => assert_eq!(actual, Err(error), "{}", case.name),
        }
    }
}

#[test]
fn split_fixtures() {
    let f = fixtures();
    for case in f.split {
        let mut n = 0;
        let mut ids = || {
            n += 1;
            format!("id-{n}")
        };
        let actual = plan_split(&f.works[&case.item], &case.request, &mut ids);
        match case.expected {
            Outcome::Ok(plan) => assert_eq!(actual, Ok(plan), "{}", case.name),
            Outcome::Err(error) => assert_eq!(actual, Err(error), "{}", case.name),
        }
    }
}

#[test]
fn legacy_progress_fixtures() {
    for case in fixtures().legacy_progress {
        assert_eq!(
            attribute_legacy_progress(&case.timelines),
            case.expected,
            "{}",
            case.name
        );
    }
}

fn binding(file: &str, start: Option<u64>, end: Option<u64>) -> Binding {
    Binding {
        file_id: file.into(),
        revision: "r".into(),
        part: 1,
        start_ms: start,
        end_ms: end,
    }
}

#[test]
fn multi_episode_files_never_fabricate_boundaries() {
    // A single-timeline file may have unknown boundaries.
    assert_eq!(validate_bindings(&[binding("f", None, None)], &[]), Ok(()));
    // Sharing a file across episodes needs known, disjoint intervals everywhere.
    let first = ("e1".to_string(), binding("f", Some(0), Some(1_000)));
    assert_eq!(
        validate_bindings(&[binding("f", None, None)], std::slice::from_ref(&first)),
        Err(IdentityError::SharedFileNeedsKnownIntervals("f".into()))
    );
    assert_eq!(
        validate_bindings(
            &[binding("f", Some(1_000), Some(2_000))],
            &[("e1".into(), binding("f", None, None))]
        ),
        Err(IdentityError::SharedFileNeedsKnownIntervals("f".into()))
    );
    assert!(matches!(
        validate_bindings(
            &[binding("f", Some(999), Some(2_000))],
            std::slice::from_ref(&first)
        ),
        Err(IdentityError::InvalidBindings(_))
    ));
    assert_eq!(
        validate_bindings(&[binding("f", Some(1_000), Some(2_000))], &[first]),
        Ok(())
    );
    // Multipart: consecutive parts, distinct files, ordered intervals.
    let mut two = binding("g", None, None);
    two.part = 2;
    assert_eq!(
        validate_bindings(&[binding("f", None, None), two.clone()], &[]),
        Ok(())
    );
    two.part = 3;
    assert!(validate_bindings(&[binding("f", None, None), two], &[]).is_err());
    assert!(validate_bindings(&[binding("f", Some(5), Some(5))], &[]).is_err());
    assert!(validate_bindings(&[], &[]).is_err());
}

#[test]
fn encodes_join_a_timeline_only_with_explicit_equivalence() {
    let empty = Timeline {
        id: "t".into(),
        versions: vec![],
    };
    assert_eq!(attach_version(&empty, Equivalence::Unknown), Ok(()));
    let f = fixtures();
    let occupied = &f.works["film"].editions[0].timelines[0];
    assert_eq!(
        attach_version(occupied, Equivalence::Unknown),
        Err(IdentityError::UnknownEquivalenceRequiresNewTimeline)
    );
    assert_eq!(attach_version(occupied, Equivalence::Declared), Ok(()));
    assert_eq!(attach_version(occupied, Equivalence::Verified), Ok(()));
}

#[test]
fn external_namespaces_separate_movies_and_television() {
    assert_eq!(
        external_namespace(" TMDB ", "movie").as_deref(),
        Some("tmdb:movie")
    );
    assert_eq!(
        external_namespace("tmdb", "series").as_deref(),
        Some("tmdb:series")
    );
    assert_ne!(
        external_namespace("tmdb", "movie"),
        external_namespace("tmdb", "series")
    );
    assert_eq!(external_namespace("tmdb:movie", "movie"), None);
    assert_eq!(external_namespace("tmdb", "unclassified"), None);
    assert_eq!(external_namespace("", "movie"), None);
}

#[test]
fn availability_is_derived_from_reviewed_content() {
    use playscale_core::identity::{
        Availability, Occurrence, Origin, Version, confirm_replacement, version_availability,
    };
    let bound = Binding {
        file_id: "f".into(),
        revision: "a".into(),
        part: 1,
        start_ms: None,
        end_ms: None,
    };
    let occ = |file: &str, revision: &str, available| Occurrence {
        file_id: file.into(),
        revision: revision.into(),
        available,
    };
    let b = std::slice::from_ref(&bound);
    let reviewed = |r: &str| std::collections::BTreeMap::from([(1u32, r.to_string())]);
    assert_eq!(
        version_availability(b, &[occ("f", "a", true)]),
        Availability::Available
    );
    // The bound file is gone but a verified copy remains.
    assert_eq!(
        version_availability(b, &[occ("f", "a", false), occ("copy", "a", true)]),
        Availability::Available
    );
    assert_eq!(
        version_availability(b, &[occ("f", "a", false)]),
        Availability::Unavailable
    );
    // Replaced in place: stale until confirmed, never silently re-pinned.
    let replaced = [occ("f", "b", true)];
    assert_eq!(version_availability(b, &replaced), Availability::Stale);
    let version = Version {
        id: "v".into(),
        origin: Origin::Original,
        equivalence: Equivalence::Unknown,
        bindings: vec![bound.clone()],
    };
    assert_eq!(
        confirm_replacement(&version, 3, 2, &reviewed("b"), &replaced),
        Err(IdentityError::StaleRevision("v".into()))
    );
    // The operator reviewed "b" but the file now holds "c": rejected.
    assert_eq!(
        confirm_replacement(&version, 3, 3, &reviewed("b"), &[occ("f", "c", true)]),
        Err(IdentityError::ReviewedContentChanged("f".into()))
    );
    let (confirmed, revision) =
        confirm_replacement(&version, 3, 3, &reviewed("b"), &replaced).unwrap();
    assert_eq!(
        (confirmed.bindings[0].revision.as_str(), revision),
        ("b", 4)
    );
    assert_eq!(confirmed.equivalence, Equivalence::Declared);
    assert_eq!(
        version_availability(&confirmed.bindings, &replaced),
        Availability::Available
    );
    assert!(confirm_replacement(&version, 3, 3, &reviewed("a"), &[occ("f", "a", true)]).is_err());
}

#[test]
fn splits_reassignments_and_renditions_respect_reviewed_content() {
    use playscale_core::identity::{
        PinnedSource, ReassignFacts, Reassignment, RenditionPlacement, Version, reassignment,
        rendition_placement,
    };
    // Two episodes bind disjoint intervals of one file; splitting one away
    // would put the file in two works.
    let part = |file: &str, start, end| Binding {
        file_id: file.into(),
        revision: "r".into(),
        part: 1,
        start_ms: Some(start),
        end_ms: Some(end),
    };
    let version = |id: &str, b: Binding| Version {
        id: id.into(),
        origin: playscale_core::identity::Origin::Original,
        equivalence: Equivalence::Declared,
        bindings: vec![b],
    };
    let mut f = fixtures();
    let film = f.works.get_mut("film").unwrap();
    film.editions[0].timelines.push(Timeline {
        id: "ep2".into(),
        versions: vec![version("ep2-v", part("shared", 1000, 2000))],
    });
    film.editions[0].timelines[0].versions[0] = version("bluray-1080", part("shared", 0, 1000));
    let mut n = 0;
    let mut ids = || {
        n += 1;
        format!("id-{n}")
    };
    assert_eq!(
        plan_split(
            &f.works["film"],
            &SplitRequest {
                item: "film".into(),
                versions: vec!["ep2-v".into()],
                new_title: "Episode 2".into(),
                expected_revision: 3,
            },
            &mut ids,
        ),
        Err(IdentityError::SharedFileSplit("shared".into()))
    );
    let facts = |bound: Option<(&str, usize)>, copy| ReassignFacts {
        bound_versions: usize::from(bound.is_some()),
        bound: bound.map(|(v, n)| (v.to_string(), n)),
        pinned_copy_remains: copy,
    };
    assert!(
        reassignment(&ReassignFacts {
            bound: Some(("v".into(), 1)),
            pinned_copy_remains: false,
            bound_versions: 2,
        })
        .is_err(),
        "shared multi-episode file"
    );
    assert_eq!(
        reassignment(&facts(None, false)),
        Ok(Reassignment::DeclareNew)
    );
    assert_eq!(
        reassignment(&facts(Some(("v", 2)), true)),
        Ok(Reassignment::RebindToCopyAndDeclareNew {
            version: "v".into()
        })
    );
    assert_eq!(
        reassignment(&facts(Some(("v", 1)), false)),
        Ok(Reassignment::MoveVersion {
            version: "v".into()
        })
    );
    assert!(reassignment(&facts(Some(("v", 2)), false)).is_err());
    let pinned = |timeline: &str, parts, interval| PinnedSource {
        timeline: timeline.into(),
        parts,
        interval,
    };
    assert_eq!(rendition_placement(&[]), RenditionPlacement::OwnTimeline);
    assert_eq!(
        rendition_placement(&[pinned("t", 1, false)]),
        RenditionPlacement::Join {
            timeline: "t".into()
        }
    );
    // One part of a multipart version, or an episode interval of a shared
    // file, does not represent the whole timeline.
    assert_eq!(
        rendition_placement(&[pinned("multi", 2, false), pinned("episode", 1, true)]),
        RenditionPlacement::OwnTimeline
    );
}

#[test]
fn explicit_versions_order_groups_and_relationships() {
    use playscale_core::identity::{
        RelationshipKind, VersionRequest, place_in_order, plan_version, validate_relationship,
    };
    let part = |file: &str, revision: &str, start: Option<u64>, end: Option<u64>| Binding {
        file_id: file.into(),
        revision: revision.into(),
        part: 1,
        start_ms: start,
        end_ms: end,
    };
    let empty = Timeline {
        id: "ep2".into(),
        versions: vec![],
    };
    let files: std::collections::BTreeMap<String, (String, String)> = [
        (
            "double".to_string(),
            ("ep1-ed".to_string(), "r".to_string()),
        ),
        ("own".to_string(), ("ep2-ed".to_string(), "r2".to_string())),
    ]
    .into();
    let request = |b: Binding| VersionRequest {
        bindings: vec![b],
        equivalence: Equivalence::Unknown,
    };
    // Episode 2 may bind the second half of episode 1's double-episode file...
    let shared = [("ep1".to_string(), part("double", "r", Some(0), Some(1_000)))];
    assert_eq!(
        plan_version(
            &empty,
            "ep2-ed",
            &request(part("double", "r", Some(1_000), Some(2_000))),
            &files,
            &shared
        ),
        Ok(())
    );
    // ...but never the whole file, nor an unknown split point.
    assert!(
        plan_version(
            &empty,
            "ep2-ed",
            &request(part("double", "r", None, None)),
            &files,
            &[]
        )
        .is_err()
    );
    assert!(
        plan_version(
            &empty,
            "ep2-ed",
            &request(part("double", "r", None, None)),
            &files,
            &shared
        )
        .is_err()
    );
    // Bindings pin the file's current reviewed revision.
    assert_eq!(
        plan_version(
            &empty,
            "ep2-ed",
            &request(part("own", "old", None, None)),
            &files,
            &[]
        ),
        Err(IdentityError::ReviewedContentChanged("own".into()))
    );
    assert_eq!(
        plan_version(
            &empty,
            "ep2-ed",
            &request(part("own", "r2", None, None)),
            &files,
            &[]
        ),
        Ok(())
    );

    let occupied: std::collections::BTreeMap<u32, String> = [(1, "ep1".to_string())].into();
    let ancestors = ["season".to_string(), "series".to_string()];
    assert!(place_in_order("ep2", &ancestors, "series", 2, &occupied).is_ok());
    assert!(place_in_order("ep2", &ancestors, "series", 1, &occupied).is_err());
    assert!(
        place_in_order("ep1", &ancestors, "series", 1, &occupied).is_ok(),
        "same place"
    );
    assert!(place_in_order("ep2", &ancestors, "other-series", 2, &occupied).is_err());

    let edges = vec![("a".to_string(), "b".to_string(), RelationshipKind::PartOf)];
    assert!(validate_relationship("b", "c", RelationshipKind::PartOf, &edges).is_ok());
    assert!(
        validate_relationship("b", "a", RelationshipKind::PartOf, &edges).is_err(),
        "cycle"
    );
    assert!(validate_relationship("b", "a", RelationshipKind::PerformedBy, &edges).is_ok());
    assert!(
        validate_relationship("a", "b", RelationshipKind::PartOf, &edges).is_err(),
        "duplicate"
    );
    assert!(validate_relationship("a", "a", RelationshipKind::CreatedBy, &edges).is_err());
}

#[test]
fn merges_remap_relationships_and_order_memberships_follow_ancestry() {
    use playscale_core::identity::{
        Edge, RelationshipKind::*, order_membership_valid, remap_relationships,
    };
    let edge = |id: &str, s: &str, t: &str, k| Edge {
        id: id.into(),
        source: s.into(),
        target: t.into(),
        kind: k,
    };
    let edges = vec![
        edge("1", "dup", "show", ExtraOf),
        edge("2", "film", "show", ExtraOf),
        edge("3", "dup", "film", CreatedBy),
        edge("4", "film", "dup", PerformedBy),
    ];
    let remap = remap_relationships(&edges, &["dup".into()], "film").unwrap();
    // dup->show becomes a duplicate of film->show; dup<->film become self edges.
    assert_eq!(remap.deletes, ["1", "3", "4"]);
    assert!(remap.updates.is_empty());
    let chain = vec![edge("a", "x", "y", PartOf), edge("b", "y", "dup", PartOf)];
    assert!(
        remap_relationships(&chain, &["dup".into()], "x").is_err(),
        "x part_of y part_of x"
    );
    let ok = remap_relationships(&chain, &["dup".into()], "z").unwrap();
    assert_eq!(
        ok.updates,
        [("b".to_string(), "y".to_string(), "z".to_string())]
    );
    assert!(order_membership_valid(
        &["e1".into(), "s1".into(), "show".into()],
        "show"
    ));
    assert!(!order_membership_valid(&["split-off".into()], "show"));
}
