//! Exact library replacement intents. Native metadata owns the definition;
//! the Motion writer owns current authorization and immutable scope ACKs.
use super::*;
use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
use catabolic_store::catalog_commands::CommandOutcome;
use motion_catalog_backend::{command_attempts, sources::SourceMapping};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) fn update_parts(
    command: &CommandEnvelope,
) -> anyhow::Result<(String, u64, Value, Vec<SourceMapping>)> {
    let payload = command.payload();
    let change = match payload["operation"].as_str() {
        Some("metadata") => &payload["mutation"]["change"],
        Some("metadata_initialize_and_patch") => payload,
        _ => anyhow::bail!("not a library replacement intent"),
    };
    let evidence: Value = serde_json::from_str(
        change["evidence_json"]
            .as_str()
            .context("library evidence missing")?,
    )?;
    let edits: Value = serde_json::from_str(
        change["edits_json"]
            .as_str()
            .context("library edits missing")?,
    )?;
    let definition = edits["motion_library"]["value"].clone();
    let id = definition["id"]
        .as_str()
        .context("library id missing")?
        .to_owned();
    let expected = evidence["expected_library_revision"]
        .as_u64()
        .context("library CAS missing")?;
    let mappings: Vec<SourceMapping> = serde_json::from_value(evidence["source_mappings"].clone())?;
    let source_ids: Vec<String> = serde_json::from_value(definition["source_ids"].clone())?;
    let actual: std::collections::BTreeSet<_> =
        mappings.iter().map(|m| m.library_id.clone()).collect();
    anyhow::ensure!(
        evidence["owner"] == "motion.library.replace"
            && change["subject"] == format!("motion.library:{id}")
            && change["namespace"] == "motion.local"
            && change["record_id"] == "document"
            && definition["contract_version"] == 1
            && expected.checked_add(1).is_some()
            && definition["catalog_revision"].as_u64() == expected.checked_add(1)
            && definition["replacement_operation"]
                .as_str()
                .is_some_and(|v| v.starts_with("library-replace:"))
            && edits.as_object().is_some_and(|e| e.len() == 2)
            && edits["title"]["op"] == "set"
            && edits["title"]["value"] == definition["name"]
            && edits["motion_library"]["op"] == "set"
            && actual.len() == mappings.len()
            && source_ids.len() == actual.len()
            && source_ids
                .iter()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                == actual,
        "library replacement proof mismatch"
    );
    Ok((id, expected, definition, mappings))
}
impl Runtime {
    pub(crate) async fn prepare_library_update(
        &self,
        operation: String,
        definition: Value,
        expected: u64,
        digest: String,
        mappings: Vec<SourceMapping>,
    ) -> anyhow::Result<CommandEnvelope> {
        let subject = format!(
            "motion.library:{}",
            definition["id"].as_str().context("library id missing")?
        );
        let (head, namespace) = self
            .with_host(move |host| {
                Ok((
                    host.metadata_source_head(&subject, "motion.local", "document")?,
                    host.authorization()?.namespace().to_owned(),
                ))
            })
            .await?;
        let edits = json!({"title":{"op":"set","value":definition["name"]},"motion_library":{"op":"set","value":definition}}).to_string();
        let evidence = json!({"owner":"motion.library.replace","request_digest":digest,"expected_library_revision":expected,"source_mappings":mappings}).to_string();
        let payload = if let Some(fingerprint) = head.schema_fingerprint {
            json!({"operation":"metadata","mutation":{"contract_version":1,"change":{"action":"patch","subject":head.subject,"namespace":"motion.local","record_id":"document","schema_fingerprint":fingerprint,"expected_revision":head.source_revision,"edits_json":edits,"evidence_json":evidence}}})
        } else {
            json!({"operation":"metadata_initialize_and_patch","subject":head.subject,"policies":[{"field":"title","value_type":"string","nullable":false,"normalization":"exact"},{"field":"motion_library","value_type":"json","nullable":false,"normalization":"exact"}],"namespace":"motion.local","record_id":"document","edits_json":edits,"evidence_json":evidence})
        };
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(head.catalog_id, namespace, &operation)?,
            head.incarnation,
            head.generation,
            &payload.to_string(),
        )?;
        update_parts(&command)?;
        self.record_intent(&operation, command).await
    }

    pub(crate) fn execute_library_update_with_host(
        host: &mut HostSession,
        command: &CommandEnvelope,
    ) -> anyhow::Result<(
        CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
    )> {
        use catabolic_core::{catalog_command::ReceiptVisibility, catalog_metadata::SourceKind};
        use catabolic_store::{
            catalog_commands::{CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority},
            catalog_metadata_store::{MetadataAccess, MetadataAuthority},
        };
        let (id, _, _, mappings) = update_parts(command)?;
        let identity = host.identity()?;
        for mapping in &mappings {
            anyhow::ensure!(
                identity["catalog_id"] == mapping.catalog.catalog_id
                    && identity["catalog_incarnation"] == mapping.catalog.incarnation
                    && identity["profile"] == mapping.profile
                    && host.source_binding(&mapping.native_source)? == mapping.native_binding
                    && host.source_observation_policy(&mapping.native_source)?
                        == mapping.native_policy,
                "library source proof changed before dispatch"
            );
        }
        let namespace = host.authorization()?.namespace().to_owned();
        let subject = format!("motion.library:{id}");
        let initialize = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.clone(),
            access: MetadataAccess::Initialize,
        };
        let source = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject,
            access: MetadataAccess::SourceV2 {
                namespace: "motion.local".into(),
                record_id: "document".into(),
                kind: SourceKind::Local,
                system_fields: std::collections::BTreeSet::from(["motion_library".into()]),
            },
        };
        let grants = CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: &namespace,
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_metadata(&source)
        .with_metadata_initialization(&initialize);
        Ok(host.execute_catalog_command_observed(
            command,
            grants,
            ReceiptPolicy {
                retention: Duration::from_secs(30 * 24 * 60 * 60),
                max_receipts: 100_000,
                max_logical_bytes: 256 * 1024 * 1024,
                max_hold_records: 100_000,
            },
        )?)
    }

    /// Actual native head and complete definition under the already-held writer.
    pub(crate) async fn library_head_in(
        host: &mut HostSession,
        db: &mut sqlx::SqliteConnection,
        id: &str,
    ) -> anyhow::Result<(Value, u64)> {
        let subject = format!("motion.library:{id}");
        let identity = host.identity()?;
        let generation = host.catalog_generation()?;
        let facts = host.projection_named_items(generation, &[subject.clone()])?;
        let items = facts["items"].as_array().context("library facts missing")?;
        anyhow::ensure!(
            items.len() == 1
                && items[0]["item_id"] == subject
                && items[0]["kind"] == "custom:motion_library"
                && items[0]["retirement"].is_null(),
            "library absent or retired"
        );
        let metadata = items[0]["metadata"].clone();
        anyhow::ensure!(
            metadata["motion_library"]["contract_version"] == 1
                && metadata["motion_library"]["id"] == id,
            "library identity changed"
        );
        let revision = motion_catalog_backend::revision_bridge::wire_revision_in(
            db,
            &subject,
            identity["catalog_id"]
                .as_str()
                .context("catalog id missing")?,
            identity["catalog_incarnation"]
                .as_str()
                .context("incarnation missing")?,
            items[0]["item_revision"]
                .as_i64()
                .context("library head missing")?,
        )
        .await?;
        Ok((metadata, revision))
    }

    /// Receipt-derived completion only. Never issue a native effect here.
    pub(crate) async fn acknowledge_library_update_in(
        host: &mut HostSession,
        db: &mut sqlx::SqliteConnection,
        original: &CommandEnvelope,
        outcome: &CommandOutcome,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(
                outcome,
                CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. }
            ),
            "library replacement receipt unavailable"
        );
        let (id, expected, definition, mappings) = update_parts(original)?;
        let next = expected
            .checked_add(1)
            .context("library revision overflow")?;
        let identity = host.identity()?;
        anyhow::ensure!(
            identity["catalog_id"] == original.key().catalog_id()
                && identity["catalog_incarnation"] == original.admission_incarnation(),
            "library receipt owner changed"
        );
        let raw = serde_json::to_string(&mappings)?;
        let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let existing:Option<(String,String,String)>=sqlx::query_as("SELECT catalog_id,incarnation,source_mappings_json FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=? AND source_digest=?").bind(&self_profile(&identity)?).bind(&id).bind(i64::try_from(next)?).bind(&digest).fetch_optional(&mut *db).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=?")
            .bind(self_profile(&identity)?).bind(&id).bind(i64::try_from(next)?).fetch_one(&mut *db).await?;
        anyhow::ensure!(
            count == i64::from(existing.is_some()),
            "ambiguous library scope acknowledgment"
        );
        if let Some(existing) = existing {
            anyhow::ensure!(
                existing
                    == (
                        original.key().catalog_id().into(),
                        original.admission_incarnation().into(),
                        raw
                    ),
                "library ACK changed"
            );
            return Ok(());
        }
        let (metadata, revision) = Self::library_head_in(host, db, &id).await?;
        anyhow::ensure!(
            revision == next && metadata["motion_library"] == definition,
            "unacknowledged library result differs from exact saved intent"
        );
        for mapping in &mappings {
            let (lifecycle, current) =
                motion_catalog_backend::source_lifecycle::current_mapping_in(
                    db,
                    &mapping.library_id,
                )
                .await?;
            anyhow::ensure!(
                current == *mapping
                    && lifecycle.state == playscale_core::source_lifecycle::State::Active,
                "library ACK current source mapping changed"
            );
            anyhow::ensure!(
                host.source_binding(&mapping.native_source)? == mapping.native_binding
                    && host.source_observation_policy(&mapping.native_source)?
                        == mapping.native_policy,
                "library ACK source proof changed"
            );
        }
        let quote = |v: &str| format!("'{}'", v.replace('\'', "''"));
        let locations: Vec<_> = mappings.iter().map(|m| quote(&m.native_source)).collect();
        let predicate = if locations.is_empty() {
            "0".into()
        } else {
            format!("a.location IN ({})", locations.join(","))
        };
        let query = format!(
            "SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile={} AND {predicate} ORDER BY w.item_id",
            quote(&self_profile(&identity)?)
        );
        let name = format!(
            "{:x}",
            Sha256::digest(format!("motion-library:{id}").as_bytes())
        );
        let saved = host.prepare_selection(
            &name,
            &json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}),
        )?;
        sqlx::query("INSERT INTO catabolic_server_scopes(profile,library_id,library_revision,catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
          .bind(self_profile(&identity)?).bind(&id).bind(i64::try_from(next)?).bind(original.key().catalog_id()).bind(original.admission_incarnation()).bind(digest).bind(raw).bind(saved["id"].as_str().context("query id missing")?).bind(saved["revision"].as_i64().context("query revision missing")?).bind(saved["digest"].as_str().context("query digest missing")?).bind(crate::now()).execute(&mut *db).await?;
        // One hint per newly acknowledged native definition, including restart
        // reconciliation. The existing-ACK return above prevents replay duplicates.
        sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('libraries',?)")
            .bind(&id)
            .execute(&mut *db)
            .await?;
        Ok(())
    }

    pub(crate) async fn reconcile_library_updates(&self) -> anyhow::Result<()> {
        let mut after = String::new();
        loop {
            let mut tx = self.journal.begin_with("BEGIN IMMEDIATE").await?;
            let rows:Vec<(String,String)>=sqlx::query_as("SELECT operation_id,envelope_json FROM catabolic_commands WHERE operation_id LIKE 'library-replace:%' AND operation_id>? ORDER BY operation_id LIMIT 128").bind(&after).fetch_all(&mut *tx).await?;
            if rows.is_empty() {
                tx.commit().await?;
                break;
            }
            after = rows.last().unwrap().0.clone();
            for (_, raw) in rows {
                let original = command_attempts::decode(&raw)?;
                let (library, _, definition, _) = update_parts(&original)?;
                anyhow::ensure!(
                    definition["replacement_operation"] == original.key().operation_id(),
                    "library original operation changed"
                );
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=?)")
                    .bind(&self.profile).bind(&library).bind(definition["catalog_revision"].as_i64().context("library revision missing")?).fetch_one(&mut *tx).await?;
                if exists {
                    Self::require_library_update_ack_in(
                        &mut tx,
                        original.key().catalog_id(),
                        original.admission_incarnation(),
                        &self.profile,
                        &definition,
                    )
                    .await?;
                    continue;
                }
                let (_, sent) = command_attempts::latest_in(&mut tx, &original).await?;
                let handle = tokio::runtime::Handle::current();
                tx = self
                    .with_host(move |host| {
                        if let Some(outcome) = host.reconcile_command_receipt(&sent)? {
                            handle.block_on(Self::acknowledge_library_update_in(
                                host, &mut tx, &original, &outcome,
                            ))?;
                        }
                        Ok(tx)
                    })
                    .await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }
}
fn self_profile(identity: &Value) -> anyhow::Result<String> {
    Ok(identity["profile"]
        .as_str()
        .context("native profile missing")?
        .into())
}

impl Runtime {
    /// A replacement definition is not a source grant until its exact durable
    /// intent and receipt-derived immutable scope ACK have both been retained.
    pub(crate) async fn require_library_update_ack_in(
        db: &mut sqlx::SqliteConnection,
        catalog: &str,
        incarnation: &str,
        profile: &str,
        definition: &Value,
    ) -> anyhow::Result<()> {
        let Some(operation) = definition.get("replacement_operation") else {
            return Ok(());
        };
        let operation = operation
            .as_str()
            .context("invalid library replacement operation")?;
        let raw: String =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(operation)
                .fetch_optional(&mut *db)
                .await?
                .context("library replacement intent missing")?;
        let original = command_attempts::decode(&raw)?;
        let (id, _, saved, mappings) = update_parts(&original)?;
        anyhow::ensure!(
            original.key().operation_id() == operation
                && original.key().catalog_id() == catalog
                && original.admission_incarnation() == incarnation
                && saved == *definition,
            "library replacement definition differs from retained intent"
        );
        let raw = serde_json::to_string(&mappings)?;
        let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let ack:Vec<(String,String,String,String)>=sqlx::query_as("SELECT catalog_id,incarnation,source_digest,source_mappings_json FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=?").bind(profile).bind(&id).bind(definition["catalog_revision"].as_i64().context("library revision missing")?).fetch_all(db).await?;
        anyhow::ensure!(
            ack == vec![(catalog.into(), incarnation.into(), digest, raw)],
            "library replacement is not receipt acknowledged"
        );
        Ok(())
    }
}
