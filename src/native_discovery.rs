//! Native observation -> Motion-owned byte probe. This adapter never invokes
//! the legacy scan catalog writer or infers work identity from matching paths.
use crate::{App, scan};
use anyhow::{Context, ensure};
use motion_catalog_backend::sources::SourceMapping;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

// Current-effect admission is deliberately separate from immutable receipt history.
async fn current_source_in(
    db: &mut sqlx::SqliteConnection,
    id: &str,
) -> anyhow::Result<SourceMapping> {
    let (lifecycle, mapping) =
        motion_catalog_backend::source_lifecycle::current_mapping_in(db, id).await?;
    ensure!(
        lifecycle.state == playscale_core::source_lifecycle::State::Active,
        "native source is not active"
    );
    Ok(mapping)
}
async fn require_current_source_in(
    db: &mut sqlx::SqliteConnection,
    saved: &SourceMapping,
) -> anyhow::Result<()> {
    ensure!(
        current_source_in(db, &saved.library_id).await? == *saved,
        "native source mapping changed before effect"
    );
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryIds {
    pub work: String,
    pub edition: String,
    pub timeline: String,
    pub version: String,
    pub version_item: String,
    pub motion_file: String,
    pub binding: String,
    pub association: String,
}
impl DiscoveryIds {
    fn new() -> Self {
        let version = crate::new_id();
        Self {
            work: crate::new_id(),
            edition: format!("motion.edition:{}", crate::new_id()),
            timeline: crate::new_id(),
            version_item: format!("motion.version:{version}"),
            version,
            motion_file: crate::new_id(),
            binding: crate::new_id(),
            association: crate::new_id(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryIntent {
    pub operation_id: String,
    pub source: SourceMapping,
    pub observed: Value,
    pub ids: DiscoveryIds,
    pub probe: Option<Value>,
}
fn text<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("missing native occurrence {key}"))
}
pub(crate) fn current_matches(probe: &Value, observed: &Value) -> bool {
    use playscale_core::catalog_registration::{ObservedOccurrenceIdentity, admits_occurrence};
    let Some(identity) = (|| {
        Some(ObservedOccurrenceIdentity {
            size: observed["size"].as_i64()?,
            mtime_ns: observed["mtime_ns"].as_i64()?,
            device: observed["device"].as_i64()?,
            inode: observed["inode"].as_i64()?,
            ctime_ns: observed["ctime_ns"].as_i64()?,
        })
    })() else {
        return false;
    };
    probe["fingerprint"]
        .as_str()
        .zip(probe["bytes"].as_i64())
        .is_some_and(|(fingerprint, bytes)| {
            admits_occurrence(
                fingerprint,
                bytes,
                true,
                observed["status"] == "present",
                identity,
            )
        })
}
/// Persist exact IDs before any native creation. Repeated inventory of unchanged
/// content reuses the same occurrence revision, intent and IDs.
pub async fn prepare(
    journal: &sqlx::SqlitePool,
    source: SourceMapping,
    observed: Value,
) -> anyhow::Result<DiscoveryIntent> {
    let mut tx = journal.begin_with("BEGIN IMMEDIATE").await?;
    require_current_source_in(&mut tx, &source).await?;
    let intent = prepare_journal_in(&mut tx, source, observed).await?;
    tx.commit().await?;
    Ok(intent)
}
/// Journal identity persistence only, not source admission or a native grant.
/// Production callers must retain the writer and validate exact current mapping/lifecycle
/// before entering. The private helper permits isolated journal contract tests
/// without fabricating registered source or lifecycle authority.
async fn prepare_journal_in(
    tx: &mut sqlx::SqliteConnection,
    source: SourceMapping,
    observed: Value,
) -> anyhow::Result<DiscoveryIntent> {
    ensure!(
        observed["location"].as_str() == Some(&source.native_source)
            && observed["status"] == "present",
        "discovery outside registered source or absent"
    );
    let file = text(&observed, "id")?;
    let revision = text(&observed, "exact_revision")?;
    let saved:Option<(String,String,String,String,Option<String>)>=sqlx::query_as("SELECT operation_id,identity_json,ids_json,observed_json,probe_json FROM catabolic_discovery_intents WHERE native_profile=? AND native_file_id=? AND native_revision=?").bind(&source.profile).bind(file).bind(revision).fetch_optional(&mut *tx).await?;
    if let Some((operation_id, identity, ids, old, probe)) = saved {
        let old_source: SourceMapping = serde_json::from_str(&identity)?;
        ensure!(old_source == source, "discovery source identity changed");
        let old: Value = serde_json::from_str(&old)?;
        ensure!(
            old == observed,
            "same native revision changed its physical snapshot"
        );
        return Ok(DiscoveryIntent {
            operation_id,
            source,
            observed,
            ids: serde_json::from_str(&ids)?,
            probe: probe.map(|p| serde_json::from_str(&p)).transpose()?,
        });
    }
    let intent = DiscoveryIntent {
        operation_id: format!("motion.discovery:{}", crate::new_id()),
        source,
        observed: observed.clone(),
        ids: DiscoveryIds::new(),
        probe: None,
    };
    sqlx::query("INSERT INTO catabolic_discovery_intents(operation_id,source_id,native_profile,native_file_id,native_revision,identity_json,ids_json,observed_json,state,created_at) VALUES(?,?,?,?,?,?,?,?,'planned',?)")
        .bind(&intent.operation_id).bind(&intent.source.library_id).bind(&intent.source.profile).bind(file).bind(revision).bind(serde_json::to_string(&intent.source)?).bind(serde_json::to_string(&intent.ids)?).bind(observed.to_string()).bind(crate::now()).execute(&mut *tx).await?;
    Ok(intent)
}
/// Probe via an opened file using Motion's existing content SHA-256 and ffprobe
/// adapter. The native exact observation independently fences every identity
/// field before accepting these bytes as a new provisional catalog entity.
pub async fn probe(
    app: &App,
    intent: DiscoveryIntent,
    stop: CancellationToken,
) -> anyhow::Result<DiscoveryIntent> {
    let permit = Arc::new(app.storage.scan_io.clone().acquire_owned().await?);
    let mut fence = app.db.begin_with("BEGIN IMMEDIATE").await?;
    require_current_source_in(&mut fence, &intent.source).await?;

    let app = app.clone();
    // Detached coordinator retains the writer and I/O permit until actual probe
    // cleanup, even when the requesting future is dropped.
    tokio::spawn(async move { probe_owned(&app, intent, stop, permit, fence).await }).await?
}
async fn probe_owned(
    app: &App,
    mut intent: DiscoveryIntent,
    stop: CancellationToken,
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    mut fence: sqlx::Transaction<'static, sqlx::Sqlite>,
) -> anyhow::Result<DiscoveryIntent> {
    ensure!(!stop.is_cancelled(), "discovery cancelled");
    let root = PathBuf::from(&intent.source.motion_root);
    ensure!(
        crate::db::root_identity(&std::fs::metadata(&root)?) == intent.source.motion_root_identity,
        "discovery root was replaced"
    );
    if let Some(saved) = &intent.probe {
        ensure!(
            current_matches(saved, &intent.observed),
            "retained probe differs from native snapshot"
        );
        return Ok(intent);
    }
    let relative = PathBuf::from(text(&intent.observed, "path")?);
    let found =
        scan::inspect_native(root.clone(), relative, &app.ffprobe, stop.clone(), permit).await?;
    ensure!(!stop.is_cancelled(), "discovery cancelled after probe");
    ensure!(
        crate::db::root_identity(&std::fs::metadata(&root)?) == intent.source.motion_root_identity,
        "discovery root changed during probe"
    );
    let media_type = if found.tracks.iter().any(|track| track.kind == "video") {
        "video"
    } else if found.tracks.iter().any(|track| track.kind == "audio") {
        "audio"
    } else {
        "unknown"
    };
    let value = json!({"title":found.title,"relative_path":found.relative,"revision":found.revision,"fingerprint":found.fingerprint,"bytes":found.bytes,"duration_seconds":found.duration,"tracks":found.tracks,"chapters":found.chapters,"media_type":media_type});
    ensure!(
        current_matches(&value, &intent.observed),
        "probe identity differs from native observed occurrence; rescan required"
    );
    let changed=sqlx::query("UPDATE catabolic_discovery_intents SET probe_json=?,state='probed' WHERE operation_id=? AND state='planned' AND probe_json IS NULL").bind(value.to_string()).bind(&intent.operation_id).execute(&mut *fence).await?.rows_affected();
    if changed == 0 {
        let saved: String = sqlx::query_scalar(
            "SELECT probe_json FROM catabolic_discovery_intents WHERE operation_id=?",
        )
        .bind(&intent.operation_id)
        .fetch_one(&mut *fence)
        .await?;
        ensure!(
            serde_json::from_str::<Value>(&saved)? == value,
            "concurrent discovery probe disagrees"
        );
    }
    intent.probe = Some(value);
    fence.commit().await?;
    Ok(intent)
}
/// Recheck through the actual retained host immediately before native command
/// dispatch. No stale probe or source policy may grant publication authority.
pub fn verify_with_host(
    host: &catabolic_runtime::host::HostSession,
    intent: &DiscoveryIntent,
) -> anyhow::Result<Value> {
    let root = PathBuf::from(&intent.source.motion_root);
    ensure!(
        crate::db::root_identity(&std::fs::metadata(&root)?) == intent.source.motion_root_identity,
        "discovery root changed before publication"
    );
    let current = scan::open_file(&root, std::path::Path::new(text(&intent.observed, "path")?))?;
    let probe = intent.probe.as_ref().context("discovery has no probe")?;
    ensure!(
        probe["fingerprint"].as_str() == Some(&scan::fingerprint(&current.metadata()?)),
        "discovery bytes changed before publication"
    );
    let snapshot = host.source_occurrences(&intent.source.native_source)?;
    ensure!(
        snapshot["catalog_id"].as_str() == Some(&intent.source.catalog.catalog_id)
            && snapshot["catalog_incarnation"].as_str() == Some(&intent.source.catalog.incarnation)
            && snapshot["profile"].as_str() == Some(&intent.source.profile),
        "discovery catalog identity changed"
    );
    ensure!(
        snapshot["binding"] == intent.source.native_binding
            && snapshot["policy"] == intent.source.native_policy,
        "discovery source binding or policy changed"
    );
    let observed = snapshot["occurrences"]
        .as_array()
        .context("native occurrences missing")?
        .iter()
        .find(|v| v["id"] == intent.observed["id"])
        .context("discovered occurrence no longer exists")?;
    ensure!(
        observed == &intent.observed,
        "discovered occurrence changed before publication"
    );
    ensure!(
        current_matches(
            intent
                .probe
                .as_ref()
                .context("discovery has no verified probe")?,
            observed
        ),
        "probe does not match exact occurrence"
    );
    Ok(snapshot)
}

/// Inventory includes all files, but automatic media catalog creation is limited
/// to the established Motion media extension set. Other occurrences stay native.
pub fn is_media_candidate(observed: &Value) -> bool {
    observed["status"] == "present"
        && observed["path"]
            .as_str()
            .and_then(|p| std::path::Path::new(p).extension())
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                [
                    "mp4", "m4v", "mkv", "webm", "mov", "avi", "ts", "m2ts", "mp3", "m4a", "flac",
                    "ogg", "wav", "opus",
                ]
                .contains(&e.to_ascii_lowercase().as_str())
            })
}

fn measured_duration_us(probe: &Value) -> Option<i64> {
    let value = probe["duration_seconds"].as_f64()? * 1_000_000.0;
    (value.is_finite() && value > 0.0 && value < i64::MAX as f64 && value.fract() == 0.0)
        .then_some(value as i64)
}
pub fn discovery_payload(intent: &DiscoveryIntent, namespace: &str) -> anyhow::Result<Value> {
    use catabolic_core::catalog_identity::{
        ExactFileRevision, IntervalUs, SourceExtent, VersionBinding, VersionContract, VersionState,
    };
    use catabolic_store::{
        catalog_identity_mutation::{
            ContextChange, IdentityMutation, TimelineChange, TimelineState, VersionChange,
        },
        catalog_import_typed::{
            ImportedAssociation, ImportedRelationship, NativeIdentityBatch, PhysicalMapping,
        },
    };
    let probe = intent.probe.as_ref().context("discovery probe missing")?;
    let ids = &intent.ids;
    let duration = measured_duration_us(probe);
    let interval = duration.map(|n| IntervalUs::new(0, n)).transpose()?;
    let exact = ExactFileRevision {
        file_id: text(&intent.observed, "id")?.into(),
        revision: text(&intent.observed, "exact_revision")?.into(),
    };
    let association = ImportedAssociation {
        source_association_id: ids.association.clone(),
        source_occurrence_id: ids.motion_file.clone(),
        item_id: ids.version_item.clone(),
        part: 1,
    };
    let mutation = IdentityMutation {
        contract_version: 1,
        profile: intent.source.profile.clone(),
        context: ContextChange {
            id: format!("motion.discovery:{}", ids.work),
            authority_namespace: namespace.into(),
            context_key: ids.work.clone(),
            expected_revision: None,
        },
        timelines: vec![TimelineChange {
            timeline_id: ids.timeline.clone(),
            edition_item_id: ids.edition.clone(),
            expected_revision: None,
            identity_state: TimelineState::Provisional,
            coverage: interval,
        }],
        versions: vec![VersionChange {
            expected_revision: None,
            expected_timeline_revision: 1,
            version: VersionContract {
                contract_version: 1,
                version_id: ids.version.clone(),
                association_item_id: ids.version_item.clone(),
                edition_item_id: ids.edition.clone(),
                timeline_id: ids.timeline.clone(),
                state: if duration.is_some() {
                    VersionState::Ready
                } else {
                    VersionState::Draft
                },
                declared_coverage: interval,
                bindings: vec![VersionBinding {
                    binding_id: ids.binding.clone(),
                    association_id: ids.association.clone(),
                    file: exact.clone(),
                    part: 1,
                    source: SourceExtent::WholeFile {
                        duration_us: duration,
                    },
                    timeline_interval: interval,
                }],
            },
            equivalence: None,
            expected_binding_revisions: [(ids.binding.clone(), None)].into(),
        }],
        evidence: vec![],
    };
    let native = NativeIdentityBatch {
        occurrences: vec![PhysicalMapping {
            source_occurrence_id: ids.motion_file.clone(),
            profile: intent.source.profile.clone(),
            location: intent.source.native_source.clone(),
            path: text(&intent.observed, "path")?.into(),
            exact,
            observed_status: Some("present".into()),
        }],
        associations: vec![association],
        mutations: vec![serde_json::to_value(mutation)?],
    };
    let relationships = vec![
        ImportedRelationship {
            source_relationship_id: format!("motion.discovery:{}:edition", ids.work),
            source_revision: 1,
            source_item_id: ids.work.clone(),
            target_item_id: ids.edition.clone(),
            kind: "custom:motion_edition".into(),
            position: None,
            metadata: json!({}),
        },
        ImportedRelationship {
            source_relationship_id: format!("motion.discovery:{}:version", ids.work),
            source_revision: 1,
            source_item_id: ids.edition.clone(),
            target_item_id: ids.version_item.clone(),
            kind: "custom:motion_version".into(),
            position: None,
            metadata: json!({}),
        },
    ];
    let kind = if probe["media_type"] == "audio" {
        "audio"
    } else {
        "video"
    };
    let native_kind = playscale_core::catalog_kind::to_native(kind)
        .context("unsupported Motion discovery media family")?;
    Ok(json!({"operation":"fresh_media_discovery","items":[
        {"id":ids.work,"kind":native_kind,"metadata":{"title":probe["title"],"motion_catalog":{"contract_version":1,"item_id":ids.work,"kind":kind,"catalog_revision":1,"match_state":"unmatched","external_ids":[],"media_type":"unclassified","parent_id":null,"number":null,"structure_revision":1},"motion_discovery":{"source_id":intent.source.library_id,"operation_id":intent.operation_id}}},
        {"id":ids.edition,"kind":"custom:motion_edition","metadata":{"motion_edition":{"contract_version":1,"source_edition":{"id":ids.edition.strip_prefix("motion.edition:").unwrap_or(&ids.edition),"item_id":ids.work,"revision":1,"label":"Original"},"source_timelines":[{"id":ids.timeline,"edition_id":ids.edition.strip_prefix("motion.edition:").unwrap_or(&ids.edition),"revision":1,"duration_ms":duration.filter(|d|d%1000==0).map(|d|d/1000),"order_group_id":null,"order_position":null}]}}},
        {"id":ids.version_item,"kind":"custom:motion_version","metadata":{"motion_version":{"contract_version":1,"source_version":{"id":ids.version,"timeline_id":ids.timeline,"revision":1,"label":"Original","origin":"original","equivalence":"unknown"}},"motion_probe":probe}}
    ],"native":native,"relationships":relationships}))
}

fn discovery_grant(
    intent: &DiscoveryIntent,
    namespace: &str,
) -> anyhow::Result<catabolic_store::catalog_discovery::FreshMediaDiscoveryAuthority> {
    use catabolic_store::{
        catalog_discovery::{FreshMediaDiscoveryAuthority, MeasuredOriginalGrant},
        catalog_identity_mutation::IdentityAuthority,
    };
    let file = catabolic_core::catalog_identity::ExactFileRevision {
        file_id: text(&intent.observed, "id")?.into(),
        revision: text(&intent.observed, "exact_revision")?.into(),
    };
    let duration = intent.probe.as_ref().and_then(measured_duration_us);
    let observed_ctime_ns = intent.observed["ctime_ns"]
        .as_i64()
        .context("missing native source ctime")?;
    Ok(FreshMediaDiscoveryAuthority {
        host_namespace: namespace.into(),
        new_item_ids: [
            intent.ids.work.clone(),
            intent.ids.edition.clone(),
            intent.ids.version_item.clone(),
        ]
        .into(),
        physical_files: [(intent.source.profile.clone(), [file.file_id.clone()].into())].into(),
        identity: IdentityAuthority {
            authority_namespace: namespace.into(),
            context_keys: [intent.ids.work.clone()].into(),
            allow_declared_evidence: false,
            allow_verified_evidence: false,
        },
        relationship_edges: [
            (
                intent.ids.work.clone(),
                intent.ids.edition.clone(),
                "custom:motion_edition".into(),
            ),
            (
                intent.ids.edition.clone(),
                intent.ids.version_item.clone(),
                "custom:motion_version".into(),
            ),
        ]
        .into(),
        measured_originals: duration
            .map(|duration_us| {
                (
                    intent.ids.version.clone(),
                    MeasuredOriginalGrant {
                        file,
                        duration_us,
                        observed_ctime_ns,
                    },
                )
            })
            .into_iter()
            .collect(),
    })
}
fn decode_command(raw: &str) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
    use catabolic_core::catalog_command::{COMMAND_CANONICALIZATION_VERSION, CommandEnvelope};
    let v: Value = serde_json::from_str(raw)?;
    ensure!(
        v["canonicalization_version"].as_u64() == Some(u64::from(COMMAND_CANONICALIZATION_VERSION)),
        "unknown discovery command encoding"
    );
    let command = CommandEnvelope::new(
        1,
        serde_json::from_value(v["key"].clone())?,
        text(&v, "admission_incarnation")?,
        v["expected_generation"]
            .as_i64()
            .context("missing discovery generation")?,
        &v["payload"].to_string(),
    )?;
    ensure!(
        command.canonical_payload() == raw,
        "noncanonical discovery command"
    );
    Ok(command)
}
/// Publish through the retained native host and durable native receipt owner.
/// Lost acknowledgment retries the exact command/IDs. If the physical proof is
/// no longer current, retry is receipt-only and cannot create new catalog rows.
pub async fn publish(app: &App, intent: DiscoveryIntent) -> anyhow::Result<Value> {
    publish_captured(app, intent, None).await
}
async fn publish_captured(
    app: &App,
    intent: DiscoveryIntent,
    accepted: Option<&playscale_core::native_scan::ExecutionIdentity>,
) -> anyhow::Result<Value> {
    dispatch_captured(app, intent, accepted)
        .await?
        .acknowledge()
        .await
}

/// Opaque ownership of a real native commit awaiting Motion acknowledgment.
/// Dropping this value rolls back only the Motion transaction. Startup recovers
/// the original native receipt; it never repeats a potentially committed effect.
pub struct PendingDiscoveryAck {
    custody: sqlx::Transaction<'static, sqlx::Sqlite>,
    runtime: Arc<crate::native_catalog::Runtime>,
    intent: DiscoveryIntent,
    outcome: Value,
}
impl PendingDiscoveryAck {
    pub async fn acknowledge(mut self) -> anyhow::Result<Value> {
        acknowledge_discovery_in(&mut self.custody, &self.intent, &self.outcome).await?;
        self.custody.commit().await?;
        self.runtime.rebuild_projection().await?;
        Ok(self.outcome)
    }
}
/// Same operator-only authority as `publish`; not an HTTP decoder. The immutable
/// intent and exact physical proof are rechecked before native dispatch. Keeping
/// the phase explicit also permits deterministic real lost-ack qualification.
pub async fn dispatch_for_acknowledgment(
    app: &App,
    intent: DiscoveryIntent,
) -> anyhow::Result<PendingDiscoveryAck> {
    dispatch_captured(app, intent, None).await
}
async fn dispatch_captured(
    app: &App,
    intent: DiscoveryIntent,
    accepted: Option<&playscale_core::native_scan::ExecutionIdentity>,
) -> anyhow::Result<PendingDiscoveryAck> {
    use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native discovery needs admitted runtime")?
        .clone();
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT command_json FROM catabolic_discovery_intents WHERE operation_id=?",
    )
    .bind(&intent.operation_id)
    .fetch_one(&app.db)
    .await?;
    let command = if let Some(raw) = existing {
        decode_command(&raw)?
    } else {
        let mut planning = app.db.begin_with("BEGIN IMMEDIATE").await?;

        require_current_source_in(&mut planning, &intent.source).await?;
        if let Some(accepted) = accepted {
            crate::native_scans::check_source_identity(accepted, &intent.source)?;
            crate::native_scans::check_execution_in(&mut planning, accepted).await?;
        }
        let planned = intent.clone();
        let (requested, mut planning) = runtime
            .with_host(move |host| {
                let snapshot = verify_with_host(host, &planned)?;
                let namespace = host.authorization()?.namespace().to_owned();
                let requested = CommandEnvelope::new(
                    1,
                    CommandKey::new(
                        &planned.source.catalog.catalog_id,
                        &namespace,
                        &planned.operation_id,
                    )?,
                    &planned.source.catalog.incarnation,
                    snapshot["generation"]
                        .as_i64()
                        .context("native generation missing")?,
                    &discovery_payload(&planned, &namespace)?.to_string(),
                )?;
                Ok((requested, planning))
            })
            .await?;
        sqlx::query("UPDATE catabolic_discovery_intents SET command_json=? WHERE operation_id=? AND state='probed' AND command_json IS NULL").bind(requested.canonical_payload()).bind(&intent.operation_id).execute(&mut *planning).await?;
        let raw: String = sqlx::query_scalar(
            "SELECT command_json FROM catabolic_discovery_intents WHERE operation_id=?",
        )
        .bind(&intent.operation_id)
        .fetch_one(&mut *planning)
        .await?;
        let saved = decode_command(&raw)?;
        ensure!(
            saved.key() == requested.key()
                && saved.payload() == requested.payload()
                && saved.admission_incarnation() == requested.admission_incarnation(),
            "discovery intent changed before dispatch"
        );
        planning.commit().await?;
        saved
    };
    let original = command;
    loop {
        let mut custody = app.db.begin_with("BEGIN IMMEDIATE").await?;
        let (_, command) =
            motion_catalog_backend::command_attempts::latest_in(&mut custody, &original).await?;
        let dispatched = command.clone();
        if let Some(accepted) = accepted {
            crate::native_scans::check_source_identity(accepted, &intent.source)?;
            crate::native_scans::check_execution_in(&mut custody, accepted).await?;
        }
        require_current_source_in(&mut custody, &intent.source).await?;

        let planned = intent.clone();
        let (outcome, witness, mut custody) = runtime
            .with_host(move |host| {
                use catabolic_core::catalog_command::ReceiptVisibility;
                use catabolic_store::catalog_commands::{
                    CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority,
                };
                let namespace = host.authorization()?.namespace().to_owned();
                ensure!(
                    command.payload() == &discovery_payload(&planned, &namespace)?,
                    "retained discovery payload differs from verified intent"
                );
                let live = verify_with_host(host, &planned).is_ok();
                let grant = discovery_grant(&planned, &namespace)?;
                let authority = CatalogAuthority::new(TrustedCommandAuthority {
                    host_namespace: &namespace,
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: live,
                    identity_authority: None,
                })
                .with_discovery(&grant);
                let (outcome, witness) = host.execute_catalog_command_observed(
                    &command,
                    authority,
                    ReceiptPolicy {
                        retention: std::time::Duration::from_secs(30 * 24 * 60 * 60),
                        max_receipts: 100_000,
                        max_logical_bytes: 256 * 1024 * 1024,
                        max_hold_records: 100_000,
                    },
                )?;
                Ok((serde_json::to_value(outcome)?, witness, custody))
            })
            .await?;
        if let Some(witness) = witness {
            motion_catalog_backend::command_attempts::record_next_in(
                &mut custody,
                &original,
                &dispatched,
                witness,
            )
            .await?;
            // A synchronous, native-minted no-effect decision is the only path here.
            // Persist the distinct next attempt before reacquiring source/demand
            // custody and rechecking the actual physical observation for dispatch.
            custody.commit().await?;
            continue;
        }
        ensure!(
            outcome["status"] == "committed" || outcome["status"] == "replay",
            "native discovery requires reconciliation: {outcome}"
        );
        return Ok(PendingDiscoveryAck {
            custody,
            runtime,
            intent,
            outcome,
        });
    }
}

async fn acknowledge_discovery_in(
    db: &mut sqlx::SqliteConnection,
    intent: &DiscoveryIntent,
    outcome: &Value,
) -> anyhow::Result<()> {
    ensure!(
        outcome["status"] == "committed" || outcome["status"] == "replay",
        "native discovery requires reconciliation: {outcome}"
    );
    let generation = outcome["committed_generation"]
        .as_i64()
        .context("discovery receipt generation missing")?;
    ensure!(generation > 0, "discovery receipt generation is invalid");
    let mapping = committed_mapping(&intent, generation)?;
    let import_id = format!("discovery:{}", intent.operation_id);
    sqlx::query("INSERT INTO catabolic_occurrence_mappings(import_id,motion_file_id,native_profile,native_file_id,mapping_json) VALUES(?,?,?,?,?) ON CONFLICT(import_id,motion_file_id) DO NOTHING").bind(&import_id).bind(&intent.ids.motion_file).bind(&intent.source.profile).bind(text(&intent.observed,"id")?).bind(serde_json::to_string(&mapping)?).execute(&mut *db).await?;
    let saved:String=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? AND motion_file_id=?").bind(&import_id).bind(&intent.ids.motion_file).fetch_one(&mut *db).await?;
    ensure!(
        serde_json::from_str::<motion_catalog_backend::occurrence_bridge::OccurrenceMapping>(
            &saved
        )? == mapping,
        "discovery mapping acknowledgment disagrees"
    );
    motion_catalog_backend::revision_bridge::acknowledge_discovery_in(
        db,
        &intent.ids.work,
        &intent.source.catalog.catalog_id,
        &intent.source.catalog.incarnation,
        &intent.operation_id,
        &outcome["result"],
    )
    .await?;
    sqlx::query("UPDATE catabolic_discovery_intents SET state='committed',receipt_json=COALESCE(receipt_json,?) WHERE operation_id=?").bind(outcome.to_string()).bind(&intent.operation_id).execute(&mut *db).await?;
    Ok(())
}

/// Startup reads only the original durable native receipt. It cannot dispatch a
/// saved intent, retry a stale generation, or infer success from current heads.
pub(crate) async fn reconcile_receipts(
    runtime: &crate::native_catalog::Runtime,
    journal: &sqlx::SqlitePool,
) -> anyhow::Result<()> {
    let mut after = String::new();
    loop {
        let ids: Vec<String> = sqlx::query_scalar("SELECT operation_id FROM catabolic_discovery_intents WHERE state!='committed' AND command_json IS NOT NULL AND operation_id>? ORDER BY operation_id LIMIT 128")
            .bind(&after).fetch_all(journal).await?;
        if ids.is_empty() {
            return Ok(());
        }
        for operation in ids {
            after = operation.clone();
            let mut custody = journal.begin_with("BEGIN IMMEDIATE").await?;
            let row: (String,String,String,String,String,String,String,String,Option<String>,String) = sqlx::query_as("SELECT source_id,native_profile,native_file_id,native_revision,identity_json,ids_json,observed_json,probe_json,receipt_json,command_json FROM catabolic_discovery_intents WHERE operation_id=? AND state!='committed'")
                .bind(&operation).fetch_one(&mut *custody).await?;
            let intent = DiscoveryIntent {
                operation_id: operation,
                source: serde_json::from_str(&row.4)?,
                ids: serde_json::from_str(&row.5)?,
                observed: serde_json::from_str(&row.6)?,
                probe: Some(serde_json::from_str(&row.7)?),
            };
            ensure!(
                row.0 == intent.source.library_id
                    && row.1 == intent.source.profile
                    && row.2 == text(&intent.observed, "id")?
                    && row.3 == text(&intent.observed, "exact_revision")?
                    && row.8.is_none(),
                "pending discovery journal identity or acknowledgment changed"
            );
            let command = decode_command(&row.9)?;
            ensure!(
                command.key().operation_id() == intent.operation_id
                    && command.key().catalog_id() == intent.source.catalog.catalog_id
                    && command.admission_incarnation() == intent.source.catalog.incarnation,
                "discovery receipt intent belongs to another identity"
            );
            motion_catalog_backend::source_lifecycle::require_historical_mapping_in(
                &mut custody,
                &intent.source,
            )
            .await?;
            let (_, command) =
                motion_catalog_backend::command_attempts::latest_in(&mut custody, &command).await?;
            let checked = intent.clone();
            let (receipt, mut custody) = runtime
                .with_host(move |host| {
                    let namespace = host.authorization()?.namespace().to_owned();
                    ensure!(
                        command.payload() == &discovery_payload(&checked, &namespace)?,
                        "discovery receipt payload differs from original proof"
                    );
                    // This API performs receipt lookup only. A missing result never
                    // permits mutation, even if the physical occurrence is current.
                    let receipt = host.reconcile_command_receipt(&command)?;
                    Ok((receipt, custody))
                })
                .await?;
            match receipt {
                None => {
                    custody.rollback().await?;
                }
                Some(outcome) => {
                    let outcome = serde_json::to_value(outcome)?;
                    acknowledge_discovery_in(&mut custody, &intent, &outcome).await?;
                    custody.commit().await?;
                }
            }
        }
    }
}

pub fn committed_mapping(
    intent: &DiscoveryIntent,
    generation: i64,
) -> anyhow::Result<motion_catalog_backend::occurrence_bridge::OccurrenceMapping> {
    use catabolic_store::catalog_import_typed::{ImportedAssociation, PhysicalMapping};
    let probe = intent.probe.as_ref().context("discovery probe missing")?;
    let ids = &intent.ids;
    let file = json!({"id":ids.motion_file,"edition_id":ids.edition.strip_prefix("motion.edition:").unwrap_or(&ids.edition),"library_id":intent.source.library_id,"relative_path":probe["relative_path"],"revision":probe["revision"],"fingerprint":probe["fingerprint"],"bytes":probe["bytes"],"duration_seconds":probe["duration_seconds"],"tracks_json":probe["tracks"].to_string(),"available":1,"generated":0,"chapters_json":if probe["chapters"].is_null(){Value::Null}else{Value::String(probe["chapters"].to_string())}});
    Ok(
        motion_catalog_backend::occurrence_bridge::OccurrenceMapping {
            source: intent.source.clone(),
            native_generation: generation,
            physical: PhysicalMapping {
                source_occurrence_id: ids.motion_file.clone(),
                profile: intent.source.profile.clone(),
                location: intent.source.native_source.clone(),
                path: text(&intent.observed, "path")?.into(),
                exact: catabolic_core::catalog_identity::ExactFileRevision {
                    file_id: text(&intent.observed, "id")?.into(),
                    revision: text(&intent.observed, "exact_revision")?.into(),
                },
                observed_status: Some("present".into()),
            },
            motion_file: file,
            version_files: vec![
                json!({"version_id":ids.version,"part":1,"file_id":ids.motion_file,"file_revision":probe["revision"],"start_ms":null,"end_ms":null}),
            ],
            association: ImportedAssociation {
                source_association_id: ids.association.clone(),
                source_occurrence_id: ids.motion_file.clone(),
                item_id: ids.version_item.clone(),
                part: 1,
            },
        },
    )
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
/// Retained server observation and discovery coordinator. The detached task
/// survives an HTTP caller disappearing, but receives cancellation immediately.
/// The actual native supervisor owns the Motion source writer fence as part of
/// its permit until OS-confirmed exit, including native cleanup-error paths.
pub async fn scan(
    app: &App,
    source_id: &str,
    cancellation: CancellationToken,
) -> anyhow::Result<Value> {
    scan_inner(app, source_id, cancellation, None, None).await
}
/// Dispatch only the immutable source accepted by the durable demand owner.
pub(crate) async fn scan_captured(
    app: &App,
    mapping: SourceMapping,
    accepted: playscale_core::native_scan::ExecutionIdentity,
    cancellation: CancellationToken,
) -> anyhow::Result<Value> {
    let source = mapping.library_id.clone();
    scan_inner(app, &source, cancellation, Some(mapping), Some(accepted)).await
}
async fn scan_inner(
    app: &App,
    source_id: &str,
    cancellation: CancellationToken,
    expected: Option<SourceMapping>,
    accepted: Option<playscale_core::native_scan::ExecutionIdentity>,
) -> anyhow::Result<Value> {
    let stop = cancellation.child_token();
    let cancel_on_drop = CancelOnDrop(stop.clone());
    let app = app.clone();
    let source_id = source_id.to_owned();
    let coordinator = tokio::spawn(async move {
        let runtime = app
            .native_catalog
            .as_ref()
            .context("native scan requires admitted runtime")?
            .clone();
        let permit = Arc::new(app.storage.scan_io.clone().acquire_owned().await?);
        let mut fence = app.db.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(accepted) = &accepted {
            crate::native_scans::check_execution_in(&mut fence, accepted).await?;
        }
        let mapping = current_source_in(&mut fence, &source_id).await?;
        if let Some(accepted) = &accepted {
            crate::native_scans::check_source_identity(accepted, &mapping)?;
        }
        ensure!(
            expected
                .as_ref()
                .is_none_or(|accepted| accepted == &mapping),
            "native source differs from accepted demand scope"
        );

        let custody = Arc::new(std::sync::Mutex::new(fence));
        let native_cancel = motion_catalog_backend::sources::ScanCancellation::default();
        if stop.is_cancelled() {
            native_cancel.cancel();
        }
        let watcher_stop = stop.clone();
        let watcher_cancel = native_cancel.clone();
        let watcher = tokio::spawn(async move {
            watcher_stop.cancelled().await;
            watcher_cancel.cancel();
        });
        let observed_mapping = mapping.clone();
        let observed = runtime
            .with_host(move |host| {
                let _permit_through_cleanup = permit.clone();
                let _fence_through_cleanup = custody.clone();
                let result = motion_catalog_backend::sources::scan_with_host(
                    host,
                    &observed_mapping,
                    &native_cancel,
                    std::time::Duration::from_secs(120),
                    (permit, custody),
                )?;
                ensure!(
                    result["complete"] == true,
                    "native scan did not prove complete coverage: {result}"
                );
                let snapshot = host.source_occurrences(&observed_mapping.native_source)?;
                ensure!(
                    snapshot["binding"] == observed_mapping.native_binding
                        && snapshot["policy"] == observed_mapping.native_policy,
                    "native source scope changed after observation"
                );
                Ok((result, snapshot))
            })
            .await;
        watcher.abort();
        let (observation, snapshot) =
            observed.context("native observation and retained cleanup")?;
        // The source writer custody and retained-host lock were released by
        // with_host above. Invalidate missing/replaced old references before a
        // later probe or discovery conflict can stop this scan's publication.
        runtime.rebuild_projection().await?;
        // Process exit and cleanup do not imply complete filesystem coverage.
        // Do not publish discoveries or satisfy a complete demand from a native
        // report that retained inventory after an unreadable/deferred subtree.
        require_complete_observation(&observation)?;
        ensure!(!stop.is_cancelled(), "native scan cancelled after cleanup");
        let mut created = Vec::new();
        let mut existing = 0usize;
        for observed in snapshot["occurrences"]
            .as_array()
            .context("native occurrence snapshot missing")?
        {
            if !is_media_candidate(observed) {
                continue;
            }
            ensure!(!stop.is_cancelled(), "native discovery cancelled");
            let known:Vec<String>=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE native_profile=? AND native_file_id=?").bind(&mapping.profile).bind(text(observed,"id")?).fetch_all(&app.db).await?;
            let mut current = false;
            for raw in known {
                let mapped: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
                    serde_json::from_str(&raw)?;
                if mapped.physical.exact.revision == text(observed, "exact_revision")? {
                    ensure!(
                        current_matches(&mapped.motion_file, observed),
                        "native exact occurrence revision did not change with physical fingerprint; explicit reconciliation required"
                    );
                    current = true;
                }
            }
            if current {
                existing += 1;
                continue;
            }
            let intent = prepare(&app.db, mapping.clone(), observed.clone())
                .await
                .context("persist native discovery intent")?;
            let intent = probe(&app, intent, stop.clone())
                .await
                .context("probe exact native occurrence")?;
            let file = intent.ids.motion_file.clone();
            let work = intent.ids.work.clone();
            let receipt = publish_captured(&app, intent, accepted.as_ref())
                .await
                .context("publish native discovery and acknowledge receipt")?;
            created.push(json!({"file_id":file,"item_id":work,"receipt_status":receipt["status"]}));
        }
        runtime.rebuild_projection().await?;
        Ok::<_, anyhow::Error>(
            json!({"source_id":source_id,"observation":observation,"created":created,"unchanged":existing}),
        )
    });
    let result = coordinator.await?;
    drop(cancel_on_drop);
    result
}

fn require_complete_observation(report: &Value) -> anyhow::Result<()> {
    ensure!(
        report["complete"] == true,
        "native observation coverage incomplete; inventory retained"
    );
    Ok(())
}

#[cfg(test)]
use crate as playscale_app;

#[cfg(test)]
#[path = "../tests/support/native_application_fixture.rs"]
mod current_source_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn current_source_admission_refuses_changed_mapping_before_journal_effect() {
        let mut fixture = current_source_fixture::Fixture::new().await;
        // This is source-mapping admission, not imported media projection:
        // activate the genuine empty catalog, then register a real source via
        // its public owner instead of seeding an unobserved physical work.
        fixture.cutover().await;
        let root = fixture._dir.path().join("registered-source");
        std::fs::create_dir(&root).unwrap();
        let registered = fixture
            .call(
                "POST",
                "/api/v2/sources",
                Some(json!({"name":"Mapping admission","root_path":root,"exclusions":[]})),
                &[
                    (
                        "authorization",
                        &format!("Bearer {}", current_source_fixture::OPERATOR),
                    ),
                    ("idempotency-key", "current-mapping-admission-source"),
                ],
            )
            .await;
        assert_eq!(
            registered.status,
            axum::http::StatusCode::CREATED,
            "{:?}",
            registered.body
        );
        let id = registered.body["id"].as_str().unwrap().to_owned();
        let mut writer = fixture.app.db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let current = current_source_in(&mut writer, &id).await.unwrap();
        require_current_source_in(&mut writer, &current)
            .await
            .unwrap();
        motion_catalog_backend::source_lifecycle::require_historical_mapping_in(
            &mut writer,
            &current,
        )
        .await
        .unwrap();
        let before: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_discovery_intents")
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        writer.rollback().await.unwrap();
        let mut substituted = current.clone();
        substituted.motion_root.push_str("-not-the-admitted-root");
        let refused = prepare(&fixture.app.db, substituted, Value::Null)
            .await
            .unwrap_err();
        assert!(
            format!("{refused:#}").contains("native source mapping changed before effect"),
            "{refused:#}"
        );
        let after: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_discovery_intents")
            .fetch_one(&fixture.app.db)
            .await
            .unwrap();
        assert_eq!(before, after);
        fixture.close().await;
    }

    #[test]
    fn helper_exit_or_cleanup_is_not_complete_inventory_coverage() {
        assert!(require_complete_observation(&json!({"complete":true})).is_ok());
        for report in [
            json!({"complete":false,"inventory_retained":true}),
            json!({"status":"completed"}),
            json!({"complete":"true"}),
            Value::Null,
        ] {
            assert!(require_complete_observation(&report).is_err());
        }
    }

    fn intent(duration: Option<f64>) -> DiscoveryIntent {
        DiscoveryIntent {
            operation_id: "motion.discovery:test".into(),
            source: SourceMapping {
                library_id: "source".into(),
                profile: "default".into(),
                native_source: "native".into(),
                catalog: playscale_core::catalog_backend::Identity {
                    catalog_id: "catalog".into(),
                    incarnation: "incarnation".into(),
                    schema_version: 39,
                },
                motion_root: "/media".into(),
                motion_root_identity: "1:2".into(),
                motion_binding_revision: 1,
                native_binding: json!({}),
                exclusions: vec![],
                native_policy: json!({}),
            },
            observed: json!({"id":"file","location":"native","path":"a.mp4","status":"present","exact_revision":"exact","size":10,"mtime_ns":20,"device":30,"inode":40,"ctime_ns":50000000060_i64}),
            ids: DiscoveryIds::new(),
            probe: Some(
                json!({"title":"A","duration_seconds":duration,"media_type":"video","relative_path":"a.mp4","revision":"content","fingerprint":"10:20:30:40:50:60","bytes":10,"tracks":[],"chapters":null}),
            ),
        }
    }
    #[test]
    fn measured_original_has_one_exact_ready_binding_and_no_equivalence() {
        let i = intent(Some(1.25));
        let payload = discovery_payload(&i, "motion").unwrap();
        let mutation: catabolic_store::catalog_identity_mutation::IdentityMutation =
            serde_json::from_value(payload["native"]["mutations"][0].clone()).unwrap();
        let version = &mutation.versions[0].version;
        catabolic_core::catalog_identity::validate_version(version).unwrap();
        assert_eq!(
            version.state,
            catabolic_core::catalog_identity::VersionState::Ready
        );
        assert_eq!(version.declared_coverage.unwrap().duration_us(), 1_250_000);
        assert_ne!(version.association_item_id, version.edition_item_id);
        assert!(mutation.versions[0].equivalence.is_none());
        assert!(mutation.evidence.is_empty());
        let grant = discovery_grant(&i, "motion").unwrap();
        assert_eq!(
            grant.measured_originals[&i.ids.version].observed_ctime_ns,
            50_000_000_060
        );
    }
    #[test]
    fn unknown_duration_stays_draft_without_placement() {
        let i = intent(None);
        let payload = discovery_payload(&i, "motion").unwrap();
        let mutation: catabolic_store::catalog_identity_mutation::IdentityMutation =
            serde_json::from_value(payload["native"]["mutations"][0].clone()).unwrap();
        let version = &mutation.versions[0].version;
        catabolic_core::catalog_identity::validate_version(version).unwrap();
        assert_eq!(
            version.state,
            catabolic_core::catalog_identity::VersionState::Draft
        );
        assert!(version.bindings[0].timeline_interval.is_none());
        assert!(version.declared_coverage.is_none());
        assert!(
            discovery_grant(&i, "motion")
                .unwrap()
                .measured_originals
                .is_empty()
        );
    }
    #[test]
    fn ready_contract_never_substitutes_for_full_physical_proof() {
        let i = intent(Some(1.0));
        let probe = i.probe.as_ref().unwrap();
        assert!(current_matches(probe, &i.observed));
        for field in ["size", "mtime_ns", "device", "inode", "ctime_ns"] {
            let mut changed = i.observed.clone();
            changed[field] = json!(changed[field].as_i64().unwrap() + 1);
            // Even the same exact native revision cannot authorize old bytes.
            assert_eq!(changed["exact_revision"], i.observed["exact_revision"]);
            assert!(!current_matches(probe, &changed), "{field}");
        }
        let mut absent = i.observed.clone();
        absent["status"] = json!("missing");
        assert!(!current_matches(probe, &absent));
    }
    #[tokio::test]
    async fn discovery_retry_keeps_ids_and_refuses_same_revision_changed_tuple() {
        async fn journal_only(
            db: &sqlx::SqlitePool,
            source: SourceMapping,
            observed: Value,
        ) -> anyhow::Result<DiscoveryIntent> {
            let mut tx = db.begin_with("BEGIN IMMEDIATE").await?;
            let intent = prepare_journal_in(&mut tx, source, observed).await?;
            tx.commit().await?;
            Ok(intent)
        }
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/0079_catabolic_discovery_intents.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        let i = intent(Some(1.0));
        let first = journal_only(&db, i.source.clone(), i.observed.clone())
            .await
            .unwrap();
        let second = journal_only(&db, i.source.clone(), i.observed.clone())
            .await
            .unwrap();
        assert_eq!(first.ids, second.ids);
        assert_eq!(first.operation_id, second.operation_id);
        sqlx::query("UPDATE catabolic_discovery_intents SET probe_json='{}',command_json='{}',receipt_json='{}',state='committed' WHERE operation_id=?")
            .bind(&first.operation_id).execute(&db).await.unwrap();
        for assignment in [
            "probe_json=NULL",
            "command_json=NULL",
            "receipt_json=NULL",
            "state='probed'",
            "ids_json='{}'",
        ] {
            assert!(
                sqlx::query(&format!(
                    "UPDATE catabolic_discovery_intents SET {assignment} WHERE operation_id=?"
                ))
                .bind(&first.operation_id)
                .execute(&db)
                .await
                .is_err(),
                "{assignment}"
            );
        }
        assert!(
            sqlx::query("DELETE FROM catabolic_discovery_intents WHERE operation_id=?")
                .bind(&first.operation_id)
                .execute(&db)
                .await
                .is_err()
        );
        let mut changed = i.observed;
        changed["ctime_ns"] = json!(60);
        assert!(journal_only(&db, i.source, changed).await.is_err());
    }
}
