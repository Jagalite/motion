//! Consistent source capture and bounded native imports. Source provenance, exact
//! owner mappings, reference obligations and writer freezes remain distinct proofs.
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
        ImportedContribution, ImportedLogicalAlias, ImportedMetadata, ImportedPin,
        ImportedPinRevision, ImportedRelationship, ImportedSourceKind, ImportedTagSet,
        LogicalAliasImportAuthority, NativeIdentityBatch, RelationshipImportAuthority,
        TagImportAuthority, TypedAuthority, TypedBatch,
    },
    catalog_metadata_store::{MetadataAccess, MetadataAuthority},
    catalog_pins::{PinAuthority, PinDecision},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

const SOURCE: &str = "motion.schema68.export.v4";
const STRUCTURE_SOURCE: &str = "motion.schema68.export.v3";
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

/// A complete, ordered set of bounded native imports from one frozen source.
/// A partition receipt is not an acknowledgment for the aggregate catalog.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MotionExportPlan {
    pub contract_version: u32,
    pub root_import_id: String,
    pub source_snapshot_id: String,
    pub source_namespace: String,
    pub audit_digest: String,
    pub partitions: Vec<MotionExportPartition>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MotionExportPartition {
    pub index: u32,
    pub import_id: String,
    pub snapshot_digest: String,
    /// Earlier partitions must be committed and independently reconciled before
    /// existing endpoint grants can be obtained for this partition.
    pub dependency_indices: Vec<u32>,
}
/// Immutable source evidence for the import-ack adapter. This is not an access
/// grant: callers must separately reconcile activation and recheck the native
/// source binding/policy under their retained owner before acknowledging it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrozenMotionSource {
    pub import_id: String,
    pub source_snapshot_id: String,
    pub audit_digest: String,
    pub source_row: Value,
    pub mapping: crate::sources::SourceMapping,
    pub logical_library_ids: Vec<String>,
}

pub(crate) fn validate_trusted_snapshot(
    import_id: &str,
    digest: String,
    text: &str,
    provenance: Option<(String, i64)>,
) -> anyhow::Result<(Snapshot, String)> {
    let snapshot: Snapshot = serde_json::from_str(text)?;
    ensure!(
        snapshot.manifest.import_id == import_id && snapshot.validate()? == digest,
        "corrupt import snapshot"
    );
    ensure!(
        matches!(
            snapshot.manifest.source_namespace.as_str(),
            SOURCE | STRUCTURE_SOURCE | PIN_SOURCE | PREVIOUS_SOURCE
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
    Ok((snapshot, digest))
}
pub(crate) async fn trusted_snapshot_in(
    connection: &mut sqlx::SqliteConnection,
    import_id: &str,
) -> anyhow::Result<(Snapshot, String)> {
    let (digest, text): (String, String) = sqlx::query_as(
        "SELECT input_digest,snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",
    )
    .bind(import_id)
    .fetch_optional(&mut *connection)
    .await?
    .context("unknown import snapshot")?;
    let provenance: Option<(String, i64)> = sqlx::query_as(
        "SELECT input_digest,format_version FROM catabolic_motion_exports WHERE import_id=?",
    )
    .bind(import_id)
    .fetch_optional(&mut *connection)
    .await?;
    validate_trusted_snapshot(import_id, digest, &text, provenance)
}
async fn trusted_snapshot_from_journal(
    journal: &sqlx::SqlitePool,
    import_id: &str,
) -> anyhow::Result<(Snapshot, String)> {
    let mut connection = journal.acquire().await?;
    trusted_snapshot_in(&mut connection, import_id).await
}

/// Journal-only source proof loader. Does not open a native Store, acquire a
/// native owner, or issue any command/physical grant. The caller must reconcile
/// activation and recheck bindings through its already-retained native owner.
pub async fn frozen_motion_sources_from_journal(
    journal: &sqlx::SqlitePool,
    identity: &playscale_core::catalog_backend::Identity,
    import_id: &str,
) -> anyhow::Result<Vec<FrozenMotionSource>> {
    let mut connection = journal.acquire().await?;
    frozen_motion_sources_in(&mut connection, identity, import_id).await
}

/// Validate through the caller's current transaction/connection, without taking
/// another pool connection or opening any native owner.
pub async fn frozen_motion_sources_in(
    connection: &mut sqlx::SqliteConnection,
    identity: &playscale_core::catalog_backend::Identity,
    import_id: &str,
) -> anyhow::Result<Vec<FrozenMotionSource>> {
    let (snapshot, digest) = trusted_snapshot_in(connection, import_id).await?;
    let (input_digest, raw): (String, String) = sqlx::query_as(
        "SELECT input_digest,audit_json FROM catabolic_reference_audits WHERE import_id=?",
    )
    .bind(import_id)
    .fetch_one(&mut *connection)
    .await?;
    let mappings: Vec<(String, String)> =
        sqlx::query_as("SELECT library_id,mapping_json FROM catabolic_source_mappings")
            .fetch_all(&mut *connection)
            .await?;
    let origin_audit: crate::reference_inventory::SourceAudit = serde_json::from_str(&raw)?;
    let origin_proof =
        crate::origin_preservation::verified_in(connection, &snapshot, &origin_audit).await?;
    validate_frozen_sources(
        identity,
        import_id,
        &snapshot,
        digest,
        input_digest,
        &raw,
        mappings.into_iter().collect(),
        &origin_audit,
        origin_proof.as_ref(),
    )
}

/// Synchronous equivalent for isolated compatibility reconstruction. All trust
/// and source parsing is shared with the async path; this opens no native owner.
pub fn frozen_motion_sources_from_connection(
    connection: &rusqlite::Connection,
    identity: &playscale_core::catalog_backend::Identity,
    import_id: &str,
) -> anyhow::Result<Vec<FrozenMotionSource>> {
    use rusqlite::OptionalExtension;
    let (digest, text): (String, String) = connection
        .query_row(
            "SELECT input_digest,snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",
            [import_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .context("unknown import snapshot")?;
    let provenance: Option<(String, i64)> = connection
        .query_row(
            "SELECT input_digest,format_version FROM catabolic_motion_exports WHERE import_id=?",
            [import_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (snapshot, digest) = validate_trusted_snapshot(import_id, digest, &text, provenance)?;
    let (input_digest, raw): (String, String) = connection.query_row(
        "SELECT input_digest,audit_json FROM catabolic_reference_audits WHERE import_id=?",
        [import_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement =
        connection.prepare("SELECT library_id,mapping_json FROM catabolic_source_mappings")?;
    let mappings = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let origin_audit: crate::reference_inventory::SourceAudit = serde_json::from_str(&raw)?;
    let origin_proof =
        crate::origin_preservation::verified_sync(connection, &snapshot, &origin_audit)?;
    validate_frozen_sources(
        identity,
        import_id,
        &snapshot,
        digest,
        input_digest,
        &raw,
        mappings,
        &origin_audit,
        origin_proof.as_ref(),
    )
}

fn validate_frozen_sources(
    identity: &playscale_core::catalog_backend::Identity,
    import_id: &str,
    snapshot: &Snapshot,
    digest: String,
    input_digest: String,
    raw: &str,
    mut mappings: BTreeMap<String, String>,
    audit: &crate::reference_inventory::SourceAudit,
    origin_proof: Option<&crate::origin_preservation::VerifiedOriginPreservation<'_>>,
) -> anyhow::Result<Vec<FrozenMotionSource>> {
    ensure!(
        input_digest == digest,
        "source descriptor audit digest changed"
    );
    ensure!(
        audit.qualified_with_origins(origin_proof)
            && snapshot
                .batches
                .iter()
                .all(|batch| batch.unresolved_evidence.is_empty()),
        "source descriptor requires a complete frozen reference contract"
    );
    let audit_digest = catabolic_core::sha256(raw.as_bytes());
    if audit.table_rows.get("libraries") == Some(&0)
        && audit.table_rows.get("library_sources") == Some(&0)
        && audit
            .physical_coverage
            .preserved_ids
            .get("libraries")
            .is_some_and(BTreeSet::is_empty)
        && audit
            .physical_coverage
            .missing_ids
            .get("libraries")
            .is_some_and(BTreeSet::is_empty)
    {
        return Ok(Vec::new());
    }
    ensure!(
        matches!(
            audit.physical_coverage.source_reference_contract,
            Some(1 | 2)
        ),
        "source descriptor requires a complete frozen source reference contract"
    );
    let mut sources = BTreeMap::new();
    let mut memberships: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for record in &audit.resolved_evidence {
        let data = &record["data"];
        if data["table"] == "libraries" && data["row"].is_object() {
            let source = data["row"].clone();
            let id = source["id"]
                .as_str()
                .context("frozen source has invalid ID")?
                .to_owned();
            if !audit.physical_coverage.preserved_ids["libraries"].contains(&id) {
                continue;
            }
            ensure!(
                sources.insert(id, source).is_none(),
                "duplicate frozen source descriptor"
            );
        } else if data["table"] == "library_sources" && data["row"].is_object() {
            let row = &data["row"];
            let source = row["source_id"]
                .as_str()
                .context("invalid frozen membership source")?;
            if !audit.physical_coverage.preserved_ids["libraries"].contains(source) {
                continue;
            }
            let library = row["library_id"]
                .as_str()
                .context("invalid frozen logical library")?;
            ensure!(
                audit.physical_coverage.preserved_ids["catalog_libraries"].contains(library),
                "frozen logical library owner missing"
            );
            ensure!(
                memberships
                    .entry(source.into())
                    .or_default()
                    .insert(library.into()),
                "duplicate frozen library membership"
            );
        }
    }
    ensure!(
        sources.keys().cloned().collect::<BTreeSet<_>>()
            == audit.physical_coverage.preserved_ids["libraries"],
        "frozen source descriptor coverage incomplete"
    );
    let mut result = Vec::new();
    for (id, source_row) in sources {
        let raw_mapping = mappings
            .remove(&id)
            .context("missing frozen source mapping")?;
        let mapping: crate::sources::SourceMapping = serde_json::from_str(&raw_mapping)?;
        identity
            .admit(&mapping.catalog)
            .map_err(|error| anyhow::anyhow!("frozen source catalog mismatch: {error:?}"))?;
        let exclusions: Vec<String> = serde_json::from_str(
            source_row["exclusions_json"]
                .as_str()
                .context("frozen source exclusions missing")?,
        )?;
        let exclusions = playscale_core::sources::normalize_exclusions(&exclusions)
            .map_err(|error| anyhow::anyhow!("frozen source exclusions: {error:?}"))?;
        ensure!(
            mapping.library_id == id
                && source_row["root"] == mapping.motion_root
                && source_row["root_identity"] == mapping.motion_root_identity
                && source_row["binding_revision"].as_i64() == Some(mapping.motion_binding_revision)
                && mapping.exclusions == exclusions,
            "frozen source mapping changed"
        );
        result.push(FrozenMotionSource {
            import_id: import_id.into(),
            source_snapshot_id: snapshot.manifest.source_snapshot_id.clone(),
            audit_digest: audit_digest.clone(),
            source_row,
            mapping,
            logical_library_ids: memberships
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .collect(),
        });
    }
    ensure!(
        memberships.is_empty(),
        "frozen membership has no physical source descriptor"
    );
    Ok(result)
}

impl NativeCatalog {
    pub async fn frozen_motion_sources(
        &self,
        import_id: &str,
    ) -> anyhow::Result<Vec<FrozenMotionSource>> {
        frozen_motion_sources_from_journal(&self.journal, &self.identity, import_id).await
    }

    pub async fn capture_motion_export_plan(
        &self,
        root_import_id: &str,
    ) -> anyhow::Result<MotionExportPlan> {
        self.status().await?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let direct: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM catabolic_motion_exports WHERE import_id=?)",
        )
        .bind(root_import_id)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(!direct, "source root already captured as a single import");
        let retained: Option<String> = sqlx::query_scalar(
            "SELECT plan_json FROM catabolic_export_plans WHERE root_import_id=?",
        )
        .bind(root_import_id)
        .fetch_optional(&mut *tx)
        .await?;
        if retained.is_some() {
            tx.rollback().await?;
            return self.motion_export_plan(root_import_id).await;
        }
        // Reject invalid/oversized native identifiers before any source work.
        let probe = SnapshotManifest {
            contract_version: 3,
            import_id: root_import_id.into(),
            source_namespace: SOURCE.into(),
            source_snapshot_id: "probe".into(),
            batches: vec![],
        };
        catalog_import::manifest_digest(&probe)?;
        let version: i64 =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
                .fetch_one(&mut *tx)
                .await?;
        ensure!(version == 93, "partition export requires Motion schema 93");
        let freeze_revision = crate::source_freeze::capture_freeze(&mut tx, root_import_id)
            .await?
            .context("partition export requires a frozen source")?;
        let (source, audit) = capture(self, &mut tx, root_import_id, Some(freeze_revision)).await?;
        ensure!(
            audit.table_rows.get("nfo_origins") == Some(&0),
            "origin-bearing aggregate exports are not yet supported; use a bounded direct import"
        );
        let audit_json = serde_json::to_string(&audit)?;
        let audit_digest = catabolic_core::sha256(audit_json.as_bytes());
        let mut plan = MotionExportPlan {
            contract_version: 1,
            root_import_id: root_import_id.into(),
            source_snapshot_id: source.manifest.source_snapshot_id.clone(),
            source_namespace: SOURCE.into(),
            audit_digest,
            partitions: Vec::new(),
        };
        let root_key = catabolic_core::sha256(root_import_id.as_bytes());
        let mut snapshots = Vec::new();
        // An empty source still gets one real zero-batch v3 import.
        let ranges = bounded_partition_ranges(&source.batches, &audit.atomic_batch_groups)?;
        let chunks: Vec<&[ImportBatch]> = ranges
            .iter()
            .map(|&(start, end)| &source.batches[start..end])
            .collect();
        ensure!(
            chunks.len() <= 4096,
            "Motion export exceeds the aggregate restart contract partition bound; no source rows were discarded"
        );
        let mut owners = BTreeSet::new();
        let mut source_ids = BTreeSet::new();
        for (index, chunk) in chunks.into_iter().enumerate() {
            for item in chunk.iter().flat_map(|b| &b.items) {
                ensure!(
                    owners.insert(item.item_id.clone())
                        && source_ids.insert(item.source_id.clone()),
                    "duplicate source owner across export partitions"
                );
            }
            let import_id = format!("motion.plan:{root_key}:{index}");
            let snapshot = Snapshot {
                manifest: SnapshotManifest {
                    contract_version: source.manifest.contract_version,
                    import_id: import_id.clone(),
                    source_namespace: SOURCE.into(),
                    source_snapshot_id: source.manifest.source_snapshot_id.clone(),
                    batches: chunk
                        .iter()
                        .map(catalog_import::describe_batch)
                        .collect::<Result<_, _>>()?,
                },
                batches: chunk.to_vec(),
            };
            let snapshot_digest = snapshot.validate()?;
            let index = u32::try_from(index)?;
            // Conservative ordering is intentional: no item is duplicated and
            // authority must still be resolved from exact committed receipts.
            plan.partitions.push(MotionExportPartition {
                index,
                import_id,
                snapshot_digest,
                dependency_indices: (0..index).collect(),
            });
            snapshots.push(snapshot);
        }
        sqlx::query("INSERT INTO catabolic_export_plans(root_import_id,source_snapshot_id,source_namespace,audit_digest,plan_json) VALUES(?,?,?,?,?)")
            .bind(root_import_id).bind(&plan.source_snapshot_id).bind(SOURCE).bind(&plan.audit_digest).bind(serde_json::to_string(&plan)?).execute(&mut *tx).await?;
        for (part, snapshot) in plan.partitions.iter().zip(&snapshots) {
            save_snapshot(&mut tx, snapshot, &part.snapshot_digest).await?;
            sqlx::query("INSERT INTO catabolic_motion_exports(import_id,input_digest,format_version) VALUES(?,?,1)").bind(&part.import_id).bind(&part.snapshot_digest).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO catabolic_reference_audits(import_id,input_digest,audit_json) VALUES(?,?,?)").bind(&part.import_id).bind(&part.snapshot_digest).bind(&audit_json).execute(&mut *tx).await?;
            sqlx::query(
                "INSERT INTO catabolic_export_freezes(import_id,freeze_revision) VALUES(?,?)",
            )
            .bind(&part.import_id)
            .bind(freeze_revision as i64)
            .execute(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO catabolic_export_plan_parts(import_id,root_import_id,ordinal,snapshot_digest) VALUES(?,?,?,?)").bind(&part.import_id).bind(root_import_id).bind(i64::from(part.index)).bind(&part.snapshot_digest).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(plan)
    }

    pub async fn motion_export_plan(
        &self,
        root_import_id: &str,
    ) -> anyhow::Result<MotionExportPlan> {
        let (source_id, namespace, audit_digest, raw): (String,String,String,String) = sqlx::query_as("SELECT source_snapshot_id,source_namespace,audit_digest,plan_json FROM catabolic_export_plans WHERE root_import_id=?").bind(root_import_id).fetch_optional(&self.journal).await?.context("unknown export plan")?;
        let plan: MotionExportPlan = serde_json::from_str(&raw)?;
        ensure!(
            plan.contract_version == 1
                && plan.root_import_id == root_import_id
                && plan.source_snapshot_id == source_id
                && plan.source_namespace == namespace
                && namespace == SOURCE
                && plan.audit_digest == audit_digest
                && !plan.partitions.is_empty(),
            "corrupt export plan"
        );
        let parts: Vec<(String,i64,String)> = sqlx::query_as("SELECT import_id,ordinal,snapshot_digest FROM catabolic_export_plan_parts WHERE root_import_id=? ORDER BY ordinal").bind(root_import_id).fetch_all(&self.journal).await?;
        ensure!(
            parts.len() == plan.partitions.len(),
            "incomplete export plan"
        );
        let mut owners = BTreeSet::new();
        for (index, ((id, ordinal, digest), part)) in parts.iter().zip(&plan.partitions).enumerate()
        {
            ensure!(
                *ordinal == index as i64
                    && part.index as usize == index
                    && id == &part.import_id
                    && digest == &part.snapshot_digest
                    && part.dependency_indices == (0..part.index).collect::<Vec<_>>(),
                "corrupt partition ordering"
            );
            let (snapshot, actual) = self.import_snapshot(id).await?;
            ensure!(
                &actual == digest
                    && snapshot.manifest.source_snapshot_id == source_id
                    && snapshot.manifest.source_namespace == namespace,
                "partition source differs from export plan"
            );
            let audit: String = sqlx::query_scalar("SELECT audit_json FROM catabolic_reference_audits WHERE import_id=? AND input_digest=?").bind(id).bind(digest).fetch_one(&self.journal).await?;
            ensure!(
                catabolic_core::sha256(audit.as_bytes()) == audit_digest,
                "partition audit differs from root"
            );
            for item in snapshot.batches.iter().flat_map(|b| &b.items) {
                ensure!(
                    owners.insert(item.item_id.clone()),
                    "duplicate partition owner"
                );
            }
        }
        Ok(plan)
    }
}

impl NativeCatalog {
    /// Capture once from this host's Motion database. Replay reads the retained
    /// export; it never recaptures newer source rows under the same import ID.
    pub async fn capture_motion_import(&self, import_id: &str) -> anyhow::Result<SnapshotManifest> {
        self.status().await?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let aggregate: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM catabolic_export_plans WHERE root_import_id=?)",
        )
        .bind(import_id)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            !aggregate,
            "source root already captured as an aggregate plan"
        );
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
            (69..=93).contains(&version),
            "Motion export requires supported host schema 69 through 93; observed {version}"
        );
        let freeze_revision = crate::source_freeze::capture_freeze(&mut tx, import_id).await?;
        let (snapshot, audit) = capture(self, &mut tx, import_id, freeze_revision).await?;
        let digest = snapshot.validate()?;
        save_snapshot(&mut tx, &snapshot, &digest).await?;
        sqlx::query("INSERT INTO catabolic_motion_exports(import_id,input_digest,format_version) VALUES(?,?,1)")
            .bind(import_id).bind(&digest).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO catabolic_reference_audits(import_id,input_digest,audit_json) VALUES(?,?,?)")
            .bind(import_id).bind(&digest).bind(serde_json::to_string(&audit)?).execute(&mut *tx).await?;
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

    /// Resolve dependencies from this host's durable earlier activation records,
    /// then verify native committed receipts and current endpoints in one owner.
    async fn motion_existing_item_authority(
        &self,
        import_id: &str,
        needed: BTreeSet<String>,
    ) -> anyhow::Result<BTreeMap<String, catabolic_store::catalog_import_typed::ExistingItemGrant>>
    {
        if needed.is_empty() {
            return Ok(BTreeMap::new());
        }
        let (root, ordinal): (String, i64) = sqlx::query_as(
            "SELECT root_import_id,ordinal FROM catabolic_export_plan_parts WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&self.journal)
        .await?
        .context("external typed endpoint requires trusted partition plan")?;
        let plan = self.motion_export_plan(&root).await?;
        ensure!(
            ordinal >= 0
                && plan
                    .partitions
                    .get(ordinal as usize)
                    .is_some_and(|p| p.import_id == import_id),
            "dependency partition mismatch"
        );
        let mut records = Vec::new();
        let mut expected_mappings = BTreeMap::new();
        for part in plan.partitions.iter().take(ordinal as usize) {
            let (snapshot, digest) = self.import_snapshot(&part.import_id).await?;
            let relevant: BTreeSet<String> = snapshot
                .batches
                .iter()
                .flat_map(|b| &b.items)
                .filter(|i| needed.contains(&i.item_id))
                .map(|i| i.item_id.clone())
                .collect();
            if relevant.is_empty() {
                continue;
            }
            let (encoded, intent, phase): (String,String,String) = sqlx::query_as("SELECT checkpoint_json,activation_envelope,phase FROM catabolic_cutover_parts WHERE import_id=? AND ordinal=?")
                .bind(&part.import_id).bind(i64::from(part.index)).fetch_optional(&self.journal).await?.context("dependency partition has no durable activation")?;
            let checkpoint: playscale_core::catalog_cutover::Checkpoint =
                serde_json::from_str(&encoded)?;
            ensure!(
                phase == "activated"
                    && checkpoint.valid()
                    && checkpoint.phase == playscale_core::catalog_cutover::Phase::Activated
                    && checkpoint.import_id == part.import_id
                    && checkpoint.input_digest == digest,
                "dependency checkpoint not activated or mismatched"
            );
            let mappings: BTreeSet<(String, String)> = snapshot
                .batches
                .iter()
                .flat_map(|b| &b.items)
                .map(|i| (i.source_id.clone(), i.item_id.clone()))
                .collect();
            expected_mappings.insert(part.import_id.clone(), mappings);
            records.push((checkpoint, intent));
        }
        let path = self.path.clone();
        let identity = self.identity.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            use catabolic_core::catalog_command::ReceiptVisibility;
            use catabolic_store::{
                Store,
                catalog_commands::{self, CommandOutcome, TrustedCommandAuthority},
                catalog_import::{self, Reconciliation},
            };
            let store = Store::open(&path, true, false)?;
            identity
                .admit(&crate::status(&store)?.identity)
                .map_err(|e| anyhow::anyhow!("dependency catalog changed: {e:?}"))?;
            let mut allowed = BTreeSet::new();
            for (checkpoint, intent) in records {
                let envelope = crate::decode_envelope(&intent)?;
                let authority = || TrustedCommandAuthority {
                    host_namespace: HOST_NAMESPACE,
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: false,
                    identity_authority: None,
                };
                ensure!(
                    catalog_import::reconcile_persisted_activation(
                        &store,
                        &envelope,
                        &crate::cutover::native_checkpoint(&checkpoint),
                        authority(),
                        crate::receipt_policy()
                    )? != Reconciliation::KeepAdmissionClosed,
                    "dependency activation cannot reconcile"
                );
                let CommandOutcome::Replay { result, .. } = catalog_commands::execute(
                    &store,
                    &envelope,
                    authority(),
                    crate::receipt_policy(),
                )?
                else {
                    anyhow::bail!("dependency receipt missing");
                };
                let ImportReceipt::Activated {
                    import_id,
                    input_digest,
                    source_snapshot_id: _,
                    mappings,
                    ..
                } = serde_json::from_value(result)?
                else {
                    anyhow::bail!("dependency receipt not activated");
                };
                ensure!(
                    import_id == checkpoint.import_id && input_digest == checkpoint.input_digest,
                    "dependency receipt identity mismatch"
                );
                let actual: BTreeSet<(String, String)> = mappings
                    .into_iter()
                    .map(|m| (m.source_id, m.item_id))
                    .collect();
                ensure!(
                    expected_mappings.get(&import_id) == Some(&actual),
                    "dependency receipt mappings differ from captured source"
                );
                allowed.extend(
                    actual
                        .into_iter()
                        .map(|(_, id)| id)
                        .filter(|id| needed.contains(id)),
                );
            }
            ensure!(
                allowed == needed,
                "typed import endpoint is outside earlier committed source owners"
            );
            needed
                .into_iter()
                .map(|id| {
                    let grant = catabolic_store::catalog_import_typed::capture_existing_item(
                        &store,
                        HOST_NAMESPACE,
                        &id,
                        &allowed,
                    )?;
                    Ok((id, grant))
                })
                .collect()
        })
        .await?
    }

    /// Grants are derived only from an immutable export captured from Motion's
    /// own tables in the same transaction as its provenance marker, never from
    /// a caller-supplied snapshot claiming to be a Motion export.
    pub async fn validate_motion_import(&self, import_id: &str) -> anyhow::Result<ImportReceipt> {
        let grants = self.motion_import_authority(import_id).await?;
        self.validate_import(import_id, Some(grants)).await
    }

    /// Reconstruct authority only from host-retained, immutable source provenance.
    /// Shared by rehearsal and cutover; accepting an import ID is not authority.
    pub(crate) async fn motion_import_authority(
        &self,
        import_id: &str,
    ) -> anyhow::Result<TypedAuthority> {
        let (snapshot, digest) = trusted_snapshot_from_journal(&self.journal, import_id).await?;
        let retained: Option<(String, String)> = sqlx::query_as(
            "SELECT input_digest,audit_json FROM catabolic_reference_audits WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&self.journal)
        .await?;
        if let Some((saved, raw)) = retained {
            ensure!(saved == digest, "origin audit input digest changed");
            let audit: crate::reference_inventory::SourceAudit = serde_json::from_str(&raw)?;
            let mut connection = self.journal.acquire().await?;
            let _proof =
                crate::origin_preservation::verified_in(&mut connection, &snapshot, &audit).await?;
        }

        let needed = referenced_typed_items(&snapshot)?;
        let owned: BTreeSet<String> = snapshot
            .batches
            .iter()
            .flat_map(|b| &b.items)
            .map(|i| i.item_id.clone())
            .collect();
        let mut grants = TypedAuthority::default();
        let occurrences: Vec<_> = snapshot
            .batches
            .iter()
            .filter_map(|b| b.typed.as_ref())
            .filter_map(|t| t.native.as_ref())
            .flat_map(|n| &n.occurrences)
            .collect();
        if !occurrences.is_empty() {
            let root: Option<String> = sqlx::query_scalar(
                "SELECT root_import_id FROM catabolic_export_plan_parts WHERE import_id=?",
            )
            .bind(import_id)
            .fetch_optional(&self.journal)
            .await?;
            if let Some(root) = &root {
                let plan = self.motion_export_plan(root).await?;
                ensure!(
                    plan.partitions
                        .iter()
                        .any(|part| part.import_id == import_id && part.snapshot_digest == digest),
                    "physical import is outside immutable export plan"
                );
            }
            let root = root.as_deref().unwrap_or(import_id);
            let retained: Vec<String> = sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? ORDER BY motion_file_id")
                .bind(root).fetch_all(&self.journal).await?;
            let retained = retained
                .into_iter()
                .map(|raw| {
                    serde_json::from_str::<crate::occurrence_bridge::OccurrenceMapping>(&raw)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let allowed = self.occurrence_authority(root).await?;
            for occurrence in occurrences {
                ensure!(
                    retained
                        .iter()
                        .any(|mapping| mapping.physical == *occurrence),
                    "exact occurrence is absent from immutable frozen source mapping"
                );
                ensure!(
                    allowed
                        .physical_files
                        .get(&occurrence.profile)
                        .is_some_and(|files| files.contains(&occurrence.exact.file_id)),
                    "physical source authority is absent"
                );
                // Only this partition's exact occurrences are admitted; unrelated
                // source files never inherit an implicit physical read grant.
                grants
                    .physical_files
                    .entry(occurrence.profile.clone())
                    .or_default()
                    .insert(occurrence.exact.file_id.clone());
            }
        }
        grants.existing_items = self
            .motion_existing_item_authority(import_id, needed)
            .await?;
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
                        for proof in &mutation.evidence {
                            ensure!(
                                proof.evidence["source_table"] == "media_versions",
                                "unrecognized source identity evidence"
                            );
                            let authority = grants.identity.as_mut().expect("context grant");
                            match proof.basis {
                                catabolic_core::catalog_identity::EquivalenceBasis::Declared => {
                                    ensure!(
                                        proof.evidence["source_version"]["equivalence"]
                                            == "declared",
                                        "source declaration mismatch"
                                    );
                                    authority.allow_declared_evidence = true;
                                }
                                catabolic_core::catalog_identity::EquivalenceBasis::Verified => {
                                    ensure!(
                                        proof.evidence["source_version"]["equivalence"]
                                            == "verified",
                                        "source verification mismatch"
                                    );
                                    authority.allow_verified_evidence = true;
                                }
                            }
                        }
                    }
                }
                if !typed.logical_aliases.is_empty() {
                    let grant =
                        grants
                            .logical_aliases
                            .get_or_insert_with(|| LogicalAliasImportAuthority {
                                host_namespace: HOST_NAMESPACE.into(),
                                aliases: BTreeSet::new(),
                                item_ids: owned
                                    .iter()
                                    .cloned()
                                    .chain(grants.existing_items.keys().cloned())
                                    .collect(),
                            });
                    grant.aliases.extend(
                        typed
                            .logical_aliases
                            .iter()
                            .map(|a| (a.alias_item_id.clone(), a.survivor_item_id.clone())),
                    );
                }
                for tags in &typed.tags {
                    ensure!(
                        tags.namespace.starts_with("motion."),
                        "unexpected Motion tag namespace"
                    );
                    ensure!(
                        tags.namespace == "motion.local" || tags.excluded_tags.is_empty(),
                        "nonlocal tag suppression"
                    );
                    grants.tag_sources.push(TagImportAuthority {
                        host_namespace: HOST_NAMESPACE.into(),
                        namespace: tags.namespace.clone(),
                        item_ids: BTreeSet::from([tags.item_id.clone()]),
                        allow_suppressions: tags.namespace == "motion.local",
                    });
                }
                for relationship in &typed.relationships {
                    grants
                        .relationships
                        .get_or_insert_with(|| RelationshipImportAuthority {
                            host_namespace: HOST_NAMESPACE.into(),
                            edges: BTreeSet::new(),
                        })
                        .edges
                        .insert((
                            relationship.source_item_id.clone(),
                            relationship.target_item_id.clone(),
                            relationship.kind.clone(),
                        ));
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
                    ensure!(
                        matches!(metadata.contract_version, 1 | 2),
                        "unsupported metadata import contract"
                    );
                    let system_fields =
                        import_system_fields(metadata.contract_version, &metadata.contributions)?;
                    ensure!(
                        metadata.system_fields == system_fields,
                        "metadata system policy differs from retained owner provenance"
                    );
                    ensure!(
                        metadata.contract_version == 1 || self.identity.schema_version >= 40,
                        "v2 metadata requires native schema 40"
                    );
                    grants.metadata.push(MetadataAuthority {
                        host_namespace: HOST_NAMESPACE.into(),
                        subject: metadata.item_id.clone(),
                        access: if metadata.contract_version == 2 {
                            MetadataAccess::InitializeV2 {
                                system_fields: system_fields.into_iter().collect(),
                            }
                        } else {
                            MetadataAccess::Initialize
                        },
                    });
                    for source in metadata.contributions {
                        let system_fields = if metadata.contract_version == 2 {
                            trusted_source_system_fields(&source)?
                        } else {
                            BTreeSet::new()
                        };
                        let kind = match source.kind {
                            ImportedSourceKind::Local => SourceKind::Local,
                            ImportedSourceKind::Provider => SourceKind::Provider,
                            ImportedSourceKind::Scan => SourceKind::Scan,
                        };
                        grants.metadata.push(MetadataAuthority {
                            host_namespace: HOST_NAMESPACE.into(),
                            subject: metadata.item_id.clone(),
                            access: if metadata.contract_version == 2 {
                                MetadataAccess::SourceV2 {
                                    namespace: source.namespace,
                                    record_id: source.record_id,
                                    kind,
                                    system_fields,
                                }
                            } else {
                                MetadataAccess::Source {
                                    namespace: source.namespace,
                                    record_id: source.record_id,
                                    kind,
                                }
                            },
                        });
                    }
                }
            }
        }
        Ok(grants)
    }
}

// These names and source identities are issued by the exporter, not inferred
// from arbitrary user field prefixes. The retained snapshot is authenticated
// before this helper is used to reconstruct native initialization/source grants.
const IMPORT_SYSTEM_FIELDS: &[&str] = &[
    "motion_catalog",
    "motion_artwork",
    "motion_library",
    "motion_edition",
    "motion_version",
    "motion_historical_version",
    "motion_historical_file",
    "motion_historical_timeline",
    "motion_source_history",
    "motion_reference_tombstone",
];
fn trusted_source_system_fields(source: &ImportedContribution) -> anyhow::Result<BTreeSet<String>> {
    let fields = source
        .fields
        .as_object()
        .context("metadata contribution must be an object")?;
    let allowed: &[&str] = match (source.namespace.as_str(), source.record_id.as_str()) {
        ("motion.structure", "source_projection") => &["motion_catalog"],
        ("motion.artwork", "source_projection") => &["motion_artwork"],
        ("motion.structure", "source_library") => &["motion_library"],
        ("motion.structure", "source_edition") => &["motion_edition"],
        ("motion.structure", "source_version") => &["motion_version", "motion_historical_version"],
        ("motion.history", "source_observation") => &[
            "motion_historical_file",
            "motion_historical_timeline",
            "motion_source_history",
            "motion_reference_tombstone",
        ],
        _ => &[],
    };
    let mut result = BTreeSet::new();
    for name in fields.keys().filter(|name| name.starts_with("motion_")) {
        ensure!(
            IMPORT_SYSTEM_FIELDS.contains(&name.as_str())
                && allowed.contains(&name.as_str())
                && matches!(source.kind, ImportedSourceKind::Scan),
            "reserved metadata policy lacks exact exporter source provenance"
        );
        result.insert(name.clone());
    }
    Ok(result)
}
fn import_system_fields(
    version: u32,
    sources: &[ImportedContribution],
) -> anyhow::Result<Vec<String>> {
    if version == 1 {
        return Ok(Vec::new());
    }
    ensure!(version == 2, "unsupported metadata import contract");
    let mut fields = BTreeSet::new();
    for source in sources {
        fields.extend(trusted_source_system_fields(source)?);
    }
    Ok(fields.into_iter().collect())
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
            target.tags.extend(typed.tags.clone());
            target.relationships.extend(typed.relationships.clone());
            target.logical_aliases.extend(typed.logical_aliases.clone());
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
    batches.push(unit);
    Ok(())
}
pub(crate) fn evidence(
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
    backend: &NativeCatalog,
    db: &mut SqliteConnection,
    import_id: &str,
    freeze_revision: Option<u64>,
) -> anyhow::Result<(Snapshot, crate::reference_inventory::SourceAudit)> {
    let metadata_version = if backend.identity.schema_version >= 40 {
        2
    } else {
        1
    };
    let mut batches = Vec::new();
    let items = rows(db, "items", None).await?;
    let source_item_ids: BTreeSet<String> = items
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_owned))
        .collect();
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
        let mut tags = vec![];
        let mut external_ids = Vec::new();
        let state = serde_json::from_value(item["match_state"].clone())?;
        let retired: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM item_aliases WHERE alias_id=?)")
                .bind(id)
                .fetch_one(&mut *db)
                .await?;
        for (source, revision, external_id, document, updated_at) in documents {
            if let Some(value) = external_id.as_ref() {
                external_ids.push(json!({"provider":source,"value":value,"namespace_version":""}));
            }
            let raw_document: Value = serde_json::from_str(&document)?;
            let doc: playscale_core::metadata::Contribution =
                serde_json::from_value(raw_document.clone())?;
            ensure!(
                source != "scan",
                "reserved scan contribution in Motion source"
            );
            ensure!(
                [
                    "motion_catalog",
                    "motion_artwork",
                    "motion_library",
                    "motion_edition",
                    "motion_version",
                    "motion_historical_version",
                    "motion_historical_file",
                    "motion_historical_timeline",
                    "motion_source_history",
                    "motion_reference_tombstone"
                ]
                .iter()
                .all(|field| !doc.values.contains_key(*field)),
                "Motion metadata uses reserved native projection field motion_catalog"
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
            ensure!(
                source == "local" || doc.excluded_tags.is_empty(),
                "provider source cannot import local tag suppressions"
            );
            for tag in doc.tags.iter().chain(&doc.excluded_tags) {
                ensure!(
                    catabolic_store::tagging::tag_name(&json!(tag))? == *tag,
                    "Motion tag identity differs from native normalization; exact label preservation is not yet qualified"
                );
            }
            if !doc.tags.is_empty() || !doc.excluded_tags.is_empty() {
                tags.push(ImportedTagSet {
                    item_id: id.into(),
                    namespace: format!("motion.{source}"),
                    source_revision: revision,
                    tags: doc.tags.iter().cloned().collect(),
                    excluded_tags: doc.excluded_tags.iter().cloned().collect(),
                });
            }
            if !doc.tags.is_empty() || !doc.excluded_tags.is_empty() || external_id.is_some() {
                pending.push((source.clone(),json!({"item_id":id,"source":source,"revision":revision,"external_id":external_id,"tags":doc.tags,"excluded_tags":doc.excluded_tags})));
            }
            sources.push(ImportedContribution {
                namespace: format!("motion.{source}"), record_id: "document".into(), source_revision: revision,
                kind: if source=="local" { ImportedSourceKind::Local } else { ImportedSourceKind::Provider },
                fields: serde_json::to_value(&doc.values)?,
                evidence: json!({"source_table":"metadata_documents","source":source,"external_id":external_id,"updated_at":updated_at,"document_json":document,"document":raw_document}),
            });
        }
        let structure: Option<(String, Option<String>, Option<i64>, i64)> = sqlx::query_as(
            "SELECT media_type,parent_id,number,revision FROM item_structure WHERE item_id=?",
        )
        .bind(id)
        .fetch_optional(&mut *db)
        .await?;
        let (media_type, parent_id, number, structure_revision) =
            structure.unwrap_or_else(|| ("unclassified".into(), None, None, 0));
        fields.insert("motion_catalog".into());
        sources.push(ImportedContribution {
            namespace: "motion.structure".into(), record_id: "source_projection".into(),
            source_revision: item["catalog_revision"].as_i64().context("invalid catalog source revision")?,
            kind: ImportedSourceKind::Scan,
            fields: json!({"motion_catalog": {
                "contract_version":1, "item_id":id, "kind":item["kind"],
                "catalog_revision":item["catalog_revision"], "match_state":item["match_state"],
                "media_type":media_type, "parent_id":parent_id, "number":number,
                "structure_revision":structure_revision,"external_ids":external_ids,
            }}),
            evidence: json!({"source_tables":["items","item_structure"],"meaning":"exact source projection; not native relationship or physical equivalence authority"}),
        });
        if let Some(artwork) = artwork_projection(db, id).await? {
            fields.insert("motion_artwork".into());
            sources.push(ImportedContribution {
                namespace:"motion.artwork".into(),record_id:"source_projection".into(),
                source_revision:item["catalog_revision"].as_i64().context("invalid artwork source revision")?,kind:ImportedSourceKind::Scan,
                fields:json!({"motion_artwork":artwork}),evidence:json!({"source_tables":["artwork_assets","artwork_contributions","artwork_selections","item_artwork_assets"],"meaning":"source-qualified reference state; not generated artifact publication"}),
            });
        }
        let policies: Vec<Value> = fields.into_iter().map(|field|json!({"field":field,"value_type":"json","nullable":false,"normalization":"exact"})).collect();
        append(
            &mut batches,
            ImportBatch {
                items: vec![ImportItem {
                    source_id: id.into(),
                    item_id: id.into(),
                    kind: playscale_core::catalog_kind::to_native(
                        item["kind"].as_str().context("invalid Motion kind")?,
                    )
                    .context("unsupported Motion media family")?
                    .into(),
                    legacy_metadata: json!({}),
                }],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: metadata_version,
                        system_fields: import_system_fields(metadata_version, &sources)?,
                        item_id: id.into(),
                        policies: json!(policies),
                        contributions: sources,
                    }],
                    pins,
                    tags,
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
    capture_libraries(db, &mut batches, metadata_version).await?;
    let physical = if freeze_revision.is_some() {
        backend.capture_motion_occurrences(db, import_id).await?
    } else {
        Default::default()
    };
    let mapped_versions =
        capture_structure(db, &mut batches, &physical.mappings, metadata_version).await?;
    let mut physical_coverage = crate::reference_inventory::PhysicalCoverage::default();
    physical_coverage.unresolved_occurrences = physical.unresolved.clone();
    physical_coverage.ownership_mismatches = sqlx::query_scalar::<_,String>("SELECT DISTINCT f.id FROM media_files f JOIN version_files b ON b.file_id=f.id JOIN media_versions v ON v.id=b.version_id JOIN timelines t ON t.id=v.timeline_id WHERE t.edition_id IS NOT f.edition_id ORDER BY f.id")
        .fetch_all(&mut *db).await?.into_iter().collect();

    physical_coverage.unassociated_files = physical
        .mappings
        .iter()
        .filter(|mapping| {
            !mapping.version_files.iter().any(|row| {
                row["version_id"]
                    .as_str()
                    .is_some_and(|id| mapped_versions.contains(id))
            })
        })
        .filter_map(|mapping| mapping.motion_file["id"].as_str().map(str::to_owned))
        .collect();

    physical_coverage
        .preserved_ids
        .insert("items".into(), source_item_ids);
    physical_coverage.preserved_ids.insert(
        "catalog_libraries".into(),
        batches
            .iter()
            .flat_map(|b| &b.items)
            .filter(|item| item.kind == "custom:motion_library")
            .filter_map(|item| {
                item.item_id
                    .strip_prefix("motion.library:")
                    .map(str::to_owned)
            })
            .collect(),
    );
    let mut source_ids = BTreeSet::new();
    for source in rows(db, "libraries", None).await? {
        let id = source["id"]
            .as_str()
            .context("invalid physical source ID")?;
        if freeze_revision.is_some()
            && source["managed"].as_i64() == Some(0)
            && source["enabled"].as_i64() == Some(1)
            && backend.qualified_source_mapping_in(db, id).await?.is_some()
        {
            source_ids.insert(id.to_owned());
        }
    }
    physical_coverage
        .preserved_ids
        .insert("libraries".into(), source_ids);

    physical_coverage.preserved_ids.insert(
        "media_files".into(),
        physical
            .mappings
            .iter()
            .filter(|mapping| {
                mapping.version_files.iter().any(|row| {
                    row["version_id"]
                        .as_str()
                        .is_some_and(|id| mapped_versions.contains(id))
                })
            })
            .filter_map(|m| m.motion_file["id"].as_str().map(str::to_owned))
            .collect(),
    );
    physical_coverage
        .preserved_ids
        .insert("media_versions".into(), mapped_versions.clone());
    physical_coverage.preserved_ids.insert(
        "editions".into(),
        batches
            .iter()
            .flat_map(|b| &b.items)
            .filter(|item| item.kind == "custom:motion_edition")
            .filter_map(|item| {
                item.item_id
                    .strip_prefix("motion.edition:")
                    .map(str::to_owned)
            })
            .collect(),
    );
    let mut timeline_ids = BTreeSet::new();
    for native in batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .filter_map(|t| t.native.as_ref())
    {
        for value in &native.mutations {
            let mutation: IdentityMutation = serde_json::from_value(value.clone())?;
            timeline_ids.extend(mutation.timelines.into_iter().map(|t| t.timeline_id));
        }
    }
    physical_coverage
        .preserved_ids
        .insert("timelines".into(), timeline_ids);
    for table in [
        "items",
        "catalog_libraries",
        "libraries",
        "media_files",
        "media_versions",
        "editions",
        "timelines",
    ] {
        let actual: BTreeSet<String> = rows(db, table, None)
            .await?
            .iter()
            .map(|row| {
                row["id"]
                    .as_str()
                    .map(str::to_owned)
                    .context("nontext source identity")
            })
            .collect::<anyhow::Result<_>>()?;
        let mapped = &physical_coverage.preserved_ids[table];
        ensure!(
            mapped.is_subset(&actual),
            "typed owner lacks frozen source identity"
        );
        physical_coverage
            .missing_ids
            .insert(table.into(), actual.difference(mapped).cloned().collect());
    }

    capture_historical_owners(db, &mut batches, &mut physical_coverage, metadata_version).await?;

    for mut mapping in physical.mappings {
        // Historical/stale bindings stay in retained raw source evidence. Only
        // versions admitted by the complete exact-binding mapper reach native.
        mapping.version_files.retain(|row| {
            row["version_id"]
                .as_str()
                .is_some_and(|id| mapped_versions.contains(id))
        });
        if mapping.version_files.is_empty() {
            continue;
        }
        let typed = mapping.typed()?;
        append(
            &mut batches,
            ImportBatch {
                typed: Some(TypedBatch {
                    native: Some(typed),
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }
    for (file_id, reason) in physical.unresolved {
        evidence(
            &mut batches,
            format!("physical-unresolved:{file_id}"),
            EvidenceKind::PhysicalOccurrence,
            json!({"file_id":file_id,"reason":reason}),
        )?;
    }
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
        "catalog_libraries",
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
    let mut audit =
        crate::reference_inventory::capture_reference_evidence(db, &mut batches).await?;
    capture_reference_tombstones(
        db,
        &mut batches,
        &audit.slots,
        &mut physical_coverage,
        metadata_version,
    )
    .await?;
    crate::reference_inventory::capture_physical_reference_coverage(
        db,
        &audit.slots,
        &mut physical_coverage,
    )
    .await?;
    if physical_coverage
        .historical_ids
        .values()
        .any(|ids| !ids.is_empty())
        || physical_coverage
            .tombstone_ids
            .values()
            .any(|ids| !ids.is_empty())
    {
        let mut historical_reference_coverage = physical_coverage.clone();
        for (table, ids) in &physical_coverage.historical_ids {
            historical_reference_coverage
                .preserved_ids
                .entry(table.clone())
                .or_default()
                .extend(ids.iter().cloned());
        }
        historical_reference_coverage.reference_slots.clear();
        crate::reference_inventory::capture_historical_reference_coverage(
            db,
            &audit.slots,
            &mut historical_reference_coverage,
        )
        .await?;
        physical_coverage.historical_reference_slots =
            historical_reference_coverage.reference_slots;
    }
    audit.physical_coverage = physical_coverage;
    audit.logical_root_structure_supported =
        rows(db, "item_structure", None).await?.iter().all(|row| {
            row["item_id"]
                .as_str()
                .is_some_and(|id| audit.physical_coverage.preserved_ids["items"].contains(id))
                && row["parent_id"].is_null()
                && row["number"].is_null()
                && row["media_type"]
                    .as_str()
                    .is_some_and(|kind| ["unclassified", "movie", "series"].contains(&kind))
                && row["revision"]
                    .as_i64()
                    .is_some_and(|revision| revision > 0)
        });

    // Auth replay records remain in Motion. Read only operation/target counts;
    // never export secret-bearing acknowledgments or issuance nonces.
    let pairing_receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_records r WHERE r.operation='approvePairing' AND EXISTS(SELECT 1 FROM pairings p JOIN devices d ON d.id=p.device_id WHERE p.id=r.target AND p.phase IN ('approved','claimed'))")
        .fetch_one(&mut *db).await?;
    audit.retained_pairing_receipts = u64::try_from(pairing_receipts)?;
    (
        audit.artwork_references_supported,
        audit.retained_catalog_artwork_receipts,
    ) = crate::artwork_import::qualify(db, &batches).await?;
    (
        audit.retained_schedules_supported,
        audit.retained_schedule_receipts,
    ) = crate::schedule_import::qualify(db).await?;
    audit.retained_source_creation_receipts = crate::source_receipt_import::qualify(db).await?;
    let receipts = rows(db, "catalog_receipts", None).await?;
    if crate::reference_inventory::zero_effect_upgrade_receipts(&receipts) {
        audit.retained_zero_effect_receipts = receipts;
    }

    // Empty catalogs still need an explicit reference audit before activation.
    evidence(
        &mut batches,
        "motion.reference-audit".into(),
        EvidenceKind::ApplicationReference,
        json!({"relational_references":"captured","embedded_references":"not_audited","writer_freeze":if freeze_revision.is_some(){"retained"}else{"not_established"},"freeze_revision":freeze_revision}),
    )?;
    let imported_items: BTreeSet<String> = batches
        .iter()
        .flat_map(|b| &b.items)
        .map(|i| i.item_id.clone())
        .collect();
    let mut logical_application_supported = true;
    for table in crate::reference_inventory::LOGICAL_APPLICATION_TABLES {
        if table.starts_with("search_") {
            continue;
        } // Derived caches are not native serving authority.
        for row in rows(db, table, None).await? {
            if *table == "collections" && !row["filter_id"].is_null() {
                logical_application_supported = false;
            }
            for slot in audit.slots.iter().filter(|slot| slot.table == *table) {
                // Every admitted application item reference is NOT NULL in
                // the exact supported schema. Nontext/null legacy corruption
                // cannot be discharged as an absent reference.
                match row.get(&slot.column).and_then(Value::as_str) {
                    Some(id) if slot.target_table == "items" && imported_items.contains(id) => {}
                    _ => logical_application_supported = false,
                }
            }
            evidence(
                &mut batches,
                format!(
                    "application-retained:{table}:{}",
                    audit.resolved_evidence.len()
                ),
                EvidenceKind::ApplicationReference,
                json!({"table":table,"row":row,"disposition":if *table == "change_events" { "historical event row retained exactly; resource_id is not a serving grant or asserted native identity" } else { "Motion-owned application row; checked native logical item IDs preserved exactly" }}),
            )?;
            // Index is only an evidence key, not a source revision.
            audit
                .resolved_evidence
                .push(json!({"application_table":table,"retained_row":row}));
        }
    }
    audit.physical_history_supported =
        crate::reference_inventory::physical_history_supported(db).await?;
    audit.retained_download_receipts =
        crate::reference_inventory::retained_download_receipts(db).await?;
    if audit.physical_history_supported {
        for table in crate::reference_inventory::PHYSICAL_HISTORY_TABLES {
            for row in rows(db, table, None).await? {
                audit.resolved_evidence.push(json!({"application_table":table,
                    "retained_row_sha256":catabolic_core::sha256(serde_json::to_vec(&row)?),
                    "disposition":"retained unchanged by Motion application owner; historical revisions never provide native current-byte authority"}));
            }
        }
    }
    let empty_cutover = freeze_revision.is_some() && audit.empty_catalog();
    // The first fully qualified nonempty contract is a logical-only catalog.
    // Other populated tables remain explicit blockers, not inferred mappings.
    let documents_supported = batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
        .flat_map(|m| &m.contributions)
        .all(|c| {
            if c.evidence["source_table"] != "metadata_documents" {
                return true;
            }
            c.evidence["external_id"].is_null()
                && c.evidence["document"].as_object().is_some_and(|d| {
                    d.keys()
                        .all(|k| ["values", "tags", "excluded_tags"].contains(&k.as_str()))
                })
        });
    let embedded_supported = batches
        .iter()
        .flat_map(|b| &b.unresolved_evidence)
        .filter(|e| e.evidence_id.starts_with("embedded-reference:"))
        .all(|e| {
            if e.data["table"] == "offline_events" || e.data["table"] == "viewing_events" {
                return audit.physical_history_supported;
            }
            (e.data["table"] == "metadata_documents"
                || (e.data["table"] == "catalog_receipts"
                    && !audit.retained_zero_effect_receipts.is_empty()))
                && e.data["references"].as_array().is_some_and(Vec::is_empty)
        });
    if freeze_revision.is_some()
        && audit.historical_source_candidate()
        && logical_application_supported
        && documents_supported
        && embedded_supported
    {
        // Contract 2 admits explicit native historical provenance owners only.
        // It never supplies physical source registration or current-byte grants.
        audit.physical_coverage.historical_reference_contract = Some(1);
        audit.physical_coverage.source_reference_contract = Some(2);
        for batch in &mut batches {
            for record in batch.unresolved_evidence.drain(..) {
                audit.resolved_evidence.push(serde_json::to_value(record)?);
            }
        }
        batches.retain(|batch| !batch.items.is_empty() || batch.typed.is_some());
    }
    let origin_proof = if freeze_revision.is_some()
        && audit.table_rows.get("nfo_origins").copied().unwrap_or(0) > 0
    {
        // Failure preserves all unresolved evidence; this never erases an
        // unsupported origin in order to reach an activation-ready snapshot.
        match crate::origin_preservation::capture_in(db, import_id, &batches, &audit).await {
            Ok(proof) => Some(proof),
            Err(_) => None,
        }
    } else {
        None
    };
    let physical_cutover = freeze_revision.is_some()
        && [
            "libraries",
            "editions",
            "timelines",
            "media_versions",
            "media_files",
        ]
        .iter()
        .any(|table| audit.table_rows.get(*table).copied().unwrap_or(0) > 0)
        && audit.physical_candidate_with_origins(origin_proof.as_ref())
        && logical_application_supported
        && documents_supported
        && embedded_supported;
    let has_origins = origin_proof.is_some();
    drop(origin_proof);
    if physical_cutover {
        // This admits preserved source ownership, NOT physical HTTP serving.
        // Native validation independently checks every typed owner; projection
        // and byte-route qualification remain separate closed capabilities.
        audit.physical_coverage.source_reference_contract = Some(1);
        if has_origins {
            audit.origin_preservation = Some(crate::origin_preservation::Contract {
                version: 1,
                origin_rows: audit.table_rows["nfo_origins"],
            });
        }

        for batch in &mut batches {
            for record in batch.unresolved_evidence.drain(..) {
                audit.resolved_evidence.push(serde_json::to_value(record)?);
            }
        }
        batches.retain(|batch| !batch.items.is_empty() || batch.typed.is_some());
    }
    if freeze_revision.is_some()
        && !empty_cutover
        && audit.logical_only_candidate()
        && logical_application_supported
        && documents_supported
        && embedded_supported
    {
        audit.logical_only_supported = true;
        // Preserve every discharged obligation in the same immutable source audit.
        // Only native typed owners supply serving/activation authority.
        for batch in &mut batches {
            for record in batch.unresolved_evidence.drain(..) {
                audit.resolved_evidence.push(serde_json::to_value(record)?);
            }
        }
        batches.retain(|b| !b.items.is_empty() || b.typed.is_some());
    }
    if empty_cutover {
        // Exact source audit remains in this host's immutable provenance table.
        // Native contract v3 represents a real empty catalog with zero batches,
        // not a fabricated item or unresolved evidence claimed to be resolved.
        batches.clear();
    }
    audit.atomic_batch_groups = repack_native_groups(&mut batches)?;
    let snapshot = Snapshot {
        manifest: SnapshotManifest {
            contract_version: if batches.is_empty() {
                // Qualified retained history can coexist with zero native owners.
                catalog_import::EMPTY_IMPORT_CONTRACT_VERSION
            } else {
                catalog_import::TYPED_IMPORT_CONTRACT_VERSION
            },
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
    // Each batch is bounded; the caller validates a single manifest or packs
    // these complete owner units into bounded partition manifests.
    Ok((snapshot, audit))
}
fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
pub(crate) async fn rows(
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
    let mut result = Vec::new();
    let mut offset = 0_i64;
    loop {
        let query = format!(
            "SELECT json_object({args}) FROM {} ORDER BY rowid LIMIT 1024 OFFSET ?",
            quote(table)
        );
        let values: Vec<String> = sqlx::query_scalar(&query)
            .bind(offset)
            .fetch_all(&mut *db)
            .await?;
        let count = values.len();
        for value in values {
            result.push(serde_json::from_str(&value)?);
        }
        if count < 1024 {
            break;
        }
        offset += 1024;
    }
    Ok(result)
}

/// Logical library definitions are native catalog owners. Their source ID list
/// is descriptive only: serving membership requires a separately saved query
/// built from independently verified native source bindings.
async fn capture_libraries(
    db: &mut SqliteConnection,
    batches: &mut Vec<ImportBatch>,
    metadata_version: u32,
) -> anyhow::Result<()> {
    for library in rows(db, "catalog_libraries", None).await? {
        let id = library["id"]
            .as_str()
            .context("invalid logical library ID")?;
        let destination = format!("motion.library:{id}");
        ensure!(
            !batches
                .iter()
                .flat_map(|b| &b.items)
                .any(|item| item.item_id == destination || item.source_id == destination),
            "logical library key collides with a source item"
        );
        let source_ids: Vec<String> = sqlx::query_scalar(
            "SELECT source_id FROM library_sources WHERE library_id=? ORDER BY source_id",
        )
        .bind(id)
        .fetch_all(&mut *db)
        .await?;
        let revision = library["revision"]
            .as_i64()
            .context("invalid logical library revision")?;
        let model = json!({"contract_version":1,"id":id,"catalog_revision":revision,"name":library["name"],"kind":library["kind"],"language":library["language"],"source_ids":source_ids});
        append(
            batches,
            ImportBatch {
                items: vec![ImportItem {
                    source_id: destination.clone(),
                    item_id: destination.clone(),
                    kind: "custom:motion_library".into(),
                    legacy_metadata: json!({}),
                }],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: metadata_version,
                        system_fields: if metadata_version == 2 {
                            vec!["motion_library".into()]
                        } else {
                            vec![]
                        },
                        item_id: destination,
                        policies: json!([{"field":"title","value_type":"json","nullable":false,"normalization":"exact"},{"field":"motion_library","value_type":"json","nullable":false,"normalization":"exact"}]),
                        contributions: vec![ImportedContribution {
                            namespace: "motion.structure".into(),
                            record_id: "source_library".into(),
                            source_revision: revision,
                            kind: ImportedSourceKind::Scan,
                            fields: json!({"title":library["name"],"motion_library":model}),
                            evidence: json!({"source_table":"catalog_libraries","source_row":library,"meaning":"exact definition; source_ids do not grant access or native query authority"}),
                        }],
                    }],
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }
    Ok(())
}

/// Editions live in a separate source keyspace, but are native item owners.
/// Keep all source rows as evidence until relationships, versions and references
/// are qualified. A duration observation alone does not establish equivalence.
async fn capture_reference_tombstones(
    db: &mut SqliteConnection,
    batches: &mut Vec<ImportBatch>,
    slots: &[crate::reference_inventory::ReferenceSlot],
    coverage: &mut crate::reference_inventory::PhysicalCoverage,
    metadata_version: u32,
) -> anyhow::Result<()> {
    let mut groups: BTreeMap<(String, String), BTreeMap<(String, String), Vec<Value>>> =
        BTreeMap::new();
    for slot in slots
        .iter()
        .filter(|slot| crate::reference_inventory::historical_tombstone_slot(slot))
    {
        let actual: BTreeSet<String> = rows(db, &slot.target_table, None)
            .await?
            .iter()
            .filter_map(|row| row["id"].as_str().map(str::to_owned))
            .collect();
        let info = sqlx::query(&format!("PRAGMA table_info({})", quote(&slot.table)))
            .fetch_all(&mut *db)
            .await?;
        let mut selected: BTreeSet<String> = info
            .iter()
            .filter(|column| column.get::<i64, _>("pk") > 0)
            .map(|column| column.get("name"))
            .collect();
        selected.insert(slot.column.clone());
        for row in rows(db, &slot.table, Some(&selected)).await? {
            let Some(id) = row[&slot.column]
                .as_str()
                .filter(|id| !id.is_empty() && !actual.contains(*id))
            else {
                continue;
            };
            groups
                .entry((slot.target_table.clone(), id.into()))
                .or_default()
                .entry((slot.table.clone(), slot.column.clone()))
                .or_default()
                .push(row);
        }
    }
    for ((target, id), references) in groups {
        let evidence:Vec<Value>=references.into_iter().map(|((table,column),rows)|Ok(json!({"table":table,"column":column,"reference_count":rows.len(),"reference_rows_sha256":catabolic_core::sha256(serde_json::to_vec(&rows)?)}))).collect::<anyhow::Result<_>>()?;
        let key = catabolic_core::sha256(serde_json::to_vec(&(&target, &id))?);
        append_historical_owner(
            batches,
            metadata_version,
            format!("motion.tombstone:{key}"),
            "custom:motion_reference_tombstone",
            "motion_reference_tombstone",
            json!({"target_table":target,"original_id":id,"absent_from_frozen_catalog":true,"reference_slots":evidence}),
            Vec::new(),
        )?;
        coverage.tombstone_ids.entry(target).or_default().insert(id);
    }
    Ok(())
}

/// Import unavailable catalog facts through ordinary descriptive/curation
/// owners, never through the physical identity mutation contract. Their source
/// IDs remain unresolved for activation until a historical projection contract
/// is independently qualified.
async fn capture_historical_owners(
    db: &mut SqliteConnection,
    batches: &mut Vec<ImportBatch>,
    coverage: &mut crate::reference_inventory::PhysicalCoverage,
    metadata_version: u32,
) -> anyhow::Result<()> {
    if ["media_versions", "media_files", "timelines", "libraries"]
        .iter()
        .all(|table| coverage.missing_ids[*table].is_empty())
        && coverage.unassociated_files.is_empty()
    {
        return Ok(());
    }
    let versions = coverage.missing_ids["media_versions"].clone();
    for batch in batches.iter_mut() {
        for item in &mut batch.items {
            if item
                .item_id
                .strip_prefix("motion.version:")
                .is_some_and(|id| versions.contains(id))
            {
                item.kind = "custom:motion_historical_version".into();
            }
        }
        if let Some(typed) = &mut batch.typed {
            for metadata in &mut typed.metadata {
                if metadata
                    .item_id
                    .strip_prefix("motion.version:")
                    .is_some_and(|id| versions.contains(id))
                {
                    metadata.system_fields = if metadata_version == 2 {
                        vec!["motion_historical_version".into()]
                    } else {
                        vec![]
                    };
                    metadata.policies = json!([{"field":"motion_historical_version","value_type":"json","nullable":false,"normalization":"exact"}]);
                    for source in &mut metadata.contributions {
                        let mut model = source.fields["motion_version"].take();
                        model["authority"] = json!(
                            "unverified legacy provenance; no native version, occurrence, equivalence or current-byte grant"
                        );
                        source.fields = json!({"motion_historical_version":model});
                    }
                }
            }
            for edge in &mut typed.relationships {
                if edge
                    .target_item_id
                    .strip_prefix("motion.version:")
                    .is_some_and(|id| versions.contains(id))
                {
                    edge.kind = "custom:motion_historical_version".into();
                }
            }
        }
    }
    coverage
        .historical_ids
        .insert("media_versions".into(), versions);
    let files: BTreeSet<String> = coverage.missing_ids["media_files"]
        .union(&coverage.unassociated_files)
        .cloned()
        .collect();
    let timelines = coverage.missing_ids["timelines"].clone();
    let mut sources = coverage.missing_ids["libraries"].clone();
    let source_files = rows(db, "media_files", None).await?;
    let editions: BTreeMap<String, String> = sqlx::query_as("SELECT id,item_id FROM editions")
        .fetch_all(&mut *db)
        .await?
        .into_iter()
        .collect();
    let mut source_works = BTreeSet::new();
    for row in source_files
        .iter()
        .filter(|row| row["id"].as_str().is_some_and(|id| files.contains(id)))
    {
        let id = row["id"].as_str().context("invalid historical file")?;
        let source = row["library_id"]
            .as_str()
            .context("invalid historical source")?;
        let edition = row["edition_id"]
            .as_str()
            .context("invalid historical edition")?;
        sources.insert(source.into());
        source_works.insert((
            source.to_owned(),
            editions
                .get(edition)
                .context("historical edition owner missing")?
                .clone(),
        ));
        append_historical_owner(
            batches,
            metadata_version,
            format!("motion.file-history:{id}"),
            "custom:motion_historical_file",
            "motion_historical_file",
            json!({"source_file":row}),
            Vec::new(),
        )?;
    }
    for row in rows(db, "timelines", None)
        .await?
        .iter()
        .filter(|row| row["id"].as_str().is_some_and(|id| timelines.contains(id)))
    {
        let id = row["id"].as_str().context("invalid historical timeline")?;
        append_historical_owner(
            batches,
            metadata_version,
            format!("motion.timeline-history:{id}"),
            "custom:motion_historical_timeline",
            "motion_historical_timeline",
            json!({"source_timeline":row}),
            Vec::new(),
        )?;
    }
    let memberships = rows(db, "library_sources", None).await?;
    for row in rows(db, "libraries", None)
        .await?
        .iter()
        .filter(|row| row["id"].as_str().is_some_and(|id| sources.contains(id)))
    {
        let id = row["id"].as_str().context("invalid historical source")?;
        let owner = format!("motion.source:{id}");
        let mut edges = Vec::new();
        for membership in memberships
            .iter()
            .filter(|membership| membership["source_id"] == id)
        {
            let library = membership["library_id"]
                .as_str()
                .context("invalid historical library")?;
            edges.push(historical_edge(
                format!("motion.library:{library}"),
                owner.clone(),
                "custom:motion_library_source",
                membership.clone(),
            ));
        }
        for (_, work) in source_works.iter().filter(|(source, _)| source == id) {
            edges.push(historical_edge(owner.clone(),work.clone(),"custom:motion_source_work",json!({"source_tables":["media_files","editions"],"meaning":"catalog membership only, no physical access grant"})));
        }
        append_historical_owner(
            batches,
            metadata_version,
            owner,
            "custom:motion_source",
            "motion_source_history",
            json!({"source_row":row}),
            edges,
        )?;
    }
    coverage.historical_ids.insert("media_files".into(), files);
    coverage
        .historical_ids
        .insert("timelines".into(), timelines);
    coverage.historical_ids.insert("libraries".into(), sources);
    Ok(())
}
fn historical_edge(
    source: String,
    target: String,
    kind: &str,
    metadata: Value,
) -> ImportedRelationship {
    let id = catabolic_core::sha256(
        serde_json::to_vec(&(&source, &target, kind)).expect("string tuple serialization"),
    );
    ImportedRelationship {
        source_relationship_id: format!("motion.history-edge:{id}"),
        source_revision: 1,
        source_item_id: source,
        target_item_id: target,
        kind: kind.into(),
        position: None,
        metadata,
    }
}
fn append_historical_owner(
    batches: &mut Vec<ImportBatch>,
    metadata_version: u32,
    id: String,
    kind: &str,
    field: &str,
    mut model: Value,
    relationships: Vec<ImportedRelationship>,
) -> anyhow::Result<()> {
    ensure!(
        !batches
            .iter()
            .flat_map(|batch| &batch.items)
            .any(|item| item.item_id == id || item.source_id == id),
        "historical owner collides with source identity"
    );
    model["contract_version"] = json!(1);
    model["authority"] = json!(
        "unverified legacy provenance; no native occurrence, version, equivalence, source registration or current-byte grant"
    );
    append(
        batches,
        ImportBatch {
            items: vec![ImportItem {
                source_id: id.clone(),
                item_id: id.clone(),
                kind: kind.into(),
                legacy_metadata: json!({}),
            }],
            typed: Some(TypedBatch {
                metadata: vec![ImportedMetadata {
                    contract_version: metadata_version,
                    system_fields: if metadata_version == 2 {
                        vec![field.into()]
                    } else {
                        vec![]
                    },
                    item_id: id,
                    policies: json!([{"field":field,"value_type":"json","nullable":false,"normalization":"exact"}]),
                    contributions: vec![ImportedContribution {
                        namespace: "motion.history".into(),
                        record_id: "source_observation".into(),
                        source_revision: 1,
                        kind: ImportedSourceKind::Scan,
                        fields: json!({field:model}),
                        evidence: json!({"meaning":"frozen source observation; original revisions retained inside source row, not native head equality"}),
                    }],
                }],
                ..Default::default()
            }),
            ..empty()
        },
    )?;
    for edges in relationships.chunks(64) {
        append(
            batches,
            ImportBatch {
                typed: Some(TypedBatch {
                    relationships: edges.to_vec(),
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }
    Ok(())
}

async fn capture_structure(
    db: &mut SqliteConnection,
    batches: &mut Vec<ImportBatch>,
    physical: &[crate::occurrence_bridge::OccurrenceMapping],
    metadata_version: u32,
) -> anyhow::Result<BTreeSet<String>> {
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
                    item_id: destination.clone(),
                    kind: "custom:motion_edition".into(),
                    legacy_metadata: json!({}),
                }],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: metadata_version,
                        system_fields: if metadata_version == 2 {
                            vec!["motion_edition".into()]
                        } else {
                            vec![]
                        },
                        item_id: destination.clone(),
                        policies: json!([{"field":"motion_edition","value_type":"json","nullable":false,"normalization":"exact"}]),
                        contributions: vec![ImportedContribution {
                            namespace: "motion.structure".into(),
                            record_id: "source_edition".into(),
                            source_revision: edition["revision"]
                                .as_i64()
                                .context("invalid edition revision")?,
                            kind: ImportedSourceKind::Scan,
                            fields: json!({"motion_edition":{"contract_version":1,"source_edition":edition,"source_timelines":timelines.iter().filter(|t|t["edition_id"]==source).collect::<Vec<_>>(),"revision_semantics":"source revisions preserved; native owner revisions start at one"}}),
                            evidence: json!({"source_table":"editions","source_id":source}),
                        }],
                    }],
                    relationships: vec![ImportedRelationship {
                        source_relationship_id: format!("motion.edition-owner:{source}"),
                        source_revision: edition["revision"]
                            .as_i64()
                            .context("invalid edition revision")?,
                        source_item_id: work.into(),
                        target_item_id: destination,
                        kind: "custom:motion_edition".into(),
                        position: None,
                        metadata: json!({"source_table":"editions","source_id":source,"source_label":edition["label"]}),
                    }],
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }
    for relationship in rows(db, "item_relationships", None).await? {
        append(
            batches,
            ImportBatch {
                typed: Some(TypedBatch {
                    relationships: vec![ImportedRelationship {
                        source_relationship_id: relationship["id"]
                            .as_str()
                            .context("invalid relationship ID")?
                            .into(),
                        source_revision: relationship["revision"]
                            .as_i64()
                            .context("invalid relationship revision")?,
                        source_item_id: relationship["source_item_id"]
                            .as_str()
                            .context("invalid relationship source")?
                            .into(),
                        target_item_id: relationship["target_item_id"]
                            .as_str()
                            .context("invalid relationship target")?
                            .into(),
                        kind: relationship["kind"]
                            .as_str()
                            .context("invalid relationship kind")?
                            .into(),
                        position: None,
                        metadata: json!({"source_table":"item_relationships","source_position":relationship["position"],"position_mapping":"preserved source position; no inferred native ordinal"}),
                    }],
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }
    let source_versions = rows(db, "media_versions", None).await?;
    let source_bindings = rows(db, "version_files", None).await?;
    let timelines_by_id: BTreeMap<String, Value> = timelines
        .iter()
        .map(|t| {
            Ok((
                t["id"].as_str().context("invalid timeline ID")?.to_owned(),
                t.clone(),
            ))
        })
        .collect::<anyhow::Result<_>>()?;
    let mut version_changes: BTreeMap<
        String,
        Vec<catabolic_store::catalog_identity_mutation::VersionChange>,
    > = BTreeMap::new();
    let mut equivalence_evidence: BTreeMap<
        String,
        Vec<catabolic_store::catalog_identity_mutation::IdentityEvidence>,
    > = BTreeMap::new();
    let mut profiles: BTreeMap<String, String> = BTreeMap::new();
    let mut mapped_versions = BTreeSet::new();
    let mut candidates: BTreeMap<
        String,
        Vec<(
            catabolic_store::catalog_identity_mutation::VersionChange,
            Option<catabolic_store::catalog_identity_mutation::IdentityEvidence>,
            String,
        )>,
    > = BTreeMap::new();
    for version in source_versions {
        let source = version["id"]
            .as_str()
            .context("invalid source version ID")?;
        let timeline_id = version["timeline_id"]
            .as_str()
            .context("invalid version timeline")?;
        let timeline = timelines_by_id
            .get(timeline_id)
            .context("missing version timeline")?;
        let (_, edition_owner) = owners
            .get(
                timeline["edition_id"]
                    .as_str()
                    .context("invalid timeline edition")?,
            )
            .context("missing version edition")?;
        let destination = format!("motion.version:{source}");
        ensure!(
            !batches
                .iter()
                .flat_map(|b| &b.items)
                .any(|i| i.item_id == destination || i.source_id == destination),
            "Motion version key collides with source owner"
        );
        let projection = json!({"contract_version":1,"source_version":version,"source_timeline":timeline,"source_bindings":source_bindings.iter().filter(|b|b["version_id"]==source).collect::<Vec<_>>(),"source_files":physical.iter().filter(|m|m.version_files.iter().any(|b|b["version_id"]==source)).map(|m|json!({"motion_file":m.motion_file,"native_profile":m.physical.profile,"native_exact":m.physical.exact,"source_occurrence_id":m.physical.source_occurrence_id})).collect::<Vec<_>>(),"revision_semantics":"source revisions preserved; native owner revisions start at one"});
        append(
            batches,
            ImportBatch {
                items: vec![ImportItem {
                    source_id: destination.clone(),
                    item_id: destination.clone(),
                    kind: "custom:motion_version".into(),
                    legacy_metadata: json!({}),
                }],
                typed: Some(TypedBatch {
                    metadata: vec![ImportedMetadata {
                        contract_version: metadata_version,
                        system_fields: if metadata_version == 2 {
                            vec!["motion_version".into()]
                        } else {
                            vec![]
                        },
                        item_id: destination.clone(),
                        policies: json!([{"field":"motion_version","value_type":"json","nullable":false,"normalization":"exact"}]),
                        contributions: vec![ImportedContribution {
                            namespace: "motion.structure".into(),
                            record_id: "source_version".into(),
                            source_revision: version["revision"]
                                .as_i64()
                                .context("invalid version revision")?,
                            kind: ImportedSourceKind::Scan,
                            fields: json!({"motion_version":projection}),
                            evidence: json!({"source_table":"media_versions","source_id":source}),
                        }],
                    }],
                    relationships: vec![ImportedRelationship {
                        source_relationship_id: format!("motion.version-owner:{source}"),
                        source_revision: version["revision"]
                            .as_i64()
                            .context("invalid version revision")?,
                        source_item_id: edition_owner.clone(),
                        target_item_id: destination,
                        kind: "custom:motion_version".into(),
                        position: None,
                        metadata: json!({"source_table":"media_versions","source_timeline_id":timeline_id}),
                    }],
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
        if let Some(candidate) = mapped_version(&version, timeline, edition_owner, physical)? {
            let expected = source_bindings
                .iter()
                .filter(|b| b["version_id"] == source)
                .count();
            if candidate.0.version.bindings.len() == expected {
                candidates
                    .entry(timeline_id.into())
                    .or_default()
                    .push(candidate);
            }
        }
    }
    for timeline in timelines {
        let edition = timeline["edition_id"]
            .as_str()
            .context("invalid timeline edition")?;
        let (work, owner) = owners
            .get(edition)
            .context("timeline edition absent from captured source")?;
        let timeline_id = timeline["id"].as_str().context("invalid timeline ID")?;
        let mut mapped = candidates.remove(timeline_id).unwrap_or_default();
        // Multiple provisional alternatives are not an equivalence declaration.
        if mapped.len() > 1 && mapped.iter().any(|(_, proof, _)| proof.is_none()) {
            mapped.clear();
        }
        if mapped.iter().any(|(v, _, _)| {
            v.version.state == catabolic_core::catalog_identity::VersionState::Draft
        }) {
            // A shared timeline cannot assert known complete coverage for one
            // encode while another retained source has unmeasured coverage.
            for (version, _, _) in &mut mapped {
                version.version.state = catabolic_core::catalog_identity::VersionState::Draft;
                version.version.declared_coverage = None;
            }
        }
        let coverage = mapped
            .first()
            .and_then(|(v, _, _)| v.version.declared_coverage);
        let established = mapped.iter().any(|(_, proof, _)| proof.is_some());
        for (version, proof, profile) in mapped {
            ensure!(
                profiles.get(work).is_none_or(|saved| saved == &profile),
                "source work spans multiple native identity profiles"
            );
            profiles.insert(work.clone(), profile);
            mapped_versions.insert(version.version.version_id.clone());
            version_changes
                .entry(work.clone())
                .or_default()
                .push(version);
            if let Some(proof) = proof {
                equivalence_evidence
                    .entry(work.clone())
                    .or_default()
                    .push(proof);
            }
        }
        contexts
            .entry(work.clone())
            .or_default()
            .push(TimelineChange {
                timeline_id: timeline_id.into(),
                edition_item_id: owner.clone(),
                expected_revision: None,
                identity_state: if established {
                    TimelineState::Established
                } else {
                    TimelineState::Provisional
                },
                coverage,
            });
    }
    for (work, timelines) in contexts {
        ensure!(
            timelines.len() <= 64,
            "Motion work exceeds native timeline mutation bound"
        );
        let mutation = IdentityMutation {
            contract_version: 1,
            profile: profiles.remove(&work).unwrap_or_else(|| "default".into()),
            context: ContextChange {
                id: format!("motion.work:{work}"),
                authority_namespace: HOST_NAMESPACE.into(),
                context_key: work.clone(),
                expected_revision: None,
            },
            timelines,
            versions: version_changes.remove(&work).unwrap_or_default(),
            evidence: equivalence_evidence.remove(&work).unwrap_or_default(),
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
    for alias in rows(db, "item_aliases", None).await? {
        let receipt_id = alias["receipt_id"]
            .as_str()
            .context("invalid alias receipt ID")?;
        let source_receipt_json: String = sqlx::query_scalar(
            "SELECT document_json FROM catalog_receipts WHERE id=? AND kind='merge'",
        )
        .bind(receipt_id)
        .fetch_one(&mut *db)
        .await?;
        append(
            batches,
            ImportBatch {
                typed: Some(TypedBatch {
                    logical_aliases: vec![ImportedLogicalAlias {
                        alias_item_id: alias["alias_id"]
                            .as_str()
                            .context("invalid alias source")?
                            .into(),
                        survivor_item_id: alias["item_id"]
                            .as_str()
                            .context("invalid alias survivor")?
                            .into(),
                        source_receipt_id: receipt_id.into(),
                        source_receipt_json,
                    }],
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
    }

    Ok(mapped_versions)
}

fn referenced_typed_items(snapshot: &Snapshot) -> anyhow::Result<BTreeSet<String>> {
    let owned: BTreeSet<String> = snapshot
        .batches
        .iter()
        .flat_map(|b| &b.items)
        .map(|i| i.item_id.clone())
        .collect();
    let mut needed = BTreeSet::new();
    for typed in snapshot.batches.iter().filter_map(|b| b.typed.as_ref()) {
        needed.extend(typed.metadata.iter().map(|m| m.item_id.clone()));
        needed.extend(typed.pins.iter().map(|p| p.item_id.clone()));
        needed.extend(typed.tags.iter().map(|t| t.item_id.clone()));
        for edge in &typed.relationships {
            needed.insert(edge.source_item_id.clone());
            needed.insert(edge.target_item_id.clone());
        }
        for alias in &typed.logical_aliases {
            needed.insert(alias.alias_item_id.clone());
            needed.insert(alias.survivor_item_id.clone());
        }
        if let Some(native) = &typed.native {
            for association in &native.associations {
                // Native owner validates the exact command; only the designated
                // subject field selects a dependency, not arbitrary metadata.
                needed.insert(association.item_id.clone());
            }
            for mutation in &native.mutations {
                let mutation: IdentityMutation = serde_json::from_value(mutation.clone())?;
                needed.extend(mutation.timelines.iter().map(|t| t.edition_item_id.clone()));
                for version in &mutation.versions {
                    needed.insert(version.version.association_item_id.clone());
                    needed.insert(version.version.edition_item_id.clone());
                }
            }
        }
    }
    Ok(needed.difference(&owned).cloned().collect())
}

pub(crate) async fn artwork_projection(
    db: &mut SqliteConnection,
    item: &str,
) -> anyhow::Result<Option<Value>> {
    let contributions: Vec<(String,String,String,i64)> = sqlx::query_as("SELECT role,source,asset_id,revision FROM artwork_contributions WHERE item_id=? ORDER BY role,source").bind(item).fetch_all(&mut *db).await?;
    let selections: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT role,asset_id,revision FROM artwork_selections WHERE item_id=? ORDER BY role",
    )
    .bind(item)
    .fetch_all(&mut *db)
    .await?;
    let associations: Vec<String> = sqlx::query_scalar(
        "SELECT asset_id FROM item_artwork_assets WHERE item_id=? ORDER BY asset_id",
    )
    .bind(item)
    .fetch_all(&mut *db)
    .await?;
    if contributions.is_empty() && selections.is_empty() && associations.is_empty() {
        return Ok(None);
    }
    let asset_ids: BTreeSet<String> = contributions
        .iter()
        .map(|(_, _, id, _)| id.clone())
        .chain(selections.iter().filter_map(|(_, id, _)| id.clone()))
        .chain(associations.iter().cloned())
        .collect();
    let mut assets = Vec::new();
    for id in asset_ids {
        let (mime, width, height, size): (String, i64, i64, i64) =
            sqlx::query_as("SELECT mime,width,height,length(bytes) FROM artwork_assets WHERE id=?")
                .bind(&id)
                .fetch_one(&mut *db)
                .await?;
        ensure!(
            (0..=8 * 1024 * 1024).contains(&size),
            "artwork source exceeds Motion asset byte contract"
        );
        let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM artwork_assets WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *db)
            .await?;
        assets.push(json!({"asset_id":id,"mime":mime,"width":width,"height":height,"size_bytes":size,"content_sha256":catabolic_core::sha256(&bytes),"owner":"motion_asset_service"}));
    }
    Ok(Some(json!({"contract_version":1,"item_id":item,
        "contributions":contributions.into_iter().map(|(role,source,asset_id,revision)|json!({"role":role,"source":source,"asset_id":asset_id,"revision":revision})).collect::<Vec<_>>(),
        "selections":selections.into_iter().map(|(role,asset_id,revision)|json!({"role":role,"asset_id":asset_id,"revision":revision})).collect::<Vec<_>>(),
        "associated_asset_ids":associations,"assets":assets,
        "semantics":"exact source references; null selected asset is explicit suppression; bytes remain Motion owned"})))
}

fn mapped_version(
    version: &Value,
    timeline: &Value,
    edition_owner: &str,
    physical: &[crate::occurrence_bridge::OccurrenceMapping],
) -> anyhow::Result<
    Option<(
        catabolic_store::catalog_identity_mutation::VersionChange,
        Option<catabolic_store::catalog_identity_mutation::IdentityEvidence>,
        String,
    )>,
> {
    use catabolic_core::catalog_identity::{
        EquivalenceBasis, EquivalenceEvidence, IntervalUs, SourceExtent, VersionBinding,
        VersionContract, VersionState,
    };
    use catabolic_store::catalog_identity_mutation::{
        EquivalenceClaim, IdentityEvidence, VersionChange,
    };
    let id = version["id"].as_str().context("invalid version ID")?;
    let timeline_id = timeline["id"].as_str().context("invalid timeline ID")?;
    let source_coverage = timeline["duration_ms"]
        .as_i64()
        .filter(|d| *d > 0)
        .map(|d| {
            d.checked_mul(1000)
                .context("source timeline duration overflow")
                .and_then(|d| Ok(IntervalUs::new(0, d)?))
        })
        .transpose()?;
    let mut records = Vec::new();
    let mut profiles = BTreeSet::new();
    for mapping in physical {
        for row in &mapping.version_files {
            if row["version_id"] != id {
                continue;
            }
            if row["file_revision"] != mapping.motion_file["revision"] {
                return Ok(None);
            }
            profiles.insert(mapping.physical.profile.clone());
            records.push((row, mapping));
        }
    }
    if records.is_empty() || profiles.len() != 1 {
        return Ok(None);
    }
    records.sort_by_key(|(r, _)| r["part"].as_u64());
    let mut bindings = Vec::new();
    let mut cursor = Some(0_i64);
    let mut source_bindings = Vec::new();
    for (index, (row, mapping)) in records.into_iter().enumerate() {
        let part = row["part"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .context("invalid version part")?;
        if part as usize != index + 1 {
            return Ok(None);
        }
        let source = match (row["start_ms"].as_i64(), row["end_ms"].as_i64()) {
            (Some(start), Some(end)) => SourceExtent::Slice {
                interval: IntervalUs::new(
                    start
                        .checked_mul(1000)
                        .context("source interval overflow")?,
                    end.checked_mul(1000).context("source interval overflow")?,
                )?,
            },
            (None, None) if row["start_ms"].is_null() && row["end_ms"].is_null() => {
                let duration_us = match mapping.motion_file["duration_seconds"].as_f64() {
                    None if mapping.motion_file["duration_seconds"].is_null() => None,
                    Some(seconds) => {
                        let micros = seconds * 1_000_000.0;
                        if !micros.is_finite()
                            || micros <= 0.0
                            || micros >= i64::MAX as f64
                            || micros.fract() != 0.0
                        {
                            return Ok(None);
                        }
                        Some(micros as i64)
                    }
                    _ => return Ok(None),
                };
                SourceExtent::WholeFile { duration_us }
            }
            _ => return Ok(None),
        };
        let placement = match (source.interval()?, cursor) {
            (Some(extent), Some(start)) => Some(extent.place_at(start)?),
            _ => None,
        };
        if matches!(source, SourceExtent::Slice { .. }) && placement.is_none() {
            return Ok(None);
        }
        cursor = placement.map(|p| p.end_us());
        let binding_id = format!("motion.binding:{id}:{part}");
        bindings.push(VersionBinding {
            binding_id,
            association_id: format!("motion.version-file:{id}:{part}"),
            file: mapping.physical.exact.clone(),
            part,
            source,
            timeline_interval: placement,
        });
        source_bindings.push(json!({"version_file":row,"motion_file_id":mapping.motion_file["id"],"motion_file_revision":mapping.motion_file["revision"],"native_exact":mapping.physical.exact}));
    }
    let ready = source_coverage.is_some_and(|coverage| cursor == Some(coverage.end_us()));
    let owner = format!("motion.version:{id}");
    let contract = VersionContract {
        contract_version: 1,
        version_id: id.into(),
        association_item_id: owner,
        edition_item_id: edition_owner.into(),
        timeline_id: timeline_id.into(),
        state: if ready {
            VersionState::Ready
        } else {
            VersionState::Draft
        },
        declared_coverage: if ready { source_coverage } else { None },
        bindings,
    };
    catabolic_core::catalog_identity::validate_version(&contract)?;
    let basis = match version["equivalence"].as_str() {
        Some("declared") => Some(EquivalenceBasis::Declared),
        Some("verified") => Some(EquivalenceBasis::Verified),
        Some("unknown") => None,
        _ => anyhow::bail!("invalid source version equivalence"),
    };
    let proof = basis.map(|basis| IdentityEvidence { id:format!("motion.equivalence:{id}"),basis,claims:vec![EquivalenceClaim { edition_item_id:edition_owner.into(),timeline_id:timeline_id.into(),timeline_revision:1,version_id:id.into(),version_revision:1,exact_files:contract.bindings.iter().map(|b|b.file.clone()).collect() }],evidence:json!({"source_table":"media_versions","source_version":version,"source_timeline":timeline,"source_bindings":source_bindings,"meaning":"retained accepted source equivalence; no equivalence inferred from paths, hashes or measured duration"}) });
    let change = VersionChange {
        expected_revision: None,
        expected_timeline_revision: 1,
        expected_binding_revisions: contract
            .bindings
            .iter()
            .map(|b| (b.binding_id.clone(), None))
            .collect(),
        version: contract,
        equivalence: proof.as_ref().map(|p| EquivalenceEvidence {
            basis: p.basis,
            evidence_id: p.id.clone(),
        }),
    };
    Ok(Some((
        change,
        proof,
        profiles.into_iter().next().expect("one profile"),
    )))
}

/// Keep every new identity context with the exact occurrence/association closure
/// it consumes. Item owners can be earlier receipt-verified partitions; a native
/// association map cannot be fabricated from a reference in a later snapshot.
fn repack_native_groups(batches: &mut Vec<ImportBatch>) -> anyhow::Result<Vec<(usize, usize)>> {
    let mut occurrences = BTreeMap::new();
    let mut associations = BTreeMap::new();
    let mut mutations = Vec::new();
    for batch in batches.iter_mut() {
        if let Some(typed) = batch.typed.as_mut() {
            if let Some(native) = typed.native.take() {
                for occurrence in native.occurrences {
                    ensure!(
                        occurrences
                            .insert(occurrence.source_occurrence_id.clone(), occurrence)
                            .is_none(),
                        "duplicate captured occurrence"
                    );
                }
                for association in native.associations {
                    ensure!(
                        associations
                            .insert(association.source_association_id.clone(), association)
                            .is_none(),
                        "duplicate captured association"
                    );
                }
                mutations.extend(native.mutations);
            }
            if typed == &TypedBatch::default() {
                batch.typed = None;
            }
        }
    }
    batches
        .retain(|b| !b.items.is_empty() || !b.unresolved_evidence.is_empty() || b.typed.is_some());
    let mut groups = Vec::new();
    for raw in mutations {
        let mutation: IdentityMutation = serde_json::from_value(raw.clone())?;
        let mut group = Vec::new();
        let required: BTreeSet<String> = mutation
            .versions
            .iter()
            .flat_map(|v| &v.version.bindings)
            .map(|b| b.association_id.clone())
            .collect();
        let mut group_occurrences = BTreeSet::new();
        for source_id in required {
            let association = associations
                .remove(&source_id)
                .context("identity binding missing captured association")?;
            if group_occurrences.insert(association.source_occurrence_id.clone()) {
                let occurrence=occurrences.remove(&association.source_occurrence_id).context("identity contexts share an occurrence; explicit context continuation is required")?;
                append(
                    &mut group,
                    ImportBatch {
                        typed: Some(TypedBatch {
                            native: Some(NativeIdentityBatch {
                                occurrences: vec![occurrence],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..empty()
                    },
                )?;
            }
            append(
                &mut group,
                ImportBatch {
                    typed: Some(TypedBatch {
                        native: Some(NativeIdentityBatch {
                            associations: vec![association],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..empty()
                },
            )?;
        }
        append(
            &mut group,
            ImportBatch {
                typed: Some(TypedBatch {
                    native: Some(NativeIdentityBatch {
                        mutations: vec![raw],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..empty()
            },
        )?;
        ensure!(
            group.len() <= catalog_import::MAX_IMPORT_BATCHES,
            "one exact native identity context exceeds import partition bound; no context split or source rows discarded"
        );
        let start = batches.len();
        batches.extend(group);
        groups.push((start, batches.len()));
    }
    ensure!(
        associations.is_empty(),
        "captured associations lack exact typed bindings"
    );
    for (_, occurrence) in occurrences {
        let start = batches.len();
        batches.push(ImportBatch {
            typed: Some(TypedBatch {
                native: Some(NativeIdentityBatch {
                    occurrences: vec![occurrence],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..empty()
        });
        groups.push((start, batches.len()));
    }
    Ok(groups)
}
fn bounded_partition_ranges(
    batches: &[ImportBatch],
    groups: &[(usize, usize)],
) -> anyhow::Result<Vec<(usize, usize)>> {
    if batches.is_empty() {
        return Ok(vec![(0, 0)]);
    }
    // Native hard limits remain unchanged. Host partitions additionally bound
    // owner work per receipt so a large catalog cannot monopolize one operation.
    const OWNER_TARGET: usize = 64;
    const TYPED_TARGET: usize = 128;
    const BYTE_TARGET: usize = 256 * 1024;
    let mut result = Vec::new();
    let mut start = 0;
    while start < batches.len() {
        let mut end = start;
        let mut items = 0;
        let mut typed = 0;
        let mut bytes = 0;
        while end < batches.len() {
            let unit_end = groups
                .iter()
                .find(|&&(a, _)| a == end)
                .map(|&(_, b)| b)
                .unwrap_or(end + 1);
            ensure!(
                unit_end <= batches.len() && unit_end > end,
                "invalid native dependency group"
            );
            let unit = &batches[end..unit_end];
            let unit_items = unit.iter().map(|b| b.items.len()).sum::<usize>();
            let unit_typed = unit
                .iter()
                .map(|b| b.typed.as_ref().map_or(0, |t| t.record_count()))
                .sum::<usize>();
            let unit_bytes = unit
                .iter()
                .map(|b| serde_json::to_vec(b).map(|v| v.len()))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .sum::<usize>();
            if end > start
                && (unit_end - start > catalog_import::MAX_IMPORT_BATCHES
                    || items + unit_items > OWNER_TARGET
                    || typed + unit_typed > TYPED_TARGET
                    || bytes + unit_bytes > BYTE_TARGET)
            {
                break;
            }
            ensure!(
                unit_end - start <= catalog_import::MAX_IMPORT_BATCHES,
                "one exact native identity context exceeds hard partition bound"
            );
            items += unit_items;
            typed += unit_typed;
            bytes += unit_bytes;
            end = unit_end;
            if items >= OWNER_TARGET || typed >= TYPED_TARGET || bytes >= BYTE_TARGET {
                break;
            }
        }
        ensure!(end > start, "no bounded export partition progress");
        result.push((start, end));
        start = end;
    }
    Ok(result)
}

#[cfg(test)]
mod metadata_contract_tests {
    use super::*;

    fn source(namespace: &str, record_id: &str, fields: Value) -> ImportedContribution {
        ImportedContribution {
            namespace: namespace.into(),
            record_id: record_id.into(),
            source_revision: 1,
            kind: ImportedSourceKind::Scan,
            fields,
            evidence: json!({}),
        }
    }

    #[test]
    fn v1_metadata_keeps_exact_legacy_serialization() {
        let original = json!({"item_id":"item","policies":[],"contributions":[]});
        let metadata: ImportedMetadata = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(metadata.contract_version, 1);
        assert!(metadata.system_fields.is_empty());
        assert_eq!(serde_json::to_value(metadata).unwrap(), original);
    }

    #[test]
    fn v2_system_grants_require_exact_exporter_source_not_field_prefix() {
        let structural = source(
            "motion.structure",
            "source_projection",
            json!({"motion_catalog":{}}),
        );
        assert_eq!(
            import_system_fields(2, &[structural.clone()]).unwrap(),
            vec!["motion_catalog"]
        );
        assert!(import_system_fields(1, &[structural]).unwrap().is_empty());
        for (namespace, record) in [
            ("motion.local", "document"),
            ("motion.structure", "document"),
            ("motion.unknown", "source_projection"),
        ] {
            assert!(
                trusted_source_system_fields(&source(
                    namespace,
                    record,
                    json!({"motion_catalog":{}})
                ))
                .is_err()
            );
        }
        let mut provider = source(
            "motion.structure",
            "source_projection",
            json!({"motion_catalog":{}}),
        );
        provider.kind = ImportedSourceKind::Provider;
        assert!(trusted_source_system_fields(&provider).is_err());
        assert!(
            trusted_source_system_fields(&source(
                "motion.local",
                "document",
                json!({"motion_user_field":"ordinary"})
            ))
            .is_err()
        );
    }
}
