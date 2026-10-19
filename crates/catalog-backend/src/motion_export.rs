//! A consistent, bounded export of Motion catalog schema 68 with host migrations through 69. This captures observations;
//! it does not freeze writers after commit or assert a completed reference audit.
use crate::{
    HOST_NAMESPACE, NativeCatalog,
    migration::{Snapshot, save_snapshot},
};
use anyhow::{Context, ensure};
use catabolic_core::catalog_metadata::SourceKind;
use catabolic_store::{
    catalog_identity_mutation::{
        ContextChange, IdentityAuthority, IdentityMutation, TimelineChange, TimelineState,
    },
    catalog_import::{
        self, EvidenceKind, ImportBatch, ImportItem, ImportReceipt, SnapshotManifest,
        UnresolvedEvidence,
    },
    catalog_import_typed::{
        ImportedContribution, ImportedMetadata, ImportedPin, ImportedPinRevision,
        ImportedSourceKind, NativeIdentityBatch, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{MetadataAccess, MetadataAuthority},
    catalog_pins::{PinAuthority, PinDecision},
};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

const SOURCE: &str = "motion.schema68.export.v3";
const PIN_SOURCE: &str = "motion.schema68.export.v2";
const PREVIOUS_SOURCE: &str = "motion.schema68.export.v1";
// Dedicated catalog facts not yet represented by a qualified typed mapping.
// Artifact bytes remain Motion-owned; their catalog references are retained here.
const EVIDENCE_TABLES: &[(&str, EvidenceKind)] = &[
    ("libraries", EvidenceKind::PhysicalOccurrence),
    ("catalog_libraries", EvidenceKind::ApplicationReference),
    ("library_sources", EvidenceKind::ApplicationReference),
    ("media_files", EvidenceKind::PhysicalOccurrence),
    ("sidecar_subtitles", EvidenceKind::PhysicalOccurrence),
    (
        "scan_incomplete_directories",
        EvidenceKind::PhysicalOccurrence,
    ),
    ("editions", EvidenceKind::TimelineVersion),
    ("timelines", EvidenceKind::TimelineVersion),
    ("media_versions", EvidenceKind::TimelineVersion),
    ("version_files", EvidenceKind::TimelineVersion),
    ("item_structure", EvidenceKind::AcceptedDecision),
    ("item_relationships", EvidenceKind::AcceptedDecision),
    ("order_groups", EvidenceKind::AcceptedDecision),
    ("item_aliases", EvidenceKind::AcceptedDecision),
    ("match_proposals", EvidenceKind::AcceptedDecision),
    ("catalog_receipts", EvidenceKind::AcceptedDecision),
    ("artwork_contributions", EvidenceKind::Contribution),
    ("artwork_selections", EvidenceKind::Contribution),
    ("item_artwork_assets", EvidenceKind::Contribution),
    ("nfo_origins", EvidenceKind::Contribution),
];

impl NativeCatalog {
    /// Capture once from this host's Motion database. Replay reads the retained
    /// export; it never recaptures newer source rows under the same import ID.
    pub async fn capture_motion_import(&self, import_id: &str) -> anyhow::Result<SnapshotManifest> {
        self.status().await?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let recorded: Option<String> = sqlx::query_scalar(
            "SELECT input_digest FROM catabolic_motion_exports WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&mut *tx)
        .await?;
        if recorded.is_some() {
            tx.rollback().await?;
            let (snapshot, digest) = self.import_snapshot(import_id).await?;
            ensure!(
                recorded.as_deref() == Some(&digest),
                "corrupt Motion export provenance"
            );
            return Ok(snapshot.manifest);
        }
        let version: i64 =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            version == 69,
            "Motion export requires host schema 69; observed {version}"
        );
        let freeze_revision = crate::source_freeze::capture_freeze(&mut tx, import_id).await?;
        let snapshot = capture(&mut tx, import_id, freeze_revision).await?;
        let digest = snapshot.validate()?;
        save_snapshot(&mut tx, &snapshot, &digest).await?;
        sqlx::query("INSERT INTO catabolic_motion_exports(import_id,input_digest,format_version) VALUES(?,?,1)")
            .bind(import_id).bind(digest).execute(&mut *tx).await?;
        if let Some(revision) = freeze_revision {
            sqlx::query(
                "INSERT INTO catabolic_export_freezes(import_id,freeze_revision) VALUES(?,?)",
            )
            .bind(import_id)
            .bind(revision as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(snapshot.manifest)
    }

    /// Grants are derived only from an immutable export captured from Motion's
    /// own tables in the same transaction as its provenance marker, never from
    /// a caller-supplied snapshot claiming to be a Motion export.
    pub async fn validate_motion_import(&self, import_id: &str) -> anyhow::Result<ImportReceipt> {
        let (snapshot, digest) = self.import_snapshot(import_id).await?;
        let provenance: Option<(String, i64)> = sqlx::query_as(
            "SELECT input_digest,format_version FROM catabolic_motion_exports WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&self.journal)
        .await?;
        ensure!(
            matches!(
                snapshot.manifest.source_namespace.as_str(),
                SOURCE | PIN_SOURCE | PREVIOUS_SOURCE
            ),
            "unsupported Motion export mapper"
        );
        let admitted = provenance.is_some_and(|(input_digest, format_version)| {
            playscale_core::catalog_backend::ExportProvenance {
                input_digest,
                source_namespace: snapshot.manifest.source_namespace.clone(),
                format_version,
            }
            .admits(&digest, &snapshot.manifest.source_namespace)
        });
        ensure!(
            admitted,
            "typed Motion grants require a trusted source export"
        );
        let mut grants = TypedAuthority::default();
        for batch in snapshot.batches {
            if let Some(typed) = batch.typed {
                if let Some(native) = &typed.native {
                    for raw in &native.mutations {
                        let mutation: IdentityMutation = serde_json::from_value(raw.clone())?;
                        ensure!(
                            mutation.context.authority_namespace == HOST_NAMESPACE,
                            "unexpected Motion editorial authority"
                        );
                        grants
                            .identity
                            .get_or_insert_with(|| IdentityAuthority {
                                authority_namespace: HOST_NAMESPACE.into(),
                                context_keys: BTreeSet::new(),
                                allow_declared_evidence: false,
                                allow_verified_evidence: false,
                            })
                            .context_keys
                            .insert(mutation.context.context_key);
                    }
                }
                for pin in &typed.pins {
                    grants
                        .pins
                        .get_or_insert_with(|| PinAuthority {
                            host_namespace: HOST_NAMESPACE.into(),
                            readable: BTreeMap::new(),
                            mutable: BTreeMap::new(),
                        })
                        .mutable
                        .entry(pin.namespace.clone())
                        .or_default()
                        .insert(pin.item_id.clone());
                }
                for metadata in typed.metadata {
                    grants.metadata.push(MetadataAuthority {
                        host_namespace: HOST_NAMESPACE.into(),
                        subject: metadata.item_id.clone(),
                        access: MetadataAccess::Initialize,
                    });
                    for source in metadata.contributions {
                        grants.metadata.push(MetadataAuthority {
                            host_namespace: HOST_NAMESPACE.into(),
                            subject: metadata.item_id.clone(),
                            access: MetadataAccess::Source {
                                namespace: source.namespace,
                                record_id: source.record_id,
                                kind: match source.kind {
                                    ImportedSourceKind::Local => SourceKind::Local,
                                    ImportedSourceKind::Provider => SourceKind::Provider,
                                    ImportedSourceKind::Scan => SourceKind::Scan,
                                },
                            },
                        });
                    }
                }
            }
        }
        self.validate_import(import_id, Some(grants)).await
    }
}

fn empty() -> ImportBatch {
    ImportBatch {
        items: vec![],
        unresolved_evidence: vec![],
        typed: None,
    }
}
fn append(batches: &mut Vec<ImportBatch>, unit: ImportBatch) -> anyhow::Result<()> {
    catalog_import::describe_batch(&unit)?;
    if let Some(last) = batches.last_mut() {
        let mut joined = last.clone();
        joined.items.extend(unit.items.clone());
        joined
            .unresolved_evidence
            .extend(unit.unresolved_evidence.clone());
        if let Some(typed) = &unit.typed {
            let target = joined.typed.get_or_insert_with(TypedBatch::default);
            target.metadata.extend(typed.metadata.clone());
            target.pins.extend(typed.pins.clone());
            if let Some(native) = &typed.native {
                let target = target.native.get_or_insert_with(Default::default);
                target.occurrences.extend(native.occurrences.clone());
                target.associations.extend(native.associations.clone());
                target.mutations.extend(native.mutations.clone());
            }
        }
        if catalog_import::describe_batch(&joined).is_ok() {
            *last = joined;
            return Ok(());
        }
    }
    ensure!(
        batches.len() < catalog_import::MAX_IMPORT_BATCHES,
        "Motion export exceeds the native import batch bound; no source rows were discarded"
    );
    batches.push(unit);
    Ok(())
}
fn evidence(
    batches: &mut Vec<ImportBatch>,
    id: String,
    kind: EvidenceKind,
    data: Value,
) -> anyhow::Result<()> {
    append(
        batches,
        ImportBatch {
            unresolved_evidence: vec![UnresolvedEvidence {
                evidence_id: id,
                kind,
                data,
            }],
            ..empty()
        },
    )
}

async fn capture(
    db: &mut SqliteConnection,
    import_id: &str,
    freeze_revision: Option<u64>,
) -> anyhow::Result<Snapshot> {
    let mut batches = Vec::new();
    let items = rows(db, "items", None).await?;
    for item in items {
        let id = item["id"].as_str().context("invalid Motion item ID")?;
        let origin: String = sqlx::query_scalar(
            "SELECT title FROM item_origins WHERE item_id=? AND length(CAST(title AS BLOB))<=60000",
        )
        .bind(id)
        .fetch_one(&mut *db)
        .await?;
        let oversized: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metadata_documents WHERE item_id=? AND (length(CAST(document_json AS BLOB))>60000 OR length(CAST(source AS BLOB))>256 OR length(CAST(external_id AS BLOB))>256))")
            .bind(id).fetch_one(&mut *db).await?;
        ensure!(
            !oversized,
            "Motion metadata input exceeds the bounded import representation"
        );
        let documents: Vec<(String,i64,Option<String>,String,i64)> = sqlx::query_as("SELECT source,revision,external_id,document_json,updated_at FROM metadata_documents WHERE item_id=? ORDER BY source LIMIT 129")
            .bind(id).fetch_all(&mut *db).await?;
        ensure!(
            documents.len() <= 127,
            "too many Motion contributions for {id}"
        );
        let mut sources = vec![ImportedContribution {
            namespace: "motion.scan".into(),
            record_id: "origin".into(),
            source_revision: 1,
            kind: ImportedSourceKind::Scan,
            fields: json!({"title":origin}),
            evidence: json!({"source_table":"item_origins","revision_basis":"first export; source has no revision column"}),
        }];
        let mut fields = BTreeSet::from(["title".to_owned()]);
        let mut pending = vec![];
        let mut pins = vec![];
        let state = serde_json::from_value(item["match_state"].clone())?;
        let retired: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM item_aliases WHERE alias_id=?)")
                .bind(id)
                .fetch_one(&mut *db)
                .await?;
        for (source, revision, external_id, document, updated_at) in documents {
            let doc: playscale_core::metadata::Contribution = serde_json::from_str(&document)?;
            ensure!(
                source != "scan",
                "reserved scan contribution in Motion source"
            );
            fields.extend(doc.values.keys().cloned());
            let identity = external_id
                .as_ref()
                .map(|value| (source.clone(), value.clone()));
            if playscale_core::catalog_backend::pinned_source_identity(
                state,
                retired,
                identity.as_ref(),
            ) {
                pins.push(ImportedPin {
                    item_id: id.into(),
                    namespace: source.clone(),
                    history: vec![ImportedPinRevision {
                        source_revision: revision,
                        decision: PinDecision::Set {
                            value: external_id.clone().expect("pinned identity exists"),
                        },
                    }],
                });
            }
            if !doc.tags.is_empty() || !doc.excluded_tags.is_empty() || external_id.is_some() {
                pending.push((source.clone(),json!({"item_id":id,"source":source,"revision":revision,"external_id":external_id,"tags":doc.tags,"excluded_tags":doc.excluded_tags})));
            }
            sources.push(ImportedContribution {
                namespace: format!("motion.{source}"), record_id: "document".into(), source_revision: revision,
                kind: if source=="local" { ImportedSourceKind::Local } else { ImportedSourceKind::Provider },
                fields: serde_json::to_value(&doc.values)?,
                evidence: json!({"source_table":"metadata_documents","source":source,"external_id":external_id,"updated_at":updated_at}),
            });
        }
        let policies: Vec<Value> = fields.into_iter().map(|field|json!({"field":field,"value_type":"json","nullable":false,"normalization":"exact"})).collect();
        append(
            &mut batches,
            ImportBatch {
                items: vec![ImportItem {
                    source_id: id.into(),
                    item_id: id.into(),
                    kind: item["kind"].as_str().context("invalid Motion kind")?.into(),
                    legacy_metadata: json!({}),
                }],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        item_id: id.into(),
                        policies: json!(policies),
                        contributions: sources,
                    }],
                    pins,
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
        // Preserve cached title, revision and matching facts without upgrading
        // them into a metadata contribution or a reviewed identity decision.
        evidence(
            &mut batches,
            format!("item:{id}"),
            EvidenceKind::AcceptedDecision,
            json!({"table":"items","row":item}),
        )?;
        for (source, data) in pending {
            evidence(
                &mut batches,
                format!("contribution:{id}:{source}"),
                EvidenceKind::Contribution,
                data,
            )?;
        }
    }
    capture_structure(db, &mut batches).await?;
    for &(table, kind) in EVIDENCE_TABLES {
        for (index, row) in rows(db, table, None).await?.into_iter().enumerate() {
            evidence(
                &mut batches,
                format!("{table}:{index}"),
                kind,
                json!({"table":table,"row":row}),
            )?;
        }
    }
    // Inventory direct relational references without copying credentials,
    // tokens, or unrelated application payloads. This is evidence for the later
    // reference audit, not an assertion that JSON-encoded references were found.
    let targets = BTreeSet::from([
        "items",
        "editions",
        "timelines",
        "media_versions",
        "media_files",
        "libraries",
    ]);
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut *db).await?;
    for table in tables {
        if table.starts_with("catabolic_")
            || EVIDENCE_TABLES.iter().any(|(name, _)| *name == table)
            || matches!(
                table.as_str(),
                "items" | "item_origins" | "metadata_documents"
            )
        {
            continue;
        }
        let fks = sqlx::query(&format!("PRAGMA foreign_key_list({})", quote(&table)))
            .fetch_all(&mut *db)
            .await?;
        let mut columns = BTreeSet::new();
        for fk in fks {
            if targets.contains(fk.get::<&str, _>("table")) {
                columns.insert(fk.get::<String, _>("from"));
            }
        }
        if columns.is_empty() {
            continue;
        }
        let info = sqlx::query(&format!("PRAGMA table_info({})", quote(&table)))
            .fetch_all(&mut *db)
            .await?;
        for column in info {
            if column.get::<i64, _>("pk") > 0 {
                columns.insert(column.get::<String, _>("name"));
            }
        }
        for (index, row) in rows(db, &table, Some(&columns))
            .await?
            .into_iter()
            .enumerate()
        {
            evidence(
                &mut batches,
                format!("reference:{table}:{index}"),
                EvidenceKind::ApplicationReference,
                json!({"table":table,"reference_columns":row}),
            )?;
        }
    }
    // Empty catalogs still need an explicit reference audit before activation.
    evidence(
        &mut batches,
        "motion.reference-audit".into(),
        EvidenceKind::ApplicationReference,
        json!({"relational_references":"captured","embedded_references":"not_audited","writer_freeze":if freeze_revision.is_some(){"retained"}else{"not_established"},"freeze_revision":freeze_revision}),
    )?;
    let snapshot = Snapshot {
        manifest: SnapshotManifest {
            contract_version: catalog_import::TYPED_IMPORT_CONTRACT_VERSION,
            import_id: import_id.into(),
            source_namespace: SOURCE.into(),
            source_snapshot_id: uuid::Uuid::new_v4().to_string(),
            batches: batches
                .iter()
                .map(catalog_import::describe_batch)
                .collect::<Result<_, _>>()?,
        },
        batches,
    };
    snapshot.validate()?;
    Ok(snapshot)
}
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
async fn rows(
    db: &mut SqliteConnection,
    table: &str,
    selected: Option<&BTreeSet<String>>,
) -> anyhow::Result<Vec<Value>> {
    let columns = sqlx::query(&format!("PRAGMA table_info({})", quote(table)))
        .fetch_all(&mut *db)
        .await?;
    ensure!(!columns.is_empty(), "missing Motion source table {table}");
    let names: Vec<String> = columns
        .iter()
        .map(|c| c.get::<String, _>("name"))
        .filter(|name| selected.is_none_or(|s| s.contains(name)))
        .collect();
    ensure!(!names.is_empty(), "empty export projection");
    // Bound values before asking SQLite/SQLx to materialize JSON documents.
    let sizes = names
        .iter()
        .map(|n| format!("coalesce(length(CAST({} AS BLOB)),0)", quote(n)))
        .collect::<Vec<_>>()
        .join("+");
    let oversized: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM {} WHERE {sizes} > 60000)",
        quote(table)
    ))
    .fetch_one(&mut *db)
    .await?;
    ensure!(
        !oversized,
        "Motion source row in {table} exceeds the bounded import representation"
    );
    let args = names
        .iter()
        .map(|n| format!("'{}',{}", n.replace('\'', "''"), quote(n)))
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT json_object({args}) FROM {} ORDER BY rowid LIMIT 1025",
        quote(table)
    );
    let values: Vec<String> = sqlx::query_scalar(&query).fetch_all(&mut *db).await?;
    ensure!(
        values.len() <= 1024,
        "Motion source table {table} exceeds the bounded import representation"
    );
    values
        .into_iter()
        .map(|v| Ok(serde_json::from_str(&v)?))
        .collect()
}

/// Editions live in a separate source keyspace, but are native item owners.
/// Keep all source rows as evidence until relationships, versions and references
/// are qualified. A duration observation alone does not establish equivalence.
async fn capture_structure(
    db: &mut SqliteConnection,
    batches: &mut Vec<ImportBatch>,
) -> anyhow::Result<()> {
    let editions = rows(db, "editions", None).await?;
    let timelines = rows(db, "timelines", None).await?;
    let mut owners = BTreeMap::new();
    let mut contexts: BTreeMap<String, Vec<TimelineChange>> = BTreeMap::new();
    for edition in editions {
        let source = edition["id"].as_str().context("invalid edition ID")?;
        let work = edition["item_id"]
            .as_str()
            .context("invalid edition work")?;
        let destination = format!("motion.edition:{source}");
        ensure!(
            !batches
                .iter()
                .flat_map(|b| &b.items)
                .any(|i| i.item_id == destination || i.source_id == destination),
            "Motion edition key collides with a source item"
        );
        owners.insert(source.to_owned(), (work.to_owned(), destination.clone()));
        append(
            batches,
            ImportBatch {
                items: vec![ImportItem {
                    source_id: destination.clone(),
                    item_id: destination,
                    kind: "custom:motion_edition".into(),
                    legacy_metadata: json!({}),
                }],
                ..empty()
            },
        )?;
    }
    for timeline in timelines {
        let edition = timeline["edition_id"]
            .as_str()
            .context("invalid timeline edition")?;
        let (work, owner) = owners
            .get(edition)
            .context("timeline edition absent from captured source")?;
        contexts
            .entry(work.clone())
            .or_default()
            .push(TimelineChange {
                timeline_id: timeline["id"]
                    .as_str()
                    .context("invalid timeline ID")?
                    .into(),
                edition_item_id: owner.clone(),
                expected_revision: None,
                identity_state: TimelineState::Provisional,
                coverage: None,
            });
    }
    for (work, timelines) in contexts {
        ensure!(
            timelines.len() <= 64,
            "Motion work exceeds native timeline mutation bound"
        );
        let mutation = IdentityMutation {
            contract_version: 1,
            profile: "default".into(),
            context: ContextChange {
                id: format!("motion.work:{work}"),
                authority_namespace: HOST_NAMESPACE.into(),
                context_key: work,
                expected_revision: None,
            },
            timelines,
            versions: vec![],
            evidence: vec![],
        };
        append(
            batches,
            ImportBatch {
                typed: Some(TypedBatch {
                    native: Some(NativeIdentityBatch {
                        mutations: vec![serde_json::to_value(mutation)?],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }
    Ok(())
}
