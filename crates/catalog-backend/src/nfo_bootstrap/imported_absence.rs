//! Complete frozen-root absence descriptions, not native first-source grants.
//! Native permanent activation and untouched current structural proof are still
//! required before this host evidence can become a private owner capability.
use crate::imported_sources::archive::{self, PreparedArchive};
use anyhow::{Context, ensure};
use catabolic_runtime::host::HostSession;
use serde::Serialize;
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize)]
struct Part {
    ordinal: u32,
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
    archived_audit_digest: String,
    ordered_parts: Vec<Part>,
    subjects: BTreeMap<String, String>,
}
/// No public fields, constructor or deserializer. This proves only complete
/// original archive absence, never current absence, authorization or execution.
pub struct ImportedAbsenceDescription {
    archive: PreparedArchive,
    claim: Claim,
    digest: String,
}
impl ImportedAbsenceDescription {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn subjects(&self) -> &BTreeMap<String, String> {
        &self.claim.subjects
    }
    pub fn root_import_id(&self) -> &str {
        self.archive.root_import_id()
    }
}

/// Capture a complete archive under the real Motion writer. The retained native
/// session is identity/generation fenced, but no full-result receipt is needed:
/// later native proof verifies the permanent Activate commitments independently.
pub async fn prepare_in(
    db: &mut SqliteConnection,
    host: &mut HostSession,
    generation: i64,
    requested: &BTreeSet<String>,
) -> anyhow::Result<ImportedAbsenceDescription> {
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    ensure!(
        host.catalog_generation()? == generation,
        "import absence generation changed"
    );
    let archive = archive::prepare_in(db).await?;
    archive.verify_custody_in(db, host).await?;
    let claim = describe(&archive, requested)?;
    let digest = catabolic_core::sha256(serde_json::to_vec(&claim)?);
    ensure!(
        host.catalog_generation()? == generation,
        "import absence generation changed"
    );
    crate::source_lifecycle::require_retained_writer_in(db).await?;
    Ok(ImportedAbsenceDescription {
        archive,
        claim,
        digest,
    })
}

fn describe(archive: &PreparedArchive, requested: &BTreeSet<String>) -> anyhow::Result<Claim> {
    ensure!(
        !requested.is_empty() && requested.len() <= 10000,
        "bounded explicit absence subjects required"
    );
    let bytes = requested
        .iter()
        .try_fold(0usize, |sum, id| sum.checked_add(id.len()))
        .context("absence subject size overflow")?;
    ensure!(
        bytes <= 2 * 1024 * 1024,
        "absence subject names exceed budget"
    );
    let first = archive
        .entries()
        .first()
        .context("complete archive is empty")?;
    // Partition exports retain the same complete root audit, not disjoint audit
    // prefixes. Count it once and require identical exact bytes in every part.
    let raw = first.audit_raw();
    let audit: crate::reference_inventory::SourceAudit = serde_json::from_str(raw)?;
    ensure!(
        audit.unknown_tables.is_empty() && audit.schema_mismatches.is_empty(),
        "unreviewed original schema"
    );
    let mut original_items = BTreeSet::new();
    let mut origins = BTreeSet::new();
    for record in &audit.resolved_evidence {
        let data = &record["data"];
        if data["table"] == "items" {
            let id = data["row"]["id"]
                .as_str()
                .context("malformed archived item")?;
            ensure!(
                original_items.insert(id),
                "duplicate archived item evidence"
            );
        }
        if data["table"] == "nfo_origins" {
            let row = &data["row"];
            let id = row["item_id"]
                .as_str()
                .context("malformed archived NFO origin")?;
            row["file_id"]
                .as_str()
                .context("malformed archived origin file")?;
            ensure!(origins.insert(id), "duplicate archived NFO origin");
        }
    }
    ensure!(
        audit.table_rows.get("items") == Some(&(original_items.len() as u64)),
        "original item audit incomplete"
    );
    ensure!(
        audit.table_rows.get("nfo_origins") == Some(&(origins.len() as u64)),
        "original origin audit incomplete"
    );
    let mut items = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    let mut nfo_tags = BTreeSet::new();
    let mut documents = BTreeSet::new();
    for entry in archive.entries() {
        ensure!(
            entry.audit_raw() == raw,
            "partition audit differs from complete root"
        );
        for batch in &entry.snapshot().batches {
            for item in &batch.items {
                ensure!(
                    items.insert(item.item_id.as_str(), item).is_none(),
                    "duplicate imported item mapping"
                );
            }
            if let Some(typed) = &batch.typed {
                for source in &typed.metadata {
                    ensure!(
                        metadata.insert(source.item_id.as_str(), source).is_none(),
                        "duplicate imported metadata subject"
                    );
                    for contribution in &source.contributions {
                        if contribution.evidence["source_table"] == "metadata_documents" {
                            let name = contribution.evidence["source"]
                                .as_str()
                                .context("archived document source missing")?;
                            ensure!(
                                contribution.namespace == format!("motion.{name}")
                                    && contribution.record_id == "document",
                                "original document namespace differs from archived source"
                            );
                            ensure!(
                                documents.insert((source.item_id.as_str(), name)),
                                "duplicate original document evidence"
                            );
                        }
                    }
                }
                for tags in &typed.tags {
                    if tags.namespace == "motion.nfo" {
                        nfo_tags.insert(tags.item_id.as_str());
                    }
                }
            }
        }
    }
    ensure!(
        audit.table_rows.get("metadata_documents") == Some(&(documents.len() as u64)),
        "original document audit incomplete"
    );
    let mut mapped_originals = BTreeSet::new();
    for item in items.values() {
        if original_items.contains(item.source_id.as_str()) {
            ensure!(
                mapped_originals.insert(item.source_id.as_str()),
                "ambiguous original item mapping"
            );
            ensure!(
                metadata.contains_key(item.item_id.as_str()),
                "original managed metadata omitted"
            );
        }
    }
    ensure!(
        mapped_originals == original_items,
        "complete original item mapping missing"
    );
    let mut subjects = BTreeMap::new();
    for id in requested {
        let item = items
            .get(id.as_str())
            .context("absence subject mapping missing")?;
        ensure!(
            original_items.contains(item.source_id.as_str()),
            "subject is not an original Motion work"
        );
        ensure!(
            !origins.contains(item.source_id.as_str()) && !nfo_tags.contains(id.as_str()),
            "original NFO origin or tag set exists"
        );
        ensure!(
            item.legacy_metadata
                .as_object()
                .is_some_and(|v| v.is_empty()),
            "unknown original legacy metadata cannot prove automatic absence"
        );
        let source = metadata
            .get(id.as_str())
            .context("managed imported metadata missing")?;
        ensure!(
            matches!(source.contract_version, 1 | 2),
            "imported first-source requires a known managed metadata contract"
        );
        ensure!(
            !source
                .contributions
                .iter()
                .any(|c| c.namespace == "motion.nfo"),
            "original NFO source exists"
        );
        let markers: Vec<_> = source
            .contributions
            .iter()
            .filter(|c| c.fields.get("motion_catalog").is_some())
            .collect();
        ensure!(
            markers.len() == 1,
            "exact original structural marker required"
        );
        let marker = markers[0];
        ensure!(
            marker.namespace == "motion.structure"
                && marker.record_id == "source_projection"
                && marker.kind == catabolic_store::catalog_import_typed::ImportedSourceKind::Scan,
            "unexpected imported structural owner"
        );
        let structure = &marker.fields["motion_catalog"];
        ensure!(
            structure["contract_version"] == 1 && structure["item_id"] == *id,
            "structural marker identity mismatch"
        );
        let identities = structure["external_ids"]
            .as_array()
            .context("complete original identity array missing")?;
        for identity in identities {
            let provider = identity["provider"]
                .as_str()
                .context("malformed original identity provider")?;
            identity["value"]
                .as_str()
                .context("malformed original identity value")?;
            identity["namespace_version"]
                .as_str()
                .context("malformed original identity version")?;
            ensure!(provider != "nfo", "original automatic NFO identity exists");
        }
        subjects.insert(id.clone(), item.source_id.clone());
    }
    let mut ordered_parts = Vec::new();
    for (ordinal, (checkpoint, raw)) in archive.activation_envelopes().enumerate() {
        let activation = crate::decode_envelope(raw)?;
        ensure!(
            activation.key().catalog_id() == archive.identity().catalog_id,
            "foreign activation catalog"
        );
        let expected = serde_json::json!({"operation":"import","mutation":catabolic_store::catalog_import::ImportMutation::Activate {
            import_id: checkpoint.import_id.clone(), input_digest: checkpoint.input_digest.clone(),
            reference_ack_id: checkpoint.reference_ack_id.clone(), mapping_digest: checkpoint.mapping_digest.clone(),
        }});
        ensure!(
            activation.payload() == &expected,
            "activation differs from archived checkpoint"
        );
        ordered_parts.push(Part {
            ordinal: u32::try_from(ordinal)?,
            import_id: checkpoint.import_id.clone(),
            input_digest: checkpoint.input_digest.clone(),
            activation_digest: activation.digest().as_str().into(),
            reference_ack_id: checkpoint.reference_ack_id.clone(),
            mapping_digest: checkpoint.mapping_digest.clone(),
        });
    }
    Ok(Claim {
        domain: "motion-complete-original-nfo-document-identity-tag-origin-absence-v1",
        catalog_id: archive.identity().catalog_id.clone(),
        incarnation: archive.identity().incarnation.clone(),
        root_import_id: archive.root_import_id().into(),
        root_manifest_digest: archive.root_manifest_digest().map(str::to_owned),
        archived_audit_digest: catabolic_core::sha256(raw.as_bytes()),
        ordered_parts,
        subjects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_activated_archive_proves_original_absence_and_refuses_existing_nfo() {
        for has_nfo in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("native.sqlite3");
            crate::initialize(path.clone()).await.unwrap();
            let db = sqlx::sqlite::SqlitePoolOptions::new()
                .after_connect(|db, _| Box::pin(crate::projection::configure_connection(db)))
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename(root.path().join("motion.sqlite3"))
                        .create_if_missing(true),
                )
                .await
                .unwrap();
            sqlx::migrate!("../../migrations").run(&db).await.unwrap();
            sqlx::raw_sql("INSERT INTO items(id,title,kind) VALUES('work','Title','video'); INSERT INTO item_origins VALUES('work','Title');")
                .execute(&db).await.unwrap();
            if has_nfo {
                // Current Motion cutover admits this document/tag cohort only
                // with NULL external identity; do not bypass its exporter guard.
                sqlx::query("INSERT INTO metadata_documents VALUES('work','nfo',1,NULL,?,1)")
                    .bind(serde_json::json!({"values":{"title":"NFO"},"tags":["drama"],"excluded_tags":[]}).to_string())
                    .execute(&db).await.unwrap();
            }
            let backend = crate::NativeCatalog::bind(
                db.clone(),
                path.clone(),
                crate::inspect(path.clone()).await.unwrap().identity,
            )
            .await
            .unwrap();
            backend.freeze_motion_source("absence", 0).await.unwrap();
            backend.capture_motion_import("absence").await.unwrap();
            backend.stage_import("absence").await.unwrap();
            backend.prepare_motion_cutover("absence").await.unwrap();
            backend
                .activate_motion_cutover(std::time::Duration::from_secs(30))
                .await
                .unwrap();
            drop(backend);
            let mut server =
                crate::cutover::open_server(db.clone(), path.clone(), "default".into())
                    .await
                    .unwrap();
            let host = server.admission.admitted_host().unwrap();
            let generation = host.catalog_generation().unwrap();
            let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let subjects = BTreeSet::from(["work".to_owned()]);
            let result = prepare_in(&mut tx, host, generation, &subjects).await;
            let mut offline_evidence = None;
            if has_nfo {
                assert!(result.is_err(), "existing original NFO is not absence");
                let origins = super::super::imported_origins::prepare_in(
                    &mut tx, host, generation, &subjects,
                )
                .await
                .unwrap();
                assert_eq!(origins.subjects().len(), 1);
                assert_eq!(origins.subjects()["work"].original_item_id(), "work");
                assert!(matches!(
                    origins.subjects()["work"].origin(),
                    super::super::imported_origins::ArchivedOrigin::AuditedAbsent
                ));
                assert_eq!(origins.digest().len(), 64);
                let references = super::super::source_history_in(&mut tx, host, generation)
                    .await
                    .unwrap();
                let evidence = super::super::offline_evidence::ImportedBootstrapEvidence::assemble(
                    origins, references,
                )
                .unwrap();
                assert_eq!(evidence.source_generation(), generation);
                assert_eq!(evidence.views().unwrap().len(), 1);
                offline_evidence = Some(evidence);
                assert!(
                    super::super::imported_origins::prepare_in(
                        &mut tx,
                        host,
                        generation + 1,
                        &subjects,
                    )
                    .await
                    .is_err()
                );
                assert!(
                    super::super::imported_origins::prepare_in(
                        &mut tx,
                        host,
                        generation,
                        &BTreeSet::from(["unknown".into()]),
                    )
                    .await
                    .is_err()
                );
            } else {
                assert!(
                    super::super::imported_origins::prepare_in(
                        &mut tx, host, generation, &subjects,
                    )
                    .await
                    .is_err(),
                    "first-source absence is not an existing NFO origin proof"
                );
                let result = result.unwrap();
                assert_eq!(
                    result.subjects(),
                    &BTreeMap::from([("work".into(), "work".into())])
                );
                assert_eq!(result.root_import_id(), "absence");
                assert_eq!(result.digest().len(), 64);
                assert!(
                    prepare_in(
                        &mut tx,
                        host,
                        generation,
                        &BTreeSet::from(["unknown".into()])
                    )
                    .await
                    .is_err()
                );
                assert!(
                    prepare_in(&mut tx, host, generation + 1, &subjects)
                        .await
                        .is_err()
                );
                assert!(
                    prepare_in(&mut tx, host, generation, &BTreeSet::new())
                        .await
                        .is_err()
                );
            }
            tx.rollback().await.unwrap();
            server.admission.admitted_host().unwrap().close().unwrap();
            drop(server);
            if let Some(evidence) = offline_evidence {
                // Install only the exact private operator resources for this
                // consumed-owner fixture. This does not qualify ordered Motion
                // control migration94–96 or ordinary native41 startup.
                sqlx::raw_sql(include_str!("../paired_restore/schema95.sql"))
                    .execute(&db)
                    .await
                    .unwrap();
                sqlx::raw_sql(include_str!("../native_bootstrap/schema96.sql"))
                    .execute(&db)
                    .await
                    .unwrap();
                let quiescence = catabolic_store::migration::NativeUpgradeQuiescence {
                    admission_stopped: true,
                    readers_and_workers_stopped: true,
                    exclusive_operational_ownership: true,
                };
                let prepared = crate::native_bootstrap::PreparedTransition::prepare(
                    &root.path().join("motion.sqlite3"),
                    &path,
                    quiescence,
                    "bootstrap-transition",
                )
                .unwrap();
                drop(prepared);
                let mut prepared =
                    crate::native_bootstrap::PreparedTransition::recover_before_migration(
                        &root.path().join("motion.sqlite3"),
                        &path,
                        quiescence,
                        "bootstrap-transition",
                    )
                    .unwrap();
                let assert_writer_retained = || {
                    let competing = rusqlite::Connection::open_with_flags(
                        root.path().join("motion.sqlite3"),
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
                    )
                    .unwrap();
                    competing.busy_timeout(std::time::Duration::ZERO).unwrap();
                    let error = competing.execute_batch("BEGIN IMMEDIATE").unwrap_err();
                    assert!(matches!(
                        error.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        )
                    ));
                    assert!(competing.is_autocommit());
                };
                {
                    let mut auth = prepared.authorization_reader().await.unwrap();
                    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
                        .fetch_one(auth.authorize_connection().await.unwrap())
                        .await
                        .unwrap();
                    assert!(
                        sqlx::query("ROLLBACK")
                            .execute(auth.authorize_connection().await.unwrap())
                            .await
                            .is_err()
                    );
                    auth.verify().await.unwrap();
                    assert_writer_retained();
                }
                // Dropping the readonly authorization borrow must not release
                // or commit the phase's WRITE before the real migration.
                assert_writer_retained();
                let mut pending = prepared.apply(None).unwrap();
                {
                    let mut auth = pending.authorization_reader().await.unwrap();
                    auth.verify().await.unwrap();
                }
                assert_writer_retained();
                let backup = pending.verified_backup_path().unwrap().to_owned();
                let views = evidence.views().unwrap();
                let pending = pending.save_command("bootstrap-command", &views).unwrap();
                drop(pending);
                // A durable saved bootstrap command cannot be reinterpreted as
                // a prepared40 phase to obtain pre-migration authorization.
                assert!(
                    crate::native_bootstrap::PreparedTransition::recover_before_migration(
                        &root.path().join("motion.sqlite3"),
                        &path,
                        quiescence,
                        "bootstrap-transition",
                    )
                    .is_err()
                );
                assert!(
                    crate::native_bootstrap::PendingTransition::recover_pending(
                        &root.path().join("motion.sqlite3"),
                        &path,
                        quiescence,
                        "bootstrap-transition",
                        &root.path().join("missing-backup"),
                    )
                    .is_err()
                );
                let mut pending = crate::native_bootstrap::PendingTransition::recover_pending(
                    &root.path().join("motion.sqlite3"),
                    &path,
                    quiescence,
                    "bootstrap-transition",
                    &backup,
                )
                .unwrap();
                {
                    let mut auth = pending.authorization_reader().await.unwrap();
                    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
                        .fetch_one(auth.authorize_connection().await.unwrap())
                        .await
                        .unwrap();
                    assert!(
                        sqlx::query("COMMIT")
                            .execute(auth.authorize_connection().await.unwrap())
                            .await
                            .is_err()
                    );
                    auth.verify().await.unwrap();
                }
                assert_writer_retained();
                let executed = pending
                    .execute_saved(
                        &views,
                        catabolic_store::catalog_commands::ReceiptPolicy {
                            retention: std::time::Duration::from_secs(3600),
                            max_receipts: 10000,
                            max_logical_bytes: 32 * 1024 * 1024,
                            max_hold_records: 10000,
                        },
                    )
                    .unwrap();
                // Native completion is not a Motion ACK. An interrupted or
                // denied final authorization must leave only the saved receipt.
                let assert_no_ack = || {
                    let reader = rusqlite::Connection::open_with_flags(
                        root.path().join("motion.sqlite3"),
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )
                    .unwrap();
                    let count: i64 = reader
                        .query_row(
                            "SELECT count(*) FROM catabolic_native_bootstrap_acks",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert_eq!(count, 0);
                };
                assert_no_ack();
                drop(executed);
                let mut recovered_execution =
                    crate::native_bootstrap::ExecutedTransition::recover_completed(
                        &root.path().join("motion.sqlite3"),
                        &path,
                        quiescence,
                        "bootstrap-transition",
                    )
                    .unwrap();
                assert_no_ack();
                {
                    let mut auth = recovered_execution.authorization_reader().await.unwrap();
                    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
                        .fetch_one(auth.authorize_connection().await.unwrap())
                        .await
                        .unwrap();
                    for forbidden in [
                        "DELETE FROM profiles",
                        "COMMIT",
                        "ROLLBACK",
                        "PRAGMA query_only=OFF",
                        "ATTACH ':memory:' AS escaped",
                    ] {
                        assert!(
                            sqlx::query(forbidden)
                                .execute(auth.authorize_connection().await.unwrap())
                                .await
                                .is_err()
                        );
                        auth.verify().await.unwrap();
                    }
                    // No ACK is available while the target authorization borrow
                    // exists. The application still evaluates actual Caller policy.
                    assert_no_ack();
                }
                let acknowledged = recovered_execution.acknowledge().unwrap();
                assert_eq!(acknowledged.identity().schema_version, 41);
                assert_eq!(acknowledged.transition_id(), "bootstrap-transition");
                let digest = acknowledged.acknowledgment_digest().to_owned();
                drop(acknowledged);
                let recovered =
                    crate::native_bootstrap::AcknowledgedNativeBootstrap::recover_completed(
                        &root.path().join("motion.sqlite3"),
                        &path,
                        quiescence,
                        "bootstrap-transition",
                    )
                    .unwrap();
                assert_eq!(recovered.acknowledgment_digest(), digest);
                drop(recovered);
            }
            db.close().await;
        }
    }
}
