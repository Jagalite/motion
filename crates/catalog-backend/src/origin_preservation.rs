//! Original NFO provenance only. No current-byte or automatic attribution grant.
//! A serialized audit contract is insufficient: every consumer rechecks the
//! complete direct snapshot and canonical occurrence owner output.
use crate::{
    migration::Snapshot, occurrence_bridge::OccurrenceMapping, reference_inventory::SourceAudit,
};
use anyhow::{Context, ensure};
use catabolic_store::catalog_import::ImportBatch;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Contract {
    pub version: u32,
    pub origin_rows: u64,
}
/// No constructor, Deserialize, Clone or public fields. Minted only by verify.
pub(crate) struct VerifiedOriginPreservation<'a> {
    audit: &'a SourceAudit,
    _batches: &'a [ImportBatch],
}
impl VerifiedOriginPreservation<'_> {
    pub(crate) fn admits(&self, audit: &SourceAudit) -> bool {
        std::ptr::eq(self.audit, audit)
    }
}
type Saved = Vec<(String, String, String, String)>;

fn text<'a>(row: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    row[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("origin evidence ID missing")
}
fn verify<'a>(
    batches: &'a [ImportBatch],
    audit: &'a SourceAudit,
    saved: Saved,
) -> anyhow::Result<VerifiedOriginPreservation<'a>> {
    let count = *audit
        .table_rows
        .get("nfo_origins")
        .context("origin count missing")?;
    ensure!(
        (1..=10000).contains(&count),
        "origin count outside bounded cohort"
    );
    let mut rows: BTreeMap<&str, BTreeMap<&str, &Value>> = BTreeMap::new();
    let evidence = audit
        .resolved_evidence
        .iter()
        .map(|record| &record["data"])
        .chain(
            batches
                .iter()
                .flat_map(|b| &b.unresolved_evidence)
                .map(|record| &record.data),
        );
    let mut evidence_bytes = 0usize;
    for data in evidence {
        let Some(table) = data["table"].as_str() else {
            continue;
        };
        if !["items", "media_files", "editions", "nfo_origins"].contains(&table) {
            continue;
        }
        let row = &data["row"];
        if !row.is_object() {
            continue;
        }
        // Count serialization without allocating another copy of the audit.
        let mut budget = ByteBudget {
            used: evidence_bytes,
        };
        serde_json::to_writer(&mut budget, row)?;
        evidence_bytes = budget.used;
        let id = text(
            row,
            if table == "nfo_origins" {
                "item_id"
            } else {
                "id"
            },
        )?;
        let group = rows.entry(table).or_default();
        ensure!(group.len() < 10000, "origin endpoint row bound exceeded");
        ensure!(
            group.insert(id, row).is_none(),
            "duplicate original origin evidence"
        );
    }
    for table in ["items", "media_files", "editions", "nfo_origins"] {
        ensure!(
            audit.table_rows.get(table).copied()
                == Some(rows.get(table).map_or(0, |r| r.len()) as u64),
            "incomplete original origin endpoint audit: {table}"
        );
    }
    let mut mappings = BTreeMap::new();
    let mut bytes = 0usize;
    ensure!(
        saved.len() <= 10000,
        "saved origin mapping count exceeds bound"
    );
    for (id, profile, file, raw) in saved {
        bytes = bytes
            .checked_add(id.len())
            .and_then(|n| n.checked_add(profile.len()))
            .and_then(|n| n.checked_add(file.len()))
            .and_then(|n| n.checked_add(raw.len()))
            .context("origin mapping bytes overflow")?;
        ensure!(
            bytes <= 16 * 1024 * 1024,
            "saved origin mapping bytes exceed bound"
        );
        let mapping: OccurrenceMapping = serde_json::from_str(&raw)?;
        ensure!(
            serde_json::to_string(&mapping)? == raw,
            "noncanonical saved origin mapping"
        );
        ensure!(
            mapping.motion_file["id"] == id
                && mapping.physical.profile == profile
                && mapping.physical.exact.file_id == file,
            "saved origin mapping key mismatch"
        );
        ensure!(
            mappings.insert(id, mapping).is_none(),
            "duplicate saved origin mapping"
        );
    }
    let mut items = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let mut occurrences = BTreeMap::new();
    let mut associations: Vec<&catabolic_store::catalog_import_typed::ImportedAssociation> =
        Vec::new();
    for batch in batches {
        for item in &batch.items {
            ensure!(
                items.insert(item.source_id.as_str(), item).is_none(),
                "duplicate original item mapping"
            );
        }
        if let Some(typed) = &batch.typed {
            for item in &typed.metadata {
                ensure!(
                    metadata.insert(item.item_id.as_str(), item).is_none(),
                    "duplicate original metadata mapping"
                );
            }
            if let Some(native) = &typed.native {
                for occurrence in &native.occurrences {
                    ensure!(
                        occurrences
                            .insert(occurrence.source_occurrence_id.as_str(), occurrence)
                            .is_none(),
                        "duplicate original occurrence"
                    );
                }
                associations.extend(&native.associations);
            }
        }
    }
    let original_items = rows.get("items").context("original items missing")?;
    ensure!(
        original_items.keys().all(|id| items
            .get(*id)
            .is_some_and(|item| metadata.contains_key(item.item_id.as_str()))),
        "incomplete original metadata subjects"
    );
    for (item_id, origin) in rows.get("nfo_origins").context("origin rows missing")? {
        ensure!(
            origin.as_object().is_some_and(|r| r.len() == 2
                && r.contains_key("item_id")
                && r.contains_key("file_id")),
            "unknown origin row fields"
        );
        let file_id = text(origin, "file_id")?;
        let item = items.get(*item_id).context("origin work not imported")?;
        ensure!(
            original_items.contains_key(item_id) && item.item_id == *item_id,
            "origin original work identity changed"
        );
        let source = metadata
            .get(item.item_id.as_str())
            .context("origin metadata missing")?;
        let docs: Vec<_> = source
            .contributions
            .iter()
            .filter(|c| c.namespace == "motion.nfo")
            .collect();
        ensure!(
            docs.len() == 1
                && docs[0].record_id == "document"
                && docs[0].evidence["source_table"] == "metadata_documents"
                && docs[0].evidence["source"] == "nfo"
                && docs[0].evidence["external_id"].is_null()
                && docs[0].source_revision > 0
                && docs[0].kind
                    == catabolic_store::catalog_import_typed::ImportedSourceKind::Provider,
            "exact supported original NFO document missing"
        );
        let file = rows
            .get("media_files")
            .and_then(|r| r.get(file_id))
            .context("origin file absent")?;
        let edition_id = text(file, "edition_id")?;
        let edition = rows
            .get("editions")
            .and_then(|r| r.get(edition_id))
            .context("origin edition absent")?;
        ensure!(
            edition["item_id"] == *item_id,
            "origin file belongs to foreign work"
        );
        ensure!(
            audit
                .physical_coverage
                .preserved_ids
                .get("media_files")
                .is_some_and(|ids| ids.contains(file_id)),
            "origin file lacks current imported owner"
        );
        let mapping = mappings.get(file_id).context("origin mapping absent")?;
        ensure!(
            mapping.motion_file == **file,
            "origin mapping frozen row mismatch"
        );
        ensure!(
            occurrences
                .get(mapping.physical.source_occurrence_id.as_str())
                .is_some_and(|p| **p == mapping.physical),
            "origin occurrence not in exact import"
        );
        let expected = mapping.typed()?;
        let linked = expected.associations.iter().any(|a| {
            if !associations.iter().any(|actual| **actual == *a) {
                return false;
            }
            let Some(owner) = items.values().find(|item| item.item_id == a.item_id) else {
                return false;
            };
            if owner.kind != "custom:motion_version" {
                return false;
            }
            let Some(version) = metadata.get(a.item_id.as_str()) else {
                return false;
            };
            let models: Vec<_> = version
                .contributions
                .iter()
                .filter_map(|c| c.fields.get("motion_version"))
                .collect();
            models.len() == 1
                && models[0]["source_timeline"]["edition_id"] == edition_id
                && models[0]["source_version"]["timeline_id"] == models[0]["source_timeline"]["id"]
                && models[0]["source_bindings"]
                    .as_array()
                    .is_some_and(|bindings| {
                        bindings.iter().any(|b| {
                            b["file_id"] == file_id
                                && b["file_revision"] == file["revision"]
                                && b["part"].as_u64() == Some(u64::from(a.part))
                        })
                    })
        });
        ensure!(
            linked,
            "origin lacks imported exact same-work version association"
        );
    }
    for key in [
        "nfo_origins.item_id->items.id",
        "nfo_origins.file_id->media_files.id",
    ] {
        let slot = audit
            .physical_coverage
            .reference_slots
            .get(key)
            .context("origin reference slot missing")?;
        ensure!(
            slot.resolved == count
                && slot.unmapped == 0
                && slot.malformed == 0
                && slot.nullable_absent == 0,
            "incomplete origin reference coverage"
        );
    }
    Ok(VerifiedOriginPreservation {
        audit,
        _batches: batches,
    })
}

async fn saved_in(db: &mut sqlx::SqliteConnection, id: &str) -> anyhow::Result<Saved> {
    let (count,bytes):(i64,i64)=sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(mapping_json AS BLOB))+length(CAST(motion_file_id AS BLOB))+length(CAST(native_profile AS BLOB))+length(CAST(native_file_id AS BLOB))),0) FROM catabolic_occurrence_mappings WHERE import_id=?").bind(id).fetch_one(&mut *db).await?;
    ensure!(
        (0..=10000).contains(&count) && (0..=16 * 1024 * 1024).contains(&bytes),
        "saved origin mapping bound exceeded"
    );
    Ok(sqlx::query_as("SELECT motion_file_id,native_profile,native_file_id,mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? ORDER BY motion_file_id").bind(id).fetch_all(&mut *db).await?)
}
pub(crate) async fn capture_in<'a>(
    db: &mut sqlx::SqliteConnection,
    id: &str,
    batches: &'a [ImportBatch],
    audit: &'a SourceAudit,
) -> anyhow::Result<VerifiedOriginPreservation<'a>> {
    verify(batches, audit, saved_in(db, id).await?)
}
pub(crate) async fn verified_in<'a>(
    db: &mut sqlx::SqliteConnection,
    snapshot: &'a Snapshot,
    audit: &'a SourceAudit,
) -> anyhow::Result<Option<VerifiedOriginPreservation<'a>>> {
    let Some(contract) = &audit.origin_preservation else {
        return Ok(None);
    };
    ensure!(
        contract.version == 1 && audit.table_rows.get("nfo_origins") == Some(&contract.origin_rows),
        "invalid origin preservation contract"
    );
    let planned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM catabolic_export_plan_parts WHERE import_id=?)",
    )
    .bind(&snapshot.manifest.import_id)
    .fetch_one(&mut *db)
    .await?;
    ensure!(!planned, "origin-bearing multipart import unsupported");
    let proof = capture_in(db, &snapshot.manifest.import_id, &snapshot.batches, audit).await?;
    ensure!(
        audit.qualified_with_origins(Some(&proof)),
        "origin audit source contract is not qualified"
    );
    ensure!(
        snapshot
            .batches
            .iter()
            .all(|b| b.unresolved_evidence.is_empty()),
        "origin contract retains unresolved evidence"
    );
    Ok(Some(proof))
}
pub(crate) fn verified_sync<'a>(
    db: &rusqlite::Connection,
    snapshot: &'a Snapshot,
    audit: &'a SourceAudit,
) -> anyhow::Result<Option<VerifiedOriginPreservation<'a>>> {
    let Some(contract) = &audit.origin_preservation else {
        return Ok(None);
    };
    ensure!(
        contract.version == 1 && audit.table_rows.get("nfo_origins") == Some(&contract.origin_rows),
        "invalid origin preservation contract"
    );
    let planned: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM catabolic_export_plan_parts WHERE import_id=?)",
        [&snapshot.manifest.import_id],
        |r| r.get(0),
    )?;
    ensure!(!planned, "origin-bearing multipart import unsupported");
    let (count,bytes):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(length(CAST(mapping_json AS BLOB))+length(CAST(motion_file_id AS BLOB))+length(CAST(native_profile AS BLOB))+length(CAST(native_file_id AS BLOB))),0) FROM catabolic_occurrence_mappings WHERE import_id=?",[&snapshot.manifest.import_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        (0..=10000).contains(&count) && (0..=16 * 1024 * 1024).contains(&bytes),
        "saved origin mapping bound exceeded"
    );
    let mut stmt=db.prepare("SELECT motion_file_id,native_profile,native_file_id,mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? ORDER BY motion_file_id")?;
    let saved = stmt
        .query_map([&snapshot.manifest.import_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let proof = verify(&snapshot.batches, audit, saved)?;
    ensure!(
        audit.qualified_with_origins(Some(&proof)),
        "origin audit source contract is not qualified"
    );
    ensure!(
        snapshot
            .batches
            .iter()
            .all(|b| b.unresolved_evidence.is_empty()),
        "origin contract retains unresolved evidence"
    );
    Ok(Some(proof))
}

struct ByteBudget {
    used: usize,
}
impl std::io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.used = self
            .used
            .checked_add(bytes.len())
            .filter(|n| *n <= 16 * 1024 * 1024)
            .ok_or_else(|| std::io::Error::other("origin evidence byte bound exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn incomplete_audit() -> SourceAudit {
        serde_json::from_value(serde_json::json!({
            "contract_version":1,"slots":[],"table_rows":{"nfo_origins":1,"items":0,"media_files":0,"editions":0},
            "unknown_tables":[],"schema_mismatches":[],"logical_only_supported":false,
            "resolved_evidence":[],"atomic_batch_groups":[]
        })).unwrap()
    }
    #[test]
    fn serialized_count_never_proves_complete_origins() {
        let mut audit = incomplete_audit();
        audit.origin_preservation = Some(Contract {
            version: 1,
            origin_rows: 1,
        });
        assert!(!audit.source_qualification_supported());
        assert!(verify(&[], &audit, vec![]).is_err());
        audit.resolved_evidence = vec![
            serde_json::json!({"data":{"table":"nfo_origins","row":{"item_id":"work","file_id":"file"}}});
            2
        ];
        assert!(
            verify(&[], &audit, vec![])
                .err()
                .unwrap()
                .to_string()
                .contains("duplicate")
        );
    }
    #[test]
    fn token_is_bound_to_exact_borrowed_audit_not_equal_count_or_copy() {
        let audit = incomplete_audit();
        let clone = audit.clone();
        let token = VerifiedOriginPreservation {
            audit: &audit,
            _batches: &[],
        };
        assert!(token.admits(&audit));
        assert!(!token.admits(&clone));
    }
    #[test]
    fn evidence_budget_counts_escaped_encoding_and_checked_overflow() {
        use std::io::Write;
        let mut budget = ByteBudget {
            used: 16 * 1024 * 1024 - 4,
        };
        assert!(serde_json::to_writer(&mut budget, "\n\n").is_err());
        let mut budget = ByteBudget { used: usize::MAX };
        assert!(budget.write_all(b"x").is_err());
    }
}
