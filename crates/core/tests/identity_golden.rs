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
