//! Native Catabolic operator adapter. This is not an application-user capability.
//! Public routes must compose Motion authorization and Catabolic scoped grants
//! before using a backend. No automatic cutover or legacy fallback occurs here.
//!
//! Motion commits immutable command intents before dispatch. Catabolic atomically
//! commits mutations and receipts; retry always uses the recorded envelope. There
//! is deliberately no claim of a transaction spanning the two databases.
pub mod migration;
pub mod motion_export;
pub mod source_freeze;

use anyhow::{Context, bail};
use catabolic_core::{
    catalog_command::{CommandEnvelope, CommandKey, ReceiptVisibility},
    catalog_host_lease::HostAuthorization,
};
use catabolic_store::{
    Store,
    catalog_commands::{self, CommandOutcome, ReceiptPolicy, TrustedCommandAuthority},
};
use playscale_core::catalog_backend::Identity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

pub const CATABOLIC_REVISION: &str = "6969f08c5d9b207afe8eec819555759be136644d";
const HOST_NAMESPACE: &str = "motion.catalog.operator.v1";

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub identity: Identity,
    pub generation: i64,
    pub sqlite_source_id: String,
}

fn status(store: &Store) -> anyhow::Result<Status> {
    let identity = Identity {
        catalog_id: store.database_id.clone(),
        incarnation: store
            .catalog_incarnation
            .clone()
            .context("native catalog required")?,
        schema_version: store.schema_version,
    };
    identity
        .validate()
        .map_err(|e| anyhow::anyhow!("catalog admission: {e:?}"))?;
    Ok(Status {
        identity,
        generation: store.db.query_row(
            "SELECT generation FROM catalog_state WHERE id=1",
            [],
            |r| r.get(0),
        )?,
        sqlite_source_id: store
            .db
            .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))?,
    })
}

/// Read-only inspection never initializes, migrates or repairs a catalog.
pub async fn inspect(path: PathBuf) -> anyhow::Result<Status> {
    tokio::task::spawn_blocking(move || status(&Store::open(&path, false, false)?)).await?
}

/// Explicit initialization only; Catabolic refuses an existing destination.
pub async fn initialize(path: PathBuf) -> anyhow::Result<Value> {
    tokio::task::spawn_blocking(move || Ok(catabolic_store::migration::initialize_native(&path)?))
        .await?
}

/// An admitted operator backend, serialized independently of async request tasks.
/// Dropping a request cannot release its worker permit or authorization lease
/// while a native operation is still running.
#[derive(Clone)]
pub struct NativeCatalog {
    path: Arc<PathBuf>,
    identity: Identity,
    authorization: HostAuthorization,
    worker: Arc<Semaphore>,
    journal: SqlitePool,
}
impl NativeCatalog {
    /// Bind explicitly to an inspected identity. Existing durable bindings are
    /// compared, never replaced. Both stores must already exist and be migrated.
    pub async fn bind(
        journal: SqlitePool,
        path: PathBuf,
        expected: Identity,
    ) -> anyhow::Result<Self> {
        expected
            .admit(&inspect(path.clone()).await?.identity)
            .map_err(|e| anyhow::anyhow!("catalog admission: {e:?}"))?;
        let encoded = serde_json::to_string(&expected)?;
        let mut tx = journal.begin().await?;
        sqlx::query("INSERT INTO catabolic_binding(id,identity_json) VALUES(1,?) ON CONFLICT(id) DO NOTHING")
            .bind(&encoded).execute(&mut *tx).await?;
        let existing: String =
            sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
                .fetch_one(&mut *tx)
                .await?;
        let identity: Identity = serde_json::from_str(&existing)?;
        identity
            .admit(&expected)
            .map_err(|e| anyhow::anyhow!("persisted catalog admission: {e:?}"))?;
        tx.commit().await?;
        let authorization = HostAuthorization::new(HOST_NAMESPACE.into(), new_gate_revision())
            .map_err(|e| anyhow::anyhow!("host authorization: {e:?}"))?;
        Ok(Self {
            path: Arc::new(path),
            identity,
            authorization,
            worker: Arc::new(Semaphore::new(1)),
            journal,
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    async fn with_store<T: Send + 'static>(
        &self,
        writable: bool,
        action: impl FnOnce(&Store) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let permit = self.worker.clone().acquire_owned().await?;
        let lease = self
            .authorization
            .acquire(self.authorization.revision())
            .map_err(|e| anyhow::anyhow!("host authorization: {e:?}"))?;
        let path = self.path.clone();
        let expected = self.identity.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _lease = lease;
            let store = Store::open(&path, writable, false)?;
            // Same opened owner for identity validation and the operation.
            expected
                .admit(&status(&store)?.identity)
                .map_err(|e| anyhow::anyhow!("catalog admission: {e:?}"))?;
            action(&store)
        })
        .await?
    }

    pub async fn status(&self) -> anyhow::Result<Status> {
        self.with_store(false, status).await
    }

    /// Prepare an operator-owned item batch. This does not dispatch a mutation.
    /// Existing operation IDs reuse their original generation and payload.
    pub async fn prepare_items(&self, operation_id: &str, items: Vec<Value>) -> anyhow::Result<()> {
        let payload = json!({"operation":"item_put_batch", "items":items});
        self.prepare_command(operation_id, payload).await
    }

    async fn prepare_command(&self, operation_id: &str, payload: Value) -> anyhow::Result<()> {
        let key = CommandKey::new(&self.identity.catalog_id, HOST_NAMESPACE, operation_id)?;
        // Validate/canonicalize even when an existing key will be replayed.
        let requested = CommandEnvelope::new(
            1,
            key.clone(),
            &self.identity.incarnation,
            0,
            &payload.to_string(),
        )?;
        if let Some(existing) = self.recorded(operation_id).await? {
            ensure_same_request(&existing, &requested)?;
            return Ok(());
        }
        let snapshot = self.status().await?;
        let envelope = CommandEnvelope::new(
            1,
            key,
            &self.identity.incarnation,
            snapshot.generation,
            &payload.to_string(),
        )?;
        let encoded = envelope.canonical_payload().to_owned();
        // Conflict-safe insert. No Catabolic effect can precede this commit.
        let mut tx = self.journal.begin().await?;
        sqlx::query("INSERT INTO catabolic_commands(operation_id,envelope_json,created_at) VALUES(?,?,?) ON CONFLICT(operation_id) DO NOTHING")
            .bind(operation_id).bind(encoded).bind(now()).execute(&mut *tx).await?;
        let recorded: String =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(operation_id)
                .fetch_one(&mut *tx)
                .await?;
        ensure_same_request(&decode_envelope(&recorded)?, &requested)?;
        tx.commit().await?;
        Ok(())
    }

    async fn recorded(&self, operation_id: &str) -> anyhow::Result<Option<CommandEnvelope>> {
        let encoded: Option<String> =
            sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
                .bind(operation_id)
                .fetch_optional(&self.journal)
                .await?;
        encoded.map(|s| decode_envelope(&s)).transpose()
    }

    fn validate_recorded_key(
        &self,
        operation_id: &str,
        envelope: &CommandEnvelope,
    ) -> anyhow::Result<()> {
        if envelope.key().operation_id() != operation_id
            || envelope.key().catalog_id() != self.identity.catalog_id
            || envelope.key().host_namespace() != HOST_NAMESPACE
            || envelope.admission_incarnation() != self.identity.incarnation
        {
            bail!("recorded command belongs to another catalog or incarnation");
        }
        Ok(())
    }

    /// Replay/execute only an intent already persisted by this host. Outcomes
    /// such as stale generation, expired receipts and restore uncertainty remain
    /// explicit; they never trigger regeneration with a new operation ID.
    pub async fn dispatch(&self, operation_id: &str) -> anyhow::Result<CommandOutcome> {
        let envelope = self
            .recorded(operation_id)
            .await?
            .context("unknown catalog command")?;
        self.validate_recorded_key(operation_id, &envelope)?;
        self.with_store(true, move |store| {
            Ok(catalog_commands::execute(
                store,
                &envelope,
                TrustedCommandAuthority {
                    host_namespace: HOST_NAMESPACE,
                    receipt_visibility: ReceiptVisibility::FullResult,
                    allow_new_commands: true,
                    identity_authority: None,
                },
                receipt_policy(),
            )?)
        })
        .await
    }

    /// Close new admission and wait for native calls, including detached async
    /// callers. A timeout keeps the gate closed; it does not reopen or kill work.
    pub async fn drain(&self, timeout: Duration) -> anyhow::Result<()> {
        let revocation = self
            .authorization
            .begin_revocation()
            .map_err(|e| anyhow::anyhow!("host revocation: {e:?}"))?;
        tokio::task::spawn_blocking(move || {
            revocation
                .wait_drained(timeout)
                .map_err(|e| anyhow::anyhow!("host drain: {e:?}"))
        })
        .await?
    }
}
fn new_gate_revision() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn ensure_same_request(
    existing: &CommandEnvelope,
    requested: &CommandEnvelope,
) -> anyhow::Result<()> {
    if existing.key() != requested.key()
        || existing.admission_incarnation() != requested.admission_incarnation()
        || catabolic_core::encode_with(existing.payload(), false, true)
            != catabolic_core::encode_with(requested.payload(), false, true)
    {
        bail!("catalog operation ID is already bound to a different request");
    }
    Ok(())
}

/// Canonicalize the parent without following/replacing the destination file.
pub fn destination(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    Ok(absolute
        .parent()
        .context("catalog parent missing")?
        .canonicalize()?
        .join(absolute.file_name().context("catalog filename missing")?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedEnvelope {
    contract_version: u32,
    canonicalization_version: u32,
    key: CommandKey,
    admission_incarnation: String,
    expected_generation: i64,
    payload: Value,
}
fn decode_envelope(encoded: &str) -> anyhow::Result<CommandEnvelope> {
    let saved: RecordedEnvelope = serde_json::from_str(encoded)?;
    if saved.canonicalization_version
        != catabolic_core::catalog_command::COMMAND_CANONICALIZATION_VERSION
    {
        bail!("unsupported journal canonicalization");
    }
    let envelope = CommandEnvelope::new(
        saved.contract_version,
        saved.key,
        saved.admission_incarnation,
        saved.expected_generation,
        &saved.payload.to_string(),
    )?;
    if envelope.canonical_payload() != encoded {
        bail!("noncanonical catalog intent");
    }
    Ok(envelope)
}

fn receipt_policy() -> ReceiptPolicy {
    ReceiptPolicy {
        retention: Duration::from_secs(30 * 24 * 60 * 60),
        max_receipts: 100_000,
        max_logical_bytes: 256 * 1024 * 1024,
        max_hold_records: 100_000,
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
async fn test_journal(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(options)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../../migrations/0066_catabolic_command_journal.sql"
    ))
    .execute(&pool)
    .await?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn aborting_async_caller_retains_native_permit_and_lease_until_exit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let path = root.join("native.sqlite3");
        initialize(path.clone()).await.unwrap();
        let journal = test_journal(&root.join("motion.sqlite3")).await.unwrap();
        let backend = NativeCatalog::bind(
            journal.clone(),
            path.clone(),
            inspect(path).await.unwrap().identity,
        )
        .await
        .unwrap();
        let (started, began) = tokio::sync::oneshot::channel();
        let (release, waiting) = std::sync::mpsc::channel();
        let clone = backend.clone();
        let task = tokio::spawn(async move {
            clone
                .with_store(false, move |_| {
                    started.send(()).unwrap();
                    waiting.recv_timeout(Duration::from_secs(10)).unwrap();
                    Ok(())
                })
                .await
        });
        began.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(backend.worker.available_permits(), 0);
        assert!(backend.drain(Duration::ZERO).await.is_err());
        release.send(()).unwrap();
        backend.drain(Duration::from_secs(5)).await.unwrap();
        assert!(backend.status().await.is_err());
        journal.close().await;
    }
}
