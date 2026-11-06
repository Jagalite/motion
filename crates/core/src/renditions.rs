//! A registration binds exact identities and revisions; its label is mutable.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub item: String,
    pub file: String,
    pub file_revision: String,
    pub source_file: String,
    pub source_revision: String,
    pub recipe: serde_json::Value,
}
impl Identity {
    pub fn register(
        &self,
        existing: Option<&Self>,
        source_item: &str,
        source_revision: &str,
        output_revision: &str,
    ) -> Result<(), &'static str> {
        if self.item != source_item
            || self.source_revision != source_revision
            || self.file_revision != output_revision
        {
            return Err("rendition_revision_conflict");
        }
        if existing.is_some_and(|old| old != self) {
            return Err("rendition_identity_conflict");
        }
        Ok(())
    }
}
pub fn available(
    output_available: bool,
    output_revision: &str,
    registered_output: &str,
    source_revision: &str,
    registered_source: &str,
) -> bool {
    output_available && output_revision == registered_output && source_revision == registered_source
}

/// One source membership observed through a rendition registration. Missing
/// source rows are represented by absent current revision, never by a trusted
/// pre-filtered authorization bit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageMembership {
    pub registered_output_revision: String,
    pub registered_source_revision: String,
    pub current_source_revision: Option<String>,
    pub source_generated: Option<bool>,
    pub library: Option<String>,
}

pub fn library_scope(
    generated: bool,
    output_revision: &str,
    direct_memberships: impl IntoIterator<Item = String>,
    lineage: &[LineageMembership],
) -> std::collections::BTreeSet<String> {
    if !generated {
        return direct_memberships.into_iter().collect();
    }
    lineage
        .iter()
        .filter(|m| {
            m.registered_output_revision == output_revision
                && m.current_source_revision.as_deref()
                    == Some(m.registered_source_revision.as_str())
                && m.source_generated == Some(false)
        })
        .filter_map(|m| m.library.clone())
        .collect()
}
