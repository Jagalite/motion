//! Read-only inventory over an immutable, provenance-checked Motion export.
//! Inventory is not an activation grant. In particular, retained IDs and JSON
//! paths do not establish native identity or physical equivalence.
use crate::{
    NativeCatalog,
    motion_export::{evidence, rows},
};
use anyhow::{Context, ensure};
use catabolic_store::catalog_import::{EvidenceKind, ImportBatch};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

const TARGETS: &[&str] = &[
    "items",
    "editions",
    "timelines",
    "media_versions",
    "media_files",
    "libraries",
    "catalog_libraries",
];
const SCHEMA_ID: &str = "motion.reference-schema.v1";
// Known application documents. Extract catalog references, not complete payloads:
// acknowledgement documents can contain content URLs/tickets that must not be copied.
const DOCUMENTS: &[(&str, &str)] = &[
    ("metadata_documents", "document_json"),
    ("catalog_receipts", "document_json"),
    ("match_proposals", "candidates_json"),
    ("match_proposals", "decision_json"),
    ("legacy_progress_attribution", "candidates_json"),
    ("delivery_sessions", "state_json"),
    ("delivery_admissions", "acknowledgement_json"),
    ("viewing_events", "acknowledgement_json"),
    ("offline_events", "event_json"),
    ("saved_filters", "terms_json"),
    ("playback_preferences", "document_json"),
    ("renditions", "recipe_json"),
];

// Exact supported schema inventory. Unknown tables are not silently classified
// as application-owned or harmless merely because no declared FK exists.
const KNOWN_TABLES: &[&str] = &[
    "_sqlx_migrations",
    "access_invitations",
    "access_owner_setup",
    "access_users",
    "api_schedules",
    "artwork_assets",
    "artwork_contributions",
    "artwork_selections",
    "backup_jobs",
    "catabolic_binding",
    "catabolic_commands",
    "catabolic_command_attempts",
    "catabolic_imported_source_descriptors",
    "catabolic_generated_source",
    "catabolic_generated_publications",
    "catabolic_processing_admissions",
    "catabolic_aggregate_issuance",
    "catabolic_native_upgrade_intent",
    "catabolic_native_upgrade_ack",
    "catabolic_cutover",
    "catabolic_cutover_sets",
    "catabolic_cutover_parts",
    "catabolic_export_freezes",
    "catabolic_export_plans",
    "catabolic_export_plan_parts",
    "catabolic_import_snapshots",
    "catabolic_motion_exports",
    "catabolic_occurrence_mappings",
    "catabolic_reference_audits",
    "catabolic_server_scopes",
    "catabolic_source_events",
    "catabolic_source_guards",
    "catabolic_source_mappings",
    "catabolic_source_state",
    "catabolic_source_registry",
    "catabolic_projection_state",
    "catabolic_projection_items",
    "catabolic_discovery_intents",
    "catabolic_projection_versions",
    "catabolic_projection_files",
    "catabolic_projection_file_libraries",
    "catabolic_projection_generated_libraries",
    "catabolic_source_lifecycle",
    "catabolic_scan_schedule_adoptions",
    "catabolic_scan_schedule_definitions",
    "catabolic_scan_schedule_cursors",
    "catabolic_scan_schedule_occurrences",
    "catabolic_source_watch_definitions",
    "catabolic_source_watch_admissions",
    "catabolic_source_registration_intents",
    "catabolic_revision_bridges",
    "catabolic_scan_requests",
    "catabolic_scan_demands",
    "catalog_libraries",
    "catalog_receipts",
    "change_events",
    "collection_items",
    "collections",
    "content_tickets",
    "credentials",
    "delivery_admissions",
    "delivery_owners",
    "delivery_sessions",
    "devices",
    "download_subtitle_artifacts",
    "download_transfers",
    "downloads",
    "editions",
    "idempotency_records",
    "invitation_attempts",
    "item_aliases",
    "item_artwork_assets",
    "item_origins",
    "item_relationships",
    "item_structure",
    "items",
    "jobs",
    "legacy_progress_attribution",
    "libraries",
    "library_sources",
    "maintenance_state",
    "markers",
    "match_proposals",
    "media_files",
    "media_versions",
    "metadata_documents",
    "metadata_jobs",
    "nfo_origins",
    "offline_events",
    "offline_lineage",
    "offline_sequences",
    "operational_metric_counters",
    "operational_metric_failures",
    "operational_metrics",
    "order_groups",
    "pairings",
    "playback_monitor_sessions",
    "playback_preferences",
    "playback_sessions",
    "playlist_entries",
    "playlists",
    "preparations",
    "preview_jobs",
    "preview_retention_cursor",
    "previews",
    "preview_sheet_artifacts",
    "processing_jobs",
    "profiles",
    "progress",
    "queue_entries",
    "queues",
    "registration_activations",
    "renditions",
    "restore_baseline_epoch",
    "restore_baselines",
    "saved_filters",
    "scan_admissions",
    "scan_demands",
    "scan_incomplete_directories",
    "scan_requests",
    "scan_schedules",
    "schedule_dispatch_progress",
    "search_dirty",
    "search_fts",
    "search_fts_config",
    "search_fts_content",
    "search_fts_data",
    "search_fts_docsize",
    "search_fts_idx",
    "search_rows",
    "server_identity",
    "server_viewing_histories",
    "sidecar_subtitles",
    "signin_audit",
    "signin_families",
    "signin_identities",
    "signin_limits",
    "signin_mail_tests",
    "signin_outbox",
    "signin_proofs",
    "signin_providers",
    "signin_requests",
    "signin_settings",
    "signin_transactions",
    "timeline_viewing",
    "timelines",
    "version_files",
    "viewing_events",
    "viewing_sessions",
    "viewing_state",
];
// Only host control and schema/configuration rows may survive the deliberately
// narrow empty-catalog cutover. Other historical/application rows stay preserved
// and prevent this path; a later full mapping contract must qualify them.
const EMPTY_EXEMPT: &[&str] = &[
    "_sqlx_migrations",
    "profiles",
    "invitation_attempts",
    "preview_retention_cursor",
    "scan_admissions",
    "server_identity",
    "server_viewing_histories",
    "signin_settings",
    "search_fts_config",
    "search_fts_data",
    "search_fts_idx",
    "search_fts_docsize",
    "search_fts_content",
    "catabolic_binding",
    "catabolic_commands",
    "catabolic_command_attempts",
    "catabolic_imported_source_descriptors",
    "catabolic_generated_source",
    "catabolic_generated_publications",
    "catabolic_processing_admissions",
    "catabolic_aggregate_issuance",
    "catabolic_native_upgrade_intent",
    "catabolic_native_upgrade_ack",
    "catabolic_cutover",
    "catabolic_cutover_sets",
    "catabolic_cutover_parts",
    "catabolic_export_freezes",
    "catabolic_export_plans",
    "catabolic_export_plan_parts",
    "catabolic_import_snapshots",
    "catabolic_motion_exports",
    "catabolic_occurrence_mappings",
    "catabolic_source_events",
    "catabolic_source_guards",
    "catabolic_source_mappings",
    "catabolic_source_state",
    "catabolic_source_registry",
    "catabolic_projection_state",
    "catabolic_projection_items",
    "catabolic_discovery_intents",
    "catabolic_projection_versions",
    "catabolic_projection_files",
    "catabolic_projection_file_libraries",
    "catabolic_projection_generated_libraries",
    "catabolic_source_lifecycle",
    "catabolic_scan_schedule_adoptions",
    "catabolic_scan_schedule_definitions",
    "catabolic_scan_schedule_cursors",
    "catabolic_scan_schedule_occurrences",
    "catabolic_source_watch_definitions",
    "catabolic_source_watch_admissions",
    "catabolic_source_registration_intents",
    "catabolic_revision_bridges",
    "catabolic_scan_requests",
    "catabolic_scan_demands",
    "catabolic_reference_audits",
];
// These schema-checked rows remain exclusively Motion authentication state.
// Their bytes are neither exported nor interpreted as native import grants.
// Motion's current auth resolver must still deny unknown/stale scope IDs.
const RETAINED_AUTH_TABLES: &[&str] = &[
    "access_invitations",
    "access_owner_setup",
    "access_users",
    "credentials",
    "devices",
    "pairings",
    "signin_audit",
    "signin_families",
    "signin_identities",
    "signin_limits",
    "signin_mail_tests",
    "signin_outbox",
    "signin_proofs",
    "signin_providers",
    "signin_requests",
    "signin_settings",
    "signin_transactions",
];
// Explicitly qualified logical application ownership. These rows remain in
// Motion, while their item identifiers are preserved by native work owners.
pub(crate) const PHYSICAL_HISTORY_TABLES: &[&str] = &[
    "downloads",
    "viewing_sessions",
    "viewing_events",
    "offline_sequences",
    "offline_lineage",
    "offline_events",
];
pub(crate) const LOGICAL_APPLICATION_TABLES: &[&str] = &[
    "progress",
    "viewing_state",
    "collections",
    "collection_items",
    "change_events",
    "search_dirty",
    "search_rows",
    "search_fts",
];
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceAudit {
    pub contract_version: u32,
    #[serde(default)]
    pub(crate) origin_preservation: Option<crate::origin_preservation::Contract>,
    pub slots: Vec<ReferenceSlot>,
    pub table_rows: BTreeMap<String, u64>,
    pub unknown_tables: Vec<String>,
    pub schema_mismatches: Vec<String>,
    pub logical_only_supported: bool,
    #[serde(default)]
    pub logical_root_structure_supported: bool,
    #[serde(default)]
    pub retained_zero_effect_receipts: Vec<Value>,
    #[serde(default)]
    pub retained_pairing_receipts: u64,
    #[serde(default)]
    pub artwork_references_supported: bool,
    #[serde(default)]
    pub retained_schedules_supported: bool,
    #[serde(default)]
    pub retained_schedule_receipts: u64,
    #[serde(default)]
    pub retained_source_creation_receipts: u64,
    #[serde(default)]
    pub retained_catalog_artwork_receipts: u64,
    #[serde(default)]
    pub physical_history_supported: bool,
    #[serde(default)]
    pub retained_download_receipts: u64,
    #[serde(default)]
    pub physical_coverage: PhysicalCoverage,
    pub resolved_evidence: Vec<Value>,
    pub atomic_batch_groups: Vec<(usize, usize)>,
}
/// Source-to-native coverage is diagnostic evidence, never an activation grant.
/// Native validation and the qualified serving projection are separate obligations.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalCoverage {
    pub preserved_ids: BTreeMap<String, BTreeSet<String>>,
    pub missing_ids: BTreeMap<String, BTreeSet<String>>,
    /// Descriptive native owners only. Never included in preserved/current IDs.
    #[serde(default)]
    pub historical_ids: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub historical_reference_slots: BTreeMap<String, ReferenceCoverage>,
    /// Preserved source references only; deliberately not activation eligibility.
    #[serde(default)]
    pub historical_reference_contract: Option<u32>,
    /// IDs absent from the frozen catalog, owned only as historical references.
    #[serde(default)]
    pub tombstone_ids: BTreeMap<String, BTreeSet<String>>,
    pub unresolved_occurrences: BTreeMap<String, String>,
    pub serving_projection_contract: Option<u32>,
    #[serde(default)]
    pub source_reference_contract: Option<u32>,
    #[serde(default)]
    pub unassociated_files: BTreeSet<String>,
    #[serde(default)]
    pub ownership_mismatches: BTreeSet<String>,
    #[serde(default)]
    pub reference_slots: BTreeMap<String, ReferenceCoverage>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCoverage {
    pub resolved: u64,
    pub nullable_absent: u64,
    pub unmapped: u64,
    pub malformed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReferenceResolution {
    Resolved,
    NullableAbsent,
    Unmapped,
    Malformed,
}
fn resolve_reference(
    value: Option<&Value>,
    nullable: bool,
    imported: Option<&BTreeSet<String>>,
) -> ReferenceResolution {
    match value {
        Some(Value::Null) if nullable => ReferenceResolution::NullableAbsent,
        Some(Value::String(id)) if imported.is_some_and(|ids| ids.contains(id)) => {
            ReferenceResolution::Resolved
        }
        Some(Value::String(_)) => ReferenceResolution::Unmapped,
        _ => ReferenceResolution::Malformed,
    }
}
/// Diagnostic only. A resolved scalar ID does not discharge embedded documents,
/// source membership, native current-revision checks, or the serving contract.
pub(crate) fn historical_tombstone_slot(slot: &ReferenceSlot) -> bool {
    !slot.declared_foreign_key
        && matches!(
            (
                slot.table.as_str(),
                slot.column.as_str(),
                slot.target_table.as_str()
            ),
            (
                "timeline_viewing" | "viewing_sessions" | "offline_events" | "offline_lineage",
                "timeline_id",
                "timelines"
            ) | ("downloads", "timeline_id", "timelines")
                | ("downloads", "version_id", "media_versions")
                | ("downloads", "file_id", "media_files")
        )
}
pub(crate) async fn capture_physical_reference_coverage(
    db: &mut SqliteConnection,
    slots: &[ReferenceSlot],
    coverage: &mut PhysicalCoverage,
) -> anyhow::Result<()> {
    capture_reference_coverage(db, slots, coverage, false).await
}
pub(crate) async fn capture_historical_reference_coverage(
    db: &mut SqliteConnection,
    slots: &[ReferenceSlot],
    coverage: &mut PhysicalCoverage,
) -> anyhow::Result<()> {
    capture_reference_coverage(db, slots, coverage, true).await
}
async fn capture_reference_coverage(
    db: &mut SqliteConnection,
    slots: &[ReferenceSlot],
    coverage: &mut PhysicalCoverage,
    historical: bool,
) -> anyhow::Result<()> {
    let tables: BTreeSet<_> = slots.iter().map(|slot| slot.table.as_str()).collect();
    for table in tables {
        let table_slots: Vec<_> = slots.iter().filter(|slot| slot.table == table).collect();
        let info = sqlx::query(&format!("PRAGMA table_info({})", quote(table)))
            .fetch_all(&mut *db)
            .await?;
        let nullable: BTreeSet<String> = info
            .iter()
            .filter(|col| col.get::<i64, _>("notnull") == 0 && col.get::<i64, _>("pk") == 0)
            .map(|col| col.get::<String, _>("name"))
            .collect();
        let columns: BTreeSet<String> =
            table_slots.iter().map(|slot| slot.column.clone()).collect();
        let admitted: BTreeMap<(&str, &str), BTreeSet<String>> = table_slots
            .iter()
            .map(|slot| {
                let mut ids = coverage
                    .preserved_ids
                    .get(&slot.target_table)
                    .cloned()
                    .unwrap_or_default();
                if historical && historical_tombstone_slot(slot) {
                    if let Some(tombstones) = coverage.tombstone_ids.get(&slot.target_table) {
                        ids.extend(tombstones.iter().cloned());
                    }
                }
                ((slot.column.as_str(), slot.target_table.as_str()), ids)
            })
            .collect();
        for row in rows(db, table, Some(&columns)).await? {
            for slot in &table_slots {
                let result = resolve_reference(
                    row.get(&slot.column),
                    nullable.contains(&slot.column),
                    admitted.get(&(slot.column.as_str(), slot.target_table.as_str())),
                );
                let summary = coverage
                    .reference_slots
                    .entry(format!(
                        "{}.{}->{}.{}",
                        slot.table, slot.column, slot.target_table, slot.target_column
                    ))
                    .or_default();
                match result {
                    ReferenceResolution::Resolved => summary.resolved += 1,
                    ReferenceResolution::NullableAbsent => summary.nullable_absent += 1,
                    ReferenceResolution::Unmapped => summary.unmapped += 1,
                    ReferenceResolution::Malformed => summary.malformed += 1,
                }
            }
        }
    }
    Ok(())
}

/// Retained application history never contributes a physical grant. Validate
/// only the supported application relationships and document shape here; the
/// exhaustive scalar catalog-ID proof remains a separate required condition.
pub(crate) async fn physical_history_supported(db: &mut SqliteConnection) -> anyhow::Result<bool> {
    let downloads = rows(db, "downloads", None).await?;
    let profiles: BTreeSet<String> = sqlx::query_scalar("SELECT id FROM profiles")
        .fetch_all(&mut *db)
        .await?
        .into_iter()
        .collect();
    let devices: BTreeSet<String> = sqlx::query_scalar("SELECT id FROM devices")
        .fetch_all(&mut *db)
        .await?
        .into_iter()
        .collect();
    let has = |set: &BTreeSet<String>, row: &Value, key: &str| {
        row[key].as_str().is_some_and(|id| set.contains(id))
    };
    for row in &downloads {
        // Subtitle track identity/publication has a separate unqualified owner.
        if !row["subtitle_track_id"].is_null()
            || !has(&profiles, row, "profile_id")
            || !has(&devices, row, "device_id")
        {
            return Ok(false);
        }
    }
    for table in ["offline_sequences", "offline_lineage", "offline_events"] {
        for row in rows(db, table, None).await? {
            if !has(&profiles, &row, "profile_id") || !has(&devices, &row, "device_id") {
                return Ok(false);
            }
            if table == "offline_events" && !qualified_offline_event(&row, &downloads) {
                return Ok(false);
            }
        }
    }
    let sessions = rows(db, "viewing_sessions", None).await?;
    for row in &sessions {
        // Motion retains the session authority. Future record/rebind calls
        // still reauthorize the profile/timeline and execute core epoch,
        // sequence, closed-status and delivery-generation rules.
        if !has(&profiles, row, "profile_id")
            || !matches!(
                row["status"].as_str(),
                Some("playing" | "paused" | "ended" | "stopped" | "superseded")
            )
        {
            return Ok(false);
        }
    }
    for row in rows(db, "viewing_events", None).await? {
        if !sessions
            .iter()
            .any(|session| qualified_viewing_ack(&row, session))
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    })
}
fn decimal(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && text.parse::<u64>().is_ok()
    })
}
fn qualified_viewing_ack(row: &Value, session: &Value) -> bool {
    if row["session_id"] != session["id"] {
        return false;
    }
    let Some(raw) = row["acknowledgement_json"].as_str() else {
        return false;
    };
    let Ok(ack) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let recorded = &ack["session"];
    let view = &ack["viewing"];
    if !exact_keys(&ack, &["session", "viewing", "duplicate"])
        || ack["duplicate"] != json!(false)
        || !exact_keys(
            recorded,
            &[
                "id",
                "revision",
                "profile_id",
                "timeline_id",
                "delivery_id",
                "sequence",
                "manual_epoch",
                "position_ms",
                "status",
            ],
        )
        || !exact_keys(
            view,
            &[
                "profile_id",
                "timeline_id",
                "revision",
                "manual_epoch",
                "position_ms",
                "watched",
                "manual_watched",
                "session_id",
            ],
        )
    {
        return false;
    }
    recorded["id"] == session["id"]
        && [recorded, view].iter().all(|object| {
            object["profile_id"] == session["profile_id"]
                && object["timeline_id"] == session["timeline_id"]
                && decimal(&object["revision"])
                && decimal(&object["manual_epoch"])
                && object["position_ms"].as_u64().is_some()
        })
        && recorded["sequence"]
            .as_str()
            .and_then(|text| text.parse::<u64>().ok())
            == row["sequence"].as_u64()
        && recorded["position_ms"] == row["position_ms"]
        && recorded["status"] == row["status"]
        && matches!(
            recorded["status"].as_str(),
            Some("playing" | "paused" | "ended" | "stopped")
        )
        && recorded["delivery_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
        && view["watched"].is_boolean()
        && (view["manual_watched"].is_null() || view["manual_watched"].is_boolean())
        && (view["session_id"].is_null() || view["session_id"] == session["id"])
}

pub(crate) async fn retained_download_receipts(db: &mut SqliteConnection) -> anyhow::Result<u64> {
    let downloads = rows(db, "downloads", None).await?;
    let receipts=sqlx::query("SELECT principal_id,target,status,body,issued_nonce IS NULL AS no_nonce FROM idempotency_records WHERE operation='createDownload'").fetch_all(&mut *db).await?;
    let mut qualified = 0;
    for receipt in receipts {
        if receipt.get::<i64, _>("status") != 202 || receipt.get::<i64, _>("no_nonce") != 1 {
            continue;
        }
        let Some(body) = receipt.get::<Option<String>, _>("body") else {
            continue;
        };
        let Ok(body) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        if downloads.iter().any(|download| {
            qualified_download_receipt(
                &body,
                receipt.get::<&str, _>("principal_id"),
                receipt.get::<&str, _>("target"),
                download,
            )
        }) {
            qualified += 1;
        }
    }
    Ok(qualified)
}
fn qualified_download_receipt(
    body: &Value,
    principal: &str,
    device: &str,
    download: &Value,
) -> bool {
    if download["principal_id"].as_str() != Some(principal)
        || download["device_id"].as_str() != Some(device)
        || !download["subtitle_track_id"].is_null()
    {
        return false;
    }
    let Some(size) = download["size_bytes"].as_i64() else {
        return false;
    };
    // createDownload only replays this retained identity, then independently
    // reauthorizes ownership and computes current state. No old URL/byte grant
    // or catalog mutation result is replayed across cutover.
    body == &json!({"id":download["id"],"revision":"1","device_id":device,
        "profile_id":download["profile_id"],"timeline_id":download["timeline_id"],
        "state":"ready","manifest_asset_id":null,"size_bytes":size.to_string(),
        "job_id":null,"subtitle_track_id":null,"device_transfer":null})
}

fn qualified_offline_event(row: &Value, downloads: &[Value]) -> bool {
    if !matches!(
        row["outcome"].as_str(),
        Some("applied" | "history_only" | "rejected")
    ) || !(row["code"].is_null()
        || matches!(
            row["code"].as_str(),
            Some(
                "not_found"
                    | "invalid_event"
                    | "invalid_position"
                    | "timeline_changed"
                    | "manual_override_newer"
                    | "viewing_newer"
            )
        ))
    {
        return false;
    }
    let Some(raw) = row["event_json"].as_str() else {
        return false;
    };
    let Ok(document) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let Some(wrapper) = document.as_object() else {
        return false;
    };
    if wrapper.len() != 1 {
        return false;
    }
    let Some(event) = wrapper.get("event") else {
        return false;
    };
    let Ok(parsed) =
        serde_json::from_value::<playscale_core::downloads::OfflineEvent>(event.clone())
    else {
        return false;
    };
    // Round-trip rejects unknown nested keys without exporting the raw document.
    if serde_json::to_value(&parsed).ok().as_ref() != Some(event)
        || row["event_id"].as_str() != Some(parsed.id.as_str())
        || row["sequence"].as_u64() != Some(parsed.sequence)
        || row["timeline_id"].as_str() != Some(parsed.timeline_id.as_str())
    {
        return false;
    }
    downloads.iter().any(|download| {
        download["id"].as_str() == Some(parsed.download_id.as_str())
            && download["device_id"] == row["device_id"]
            && download["profile_id"] == row["profile_id"]
            && download["timeline_id"] == row["timeline_id"]
    })
}

// Only these reviewed additive control tables may be absent from an immutable
// pre-upgrade audit. Their current schema/activation transition is independently
// validated by the sealed control migrator. Never generalize this by prefix.
const POST_V84_CONTROL_TABLES: &[&str] = &[
    "catabolic_command_attempts",
    "catabolic_imported_source_descriptors",
    "catabolic_generated_source",
    "catabolic_generated_publications",
    "catabolic_processing_admissions",
    "catabolic_aggregate_issuance",
    "catabolic_native_upgrade_intent",
    "catabolic_native_upgrade_ack",
    "catabolic_projection_file_libraries",
    "catabolic_projection_generated_libraries",
    "catabolic_source_lifecycle",
    "catabolic_scan_schedule_adoptions",
    "catabolic_scan_schedule_definitions",
    "catabolic_scan_schedule_cursors",
    "catabolic_scan_schedule_occurrences",
    "catabolic_source_watch_definitions",
    "catabolic_source_watch_admissions",
    "preview_sheet_artifacts",
];

impl SourceAudit {
    pub(crate) fn source_qualification_supported(&self) -> bool {
        self.empty_catalog()
            || (self.logical_only_supported && self.logical_only_candidate())
            || (self.physical_coverage.source_reference_contract == Some(1)
                && self.physical_source_candidate())
            || (self.physical_coverage.source_reference_contract == Some(2)
                && self.physical_coverage.historical_reference_contract == Some(1)
                && self.historical_source_candidate())
    }

    fn zero_effect_receipts_only(&self) -> bool {
        self.table_rows
            .get("catalog_receipts")
            .copied()
            .unwrap_or(0)
            == self.retained_zero_effect_receipts.len() as u64
            && [
                "legacy_progress_attribution",
                "timeline_viewing",
                "viewing_sessions",
            ]
            .iter()
            .all(|table| self.table_rows.get(*table) == Some(&0))
            && zero_effect_upgrade_receipts(&self.retained_zero_effect_receipts)
    }

    /// A complete source reference proof is still not serving authority. This
    /// excludes aliases, artifacts, arbitrary acknowledgments, and physical
    /// changes that cannot be represented by exact native typed owners.
    pub(crate) fn historical_source_candidate(&self) -> bool {
        if self
            .physical_coverage
            .historical_ids
            .values()
            .all(BTreeSet::is_empty)
            && self
                .physical_coverage
                .tombstone_ids
                .values()
                .all(BTreeSet::is_empty)
        {
            return false;
        }
        let mut candidate = self.clone();
        let coverage = &mut candidate.physical_coverage;
        for (table, ids) in coverage.historical_ids.clone() {
            coverage
                .preserved_ids
                .entry(table.clone())
                .or_default()
                .extend(ids.iter().cloned());
            if let Some(missing) = coverage.missing_ids.get_mut(&table) {
                missing.retain(|id| !ids.contains(id));
            }
        }
        let files = coverage
            .historical_ids
            .get("media_files")
            .cloned()
            .unwrap_or_default();
        coverage
            .unresolved_occurrences
            .retain(|id, _| !files.contains(id));
        coverage.unassociated_files.retain(|id| !files.contains(id));
        coverage.reference_slots = coverage.historical_reference_slots.clone();
        candidate.physical_source_candidate()
    }

    /// Diagnostic only. No result here is used to grant import admission.
    fn physical_candidate_diagnostics(&self) -> Value {
        const PHYSICAL: &[&str] = &[
            "items",
            "item_origins",
            "metadata_documents",
            "item_structure",
            "libraries",
            "catalog_libraries",
            "library_sources",
            "editions",
            "timelines",
            "media_versions",
            "media_files",
            "version_files",
            "timeline_viewing",
        ];
        let disallowed_tables: Vec<_> = self
            .table_rows
            .iter()
            .filter_map(|(table, count)| {
                let admitted = *count == 0
                    || EMPTY_EXEMPT.contains(&table.as_str())
                    || RETAINED_AUTH_TABLES.contains(&table.as_str())
                    || (table == "idempotency_records"
                        && *count
                            == self.retained_pairing_receipts
                                + self.retained_download_receipts
                                + self.retained_schedule_receipts
                                + self.retained_source_creation_receipts
                        && (self.retained_download_receipts == 0
                            || self.physical_history_supported))
                    || (table == "catalog_receipts"
                        && *count == self.retained_zero_effect_receipts.len() as u64
                        && zero_effect_upgrade_receipts(&self.retained_zero_effect_receipts))
                    || (table == "api_schedules" && self.retained_schedules_supported)
                    || PHYSICAL.contains(&table.as_str())
                    || (self.physical_history_supported
                        && PHYSICAL_HISTORY_TABLES.contains(&table.as_str()))
                    || LOGICAL_APPLICATION_TABLES.contains(&table.as_str());
                (!admitted).then(|| json!({"table":table,"rows":count}))
            })
            .collect();
        let targets: Vec<_> = TARGETS
            .iter()
            .filter_map(|table| {
                let preserved = self
                    .physical_coverage
                    .preserved_ids
                    .get(*table)
                    .map(BTreeSet::len);
                let missing = self
                    .physical_coverage
                    .missing_ids
                    .get(*table)
                    .map(BTreeSet::len);
                let rows = self.table_rows.get(*table).copied();
                let agrees = preserved
                    .zip(missing)
                    .is_some_and(|(p, m)| rows == Some((p + m) as u64));
                (!agrees).then(
                    || json!({"table":table,"rows":rows,"preserved":preserved,"missing":missing}),
                )
            })
            .collect();
        let slots: Vec<_> = self.slots.iter().filter_map(|slot| {
            let key=format!("{}.{}->{}.{}",slot.table,slot.column,slot.target_table,slot.target_column);
            let observed=self.physical_coverage.reference_slots.get(&key).map_or(0,|s|s.resolved+s.nullable_absent+s.unmapped+s.malformed);
            (self.table_rows.get(&slot.table).copied()!=Some(observed)).then(||json!({"slot":key,"rows":self.table_rows.get(&slot.table),"observed":observed}))
        }).collect();
        json!({"candidate":self.physical_source_candidate(),"contract_version":self.contract_version,
            "unknown_tables":self.unknown_tables,"schema_mismatches":self.schema_mismatches,
            "missing_known_tables":KNOWN_TABLES.iter().filter(|t|!self.table_rows.contains_key(**t)&&!POST_V84_CONTROL_TABLES.contains(t)).collect::<Vec<_>>(),
            "disallowed_tables":disallowed_tables,"target_mismatches":targets,"slot_mismatches":slots,
            "logical_root_structure_supported":self.logical_root_structure_supported,
            "idempotency_rows":self.table_rows.get("idempotency_records"),"pairing_receipts":self.retained_pairing_receipts,
            "download_receipts":self.retained_download_receipts,"schedule_receipts":self.retained_schedule_receipts,
            "source_creation_receipts":self.retained_source_creation_receipts,
            "catalog_receipt_rows":self.table_rows.get("catalog_receipts"),"zero_effect_receipts":self.retained_zero_effect_receipts.len(),
            "zero_effect_valid":zero_effect_upgrade_receipts(&self.retained_zero_effect_receipts),
            "missing_ids":self.physical_coverage.missing_ids.values().map(BTreeSet::len).sum::<usize>(),
            "unresolved_occurrences":self.physical_coverage.unresolved_occurrences.len(),
            "unassociated_files":self.physical_coverage.unassociated_files.len(),
            "ownership_mismatches":self.physical_coverage.ownership_mismatches.len(),
            "unmapped_slots":self.physical_coverage.reference_slots.values().filter(|s|s.unmapped!=0||s.malformed!=0).count()})
    }

    pub fn physical_source_candidate(&self) -> bool {
        self.physical_candidate_with_origins(None)
    }
    pub(crate) fn qualified_with_origins(
        &self,
        proof: Option<&crate::origin_preservation::VerifiedOriginPreservation<'_>>,
    ) -> bool {
        self.source_qualification_supported()
            || (self.physical_coverage.source_reference_contract == Some(1)
                && self.origin_preservation.as_ref().is_some_and(|c| {
                    c.version == 1 && self.table_rows.get("nfo_origins") == Some(&c.origin_rows)
                })
                && proof.is_some_and(|p| p.admits(self))
                && self.physical_candidate_with_origins(proof))
    }
    pub(crate) fn physical_candidate_with_origins(
        &self,
        proof: Option<&crate::origin_preservation::VerifiedOriginPreservation<'_>>,
    ) -> bool {
        const PHYSICAL: &[&str] = &[
            "items",
            "item_origins",
            "metadata_documents",
            "item_structure",
            "libraries",
            "catalog_libraries",
            "library_sources",
            "editions",
            "timelines",
            "media_versions",
            "media_files",
            "version_files",
            "timeline_viewing",
        ];
        self.contract_version == 1
            && self.unknown_tables.is_empty()
            && self.schema_mismatches.is_empty()
            && KNOWN_TABLES.iter().all(|table| {
                self.table_rows.contains_key(*table) || POST_V84_CONTROL_TABLES.contains(table)
            })
            && self.table_rows.iter().all(|(table, count)| {
                *count == 0
                    || EMPTY_EXEMPT.contains(&table.as_str())
                    || RETAINED_AUTH_TABLES.contains(&table.as_str())
                    || (table == "idempotency_records"
                        && *count
                            == self.retained_pairing_receipts
                                + self.retained_download_receipts
                                + self.retained_schedule_receipts
                                + self.retained_source_creation_receipts
                        && (self.retained_download_receipts == 0
                            || self.physical_history_supported))
                    || (table == "catalog_receipts"
                        && *count == self.retained_zero_effect_receipts.len() as u64
                        && zero_effect_upgrade_receipts(&self.retained_zero_effect_receipts))
                    || (table == "api_schedules" && self.retained_schedules_supported)
                    || PHYSICAL.contains(&table.as_str())
                    || (table == "nfo_origins" && proof.is_some_and(|p| p.admits(self)))
                    || (self.physical_history_supported
                        && PHYSICAL_HISTORY_TABLES.contains(&table.as_str()))
                    || LOGICAL_APPLICATION_TABLES.contains(&table.as_str())
            })
            && self.logical_root_structure_supported
            && TARGETS.iter().all(|table| {
                let Some(preserved) = self.physical_coverage.preserved_ids.get(*table) else {
                    return false;
                };
                let Some(missing) = self.physical_coverage.missing_ids.get(*table) else {
                    return false;
                };
                self.table_rows.get(*table).copied()
                    == Some((preserved.len() + missing.len()) as u64)
            })
            && self.slots.iter().all(|slot| {
                let key = format!(
                    "{}.{}->{}.{}",
                    slot.table, slot.column, slot.target_table, slot.target_column
                );
                let observed =
                    self.physical_coverage
                        .reference_slots
                        .get(&key)
                        .map_or(0, |counts| {
                            counts.resolved
                                + counts.nullable_absent
                                + counts.unmapped
                                + counts.malformed
                        });
                self.table_rows.get(&slot.table).copied() == Some(observed)
            })
            && self
                .physical_coverage
                .missing_ids
                .values()
                .all(BTreeSet::is_empty)
            && self.physical_coverage.unresolved_occurrences.is_empty()
            && self.physical_coverage.unassociated_files.is_empty()
            && self.physical_coverage.ownership_mismatches.is_empty()
            && self
                .physical_coverage
                .reference_slots
                .values()
                .all(|slot| slot.unmapped == 0 && slot.malformed == 0)
    }

    pub fn logical_only_candidate(&self) -> bool {
        self.contract_version == 1
            && self.unknown_tables.is_empty()
            && self.schema_mismatches.is_empty()
            && KNOWN_TABLES.iter().all(|table| {
                self.table_rows.contains_key(*table) || POST_V84_CONTROL_TABLES.contains(table)
            })
            && self.table_rows.iter().all(|(table, count)| {
                *count == 0
                    || EMPTY_EXEMPT.contains(&table.as_str())
                    || RETAINED_AUTH_TABLES.contains(&table.as_str())
                    || (table == "idempotency_records"
                        && *count
                            == self.retained_pairing_receipts
                                + self.retained_catalog_artwork_receipts)
                    || (table == "catalog_receipts" && self.zero_effect_receipts_only())
                    || (self.artwork_references_supported
                        && crate::artwork_import::TABLES.contains(&table.as_str()))
                    || ["items", "item_origins", "metadata_documents"].contains(&table.as_str())
                    || (table == "item_structure" && self.logical_root_structure_supported)
                    || LOGICAL_APPLICATION_TABLES.contains(&table.as_str())
            })
            && self.slots.iter().all(|slot| {
                self.table_rows.get(&slot.table) == Some(&0)
                    || (self.artwork_references_supported
                        && crate::artwork_import::TABLES.contains(&slot.table.as_str())
                        && slot.target_table == "items")
                    || ["item_origins", "metadata_documents"].contains(&slot.table.as_str())
                    || (slot.table == "item_structure"
                        && slot.target_table == "items"
                        && self.logical_root_structure_supported)
                    || (LOGICAL_APPLICATION_TABLES.contains(&slot.table.as_str())
                        && slot.target_table == "items")
            })
    }
    pub fn empty_catalog(&self) -> bool {
        self.contract_version == 1
            && self.unknown_tables.is_empty()
            && self.schema_mismatches.is_empty()
            && self
                .slots
                .iter()
                .all(|slot| self.table_rows.get(&slot.table) == Some(&0))
            && KNOWN_TABLES.iter().all(|table| {
                self.table_rows.contains_key(*table) || POST_V84_CONTROL_TABLES.contains(table)
            })
            && self.table_rows.iter().all(|(table, count)| {
                *count == 0
                    || EMPTY_EXEMPT.contains(&table.as_str())
                    || RETAINED_AUTH_TABLES.contains(&table.as_str())
                    || (table == "idempotency_records" && *count == self.retained_pairing_receipts)
                    || (table == "catalog_receipts" && self.zero_effect_receipts_only())
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSlot {
    pub table: String,
    pub column: String,
    pub target_table: String,
    pub target_column: String,
    pub declared_foreign_key: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceInventory {
    pub import_id: String,
    pub source_namespace: String,
    pub input_digest: String,
    pub schema_recorded: bool,
    pub slots: Vec<ReferenceSlot>,
    pub scalar_reference_rows: usize,
    pub embedded_reference_rows: usize,
    pub unknown_metadata_fields: Vec<UnknownMetadataField>,
    pub unresolved_evidence: usize,
    pub retained_resolved_evidence: usize,
    pub retained_motion_auth_rows: BTreeMap<String, u64>,
    pub empty_catalog: bool,
    pub physical_coverage: PhysicalCoverage,
    pub unknown_tables: Vec<String>,
    /// Only true once *all* migration evidence has a qualified native owner.
    /// Discovery of references alone never clears this gate.
    pub activation_ready: bool,
    pub qualification: String,
    pub schema_mismatches: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnknownMetadataField {
    pub item_id: String,
    pub source: String,
    pub field: String,
}
impl NativeCatalog {
    /// Counts and predicate failures only; no raw receipt/auth/document data.
    /// This is diagnostic evidence, never an import or serving grant.
    pub async fn motion_reference_candidate_diagnostics(
        &self,
        import_id: &str,
    ) -> anyhow::Result<Value> {
        let raw: String = sqlx::query_scalar("SELECT audit_json FROM catabolic_reference_audits WHERE import_id=? AND length(CAST(audit_json AS BLOB))<=16777216")
            .bind(import_id).fetch_one(&self.journal).await?;
        let audit: SourceAudit = serde_json::from_str(&raw)?;
        Ok(audit.physical_candidate_diagnostics())
    }

    pub async fn motion_reference_inventory(
        &self,
        import_id: &str,
    ) -> anyhow::Result<ReferenceInventory> {
        // This independently verifies retained provenance; copied export JSON
        // cannot claim either typed authority or a trusted inventory.
        self.motion_import_authority(import_id).await?;
        let (snapshot, digest) = self.import_snapshot(import_id).await?;
        let retained: Option<(String, String)> = sqlx::query_as(
            "SELECT input_digest,audit_json FROM catabolic_reference_audits WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&self.journal)
        .await?;
        let audit: Option<SourceAudit> = retained
            .map(|(saved_digest, raw)| -> anyhow::Result<_> {
                ensure!(
                    saved_digest == digest,
                    "reference audit digest differs from retained source"
                );
                Ok(serde_json::from_str(&raw)?)
            })
            .transpose()?;
        let mut connection = self.journal.acquire().await?;
        let origin_proof = if let Some(audit) = &audit {
            crate::origin_preservation::verified_in(&mut connection, &snapshot, audit).await?
        } else {
            None
        };
        drop(connection);
        let empty_catalog = audit.as_ref().is_some_and(SourceAudit::empty_catalog);
        let unknown_tables = audit
            .as_ref()
            .map(|a| a.unknown_tables.clone())
            .unwrap_or_default();
        let mut slots = audit.as_ref().map(|a| a.slots.clone()).unwrap_or_default();
        let mut schema_recorded = audit.is_some();
        let mut schema_evidence_seen = false;
        let mut scalar_reference_rows = 0;
        let mut embedded_reference_rows = 0;
        let mut unknown_metadata_fields = Vec::new();
        let mut unresolved_evidence = 0;
        for batch in &snapshot.batches {
            for record in &batch.unresolved_evidence {
                unresolved_evidence += 1;
                if record.evidence_id == SCHEMA_ID {
                    ensure!(!schema_evidence_seen, "duplicate reference schema evidence");
                    schema_evidence_seen = true;
                    let recorded_slots: Vec<ReferenceSlot> =
                        serde_json::from_value(record.data["slots"].clone())?;
                    ensure!(
                        audit.is_none() || slots == recorded_slots,
                        "reference audit schema differs from snapshot"
                    );
                    slots = recorded_slots;
                    schema_recorded = true;
                }
                if record.evidence_id.starts_with("reference:")
                    || record.evidence_id.starts_with("semantic-reference:")
                {
                    scalar_reference_rows += 1;
                }
                if record.evidence_id.starts_with("embedded-reference:") {
                    embedded_reference_rows += 1;
                }
            }
            if let Some(typed) = &batch.typed {
                for item in &typed.metadata {
                    for contribution in &item.contributions {
                        if let Some(document) = contribution
                            .evidence
                            .get("document")
                            .and_then(Value::as_object)
                        {
                            for field in document.keys().filter(|f| {
                                !["values", "tags", "excluded_tags"].contains(&f.as_str())
                            }) {
                                unknown_metadata_fields.push(UnknownMetadataField {
                                    item_id: item.item_id.clone(),
                                    source: contribution.namespace.clone(),
                                    field: field.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        // Counts describe captured references, including obligations discharged
        // by a qualified exact mapping. Zero unresolved is not zero history.
        if let Some(audit) = &audit {
            for record in &audit.resolved_evidence {
                let Some(id) = record.get("evidence_id").and_then(Value::as_str) else {
                    continue;
                };
                if id.starts_with("reference:") || id.starts_with("semantic-reference:") {
                    scalar_reference_rows += 1;
                }
                if id.starts_with("embedded-reference:") {
                    embedded_reference_rows += 1;
                }
            }
        }
        let activation_ready = schema_recorded
            && audit
                .as_ref()
                .is_some_and(|a| a.qualified_with_origins(origin_proof.as_ref()))
            && unresolved_evidence == 0;
        let qualification = if audit.as_ref().is_some_and(|a| {
            a.physical_coverage.source_reference_contract == Some(2)
                && a.historical_source_candidate()
        }) {
            "historical_source"
        } else if empty_catalog {
            "empty"
        } else if audit
            .as_ref()
            .is_some_and(|a| a.logical_only_supported && a.logical_only_candidate())
        {
            "logical_only"
        } else if audit.as_ref().is_some_and(|a| {
            a.physical_coverage.source_reference_contract == Some(1)
                && a.physical_candidate_with_origins(origin_proof.as_ref())
        }) {
            "physical_source"
        } else {
            "unresolved"
        }
        .into();
        drop(origin_proof);
        Ok(ReferenceInventory {
            import_id: import_id.into(),
            source_namespace: snapshot.manifest.source_namespace,
            input_digest: digest,
            schema_recorded,
            slots,
            scalar_reference_rows,
            embedded_reference_rows,
            unknown_metadata_fields,
            unresolved_evidence,
            retained_resolved_evidence: audit.as_ref().map_or(0, |a| a.resolved_evidence.len()),
            retained_motion_auth_rows: audit
                .as_ref()
                .map(|a| {
                    a.table_rows
                        .iter()
                        .filter(|(table, _)| RETAINED_AUTH_TABLES.contains(&table.as_str()))
                        .map(|(table, count)| (table.clone(), *count))
                        .collect()
                })
                .unwrap_or_default(),
            empty_catalog,
            physical_coverage: audit
                .as_ref()
                .map(|a| a.physical_coverage.clone())
                .unwrap_or_default(),
            unknown_tables,
            activation_ready,
            qualification,
            schema_mismatches: audit
                .as_ref()
                .map(|a| a.schema_mismatches.clone())
                .unwrap_or_default(),
        })
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
fn semantic_target(table: &str, column: &str) -> Option<&'static str> {
    match column {
        "item_id" | "source_item_id" | "target_item_id" | "alias_id" => Some("items"),
        "edition_id" => Some("editions"),
        "timeline_id" => Some("timelines"),
        "version_id" => Some("media_versions"),
        "file_id" | "source_file_id" | "output_file_id" => Some("media_files"),
        "library_id" if matches!(table, "scan_requests" | "api_schedules" | "library_sources") => {
            Some("catalog_libraries")
        }
        "library_id" | "source_id"
            if matches!(
                table,
                "jobs" | "media_files" | "scan_schedules" | "library_sources" | "scan_demands"
            ) =>
        {
            Some("libraries")
        }
        _ => None,
    }
}

pub(crate) async fn capture_reference_evidence(
    db: &mut SqliteConnection,
    batches: &mut Vec<ImportBatch>,
) -> anyhow::Result<SourceAudit> {
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .fetch_all(&mut *db).await?;
    let mut slots = Vec::new();
    let mut table_rows = BTreeMap::new();
    let mut unknown_tables = Vec::new();
    let mut columns_by_table = BTreeMap::new();
    let expected_columns: BTreeMap<String, Vec<String>> =
        serde_json::from_str(include_str!("reference_schema_v74.json"))?;
    let expected_rules: BTreeMap<String, Value> =
        serde_json::from_str(include_str!("reference_schema_rules.json"))?;
    let mut schema_mismatches = Vec::new();
    for table in tables {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}", quote(&table)))
            .fetch_one(&mut *db)
            .await?;
        table_rows.insert(table.clone(), u64::try_from(count)?);
        if !KNOWN_TABLES.contains(&table.as_str()) {
            unknown_tables.push(table.clone());
        }
        let columns = sqlx::query(&format!("PRAGMA table_info({})", quote(&table)))
            .fetch_all(&mut *db)
            .await?;
        let names: BTreeSet<String> = columns.iter().map(|c| c.get("name")).collect();
        let observed: Vec<String> = columns.iter().map(|c| c.get("name")).collect();
        if expected_columns.get(&table) != Some(&observed) {
            schema_mismatches.push(table.clone());
        }
        if let Some(expected) = expected_rules.get(&table) {
            let shape: Vec<Value> = columns
                .iter()
                .map(|column| {
                    json!([
                        column.get::<String, _>("name"),
                        column.get::<String, _>("type"),
                        column.get::<i64, _>("notnull"),
                        column.get::<i64, _>("pk")
                    ])
                })
                .collect();
            let keys = sqlx::query(&format!("PRAGMA foreign_key_list({})", quote(&table)))
                .fetch_all(&mut *db)
                .await?;
            let mut keys: Vec<(String, Option<String>, String, String, String, String)> = keys
                .iter()
                .map(|key| {
                    (
                        key.get("from"),
                        key.get("to"),
                        key.get("table"),
                        key.get("on_update"),
                        key.get("on_delete"),
                        key.get("match"),
                    )
                })
                .collect();
            keys.sort();
            if expected["columns"] != json!(shape)
                || expected["foreign_keys"] != serde_json::to_value(keys)?
            {
                schema_mismatches.push(format!("{table}:column_or_reference_rules"));
            }
        }
        if table.starts_with("catabolic_") {
            continue;
        }
        columns_by_table.insert(table.clone(), names.clone());
        let mut declared = BTreeSet::new();
        for fk in sqlx::query(&format!("PRAGMA foreign_key_list({})", quote(&table)))
            .fetch_all(&mut *db)
            .await?
        {
            let target: String = fk.get("table");
            if TARGETS.contains(&target.as_str()) {
                let column: String = fk.get("from");
                declared.insert(column.clone());
                slots.push(ReferenceSlot {
                    table: table.clone(),
                    column,
                    target_table: target,
                    target_column: fk
                        .get::<Option<String>, _>("to")
                        .unwrap_or_else(|| "id".into()),
                    declared_foreign_key: true,
                });
            }
        }
        let mut semantic = BTreeSet::new();
        for column in names {
            if !declared.contains(&column) {
                if let Some(target) = semantic_target(&table, &column) {
                    slots.push(ReferenceSlot {
                        table: table.clone(),
                        column: column.clone(),
                        target_table: target.into(),
                        target_column: "id".into(),
                        declared_foreign_key: false,
                    });
                    semantic.insert(column);
                }
            }
        }
        if !semantic.is_empty() {
            for column in columns.iter().filter(|c| c.get::<i64, _>("pk") > 0) {
                semantic.insert(column.get("name"));
            }
            for (index, row) in rows(db, &table, Some(&semantic))
                .await?
                .into_iter()
                .enumerate()
            {
                evidence(
                    batches,
                    format!("semantic-reference:{table}:{index}"),
                    EvidenceKind::ApplicationReference,
                    json!({"table":table,"reference_columns":row}),
                )?;
            }
        }
    }
    slots.sort_by(|a, b| {
        (&a.table, &a.column, &a.target_table).cmp(&(&b.table, &b.column, &b.target_table))
    });
    evidence(
        batches,
        SCHEMA_ID.into(),
        EvidenceKind::ApplicationReference,
        json!({"contract_version":1,"slots":slots.clone(),"meaning":"reference inventory, not native mapping or activation authority"}),
    )?;
    for &(table, column) in DOCUMENTS {
        ensure!(
            columns_by_table
                .get(table)
                .is_some_and(|c| c.contains(column)),
            "missing embedded reference source {table}.{column}"
        );
        let info = sqlx::query(&format!("PRAGMA table_info({})", quote(table)))
            .fetch_all(&mut *db)
            .await?;
        let mut selected: BTreeSet<String> = info
            .iter()
            .filter(|c| c.get::<i64, _>("pk") > 0)
            .map(|c| c.get("name"))
            .collect();
        selected.insert(column.into());
        for (index, mut row) in rows(db, table, Some(&selected))
            .await?
            .into_iter()
            .enumerate()
        {
            let raw = row
                .as_object_mut()
                .context("invalid source row")?
                .remove(column)
                .context("missing embedded document")?;
            if raw.is_null() {
                continue;
            }
            let raw = raw.as_str().context("embedded document is not text")?;
            let document: Value = serde_json::from_str(raw)
                .with_context(|| format!("invalid JSON in {table}.{column}"))?;
            let mut references = Vec::new();
            discover(&document, "", &mut references);
            // Record every examined document, even when no known key exists.
            // An empty discovery is explicitly not proof of no references.
            evidence(
                batches,
                format!("embedded-reference:{table}:{column}:{index}"),
                EvidenceKind::ApplicationReference,
                json!({"table":table,"column":column,"row_key":row,"references":references,"classification":"known_key_inventory_only","unrecognized_shapes_require_review":true}),
            )?;
        }
    }
    Ok(SourceAudit {
        contract_version: 1,
        slots,
        table_rows,
        unknown_tables,
        schema_mismatches,
        logical_only_supported: false,
        logical_root_structure_supported: false,
        origin_preservation: None,
        retained_zero_effect_receipts: Vec::new(),
        retained_pairing_receipts: 0,
        artwork_references_supported: false,
        retained_schedules_supported: false,
        retained_schedule_receipts: 0,
        retained_source_creation_receipts: 0,
        retained_catalog_artwork_receipts: 0,
        physical_history_supported: false,
        retained_download_receipts: 0,
        physical_coverage: PhysicalCoverage::default(),
        resolved_evidence: Vec::new(),
        atomic_batch_groups: Vec::new(),
    })
}
pub(crate) fn zero_effect_upgrade_receipts(rows: &[Value]) -> bool {
    let mut kinds = BTreeSet::new();
    rows.iter().all(|row| {
        let Some(kind) = row["kind"].as_str() else {
            return false;
        };
        if !kinds.insert(kind)
            || row["id"].as_str().is_none()
            || row["created_at"].as_i64().is_none()
        {
            return false;
        }
        let Some(raw) = row["document_json"].as_str() else {
            return false;
        };
        let Ok(body) = serde_json::from_str::<Value>(raw) else {
            return false;
        };
        if serde_json::to_string(&body).ok().as_deref() != Some(raw) {
            return false;
        }
        match kind {
            "migration:legacy_progress" => body == json!({"outcomes":{}}),
            "migration:timeline_progress_repair" => {
                body == json!({"imported":0,"retracted_unattributed":0})
            }
            _ => false,
        }
    })
}

fn discover(value: &Value, path: &str, result: &mut Vec<Value>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                let target = match key.as_str() {
                    "item_id" | "source_item_id" | "target_item_id" | "alias_id" => Some("items"),
                    "edition_id" => Some("editions"),
                    "timeline_id" | "timeline" => Some("timelines"),
                    "version_id" => Some("media_versions"),
                    "file_id" | "source_file_id" | "source_file" | "output_file_id" => {
                        Some("media_files")
                    }
                    _ => None,
                };
                if let (Some(target), Some(id)) = (target, value.as_str()) {
                    result.push(json!({"path":path,"target_table":target,"source_id":id,"mapping":"unqualified"}));
                }
                discover(value, &path, result);
            }
        }
        Value::Array(values) => {
            for (i, value) in values.iter().enumerate() {
                discover(value, &format!("{path}/{i}"), result);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod reference_policy_tests {
    use super::*;
    #[test]
    fn tombstones_are_restricted_to_exact_nonforeign_history_slots() {
        let mut slot = ReferenceSlot {
            table: "downloads".into(),
            column: "file_id".into(),
            target_table: "media_files".into(),
            target_column: "id".into(),
            declared_foreign_key: false,
        };
        assert!(historical_tombstone_slot(&slot));
        slot.declared_foreign_key = true;
        assert!(!historical_tombstone_slot(&slot));
        slot.declared_foreign_key = false;
        slot.table = "version_files".into();
        assert!(!historical_tombstone_slot(&slot));
        slot.table = "content_tickets".into();
        assert!(!historical_tombstone_slot(&slot));
        slot.table = "unreviewed_history".into();
        assert!(!historical_tombstone_slot(&slot));
        slot.table = "downloads".into();
        slot.target_table = "items".into();
        assert!(!historical_tombstone_slot(&slot));
    }
    #[test]
    fn every_reviewed_foreign_key_array_matches_checker_canonical_order() {
        let rules: BTreeMap<String, Value> =
            serde_json::from_str(include_str!("reference_schema_rules.json")).unwrap();
        for (table, rule) in rules {
            // Match the checker's exact tuple types and Rust ordering, including
            // nullable referenced columns; JSON text ordering is not equivalent.
            let keys: Vec<(String, Option<String>, String, String, String, String)> =
                serde_json::from_value(rule["foreign_keys"].clone()).unwrap_or_else(|error| {
                    panic!("invalid reviewed FK tuples for {table}: {error}")
                });
            let mut canonical = keys.clone();
            canonical.sort();
            assert_eq!(
                keys, canonical,
                "reviewed foreign keys for {table} are not in checker canonical order"
            );
        }
    }
    #[test]
    fn every_reviewed_table_has_an_exact_column_inventory() {
        let columns: BTreeMap<String, Vec<String>> =
            serde_json::from_str(include_str!("reference_schema_v74.json")).unwrap();
        for table in KNOWN_TABLES {
            assert!(
                columns
                    .get(*table)
                    .is_some_and(|columns| !columns.is_empty()),
                "reviewed table {table} lacks exact schema inventory"
            );
        }
    }
    #[tokio::test]
    async fn publication_control_shapes_match_compiled_migrations_and_reject_drift() {
        use sqlx::Connection;
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        crate::projection::configure_connection(&mut db)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations")
            .run(&mut db)
            .await
            .unwrap();
        let mut batches = Vec::new();
        let audit = capture_reference_evidence(&mut db, &mut batches)
            .await
            .unwrap();
        assert!(
            audit.schema_mismatches.is_empty(),
            "{:?}",
            audit.schema_mismatches
        );
        sqlx::query("ALTER TABLE catabolic_generated_publications ADD COLUMN unreviewed TEXT")
            .execute(&mut db)
            .await
            .unwrap();
        let audit = capture_reference_evidence(&mut db, &mut Vec::new())
            .await
            .unwrap();
        assert!(
            audit
                .schema_mismatches
                .iter()
                .any(|value| value == "catabolic_generated_publications")
        );
        assert!(
            audit
                .schema_mismatches
                .iter()
                .any(|value| value == "catabolic_generated_publications:column_or_reference_rules")
        );
    }
    #[test]
    fn old_audit_may_omit_only_reviewed_additive_control_tables() {
        let mut audit:SourceAudit=serde_json::from_value(json!({"contract_version":1,"slots":[],"table_rows":KNOWN_TABLES.iter().map(|table|((*table).to_owned(),0u64)).collect::<BTreeMap<_,_>>(),"unknown_tables":[],"schema_mismatches":[],"logical_only_supported":false,"resolved_evidence":[],"atomic_batch_groups":[]})).unwrap();
        assert!(audit.empty_catalog());
        for table in POST_V84_CONTROL_TABLES {
            audit.table_rows.remove(*table);
        }
        assert!(audit.empty_catalog());
        let saved = audit.clone();
        audit.table_rows.remove("catabolic_source_guards");
        assert!(!audit.empty_catalog());
        let mut audit = saved.clone();
        audit.table_rows.remove("profiles");
        assert!(!audit.empty_catalog());
        let mut audit = saved;
        audit
            .unknown_tables
            .push("catabolic_unreviewed_future".into());
        assert!(!audit.empty_catalog());
    }
    #[test]
    fn viewing_ack_is_historical_scalar_state_not_a_delivery_grant() {
        let session = json!({"id":"session","profile_id":"profile","timeline_id":"timeline","revision":8,"delivery_id":"new-delivery"});
        let ack = json!({"session":{"id":"session","revision":"2","profile_id":"profile","timeline_id":"timeline","delivery_id":"old-delivery","sequence":"1","manual_epoch":"1","position_ms":250,"status":"paused"},"viewing":{"profile_id":"profile","timeline_id":"timeline","revision":"4","manual_epoch":"1","position_ms":250,"watched":false,"manual_watched":null,"session_id":"session"},"duplicate":false});
        let row = json!({"session_id":"session","sequence":1,"position_ms":250,"status":"paused","acknowledgement_json":ack.to_string()});
        assert!(qualified_viewing_ack(&row, &session));
        for (key, value) in [
            ("timeline_id", json!("other")),
            ("content_url", json!("secret")),
            ("revision", json!(7)),
        ] {
            let mut altered = ack.clone();
            altered["session"][key] = value;
            let mut bad = row.clone();
            bad["acknowledgement_json"] = json!(altered.to_string());
            assert!(!qualified_viewing_ack(&bad, &session));
        }
        let mut bad = row.clone();
        bad["sequence"] = json!(2);
        assert!(!qualified_viewing_ack(&bad, &session));
    }
    #[test]
    fn download_receipt_preserves_identity_but_never_an_old_byte_grant() {
        let download = json!({"id":"download","principal_id":"principal","device_id":"device","profile_id":"profile","timeline_id":"timeline","size_bytes":11,"subtitle_track_id":null,"revision":2,"state":"cancelled"});
        let body = json!({"id":"download","revision":"1","device_id":"device","profile_id":"profile","timeline_id":"timeline","state":"ready","manifest_asset_id":null,"size_bytes":"11","job_id":null,"subtitle_track_id":null,"device_transfer":null});
        assert!(qualified_download_receipt(
            &body,
            "principal",
            "device",
            &download
        ));
        assert!(!qualified_download_receipt(
            &body, "other", "device", &download
        ));
        assert!(!qualified_download_receipt(
            &body,
            "principal",
            "other",
            &download
        ));
        let mut extra = body.clone();
        extra["content_url"] = json!("old-secret-url");
        assert!(!qualified_download_receipt(
            &extra,
            "principal",
            "device",
            &download
        ));
        let mut other = body;
        other["timeline_id"] = json!("another");
        assert!(!qualified_download_receipt(
            &other,
            "principal",
            "device",
            &download
        ));
    }
    #[test]
    fn offline_history_requires_exact_document_and_retained_device_owner() {
        let download = json!({"id":"download","device_id":"device","profile_id":"profile","timeline_id":"timeline"});
        let document = json!({"event":{"id":"event","sequence":1,"download_id":"download","timeline_id":"timeline","timeline_revision":3,"base_viewing_revision":1,"base_manual_epoch":1,"position_ms":250,"status":"paused","device_time":"old-clock"}});
        let row = json!({"device_id":"device","profile_id":"profile","event_id":"event","sequence":1,"timeline_id":"timeline","event_json":document.to_string(),"outcome":"history_only","code":"timeline_changed"});
        assert!(qualified_offline_event(
            &row,
            std::slice::from_ref(&download)
        ));
        for (key, value) in [
            ("device_id", json!("other")),
            ("timeline_id", json!("other")),
            ("sequence", json!(2)),
        ] {
            let mut changed = row.clone();
            changed[key] = value;
            assert!(!qualified_offline_event(
                &changed,
                std::slice::from_ref(&download)
            ));
        }
        let mut unknown = document.clone();
        unknown["event"]["file_id"] = json!("unreviewed");
        let mut changed = row.clone();
        changed["event_json"] = json!(unknown.to_string());
        assert!(!qualified_offline_event(
            &changed,
            std::slice::from_ref(&download)
        ));
        assert!(!qualified_offline_event(&row, &[]));
    }
    #[test]
    fn nullable_scalar_policy_never_turns_malformed_or_historical_ids_into_authority() {
        let ids = BTreeSet::from(["timeline".to_owned()]);
        assert_eq!(
            resolve_reference(Some(&json!("timeline")), false, Some(&ids)),
            ReferenceResolution::Resolved
        );
        assert_eq!(
            resolve_reference(Some(&Value::Null), true, Some(&ids)),
            ReferenceResolution::NullableAbsent
        );
        assert_eq!(
            resolve_reference(Some(&Value::Null), false, Some(&ids)),
            ReferenceResolution::Malformed
        );
        assert_eq!(
            resolve_reference(None, true, Some(&ids)),
            ReferenceResolution::Malformed
        );
        assert_eq!(
            resolve_reference(Some(&json!(17)), true, Some(&ids)),
            ReferenceResolution::Malformed
        );
        assert_eq!(
            resolve_reference(Some(&json!({"timeline_id":"timeline"})), true, Some(&ids)),
            ReferenceResolution::Malformed
        );
        assert_eq!(
            resolve_reference(Some(&json!("retired-history")), false, Some(&ids)),
            ReferenceResolution::Unmapped
        );
        assert_eq!(
            resolve_reference(Some(&json!("timeline")), false, None),
            ReferenceResolution::Unmapped
        );
    }
}
