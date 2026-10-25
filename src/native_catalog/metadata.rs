//! Metadata reads and aggregate issuance share retained native owner custody.
use super::*;
impl Runtime {
    pub(crate) async fn metadata_and_commit(
        &self,
        mut custody: sqlx::Transaction<'static, sqlx::Sqlite>,
        subject: String,
    ) -> anyhow::Result<(
        catabolic_store::catalog_metadata_store::ContributionSnapshot,
        u64,
    )> {
        let handle = tokio::runtime::Handle::current();
        self.with_host(move |host| {
            let snapshot = host.metadata_contributions(&subject)?;
            let revision =
                handle.block_on(motion_catalog_backend::aggregate_revision::issue_work_in(
                    &mut custody,
                    host,
                    &subject,
                ))?;
            handle.block_on(custody.commit())?;
            Ok((snapshot, revision.revision))
        })
        .await
    }
}

impl Runtime {
    /// Prepare an exact attributed-source patch, never a legacy document write.
    /// Native39 retains its fixed-schema cohort. Native40 explicitly delegates
    /// append-only schema evolution and atomic tag replacement to its typed owner.
    pub(crate) async fn prepare_contribution(
        &self,
        operation: String,
        subject: String,
        source: String,
        contribution: playscale_core::metadata::Contribution,
        digest: String,
        expected: u64,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
        let namespace = format!("motion.{source}");
        let (head, snapshot, host_namespace, schema_version) = self
            .with_host(move |host| {
                Ok((
                    host.metadata_source_head(&subject, &namespace, "document")?,
                    host.metadata_contributions(&subject)?,
                    host.authorization()?.namespace().to_owned(),
                    host.identity()?["schema_version"]
                        .as_u64()
                        .context("native schema version absent")?,
                ))
            })
            .await?;
        let evidence = serde_json::json!({"owner":"motion.metadata.replace","request_digest":digest,"expected_aggregate_revision":expected}).to_string();
        if schema_version == 40 {
            let legacy = snapshot.sources.iter().find(|record| {
                record.namespace == catabolic_store::catalog_metadata_store::LEGACY_NAMESPACE
            });
            let has_scan = snapshot
                .sources
                .iter()
                .any(|record| record.namespace == "motion.scan");
            let baseline = legacy.map(|record| &record.fields);
            let public_legacy = baseline
                .is_some_and(|fields| fields.keys().any(|field| !field.starts_with("motion_")));
            let mut origin_creation_intent = None;
            if !has_scan && (snapshot.schema_fingerprint.is_none() || public_legacy) {
                let mut tx = self.journal.begin().await?;
                origin_creation_intent =
                    motion_catalog_backend::revision_bridge::created_origin_intent_in(
                        &mut tx,
                        &head.subject,
                        &head.catalog_id,
                        &head.incarnation,
                    )
                    .await?;
                tx.commit().await?;
            }
            if source != "local" && (snapshot.schema_fingerprint.is_none() || public_legacy) {
                let only_title = match baseline {
                    Some(fields) => fields
                        .keys()
                        .all(|field| field == "title" || field.starts_with("motion_")),
                    None => snapshot.metadata.as_object().is_some_and(|fields| {
                        fields
                            .keys()
                            .all(|field| field == "title" || field.starts_with("motion_"))
                    }),
                };
                anyhow::ensure!(
                    origin_creation_intent.is_some() && only_title,
                    "provider replacement requires exact receipt-proven scan origin, without unknown legacy fields"
                );
            }
            let request = catabolic_store::catalog_metadata_store::ContributionReplace {
                contract_version: 2,
                subject: head.subject,
                namespace: head.namespace,
                record_id: "document".into(),
                expected_schema_fingerprint: head.schema_fingerprint,
                expected_revision: head.source_revision,
                fields_json: serde_json::to_string(&contribution.values)?,
                evidence_json: evidence,
                tags: contribution.tags.into_iter().collect(),
                excluded_tags: contribution.excluded_tags.into_iter().collect(),
                origin_creation_intent,
            };
            let payload =
                serde_json::json!({"operation":"metadata_contribution_replace","request":request});
            let command = CommandEnvelope::new(
                1,
                CommandKey::new(head.catalog_id, host_namespace, &operation)?,
                head.incarnation,
                head.generation,
                &payload.to_string(),
            )?;
            return self.record_intent(&operation, command).await;
        }
        anyhow::ensure!(
            schema_version == 39,
            "unsupported contribution native schema"
        );
        let fields = contribution.values;
        let fingerprint = head.schema_fingerprint.clone();
        let old = snapshot
            .sources
            .iter()
            .find(|s| s.namespace == head.namespace && s.record_id == "document");
        let mut edits = serde_json::Map::new();
        if let Some(old) = old {
            for key in old
                .fields
                .keys()
                .filter(|key| !fields.contains_key(*key) && !key.starts_with("motion_"))
            {
                edits.insert(key.clone(), serde_json::json!({"op":"remove"}));
            }
        }
        for (key, value) in fields {
            edits.insert(key, serde_json::json!({"op":"set","value":value}));
        }

        let payload = if let Some(fingerprint) = fingerprint {
            serde_json::json!({"operation":"metadata","mutation":{"contract_version":1,"change":{
                "action":"patch","subject":head.subject,"namespace":head.namespace,"record_id":"document",
                "schema_fingerprint":fingerprint,"expected_revision":head.source_revision,
                "edits_json":serde_json::Value::Object(edits).to_string(),
                "evidence_json":evidence
            }}})
        } else {
            let mut keys: std::collections::BTreeSet<String> = snapshot
                .metadata
                .as_object()
                .context("native item metadata is not an object")?
                .keys()
                .cloned()
                .collect();
            keys.extend(edits.keys().cloned());
            let policies: Vec<_> = keys.into_iter().map(|field| serde_json::json!({"field":field,"value_type":"json","nullable":false,"normalization":"exact"})).collect();
            serde_json::json!({"operation":"metadata_initialize_and_patch","subject":head.subject,"namespace":head.namespace,"record_id":"document","policies":policies,"edits_json":serde_json::Value::Object(edits).to_string(),"evidence_json":evidence})
        };
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(head.catalog_id, host_namespace, &operation)?,
            head.incarnation,
            head.generation,
            &payload.to_string(),
        )?;
        self.record_intent(&operation, command).await
    }

    pub(crate) fn execute_contribution_with_host(
        host: &mut HostSession,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        subject: String,
        source: String,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
    )> {
        use catabolic_core::{catalog_command::ReceiptVisibility, catalog_metadata::SourceKind};
        use catabolic_store::{
            catalog_commands::{CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority},
            catalog_metadata_store::{MetadataAccess, MetadataAuthority},
        };
        let namespace = host.authorization()?.namespace().to_owned();
        let grant = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.clone(),
            access: MetadataAccess::Source {
                namespace: format!("motion.{source}"),
                record_id: "document".into(),
                kind: if source == "local" {
                    SourceKind::Local
                } else {
                    SourceKind::Provider
                },
            },
        };
        let initialization_access = if host.identity()?["schema_version"].as_u64() == Some(40) {
            let snapshot = host.metadata_contributions(&subject)?;
            let mut system_fields = std::collections::BTreeSet::new();
            if let Some(marker) = snapshot.metadata.get("motion_catalog") {
                anyhow::ensure!(
                    marker["contract_version"] == 1
                        && marker["item_id"].as_str() == Some(subject.as_str()),
                    "unproven native work structural marker"
                );
                system_fields.insert("motion_catalog".to_owned());
            }
            MetadataAccess::InitializeV2 { system_fields }
        } else {
            MetadataAccess::Initialize
        };
        let initialization = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.clone(),
            access: initialization_access,
        };
        let extension = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.clone(),
            access: MetadataAccess::ExtendSchema,
        };
        let tags = catabolic_store::catalog_tag_mutation::TagAuthority {
            host_namespace: namespace.clone(),
            allowed_subjects: std::collections::BTreeSet::from([
                catabolic_store::catalog_tag_mutation::TagSubject::Item(subject),
            ]),
            allow_global_merge: false,
        };
        let origin = command.payload()["request"]["origin_creation_intent"]
            .as_str()
            .map(|raw| {
                let creation = motion_catalog_backend::command_attempts::decode(raw)?;
                host.verified_metadata_created_origin(&creation, &grant.subject)
                    .map_err(anyhow::Error::from)
            })
            .transpose()?;
        let mut authority = CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: &namespace,
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_metadata(&grant)
        .with_metadata_initialization(&initialization)
        .with_metadata_schema(&extension)
        .with_tags(&tags);
        if let Some(origin) = &origin {
            authority = authority.with_metadata_origin(origin);
        }
        Ok(host.execute_catalog_command_observed(
            command,
            authority,
            ReceiptPolicy {
                retention: Duration::from_secs(30 * 24 * 60 * 60),
                max_receipts: 100_000,
                max_logical_bytes: 256 * 1024 * 1024,
                max_hold_records: 100_000,
            },
        )?)
    }
}
