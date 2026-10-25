//! Complete descriptive frozen source inventory. Neither historical rows nor
//! this result establish a source binding, scan grant, or current byte authority.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub struct Prepared {
    archive: super::archive::PreparedArchive,
    physical: BTreeSet<String>,
    historical: BTreeMap<String, Value>,
}
#[derive(Debug)]
pub struct Inventory {
    pub catalog_id: String,
    pub incarnation: String,
    pub activation_digest: String,
    pub physical_source_ids: Vec<String>,
    pub historical_source_ids: Vec<String>,
}
/// Applies source-specific interpretation to the complete trusted archive.
/// Other consumers must apply their own exact baseline qualification.
pub async fn prepare_in(db: &mut SqliteConnection) -> anyhow::Result<Prepared> {
    let archive = super::archive::prepare_in(db).await?;
    let mut expected_historical = BTreeSet::new();
    let mut expected_physical = BTreeSet::new();
    let mut expected_rows: BTreeMap<String, Value> = BTreeMap::new();
    let mut historical = BTreeMap::new();
    let mut physical = BTreeSet::new();
    let mut total_sources = None;
    for entry in &archive.entries {
        let cp = &entry.checkpoint;
        let snapshot = &entry.snapshot;
        let raw = &entry.audit_raw;
        let audit: crate::reference_inventory::SourceAudit = serde_json::from_str(raw)?;
        let origin_proof = crate::origin_preservation::verified_in(db, snapshot, &audit).await?;
        ensure!(
            audit.qualified_with_origins(origin_proof.as_ref()),
            "historical source audit is unqualified"
        );
        let count = *audit
            .table_rows
            .get("libraries")
            .context("source inventory count missing")?;
        ensure!(
            count <= 10000 && total_sources.is_none_or(|n| n == count),
            "partition source inventory counts disagree"
        );
        total_sources = Some(count);
        expected_historical.extend(
            audit
                .physical_coverage
                .historical_ids
                .get("libraries")
                .into_iter()
                .flatten()
                .cloned(),
        );
        expected_physical.extend(
            audit
                .physical_coverage
                .preserved_ids
                .get("libraries")
                .into_iter()
                .flatten()
                .cloned(),
        );
        for record in &audit.resolved_evidence {
            let data = &record["data"];
            if data["table"] == "libraries" && data["row"].is_object() {
                let row = data["row"].clone();
                let id = row["id"]
                    .as_str()
                    .context("historical source row ID missing")?
                    .to_owned();
                if let Some(old) = expected_rows.insert(id, row.clone()) {
                    ensure!(old == row, "duplicate frozen source rows disagree");
                }
            }
        }
        for proof in
            crate::motion_export::frozen_motion_sources_in(db, &archive.identity, &cp.import_id)
                .await?
        {
            physical.insert(proof.mapping.library_id);
        }
        for batch in &snapshot.batches {
            if let Some(typed) = &batch.typed {
                for metadata in &typed.metadata {
                    for contribution in &metadata.contributions {
                        if let Some(model) = contribution.fields.get("motion_source_history") {
                            let row = model["source_row"].clone();
                            let id = row["id"]
                                .as_str()
                                .context("historical typed source ID missing")?
                                .to_owned();
                            ensure!(
                                *model
                                    == serde_json::json!({"contract_version":1,"source_row":row,"authority":"unverified legacy provenance; no native occurrence, version, equivalence, source registration or current-byte grant"})
                                    && contribution.namespace == "motion.history"
                                    && contribution.record_id == "source_observation"
                                    && contribution.source_revision == 1
                                    && contribution.kind == catabolic_store::catalog_import_typed::ImportedSourceKind::Scan
                                    && metadata.item_id == format!("motion.source:{id}")
                                    && snapshot.batches.iter().flat_map(|batch| &batch.items)
                                        .any(|item| item.item_id == metadata.item_id
                                            && item.kind == "custom:motion_source"),
                                "historical typed owner mismatch"
                            );
                            if let Some(old) = historical.insert(id, row.clone()) {
                                ensure!(old == row, "partition historical owners disagree");
                            }
                        }
                    }
                }
            }
        }
    }
    ensure!(
        physical == expected_physical
            && historical.keys().cloned().collect::<BTreeSet<_>>() == expected_historical,
        "historical/physical inventory coverage mismatch"
    );
    let all: BTreeSet<_> = physical.union(&expected_historical).cloned().collect();
    ensure!(
        Some(all.len() as u64) == total_sources
            && expected_rows.keys().cloned().collect::<BTreeSet<_>>() == all,
        "frozen source row coverage incomplete"
    );
    for (id, row) in &historical {
        ensure!(
            expected_rows.get(id) == Some(row),
            "typed historical source differs from frozen row"
        );
    }
    Ok(Prepared {
        archive,
        physical,
        historical,
    })
}
impl Prepared {
    /// Caller retains its Motion writer and actual admitted host throughout.
    pub async fn verify_in(
        self,
        db: &mut SqliteConnection,
        host: &mut HostSession,
    ) -> anyhow::Result<Inventory> {
        let archive = self.archive.verify_in(db, host).await?;
        let historical_source_ids = self
            .historical
            .into_keys()
            .filter(|id| !self.physical.contains(id))
            .collect();
        Ok(Inventory {
            catalog_id: archive.identity().catalog_id.clone(),
            incarnation: archive.identity().incarnation.clone(),
            activation_digest: archive.activation_digest()?,
            physical_source_ids: self.physical.into_iter().collect(),
            historical_source_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_historical_inventory_requires_activation_and_never_registers_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.sqlite3");
        crate::initialize(path.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(crate::projection::configure_connection(connection))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(dir.path().join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let backend = crate::NativeCatalog::bind(
            db.clone(),
            path.clone(),
            crate::inspect(path.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        // Genuine legacy provenance with no registered native root. The normal
        // typed historical mapper, not a fabricated mapping, owns its import.
        sqlx::query("INSERT INTO libraries(id,name,root,root_identity,managed,enabled,revision,binding_revision,exclusions_json,scan_barrier) VALUES('offline','Historical source',?,'unavailable:legacy',0,1,7,3,'[]',9)")
            .bind(dir.path().join("missing").to_str().unwrap()).execute(&db).await.unwrap();
        backend
            .freeze_motion_source("historical-inventory", 0)
            .await
            .unwrap();
        backend
            .capture_motion_import("historical-inventory")
            .await
            .unwrap();
        backend.stage_import("historical-inventory").await.unwrap();
        backend
            .prepare_motion_cutover("historical-inventory")
            .await
            .unwrap();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(prepare_in(&mut tx).await.is_err());
        tx.rollback().await.unwrap();
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(30))
            .await
            .unwrap();
        drop(backend);
        let mut server = crate::cutover::open_server(db.clone(), path, "default".into())
            .await
            .unwrap();
        let host = server.admission.admitted_host().unwrap();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let generation = host.catalog_generation().unwrap();
        let references = crate::nfo_bootstrap::source_history_in(&mut tx, host, generation)
            .await
            .unwrap();
        assert!(references.inventory().subjects().is_empty());
        assert!(references.unresolved().is_empty());
        drop(references);
        let archive = super::super::archive::prepare_in(&mut tx)
            .await
            .unwrap()
            .verify_in(&mut tx, host)
            .await
            .unwrap();
        assert_eq!(archive.root_import_id(), "historical-inventory");
        assert!(archive.root_manifest_digest().is_none());
        assert_eq!(archive.entries().len(), 1);
        assert_eq!(archive.activations().count(), 1);
        let entry = &archive.entries()[0];
        assert_eq!(
            entry.snapshot().validate().unwrap(),
            entry.checkpoint().input_digest
        );
        assert!(
            serde_json::from_str::<Value>(entry.audit_raw())
                .unwrap()
                .is_object()
        );
        drop(archive);
        let prepared = prepare_in(&mut tx).await.unwrap();
        let result = prepared.verify_in(&mut tx, host).await.unwrap();
        assert_eq!(result.historical_source_ids, vec!["offline"]);
        assert!(result.physical_source_ids.is_empty());
        let mappings: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_source_mappings")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(mappings, 0);
        // Corrupt the opaque prepared control in this private test only. There
        // is no deserializer/public constructor for such an altered phase.
        let mut altered = prepare_in(&mut tx).await.unwrap();
        altered.archive.control.single = None;
        assert!(altered.verify_in(&mut tx, host).await.is_err());
        let mut altered = prepare_in(&mut tx).await.unwrap();
        altered.archive.identity.incarnation = "different-incarnation".into();
        assert!(altered.verify_in(&mut tx, host).await.is_err());
        tx.rollback().await.unwrap();
        server.admission.admitted_host().unwrap().close().unwrap();
    }
}
