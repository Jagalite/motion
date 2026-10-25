//! Durable host/native handoff. Never reopens legacy admission on an uncertain
//! receipt and never creates activation authority from request-provided JSON.
use crate::{HOST_NAMESPACE, NativeCatalog, decode_envelope, receipt_policy, status};
use anyhow::{Context, bail, ensure};
use catabolic_core::catalog_command::ReceiptVisibility;
use catabolic_runtime::host::{
    HostSession, IMPORT_RESTART_CONTRACT_VERSION, ImportRestartAdmission, RecordedImportHandshake,
};
use catabolic_store::{
    Store,
    catalog_commands::{self, CatalogAuthority, CommandOutcome, TrustedCommandAuthority},
    catalog_import::{
        self as native, ActivationBinding, HostCutoverCheckpoint, HostCutoverPhase,
        ImportAuthority, ImportMutation, ImportReceipt, ImportStep, Reconciliation,
        TrustedImportActivation,
    },
};
use playscale_core::{
    catalog_backend::Identity,
    catalog_cutover::{Checkpoint, Phase},
};
use serde_json::json;
use sqlx::SqlitePool;
use std::{path::PathBuf, time::Duration};

pub struct ServerAdmission {
    pub admission: ImportRestartAdmission,
    pub identity: Identity,
}
/// Compare the actual retained owner, not a compiled latest schema number.
/// Existing bindings do not authorize silent schema upgrades on restart.
pub(crate) fn verify_admitted_identity(
    host: &HostSession,
    expected: &Identity,
) -> anyhow::Result<()> {
    let actual = host.identity()?;
    let observed = Identity {
        catalog_id: actual["catalog_id"]
            .as_str()
            .context("native catalog identity missing")?
            .to_owned(),
        incarnation: actual["catalog_incarnation"]
            .as_str()
            .context("native incarnation missing")?
            .to_owned(),
        schema_version: actual["schema_version"]
            .as_u64()
            .context("native schema missing")?
            .try_into()?,
    };
    expected
        .admit(&observed)
        .map_err(|e| anyhow::anyhow!("admitted native binding mismatch: {e:?}"))
}
pub(crate) fn native_checkpoint(saved: &Checkpoint) -> HostCutoverCheckpoint {
    HostCutoverCheckpoint {
        catalog_id: saved.catalog_id.clone(),
        incarnation: saved.incarnation.clone(),
        phase: match saved.phase {
            Phase::Validated => HostCutoverPhase::Validated,
            Phase::Activated => HostCutoverPhase::Activated,
        },
        import_id: saved.import_id.clone(),
        input_digest: saved.input_digest.clone(),
        reference_ack_id: saved.reference_ack_id.clone(),
        mapping_digest: saved.mapping_digest.clone(),
    }
}
pub(crate) async fn recorded(pool: &SqlitePool) -> anyhow::Result<(Checkpoint, String)> {
    let (import_id, text, envelope, phase):(String,String,String,String)=sqlx::query_as("SELECT import_id,checkpoint_json,activation_envelope,phase FROM catabolic_cutover WHERE singleton=1").fetch_optional(pool).await?.context("no durable Catabolic cutover checkpoint")?;
    let checkpoint: Checkpoint = serde_json::from_str(&text)?;
    ensure!(
        checkpoint.valid()
            && checkpoint.import_id == import_id
            && phase
                == match checkpoint.phase {
                    Phase::Validated => "validated",
                    Phase::Activated => "activated",
                },
        "invalid durable cutover checkpoint"
    );
    Ok((checkpoint, envelope))
}
/// Receipt-only startup. The returned object retains the native writer session
/// used to verify the actual committed receipt through subsequent admissions.
pub async fn open_server(
    journal: SqlitePool,
    native_path: PathBuf,
    profile: String,
) -> anyhow::Result<ServerAdmission> {
    let has_set: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_cutover_sets WHERE singleton=1)")
            .fetch_one(&journal)
            .await?;
    if has_set {
        return crate::partition_cutover::open_server(journal, native_path, profile).await;
    }
    let (checkpoint, envelope) = recorded(&journal).await?;
    ensure!(
        checkpoint.phase == Phase::Activated,
        "host activation is not acknowledged; use operator reconciliation"
    );
    let state = crate::source_freeze::admission(&journal)
        .await?
        .context("missing source freeze")?;
    ensure!(
        matches!(
            state.mode,
            playscale_core::catalog_source::Mode::Sealed { .. }
        ) && state.active_freeze()
            == Some((checkpoint.import_id.as_str(), checkpoint.freeze_revision)),
        "cutover source freeze mismatch"
    );
    let persisted: String =
        sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
            .fetch_one(&journal)
            .await?;
    let identity: Identity = serde_json::from_str(&persisted)?;
    identity
        .validate()
        .map_err(|e| anyhow::anyhow!("cutover binding invalid: {e:?}"))?;
    ensure!(
        identity.catalog_id == checkpoint.catalog_id
            && identity.incarnation == checkpoint.incarnation,
        "cutover binding differs from retained checkpoint"
    );
    let mut tx = journal.begin().await?;
    crate::source_freeze::verify(&mut tx).await?;
    tx.commit().await?;
    let identity = crate::native_upgrade::effective_identity(&journal, &identity).await?;
    tokio::task::spawn_blocking(move || {
        let handshake = RecordedImportHandshake {
            activation_intent: decode_envelope(&envelope)?,
            checkpoint: native_checkpoint(&checkpoint),
        };
        let mut admission = HostSession::open_after_import_cutover(
            &native_path,
            &profile,
            IMPORT_RESTART_CONTRACT_VERSION,
            &handshake,
            TrustedCommandAuthority {
                host_namespace: HOST_NAMESPACE,
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: false,
                identity_authority: None,
            },
            uuid::Uuid::new_v4().to_string(),
            receipt_policy(),
        )?;
        ensure!(
            admission.decision() == Reconciliation::AdmitCatabolicOnly,
            "native activation receipt does not admit server"
        );
        verify_admitted_identity(admission.admitted_host()?, &identity)?;
        Ok(ServerAdmission {
            admission,
            identity,
        })
    })
    .await?
}

impl NativeCatalog {
    /// Persist the exact native activation intent only after the frozen export
    /// passes the native typed owner and the host reference inventory is complete.
    pub async fn prepare_motion_cutover(&self, import_id: &str) -> anyhow::Result<Checkpoint> {
        ensure!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM catabolic_cutover_sets")
                .fetch_one(&self.journal)
                .await?
                == 0,
            "partition cutover already owns host"
        );
        ensure!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM catabolic_export_plans WHERE root_import_id=?"
            )
            .bind(import_id)
            .fetch_one(&self.journal)
            .await?
                == 0,
            "partition export requires aggregate cutover"
        );
        let state = self.require_frozen_export(import_id).await?;
        ensure!(
            state.active_freeze().is_some_and(|(id, _)| id == import_id),
            "partitioned exports require aggregate cutover; one partition cannot activate the server"
        );
        let (_, freeze_revision) = state.active_freeze().context("missing freeze")?;
        let (snapshot, input_digest) = self.import_snapshot(import_id).await?;
        let inventory = self.motion_reference_inventory(import_id).await?;
        ensure!(
            inventory.activation_ready,
            "host reference inventory has unresolved obligations"
        );
        // Native validation independently refuses any unresolved source evidence.
        // The inventory is retained in the acknowledgment digest; neither caller
        // supplies a receipt or a boolean asserting that references are mapped.
        let validation = self.validate_motion_import(import_id).await?;
        let ImportReceipt::ValidatedStaging {
            validation_generation,
            unresolved_evidence: 0,
            activation_supported: true,
            mapping_digest,
            ..
        } = validation
        else {
            bail!("native mapping/reference obligations remain unresolved")
        };
        let reference_ack_id = format!(
            "motion-ref-{}",
            catabolic_core::sha256(
                catabolic_core::encode_with(&serde_json::to_value(&inventory)?, false, true)
                    .as_bytes()
            )
        );
        let checkpoint = Checkpoint {
            catalog_id: self.identity.catalog_id.clone(),
            incarnation: self.identity.incarnation.clone(),
            import_id: import_id.into(),
            input_digest: input_digest.clone(),
            mapping_digest: mapping_digest.clone(),
            reference_ack_id: reference_ack_id.clone(),
            freeze_revision,
            validation_generation,
            phase: Phase::Validated,
        };
        ensure!(checkpoint.valid(), "invalid cutover binding");
        let operation = native::operation_id(import_id, ImportStep::Activate)?;
        self.prepare_command(&operation,json!({"operation":"import","mutation":ImportMutation::Activate{import_id:import_id.into(),input_digest,reference_ack_id,mapping_digest}})).await?;
        let envelope = self
            .recorded(&operation)
            .await?
            .context("missing activation intent")?;
        ensure!(
            envelope.expected_generation() == validation_generation,
            "catalog changed since validation"
        );
        let encoded = serde_json::to_string(&checkpoint)?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let current = crate::source_freeze::read(&mut tx).await?;
        ensure!(
            current == state,
            "source freeze changed during cutover preparation"
        );
        crate::source_freeze::verify(&mut tx).await?;
        sqlx::query("INSERT INTO catabolic_cutover VALUES(1,?,?,?,'validated') ON CONFLICT(singleton) DO NOTHING").bind(import_id).bind(&encoded).bind(envelope.canonical_payload()).execute(&mut *tx).await?;
        let saved: (String, String) = sqlx::query_as(
            "SELECT checkpoint_json,activation_envelope FROM catabolic_cutover WHERE singleton=1",
        )
        .fetch_one(&mut *tx)
        .await?;
        ensure!(
            saved == (encoded, envelope.canonical_payload().into()),
            "another cutover already owns this host"
        );
        tx.commit().await?;
        // Retain exact source namespace in snapshot rather than reconstructing it.
        ensure!(
            !snapshot.manifest.source_namespace.is_empty(),
            "missing source namespace"
        );
        Ok(checkpoint)
    }

    /// Seal legacy ownership, close and drain the exact admitted gate, activate,
    /// then acknowledge only the native owner's actual matching durable receipt.
    pub async fn activate_motion_cutover(&self, timeout: Duration) -> anyhow::Result<Checkpoint> {
        let (checkpoint, receipt) = self.commit_motion_cutover(timeout).await?;
        self.acknowledge_cutover(checkpoint, receipt).await
    }

    async fn commit_motion_cutover(
        &self,
        timeout: Duration,
    ) -> anyhow::Result<(Checkpoint, ImportReceipt)> {
        let (checkpoint, encoded) = recorded(&self.journal).await?;
        ensure!(
            checkpoint.phase == Phase::Validated,
            "cutover already activated; reopen through receipt reconciliation"
        );
        let (snapshot, digest) = self.import_snapshot(&checkpoint.import_id).await?;
        ensure!(
            digest == checkpoint.input_digest,
            "cutover snapshot changed"
        );
        self.require_frozen_export(&checkpoint.import_id).await?;
        let typed = self.motion_import_authority(&checkpoint.import_id).await?;
        let envelope = decode_envelope(&encoded)?;
        self.validate_recorded_key(
            &native::operation_id(&checkpoint.import_id, ImportStep::Activate)?,
            &envelope,
        )?;
        self.seal_motion_freeze(checkpoint.freeze_revision).await?;
        let witness = self.drain_witness(timeout).await?;
        let grant = TrustedImportActivation::after_drain(
            &self.authorization,
            witness,
            ActivationBinding {
                catalog_id: checkpoint.catalog_id.clone(),
                incarnation: checkpoint.incarnation.clone(),
                import_id: checkpoint.import_id.clone(),
                input_digest: checkpoint.input_digest.clone(),
                source_snapshot_id: snapshot.manifest.source_snapshot_id.clone(),
                validation_generation: checkpoint.validation_generation,
                reference_ack_id: checkpoint.reference_ack_id.clone(),
                mapping_digest: checkpoint.mapping_digest.clone(),
            },
        )?;
        let path = self.path.clone();
        let identity = self.identity.clone();
        let imports = ImportAuthority {
            host_namespace: HOST_NAMESPACE.into(),
            import_id: checkpoint.import_id.clone(),
            source_namespace: snapshot.manifest.source_namespace,
        };
        let receipt: ImportReceipt =
            tokio::task::spawn_blocking(move || -> anyhow::Result<ImportReceipt> {
                let store = Store::open(&path, true, false)?;
                identity
                    .admit(&status(&store)?.identity)
                    .map_err(|e| anyhow::anyhow!("catalog changed: {e:?}"))?;
                let authority = CatalogAuthority::new(TrustedCommandAuthority {
                    host_namespace: HOST_NAMESPACE,
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: true,
                    identity_authority: None,
                })
                .with_imports(&imports)
                .with_import_typed(&typed)
                .with_import_activation(&grant);
                match catalog_commands::execute_with_authority(
                    &store,
                    &envelope,
                    authority,
                    receipt_policy(),
                )? {
                    CommandOutcome::Committed { result, .. }
                    | CommandOutcome::Replay { result, .. } => Ok(serde_json::from_value(result)?),
                    other => bail!("activation requires reconciliation: {}", other.code()),
                }
            })
            .await??;
        Ok((checkpoint, receipt))
    }

    /// Recovery after native COMMIT but before the host acknowledgment. This
    /// only reads the original receipt; an absent intent is never executed.
    pub async fn reconcile_motion_cutover(&self) -> anyhow::Result<Checkpoint> {
        let (checkpoint, encoded) = recorded(&self.journal).await?;
        let state = self.require_frozen_export(&checkpoint.import_id).await?;
        ensure!(
            matches!(
                state.mode,
                playscale_core::catalog_source::Mode::Sealed { .. }
            ) && state.active_freeze()
                == Some((checkpoint.import_id.as_str(), checkpoint.freeze_revision)),
            "cutover source seal mismatch"
        );
        let path = self.path.clone();
        let expected = self.identity.clone();
        let saved = checkpoint.clone();
        let receipt: ImportReceipt =
            tokio::task::spawn_blocking(move || -> anyhow::Result<ImportReceipt> {
                let store = Store::open(&path, true, false)?;
                expected
                    .admit(&status(&store)?.identity)
                    .map_err(|e| anyhow::anyhow!("restored/replaced catalog: {e:?}"))?;
                let envelope = decode_envelope(&encoded)?;
                let authority = TrustedCommandAuthority {
                    host_namespace: HOST_NAMESPACE,
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: false,
                    identity_authority: None,
                };
                let decision = native::reconcile_persisted_activation(
                    &store,
                    &envelope,
                    &native_checkpoint(&saved),
                    authority,
                    receipt_policy(),
                )?;
                ensure!(
                    decision != Reconciliation::KeepAdmissionClosed,
                    "activation receipt missing or changed; admission remains closed"
                );
                match catalog_commands::execute(
                    &store,
                    &envelope,
                    TrustedCommandAuthority {
                        host_namespace: HOST_NAMESPACE,
                        receipt_visibility: ReceiptVisibility::FullResult,
                        allow_new_commands: false,
                        identity_authority: None,
                    },
                    receipt_policy(),
                )? {
                    CommandOutcome::Replay { result, .. } => Ok(serde_json::from_value(result)?),
                    _ => bail!("activation receipt unavailable"),
                }
            })
            .await??;
        if checkpoint.phase == Phase::Activated {
            return Ok(checkpoint);
        }
        self.acknowledge_cutover(checkpoint, receipt).await
    }

    async fn acknowledge_cutover(
        &self,
        checkpoint: Checkpoint,
        receipt: ImportReceipt,
    ) -> anyhow::Result<Checkpoint> {
        ensure!(
            native::reconcile_activation(
                Some(&receipt),
                &native_checkpoint(&checkpoint),
                &self.identity.catalog_id,
                &self.identity.incarnation
            ) == Reconciliation::FinishHostActivation,
            "activation receipt mismatch"
        );
        let mut observed = checkpoint.clone();
        observed.phase = Phase::Activated;
        let activated = checkpoint
            .acknowledge(&observed)
            .map_err(anyhow::Error::msg)?;
        let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let current = crate::source_freeze::read(&mut tx).await?;
        ensure!(
            matches!(
                current.mode,
                playscale_core::catalog_source::Mode::Sealed { .. }
            ) && current.active_freeze()
                == Some((checkpoint.import_id.as_str(), checkpoint.freeze_revision)),
            "source seal changed"
        );
        crate::source_freeze::verify(&mut tx).await?;
        crate::revision_bridge::acknowledge_import_in(&mut tx, &receipt).await?;
        let changed=sqlx::query("UPDATE catabolic_cutover SET checkpoint_json=?,phase='activated' WHERE singleton=1 AND checkpoint_json=? AND phase='validated'").bind(serde_json::to_string(&activated)?).bind(serde_json::to_string(&checkpoint)?).execute(&mut *tx).await?;
        ensure!(changed.rows_affected() == 1, "host checkpoint changed");
        tx.commit().await?;
        Ok(activated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lost_native_ack_is_receipt_only_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let path = base.join("native.sqlite3");
        crate::initialize(path.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(crate::projection::configure_connection(connection))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let backend = NativeCatalog::bind(
            db.clone(),
            path.clone(),
            crate::inspect(path.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        backend.freeze_motion_source("lost-ack", 0).await.unwrap();
        backend.capture_motion_import("lost-ack").await.unwrap();
        backend.stage_import("lost-ack").await.unwrap();
        backend.prepare_motion_cutover("lost-ack").await.unwrap();
        let (_checkpoint, _lost_receipt) = backend
            .commit_motion_cutover(Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(recorded(&db).await.unwrap().0.phase, Phase::Validated);
        assert!(
            open_server(db.clone(), path.clone(), "default".into())
                .await
                .is_err()
        );
        let active = backend.reconcile_motion_cutover().await.unwrap();
        assert_eq!(active.phase, Phase::Activated);
        drop(backend);
        let mut server = open_server(db.clone(), path, "default".into())
            .await
            .unwrap();
        server.admission.admitted_host().unwrap().close().unwrap();
    }
    #[tokio::test]
    async fn absent_activation_is_not_executed_by_reconciliation() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let path = base.join("native.sqlite3");
        crate::initialize(path.clone()).await.unwrap();
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(crate::projection::configure_connection(connection))
            })
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(base.join("motion.sqlite3"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations").run(&db).await.unwrap();
        let backend = NativeCatalog::bind(
            db.clone(),
            path.clone(),
            crate::inspect(path.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        backend.freeze_motion_source("absent", 0).await.unwrap();
        backend.capture_motion_import("absent").await.unwrap();
        backend.stage_import("absent").await.unwrap();
        backend.prepare_motion_cutover("absent").await.unwrap();
        backend.seal_motion_freeze(1).await.unwrap();
        let before = backend.status().await.unwrap().generation;
        assert!(backend.reconcile_motion_cutover().await.is_err());
        assert_eq!(backend.status().await.unwrap().generation, before);
        assert_eq!(recorded(&db).await.unwrap().0.phase, Phase::Validated);
    }
    #[test]
    fn restart_identity_uses_retained_schema_and_rejects_changed_binding() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        catabolic_store::migration::initialize_native(&path).unwrap();
        let gate = catabolic_core::catalog_host_lease::HostAuthorization::new(
            "motion-test".into(),
            "revision-1".into(),
        )
        .unwrap();
        let host = HostSession::open_authorized(
            &path,
            "default",
            true,
            catabolic_runtime::host::CONTRACT_VERSION,
            gate,
        )
        .unwrap();
        let actual = host.identity().unwrap();
        let identity = Identity {
            catalog_id: actual["catalog_id"].as_str().unwrap().into(),
            incarnation: actual["catalog_incarnation"].as_str().unwrap().into(),
            schema_version: actual["schema_version"]
                .as_u64()
                .unwrap()
                .try_into()
                .unwrap(),
        };
        verify_admitted_identity(&host, &identity).unwrap();
        let mut wrong = identity.clone();
        wrong.schema_version += 1;
        assert!(verify_admitted_identity(&host, &wrong).is_err());
        wrong = identity.clone();
        wrong.incarnation.push_str("-restored");
        assert!(verify_admitted_identity(&host, &wrong).is_err());
        wrong = identity;
        wrong.catalog_id.push_str("-other");
        assert!(verify_admitted_identity(&host, &wrong).is_err());
    }
}
