//! Host binding admission. Catabolic owns command replay and catalog mutations;
//! Motion owns which catalog/incarnation its persisted commands may address.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub catalog_id: String,
    pub incarnation: String,
    pub schema_version: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    InvalidIdentity,
    CatalogChanged,
    RestoreDetected,
    SchemaChanged,
}

impl Identity {
    pub fn validate(&self) -> Result<(), AdmissionError> {
        if self.catalog_id.is_empty() || self.incarnation.is_empty() || self.schema_version == 0 {
            return Err(AdmissionError::InvalidIdentity);
        }
        Ok(())
    }

    /// A restore is reconciliation work, never permission to silently rebind.
    pub fn admit(&self, observed: &Self) -> Result<(), AdmissionError> {
        self.validate()?;
        observed.validate()?;
        if self.catalog_id != observed.catalog_id {
            return Err(AdmissionError::CatalogChanged);
        }
        if self.incarnation != observed.incarnation {
            return Err(AdmissionError::RestoreDetected);
        }
        if self.schema_version != observed.schema_version {
            return Err(AdmissionError::SchemaChanged);
        }
        Ok(())
    }
}

/// Interpret an immutable source-record envelope through an already admitted
/// current owner. This is NOT catalog admission or permission to upgrade:
/// binding/activation and the exact upgrade acknowledgment remain strict.
/// Native 39→40 does not change source binding or observation record semantics.
pub fn admit_retained_source_record(
    current: &Identity,
    recorded: &Identity,
) -> Result<(), AdmissionError> {
    current.validate()?;
    recorded.validate()?;
    if current.catalog_id != recorded.catalog_id {
        return Err(AdmissionError::CatalogChanged);
    }
    if current.incarnation != recorded.incarnation {
        return Err(AdmissionError::RestoreDetected);
    }
    if current.schema_version == recorded.schema_version
        || (current.schema_version == 40 && recorded.schema_version == 39)
    {
        Ok(())
    } else {
        Err(AdmissionError::SchemaChanged)
    }
}

#[cfg(test)]
mod retained_source_tests {
    use super::*;
    #[test]
    fn source_envelope_compatibility_never_changes_catalog_admission() {
        let old = Identity {
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            schema_version: 39,
        };
        let mut current = old.clone();
        current.schema_version = 40;
        assert!(admit_retained_source_record(&current, &old).is_ok());
        assert_eq!(current.admit(&old), Err(AdmissionError::SchemaChanged));
        assert!(admit_retained_source_record(&old, &current).is_err());
        for schema in [0, 38, 41] {
            current.schema_version = schema;
            assert!(admit_retained_source_record(&current, &old).is_err());
        }
        current.schema_version = 40;
        current.incarnation = "restored".into();
        assert_eq!(
            admit_retained_source_record(&current, &old),
            Err(AdmissionError::RestoreDetected)
        );
        current.incarnation = old.incarnation.clone();
        current.catalog_id = "other".into();
        assert_eq!(
            admit_retained_source_record(&current, &old),
            Err(AdmissionError::CatalogChanged)
        );
    }
}

/// Immutable evidence that this host captured an import from its own source.
/// The adapter must persist this with the snapshot in one transaction. Merely
/// deserializing an import whose namespace looks familiar is not this evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportProvenance {
    pub input_digest: String,
    pub source_namespace: String,
    pub format_version: i64,
}
impl ExportProvenance {
    /// Authorizes interpretation by the supported source-export mapper only.
    /// This is not a writer freeze, a reference audit, or activation authority.
    pub fn admits(&self, input_digest: &str, source_namespace: &str) -> bool {
        self.format_version == 1
            && self.input_digest.len() == 64
            && self
                .input_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && !self.source_namespace.is_empty()
            && self.input_digest == input_digest
            && self.source_namespace == source_namespace
    }
}

/// Export only identities that the live source actually protects against
/// withdrawal. Provider evidence and identities on retired aliases are not
/// accepted native decisions. The adapter supplies facts from one snapshot.
pub fn pinned_source_identity(
    state: crate::matching::MatchState,
    retired: bool,
    current: Option<&(String, String)>,
) -> bool {
    !retired && !crate::matching::provider_identity_allowed(state, current, None)
}
