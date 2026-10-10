//! Logical libraries and registered storage sources.
//!
//! A library is a logical collection (kind, language, policy) over one or more
//! read-only storage sources. A source is a registered root with an expected
//! volume binding; its binding revision changes only on a validated rebind, and
//! scan publication is fenced on it. Exclusions narrow a source's scope.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type Id = String;

pub const MAX_SOURCES: usize = 100;
pub const MAX_EXCLUSIONS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryKind {
    Movies,
    Television,
    Music,
    Photos,
    PersonalVideo,
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceError {
    InvalidName,
    InvalidLanguage,
    TooManySources,
    DuplicateSource(Id),
    UnknownSource(Id),
    InvalidExclusion(usize),
    TooManyExclusions,
    StaleRevision,
    RevisionExhausted,
}

pub fn valid_name(name: &str) -> Result<(), SourceError> {
    if name.trim().is_empty() || name.len() > 200 {
        Err(SourceError::InvalidName)
    } else {
        Ok(())
    }
}

/// BCP 47-shaped tag (letters/digits/hyphen, at most 35 bytes) or empty for
/// "no preference".
pub fn valid_language(tag: &str) -> Result<(), SourceError> {
    let ok = tag.len() <= 35
        && tag
            .split('-')
            .all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()));
    if tag.is_empty() || ok {
        Ok(())
    } else {
        Err(SourceError::InvalidLanguage)
    }
}

/// A library references existing, registered (non-managed) sources, each once.
pub fn validate_library(
    name: &str,
    language: &str,
    source_ids: &[Id],
    registered: &BTreeSet<Id>,
) -> Result<(), SourceError> {
    valid_name(name)?;
    valid_language(language)?;
    if source_ids.len() > MAX_SOURCES {
        return Err(SourceError::TooManySources);
    }
    let mut seen = BTreeSet::new();
    for id in source_ids {
        if !seen.insert(id) {
            return Err(SourceError::DuplicateSource(id.clone()));
        }
        if !registered.contains(id) {
            return Err(SourceError::UnknownSource(id.clone()));
        }
    }
    Ok(())
}

/// Normalize exclusions: source-relative directory or file paths with `/`
/// separators, no `.`/`..`/empty components, no absolute paths. Each excludes
/// itself and everything beneath it. Duplicates collapse; order is canonical.
pub fn normalize_exclusions(patterns: &[String]) -> Result<Vec<String>, SourceError> {
    if patterns.len() > MAX_EXCLUSIONS {
        return Err(SourceError::TooManyExclusions);
    }
    let mut out = BTreeSet::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let trimmed = pattern.trim_end_matches('/');
        let valid = !trimmed.is_empty()
            && pattern.len() <= 1024
            && !trimmed.starts_with('/')
            && !trimmed.contains('\\')
            && !trimmed.contains('\0')
            && trimmed
                .split('/')
                .all(|c| !c.is_empty() && c != "." && c != "..");
        if !valid {
            return Err(SourceError::InvalidExclusion(index));
        }
        out.insert(trimmed.to_string());
    }
    Ok(out.into_iter().collect())
}

/// Is a source-relative path out of scope?
pub fn excluded(path: &str, exclusions: &[String]) -> bool {
    exclusions.iter().any(|e| {
        path == e
            || path
                .strip_prefix(e.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Scan publication requires the source binding the attempt started against.
/// A rebind (relocation) in between invalidates the attempt's observations.
pub fn binding_current(at_start: u64, now: u64) -> bool {
    at_start == now
}

pub fn advance(current: u64, expected: u64) -> Result<u64, SourceError> {
    if current != expected {
        Err(SourceError::StaleRevision)
    } else {
        current.checked_add(1).ok_or(SourceError::RevisionExhausted)
    }
}

/// A new root may not contain, or lie inside, another registered root:
/// overlapping sources would observe the same files twice. Identical roots are
/// the same registration. Paths are canonical absolute paths.
pub fn overlapping<'a>(candidate: &str, registered: &[&'a str]) -> Option<&'a str> {
    let inside = |a: &str, b: &str| {
        a.strip_prefix(b)
            .is_some_and(|rest| rest.starts_with('/') || b.ends_with('/'))
    };
    registered
        .iter()
        .copied()
        .find(|r| *r != candidate && (inside(candidate, r) || inside(r, candidate)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
    Unknown,
    Degraded,
}

/// A source's availability from what is known without touching the disk:
/// enablement and the last finished scan (`Some(true)` complete, `Some(false)`
/// partial, `None` never published). A failed latest attempt degrades it.
pub fn source_availability(
    enabled: bool,
    last_published_complete: Option<bool>,
    last_failed: bool,
) -> Availability {
    match (enabled, last_published_complete, last_failed) {
        (false, ..) => Availability::Unavailable,
        (true, _, true) | (true, Some(false), _) => Availability::Degraded,
        (true, Some(true), false) => Availability::Available,
        (true, None, false) => Availability::Unknown,
    }
}

/// A library is available only if every source is; unavailable only if every
/// source is; unknown with no sources or nothing observed; otherwise degraded.
pub fn library_availability(sources: &[Availability]) -> Availability {
    if sources.is_empty() || sources.iter().all(|s| *s == Availability::Unknown) {
        Availability::Unknown
    } else if sources.iter().all(|s| *s == Availability::Available) {
        Availability::Available
    } else if sources.iter().all(|s| *s == Availability::Unavailable) {
        Availability::Unavailable
    } else {
        Availability::Degraded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusions_are_normalized_prefixes() {
        let e = normalize_exclusions(&["Extras/".into(), "Extras".into(), "a/b".into()]).unwrap();
        assert_eq!(e, ["Extras", "a/b"]);
        assert!(excluded("Extras", &e));
        assert!(excluded("Extras/x.mkv", &e));
        assert!(!excluded("ExtrasMore/x.mkv", &e));
        assert!(excluded("a/b/c.mkv", &e));
        assert!(!excluded("a/bc.mkv", &e));
        for bad in ["", "/abs", "../up", "a/../b", "a//b", "./a", "a\\b"] {
            assert!(normalize_exclusions(&[bad.into()]).is_err(), "{bad}");
        }
    }
    #[test]
    fn libraries_reference_registered_sources_once() {
        let registered: BTreeSet<Id> = ["s1".into(), "s2".into()].into();
        assert!(
            validate_library("Movies", "en-US", &["s1".into(), "s2".into()], &registered).is_ok()
        );
        assert_eq!(
            validate_library("Movies", "", &["s1".into(), "s1".into()], &registered),
            Err(SourceError::DuplicateSource("s1".into()))
        );
        assert_eq!(
            validate_library("Movies", "", &["s3".into()], &registered),
            Err(SourceError::UnknownSource("s3".into()))
        );
        assert_eq!(
            validate_library("Movies", "en_US", &[], &registered),
            Err(SourceError::InvalidLanguage)
        );
        assert!(validate_library(" ", "", &[], &registered).is_err());
        assert!(binding_current(3, 3) && !binding_current(3, 4));
    }
    #[test]
    fn nested_roots_overlap_but_siblings_do_not() {
        let roots = ["/media/movies", "/media/tv"];
        assert_eq!(
            overlapping("/media/movies/4k", &roots),
            Some("/media/movies")
        );
        assert_eq!(overlapping("/media", &roots), Some("/media/movies"));
        assert_eq!(
            overlapping("/media/movies", &roots),
            None,
            "same registration"
        );
        assert_eq!(overlapping("/media/movies-old", &roots), None);
        assert_eq!(overlapping("/srv/music", &roots), None);
    }
    #[test]
    fn relocation_commit_rechecks_every_reviewed_fact() {
        let plan = RelocationPlan {
            source: "s".into(),
            expected_revision: 2,
            binding_revision: 1,
            root: "/new".into(),
            root_identity: "dev:ino".into(),
            verified: vec![("f".into(), "stamp".into())],
            out_of_scope: vec![],
        };
        let facts = || RelocationFacts {
            revision: 2,
            binding_revision: 1,
            root_identity: Some("dev:ino".into()),
            fingerprints: [("f".to_string(), Some("stamp".to_string()))].into(),
            overlapping_root: None,
        };
        assert_eq!(relocation_commit(&plan, &facts()), Ok(()));
        let mut f = facts();
        f.binding_revision = 2;
        assert_eq!(
            relocation_commit(&plan, &f),
            Err(RelocationConflict::SourceChanged)
        );
        let mut f = facts();
        f.root_identity = Some("other".into());
        assert_eq!(
            relocation_commit(&plan, &f),
            Err(RelocationConflict::RootChanged)
        );
        let mut f = facts();
        f.fingerprints.insert("f".into(), None);
        assert_eq!(
            relocation_commit(&plan, &f),
            Err(RelocationConflict::FileChanged("f".into()))
        );
        let mut f = facts();
        f.overlapping_root = Some("/new/inner".into());
        assert!(matches!(
            relocation_commit(&plan, &f),
            Err(RelocationConflict::Overlaps(_))
        ));
    }
    #[test]
    fn availability_is_derived_from_enablement_and_last_scan() {
        use Availability::*;
        assert_eq!(source_availability(false, Some(true), false), Unavailable);
        assert_eq!(source_availability(true, Some(true), false), Available);
        assert_eq!(source_availability(true, Some(false), false), Degraded);
        assert_eq!(source_availability(true, Some(true), true), Degraded);
        assert_eq!(source_availability(true, None, false), Unknown);
        assert_eq!(library_availability(&[]), Unknown);
        assert_eq!(library_availability(&[Available, Available]), Available);
        assert_eq!(
            library_availability(&[Unavailable, Unavailable]),
            Unavailable
        );
        assert_eq!(library_availability(&[Available, Unavailable]), Degraded);
        assert_eq!(library_availability(&[Unknown, Available]), Degraded);
    }
}

/// What a relocation preview verified: the new root's identity and the
/// fingerprint of every in-scope cataloged file at the new location (each
/// already content-verified by hashing during the preview).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelocationPlan {
    pub source: Id,
    pub expected_revision: u64,
    pub binding_revision: u64,
    pub root: String,
    pub root_identity: String,
    /// `(file id, fingerprint at the new root)` for verified files.
    pub verified: Vec<(Id, String)>,
    /// Files outside the source's scope (exclusions) at preview time.
    pub out_of_scope: Vec<Id>,
}

/// Facts observed at commit, inside the writer transaction (stat only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelocationFacts {
    pub revision: u64,
    pub binding_revision: u64,
    pub root_identity: Option<String>,
    pub fingerprints: std::collections::BTreeMap<Id, Option<String>>,
    pub overlapping_root: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelocationConflict {
    SourceChanged,
    RootChanged,
    FileChanged(Id),
    Overlaps(String),
}

/// Commit applies only the reviewed plan: the source must be unchanged, the
/// new root must still be the same volume, no verified file may have changed
/// since its hash was checked, and the root must not overlap another source.
pub fn relocation_commit(
    plan: &RelocationPlan,
    facts: &RelocationFacts,
) -> Result<(), RelocationConflict> {
    if facts.revision != plan.expected_revision || facts.binding_revision != plan.binding_revision {
        return Err(RelocationConflict::SourceChanged);
    }
    if facts.root_identity.as_deref() != Some(plan.root_identity.as_str()) {
        return Err(RelocationConflict::RootChanged);
    }
    if let Some(other) = &facts.overlapping_root {
        return Err(RelocationConflict::Overlaps(other.clone()));
    }
    for (file, fingerprint) in &plan.verified {
        if facts.fingerprints.get(file).and_then(|f| f.as_deref()) != Some(fingerprint.as_str()) {
            return Err(RelocationConflict::FileChanged(file.clone()));
        }
    }
    Ok(())
}
