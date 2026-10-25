//! Terminal aggregate preconditions. The application writer is acquired first;
//! retained native custody covers issuance, effect and the actual commit.
use super::*;
use playscale_core::catalog_aggregate_revision::IssuedRevision;

pub(crate) enum Guarded<T> {
    Mismatch(IssuedRevision),
    Applied { value: T, revision: IssuedRevision },
}
impl Runtime {
    /// Caller has already reauthorized every affected endpoint under `custody`.
    /// The callback may dispatch only its independently prepared exact intent.
    /// Aggregate tokens never replace a native metadata/edge revision check.
    pub(crate) async fn guard_work_and_commit<T, F>(
        &self,
        custody: sqlx::Transaction<'static, sqlx::Sqlite>,
        subject: String,
        expected: u64,
        effect: F,
    ) -> anyhow::Result<Guarded<T>>
    where
        T: Send + 'static,
        F: FnOnce(
                &mut HostSession,
                &mut sqlx::Transaction<'static, sqlx::Sqlite>,
                &IssuedRevision,
            ) -> anyhow::Result<T>
            + Send
            + 'static,
    {
        self.terminal_work_and_commit(custody, subject, Some(expected), effect)
            .await
    }
    /// For an operation with its own exact native edge/version CAS rather than
    /// a source-work If-Match. Still publishes the full dependency transition.
    pub(crate) async fn mutate_work_and_commit<T, F>(
        &self,
        custody: sqlx::Transaction<'static, sqlx::Sqlite>,
        subject: String,
        effect: F,
    ) -> anyhow::Result<Guarded<T>>
    where
        T: Send + 'static,
        F: FnOnce(
                &mut HostSession,
                &mut sqlx::Transaction<'static, sqlx::Sqlite>,
                &IssuedRevision,
            ) -> anyhow::Result<T>
            + Send
            + 'static,
    {
        self.terminal_work_and_commit(custody, subject, None, effect)
            .await
    }
    async fn terminal_work_and_commit<T, F>(
        &self,
        mut custody: sqlx::Transaction<'static, sqlx::Sqlite>,
        subject: String,
        expected: Option<u64>,
        effect: F,
    ) -> anyhow::Result<Guarded<T>>
    where
        T: Send + 'static,
        F: FnOnce(
                &mut HostSession,
                &mut sqlx::Transaction<'static, sqlx::Sqlite>,
                &IssuedRevision,
            ) -> anyhow::Result<T>
            + Send
            + 'static,
    {
        let handle = tokio::runtime::Handle::current();
        self.with_host(move |host| {
            let current =
                handle.block_on(motion_catalog_backend::aggregate_revision::issue_work_in(
                    &mut custody,
                    host,
                    &subject,
                ))?;
            if expected.is_some_and(|expected| current.revision != expected) {
                // A 412 may expose this current token only after its issuance is
                // durable. Rollback/error never leaks a tentative revision.
                handle.block_on(custody.commit())?;
                return Ok(Guarded::Mismatch(current));
            }
            let value = effect(host, &mut custody, &current)?;
            let revision =
                handle.block_on(motion_catalog_backend::aggregate_revision::issue_work_in(
                    &mut custody,
                    host,
                    &subject,
                ))?;
            handle.block_on(custody.commit())?;
            Ok(Guarded::Applied { value, revision })
        })
        .await
    }
}

impl Runtime {
    pub(crate) fn execute_metadata_with_host(
        host: &mut HostSession,
        command: &catabolic_core::catalog_command::CommandEnvelope,
        subject: String,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
    )> {
        use catabolic_core::{catalog_command::ReceiptVisibility, catalog_metadata::SourceKind};
        use catabolic_store::{
            catalog_commands::{CatalogAuthority, ReceiptPolicy, TrustedCommandAuthority},
            catalog_metadata_store::{MetadataAccess, MetadataAuthority},
        };
        // This private adapter owns only the already-authorized work title
        // and Motion work-structure document, never arbitrary system fields.
        let payload = command.payload();
        let mutation = match payload["operation"].as_str() {
            Some("metadata") => &payload["mutation"]["change"],
            Some("metadata_initialize_and_patch") => payload,
            _ => anyhow::bail!("work update operation changed"),
        };
        anyhow::ensure!(
            mutation["subject"] == subject
                && mutation["namespace"] == "motion.local"
                && mutation["record_id"] == "document",
            "work update subject/source changed"
        );
        let edits: serde_json::Value = serde_json::from_str(
            mutation["edits_json"]
                .as_str()
                .context("work edits missing")?,
        )?;
        anyhow::ensure!(
            edits.as_object().is_some_and(|fields| fields.len() == 2)
                && edits["title"]["op"] == "set"
                && edits["title"]["value"].is_string()
                && edits["motion_catalog"]["op"] == "set"
                && edits["motion_catalog"]["value"]["contract_version"] == 1
                && edits["motion_catalog"]["value"]["item_id"] == subject,
            "work update exceeds title/structure grant"
        );
        let namespace = host
            .authorization()
            .map_err(|e| anyhow::anyhow!("native authority: {e:?}"))?
            .namespace()
            .to_owned();
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
                system_fields: std::collections::BTreeSet::from(["motion_catalog".into()]),
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
        let (outcome, witness) = host
            .execute_catalog_command_observed(
                &command,
                grants,
                ReceiptPolicy {
                    retention: Duration::from_secs(30 * 24 * 60 * 60),
                    max_receipts: 100_000,
                    max_logical_bytes: 256 * 1024 * 1024,
                    max_hold_records: 100_000,
                },
            )
            .map_err(|e| anyhow::anyhow!("native metadata command: {e:?}"))?;
        Ok((outcome, witness))
    }
}
