//! Receipt-only restart reconciliation. Never dispatch a pending edition write
//! on startup and never reconstruct its baseline from the current native head.
use super::*;
impl Runtime {
    pub(crate) async fn reconcile_empty_editions(&self) -> anyhow::Result<()> {
        let identity = self.identity().await?;
        let mut after = String::new();
        loop {
            let rows:Vec<(String,String)>=sqlx::query_as("SELECT operation_id,envelope_json FROM catabolic_commands WHERE operation_id>? ORDER BY operation_id LIMIT 128").bind(&after).fetch_all(&self.journal).await?;
            if rows.is_empty() {
                break;
            }
            after = rows
                .last()
                .context("edition receipt page missing")?
                .0
                .clone();
            for (operation, saved) in rows {
                let value: serde_json::Value = serde_json::from_str(&saved)?;
                if value["payload"]["operation"] != "empty_edition_create" {
                    continue;
                }
                let original = motion_catalog_backend::command_attempts::decode(&saved)?;
                anyhow::ensure!(
                    original.key().operation_id() == operation,
                    "edition intent key changed"
                );
                let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
                    self.journal.begin_with("BEGIN IMMEDIATE").await?;
                let (_, command) =
                    motion_catalog_backend::command_attempts::latest_in(&mut tx, &original).await?;
                anyhow::ensure!(
                    identity["catalog_id"] == command.key().catalog_id()
                        && identity["catalog_incarnation"] == command.admission_incarnation(),
                    "edition intent belongs to another admitted catalog"
                );
                let request: catabolic_store::catalog_edition_creation::EmptyEditionRequest =
                    serde_json::from_value(command.payload()["request"].clone())?;
                let id = format!("motion.edition:{}", request.edition_id);
                let prior:Option<(String,String,i64,i64,String)>=sqlx::query_as("SELECT catalog_id,incarnation,motion_base,native_base,proof_json FROM catabolic_revision_bridges WHERE native_id=?").bind(&id).fetch_optional(&mut *tx).await?;
                if let Some((catalog, incarnation, motion, native, proof)) = prior {
                    let proof: serde_json::Value = serde_json::from_str(&proof)?;
                    anyhow::ensure!(
                        catalog == command.key().catalog_id()
                            && incarnation == command.admission_incarnation()
                            && motion == 1
                            && native > 0
                            && proof["kind"] == "native_empty_edition"
                            && proof["intent"] == command.canonical_payload()
                            && proof["result"]["item_revision_baselines"][&id] == native,
                        "edition acknowledgment identity changed"
                    );
                    tx.commit().await?;
                    continue;
                }
                let handle = tokio::runtime::Handle::current();
                self.with_host(move |host| {
                    if let Some(outcome) = host.reconcile_command_receipt(&command)? {
                        handle.block_on(
                            motion_catalog_backend::revision_bridge::acknowledge_empty_edition_in(
                                &mut tx, &command, &outcome,
                            ),
                        )?;
                    }
                    handle.block_on(tx.commit())?;
                    Ok(())
                })
                .await?;
            }
        }
        Ok(())
    }
}
