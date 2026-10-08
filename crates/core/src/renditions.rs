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
