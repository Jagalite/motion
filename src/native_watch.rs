//! Internal-only authorized watch composition. No route or main worker invokes
//! this owner. Native observation_sources is the sole dirty-generation journal.
use crate::{App, v2::auth::Caller};
use anyhow::{Context, ensure};
use catabolic_runtime::source_events::{
    PersistedSourceEvents, SourceEventBatch, SourceEventMonitor, SourceEventScope,
};
use motion_catalog_backend::sources::SourceMapping;
use playscale_core::{
    access::{self, Permission, Principal},
    source_lifecycle::LifecycleProof,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Definition {
    pub id: String,
    pub revision: i64,
    pub catalog_id: String,
    pub incarnation: String,
    pub library_id: String,
    pub source_id: String,
    pub enabled: bool,
    pub tombstoned: bool,
    pub requester_id: String,
    pub credential_hash: Option<String>,
    pub ingress_id: Option<String>,
    pub lifecycle: LifecycleProof,
}
#[derive(Debug)]
struct AccessDecision(access::AccessError);
impl std::fmt::Display for AccessDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "watch access decision: {:?}", self.0)
    }
}
impl std::error::Error for AccessDecision {}
#[derive(Debug)]
struct TransportDecision(crate::v2::Problem);
impl std::fmt::Display for TransportDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "watch admission: {} ({})", self.0.code, self.0.status)
    }
}
impl std::error::Error for TransportDecision {}
fn scope_ids(p: &Principal) -> Option<Vec<String>> {
    match access::catalog_scope(p) {
        access::CatalogScope::All => None,
        access::CatalogScope::Libraries(ids) => Some(ids.into_iter().collect()),
        access::CatalogScope::Nothing => Some(vec![]),
    }
}
async fn load_in(db: &mut SqliteConnection, id: &str) -> anyhow::Result<Option<Definition>> {
    let bytes:Option<i64>=sqlx::query_scalar("SELECT length(CAST(definition_json AS BLOB)) FROM catabolic_source_watch_definitions WHERE id=? ORDER BY revision DESC LIMIT 1").bind(id).fetch_optional(&mut *db).await?;
    let Some(bytes) = bytes else { return Ok(None) };
    ensure!(
        (0..=65536).contains(&bytes),
        "watch definition exceeds byte bound"
    );
    let raw:String=sqlx::query_scalar("SELECT definition_json FROM catabolic_source_watch_definitions WHERE id=? ORDER BY revision DESC LIMIT 1").bind(id).fetch_one(db).await?;
    let d: Definition = serde_json::from_str(&raw)?;
    ensure!(
        d.id == id && d.revision > 0 && (!d.tombstoned || !d.enabled),
        "invalid watch definition"
    );
    Ok(Some(d))
}
async fn insert_in(db: &mut SqliteConnection, d: &Definition) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO catabolic_source_watch_definitions(id,revision,catalog_id,incarnation,origin,library_id,source_id,enabled,tombstoned,definition_json,created_at) VALUES(?,?,?,?,'authorized_create',?,?,?,?,?,?)")
        .bind(&d.id).bind(d.revision).bind(&d.catalog_id).bind(&d.incarnation).bind(&d.library_id).bind(&d.source_id).bind(d.enabled).bind(d.tombstoned).bind(serde_json::to_string(d)?).bind(crate::now()).execute(db).await?;
    Ok(())
}
async fn current_actor(db: &mut SqliteConnection, d: &Definition) -> anyhow::Result<Principal> {
    let p = crate::metadata_jobs::principal(
        db,
        &d.requester_id,
        d.credential_hash.as_deref(),
        d.ingress_id.as_deref(),
        Permission::SourcesManage,
    )
    .await
    .map_err(TransportDecision)?;
    ensure!(
        p.id == d.requester_id && access::catalog_scope(&p).library(&d.library_id),
        "watch requester or scope changed"
    );
    Ok(p)
}
/// Caller identity is a real resolved authentication proof, never saved JSON.
/// Stable id + expected revision give the internal caller a CAS boundary; a
/// future HTTP adapter must additionally own its usual idempotency receipt.
pub(crate) async fn configure(
    app: &App,
    caller: &Caller,
    id: &str,
    expected: Option<u64>,
    library: &str,
    source: &str,
) -> anyhow::Result<Definition> {
    caller
        .require(Permission::SourcesManage)
        .map_err(TransportDecision)?;
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "invalid watch identity"
    );
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native watch owner missing")?;
    let selection = runtime
        .prepare_library_definitions(scope_ids(&caller.principal))
        .await?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await
        .map_err(TransportDecision)?;
    ensure!(
        access::catalog_scope(&p) == access::catalog_scope(&caller.principal)
            && access::catalog_scope(&p).library(library),
        "watch principal changed during preparation"
    );
    let (_, sources) = crate::native_catalog::scan_sources_in(
        app,
        &mut tx,
        library,
        selection,
        Some(&[source.to_owned()]),
    )
    .await
    .map_err(TransportDecision)?;
    ensure!(sources.len() == 1, "watch requires one exact source");
    let lifecycle =
        motion_catalog_backend::source_lifecycle::require_active_in(&mut tx, source).await?;
    ensure!(
        lifecycle.mapping_digest
            == motion_catalog_backend::source_lifecycle::mapping_digest(&sources[0])?,
        "watch mapping differs from lifecycle"
    );
    let revision = match (load_in(&mut tx, id).await?, expected) {
        (None, None) => 1,
        (Some(old), Some(expected)) => {
            ensure!(
                !old.tombstoned && access::catalog_scope(&p).library(&old.library_id),
                "watch unavailable"
            );
            let access::Revised::Next(next) =
                access::revise(old.revision.try_into()?, expected, true).map_err(AccessDecision)?
            else {
                unreachable!()
            };
            i64::try_from(next)?
        }
        _ => return Err(AccessDecision(access::AccessError::StaleRevision).into()),
    };
    let d = Definition {
        id: id.into(),
        revision,
        catalog_id: lifecycle.catalog_id.clone(),
        incarnation: lifecycle.incarnation.clone(),
        library_id: library.into(),
        source_id: source.into(),
        enabled: true,
        tombstoned: false,
        requester_id: p.id.clone(),
        credential_hash: caller.credential().map(|(hash, _)| hash.to_owned()),
        ingress_id: caller.ingress_device_id().map(str::to_owned),
        lifecycle,
    };
    current_actor(&mut tx, &d).await?;
    insert_in(&mut tx, &d).await?;
    tx.commit().await?;
    Ok(d)
}
/// Revoking a watch never requires the source to remain physically available.
pub(crate) async fn disable(
    app: &App,
    caller: &Caller,
    id: &str,
    expected: u64,
    tombstone: bool,
) -> anyhow::Result<()> {
    caller
        .require(Permission::SourcesManage)
        .map_err(TransportDecision)?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await
        .map_err(TransportDecision)?;
    let mut d = load_in(&mut tx, id).await?.context("watch not found")?;
    ensure!(
        !d.tombstoned && access::catalog_scope(&p).library(&d.library_id),
        "watch unavailable"
    );
    let access::Revised::Next(next) =
        access::revise(d.revision.try_into()?, expected, true).map_err(AccessDecision)?
    else {
        unreachable!()
    };
    d.revision = next.try_into()?;
    d.enabled = false;
    d.tombstoned = tombstone;
    insert_in(&mut tx, &d).await?;
    tx.commit().await?;
    Ok(())
}
struct Prepared {
    definition: Definition,
    principal: Principal,
    proof: Value,
    mapping: SourceMapping,
    tx: sqlx::Transaction<'static, sqlx::Sqlite>,
}
async fn prepare(app: &App, id: &str) -> anyhow::Result<Prepared> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native watch owner missing")?;
    let mut read = app.db.begin().await?;
    let original = load_in(&mut read, id).await?.context("watch not found")?;
    let initial = current_actor(&mut read, &original).await?;
    read.commit().await?;
    let selection = runtime
        .prepare_library_definitions(scope_ids(&initial))
        .await?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let current = load_in(&mut tx, id).await?.context("watch disappeared")?;
    ensure!(
        current == original && current.enabled && !current.tombstoned,
        "watch changed or disabled"
    );
    let p = current_actor(&mut tx, &current).await?;
    ensure!(p == initial, "watch actor changed during scope preparation");
    let lifecycle =
        motion_catalog_backend::source_lifecycle::require_active_in(&mut tx, &current.source_id)
            .await?;
    ensure!(
        lifecycle == current.lifecycle,
        "watch source lifecycle changed"
    );
    let (mut proof, mut sources) = crate::native_catalog::scan_sources_in(
        app,
        &mut tx,
        &current.library_id,
        selection,
        Some(&[current.source_id.clone()]),
    )
    .await
    .map_err(TransportDecision)?;
    ensure!(
        sources.len() == 1
            && proof["catalog_id"] == current.catalog_id
            && proof["incarnation"] == current.incarnation,
        "watch catalog/source identity changed"
    );
    let mapping = sources.remove(0);
    ensure!(
        motion_catalog_backend::source_lifecycle::mapping_digest(&mapping)?
            == current.lifecycle.mapping_digest,
        "watch binding changed"
    );
    proof["accepted_principal"] = serde_json::to_value(&p)?;
    Ok(Prepared {
        definition: current,
        principal: p,
        proof,
        mapping,
        tx,
    })
}
pub(crate) struct Monitor {
    definition: Definition,
    scope: SourceEventScope,
    monitor: SourceEventMonitor,
}
pub(crate) struct Batch {
    definition: Definition,
    scope: SourceEventScope,
    batch: SourceEventBatch,
}
impl Monitor {
    /// Filesystem-only collection, never holds the application/native writer.
    pub(crate) fn collect(
        &mut self,
        wait: std::time::Duration,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<Batch> {
        Ok(Batch {
            definition: self.definition.clone(),
            scope: self.scope.clone(),
            batch: self.monitor.collect(wait, cancel)?,
        })
    }
    pub(crate) fn acknowledge(&mut self, ack: PersistedSourceEvents) -> anyhow::Result<()> {
        self.monitor.acknowledge(ack)?;
        Ok(())
    }
}
pub(crate) async fn monitor(app: &App, id: &str) -> anyhow::Result<Monitor> {
    let p = prepare(app, id).await?;
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native watch owner missing")?;
    let source = p.mapping.native_source.clone();
    let (scope, p) = runtime
        .with_host(move |host| Ok((host.capture_source_events(&[source])?, p)))
        .await?;
    p.tx.commit().await?;
    let definition = p.definition;
    let saved = scope.clone();
    let monitor = tokio::task::spawn_blocking(move || scope.monitor()).await??;
    Ok(Monitor {
        definition,
        scope: saved,
        monitor,
    })
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    NoDirtyGeneration,
    CapacityBlocked,
    Linked(String),
}
/// Both fresh hints and restart repair reauthorize exact current configuration.
/// A native commit can precede Motion commit; retry repairs the missing link by
/// observing native generation, not by interpreting absence as authorization.
pub(crate) async fn admit(
    app: &App,
    id: &str,
    batch: Option<Batch>,
) -> anyhow::Result<(Admission, Option<PersistedSourceEvents>)> {
    let p = prepare(app, id).await?;
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native watch owner missing")?;
    let source = p.mapping.native_source.clone();
    if let Some(batch) = &batch {
        ensure!(
            batch.definition == p.definition,
            "event belongs to obsolete watch definition"
        );
        let affected: Vec<_> = batch.batch.affected_sources().collect();
        ensure!(
            affected.iter().all(|name| *name == source),
            "watch batch contains an unauthorized source"
        );
    }
    let (mut p, ack, generation) = runtime
        .with_host(move |host| {
            // Moving Prepared retains the actual Motion writer through a detached
            // blocking caller, including any native commit before SQL acknowledgement.
            let scope = if let Some(batch) = &batch {
                batch.scope.clone()
            } else {
                host.capture_source_events(&[source.clone()])?
            };
            let ack = batch
                .map(|batch| host.invalidate_source_events(batch.batch))
                .transpose()?;
            let facts = host.source_event_generations(&scope)?;
            ensure!(
                facts.len() == 1 && facts[0].source == source,
                "native watch generation source differs"
            );
            Ok((p, ack, facts[0].dirty_generation))
        })
        .await?;
    let Some(generation) = generation else {
        p.tx.commit().await?;
        return Ok((Admission::NoDirtyGeneration, ack));
    };
    let saved_bytes: Option<i64> = sqlx::query_scalar("SELECT length(CAST(accepted_scope_json AS BLOB)) FROM catabolic_source_watch_admissions WHERE watch_id=? AND watch_revision=? AND native_dirty_generation=?")
        .bind(&p.definition.id).bind(p.definition.revision).bind(generation).fetch_optional(&mut *p.tx).await?;
    ensure!(
        saved_bytes.is_none_or(|n| (0..=1024 * 1024).contains(&n)),
        "watch saved scope exceeds byte bound"
    );
    let existing:Option<(String,String)>=sqlx::query_as("SELECT request_id,accepted_scope_json FROM catabolic_source_watch_admissions WHERE watch_id=? AND watch_revision=? AND native_dirty_generation=?")
        .bind(&p.definition.id).bind(p.definition.revision).bind(generation).fetch_optional(&mut *p.tx).await?;
    if let Some((id, saved)) = existing {
        let request_bytes: i64 = sqlx::query_scalar(
            "SELECT length(CAST(scope_json AS BLOB)) FROM catabolic_scan_requests WHERE id=?",
        )
        .bind(&id)
        .fetch_one(&mut *p.tx)
        .await?;
        ensure!(
            (0..=1024 * 1024).contains(&request_bytes),
            "watch request scope exceeds byte bound"
        );
        let actual: String =
            sqlx::query_scalar("SELECT scope_json FROM catabolic_scan_requests WHERE id=?")
                .bind(&id)
                .fetch_one(&mut *p.tx)
                .await?;
        ensure!(
            serde_json::from_str::<Value>(&saved)? == serde_json::from_str::<Value>(&actual)?,
            "watch admission/request proof differs"
        );
        let scope: Value = serde_json::from_str(&saved)?;
        verify_execution_in(&mut p.tx, &scope, &id, &p.definition.source_id).await?;
        p.tx.commit().await?;
        return Ok((Admission::Linked(id), ack));
    }
    if !crate::native_scans::capacity_in(&mut p.tx, 1).await? {
        p.tx.commit().await?;
        return Ok((Admission::CapacityBlocked, None));
    }
    p.proof["watch_contract_version"] = json!(1);
    p.proof["watch_occurrence"] = json!({"watch_id":p.definition.id,"revision":p.definition.revision,"source_id":p.definition.source_id,"native_dirty_generation":generation});
    p.proof["watch_source_lifecycle"] = serde_json::to_value(&p.definition.lifecycle)?;
    let request = crate::native_scans::admit_in(
        &mut p.tx,
        &p.definition.library_id,
        &p.principal.id,
        p.proof,
        vec![p.mapping],
        false,
        false,
    )
    .await?;
    let request = request["id"]
        .as_str()
        .context("watch request id missing")?
        .to_owned();
    let accepted: String =
        sqlx::query_scalar("SELECT scope_json FROM catabolic_scan_requests WHERE id=?")
            .bind(&request)
            .fetch_one(&mut *p.tx)
            .await?;
    sqlx::query("INSERT INTO catabolic_source_watch_admissions(watch_id,watch_revision,source_id,native_dirty_generation,request_id,accepted_scope_json,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&p.definition.id).bind(p.definition.revision).bind(&p.definition.source_id).bind(generation).bind(&request).bind(accepted).bind(crate::now()).execute(&mut *p.tx).await?;
    p.tx.commit().await?;
    Ok((Admission::Linked(request), ack))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Occurrence {
    watch_id: String,
    revision: i64,
    source_id: String,
    native_dirty_generation: i64,
}
fn accepted(scope: &Value) -> anyhow::Result<Option<(Occurrence, LifecycleProof)>> {
    let keys = [
        "watch_contract_version",
        "watch_occurrence",
        "watch_source_lifecycle",
    ];
    if keys.iter().all(|key| scope.get(*key).is_none()) {
        return Ok(None);
    }
    ensure!(
        keys.iter().all(|key| scope.get(*key).is_some()) && scope["watch_contract_version"] == 1,
        "incomplete watch acceptance proof"
    );
    ensure!(
        [
            "schedule_contract_version",
            "schedule_occurrence",
            "schedule_source_lifecycle"
        ]
        .iter()
        .all(|key| scope.get(*key).is_none()),
        "mixed watch and schedule proof"
    );
    let occurrence: Occurrence = serde_json::from_value(scope["watch_occurrence"].clone())?;
    let lifecycle: LifecycleProof =
        serde_json::from_value(scope["watch_source_lifecycle"].clone())?;
    ensure!(
        !occurrence.watch_id.is_empty()
            && occurrence.revision > 0
            && occurrence.native_dirty_generation >= 0
            && occurrence.source_id == lifecycle.source_id
            && lifecycle.state == playscale_core::source_lifecycle::State::Active
            && scope["catalog_id"] == lifecycle.catalog_id
            && scope["incarnation"] == lifecycle.incarnation,
        "invalid watch acceptance proof"
    );
    Ok(Some((occurrence, lifecycle)))
}
pub(crate) async fn verify_admission_in(
    db: &mut SqliteConnection,
    scope: &Value,
    sources: &[SourceMapping],
) -> anyhow::Result<()> {
    let Some((occurrence, lifecycle)) = accepted(scope)? else {
        return Ok(());
    };
    let definition = load_in(db, &occurrence.watch_id)
        .await?
        .context("watch definition missing")?;
    ensure!(
        definition.revision == occurrence.revision
            && definition.enabled
            && !definition.tombstoned
            && definition.source_id == occurrence.source_id
            && definition.lifecycle == lifecycle
            && scope["library_id"] == definition.library_id,
        "watch definition changed before admission"
    );
    ensure!(
        sources.len() == 1
            && sources[0].library_id == occurrence.source_id
            && motion_catalog_backend::source_lifecycle::mapping_digest(&sources[0])?
                == lifecycle.mapping_digest,
        "watch accepted source set differs"
    );
    ensure!(
        motion_catalog_backend::source_lifecycle::require_active_in(db, &occurrence.source_id)
            .await?
            == lifecycle,
        "watch lifecycle changed before admission"
    );
    Ok(())
}
pub(crate) async fn verify_execution_in(
    db: &mut SqliteConnection,
    scope: &Value,
    request_id: &str,
    source_id: &str,
) -> anyhow::Result<()> {
    let Some((occurrence, lifecycle)) = accepted(scope)? else {
        return Ok(());
    };
    ensure!(
        source_id == occurrence.source_id,
        "watch executing source differs"
    );
    let bytes: Option<i64> = sqlx::query_scalar("SELECT length(CAST(accepted_scope_json AS BLOB)) FROM catabolic_source_watch_admissions WHERE watch_id=? AND watch_revision=? AND native_dirty_generation=? AND source_id=? AND request_id=?")
        .bind(&occurrence.watch_id).bind(occurrence.revision).bind(occurrence.native_dirty_generation).bind(source_id).bind(request_id).fetch_optional(&mut *db).await?;
    ensure!(
        bytes.is_some_and(|n| (0..=1024 * 1024).contains(&n)),
        "watch occurrence missing or oversized"
    );
    let saved: String = sqlx::query_scalar("SELECT accepted_scope_json FROM catabolic_source_watch_admissions WHERE watch_id=? AND watch_revision=? AND native_dirty_generation=? AND request_id=?")
        .bind(&occurrence.watch_id).bind(occurrence.revision).bind(occurrence.native_dirty_generation).bind(request_id).fetch_one(&mut *db).await?;
    ensure!(
        serde_json::from_str::<Value>(&saved)? == *scope,
        "watch occurrence differs from accepted request"
    );
    let ids: Vec<String> = sqlx::query_scalar("SELECT source_id FROM catabolic_scan_demands WHERE request_id=? ORDER BY source_id LIMIT 2").bind(request_id).fetch_all(&mut *db).await?;
    ensure!(
        ids.len() == 1 && ids[0] == source_id,
        "watch demand source set differs"
    );
    // Configuration edits do not revoke a previously accepted bounded service
    // demand. Explicit request cancellation and current lifecycle still fence effects.
    ensure!(
        motion_catalog_backend::source_lifecycle::require_active_in(db, source_id).await?
            == lifecycle,
        "watch lifecycle changed before effect"
    );
    Ok(())
}

#[cfg(test)]
mod tests;

mod inventory;
pub(crate) use inventory::{ConfigurationState, inventory};
