//! Retained native server admission. A configured native catalog is never
//! replaced with a legacy connection when admission or an operation fails.
//!
//! The cutover owner retains the native writer from reconciliation through
//! serving. Blocking workers own the runtime mutex even if an HTTP future is
//! cancelled, so neither cancellation nor an async timeout releases ownership.
pub(crate) mod artwork;
mod edition;
mod guarded;
mod library_policy;
pub(crate) mod library_update;
mod metadata;
pub(crate) use guarded::Guarded;
mod scan_scope;
pub(crate) use scan_scope::sources_in as scan_sources_in;
mod scoped;
pub(crate) mod terminal;
use anyhow::Context;
use catabolic_runtime::host::HostSession;
use motion_catalog_backend::cutover::ServerAdmission;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Debug)]
pub(crate) struct ReadFailure(pub catabolic_store::catalog_scope::ScopeErrorCode);
impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native scoped read: {:?}", self.0)
    }
}
impl std::error::Error for ReadFailure {}

pub(crate) mod timeline_visibility;

/// Only a forward generation advance under the same admitted identity may be
/// retried by rolling back and rebuilding before acquiring a new writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionFreshness {
    Current,
    Advanced { projected: i64, current: i64 },
}
fn projection_freshness(
    state: &(String, String, i64),
    identity: &serde_json::Value,
    current: i64,
) -> anyhow::Result<ProjectionFreshness> {
    anyhow::ensure!(
        identity["catalog_id"].as_str() == Some(state.0.as_str())
            && identity["catalog_incarnation"].as_str() == Some(state.1.as_str()),
        "native projection belongs to another catalog identity"
    );
    anyhow::ensure!(
        state.2 >= 0 && current >= state.2,
        "native projection generation is invalid or ahead of retained owner"
    );
    Ok(if current == state.2 {
        ProjectionFreshness::Current
    } else {
        ProjectionFreshness::Advanced {
            projected: state.2,
            current,
        }
    })
}

/// Origin-typed refusal before an ordinary callback is admitted. This does not
/// classify callback, storage, inventory or cleanup failures as cancellation.
#[derive(Debug)]
pub(crate) struct OrdinaryAdmissionDraining;
impl std::fmt::Display for OrdinaryAdmissionDraining {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native ordinary admission is draining")
    }
}
impl std::error::Error for OrdinaryAdmissionDraining {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OrdinaryAdmission {
    Open,
    Draining,
}

struct RuntimeAdmission {
    server: ServerAdmission,
    terminal: bool,
}
pub struct Runtime {
    admission: Arc<Mutex<RuntimeAdmission>>,
    ordinary_admission: Arc<Mutex<OrdinaryAdmission>>,
    path: PathBuf,
    profile: String,
    journal: sqlx::SqlitePool,
    motion_path: PathBuf,
}

/// Version of the deliberately bounded HTTP serving contract. Unsupported
/// families remain denied by the positive v2 route allowlist.
pub const SERVING_CONTRACT_VERSION: u32 = 1;

impl Runtime {
    /// This does not initialize a catalog, migrate live data or grant activation.
    /// Both databases must already agree on a completed operator cutover.
    pub async fn open(
        journal: sqlx::SqlitePool,
        path: PathBuf,
        profile: String,
    ) -> anyhow::Result<Self> {
        let motion_file: String =
            sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name='main'")
                .fetch_one(&journal)
                .await?;
        anyhow::ensure!(
            !motion_file.is_empty(),
            "native server requires durable Motion control storage"
        );
        let motion_path = PathBuf::from(motion_file);
        let admission = motion_catalog_backend::cutover::open_server(
            journal.clone(),
            path.clone(),
            profile.clone(),
        )
        .await?;
        let runtime = Self {
            ordinary_admission: Arc::new(Mutex::new(OrdinaryAdmission::Open)),
            admission: Arc::new(Mutex::new(RuntimeAdmission {
                server: admission,
                terminal: false,
            })),
            path,
            profile,
            journal,
            motion_path,
        };
        let imported =
            motion_catalog_backend::imported_sources::prepare_imported_sources(&runtime.journal)
                .await?;
        let custody = runtime.journal.begin_with("BEGIN IMMEDIATE").await?;
        let handle = tokio::runtime::Handle::current();
        runtime
            .with_host(move |host| {
                handle.block_on(motion_catalog_backend::imported_sources::acknowledge(
                    custody, host, imported,
                ))
            })
            .await?;
        runtime.initialize_source_lifecycles().await?;
        runtime.reconcile_creation_receipts().await?;
        runtime.reconcile_empty_editions().await?;
        crate::native_discovery::reconcile_receipts(&runtime, &runtime.journal).await?;
        crate::native_processing::reconcile_receipts(&runtime, &runtime.journal).await?;
        runtime.reconcile_library_updates().await?;
        runtime.prepare_library_scopes().await?;
        // Register the fixed unrestricted read selection before any processing
        // admission captures its native manifest. The first sidebar read must
        // not create a saved query and advance that manifest's fallback epoch.
        // This prepares a definition, not a principal grant; reads still admit
        // their freshly authorized scope through the retained native owner.
        runtime.prepare_all_scope().await?;
        runtime.rebuild_projection().await?;
        crate::native_processing::acknowledge_jobs(&runtime, &runtime.journal).await?;
        Ok(runtime)
    }

    /// Establish administrative source state only from the independently
    /// retained descriptor and native binding owner. Offline roots remain active
    /// administrative sources; actual byte/scan admission verifies availability.
    async fn initialize_source_lifecycles(&self) -> anyhow::Result<()> {
        let mut after = String::new();
        loop {
            let mut custody = self.journal.begin_with("BEGIN IMMEDIATE").await?;
            let ids: Vec<String> = sqlx::query_scalar(
                "SELECT library_id FROM catabolic_source_mappings WHERE library_id>? AND library_id<>? ORDER BY library_id LIMIT 64")
                .bind(&after).bind(motion_catalog_backend::generated_source::SOURCE_ID)
                .fetch_all(&mut *custody).await?;
            if ids.is_empty() {
                custody.commit().await?;
                break;
            }
            after = ids.last().context("source lifecycle page missing")?.clone();
            let handle = tokio::runtime::Handle::current();
            self.with_host(move |host| {
                handle.block_on(async {
                    for id in ids {
                        motion_catalog_backend::source_lifecycle::initialize_with_host(
                            &mut custody,
                            host,
                            &id,
                            crate::now(),
                        )
                        .await?;
                    }
                    custody.commit().await?;
                    Ok(())
                })
            })
            .await?;
        }
        Ok(())
    }

    /// Recover only receipt-derived acknowledgments. An absent receipt remains
    /// pending: startup must never turn a saved HTTP intent into a new write.
    async fn reconcile_creation_receipts(&self) -> anyhow::Result<()> {
        use catabolic_core::catalog_command::CommandEnvelope;
        use catabolic_store::catalog_commands::CommandOutcome;
        let identity = self.identity().await?;
        let catalog = identity["catalog_id"]
            .as_str()
            .context("native catalog identity missing")?;
        let incarnation = identity["catalog_incarnation"]
            .as_str()
            .context("native incarnation missing")?;
        let mut after = String::new();
        loop {
            let mut custody = self.journal.begin_with("BEGIN IMMEDIATE").await?;
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT operation_id,envelope_json FROM catabolic_commands WHERE operation_id>? ORDER BY operation_id LIMIT 128")
                .bind(&after).fetch_all(&mut *custody).await?;
            if rows.is_empty() {
                custody.commit().await?;
                break;
            }
            after = rows.last().context("receipt page missing")?.0.clone();
            for (operation, saved) in rows {
                let value: serde_json::Value = serde_json::from_str(&saved)?;
                if value["payload"]["operation"] != "item_create_batch" {
                    continue;
                }
                let command = CommandEnvelope::new(
                    value["canonicalization_version"]
                        .as_u64()
                        .context("intent version missing")?
                        .try_into()?,
                    serde_json::from_value(value["key"].clone())?,
                    value["admission_incarnation"]
                        .as_str()
                        .context("intent incarnation missing")?,
                    value["expected_generation"]
                        .as_i64()
                        .context("intent generation missing")?,
                    &value["payload"].to_string(),
                )?;
                anyhow::ensure!(
                    command.canonical_payload() == saved
                        && command.key().operation_id() == operation
                        && command.key().catalog_id() == catalog
                        && command.admission_incarnation() == incarnation,
                    "durable creation intent identity changed"
                );
                let (_, command) =
                    motion_catalog_backend::command_attempts::latest_in(&mut custody, &command)
                        .await?;
                let ids = command.payload()["items"]
                    .as_array()
                    .context("creation items missing")?
                    .iter()
                    .map(|item| {
                        item["id"]
                            .as_str()
                            .map(str::to_owned)
                            .context("creation ID missing")
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                anyhow::ensure!(
                    !ids.is_empty() && ids.len() <= 128,
                    "creation receipt bounds"
                );
                let mut acknowledged = true;
                for id in &ids {
                    let bridge:Option<(String,String,i64,i64,String)>=sqlx::query_as(
                        "SELECT catalog_id,incarnation,motion_base,native_base,proof_json FROM catabolic_revision_bridges WHERE native_id=?")
                        .bind(id).fetch_optional(&mut *custody).await?;
                    if let Some((catalog, incarnation, motion, native, proof)) = bridge {
                        let proof: serde_json::Value = serde_json::from_str(&proof)?;
                        anyhow::ensure!(
                            catalog == command.key().catalog_id()
                                && incarnation == command.admission_incarnation()
                                && motion == 1
                                && native == 1
                                && proof["kind"] == "native_create_only"
                                && proof["intent"] == command.canonical_payload(),
                            "creation acknowledgment identity changed"
                        );
                    } else {
                        acknowledged = false;
                    }
                }
                // An independently committed full-receipt acknowledgment stays
                // valid after native receipt retention expires.
                if acknowledged {
                    continue;
                }
                let sent = command.clone();
                let (outcome, returned) = self
                    .with_host(move |host| {
                        let outcome = host
                            .reconcile_command_receipt(&sent)
                            .map_err(|e| anyhow::anyhow!("native receipt reconciliation: {e:?}"))?;
                        Ok((outcome, custody))
                    })
                    .await?;
                custody = returned;
                match outcome {
                    Some(outcome @ CommandOutcome::Replay { .. }) => {
                        for id in ids {
                            motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
                                &mut custody,
                                &id,
                                &command,
                                &outcome,
                            )
                            .await?;
                        }
                    }
                    None => {}
                    Some(other) => {
                        anyhow::bail!("creation receipt cannot be reconciled: {}", other.code())
                    }
                }
            }
            custody.commit().await?;
        }
        Ok(())
    }

    /// The executable's packaging owner must supply this grant. Opening a
    /// library runtime does not prove that its caller embeds the closed worker
    /// dispatcher (notably, integration-test harnesses do not).
    pub async fn register_observer(
        &self,
        artifact: catabolic_runtime::helper::TrustedClosedObservationArtifact,
    ) -> anyhow::Result<()> {
        self.with_host(move |host| {
            host.register_closed_observation_helper(&artifact)
                .map_err(|e| anyhow::anyhow!("native observer registration: {e:?}"))
        })
        .await
    }

    /// Admit the supported contract only after the durable cutover and current
    /// native-authoritative projection agree. The HTTP positive allowlist, not
    /// stale legacy rows, defines which operations this contract can serve.
    pub async fn require_serving_routes(&self) -> anyhow::Result<()> {
        let mut custody = self.journal.begin_with("BEGIN IMMEDIATE").await?;
        self.validate_projection(&mut custody).await?;
        custody.commit().await?;
        Ok(())
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Refuse new ordinary work without waiting for a currently admitted
    /// physical owner to finish. No callback ever runs while this gate is held.
    pub(crate) fn begin_drain(&self) {
        *self
            .ordinary_admission
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = OrdinaryAdmission::Draining;
    }

    pub(crate) async fn with_host<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut HostSession) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        {
            let gate = self
                .ordinary_admission
                .lock()
                .map_err(|_| anyhow::anyhow!("native admission gate poisoned"))?;
            if *gate != OrdinaryAdmission::Open {
                return Err(OrdinaryAdmissionDraining.into());
            }
        }
        let owner = self.admission.clone();
        let gate = self.ordinary_admission.clone();
        tokio::task::spawn_blocking(move || {
            let mut admission = owner
                .lock()
                .map_err(|_| anyhow::anyhow!("native runtime poisoned"))?;
            anyhow::ensure!(!admission.terminal, "native admission is terminally closed");
            {
                // Linearization point: host custody is already held. An action
                // admitted here is existing work; BeginDrain never interrupts it.
                let gate = gate
                    .lock()
                    .map_err(|_| anyhow::anyhow!("native admission gate poisoned"))?;
                if *gate != OrdinaryAdmission::Open {
                    return Err(OrdinaryAdmissionDraining.into());
                }
            }
            let host = admission
                .server
                .admission
                .admitted_host()
                .map_err(|e| anyhow::anyhow!("native server admission: {e:?}"))?;
            action(host)
        })
        .await
        .context("native runtime worker failed")?
    }

    /// Read-only application admission never queues behind a long native observer.
    /// Reserve Motion first, then attempt native custody once. Busy means no grant.
    pub(crate) async fn try_rebuild_projection(&self) -> anyhow::Result<bool> {
        let path = self.motion_path.clone();
        let owner = self.admission.clone();
        let gate = self.ordinary_admission.clone();
        tokio::task::spawn_blocking(move || {
            if *gate
                .lock()
                .map_err(|_| anyhow::anyhow!("native admission gate poisoned"))?
                != OrdinaryAdmission::Open
            {
                return Err(OrdinaryAdmissionDraining.into());
            }
            let Some(writer) =
                motion_catalog_backend::projection::ProjectionWriter::try_reserve(&path)?
            else {
                return Ok(false);
            };
            let mut admission = match owner.try_lock() {
                Ok(owner) => owner,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    anyhow::bail!("native runtime poisoned")
                }
            };
            anyhow::ensure!(!admission.terminal, "native admission is terminally closed");
            if *gate
                .lock()
                .map_err(|_| anyhow::anyhow!("native admission gate poisoned"))?
                != OrdinaryAdmission::Open
            {
                return Err(OrdinaryAdmissionDraining.into());
            }
            let host = admission
                .server
                .admission
                .admitted_host()
                .map_err(|e| anyhow::anyhow!("native server admission: {e:?}"))?;
            writer.rebuild(host)?;
            Ok(true)
        })
        .await
        .context("native projection try-admission worker failed")?
    }

    /// Same retained WRITE and exact identity/generation classification as the
    /// ordinary path, but never wait on the native owner while holding Motion.
    pub(crate) async fn try_projection_freshness(
        &self,
        conn: &mut sqlx::SqliteConnection,
    ) -> anyhow::Result<Option<ProjectionFreshness>> {
        let state: (String,String,i64) = sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1")
            .fetch_one(conn).await?;
        let owner = self.admission.clone();
        let gate = self.ordinary_admission.clone();
        tokio::task::spawn_blocking(move || {
            let mut admission = match owner.try_lock() {
                Ok(owner) => owner,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    anyhow::bail!("native runtime poisoned")
                }
            };
            anyhow::ensure!(!admission.terminal, "native admission is terminally closed");
            if *gate
                .lock()
                .map_err(|_| anyhow::anyhow!("native admission gate poisoned"))?
                != OrdinaryAdmission::Open
            {
                return Err(OrdinaryAdmissionDraining.into());
            }
            let host = admission
                .server
                .admission
                .admitted_host()
                .map_err(|e| anyhow::anyhow!("native server admission: {e:?}"))?;
            Ok(Some(projection_freshness(
                &state,
                &host.identity()?,
                host.catalog_generation()?,
            )?))
        })
        .await
        .context("native projection freshness try-admission worker failed")?
    }

    pub(crate) async fn rebuild_projection(
        &self,
    ) -> anyhow::Result<motion_catalog_backend::projection::ProjectionStatus> {
        let path = self.motion_path.clone();
        // All cross-owner operations reserve Motion before waiting for native.
        // The opaque writer remains owned by the queued blocking closure even
        // when the HTTP receiver is cancelled.
        let writer = tokio::task::spawn_blocking(move || {
            motion_catalog_backend::projection::ProjectionWriter::reserve(&path)
        })
        .await
        .context("native projection reservation worker failed")??;
        self.with_host(move |host| writer.rebuild(host)).await
    }

    /// Validate a disposable model while the caller retains Motion's writer
    /// reservation. Never rebuild within that reservation. All native mutation
    /// adapters, including saved-query preparation, take this fence first.
    pub(crate) async fn validate_projection(
        &self,
        conn: &mut sqlx::SqliteConnection,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.projection_freshness(conn).await? == ProjectionFreshness::Current,
            "native projection generation advanced"
        );
        Ok(())
    }

    /// Caller owns the Motion writer. This only classifies currentness; it
    /// grants no cache access and never rebuilds within that reservation.
    pub(crate) async fn projection_freshness(
        &self,
        conn: &mut sqlx::SqliteConnection,
    ) -> anyhow::Result<ProjectionFreshness> {
        let state: (String, String, i64) = sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1")
            .fetch_one(conn).await?;
        self.with_host(move |host| {
            projection_freshness(&state, &host.identity()?, host.catalog_generation()?)
        })
        .await
    }

    pub async fn identity(&self) -> anyhow::Result<serde_json::Value> {
        self.with_host(|host| {
            host.identity()
                .map_err(|e| anyhow::anyhow!("native identity: {e:?}"))
        })
        .await
    }

    /// Commit the exact intent before native dispatch. Retrying the same HTTP
    /// operation uses the original generation and item ID, even after a crash.
    pub(crate) async fn prepare_work(
        &self,
        operation: String,
        id: String,
        title: String,
        media_type: String,
        request_digest: String,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        let native_kind = playscale_core::catalog_kind::to_native("video")
            .context("unsupported native work kind")?;
        let payload = serde_json::json!({"operation":"item_create_batch","items":[{
            "id":id,"kind":native_kind,"metadata":{"title":title,"motion_catalog":{
                "contract_version":1,"item_id":id,"kind":"video","catalog_revision":1,"creation_request_digest":request_digest,
                "match_state":"unmatched","media_type":media_type,"parent_id":null,"external_ids":[],
                "number":null,"structure_revision":1
            }}
        }]});
        self.prepare_item_intent(operation, id, payload).await
    }

    pub(crate) async fn prepare_library(
        &self,
        operation: String,
        id: String,
        definition: serde_json::Value,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        let native_id = format!("motion.library:{id}");
        let payload = serde_json::json!({"operation":"item_create_batch","items":[{
            "id":native_id,"kind":"custom:motion_library","metadata":{
                "title":definition["name"],"motion_library":definition
            }
        }]});
        self.prepare_item_intent(operation, native_id, payload)
            .await
    }

    async fn prepare_item_intent(
        &self,
        operation: String,
        id: String,
        payload: serde_json::Value,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
        let prior: Option<String> =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(&operation)
                .fetch_optional(&self.journal)
                .await?;
        let existing_intent = prior.is_some();
        let create_id = id.clone();
        let (identity, generation, namespace) = self
            .with_host(move |host| {
                let (exists, generation) = host
                    .observe_item_for_create(&create_id)
                    .map_err(|e| anyhow::anyhow!("native create observation: {e:?}"))?;
                anyhow::ensure!(
                    existing_intent || !exists,
                    "new work ID already exists in the native catalog"
                );
                Ok((
                    host.identity()
                        .map_err(|e| anyhow::anyhow!("native identity: {e:?}"))?,
                    generation,
                    host.authorization()
                        .map_err(|e| anyhow::anyhow!("native authority: {e:?}"))?
                        .namespace()
                        .to_owned(),
                ))
            })
            .await?;
        let requested = CommandEnvelope::new(
            1,
            CommandKey::new(
                identity["catalog_id"]
                    .as_str()
                    .context("missing catalog id")?,
                namespace,
                &operation,
            )?,
            identity["catalog_incarnation"]
                .as_str()
                .context("missing incarnation")?,
            generation,
            &payload.to_string(),
        )?;
        self.record_intent(&operation, requested).await
    }

    async fn record_intent(
        &self,
        operation: &str,
        requested: catabolic_core::catalog_command::CommandEnvelope,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        use catabolic_core::catalog_command::CommandEnvelope;
        let mut tx = self.journal.begin().await?;
        sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?) ON CONFLICT(operation_id) DO NOTHING")
            .bind(&operation).bind(requested.canonical_payload()).bind(crate::now()).execute(&mut *tx).await?;
        let saved: String =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(&operation)
                .fetch_one(&mut *tx)
                .await?;
        let value: serde_json::Value = serde_json::from_str(&saved)?;
        anyhow::ensure!(
            value["canonicalization_version"].as_u64()
                == Some(u64::from(
                    catabolic_core::catalog_command::COMMAND_CANONICALIZATION_VERSION
                )),
            "unknown intent encoding"
        );
        let recorded = CommandEnvelope::new(
            1,
            serde_json::from_value(value["key"].clone())?,
            value["admission_incarnation"]
                .as_str()
                .context("missing intent incarnation")?,
            value["expected_generation"]
                .as_i64()
                .context("missing intent generation")?,
            &value["payload"].to_string(),
        )?;
        anyhow::ensure!(
            recorded.canonical_payload() == saved
                && recorded.key() == requested.key()
                && recorded.admission_incarnation() == requested.admission_incarnation()
                && recorded.payload() == requested.payload(),
            "operation key is bound to a different native intent"
        );
        tx.commit().await?;
        Ok(recorded)
    }

    pub(crate) async fn prepare_work_update(
        &self,
        operation: String,
        subject: String,
        title: String,
        projection: serde_json::Value,
        request_digest: String,
    ) -> anyhow::Result<catabolic_core::catalog_command::CommandEnvelope> {
        use catabolic_core::catalog_command::{CommandEnvelope, CommandKey};
        let head_subject = subject.clone();
        let (head, host_namespace) = self
            .with_host(move |host| {
                Ok((
                    host.metadata_source_head(&head_subject, "motion.local", "document")
                        .map_err(|e| anyhow::anyhow!("native metadata head: {e:?}"))?,
                    host.authorization()
                        .map_err(|e| anyhow::anyhow!("native authority: {e:?}"))?
                        .namespace()
                        .to_owned(),
                ))
            })
            .await?;
        let edits = serde_json::json!({"title":{"op":"set","value":title},"motion_catalog":{"op":"set","value":projection}}).to_string();
        let evidence = serde_json::json!({"owner":"motion.catalog.replace","request_digest":request_digest,"expected_catalog_revision":projection["catalog_revision"].as_u64().context("missing next revision")?.checked_sub(1).context("invalid next revision")?}).to_string();
        let payload = match head.schema_fingerprint {
            Some(fingerprint) => {
                serde_json::json!({"operation":"metadata","mutation":{"contract_version":1,"change":{
                    "action":"patch","subject":subject,"namespace":"motion.local","record_id":"document",
                    "schema_fingerprint":fingerprint,"expected_revision":head.source_revision,"edits_json":edits,"evidence_json":evidence
                }}})
            }
            None => {
                serde_json::json!({"operation":"metadata_initialize_and_patch","subject":subject,
                "policies":[{"field":"title","value_type":"string","nullable":false,"normalization":"exact"},
                    {"field":"motion_catalog","value_type":"json","nullable":false,"normalization":"exact"}],
                "namespace":"motion.local","record_id":"document","edits_json":edits,"evidence_json":evidence})
            }
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

    pub(crate) async fn execute_work_update<C: Send + 'static>(
        &self,
        command: catabolic_core::catalog_command::CommandEnvelope,
        subject: String,
        custody: C,
    ) -> anyhow::Result<(catabolic_store::catalog_commands::CommandOutcome, C)> {
        let (outcome, _, custody) = self
            .execute_work_update_observed(command, subject, custody)
            .await?;
        Ok((outcome, custody))
    }
    pub(crate) async fn execute_work_update_observed<C: Send + 'static>(
        &self,
        command: catabolic_core::catalog_command::CommandEnvelope,
        subject: String,
        custody: C,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
        C,
    )> {
        self.with_host(move |host| {
            let (outcome, witness) = Self::execute_metadata_with_host(host, &command, subject)?;
            Ok((outcome, witness, custody))
        })
        .await
    }

    /// Call only while the caller's current Motion write authorization is held.
    pub(crate) async fn execute_work<C: Send + 'static>(
        &self,
        command: catabolic_core::catalog_command::CommandEnvelope,
        custody: C,
    ) -> anyhow::Result<(catabolic_store::catalog_commands::CommandOutcome, C)> {
        let (outcome, _, custody) = self.execute_work_observed(command, custody).await?;
        Ok((outcome, custody))
    }
    pub(crate) async fn execute_work_observed<C: Send + 'static>(
        &self,
        command: catabolic_core::catalog_command::CommandEnvelope,
        custody: C,
    ) -> anyhow::Result<(
        catabolic_store::catalog_commands::CommandOutcome,
        Option<catabolic_runtime::host::StaleGenerationWitness>,
        C,
    )> {
        self.with_host(move |host| {
            use catabolic_core::catalog_command::ReceiptVisibility;
            use catabolic_store::catalog_commands::{ReceiptPolicy, TrustedCommandAuthority};
            let namespace = host
                .authorization()
                .map_err(|e| anyhow::anyhow!("native authority: {e:?}"))?
                .namespace()
                .to_owned();
            let (outcome, witness) = host
                .execute_command_observed(
                    &command,
                    TrustedCommandAuthority {
                        host_namespace: &namespace,
                        receipt_visibility: ReceiptVisibility::FullResult,
                        allow_new_commands: true,
                        identity_authority: None,
                    },
                    ReceiptPolicy {
                        retention: Duration::from_secs(30 * 24 * 60 * 60),
                        max_receipts: 100_000,
                        max_logical_bytes: 256 * 1024 * 1024,
                        max_hold_records: 100_000,
                    },
                )
                .map_err(|e| anyhow::anyhow!("native command: {e:?}"))?;
            // Custody includes the Motion writer authorization transaction.
            // It follows the queued worker through dispatch even if HTTP drops.
            Ok((outcome, witness, custody))
        })
        .await
    }

    /// Unrestricted Motion principals may browse work records, including works
    /// without physical membership. Restricted principals must use separately
    /// proven per-library scopes and cannot substitute this grant.
    pub(crate) async fn prepare_all_scope(
        &self,
    ) -> anyhow::Result<catabolic_store::catalog_scope::TrustedSelection> {
        use catabolic_store::catalog_scope::{SavedQueryRef, TrustedSelection};
        use sha2::{Digest, Sha256};
        let profile = self.profile.clone();
        let digest = format!("{:x}", Sha256::digest(b"[]"));
        let custody: sqlx::Transaction<'static, sqlx::Sqlite> =
            self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let (identity, saved, mut tx) = self.with_host(move |host| {
            let identity = host.identity().map_err(|e| anyhow::anyhow!("native identity: {e:?}"))?;
            let definition = serde_json::json!({"version":1,"mode":"selection","selection":{
                "language":"sql","query":"SELECT item_id FROM catalog_items WHERE json_type(metadata,'$.motion_catalog')='object' AND item_id=json_extract(metadata,'$.motion_catalog.item_id') ORDER BY item_id"
            }});
            let saved = host.prepare_selection("motion-server-works-v1", &definition)
                .map_err(|e| anyhow::anyhow!("native selection preparation: {e:?}"))?;
            Ok((identity, saved, custody))
        }).await?;
        let catalog = identity["catalog_id"]
            .as_str()
            .context("missing catalog id")?;
        let incarnation = identity["catalog_incarnation"]
            .as_str()
            .context("missing incarnation")?;
        let query_id = saved["id"].as_str().context("missing saved query id")?;
        let revision = saved["revision"]
            .as_i64()
            .context("missing saved query revision")?;
        let query_digest = saved["digest"]
            .as_str()
            .context("missing saved query digest")?;
        sqlx::query("INSERT INTO catabolic_server_scopes(profile,library_id,library_revision,catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest,created_at) VALUES(?,'*',0,?,?,?,'[]',?,?,?,?) ON CONFLICT DO NOTHING")
            .bind(&profile).bind(catalog).bind(incarnation).bind(&digest)
            .bind(query_id).bind(revision).bind(query_digest).bind(crate::now()).execute(&mut *tx).await?;
        let stored:(String,String,String,i64,String) = sqlx::query_as("SELECT catalog_id,incarnation,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id='*' AND library_revision=0 AND source_digest=?")
            .bind(&profile).bind(&digest).fetch_one(&mut *tx).await?;
        anyhow::ensure!(
            stored
                == (
                    catalog.into(),
                    incarnation.into(),
                    query_id.into(),
                    revision,
                    query_digest.into()
                ),
            "saved server scope identity changed"
        );
        tx.commit().await?;
        let reference = SavedQueryRef::new(query_id.into(), revision, query_digest.into())
            .map_err(|e| anyhow::anyhow!("native query reference: {e:?}"))?;
        TrustedSelection::new(
            reference,
            vec![
                "title".into(),
                "motion_catalog".into(),
                "motion_artwork".into(),
            ],
        )
        .map_err(|e| anyhow::anyhow!("native field scope: {e:?}"))
    }

    pub(crate) async fn prepare_library_definitions(
        &self,
        ids: Option<Vec<String>>,
    ) -> anyhow::Result<catabolic_store::catalog_scope::TrustedSelection> {
        use catabolic_store::catalog_scope::{SavedQueryRef, TrustedSelection};
        use sha2::{Digest, Sha256};
        let predicate = match ids {
            None => "1".to_owned(),
            Some(ids) => {
                anyhow::ensure!(
                    ids.len() <= 200
                        && ids.iter().all(|id| !id.is_empty()
                            && id.len() <= 128
                            && id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))),
                    "library definition scope exceeds policy identity contract"
                );
                let literals: Vec<String> = ids
                    .into_iter()
                    .map(|id| format!("'motion.library:{}'", id.replace('\'', "''")))
                    .collect();
                if literals.is_empty() {
                    "0".into()
                } else {
                    format!("item_id IN ({})", literals.join(","))
                }
            }
        };
        let query = format!(
            "SELECT item_id FROM catalog_items WHERE kind='custom:motion_library' AND json_type(metadata,'$.motion_library')='object' AND {predicate} ORDER BY item_id"
        );
        anyhow::ensure!(
            query.len() <= 32 * 1024,
            "library definition query exceeds budget"
        );
        let name = format!(
            "{:x}",
            Sha256::digest(format!("motion-libraries:{query}").as_bytes())
        );
        let custody: sqlx::Transaction<'static, sqlx::Sqlite> =
            self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let (saved,custody)=self.with_host(move|host|{
            let saved=host.prepare_selection(&name,&serde_json::json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}))?;
            Ok((saved,custody))
        }).await?;
        custody.commit().await?;
        let reference = SavedQueryRef::new(
            saved["id"].as_str().context("library query id")?.into(),
            saved["revision"]
                .as_i64()
                .context("library query revision")?,
            saved["digest"]
                .as_str()
                .context("library query digest")?
                .into(),
        )
        .map_err(|e| anyhow::anyhow!("library query reference: {e:?}"))?;
        TrustedSelection::new(reference, vec!["title".into(), "motion_library".into()])
            .map_err(|e| anyhow::anyhow!("library field grant: {e:?}"))
    }

    /// Reconstruct only scope acknowledgments from authoritative native library
    /// definitions and independently retained exact physical source mappings.
    /// This runs before serving; no library metadata alone grants a source.
    pub(crate) async fn prepare_library_scopes(&self) -> anyhow::Result<()> {
        use motion_catalog_backend::sources::SourceMapping;
        use sha2::{Digest, Sha256};
        use std::collections::BTreeSet;
        let grant = self.prepare_library_definitions(None).await?;
        let mut custody: sqlx::Transaction<'static, sqlx::Sqlite> =
            self.journal.begin_with("BEGIN IMMEDIATE").await?;
        let libraries = self.read_libraries(&mut custody, vec![grant], None).await?;
        let mut prepared = Vec::new();
        for row in libraries {
            let metadata: serde_json::Value =
                serde_json::from_str(row[2].as_str().context("missing native library metadata")?)?;
            let definition = &metadata["motion_library"];
            let id = definition["id"]
                .as_str()
                .context("native library id missing")?
                .to_owned();
            anyhow::ensure!(
                definition["contract_version"] == 1
                    && row[0] == format!("motion.library:{id}")
                    && row[1] == "custom:motion_library",
                "native library definition identity mismatch"
            );
            let revision = definition["catalog_revision"]
                .as_u64()
                .and_then(|v| i64::try_from(v).ok())
                .context("invalid native library revision")?;
            let ids: Vec<String> = serde_json::from_value(definition["source_ids"].clone())?;
            anyhow::ensure!(
                ids.len() <= 100 && ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
                "native library source membership exceeds contract"
            );
            let mut mappings = Vec::new();
            for source in ids {
                let raw: String = sqlx::query_scalar(
                    "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
                )
                .bind(&source)
                .fetch_one(&mut *custody)
                .await?;
                let mapping: SourceMapping = serde_json::from_str(&raw)?;
                anyhow::ensure!(
                    mapping.library_id == source,
                    "source mapping identity mismatch"
                );
                motion_catalog_backend::sources::check_mapping_in(&mut custody, &mapping).await?;
                mappings.push(mapping);
            }
            mappings.sort_by(|a, b| a.library_id.cmp(&b.library_id));
            prepared.push((id, revision, mappings));
        }
        let (identity,scopes,mut custody)=self.with_host(move|host| {
            let identity=host.identity()?;
            let mut scopes=Vec::new();
            for (library,revision,mappings) in prepared {
                for mapping in &mappings {
                    anyhow::ensure!(identity["catalog_id"]==mapping.catalog.catalog_id && identity["catalog_incarnation"]==mapping.catalog.incarnation && identity["profile"]==mapping.profile,"library source belongs to another native owner");
                    anyhow::ensure!(host.source_binding(&mapping.native_source)?==mapping.native_binding && host.source_observation_policy(&mapping.native_source)?==mapping.native_policy,"library source native binding/policy changed");
                }
                let quote=|value:&str|format!("'{}'",value.replace('\'',"''"));
                let locations:Vec<_>=mappings.iter().map(|m|quote(&m.native_source)).collect();
                let predicate=if locations.is_empty(){"0".to_owned()}else{format!("a.location IN ({})",locations.join(","))};
                let query=format!("SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile={} AND {predicate} ORDER BY w.item_id",quote(identity["profile"].as_str().context("missing native profile")?));
                let name=format!("{:x}",Sha256::digest(format!("motion-library:{library}").as_bytes()));
                let saved=host.prepare_selection(&name,&serde_json::json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}))?;
                scopes.push((library,revision,mappings,saved));
            }
            Ok((identity,scopes,custody))
        }).await?;
        for (library, revision, mappings, saved) in scopes {
            let raw = serde_json::to_string(&mappings)?;
            let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
            let catalog = identity["catalog_id"]
                .as_str()
                .context("native catalog identity")?;
            let incarnation = identity["catalog_incarnation"]
                .as_str()
                .context("native incarnation")?;
            let query_id = saved["id"].as_str().context("native query identity")?;
            let query_revision = saved["revision"]
                .as_i64()
                .context("native query revision")?;
            let query_digest = saved["digest"].as_str().context("native query digest")?;
            sqlx::query("INSERT INTO catabolic_server_scopes(profile,library_id,library_revision,catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING")
                .bind(&self.profile).bind(&library).bind(revision).bind(catalog).bind(incarnation).bind(&digest).bind(&raw).bind(query_id).bind(query_revision).bind(query_digest).bind(crate::now()).execute(&mut *custody).await?;
            let actual:(String,String,String,String,i64,String)=sqlx::query_as("SELECT catalog_id,incarnation,source_mappings_json,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=? AND source_digest=?").bind(&self.profile).bind(&library).bind(revision).bind(&digest).fetch_one(&mut *custody).await?;
            anyhow::ensure!(
                actual
                    == (
                        catalog.into(),
                        incarnation.into(),
                        raw,
                        query_id.into(),
                        query_revision,
                        query_digest.into()
                    ),
                "library scope acknowledgment changed"
            );
        }
        custody.commit().await?;
        Ok(())
    }

    /// Read only through trusted saved-query capabilities. Selections come from
    /// the integration scope journal after current Motion authorization, never
    /// from the HTTP body. Pagination stays within one native session; native
    /// cursor strings must not escape into a later, unrelated request session.
    pub(crate) async fn read_items(
        &self,
        conn: &mut sqlx::SqliteConnection,
        selections: Vec<catabolic_store::catalog_scope::TrustedSelection>,
        item: Option<String>,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.read_scoped_records(conn, selections, item, "motion_catalog", false)
            .await
    }

    pub(crate) async fn read_projectable_items(
        &self,
        conn: &mut sqlx::SqliteConnection,
        selections: Vec<catabolic_store::catalog_scope::TrustedSelection>,
        item: Option<String>,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.read_scoped_records(conn, selections, item, "motion_catalog", true)
            .await
    }

    pub(crate) async fn read_libraries(
        &self,
        conn: &mut sqlx::SqliteConnection,
        selections: Vec<catabolic_store::catalog_scope::TrustedSelection>,
        item: Option<String>,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        self.read_scoped_records(conn, selections, item, "motion_library", false)
            .await
    }

    async fn read_scoped_records(
        &self,
        conn: &mut sqlx::SqliteConnection,
        selections: Vec<catabolic_store::catalog_scope::TrustedSelection>,
        item: Option<String>,
        field: &'static str,
        allow_physical: bool,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let path = self.path.clone();
        let profile = self.profile.clone();
        let (identity, mut rows) = self
            .with_host(move |host| {
                use catabolic_store::catalog_scope::{
                    ReadBudget, ScopeReadRequest, ScopedReadSession, TrustedHostReadGrant,
                };
                let identity = host
                    .identity()
                    .map_err(|e| anyhow::anyhow!("native identity: {e:?}"))?;
                let authority = host
                    .authorization()
                    .map_err(|e| anyhow::anyhow!("native authority: {e:?}"))?;
                let mut session = ScopedReadSession::open_expected(
                    &path,
                    identity["catalog_id"]
                        .as_str()
                        .context("missing catalog identity")?,
                    identity["catalog_incarnation"]
                        .as_str()
                        .context("missing catalog incarnation")?,
                )
                .map_err(|e| anyhow::anyhow!("native read owner: {e:?}"))?;
                let budget = || {
                    ReadBudget::new(Duration::from_secs(5), 10_000)
                        .map_err(|e| anyhow::anyhow!("native read budget: {e:?}"))
                };
                let lease = || {
                    authority
                        .acquire(authority.revision())
                        .map_err(|e| anyhow::anyhow!("native read lease: {e:?}"))
                };
                let grant =
                    TrustedHostReadGrant::new(authority.clone(), profile, selections, vec![])
                        .map_err(|e| anyhow::anyhow!("native read grant: {e:?}"))?;
                let cap = session
                    .admit_host_scope(grant, &lease()?, budget()?)
                    .map_err(|e| anyhow::anyhow!("native scope admission: {e:?}"))?;
                let mut cursor = None;
                let mut rows = Vec::new();
                let mut bytes = 0usize;
                loop {
                    let request = match &item {
                        Some(id) => ScopeReadRequest::GetItem(id.clone()),
                        None => ScopeReadRequest::ItemsPage {
                            cursor: cursor.take(),
                            limit: 256,
                        },
                    };
                    let payload = session
                        .read_host_scope(&cap, lease()?, request, budget()?)
                        .map_err(|e| anyhow::Error::new(ReadFailure(e.code)))?
                        .deliver(Clone::clone);
                    let page = payload["result"]["rows"]
                        .as_array()
                        .context("native read rows missing")?;
                    bytes = bytes.saturating_add(serde_json::to_vec(page)?.len());
                    anyhow::ensure!(
                        rows.len() + page.len() <= 10_000 && bytes <= 4 * 1024 * 1024,
                        "native read exceeds bounded result budget"
                    );
                    rows.extend(page.iter().cloned());
                    cursor = payload["next_cursor"].as_str().map(str::to_owned);
                    if item.is_some() || cursor.is_none() {
                        break;
                    }
                }
                let ids: Vec<String> = rows
                    .iter()
                    .map(|row| {
                        row[0]
                            .as_str()
                            .map(str::to_owned)
                            .context("native item ID missing")
                    })
                    .collect::<anyhow::Result<_>>()?;
                let mut allowed_fields = vec!["title".into(), field.into()];
                if allow_physical {
                    allowed_fields.push("motion_discovery".into());
                }
                if field == "motion_catalog" {
                    let mut artwork_present = false;
                    for row in &rows {
                        let metadata: serde_json::Value = serde_json::from_str(
                            row[2]
                                .as_str()
                                .context("native metadata encoding missing")?,
                        )?;
                        if metadata.get("motion_artwork").is_some() {
                            let subject =
                                row[0].as_str().context("native artwork item ID missing")?;
                            let snapshot = host.metadata_contributions(subject)?;
                            bytes = bytes.saturating_add(serde_json::to_vec(&snapshot)?.len());
                            anyhow::ensure!(
                                bytes <= 4 * 1024 * 1024
                                    && snapshot.generation == host.catalog_generation()?
                                    && metadata.as_object().is_some_and(|fields| fields
                                        .iter()
                                        .all(|(key, value)| snapshot.metadata.get(key)
                                            == Some(value))),
                                "native artwork proof changed or exceeds read budget"
                            );
                            artwork::references(&snapshot)?;
                            artwork_present = true;
                        }
                    }
                    if artwork_present {
                        allowed_fields.push("motion_artwork".into());
                    }
                }
                let shapes = host
                    .item_projection_shapes(&ids, &allowed_fields)
                    .map_err(|e| anyhow::anyhow!("native projection shape: {e:?}"))?;
                let items = shapes["items"]
                    .as_array()
                    .context("native projection facts missing")?;
                anyhow::ensure!(
                    items.len() == ids.len()
                        && items
                            .iter()
                            .all(|item| item["requires_rich_projection"] == false
                                || item["logical_work_projection"] == true
                                || (allow_physical && item["physical_work_projection"] == true)),
                    "native item requires richer projection"
                );
                // Wire revisions come from the native item revision owner, never
                // the imported source revision nested in a metadata observation.
                for row in &mut rows {
                    let id = row[0]
                        .as_str()
                        .context("missing scoped item ID")?
                        .to_owned();
                    let revision = items
                        .iter()
                        .find(|shape| shape["id"] == id)
                        .and_then(|shape| shape["item_revision"].as_u64())
                        .context("native item revision missing")?;
                    let mut metadata: serde_json::Value = serde_json::from_str(
                        row[2]
                            .as_str()
                            .context("native metadata encoding missing")?,
                    )?;
                    metadata[field]["catalog_revision"] = serde_json::json!(revision);
                    row[2] = serde_json::Value::String(metadata.to_string());
                    if allow_physical {
                        let shape = items
                            .iter()
                            .find(|shape| shape["id"] == id)
                            .context("native shape missing")?
                            .clone();
                        let values = row.as_array_mut().context("native wire row shape")?;
                        // ItemsPage includes an internal scoped-total column;
                        // GetItem does not. Neither may shift the typed shape.
                        anyhow::ensure!(values.len() >= 3, "native wire row shape");
                        values.truncate(3);
                        values.push(shape);
                    }
                }
                Ok((identity, rows))
            })
            .await?;
        let catalog = identity["catalog_id"]
            .as_str()
            .context("native catalog id missing")?;
        let incarnation = identity["catalog_incarnation"]
            .as_str()
            .context("native incarnation missing")?;
        for row in &mut rows {
            let id = row[0].as_str().context("native row id missing")?;
            let mut metadata: serde_json::Value =
                serde_json::from_str(row[2].as_str().context("native metadata missing")?)?;
            let head = metadata[field]["catalog_revision"]
                .as_i64()
                .context("native head missing")?;
            let wire = motion_catalog_backend::revision_bridge::wire_revision_in(
                &mut *conn,
                id,
                catalog,
                incarnation,
                head,
            )
            .await?;
            metadata[field]["catalog_revision"] = serde_json::json!(wire);
            if field == "motion_library" {
                Self::require_library_update_ack_in(
                    conn,
                    catalog,
                    incarnation,
                    &self.profile,
                    &metadata[field],
                )
                .await?;
            }
            row[2] = serde_json::Value::String(metadata.to_string());
        }
        Ok(rows)
    }

    /// Terminal read transaction step: complete principal-independent captures,
    /// durable token issuance and commit all remain under this native owner.
    /// No token escapes when cancellation or commit fails.
    pub(crate) async fn issue_work_revisions_and_commit(
        &self,
        mut custody: sqlx::Transaction<'static, sqlx::Sqlite>,
        ids: Vec<String>,
    ) -> anyhow::Result<
        std::collections::BTreeMap<
            String,
            playscale_core::catalog_aggregate_revision::IssuedRevision,
        >,
    > {
        anyhow::ensure!(
            ids.len() <= 1000,
            "aggregate response issuance exceeds bound"
        );
        let handle = tokio::runtime::Handle::current();
        self.with_host(move |host| {
            handle.block_on(async move {
                let mut issued = std::collections::BTreeMap::new();
                for id in ids {
                    if !issued.contains_key(&id) {
                        let revision = motion_catalog_backend::aggregate_revision::issue_work_in(
                            &mut custody,
                            host,
                            &id,
                        )
                        .await?;
                        issued.insert(id, revision);
                    }
                }
                custody.commit().await?;
                Ok(issued)
            })
        })
        .await
    }

    /// Revokes admission before waiting for native children and leases. A false
    /// drain result is an error, never a clean-shutdown acknowledgment.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.begin_drain();
        terminal::shutdown_runtime(self.admission.clone()).await
    }
}

/// Diagnostics, not false promises or an all-or-nothing startup requirement.
pub fn unsupported_serving_routes() -> &'static [&'static str] {
    &[
        "relationships/merge/split",
        "edition/timeline/version/order reads and writes",
        "automatic library/source scheduling beyond explicit native scan requests",
        "tags/artwork/matching mutations",
        "playback, download listing/cancellation/manifests, derived content",
        "viewing history/list/continue and organization catalog reference reads",
        "maintenance/search/background catalog writers",
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn projection_refresh_classifies_only_same_identity_forward_generation() {
        let state = ("catalog".into(), "incarnation".into(), 7);
        let identity =
            serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"});
        assert_eq!(
            super::projection_freshness(&state, &identity, 7).unwrap(),
            super::ProjectionFreshness::Current
        );
        assert_eq!(
            super::projection_freshness(&state, &identity, 8).unwrap(),
            super::ProjectionFreshness::Advanced {
                projected: 7,
                current: 8
            }
        );
        assert!(super::projection_freshness(&state, &identity, 6).is_err());
        assert!(
            super::projection_freshness(
                &("catalog".into(), "incarnation".into(), -1),
                &identity,
                0
            )
            .is_err()
        );
        for identity in [
            serde_json::json!({"catalog_id":"other","catalog_incarnation":"incarnation"}),
            serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"other"}),
            serde_json::json!({}),
        ] {
            assert!(super::projection_freshness(&state, &identity, 8).is_err());
        }
    }
    #[test]
    fn native_serving_contract_never_claims_unsupported_families() {
        let missing = super::unsupported_serving_routes();
        assert_eq!(super::SERVING_CONTRACT_VERSION, 1);
        assert!(
            missing.contains(&"playback, download listing/cancellation/manifests, derived content")
        );
        assert!(missing.contains(&"maintenance/search/background catalog writers"));
    }
    async fn fixture() -> (tempfile::TempDir, std::sync::Arc<super::Runtime>) {
        use motion_catalog_backend::{NativeCatalog, initialize, inspect};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("native.sqlite3");
        initialize(path.clone()).await.unwrap();
        let motion_path = root.path().join("motion.sqlite3");
        let db = crate::db::connect_native_bootstrap(&motion_path)
            .await
            .unwrap();
        let backend = NativeCatalog::bind(
            db.clone(),
            path.clone(),
            inspect(path.clone()).await.unwrap().identity,
        )
        .await
        .unwrap();
        backend.freeze_motion_source("empty", 0).await.unwrap();
        backend.capture_motion_import("empty").await.unwrap();
        backend.stage_import("empty").await.unwrap();
        let inventory = backend.motion_reference_inventory("empty").await.unwrap();
        assert!(
            inventory.activation_ready,
            "fresh fixture inventory unresolved: {}",
            serde_json::to_string(&inventory).unwrap()
        );
        backend.prepare_motion_cutover("empty").await.unwrap();
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        drop(backend);
        // Test executable intentionally does not claim packaged observer support.
        let runtime = super::Runtime::open(db, path, "default".into())
            .await
            .unwrap();
        (root, std::sync::Arc::new(runtime))
    }

    #[tokio::test]
    async fn aggregate_tokens_commit_under_retained_owner_and_failed_batch_issues_nothing() {
        let (_root, runtime) = fixture().await;
        let command = runtime
            .prepare_work(
                "aggregate-create".into(),
                "aggregate-work".into(),
                "Original".into(),
                "movie".into(),
                "aggregate-digest".into(),
            )
            .await
            .unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (outcome, mut tx) = runtime.execute_work(command.clone(), tx).await.unwrap();
        motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
            &mut tx,
            "aggregate-work",
            &command,
            &outcome,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let first = runtime
            .issue_work_revisions_and_commit(tx, vec!["aggregate-work".into()])
            .await
            .unwrap()
            .remove("aggregate-work")
            .unwrap();
        assert_eq!(first.revision, 1);
        runtime.with_host(|host|{host.prepare_selection("aggregate-unrelated-query",&serde_json::json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE 0"}}))?;Ok(())}).await.unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let next = runtime
            .issue_work_revisions_and_commit(tx, vec!["aggregate-work".into()])
            .await
            .unwrap()
            .remove("aggregate-work")
            .unwrap();
        assert_eq!(next.revision, first.revision);
        assert!(next.observation_epoch > first.observation_epoch);
        let before: String = sqlx::query_scalar(
            "SELECT state_json FROM catabolic_aggregate_issuance WHERE work_id='aggregate-work'",
        )
        .fetch_one(&runtime.journal)
        .await
        .unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(
            runtime
                .issue_work_revisions_and_commit(
                    tx,
                    vec!["aggregate-work".into(), "missing-work".into()]
                )
                .await
                .is_err()
        );
        let after: String = sqlx::query_scalar(
            "SELECT state_json FROM catabolic_aggregate_issuance WHERE work_id='aggregate-work'",
        )
        .fetch_one(&runtime.journal)
        .await
        .unwrap();
        assert_eq!(
            before, after,
            "a failed response must not commit partial issuance"
        );
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        match runtime
            .guard_work_and_commit(
                tx,
                "aggregate-work".into(),
                999,
                |_, _, _| -> anyhow::Result<()> { panic!("stale aggregate must not dispatch") },
            )
            .await
            .unwrap()
        {
            super::Guarded::Mismatch(current) => {
                assert_eq!(current.revision, 1);
                let stored:String=sqlx::query_scalar("SELECT state_json FROM catabolic_aggregate_issuance WHERE work_id='aggregate-work'").fetch_one(&runtime.journal).await.unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&stored).unwrap()["observation_epoch"],
                    serde_json::json!(current.observation_epoch)
                );
            }
            _ => panic!("wrong aggregate guard outcome"),
        }
        let before_guard: String = sqlx::query_scalar(
            "SELECT state_json FROM catabolic_aggregate_issuance WHERE work_id='aggregate-work'",
        )
        .fetch_one(&runtime.journal)
        .await
        .unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(
            runtime
                .guard_work_and_commit(
                    tx,
                    "aggregate-work".into(),
                    1,
                    |_, _, _| -> anyhow::Result<()> { anyhow::bail!("refused effect") }
                )
                .await
                .is_err()
        );
        let after_guard: String = sqlx::query_scalar(
            "SELECT state_json FROM catabolic_aggregate_issuance WHERE work_id='aggregate-work'",
        )
        .fetch_one(&runtime.journal)
        .await
        .unwrap();
        assert_eq!(
            before_guard, after_guard,
            "failed guarded effect must not expose tentative issuance"
        );
        runtime.shutdown().await.unwrap();
        let reopened = super::Runtime::open(
            runtime.journal.clone(),
            runtime.path.clone(),
            "default".into(),
        )
        .await
        .unwrap();
        let tx = reopened
            .journal
            .begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap();
        let recovered = reopened
            .issue_work_revisions_and_commit(tx, vec!["aggregate-work".into()])
            .await
            .unwrap()
            .remove("aggregate-work")
            .unwrap();
        assert_eq!(recovered.revision, first.revision);
        assert!(recovered.observation_epoch > next.observation_epoch);
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn library_scope_composes_101_and_200_policy_ids_without_native_grant_expansion() {
        use serde_json::json;
        use std::collections::BTreeSet;
        let (_root, runtime) = fixture().await;
        let mut ids = BTreeSet::new();
        for index in 0..200 {
            let id = format!("scope-library-{index:03}");
            let command = runtime
                .prepare_library(
                    format!("scope-create-{index}"),
                    id.clone(),
                    json!({
                        "contract_version":1,"id":id,"name":id,"kind":"mixed","language":"en",
                        "source_ids":[],"catalog_revision":1
                    }),
                )
                .await
                .unwrap();
            let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let (outcome, mut tx) = runtime.execute_work(command.clone(), tx).await.unwrap();
            motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
                &mut tx,
                &format!("motion.library:{id}"),
                &command,
                &outcome,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            ids.insert(id);
        }
        let unrelated = runtime
            .prepare_work(
                "scope-outside-create".into(),
                "outside-work".into(),
                "Outside every empty library".into(),
                "movie".into(),
                "scope-outside-digest".into(),
            )
            .await
            .unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (outcome, mut tx) = runtime.execute_work(unrelated.clone(), tx).await.unwrap();
        motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
            &mut tx,
            "outside-work",
            &unrelated,
            &outcome,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        runtime.prepare_library_scopes().await.unwrap();
        for count in [101, 200] {
            let allowed = ids.iter().take(count).cloned().collect();
            let scope = runtime.prepare_library_read_scope(allowed).await.unwrap();
            runtime.rebuild_projection().await.unwrap();
            let mut tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
            runtime.validate_projection(&mut tx).await.unwrap();
            let definitions = runtime
                .validate_library_read_scope(&mut tx, &scope)
                .await
                .unwrap();
            assert_eq!(definitions.len(), count, "no allowed library is truncated");
            // Empty physical memberships must remain empty, never degrade to All.
            let works = runtime
                .read_projectable_items(&mut tx, vec![scope.selection], None)
                .await
                .unwrap();
            assert!(works.is_empty());
            tx.commit().await.unwrap();
        }
        ids.insert("scope-library-over-limit".into());
        assert!(runtime.prepare_library_read_scope(ids).await.is_err());
    }

    #[tokio::test]
    async fn restart_reconciles_committed_create_without_host_ack_and_never_dispatches_pending() {
        use catabolic_store::catalog_commands::CommandOutcome;
        let (_root, runtime) = fixture().await;
        let committed = runtime
            .prepare_work(
                "recovery-created".into(),
                "recovered-work".into(),
                "Recovered".into(),
                "movie".into(),
                "recovery-digest".into(),
            )
            .await
            .unwrap();
        // Advance an unrelated native owner after the durable original intent.
        runtime.with_host(|host| {
            host.prepare_selection("retry-generation-advance", &serde_json::json!({"version":1,"mode":"selection","selection":{"language":"sql","query":"SELECT item_id FROM catalog_items WHERE 0"}}))?;
            Ok(())
        }).await.unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (refusal, witness, mut tx) = runtime
            .execute_work_observed(committed.clone(), tx)
            .await
            .unwrap();
        assert!(matches!(refusal, CommandOutcome::StaleGeneration));
        let successor = motion_catalog_backend::command_attempts::record_next_in(
            &mut tx,
            &committed,
            &committed,
            witness.expect("actual older-generation witness"),
        )
        .await
        .unwrap();
        assert_ne!(successor.key(), committed.key());
        assert_eq!(successor.payload(), committed.payload());
        tx.commit().await.unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let owner = runtime.clone();
        let committed = successor;
        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(async move {
            let (outcome, witness, tx) = owner
                .execute_work_observed(committed.clone(), tx)
                .await
                .unwrap();
            assert!(matches!(outcome, CommandOutcome::Committed { .. }));
            assert!(witness.is_none());
            let (replay, witness, tx) = owner.execute_work_observed(committed, tx).await.unwrap();
            assert!(matches!(replay, CommandOutcome::Replay { .. }));
            assert!(witness.is_none(), "a replay must never authorize a new key");
            committed_tx.send(()).unwrap();
            // Keep the real writer transaction in the HTTP task until the
            // test cancels precisely after native commit and before host ack.
            std::future::pending::<()>().await;
            drop(tx);
        });
        committed_rx.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let pending = runtime
            .prepare_work(
                "recovery-pending".into(),
                "pending-work".into(),
                "Pending".into(),
                "movie".into(),
                "pending-digest".into(),
            )
            .await
            .unwrap();
        let future = catabolic_core::catalog_command::CommandEnvelope::new(
            1,
            catabolic_core::catalog_command::CommandKey::new(
                pending.key().catalog_id(),
                pending.key().host_namespace(),
                "future-generation-refusal",
            )
            .unwrap(),
            pending.admission_incarnation(),
            pending.expected_generation() + 100,
            &pending.payload().to_string(),
        )
        .unwrap();
        let custody = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (future_outcome, witness, custody) = runtime
            .execute_work_observed(future, custody)
            .await
            .unwrap();
        assert!(matches!(future_outcome, CommandOutcome::StaleGeneration));
        assert!(
            witness.is_none(),
            "future expected generations cannot be superseded"
        );
        custody.commit().await.unwrap();
        let db = runtime.journal.clone();
        let path = runtime.path.clone();
        runtime.shutdown().await.unwrap();
        drop(runtime);
        let reopened = super::Runtime::open(db.clone(), path, "default".into())
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE id='recovered-work'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let absent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE id='pending-work'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(absent, 0);
        reopened
            .with_host(move |host| {
                assert!(host.reconcile_command_receipt(&pending)?.is_none());
                Ok(())
            })
            .await
            .unwrap();
        reopened.require_serving_routes().await.unwrap();
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_draining_refuses_queued_clones_without_interrupting_owned_cleanup() {
        let (_root, runtime) = fixture().await;
        let active_runtime = runtime.clone();
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let active = tokio::spawn(async move {
            active_runtime
                .with_host(move |host| {
                    let _ = entered.send(());
                    release_rx.recv()?;
                    // Existing closure still owns a usable native host after drain.
                    host.identity()?;
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();
        let queued_runtime = runtime.clone();
        let reached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let action_reached = reached.clone();
        let mut queued = Box::pin(queued_runtime.with_host(move |_| {
            action_reached.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }));
        // Poll the actual with_host future through enqueue while host is held.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), &mut queued)
                .await
                .is_err()
        );
        runtime.begin_drain();
        runtime.begin_drain();
        let closed = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runtime.with_host(|_| Ok(())),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(
            closed
                .downcast_ref::<super::OrdinaryAdmissionDraining>()
                .is_some()
        );
        assert!(!active.is_finished());
        release.send(()).unwrap();
        active.await.unwrap().unwrap();
        let closed = queued.await.unwrap_err();
        assert!(
            closed
                .downcast_ref::<super::OrdinaryAdmissionDraining>()
                .is_some()
        );
        assert!(!reached.load(std::sync::atomic::Ordering::Acquire));
        let terminal = runtime.close_admission_for_stopped_service().await.unwrap();
        terminal.close().await.unwrap();
        assert!(runtime.with_host(|_| Ok(())).await.is_err());
    }

    #[tokio::test]
    async fn failed_native_idle_validation_still_refuses_retained_clones() {
        let (_root, runtime) = fixture().await;
        let retained = runtime.clone();
        runtime
            .with_host(|host| {
                host.begin_drain();
                Ok(())
            })
            .await
            .unwrap();
        assert!(runtime.close_admission_for_stopped_service().await.is_err());
        assert!(retained.with_host(|_| Ok(())).await.is_err());
        assert!(runtime.close_admission_for_stopped_service().await.is_err());
        runtime.shutdown().await.unwrap();
        assert!(retained.with_host(|_| Ok(())).await.is_err());
    }

    #[tokio::test]
    async fn terminal_runtime_refuses_retained_clone_and_export_failure_cannot_reopen() {
        let (root, runtime) = fixture().await;
        let retained = runtime.clone();
        let mut terminal = runtime.close_admission_for_stopped_service().await.unwrap();
        assert!(retained.with_host(|_| Ok(())).await.is_err());
        assert!(runtime.close_admission_for_stopped_service().await.is_err());
        let absent = root.path().join("absent-export-parent");
        assert!(
            terminal
                .with_export(
                    absent,
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    |_, _| Ok(())
                )
                .await
                .is_err()
        );
        assert!(retained.with_host(|_| Ok(())).await.is_err());
        terminal.close().await.unwrap();
        assert!(retained.with_host(|_| Ok(())).await.is_err());
    }

    #[tokio::test]
    async fn native_runtime_open_never_grants_unverified_executable_scan_authority() {
        let (_root, runtime) = fixture().await;
        runtime.require_serving_routes().await.unwrap();
        runtime
            .with_host(|host| {
                let error = match host.start_scan("unregistered", ()) {
                    Ok(_) => anyhow::bail!("unregistered executable admitted a scan"),
                    Err(error) => error,
                };
                assert_eq!(error.code, catabolic_runtime::host::ErrorCode::MissingTool);
                assert!(host.worker_inventory()?.admits_new_work());
                Ok(())
            })
            .await
            .unwrap();
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_serving_admission_rejects_stale_projection_until_rebuilt() {
        let (_root, runtime) = fixture().await;
        runtime.require_serving_routes().await.unwrap();
        runtime.prepare_all_scope().await.unwrap();
        assert!(runtime.require_serving_routes().await.is_err());
        runtime.rebuild_projection().await.unwrap();
        runtime.require_serving_routes().await.unwrap();
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn projection_waits_for_motion_before_taking_native_owner() {
        use std::time::Duration;
        let (_root, runtime) = fixture().await;
        // Model an authenticated read: Motion's revocation fence is already
        // held, but it has not yet acquired the native owner.
        let custody = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let worker = runtime.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let rebuild = tokio::spawn(async move {
            let future = worker.rebuild_projection();
            tokio::pin!(future);
            tokio::select! {
                biased;
                result = &mut future => panic!("projection escaped Motion fence: {result:?}"),
                _ = tokio::time::sleep(Duration::from_millis(75)) => { started_tx.send(()).unwrap(); }
            }
            future.await
        });
        started_rx.await.unwrap();
        // If rebuild acquired native first this read would wait behind its
        // SQLite lock timeout, despite holding the very fence it needs.
        tokio::time::timeout(Duration::from_millis(500), runtime.identity())
            .await
            .expect("projection inverted Motion/native lock order")
            .unwrap();
        custody.commit().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), rebuild)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_queued_native_dispatch_retains_real_revocation_reservation() {
        use std::time::Duration;
        let (_root, runtime) = fixture().await;
        sqlx::query("INSERT INTO access_users(id,name,revision,disabled,profile_ids,permissions,policy,created_at) VALUES('writer','Writer',1,0,'[]','[]','{}',0)")
            .execute(&runtime.journal).await.unwrap();
        let command = runtime
            .prepare_work(
                "create-cancelled".into(),
                "created-work".into(),
                "Created".into(),
                "movie".into(),
                "request-digest".into(),
            )
            .await
            .unwrap();
        let owner = runtime.admission.clone();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = tokio::task::spawn_blocking(move || {
            let _guard = owner.lock().unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.await.unwrap();
        let mut custody: sqlx::Transaction<'static, sqlx::Sqlite> =
            runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let disabled: bool =
            sqlx::query_scalar("SELECT disabled FROM access_users WHERE id='writer'")
                .fetch_one(&mut *custody)
                .await
                .unwrap();
        assert!(!disabled);
        let worker = runtime.clone();
        let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(async move {
            let dispatch = worker.execute_work(command, custody);
            tokio::pin!(dispatch);
            tokio::select! {
                biased;
                result = &mut dispatch => { panic!("dispatch escaped held native owner: {result:?}"); },
                _ = tokio::time::sleep(Duration::from_millis(30)) => { queued_tx.send(()).unwrap(); }
            }
            dispatch.await
        });
        queued_rx.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        let db = runtime.journal.clone();
        let mut revocation = tokio::spawn(async move {
            sqlx::query("UPDATE access_users SET disabled=1,revision=revision+1 WHERE id='writer'")
                .execute(&db)
                .await
                .unwrap();
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(75), &mut revocation)
                .await
                .is_err(),
            "revocation committed while queued native effect retained authorization"
        );
        release_tx.send(()).unwrap();
        holder.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), revocation)
            .await
            .unwrap()
            .unwrap();
        let exists = runtime
            .with_host(|host| Ok(host.observe_item_for_create("created-work").unwrap().0))
            .await
            .unwrap();
        assert!(
            exists,
            "already authorized dispatch may finish before revocation"
        );
        let disabled: bool =
            sqlx::query_scalar("SELECT disabled FROM access_users WHERE id='writer'")
                .fetch_one(&runtime.journal)
                .await
                .unwrap();
        assert!(disabled);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn library_replacement_recovers_only_committed_receipt_before_scope_ack() {
        use catabolic_store::catalog_commands::CommandOutcome;
        use serde_json::json;
        let (_root, runtime) = fixture().await;
        let id = "recovery-library";
        let original = json!({"contract_version":1,"id":id,"name":"Original","kind":"mixed","language":"en","source_ids":[],"catalog_revision":1});
        let creation = runtime
            .prepare_library(
                "library-recovery-create".into(),
                id.into(),
                original.clone(),
            )
            .await
            .unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (outcome, mut tx) = runtime.execute_work(creation.clone(), tx).await.unwrap();
        motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
            &mut tx,
            &format!("motion.library:{id}"),
            &creation,
            &outcome,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        runtime.prepare_library_scopes().await.unwrap();
        let mut changed = original.clone();
        changed["name"] = json!("Changed");
        changed["catalog_revision"] = json!(2);
        changed["replacement_operation"] = json!("library-replace:recovery");
        let command = runtime
            .prepare_library_update(
                "library-replace:recovery".into(),
                changed.clone(),
                1,
                "exact-request".into(),
                vec![],
            )
            .await
            .unwrap();
        let tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let sent = command.clone();
        let (outcome, tx) = runtime
            .with_host(move |host| {
                let (outcome, witness) =
                    super::Runtime::execute_library_update_with_host(host, &sent)?;
                anyhow::ensure!(witness.is_none(), "unexpected generation drift");
                Ok((outcome, tx))
            })
            .await
            .unwrap();
        assert!(matches!(outcome, CommandOutcome::Committed { .. }));
        // Actual native commit, then real host transaction rollback before ACK.
        drop(tx);
        let grant = runtime.prepare_library_definitions(None).await.unwrap();
        let mut tx = runtime.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(
            runtime
                .read_libraries(&mut tx, vec![grant], None)
                .await
                .is_err(),
            "unacknowledged replacement cannot grant its current definition"
        );
        tx.rollback().await.unwrap();
        runtime.shutdown().await.unwrap();
        let reopened = super::Runtime::open(
            runtime.journal.clone(),
            runtime.path.clone(),
            "default".into(),
        )
        .await
        .unwrap();
        let grant = reopened.prepare_library_definitions(None).await.unwrap();
        let mut tx = reopened
            .journal
            .begin_with("BEGIN IMMEDIATE")
            .await
            .unwrap();
        let rows = reopened
            .read_libraries(&mut tx, vec![grant], None)
            .await
            .unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(rows[0][2].as_str().unwrap()).unwrap();
        assert_eq!(metadata["motion_library"], changed);
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM catabolic_server_scopes WHERE library_id=?")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(count, 2, "history and new receipt ACK must both survive");
        tx.commit().await.unwrap();
        let mut pending = changed.clone();
        pending["name"] = json!("Never dispatched");
        pending["catalog_revision"] = json!(3);
        pending["replacement_operation"] = json!("library-replace:pending");
        reopened
            .prepare_library_update(
                "library-replace:pending".into(),
                pending,
                2,
                "pending-request".into(),
                vec![],
            )
            .await
            .unwrap();
        reopened.shutdown().await.unwrap();
        let again = super::Runtime::open(
            runtime.journal.clone(),
            runtime.path.clone(),
            "default".into(),
        )
        .await
        .unwrap();
        let grant = again.prepare_library_definitions(None).await.unwrap();
        let mut tx = again.journal.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let rows = again
            .read_libraries(&mut tx, vec![grant], None)
            .await
            .unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(rows[0][2].as_str().unwrap()).unwrap();
        assert_eq!(
            metadata["motion_library"], changed,
            "startup must not execute a pending HTTP replacement"
        );
        tx.commit().await.unwrap();
        again.shutdown().await.unwrap();
    }
}
