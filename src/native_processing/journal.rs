//! Durable command reservation and exact receipt recording. No path here
//! dispatches a command after restart or promotes stored JSON to a native grant.
use super::{PublicationPlan, PublicationRecord, retain_in};
use anyhow::{Context, ensure};
use catabolic_core::catalog_command::CommandEnvelope;
use catabolic_store::catalog_commands::CommandOutcome;
use playscale_core::{
    processing::{Completion, PreparationFacts, State},
    processing_publication::{CommittedReceipt, Identity, Reservation},
};
use serde_json::{Value, json};

pub(super) struct SavedIntent {
    pub command: CommandEnvelope,
    /// Verified latest immutable native attempt; never replaces business identity.
    pub attempt: CommandEnvelope,
    pub reservation: Reservation,
}
impl SavedIntent {
    fn encode(&self) -> anyhow::Result<Value> {
        Ok(
            json!({"contract_version":1,"command":serde_json::from_str::<Value>(self.command.canonical_payload())?,"reservation":self.reservation}),
        )
    }
    fn decode(value: &Value) -> anyhow::Result<Self> {
        ensure!(
            value["contract_version"] == 1,
            "unsupported generated intent"
        );
        let saved = &value["command"];
        let command = CommandEnvelope::new(
            1,
            serde_json::from_value(saved["key"].clone())?,
            saved["admission_incarnation"]
                .as_str()
                .context("generated intent incarnation missing")?,
            saved["expected_generation"]
                .as_i64()
                .context("generated intent generation missing")?,
            &saved["payload"].to_string(),
        )?;
        ensure!(
            serde_json::from_str::<Value>(command.canonical_payload())? == *saved,
            "generated intent canonical command changed"
        );
        let reservation: Reservation = serde_json::from_value(value["reservation"].clone())?;
        reservation.validate().map_err(anyhow::Error::msg)?;
        let id = reservation.identity();
        ensure!(
            id.catalog_id == command.key().catalog_id()
                && id.incarnation == command.admission_incarnation()
                && id.operation_id == command.key().operation_id()
                && id.payload_digest == command.digest().as_str()
                && command.payload()["operation"] == "generated_output_admit"
                && command.payload()["request"]["output"]["source_occurrence_id"]
                    == id.output_file_id,
            "generated reservation differs from command"
        );
        Ok(Self {
            attempt: command.clone(),
            command,
            reservation,
        })
    }
}

/// Caller observes authorization/source/preparation facts under this same owned
/// writer and commits before any dispatch. A competing cancel must acquire the
/// writer and inspect this reservation before changing the attempt.
pub(super) async fn reserve_command_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    plan: &PublicationPlan,
    command: CommandEnvelope,
    before: State,
    completion: Completion,
    facts: PreparationFacts,
) -> anyhow::Result<SavedIntent> {
    ensure!(
        command.key().operation_id() == plan.operation_id
            && command.payload()["request"]["job_id"] == plan.job_id
            && command.payload()["request"]["attempt"].as_i64() == Some(plan.attempt),
        "generated command attempt differs from planned output"
    );
    let output = plan.source["ids"]["file"]
        .as_str()
        .context("generated output ID missing")?;
    let reservation = Reservation::reserve(
        Identity {
            catalog_id: command.key().catalog_id().into(),
            incarnation: command.admission_incarnation().into(),
            operation_id: plan.operation_id.clone(),
            payload_digest: command.digest().as_str().into(),
            output_file_id: output.into(),
        },
        before,
        completion,
        facts,
    )
    .map_err(anyhow::Error::msg)?;
    sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?) ON CONFLICT(operation_id) DO NOTHING")
        .bind(&plan.operation_id).bind(command.canonical_payload()).bind(crate::now()).execute(&mut **tx).await?;
    let original: String =
        sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
            .bind(&plan.operation_id)
            .fetch_one(&mut **tx)
            .await?;
    ensure!(
        original == command.canonical_payload(),
        "generated original business command changed"
    );
    let saved = SavedIntent {
        attempt: command.clone(),
        command,
        reservation,
    };
    let value = saved.encode()?;
    SavedIntent::decode(&value)?;
    retain_in(tx, &plan.operation_id, PublicationRecord::Command, &value).await?;
    load_in(&mut **tx, &plan.operation_id).await
}

pub(super) async fn load_in(
    conn: &mut sqlx::SqliteConnection,
    operation: &str,
) -> anyhow::Result<SavedIntent> {
    let encoded:String=sqlx::query_scalar("SELECT command_json FROM catabolic_generated_publications WHERE operation_id=? AND command_json IS NOT NULL")
        .bind(operation).fetch_one(&mut *conn).await?;
    let mut saved = SavedIntent::decode(&serde_json::from_str(&encoded)?)?;
    ensure!(
        saved.command.key().operation_id() == operation,
        "generated business operation changed"
    );
    let (_, attempt) =
        motion_catalog_backend::command_attempts::latest_in(conn, &saved.command).await?;
    saved.attempt = attempt;
    Ok(saved)
}

/// Only the retained native owner can mint this older-generation no-effect
/// witness. Caller must commit the successor before dispatching it.
pub(super) async fn record_next_in(
    conn: &mut sqlx::SqliteConnection,
    intent: &SavedIntent,
    witness: catabolic_runtime::host::StaleGenerationWitness,
) -> anyhow::Result<SavedIntent> {
    let attempt = motion_catalog_backend::command_attempts::record_next_in(
        conn,
        &intent.command,
        &intent.attempt,
        witness,
    )
    .await?;
    Ok(SavedIntent {
        command: intent.command.clone(),
        attempt,
        reservation: intent.reservation.clone(),
    })
}

/// Only actual full native Committed/Replay outcomes can reach this recorder.
/// Missing/expired/redacted/unknown outcomes retain the reservation unchanged.
pub(super) async fn record_receipt_in(
    conn: &mut sqlx::SqliteConnection,
    intent: &SavedIntent,
    outcome: &CommandOutcome,
) -> anyhow::Result<Reservation> {
    let (result, generation, retain_until) = match outcome {
        CommandOutcome::Committed {
            result,
            committed_generation,
            retain_until_us,
        }
        | CommandOutcome::Replay {
            result,
            committed_generation,
            retain_until_us,
        } => (result, *committed_generation, *retain_until_us),
        _ => anyhow::bail!("generated full receipt required: {}", outcome.code()),
    };
    let request = &intent.command.payload()["request"];
    ensure!(
        result["operation"] == "generated_output_admit"
            && result["job_id"] == request["job_id"]
            && result["attempt"] == request["attempt"]
            && result["preparation_digest"] == request["preparation_digest"]
            && result["output"] == request["output"]
            && result["output_ctime_ns"] == request["output_ctime_ns"]
            && result["output_size"] == request["output_size"]
            && result["output_sha256"] == request["output_sha256"]
            && result["output_duration_us"] == request["output_duration_us"]
            && result["provenance"] == request["provenance"],
        "generated receipt does not match exact prepared output"
    );
    // Bind the actual retained native receipt attempt, while core reservation
    // and all application references retain the original business identity.
    let (_, latest) =
        motion_catalog_backend::command_attempts::latest_in(conn, &intent.command).await?;
    ensure!(
        latest == intent.attempt,
        "generated receipt attempt is not the exact current durable chain"
    );
    let receipt = json!({"contract_version":1,"result":result,"committed_generation":generation,"retain_until_us":retain_until,
        "attempt_envelope":serde_json::from_str::<Value>(intent.attempt.canonical_payload())?});
    let digest = catabolic_core::sha256(serde_json::to_vec(&receipt)?);
    let recorded = intent
        .reservation
        .record_receipt(CommittedReceipt {
            identity: intent.reservation.identity().clone(),
            receipt_digest: digest,
        })
        .map_err(anyhow::Error::msg)?;
    retain_in(
        conn,
        intent.command.key().operation_id(),
        PublicationRecord::Receipt,
        &receipt,
    )
    .await?;
    Ok(recorded)
}

/// Record the receipt-derived physical translation before rebuilding disposable
/// caches. This is not the application job ACK: cancellation remains reserved
/// until that separate FK-safe acknowledgment is durably complete.
pub(super) async fn acknowledge_mapping_in(
    conn: &mut sqlx::SqliteConnection,
    intent: &SavedIntent,
) -> anyhow::Result<()> {
    let (output, receipt): (String, String) = sqlx::query_as(
        "SELECT output_json,receipt_json FROM catabolic_generated_publications WHERE operation_id=? AND receipt_json IS NOT NULL",
    ).bind(intent.command.key().operation_id()).fetch_one(&mut *conn).await?;
    let output: Value = serde_json::from_str(&output)?;
    let receipt: Value = serde_json::from_str(&receipt)?;
    let source = motion_catalog_backend::generated_source::mapping_in(conn)
        .await?
        .context("private generated source missing")?;
    ensure!(
        output["mapping"] == serde_json::to_value(&source)?,
        "generated output source changed"
    );
    let mapping = motion_catalog_backend::generated_publication::mapping(
        &intent.command,
        &receipt["result"],
        &source,
        &output["probe"],
        receipt["committed_generation"]
            .as_i64()
            .context("generated receipt generation missing")?,
    )?;
    let file_id = mapping.motion_file["id"]
        .as_str()
        .context("generated mapped file ID missing")?;
    let import_id = format!("generated:{}", intent.command.key().operation_id());
    sqlx::query("INSERT INTO catabolic_occurrence_mappings(import_id,motion_file_id,native_profile,native_file_id,mapping_json) VALUES(?,?,?,?,?) ON CONFLICT(import_id,motion_file_id) DO NOTHING")
        .bind(&import_id).bind(file_id).bind(&mapping.physical.profile).bind(&mapping.physical.exact.file_id).bind(serde_json::to_string(&mapping)?).execute(&mut *conn).await?;
    let saved:String=sqlx::query_scalar("SELECT mapping_json FROM catabolic_occurrence_mappings WHERE import_id=? AND motion_file_id=?")
        .bind(&import_id).bind(file_id).fetch_one(&mut *conn).await?;
    ensure!(
        serde_json::from_str::<motion_catalog_backend::occurrence_bridge::OccurrenceMapping>(
            &saved
        )? == mapping,
        "generated physical acknowledgment changed"
    );
    let id = intent.command.payload()["request"]["item"]["id"]
        .as_str()
        .context("generated item missing")?;
    let native = receipt["result"]["item_revision_baselines"][id]
        .as_i64()
        .filter(|head| *head > 0)
        .context("generated atomic item baseline missing")?;
    let proof = json!({"kind":"native_generated_output","operation_id":intent.command.key().operation_id(),"native_base":native});
    sqlx::query("INSERT INTO catabolic_revision_bridges(native_id,catalog_id,incarnation,motion_base,native_base,proof_json) VALUES(?,?,?,1,?,?) ON CONFLICT(native_id) DO NOTHING")
        .bind(id).bind(intent.command.key().catalog_id()).bind(intent.command.admission_incarnation()).bind(native).bind(proof.to_string()).execute(&mut *conn).await?;
    let saved:(String,String,i64,i64,String)=sqlx::query_as("SELECT catalog_id,incarnation,motion_base,native_base,proof_json FROM catabolic_revision_bridges WHERE native_id=?")
        .bind(id).fetch_one(&mut *conn).await?;
    ensure!(
        saved.0 == intent.command.key().catalog_id()
            && saved.1 == intent.command.admission_incarnation()
            && saved.2 == 1
            && saved.3 == native
            && serde_json::from_str::<Value>(&saved.4)? == proof,
        "generated receipt-derived baseline changed"
    );
    Ok(())
}

/// A reservation is evidence that publication is already committing, NOT that
/// a native result exists. Startup must call this receipt-only owner operation;
/// `None` leaves intent/output/attempt retained and performs no dispatch.
pub(super) fn reconcile_only(
    host: &mut catabolic_runtime::host::HostSession,
    intent: &SavedIntent,
) -> anyhow::Result<Option<CommandOutcome>> {
    Ok(host.reconcile_command_receipt(&intent.attempt)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;
    #[tokio::test]
    async fn actual_stale_attempt_preserves_business_reservation_and_restart_never_dispatches() {
        use catabolic_core::catalog_command::{CommandKey, ReceiptVisibility};
        use catabolic_store::catalog_commands::{ReceiptPolicy, TrustedCommandAuthority};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite");
        catabolic_store::migration::initialize_native_at(&path, 39).unwrap();
        let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
            "motion".into(),
            "test".into(),
        )
        .unwrap();
        let mut host = catabolic_runtime::host::HostSession::open_authorized(
            &path,
            "default",
            true,
            catabolic_runtime::host::CONTRACT_VERSION,
            gate,
        )
        .unwrap();
        let identity = host.identity().unwrap();
        let generation = host.catalog_generation().unwrap();
        let command=CommandEnvelope::new(1,CommandKey::new(identity["catalog_id"].as_str().unwrap(),"motion","generated-stale").unwrap(),identity["catalog_incarnation"].as_str().unwrap(),generation,&json!({"operation":"generated_output_admit","request":{"job_id":"job","attempt":1,"output":{"source_occurrence_id":"output"}}}).to_string()).unwrap();
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0066_catabolic_command_journal.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0085_catabolic_command_attempts.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0087_catabolic_generated_publications.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        sqlx::raw_sql("CREATE TABLE catabolic_discovery_intents(operation_id TEXT PRIMARY KEY,command_json TEXT)").execute(&mut db).await.unwrap();
        let plan = PublicationPlan {
            operation_id: "generated-stale".into(),
            job_id: "job".into(),
            attempt: 1,
            output_relative: "result.mp4".into(),
            source: json!({"ids":{"file":"output"}}),
        };
        super::super::plan_in(&mut db, &plan).await.unwrap();
        retain_in(
            &mut db,
            &plan.operation_id,
            PublicationRecord::Output,
            &json!({"fixture":"not a publication grant"}),
        )
        .await
        .unwrap();
        let state = State {
            job: playscale_core::jobs::Job {
                phase: playscale_core::jobs::Phase::Running,
                attempt: 1,
            },
            source_revision: "source".into(),
            published_output: None,
        };
        let completion = Completion {
            attempt: 1,
            current_source_revision: Some("source".into()),
            validated_output: Some("output".into()),
        };
        let facts = PreparationFacts {
            requester_authorized: true,
            source_enabled: true,
            source_in_scope: true,
            exact_whole_timeline_binding: true,
            recipe_unchanged: true,
        };
        let mut tx = db.begin().await.unwrap();
        let original =
            reserve_command_in(&mut tx, &plan, command.clone(), state, completion, facts)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        host.prepare_selection("advance-generation",&json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE 0"}})).unwrap();
        // The real native stale decision precedes payload parsing/effects; this
        // deliberately incomplete output cannot become a publication grant.
        let (outcome, witness) = host
            .execute_command_observed(
                &command,
                TrustedCommandAuthority {
                    host_namespace: "motion",
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: true,
                    identity_authority: None,
                },
                ReceiptPolicy {
                    retention: std::time::Duration::from_secs(60),
                    max_receipts: 100,
                    max_logical_bytes: 1024 * 1024,
                    max_hold_records: 100,
                },
            )
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::StaleGeneration));
        let mut tx = db.begin().await.unwrap();
        let next = record_next_in(&mut tx, &original, witness.unwrap())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(next.command, original.command);
        assert_eq!(next.reservation, original.reservation);
        assert_ne!(next.attempt.key(), next.command.key());
        assert_eq!(next.attempt.payload(), next.command.payload());
        drop(next);
        host.close().unwrap();
        let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
            "motion".into(),
            "test-restart".into(),
        )
        .unwrap();
        let mut host = catabolic_runtime::host::HostSession::open_authorized(
            &path,
            "default",
            true,
            catabolic_runtime::host::CONTRACT_VERSION,
            gate,
        )
        .unwrap();
        let restored = load_in(&mut db, &plan.operation_id).await.unwrap();
        assert_eq!(restored.command, command);
        assert!(reconcile_only(&mut host, &restored).unwrap().is_none());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM catabolic_generated_publications WHERE receipt_json IS NOT NULL").fetch_one(&mut db).await.unwrap(),0);
        host.close().unwrap();
    }

    #[tokio::test]
    async fn durable_reservation_replays_exact_receipt_but_absence_cannot_acknowledge() {
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0087_catabolic_generated_publications.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        sqlx::raw_sql("CREATE TABLE catabolic_commands(operation_id TEXT PRIMARY KEY,envelope_json TEXT NOT NULL,created_at INTEGER NOT NULL); CREATE TABLE catabolic_discovery_intents(operation_id TEXT PRIMARY KEY,command_json TEXT);").execute(&mut db).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0085_catabolic_command_attempts.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        let plan = PublicationPlan {
            operation_id: "operation".into(),
            job_id: "job".into(),
            attempt: 1,
            output_relative: "output.mp4".into(),
            source: json!({"ids":{"file":"output"}}),
        };
        super::super::plan_in(&mut db, &plan).await.unwrap();
        retain_in(
            &mut db,
            "operation",
            PublicationRecord::Output,
            &json!({"validated":"output"}),
        )
        .await
        .unwrap();
        let request = json!({"job_id":"job","attempt":1,"preparation_digest":"a".repeat(64),
            "output":{"source_occurrence_id":"output"},"output_ctime_ns":2,"output_size":3,
            "output_sha256":"b".repeat(64),"output_duration_us":100,"provenance":{"recipe":"remux_mp4"}});
        let command = CommandEnvelope::new(
            1,
            catabolic_core::catalog_command::CommandKey::new("catalog", "motion", "operation")
                .unwrap(),
            "incarnation",
            1,
            &json!({"operation":"generated_output_admit","request":request}).to_string(),
        )
        .unwrap();
        let before = State {
            job: playscale_core::jobs::Job {
                phase: playscale_core::jobs::Phase::Running,
                attempt: 1,
            },
            source_revision: "source".into(),
            published_output: None,
        };
        let completion = Completion {
            attempt: 1,
            current_source_revision: Some("source".into()),
            validated_output: Some("output".into()),
        };
        let facts = PreparationFacts {
            requester_authorized: true,
            source_enabled: true,
            source_in_scope: true,
            exact_whole_timeline_binding: true,
            recipe_unchanged: true,
        };
        let mut tx = db.begin().await.unwrap();
        reserve_command_in(&mut tx, &plan, command, before.clone(), completion, facts)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let restored = load_in(&mut db, "operation").await.unwrap();
        assert_eq!(
            restored.reservation.recovery().unwrap(),
            playscale_core::processing_publication::RecoveryAction::ReconcileOriginalReceipt
        );
        assert!(
            record_receipt_in(
                &mut db,
                &restored,
                &CommandOutcome::OutcomeUnknownAfterRestore
            )
            .await
            .is_err()
        );
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM catabolic_generated_publications WHERE receipt_json IS NOT NULL").fetch_one(&mut db).await.unwrap(),0);
        let mut result = request.clone();
        result["operation"] = json!("generated_output_admit");
        let outcome = CommandOutcome::Committed {
            result: result.clone(),
            committed_generation: 2,
            retain_until_us: 1000,
        };
        let recorded = record_receipt_in(&mut db, &restored, &outcome)
            .await
            .unwrap();
        assert_eq!(
            recorded
                .acknowledge(&before)
                .unwrap()
                .1
                .published_output
                .as_deref(),
            Some("output")
        );
        let replay = CommandOutcome::Replay {
            result: result.clone(),
            committed_generation: 2,
            retain_until_us: 1000,
        };
        assert_eq!(
            record_receipt_in(&mut db, &restored, &replay)
                .await
                .unwrap(),
            recorded
        );
        result["output_sha256"] = json!("c".repeat(64));
        assert!(
            record_receipt_in(
                &mut db,
                &restored,
                &CommandOutcome::Replay {
                    result,
                    committed_generation: 2,
                    retain_until_us: 1000
                }
            )
            .await
            .is_err()
        );
    }
}
