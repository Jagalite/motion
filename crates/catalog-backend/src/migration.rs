//! Durable migration staging. Native owners validate batches, identities and
//! receipts. This module never activates staged data or mints an activation grant.
use super::*;
pub use catabolic_store::catalog_import::{ImportBatch, SnapshotManifest};
use catabolic_store::{
    catalog_commands::CatalogAuthority,
    catalog_import::{self as native, ImportAuthority, ImportMutation, ImportReceipt, ImportStep},
    catalog_import_typed::TypedAuthority,
};

/// One immutable source snapshot. This is an input contract, not evidence that
/// Motion writers have been frozen or that application references were audited.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub manifest: SnapshotManifest,
    pub batches: Vec<ImportBatch>,
}
impl Snapshot {
    pub fn validate(&self) -> anyhow::Result<String> {
        let digest = native::manifest_digest(&self.manifest)?;
        anyhow::ensure!(
            self.batches.len() == self.manifest.batches.len(),
            "import batch count mismatch"
        );
        for (batch, descriptor) in self.batches.iter().zip(&self.manifest.batches) {
            anyhow::ensure!(
                &native::describe_batch(batch)? == descriptor,
                "import batch descriptor mismatch"
            );
        }
        Ok(digest)
    }
}

impl NativeCatalog {
    /// Commit all source bytes before native work. An import identity can never
    /// be rebound to a newer/different snapshot, even if no batch was dispatched.
    pub async fn prepare_import(&self, snapshot: Snapshot) -> anyhow::Result<()> {
        let digest = snapshot.validate()?;
        self.status().await?;
        let mut tx = self.journal.begin().await?;
        save_snapshot(&mut tx, &snapshot, &digest).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(crate) async fn import_snapshot(
        &self,
        import_id: &str,
    ) -> anyhow::Result<(Snapshot, String)> {
        let (digest, text): (String, String) = sqlx::query_as(
            "SELECT input_digest,snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",
        )
        .bind(import_id)
        .fetch_optional(&self.journal)
        .await?
        .context("unknown import snapshot")?;
        let snapshot: Snapshot = serde_json::from_str(&text)?;
        anyhow::ensure!(
            snapshot.manifest.import_id == import_id && snapshot.validate()? == digest,
            "corrupt import snapshot"
        );
        Ok((snapshot, digest))
    }

    /// Stage from the retained snapshot, including after restart. Returning does
    /// not make any staged item visible to ordinary catalog reads or search.
    pub async fn stage_import(&self, import_id: &str) -> anyhow::Result<Vec<ImportReceipt>> {
        let (snapshot, digest) = self.import_snapshot(import_id).await?;
        let mut receipts = vec![
            self.import_step(
                &snapshot.manifest,
                ImportStep::Prepare,
                ImportMutation::Prepare {
                    manifest: snapshot.manifest.clone(),
                    input_digest: digest.clone(),
                },
                None,
            )
            .await?,
        ];
        for (index, batch) in snapshot.batches.into_iter().enumerate() {
            receipts.push(
                self.import_step(
                    &snapshot.manifest,
                    ImportStep::Batch(index as u32),
                    ImportMutation::Stage {
                        import_id: import_id.into(),
                        input_digest: digest.clone(),
                        batch_index: index as u32,
                        batch,
                    },
                    None,
                )
                .await?,
            );
        }
        Ok(receipts)
    }

    /// Typed owner grants must come from trusted application migration code;
    /// they are not deserialized from the snapshot or reconstructed from its claims.
    pub async fn validate_import(
        &self,
        import_id: &str,
        typed: Option<TypedAuthority>,
    ) -> anyhow::Result<ImportReceipt> {
        let (snapshot, input_digest) = self.import_snapshot(import_id).await?;
        self.import_step(
            &snapshot.manifest,
            ImportStep::Validate,
            ImportMutation::Validate {
                import_id: import_id.into(),
                input_digest,
            },
            typed,
        )
        .await
    }

    async fn import_step(
        &self,
        manifest: &SnapshotManifest,
        step: ImportStep,
        mutation: ImportMutation,
        typed: Option<TypedAuthority>,
    ) -> anyhow::Result<ImportReceipt> {
        let operation_id = native::operation_id(&manifest.import_id, step)?;
        self.prepare_command(
            &operation_id,
            json!({"operation":"import","mutation":mutation}),
        )
        .await?;
        let envelope = self
            .recorded(&operation_id)
            .await?
            .context("missing durable import command")?;
        self.validate_recorded_key(&operation_id, &envelope)?;
        let grant = ImportAuthority {
            host_namespace: HOST_NAMESPACE.into(),
            import_id: manifest.import_id.clone(),
            source_namespace: manifest.source_namespace.clone(),
        };
        self.with_store(true, move |store| {
            let mut authority = CatalogAuthority::new(TrustedCommandAuthority {
                host_namespace: HOST_NAMESPACE,
                receipt_visibility: ReceiptVisibility::FullResult,
                allow_new_commands: true,
                identity_authority: None,
            })
            .with_imports(&grant);
            if let Some(typed) = typed.as_ref() {
                authority = authority.with_import_typed(typed);
            }
            let outcome = catalog_commands::execute_with_authority(
                store,
                &envelope,
                authority,
                receipt_policy(),
            )?;
            match outcome {
                CommandOutcome::Committed { result, .. }
                | CommandOutcome::Replay { result, .. } => Ok(serde_json::from_value(result)?),
                other => bail!("import command requires reconciliation: {}", other.code()),
            }
        })
        .await
    }
}

/// Caller commits this with any source-provenance marker in the same transaction.
pub(crate) async fn save_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    snapshot: &Snapshot,
    digest: &str,
) -> anyhow::Result<()> {
    let encoded = catabolic_core::encode_with(&serde_json::to_value(snapshot)?, false, true);
    anyhow::ensure!(
        encoded.len() <= 2 * 1024 * 1024,
        "import snapshot exceeds host byte bound"
    );
    sqlx::query("INSERT INTO catabolic_import_snapshots(import_id,input_digest,snapshot_json,created_at) VALUES(?,?,?,?) ON CONFLICT(import_id) DO NOTHING")
            .bind(&snapshot.manifest.import_id).bind(digest).bind(&encoded).bind(now()).execute(&mut **tx).await?;
    let saved: (String, String) = sqlx::query_as(
        "SELECT input_digest,snapshot_json FROM catabolic_import_snapshots WHERE import_id=?",
    )
    .bind(&snapshot.manifest.import_id)
    .fetch_one(&mut **tx)
    .await?;
    anyhow::ensure!(
        saved == (digest.to_owned(), encoded),
        "import ID already belongs to a different snapshot"
    );
    Ok(())
}
