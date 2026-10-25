//! Complete archived origin descriptions for EXISTING imported NFO sources.
//! These are neither first-source absence grants nor current file/ACL authority.
use crate::{
    imported_sources::archive::{self, PreparedArchive},
    occurrence_bridge::OccurrenceMapping,
};
use anyhow::{Context, ensure};
use catabolic_runtime::host::HostSession;
use catabolic_store::catalog_import_typed::PhysicalMapping;
use serde::Serialize;
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

const MAX_CLAIM_BYTES: usize = 16 * 1024 * 1024;
/// Counts serialized borrowed values without allocating their encoded output.
struct OutputBudget {
    used: usize,
}
impl std::io::Write for OutputBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.used = self
            .used
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_CLAIM_BYTES)
            .ok_or_else(|| std::io::Error::other("origin description output exceeds bounds"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl OutputBudget {
    fn charge(&mut self, value: &impl Serialize) -> anyhow::Result<()> {
        serde_json::to_writer(&mut *self, value)?;
        // Covers field names, enum/map framing and owned container bookkeeping.
        std::io::Write::write_all(self, &[0; 512])?;
        Ok(())
    }
}

#[derive(Serialize)]
pub enum ArchivedOrigin {
    AuditedAbsent,
    Physical {
        physical_part: usize,
        motion_file_id: String,
        saved_mapping_digest: String,
        physical: PhysicalMapping,
    },
}
#[derive(Serialize)]
pub struct ArchivedSubject {
    metadata_part: usize,
    original_item_id: String,
    origin: ArchivedOrigin,
}
impl ArchivedSubject {
    pub fn metadata_part(&self) -> usize {
        self.metadata_part
    }
    pub fn original_item_id(&self) -> &str {
        &self.original_item_id
    }
    pub fn origin(&self) -> &ArchivedOrigin {
        &self.origin
    }
}
#[derive(Serialize)]
struct Part {
    import_id: String,
    input_digest: String,
    activation_digest: String,
    reference_ack_id: String,
    mapping_digest: String,
}
#[derive(Serialize)]
struct Claim {
    domain: &'static str,
    catalog_id: String,
    incarnation: String,
    root_import_id: String,
    root_manifest_digest: Option<String>,
    audit_digest: String,
    parts: Vec<Part>,
    subjects: BTreeMap<String, ArchivedSubject>,
}
/// Constructor is the complete retained archive audit below, never request JSON.
/// Native owners must still verify permanent activations, exact effects and tags.
pub struct ImportedOriginsDescription {
    _archive: PreparedArchive,
    claim: Claim,
    digest: String,
}
impl ImportedOriginsDescription {
    pub(crate) fn archive(&self) -> &PreparedArchive {
        &self._archive
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn subjects(&self) -> &BTreeMap<String, ArchivedSubject> {
        &self.claim.subjects
    }
    pub fn audit_digest(&self) -> &str {
        &self.claim.audit_digest
    }
}

pub async fn prepare_in(
    db: &mut SqliteConnection,
    host: &mut HostSession,
    generation: i64,
    requested: &BTreeSet<String>,
) -> anyhow::Result<ImportedOriginsDescription> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    ensure!(
        host.catalog_generation()? == generation,
        "origin description generation changed"
    );
    let archive = archive::prepare_in(db).await?;
    archive.verify_custody_in(db, host).await?;
    let (count, bytes): (i64,i64) = sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(motion_file_id AS BLOB))+length(CAST(native_profile AS BLOB))+length(CAST(native_file_id AS BLOB))+length(CAST(mapping_json AS BLOB))),0) FROM catabolic_occurrence_mappings WHERE import_id=?")
        .bind(archive.root_import_id()).fetch_one(&mut *db).await?;
    ensure!(
        (0..=10000).contains(&count) && (0..=16 * 1024 * 1024).contains(&bytes),
        "archived occurrence mappings exceed bounds"
    );
    let rows: Vec<(String,String,String,String)> = sqlx::query_as("SELECT motion_file_id,native_profile,native_file_id,mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? ORDER BY motion_file_id")
        .bind(archive.root_import_id()).fetch_all(&mut *db).await?;
    ensure!(
        rows.len() == usize::try_from(count)?,
        "archived occurrence count changed"
    );
    let mut mappings = BTreeMap::new();
    for (id, profile, file, raw) in rows {
        let mapping: OccurrenceMapping = serde_json::from_str(&raw)?;
        ensure!(
            serde_json::to_string(&mapping)? == raw,
            "archived occurrence is not canonical owner output"
        );
        ensure!(
            mapping.motion_file["id"] == id
                && mapping.physical.profile == profile
                && mapping.physical.exact.file_id == file,
            "archived occurrence key mismatch"
        );
        ensure!(
            mappings
                .insert(id, (mapping, catabolic_core::sha256(raw.as_bytes())))
                .is_none(),
            "duplicate archived occurrence"
        );
    }
    let claim = describe(&archive, requested, &mappings)?;
    // Admit exact final encoding before allocating it, independently of input bounds.
    let mut encoded = OutputBudget { used: 0 };
    serde_json::to_writer(&mut encoded, &claim)?;
    let digest = catabolic_core::sha256(serde_json::to_vec(&claim)?);
    archive.verify_custody_in(db, host).await?;
    ensure!(
        host.catalog_generation()? == generation,
        "origin description generation changed"
    );
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    Ok(ImportedOriginsDescription {
        _archive: archive,
        claim,
        digest,
    })
}
fn describe(
    archive: &PreparedArchive,
    requested: &BTreeSet<String>,
    mappings: &BTreeMap<String, (OccurrenceMapping, String)>,
) -> anyhow::Result<Claim> {
    let requested_bytes = requested
        .iter()
        .try_fold(0usize, |n, id| n.checked_add(id.len()))
        .context("origin subjects overflow")?;
    ensure!(
        !requested.is_empty() && requested.len() <= 10000 && requested_bytes <= 2 * 1024 * 1024,
        "bounded explicit origin subjects required"
    );
    let raw = archive
        .entries()
        .first()
        .context("empty origin archive")?
        .audit_raw();
    let audit: crate::reference_inventory::SourceAudit = serde_json::from_str(raw)?;
    ensure!(
        audit.unknown_tables.is_empty() && audit.schema_mismatches.is_empty(),
        "unreviewed origin archive schema"
    );
    let mut originals = BTreeSet::new();
    let mut origins = BTreeMap::new();
    let mut files = BTreeMap::new();
    for record in &audit.resolved_evidence {
        let data = &record["data"];
        let row = &data["row"];
        match data["table"].as_str() {
            Some("items") => {
                ensure!(
                    originals.insert(row["id"].as_str().context("archived item ID missing")?),
                    "duplicate archived item"
                );
            }
            Some("nfo_origins") => {
                ensure!(
                    origins
                        .insert(
                            row["item_id"]
                                .as_str()
                                .context("archived origin subject missing")?,
                            row["file_id"]
                                .as_str()
                                .context("archived origin file missing")?
                        )
                        .is_none(),
                    "duplicate archived origin"
                );
            }
            Some("media_files") => {
                ensure!(
                    files
                        .insert(row["id"].as_str().context("archived file ID missing")?, row)
                        .is_none(),
                    "duplicate archived file"
                );
            }
            _ => {}
        }
    }
    for (table, count) in [
        ("items", originals.len()),
        ("nfo_origins", origins.len()),
        ("media_files", files.len()),
    ] {
        ensure!(
            audit.table_rows.get(table) == Some(&(count as u64)),
            "complete archived {table} audit missing"
        );
    }
    ensure!(
        origins
            .iter()
            .all(|(item, file)| originals.contains(item) && files.contains_key(file)),
        "archived origin has unresolved subject/file"
    );
    let mut items = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let mut physical = BTreeMap::new();
    for (ordinal, entry) in archive.entries().iter().enumerate() {
        ensure!(
            entry.audit_raw() == raw,
            "root origin audit differs between partitions"
        );
        for batch in &entry.snapshot().batches {
            for item in &batch.items {
                ensure!(
                    items.insert(item.item_id.as_str(), item).is_none(),
                    "ambiguous imported subject mapping"
                );
            }
            if let Some(typed) = &batch.typed {
                for source in &typed.metadata {
                    ensure!(
                        metadata
                            .insert(source.item_id.as_str(), (ordinal, source))
                            .is_none(),
                        "ambiguous imported metadata"
                    );
                }
                if let Some(native) = &typed.native {
                    for occurrence in &native.occurrences {
                        ensure!(
                            physical
                                .insert(
                                    occurrence.source_occurrence_id.as_str(),
                                    (ordinal, occurrence)
                                )
                                .is_none(),
                            "ambiguous archived physical occurrence"
                        );
                    }
                }
            }
        }
    }
    let mut mapped = BTreeSet::new();
    for item in items.values() {
        if originals.contains(item.source_id.as_str()) {
            ensure!(
                mapped.insert(item.source_id.as_str())
                    && metadata.contains_key(item.item_id.as_str()),
                "incomplete original work mapping"
            );
        }
    }
    ensure!(
        mapped == originals,
        "complete original item mapping missing"
    );
    let mut output = OutputBudget { used: 0 };
    output.charge(&(
        archive.identity().catalog_id.as_str(),
        archive.identity().incarnation.as_str(),
        archive.root_import_id(),
        archive.root_manifest_digest(),
    ))?;
    let mut subjects = BTreeMap::new();
    for subject in requested {
        let item = items
            .get(subject.as_str())
            .context("requested origin subject missing")?;
        ensure!(
            originals.contains(item.source_id.as_str()),
            "not an original Motion work"
        );
        let (metadata_part, source) = metadata
            .get(subject.as_str())
            .context("requested metadata missing")?;
        ensure!(
            matches!(source.contract_version, 1 | 2),
            "unknown imported metadata contract"
        );
        let nfo: Vec<_> = source
            .contributions
            .iter()
            .filter(|c| c.namespace == "motion.nfo")
            .collect();
        ensure!(
            nfo.len() == 1
                && nfo[0].record_id == "document"
                && nfo[0].evidence["source_table"] == "metadata_documents"
                && nfo[0].evidence["source"] == "nfo",
            "existing exact original NFO document required"
        );
        output.charge(&(subject.as_str(), item.source_id.as_str(), metadata_part))?;
        let origin = if let Some(file) = origins.get(item.source_id.as_str()) {
            let (mapping, mapping_digest) = mappings
                .get(*file)
                .context("exact saved origin mapping unavailable")?;
            ensure!(
                files
                    .get(file)
                    .is_some_and(|row| **row == mapping.motion_file),
                "origin mapping differs from archived file row"
            );
            let (part, occurrence) = physical
                .get(mapping.physical.source_occurrence_id.as_str())
                .context("origin physical part missing")?;
            ensure!(
                **occurrence == mapping.physical,
                "origin physical mapping differs from activated input"
            );
            output.charge(&(*part, *file, mapping_digest.as_str(), &mapping.physical))?;
            ArchivedOrigin::Physical {
                physical_part: *part,
                motion_file_id: (*file).into(),
                saved_mapping_digest: mapping_digest.clone(),
                physical: mapping.physical.clone(),
            }
        } else {
            ArchivedOrigin::AuditedAbsent
        };
        subjects.insert(
            subject.clone(),
            ArchivedSubject {
                metadata_part: *metadata_part,
                original_item_id: item.source_id.clone(),
                origin,
            },
        );
    }
    let mut parts = Vec::new();
    for (checkpoint, raw) in archive.activation_envelopes() {
        let command = crate::decode_envelope(raw)?;
        ensure!(
            command.key().catalog_id() == archive.identity().catalog_id
                && command.payload()
                    == &serde_json::json!({"operation":"import","mutation":catabolic_store::catalog_import::ImportMutation::Activate{import_id:checkpoint.import_id.clone(),input_digest:checkpoint.input_digest.clone(),reference_ack_id:checkpoint.reference_ack_id.clone(),mapping_digest:checkpoint.mapping_digest.clone()}}),
            "archived activation commitment differs"
        );
        output.charge(&(
            checkpoint.import_id.as_str(),
            checkpoint.input_digest.as_str(),
            command.digest().as_str(),
            checkpoint.reference_ack_id.as_str(),
            checkpoint.mapping_digest.as_str(),
        ))?;
        parts.push(Part {
            import_id: checkpoint.import_id.clone(),
            input_digest: checkpoint.input_digest.clone(),
            activation_digest: command.digest().as_str().into(),
            reference_ack_id: checkpoint.reference_ack_id.clone(),
            mapping_digest: checkpoint.mapping_digest.clone(),
        });
    }
    ensure!(
        parts.len() == archive.entries().len(),
        "incomplete activation part closure"
    );
    Ok(Claim {
        domain: "motion-existing-nfo-complete-origin-audit-v1",
        catalog_id: archive.identity().catalog_id.clone(),
        incarnation: archive.identity().incarnation.clone(),
        root_import_id: archive.root_import_id().into(),
        root_manifest_digest: archive.root_manifest_digest().map(str::to_owned),
        audit_digest: catabolic_core::sha256(raw.as_bytes()),
        parts,
        subjects,
    })
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    #[test]
    fn replicated_outputs_consume_one_cumulative_budget_before_copying() {
        let borrowed = "x".repeat(1024 * 1024);
        let mut budget = OutputBudget { used: 0 };
        for _ in 0..15 {
            budget.charge(&borrowed).unwrap();
        }
        assert!(budget.charge(&borrowed).is_err());
    }

    #[test]
    fn encoded_escapes_and_checked_overflow_are_charged() {
        let mut budget = OutputBudget { used: 0 };
        budget.charge(&"\u{0000}").unwrap();
        assert_eq!(budget.used, 8 + 512);
        let mut overflow = OutputBudget { used: usize::MAX };
        assert!(std::io::Write::write_all(&mut overflow, b"x").is_err());
    }
}
