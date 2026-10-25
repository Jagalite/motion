//! Immutable receipt-derived baselines between Motion and native CAS domains.
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::{catalog_commands::CommandOutcome, catalog_import::ImportReceipt};
use serde_json::json;
use sqlx::SqliteConnection;

async fn record(
    db: &mut SqliteConnection,
    id: &str,
    catalog: &str,
    incarnation: &str,
    motion: i64,
    native: i64,
    proof: serde_json::Value,
) -> anyhow::Result<()> {
    ensure!(motion >= 1 && native >= 1, "invalid revision baseline");
    let old:Option<(String,String,i64,i64)>=sqlx::query_as("SELECT catalog_id,incarnation,motion_base,native_base FROM catabolic_revision_bridges WHERE native_id=?").bind(id).fetch_optional(&mut *db).await?;
    if let Some(old) = old {
        ensure!(
            old == (catalog.into(), incarnation.into(), motion, native),
            "native revision baseline changed"
        );
        return Ok(());
    }
    sqlx::query("INSERT INTO catabolic_revision_bridges VALUES(?,?,?,?,?,?)")
        .bind(id)
        .bind(catalog)
        .bind(incarnation)
        .bind(motion)
        .bind(native)
        .bind(proof.to_string())
        .execute(db)
        .await?;
    Ok(())
}
pub async fn wire_revision_in(
    db: &mut SqliteConnection,
    id: &str,
    catalog: &str,
    incarnation: &str,
    head: i64,
) -> anyhow::Result<u64> {
    let (c,i,base,native):(String,String,i64,i64)=sqlx::query_as("SELECT catalog_id,incarnation,motion_base,native_base FROM catabolic_revision_bridges WHERE native_id=?").bind(id).fetch_optional(db).await?.context("receipt-derived revision baseline is missing")?;
    ensure!(
        c == catalog && i == incarnation,
        "revision baseline incarnation mismatch"
    );
    let revision = playscale_core::catalog_projection::bridged_revision(base, native, head)
        .context("native revision precedes baseline or overflows")?;
    Ok(revision.try_into()?)
}
/// Call only under the same host acknowledgment transaction after independently
/// reconciling this exact durable activation receipt against the saved checkpoint.
pub(crate) async fn acknowledge_import_in(
    db: &mut SqliteConnection,
    receipt: &ImportReceipt,
) -> anyhow::Result<()> {
    let ImportReceipt::Activated {
        catalog_id,
        incarnation,
        import_id,
        item_revision_baselines,
        mappings,
        ..
    } = receipt
    else {
        anyhow::bail!("activation receipt required")
    };
    let snapshot: String = sqlx::query_scalar(
        "SELECT snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",
    )
    .bind(import_id)
    .fetch_one(&mut *db)
    .await?;
    let snapshot: crate::migration::Snapshot = serde_json::from_str(&snapshot)?;
    let mut source_bases = std::collections::BTreeMap::<String, i64>::new();
    for metadata in snapshot
        .batches
        .iter()
        .filter_map(|b| b.typed.as_ref())
        .flat_map(|t| &t.metadata)
    {
        for contribution in &metadata.contributions {
            if !matches!(
                contribution.namespace.as_str(),
                "motion.structure" | "motion.history"
            ) {
                continue;
            }
            let fields = &contribution.fields;
            let base = if fields["motion_catalog"]["item_id"] == metadata.item_id {
                fields["motion_catalog"]["catalog_revision"].as_i64()
            } else if fields["motion_library"]["contract_version"] == 1 {
                let library = &fields["motion_library"];
                ensure!(
                    library["id"]
                        .as_str()
                        .is_some_and(|id| metadata.item_id == format!("motion.library:{id}")),
                    "library baseline identity mismatch"
                );
                library["catalog_revision"].as_i64()
            } else if fields["motion_edition"]["contract_version"] == 1 {
                let edition = &fields["motion_edition"]["source_edition"];
                ensure!(
                    edition["id"]
                        .as_str()
                        .is_some_and(|id| metadata.item_id == format!("motion.edition:{id}")),
                    "edition baseline identity mismatch"
                );
                edition["revision"].as_i64()
            } else {
                historical_receipt_anchor(fields, &metadata.item_id)?
            };
            if let Some(base) = base {
                ensure!(base >= 1, "invalid source revision baseline");
                if let Some(old) = source_bases.insert(metadata.item_id.clone(), base) {
                    ensure!(old == base, "conflicting source revision baselines");
                }
            }
        }
    }
    for mapping in mappings {
        // Mapping source/native IDs may be heterogeneous; select exact frozen
        // source works, never infer a work from a physical or library ID.
        let id = mapping.item_id.as_str();
        let prior:Option<(String,String,i64,String)>=sqlx::query_as("SELECT catalog_id,incarnation,native_base,proof_json FROM catabolic_revision_bridges WHERE native_id=?").bind(id).fetch_optional(&mut *db).await?;
        if let Some((catalog, inc, head, proof)) = prior {
            let proof: serde_json::Value = serde_json::from_str(&proof)?;
            ensure!(
                catalog == *catalog_id
                    && inc == *incarnation
                    && item_revision_baselines.get(id) == Some(&head)
                    && proof["kind"] == "native_import_activation"
                    && proof["import_id"] == *import_id,
                "replayed activation revision proof changed"
            );
            // Application edits may have advanced the disposable Motion row.
            // Its current revision can never replace this immutable source base.
            continue;
        }
        let Some(base) = source_bases.get(id).copied() else {
            continue;
        };
        let native = *item_revision_baselines
            .get(id)
            .context("activation receipt lacks atomic item revision baseline")?;
        record(
            db,
            id,
            catalog_id,
            incarnation,
            base,
            native,
            json!({"kind":"native_import_activation","import_id":import_id,"native_base":native}),
        )
        .await?;
    }
    Ok(())
}
/// A create-only command has no previous item or metadata owner. Its initial
/// native item head is one by that command's creation contract. Never accept
/// upsert, redacted/expired/unknown outcomes, or a later observed current head.
pub async fn acknowledge_fresh_in(
    db: &mut SqliteConnection,
    id: &str,
    intent: &CommandEnvelope,
    outcome: &CommandOutcome,
) -> anyhow::Result<()> {
    let result = match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => anyhow::bail!("full successful creation receipt required"),
    };
    ensure!(
        intent.payload()["operation"] == "item_create_batch"
            && result["operation"] == "item_create_batch",
        "create-only receipt required"
    );
    let requested = intent.payload()["items"]
        .as_array()
        .context("creation items missing")?
        .iter()
        .find(|i| i["id"] == id)
        .context("item absent from create intent")?;
    ensure!(
        requested["identities"].is_null(),
        "fresh revision bridge cannot admit identity coalescing"
    );
    ensure!(
        result["items"]
            .as_array()
            .context("creation result missing")?
            .iter()
            .any(|i| i["id"] == id),
        "item absent from creation result"
    );
    record(
        db,
        id,
        intent.key().catalog_id(),
        intent.admission_incarnation(),
        1,
        1,
        json!({"kind":"native_create_only","intent":intent.canonical_payload(),"result":result}),
    )
    .await
}
/// Full receipt acknowledgment for the exact fixed empty-edition command.
/// The retained host must reconcile this outcome before this host-only bridge.
pub async fn acknowledge_empty_edition_in(
    db: &mut SqliteConnection,
    intent: &CommandEnvelope,
    outcome: &CommandOutcome,
) -> anyhow::Result<()> {
    let result = match outcome {
        CommandOutcome::Committed { result, .. } | CommandOutcome::Replay { result, .. } => result,
        _ => anyhow::bail!("full empty edition receipt required"),
    };
    ensure!(
        intent.payload()["operation"] == "empty_edition_create"
            && result["operation"] == "empty_edition_create"
            && result["contract_version"] == 1,
        "empty edition receipt operation mismatch"
    );
    let request: catabolic_store::catalog_edition_creation::EmptyEditionRequest =
        serde_json::from_value(intent.payload()["request"].clone())?;
    let id = format!("motion.edition:{}", request.edition_id);
    let model = &result["item"]["metadata"]["motion_edition"];
    ensure!(
        result["work_id"] == request.work_id
            && result["edition_id"] == request.edition_id
            && result["timeline_id"] == request.timeline_id
            && request.timeline_id == request.edition_id
            && result["item"]["id"] == id
            && result["item"]["kind"] == "custom:motion_edition"
            && model["contract_version"] == 1
            && model["origin"] == "native_empty_edition"
            && model["creation_request_digest"] == request.request_digest
            && model["source_edition"]
                == json!({"id":request.edition_id,"item_id":request.work_id,"label":request.label,"revision":1})
            && model["source_timelines"]
                == json!([{"id":request.timeline_id,"edition_id":request.edition_id,"revision":1,"duration_ms":null,"order_group_id":null,"order_position":null}])
            && model["editorial_context"]["id"] == request.context_id
            && result["timeline_revision"] == 1,
        "empty edition receipt protected identity changed"
    );
    let native = result["item_revision_baselines"][&id]
        .as_i64()
        .filter(|n| *n > 0)
        .context("empty edition atomic item baseline missing")?;
    ensure!(
        result["item_revision_baselines"]
            .as_object()
            .is_some_and(|v| v.len() == 1),
        "unexpected empty edition baseline set"
    );
    record(
        db,
        &id,
        intent.key().catalog_id(),
        intent.admission_incarnation(),
        1,
        native,
        json!({"kind":"native_empty_edition","intent":intent.canonical_payload(),"result":result}),
    )
    .await
}

/// The coordinator calls this only for the full committed/replayed outcome of
/// its exact persisted discovery intent, in the same host acknowledgment txn.
pub async fn acknowledge_discovery_in(
    db: &mut SqliteConnection,
    id: &str,
    catalog: &str,
    incarnation: &str,
    operation_id: &str,
    result: &serde_json::Value,
) -> anyhow::Result<()> {
    ensure!(
        result["operation"] == "fresh_media_discovery",
        "discovery receipt required"
    );
    ensure!(
        result["items"]
            .as_array()
            .context("discovery items missing")?
            .iter()
            .any(|i| i["id"] == id),
        "discovery item missing"
    );
    for item in result["items"]
        .as_array()
        .context("discovery items missing")?
    {
        let native_id = item["id"].as_str().context("discovery item ID missing")?;
        let native = result["item_revision_baselines"][native_id]
            .as_i64()
            .context("atomic discovery item baseline missing")?;
        record(
            db,
            native_id,
            catalog,
            incarnation,
            1,
            native,
            json!({"kind":"native_discovery","operation_id":operation_id,"native_base":native}),
        )
        .await?;
    }
    Ok(())
}

// History metadata is immutable source evidence, not an editable current file
// owner. Record the atomic import head so a later metadata edit cannot rewrite
// preserved application revisions under the same descriptive owner ID.
fn historical_receipt_anchor(
    fields: &serde_json::Value,
    item_id: &str,
) -> anyhow::Result<Option<i64>> {
    let tombstone = &fields["motion_reference_tombstone"];
    if !tombstone.is_null() {
        ensure!(
            tombstone["contract_version"] == 1 && tombstone["absent_from_frozen_catalog"] == true,
            "invalid historical reference tombstone"
        );
        let table = tombstone["target_table"]
            .as_str()
            .context("tombstone target missing")?;
        let original = tombstone["original_id"]
            .as_str()
            .context("tombstone identity missing")?;
        ensure!(
            ["timelines", "media_versions", "media_files"].contains(&table),
            "unsupported tombstone target"
        );
        let digest = catabolic_core::sha256(serde_json::to_vec(&(table, original))?);
        ensure!(
            item_id == format!("motion.tombstone:{digest}"),
            "tombstone receipt identity mismatch"
        );
        return Ok(Some(1));
    }
    for (field, row, prefix) in [
        (
            "motion_historical_file",
            "source_file",
            "motion.file-history:",
        ),
        (
            "motion_historical_timeline",
            "source_timeline",
            "motion.timeline-history:",
        ),
        (
            "motion_historical_version",
            "source_version",
            "motion.version:",
        ),
        ("motion_source_history", "source_row", "motion.source:"),
    ] {
        if fields[field].is_null() {
            continue;
        }
        ensure!(
            fields[field]["contract_version"] == 1,
            "historical receipt contract changed"
        );
        let id = fields[field][row]["id"]
            .as_str()
            .context("historical source identity missing")?;
        ensure!(
            item_id == format!("{prefix}{id}"),
            "historical receipt identity mismatch"
        );
        return Ok(Some(1));
    }
    Ok(None)
}

/// Descriptive provenance for the exact successful fresh-work creation attempt.
/// This is NOT an initialization grant. The caller must reconcile this exact
/// full native receipt under retained identity/custody before deriving one.
pub async fn created_origin_intent_in(
    db: &mut SqliteConnection,
    id: &str,
    catalog: &str,
    incarnation: &str,
) -> anyhow::Result<Option<String>> {
    let row: Option<(String,String,i64,i64,String)> = sqlx::query_as("SELECT catalog_id,incarnation,motion_base,native_base,proof_json FROM catabolic_revision_bridges WHERE native_id=?")
        .bind(id).fetch_optional(&mut *db).await?;
    let Some((stored_catalog, stored_incarnation, motion_base, native_base, raw)) = row else {
        return Ok(None);
    };
    ensure!(
        stored_catalog == catalog && stored_incarnation == incarnation,
        "creation bridge identity changed"
    );
    let proof: serde_json::Value = serde_json::from_str(&raw)?;
    if proof["kind"] != "native_create_only" {
        return Ok(None);
    }
    ensure!(
        motion_base == 1 && native_base == 1,
        "fresh creation baseline changed"
    );
    let canonical = proof["intent"]
        .as_str()
        .context("creation proof intent missing")?;
    let completed = crate::command_attempts::decode(canonical)?;
    ensure!(
        completed.key().catalog_id() == catalog
            && completed.admission_incarnation() == incarnation
            && completed.payload()["operation"] == "item_create_batch",
        "creation proof command identity changed"
    );
    let requested = completed.payload()["items"]
        .as_array()
        .context("creation proof items missing")?;
    let matching: Vec<_> = requested.iter().filter(|item| item["id"] == id).collect();
    ensure!(
        matching.len() == 1 && matching[0]["identities"].is_null(),
        "creation proof subject absent, ambiguous or identity-coalescing"
    );
    // Libraries and other fresh native owner domains are not work origins.
    let item = matching[0];
    if item["metadata"]["motion_catalog"].is_null() {
        return Ok(None);
    }
    ensure!(
        item["metadata"]["motion_catalog"]["contract_version"] == 1
            && item["metadata"]["motion_catalog"]["item_id"] == id
            && matches!(item["kind"].as_str(), Some("video" | "custom:motion_audio")),
        "creation work origin projection changed"
    );
    ensure!(
        proof["result"]["operation"] == "item_create_batch"
            && proof["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().filter(|item| item["id"] == id).count() == 1),
        "creation proof full result missing its subject"
    );
    // ACK stores the successful attempt, not necessarily the original key.
    // Resolve only exact stored bytes, then validate the entire bounded chain.
    let originals: Vec<String> = sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE envelope_json=?1 UNION ALL SELECT original_envelope_json FROM catabolic_command_attempts WHERE envelope_json=?1 LIMIT 2")
        .bind(canonical).fetch_all(&mut *db).await?;
    ensure!(
        originals.len() == 1,
        "creation proof business intent missing or ambiguous"
    );
    let original = crate::command_attempts::decode(&originals[0])?;
    let (_, latest) = crate::command_attempts::latest_in(db, &original).await?;
    ensure!(
        latest.canonical_payload() == canonical,
        "creation proof is not the exact terminal attempt"
    );
    Ok(Some(canonical.to_owned()))
}

#[cfg(test)]
mod origin_tests {
    use super::*;
    use catabolic_core::catalog_command::CommandKey;
    use sqlx::Connection;

    fn intent(id: &str, operation: &str, generation: i64) -> CommandEnvelope {
        CommandEnvelope::new(1,CommandKey::new("catalog","host",operation).unwrap(),"incarnation",generation,
            &json!({"operation":"item_create_batch","items":[{"id":id,"kind":"video","metadata":{"title":"Original","motion_catalog":{"contract_version":1,"item_id":id}}}]}).to_string()).unwrap()
    }
    async fn fixture() -> (SqliteConnection, CommandEnvelope) {
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        // Audit-reader fixture only: no fabricated receipt is used to authorize
        // a native mutation, and mutable tables allow explicit corruption tests.
        sqlx::raw_sql("CREATE TABLE catabolic_revision_bridges(native_id TEXT PRIMARY KEY,catalog_id TEXT,incarnation TEXT,motion_base INTEGER,native_base INTEGER,proof_json TEXT); CREATE TABLE catabolic_commands(operation_id TEXT PRIMARY KEY,envelope_json TEXT,created_at INTEGER); CREATE TABLE catabolic_discovery_intents(operation_id TEXT,command_json TEXT); CREATE TABLE catabolic_command_attempts(business_id TEXT,attempt INTEGER,original_envelope_json TEXT,prior_envelope_json TEXT,envelope_json TEXT,stale_witness_json TEXT,created_at INTEGER);").execute(&mut db).await.unwrap();
        let command = intent("work", "create-work", 7);
        sqlx::query("INSERT INTO catabolic_commands VALUES(?,?,0)")
            .bind(command.key().operation_id())
            .bind(command.canonical_payload())
            .execute(&mut db)
            .await
            .unwrap();
        proof(&mut db, &command).await;
        (db, command)
    }
    async fn proof(db: &mut SqliteConnection, command: &CommandEnvelope) {
        sqlx::query("INSERT OR REPLACE INTO catabolic_revision_bridges VALUES('work','catalog','incarnation',1,1,?)")
            .bind(json!({"kind":"native_create_only","intent":command.canonical_payload(),"result":{"operation":"item_create_batch","items":[{"id":"work"}]}}).to_string()).execute(db).await.unwrap();
    }
    #[tokio::test]
    async fn created_origin_is_exact_and_rejects_subject_identity_and_canonical_tampering() {
        let (mut db, command) = fixture().await;
        assert_eq!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .unwrap(),
            Some(command.canonical_payload().into())
        );
        assert!(
            created_origin_intent_in(&mut db, "work", "other", "incarnation")
                .await
                .is_err()
        );
        assert!(
            created_origin_intent_in(&mut db, "work", "catalog", "other")
                .await
                .is_err()
        );
        assert_eq!(
            created_origin_intent_in(&mut db, "missing", "catalog", "incarnation")
                .await
                .unwrap(),
            None
        );
        proof(&mut db, &intent("other-work", "create-work", 7)).await;
        assert!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .is_err()
        );
        proof(&mut db, &command).await;
        sqlx::query(
            "UPDATE catabolic_revision_bridges SET proof_json=json_set(proof_json,'$.intent',? )",
        )
        .bind(format!(" {}", command.canonical_payload()))
        .execute(&mut db)
        .await
        .unwrap();
        assert!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .is_err()
        );
        proof(&mut db, &command).await;
        sqlx::query("UPDATE catabolic_revision_bridges SET native_base=2")
            .execute(&mut db)
            .await
            .unwrap();
        assert!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .is_err()
        );
        sqlx::query(r#"UPDATE catabolic_revision_bridges SET proof_json='{"kind":"native_import_activation"}'"#)
        .execute(&mut db)
        .await
        .unwrap();
        assert_eq!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .unwrap(),
            None
        );
    }
    #[tokio::test]
    async fn created_origin_checks_successful_attempt_against_the_entire_saved_chain() {
        let (mut db, original) = fixture().await;
        let operation =
            playscale_core::catalog_command_attempt::operation_id(original.digest().as_str(), 1)
                .unwrap();
        let next = intent("work", &operation, 11);
        sqlx::query("INSERT INTO catabolic_command_attempts VALUES(?,1,?,?,?,?,0)")
            .bind(original.key().operation_id())
            .bind(original.canonical_payload())
            .bind(original.canonical_payload())
            .bind(next.canonical_payload())
            .bind(
                json!({"envelope":original.canonical_payload(),"current_generation":11})
                    .to_string(),
            )
            .execute(&mut db)
            .await
            .unwrap();
        // An older proof cannot silently migrate to a newer prepared attempt.
        assert!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .is_err()
        );
        proof(&mut db, &next).await;
        assert_eq!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .unwrap(),
            Some(next.canonical_payload().into())
        );
        sqlx::query("UPDATE catabolic_command_attempts SET stale_witness_json=json_set(stale_witness_json,'$.current_generation',7)").execute(&mut db).await.unwrap();
        assert!(
            created_origin_intent_in(&mut db, "work", "catalog", "incarnation")
                .await
                .is_err()
        );
    }
}
