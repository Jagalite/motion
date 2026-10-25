//! Artwork references are current native metadata, not authority inferred from
//! retained Motion association rows. Immutable bytes remain a separate owner.
use super::*;
use playscale_core::artwork::native::ReferenceState;

pub(crate) fn references(
    snapshot: &catabolic_store::catalog_metadata_store::ContributionSnapshot,
) -> anyhow::Result<ReferenceState> {
    let mut owners = snapshot
        .sources
        .iter()
        .filter(|source| source.fields.contains_key("motion_artwork"));
    let owner = owners.next();
    anyhow::ensure!(
        owners.next().is_none(),
        "ambiguous native artwork source ownership"
    );
    match (snapshot.metadata.get("motion_artwork"), owner) {
        (None, None) => Ok(ReferenceState::empty(&snapshot.subject)?),
        (Some(value), Some(source)) => {
            anyhow::ensure!(
                source.namespace == "motion.artwork"
                    && source.record_id == "source_projection"
                    && source.fields.len() == 1
                    && source.fields.get("motion_artwork") == Some(value),
                "artwork effective field lacks exact current source ownership"
            );
            Ok(ReferenceState::parse_json(
                &value.to_string(),
                &snapshot.subject,
            )?)
        }
        _ => anyhow::bail!("native artwork source and effective projection diverged"),
    }
}

impl Runtime {
    pub(crate) async fn prepare_artwork(
        &self,
        operation: String,
        subject: String,
        state: ReferenceState,
        digest: String,
        expected: u64,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
        anyhow::ensure!(state.item_id() == subject, "artwork state subject mismatch");
        let (head, snapshot, namespace) = self
            .with_host(move |host| {
                anyhow::ensure!(
                    host.identity()?["schema_version"] == 40,
                    "artwork mutation requires native40"
                );
                Ok((
                    host.metadata_source_head(&subject, "motion.artwork", "source_projection")?,
                    host.metadata_contributions(&subject)?,
                    host.authorization()?.namespace().to_owned(),
                ))
            })
            .await?;
        references(&snapshot)?;
        let has_scan = snapshot
            .sources
            .iter()
            .any(|source| source.namespace == "motion.scan");
        let mut origin_creation_intent = None;
        if !has_scan && snapshot.schema_fingerprint.is_none() {
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
        let request = catabolic_store::catalog_metadata_store::MetadataSystemReplace {
            contract_version:1,subject:head.subject,namespace:"motion.artwork".into(),record_id:"source_projection".into(),
            expected_schema_fingerprint:head.schema_fingerprint,expected_revision:head.source_revision,
            fields_json:serde_json::json!({"motion_artwork":serde_json::from_str::<serde_json::Value>(&state.to_json()?)?}).to_string(),
            evidence_json:serde_json::json!({"owner":"motion.artwork.references.v1","request_digest":digest,"expected_aggregate_revision":expected}).to_string(),
            origin_creation_intent,
        };
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(head.catalog_id, namespace, &operation)?,
            head.incarnation,
            head.generation,
            &serde_json::json!({"operation":"metadata_system_replace","request":request})
                .to_string(),
        )?;
        self.record_intent(&operation, command).await
    }

    pub(crate) async fn prepare_artwork_attach(
        &self,
        operation: String,
        subject: String,
        asset: playscale_core::artwork::native::AssetReference,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
        let (head, snapshot, namespace) = self
            .with_host(move |host| {
                anyhow::ensure!(
                    host.identity()?["schema_version"] == 40,
                    "artwork attachment requires native40"
                );
                Ok((
                    host.metadata_source_head(&subject, "motion.artwork", "source_projection")?,
                    host.metadata_contributions(&subject)?,
                    host.authorization()?.namespace().to_owned(),
                ))
            })
            .await?;
        references(&snapshot)?;
        let mut origin_creation_intent = None;
        if snapshot.schema_fingerprint.is_none()
            && !snapshot
                .sources
                .iter()
                .any(|source| source.namespace == "motion.scan")
        {
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
        let request = catabolic_store::catalog_metadata_store::MetadataArtworkAttach {
            contract_version: 1,
            subject: head.subject,
            asset,
            origin_creation_intent,
        };
        let command = CommandEnvelope::new(
            1,
            CommandKey::new(head.catalog_id, namespace, &operation)?,
            head.incarnation,
            head.generation,
            &serde_json::json!({"operation":"metadata_artwork_attach", "request":request})
                .to_string(),
        )?;
        self.record_intent(&operation, command).await
    }

    pub(crate) fn execute_artwork_with_host(
        host: &mut catabolic_runtime::host::HostSession,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        subject: &str,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
    )> {
        Self::execute_artwork_command(host, command, subject, None)
    }

    pub(crate) fn execute_artwork_attach_with_host(
        host: &mut catabolic_runtime::host::HostSession,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        subject: &str,
        verified: &playscale_core::artwork::native::AssetReference,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
    )> {
        Self::execute_artwork_command(host, command, subject, Some(verified))
    }

    fn execute_artwork_command(
        host: &mut catabolic_runtime::host::HostSession,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        subject: &str,
        verified: Option<&playscale_core::artwork::native::AssetReference>,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
    )> {
        use catabolic_core::{catalog_command::ReceiptVisibility, catalog_metadata::SourceKind};
        use catabolic_store::{
            catalog_commands::{CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority},
            catalog_metadata_store::{MetadataAccess, MetadataAuthority, MetadataSystemReplace},
        };
        let origin_creation_intent = if let Some(verified) = verified {
            anyhow::ensure!(
                command.payload()["operation"] == "metadata_artwork_attach",
                "not an artwork attach operation"
            );
            let request: catabolic_store::catalog_metadata_store::MetadataArtworkAttach =
                serde_json::from_value(command.payload()["request"].clone())?;
            anyhow::ensure!(
                request.subject == subject && &request.asset == verified,
                "artwork attach descriptor mismatch"
            );
            request.origin_creation_intent
        } else {
            anyhow::ensure!(
                command.payload()["operation"] == "metadata_system_replace",
                "not an artwork source operation"
            );
            let request: MetadataSystemReplace =
                serde_json::from_value(command.payload()["request"].clone())?;
            anyhow::ensure!(
                request.subject == subject
                    && request.namespace == "motion.artwork"
                    && request.record_id == "source_projection",
                "artwork source intent mismatch"
            );
            let fields =
                catabolic_core::catalog_metadata::replacement_fields(&request.fields_json)?;
            anyhow::ensure!(
                fields.len() == 1 && fields.contains_key("motion_artwork"),
                "artwork intent has extra fields"
            );
            ReferenceState::parse_json(&fields["motion_artwork"].to_string(), subject)?;
            request.origin_creation_intent
        };
        let namespace = host.authorization()?.namespace().to_owned();
        let source = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.into(),
            access: MetadataAccess::SourceV2 {
                namespace: "motion.artwork".into(),
                record_id: "source_projection".into(),
                kind: SourceKind::Scan,
                system_fields: ["motion_artwork".into()].into(),
            },
        };
        let snapshot = host.metadata_contributions(subject)?;
        references(&snapshot)?;
        if snapshot.schema_fingerprint.is_none() {
            anyhow::ensure!(
                snapshot.metadata.as_object().is_some_and(|fields| fields
                    .keys()
                    .all(|field| !field.starts_with("motion_") || field == "motion_catalog")),
                "unmanaged artwork subject has unproven protected owner fields"
            );
        }
        let mut initial_system = std::collections::BTreeSet::new();
        if let Some(marker) = snapshot.metadata.get("motion_catalog") {
            anyhow::ensure!(
                marker["contract_version"] == 1 && marker["item_id"].as_str() == Some(subject),
                "unproven artwork work structure"
            );
            for source in snapshot
                .sources
                .iter()
                .filter(|source| source.fields.contains_key("motion_catalog"))
            {
                let known = (source.namespace.as_str(), source.record_id.as_str());
                anyhow::ensure!(
                    matches!(
                        known,
                        ("motion.structure", "source_projection")
                            | ("motion.local", "document")
                            | ("catabolic.legacy_import.v1", "items.metadata")
                    ),
                    "unproven artwork structural source"
                );
                let value = &source.fields["motion_catalog"];
                anyhow::ensure!(
                    value["contract_version"] == 1 && value["item_id"].as_str() == Some(subject),
                    "invalid retained artwork structural source"
                );
            }
            initial_system.insert("motion_catalog".into());
        }
        let initialize = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.into(),
            access: MetadataAccess::InitializeV2 {
                system_fields: initial_system,
            },
        };
        let extension = MetadataAuthority {
            host_namespace: namespace.clone(),
            subject: subject.into(),
            access: MetadataAccess::ExtendSchema,
        };
        let origin = origin_creation_intent
            .as_deref()
            .map(|raw| {
                let creation = motion_catalog_backend::command_attempts::decode(raw)?;
                host.verified_metadata_created_origin(&creation, subject)
                    .map_err(anyhow::Error::from)
            })
            .transpose()?;
        let attach =
            verified.map(
                |asset| catabolic_store::catalog_metadata_store::ArtworkAttachAuthority {
                    host_namespace: namespace.clone(),
                    subject: subject.into(),
                    asset: asset.clone(),
                },
            );
        let mut authority = CatalogAuthority::new(TrustedCommandAuthority {
            host_namespace: &namespace,
            receipt_visibility: ReceiptVisibility::FullResult,
            allow_new_commands: true,
            identity_authority: None,
        })
        .with_metadata(&source)
        .with_metadata_initialization(&initialize)
        .with_metadata_schema(&extension);
        if let Some(origin) = &origin {
            authority = authority.with_metadata_origin(origin);
        }
        if let Some(attach) = &attach {
            authority = authority.with_artwork_attach(attach);
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
