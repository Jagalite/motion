//! Pure source identity admission shared by registration, scanning and export.
//! Filesystem/database adapters supply observations; none of these decisions
//! permits replacing an existing binding or recovering a different incarnation.
use crate::catalog_backend::{AdmissionError, Identity};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub library_id: String,
    pub profile: String,
    pub native_source: String,
    pub catalog: Identity,
    pub motion_root: String,
    pub motion_root_identity: String,
    pub motion_binding_revision: i64,
    pub exclusions: Vec<String>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    Invalid,
    Catalog(AdmissionError),
    MappingChanged,
}
impl SourceIdentity {
    pub fn validate(&self) -> Result<(), Rejection> {
        self.catalog.validate().map_err(Rejection::Catalog)?;
        if self.library_id.is_empty()
            || !name(&self.profile)
            || !name(&self.native_source)
            || self.motion_root.is_empty()
            || self.motion_root_identity.is_empty()
            || self.motion_binding_revision < 1
        {
            return Err(Rejection::Invalid);
        }
        if crate::sources::normalize_exclusions(&self.exclusions).map_err(|_| Rejection::Invalid)?
            != self.exclusions
        {
            return Err(Rejection::Invalid);
        }
        Ok(())
    }
    pub fn admit(&self, observed: &Self) -> Result<(), Rejection> {
        self.validate()?;
        observed.validate()?;
        self.catalog
            .admit(&observed.catalog)
            .map_err(Rejection::Catalog)?;
        if self != observed {
            return Err(Rejection::MappingChanged);
        }
        Ok(())
    }
}
pub fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> SourceIdentity {
        SourceIdentity {
            library_id: "library".into(),
            profile: "default".into(),
            native_source: "media".into(),
            catalog: Identity {
                catalog_id: "catalog".into(),
                incarnation: "incarnation".into(),
                schema_version: 39,
            },
            motion_root: "/media".into(),
            motion_root_identity: "volume".into(),
            motion_binding_revision: 1,
            exclusions: vec![],
        }
    }
    #[test]
    fn every_identity_component_is_required() {
        let original = identity();
        assert_eq!(original.admit(&original), Ok(()));
        for field in 0..10 {
            let mut changed = original.clone();
            match field {
                0 => changed.library_id.push('x'),
                1 => changed.profile.push('x'),
                2 => changed.native_source.push('x'),
                3 => changed.catalog.catalog_id.push('x'),
                4 => changed.catalog.incarnation.push('x'),
                5 => changed.catalog.schema_version += 1,
                6 => changed.motion_root.push('x'),
                7 => changed.motion_root_identity.push('x'),
                8 => changed.motion_binding_revision += 1,
                _ => changed.exclusions.push("private".into()),
            }
            assert!(original.admit(&changed).is_err(), "field {field}");
        }
    }
    #[test]
    fn malformed_names_and_revisions_are_not_admitted() {
        for value in [
            "",
            "../escape",
            "a/b",
            "with space",
            "α",
            "-name",
            "_name",
            ".name",
        ] {
            let mut changed = identity();
            changed.native_source = value.into();
            assert!(changed.validate().is_err());
        }
        assert!(name(&"a".repeat(64)));
        assert!(!name(&"a".repeat(65)));
        let mut changed = identity();
        changed.motion_binding_revision = 0;
        assert!(changed.validate().is_err());
    }
}

/// Independent physical identity observations. Path equality is deliberately
/// absent: it is only a candidate lookup, never a continuity decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedOccurrenceIdentity {
    pub size: i64,
    pub mtime_ns: i64,
    pub device: i64,
    pub inode: i64,
    pub ctime_ns: i64,
}
pub fn admits_occurrence(
    fingerprint: &str,
    expected_bytes: i64,
    source_available: bool,
    observed_present: bool,
    observed: ObservedOccurrenceIdentity,
) -> bool {
    let Ok(v) = fingerprint
        .split(':')
        .map(str::parse::<i128>)
        .collect::<Result<Vec<_>, _>>()
    else {
        return false;
    };
    v.len() == 6
        && source_available
        && observed_present
        && i128::from(observed.size) == v[0]
        && expected_bytes == observed.size
        && i128::from(observed.mtime_ns) == v[1]
        && i128::from(observed.device) == v[2]
        && i128::from(observed.inode) == v[3]
        && v[5] >= 0
        && v[5] < 1_000_000_000
        && Some(i128::from(observed.ctime_ns))
            == v[4]
                .checked_mul(1_000_000_000)
                .and_then(|n| n.checked_add(v[5]))
}

#[cfg(test)]
mod occurrence_tests {
    use super::*;
    #[test]
    fn fingerprint_requires_every_identity_field_and_live_source() {
        let observed = ObservedOccurrenceIdentity {
            size: 1,
            mtime_ns: 2,
            device: 3,
            inode: 4,
            ctime_ns: 5_000_000_006,
        };
        assert!(admits_occurrence("1:2:3:4:5:6", 1, true, true, observed));
        for raw in [
            "1:2:3:4:5",
            "1:2:3:4:5:1000000000",
            "1:2:3:4:5:-1",
            "1:2:3:4:170141183460469231731687303715884105727:6",
            "1:2:9:4:5:6",
            "1:2:3:9:5:6",
        ] {
            assert!(!admits_occurrence(raw, 1, true, true, observed));
        }
        assert!(!admits_occurrence("1:2:3:4:5:6", 1, false, true, observed));
        assert!(!admits_occurrence("1:2:3:4:5:6", 1, true, false, observed));
        assert!(!admits_occurrence("1:2:3:4:5:6", 2, true, true, observed));
    }
}

/// Evidence for one already matched native file identity. Omission from a list
/// is Unknown, never proof that its retained real locator may be reused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OccurrencePresence {
    Present,
    Missing,
    Unknown,
}
pub fn retires_occurrence(
    presence: OccurrencePresence,
    original_revision: &str,
    observed_revision: Option<&str>,
    complete_fingerprint_matches: Option<bool>,
) -> bool {
    if original_revision.is_empty() {
        return false;
    }
    match presence {
        OccurrencePresence::Missing => true,
        OccurrencePresence::Present => observed_revision.is_some_and(|revision| {
            !revision.is_empty()
                && (revision != original_revision || complete_fingerprint_matches == Some(false))
        }),
        OccurrencePresence::Unknown => false,
    }
}
#[cfg(test)]
mod retirement_tests {
    use super::*;
    #[test]
    fn historical_locator_requires_explicit_native_staleness() {
        assert!(retires_occurrence(
            OccurrencePresence::Missing,
            "old",
            None,
            None
        ));
        assert!(retires_occurrence(
            OccurrencePresence::Present,
            "old",
            Some("new"),
            None
        ));
        assert!(retires_occurrence(
            OccurrencePresence::Present,
            "same",
            Some("same"),
            Some(false)
        ));
        assert!(!retires_occurrence(
            OccurrencePresence::Present,
            "same",
            Some("same"),
            None
        ));
        assert!(!retires_occurrence(
            OccurrencePresence::Present,
            "same",
            Some("same"),
            Some(true)
        ));
        assert!(!retires_occurrence(
            OccurrencePresence::Present,
            "old",
            None,
            Some(false)
        ));
        assert!(!retires_occurrence(
            OccurrencePresence::Unknown,
            "old",
            None,
            None
        ));
        assert!(!retires_occurrence(
            OccurrencePresence::Unknown,
            "old",
            Some("new"),
            Some(false)
        ));
    }
}
