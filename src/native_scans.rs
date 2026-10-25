//! Durable native scan custody. HTTP admission supplies exact authorized source
//! proof; execution never broadens it using a later logical-library definition.
use crate::App;
use anyhow::{Context, ensure};
use motion_catalog_backend::sources::SourceMapping;
use playscale_core::{
    demands::{self, Attempt, Demand, DemandStatus},
    jobs::Phase,
    native_scan::{self, Outcome},
};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

const MAX_PENDING: i64 = 1024;
const MAX_ATTEMPTS: i64 = 32;

pub(crate) async fn capacity_in(db: &mut SqliteConnection, sources: usize) -> anyhow::Result<bool> {
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_demands WHERE status='pending'")
            .fetch_one(db)
            .await?;
    Ok(pending
        .checked_add(i64::try_from(sources)?)
        .is_some_and(|total| total <= MAX_PENDING))
}
type SignalKey = (usize, String, String);
#[derive(Default)]
struct SignalState {
    active: BTreeMap<SignalKey, ActiveSignal>,
    inhibited: BTreeMap<(usize, String), usize>,
}
struct ActiveSignal {
    stop: CancellationToken,
    done: CancellationToken,
}
static SIGNALS: OnceLock<Mutex<SignalState>> = OnceLock::new();
fn signals() -> &'static Mutex<SignalState> {
    SIGNALS.get_or_init(Default::default)
}
fn owner(app: &App) -> anyhow::Result<usize> {
    Ok(std::sync::Arc::as_ptr(
        app.native_catalog
            .as_ref()
            .context("native scan owner missing")?,
    ) as usize)
}
struct SignalGuard {
    key: SignalKey,
    done: CancellationToken,
}
impl Drop for SignalGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = signals().lock() {
            state.active.remove(&self.key);
        }
        // This guard survives native cleanup AND the attempt acknowledgment.
        self.done.cancel();
    }
}
/// Conservative cancellation custody is not a durable cancellation receipt.
/// Keep this handle through fresh authorization and SQL cancellation commit.
/// Dropping it (including caller abort) permits pending work to reconcile again.
pub struct CancellationRequest {
    key: (usize, String),
    done: Vec<CancellationToken>,
    // Keep the actual owner alive so its pointer cannot be recycled while held.
    _runtime: std::sync::Arc<crate::native_catalog::Runtime>,
}
impl CancellationRequest {
    pub async fn wait_cleanup(&self) {
        for done in &self.done {
            done.cancelled().await;
        }
    }
}
impl Drop for CancellationRequest {
    fn drop(&mut self) {
        if let Ok(mut state) = signals().lock() {
            if let Some(count) = state.inhibited.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    state.inhibited.remove(&self.key);
                }
            }
        }
    }
}
/// Call only after current read-transaction authentication and exact request
/// visibility. This signals interruption and inhibits reclaim, NOT acknowledgment.
pub fn signal_cancel(app: &App, request_id: &str) -> anyhow::Result<CancellationRequest> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native scan owner missing")?
        .clone();
    let owner = owner(app)?;
    let key = (owner, request_id.to_owned());
    let mut state = signals()
        .lock()
        .map_err(|_| anyhow::anyhow!("scan cancellation registry poisoned"))?;
    *state.inhibited.entry(key.clone()).or_default() += 1;
    let mut done = Vec::new();
    for ((active_owner, request, _), active) in &state.active {
        if *active_owner == owner && request == request_id {
            active.stop.cancel();
            done.push(active.done.clone());
        }
    }
    Ok(CancellationRequest {
        key,
        done,
        _runtime: runtime,
    })
}

pub async fn admit_in(
    db: &mut SqliteConnection,
    library_id: &str,
    requester_id: &str,
    scope_proof: Value,
    sources: Vec<SourceMapping>,
    verify: bool,
    require_complete: bool,
) -> anyhow::Result<Value> {
    admit_inner(
        db,
        library_id,
        requester_id,
        scope_proof,
        sources,
        verify,
        require_complete,
    )
    .await
}
async fn admit_inner(
    db: &mut SqliteConnection,
    library_id: &str,
    requester_id: &str,
    mut scope_proof: Value,
    sources: Vec<SourceMapping>,
    verify: bool,
    require_complete: bool,
) -> anyhow::Result<Value> {
    ensure!(
        !library_id.is_empty() && !requester_id.is_empty(),
        "native scan principal/library missing"
    );
    ensure!(
        !sources.is_empty() && sources.len() <= 100,
        "select 1-100 native scan sources"
    );
    ensure!(
        sources
            .iter()
            .map(|m| &m.library_id)
            .collect::<BTreeSet<_>>()
            .len()
            == sources.len(),
        "duplicate native scan source"
    );
    ensure!(
        scope_proof.is_object() && scope_proof.to_string().len() <= 1024 * 1024,
        "bounded native scan scope proof required"
    );
    let principal: playscale_core::access::Principal =
        serde_json::from_value(scope_proof["accepted_principal"].clone())
            .context("native scan accepted authorization missing")?;
    ensure!(
        scope_proof["contract_version"] == 1
            && scope_proof["library_id"] == library_id
            && principal.id == requester_id
            && principal.allows(playscale_core::access::Permission::SourcesManage)
            && playscale_core::access::catalog_scope(&principal).library(library_id),
        "native scan accepted principal does not authorize library scope"
    );
    ensure!(
        sources
            .iter()
            .all(|m| scope_proof["catalog_id"] == m.catalog.catalog_id
                && scope_proof["incarnation"] == m.catalog.incarnation),
        "native scan sources differ from accepted catalog scope"
    );
    ensure!(
        scope_proof["execution_policy"].is_null()
            || scope_proof["execution_policy"] == native_scan::ACCEPTED_SERVICE_POLICY,
        "unsupported accepted native scan execution policy"
    );
    scope_proof["execution_policy"] = json!(native_scan::ACCEPTED_SERVICE_POLICY);
    ensure!(
        capacity_in(db, sources.len()).await?,
        "native scan pending capacity reached"
    );
    for mapping in &sources {
        motion_catalog_backend::sources::check_mapping_in(db, mapping).await?;
        motion_catalog_backend::source_lifecycle::require_active_in(db, &mapping.library_id)
            .await?;
    }
    crate::native_schedules::verify_admission_in(db, &scope_proof, &sources).await?;
    crate::native_watch::verify_admission_in(db, &scope_proof, &sources).await?;
    let id = crate::new_id();
    sqlx::query("INSERT INTO catabolic_scan_requests(id,library_id,requester_id,scope_json,verify,require_complete,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&id).bind(library_id).bind(requester_id).bind(scope_proof.to_string()).bind(verify).bind(require_complete).bind(crate::now()).execute(&mut *db).await?;
    let barrier: i64 =
        sqlx::query_scalar("SELECT admission FROM catabolic_scan_requests WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *db)
            .await?;
    for mapping in sources {
        let demand = Demand {
            id: crate::new_id(),
            barrier: u64::try_from(barrier)?,
            verify,
            require_complete,
            status: DemandStatus::Pending,
        };
        ensure!(
            matches!(demands::admit(&demand, &[]), demands::Admission::Enqueue),
            "native demand admission disagrees"
        );
        sqlx::query("INSERT INTO catabolic_scan_demands(id,request_id,source_id,mapping_json,barrier,phase,status) VALUES(?,?,?,?,?,'queued','pending')")
            .bind(&demand.id).bind(&id).bind(&mapping.library_id).bind(serde_json::to_string(&mapping)?).bind(barrier).execute(&mut *db).await?;
    }
    hint_in(db, &id).await?;
    get_in(db, &id).await
}
#[derive(sqlx::FromRow)]
struct DemandRow {
    id: String,
    request_id: String,
    source_id: String,
    mapping_json: String,
    barrier: i64,
    phase: String,
    status: String,
    attempt: i64,
    started_barrier: Option<i64>,
    result_json: Option<String>,
    error: Option<String>,
    verify: bool,
    require_complete: bool,
    scope_json: String,
}
const ROWS: &str = "SELECT d.*,r.verify,r.require_complete,r.scope_json FROM catabolic_scan_demands d JOIN catabolic_scan_requests r ON r.id=d.request_id";
impl DemandRow {
    fn demand(&self) -> anyhow::Result<Demand> {
        Ok(Demand {
            id: self.id.clone(),
            barrier: u64::try_from(self.barrier)?,
            verify: self.verify,
            require_complete: self.require_complete,
            status: serde_json::from_value(json!(self.status))?,
        })
    }
    fn execution(&self) -> anyhow::Result<native_scan::ExecutionIdentity> {
        let mapping: SourceMapping = serde_json::from_str(&self.mapping_json)?;
        let scope: Value = serde_json::from_str(&self.scope_json)?;
        ensure!(
            scope["execution_policy"] == native_scan::ACCEPTED_SERVICE_POLICY,
            "native scan lacks accepted bounded-service policy"
        );
        Ok(native_scan::ExecutionIdentity {
            policy_version: 1,
            request_id: self.request_id.clone(),
            demand_id: self.id.clone(),
            source_id: self.source_id.clone(),
            source_digest: catabolic_core::sha256(serde_json::to_vec(&mapping)?),
            attempt: u32::try_from(self.attempt)?,
        })
    }
    fn attempt(&self) -> anyhow::Result<Attempt> {
        Ok(Attempt {
            job: self.id.clone(),
            phase: serde_json::from_value(json!(self.phase))?,
            verify: true,
            started_barrier: self.started_barrier.map(u64::try_from).transpose()?,
            direct: false,
        })
    }
}
fn name<T: serde::Serialize>(v: T) -> anyhow::Result<String> {
    Ok(serde_json::to_value(v)?
        .as_str()
        .context("state serialization")?
        .to_owned())
}
pub async fn get_in(db: &mut SqliteConnection, id: &str) -> anyhow::Result<Value> {
    let (library,requester,revision,created,complete):(String,String,i64,i64,bool)=sqlx::query_as("SELECT library_id,requester_id,revision,created_at,require_complete FROM catabolic_scan_requests WHERE id=?").bind(id).fetch_one(&mut *db).await?;
    let rows: Vec<DemandRow> =
        sqlx::query_as(&format!("{ROWS} WHERE d.request_id=? ORDER BY d.source_id"))
            .bind(id)
            .fetch_all(&mut *db)
            .await?;
    let mut sources = Vec::new();
    let mut statuses = Vec::new();
    for row in rows {
        let result: Value = row
            .result_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or(Value::Null);
        let status = if row.status == "pending" {
            if row.phase == "running" || row.phase == "cancelling" {
                "running"
            } else {
                "queued"
            }
        } else {
            row.status.as_str()
        };
        statuses.push(status.to_owned());
        let scans = result["observation"]["scans"].as_array();
        let observed: u64 = scans
            .into_iter()
            .flatten()
            .filter_map(|scan| scan["observed"].as_u64())
            .sum();
        let directories: u64 = scans
            .into_iter()
            .flatten()
            .filter_map(|scan| scan["coverage"]["complete"].as_u64())
            .sum();
        sources.push(json!({"source_id":row.source_id,"attempt_generation":row.attempt.to_string(),"status":status,"complete_directories":directories.to_string(),"incomplete_directories":"0","observed_files":observed.to_string(),"verified_files":result["created"].as_array().map(|v|v.len()).unwrap_or(0).to_string(),"error_codes":row.error.into_iter().collect::<Vec<_>>()}));
    }
    let status = if statuses.iter().any(|s| s == "running") {
        "running"
    } else if statuses.iter().any(|s| s == "queued") {
        "queued"
    } else if statuses.iter().any(|s| s == "failed") {
        "failed"
    } else if statuses.iter().any(|s| s == "cancelled") {
        "cancelled"
    } else if statuses.iter().all(|s| s == "complete") {
        "complete"
    } else {
        "partial"
    };
    Ok(
        json!({"id":id,"revision":revision.to_string(),"library_id":library,"requester_id":requester,"status":status,"require_complete":complete,"captured_after":crate::v2::timestamp(created),"sources":sources,"job_ids":[],"complete":status=="complete"}),
    )
}
/// Application request hint only; current requester/library authorization is
/// resolved at disclosure, never inherited from this durable invalidation.
async fn hint_in(db: &mut SqliteConnection, id: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'scan',id,library_id FROM catabolic_scan_requests WHERE id=?")
        .bind(id).execute(db).await?;
    Ok(())
}
pub async fn cancel_in(db: &mut SqliteConnection, id: &str) -> anyhow::Result<Value> {
    let rows: Vec<DemandRow> =
        sqlx::query_as(&format!("{ROWS} WHERE d.request_id=? ORDER BY d.source_id"))
            .bind(id)
            .fetch_all(&mut *db)
            .await?;
    let mut changed = false;
    for row in rows {
        let d = row.demand()?;
        let a = row.attempt()?;
        let (status, _) = demands::cancel(&d, &[], &[a]);
        if status == DemandStatus::Cancelled && d.status == DemandStatus::Pending {
            changed |= sqlx::query("UPDATE catabolic_scan_demands SET status='cancelled',phase='cancelled',error=NULL WHERE id=? AND status='pending'").bind(row.id).execute(&mut *db).await?.rows_affected() != 0;
        }
    }
    if changed {
        hint_in(db, id).await?;
    }
    get_in(db, id).await
}

static WORKERS: OnceLock<Mutex<BTreeSet<usize>>> = OnceLock::new();
struct WorkerGuard(usize);
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Some(workers) = WORKERS.get() {
            if let Ok(mut workers) = workers.lock() {
                workers.remove(&self.0);
            }
        }
    }
}
async fn recover(app: &App) -> anyhow::Result<()> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native scan owner missing")?
        .clone();
    let custody = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let mut custody = runtime
        .with_host(move |host| {
            ensure!(
                host.worker_inventory()?.admits_new_work(),
                "native observation cleanup is unresolved"
            );
            Ok(custody)
        })
        .await?;
    // Startup owns the server process lock and the unique in-process worker. An
    // interrupted demand is pending, never silently completed/cancelled on restart.
    sqlx::query("INSERT INTO change_events(topic,resource_id,scope_library) SELECT 'scan',r.id,r.library_id FROM catabolic_scan_requests r WHERE EXISTS(SELECT 1 FROM catabolic_scan_demands d WHERE d.request_id=r.id AND d.status='pending' AND d.phase IN ('running','cancelling'))")
        .execute(&mut *custody).await?;
    sqlx::query("UPDATE catabolic_scan_demands SET phase='queued',started_barrier=NULL,error='native_scan_interrupted' WHERE status='pending' AND phase IN ('running','cancelling')")
    .execute(&mut *custody).await?;
    custody.commit().await?;
    Ok(())
}
/// A failed BEGIN has acquired no demand or native effect. Contention is an
/// idle scheduling observation, not a reason to cancel unrelated render owners.
pub(crate) async fn begin_claim(
    db: &sqlx::SqlitePool,
) -> anyhow::Result<Option<sqlx::Transaction<'static, sqlx::Sqlite>>> {
    match db.begin_with("BEGIN IMMEDIATE").await {
        Ok(tx) => Ok(Some(tx)),
        Err(sqlx::Error::Database(error))
            if error
                .code()
                .and_then(|v| v.parse::<i32>().ok())
                .is_some_and(|v| matches!(v & 255, 5 | 6)) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}
async fn claim(
    app: &App,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<(DemandRow, CancellationToken, SignalGuard)>> {
    let owner = owner(app)?;
    let Some(mut tx) = begin_claim(&app.db).await? else {
        return Ok(None);
    };
    let sql = format!(
        "{ROWS} WHERE d.status='pending' AND d.phase='queued' AND NOT EXISTS(SELECT 1 FROM catabolic_scan_demands active WHERE active.source_id=d.source_id AND active.phase IN ('running','cancelling')) ORDER BY r.admission,d.source_id LIMIT {MAX_PENDING}"
    );
    let rows: Vec<DemandRow> = sqlx::query_as(&sql).fetch_all(&mut *tx).await?;
    let row = {
        let state = signals()
            .lock()
            .map_err(|_| anyhow::anyhow!("scan cancellation registry poisoned"))?;
        rows.into_iter().find(|row| {
            !state
                .inhibited
                .contains_key(&(owner, row.request_id.clone()))
        })
    };
    let Some(mut row) = row else {
        tx.rollback().await?;
        return Ok(None);
    };
    if row.attempt >= MAX_ATTEMPTS {
        sqlx::query("UPDATE catabolic_scan_demands SET phase='failed',status='failed',error='native_scan_attempt_limit' WHERE id=?").bind(&row.id).execute(&mut *tx).await?;
        hint_in(&mut tx, &row.request_id).await?;
        tx.commit().await?;
        return Ok(None);
    }
    let mapping: SourceMapping = serde_json::from_str(&row.mapping_json)?;
    motion_catalog_backend::sources::check_mapping_in(&mut tx, &mapping).await?;
    motion_catalog_backend::source_lifecycle::require_active_in(&mut tx, &mapping.library_id)
        .await?;
    row.attempt += 1;
    row.started_barrier = Some(row.barrier);
    row.phase = "running".into();
    ensure!(
        demands::can_satisfy(&row.attempt()?, &row.demand()?),
        "native scan freshness admission failed"
    );
    // Reserve completion custody atomically with the inhibition check, before
    // committing the claim. A signal cannot miss a claimed-but-unregistered task.
    let stop = shutdown.child_token();
    let done = CancellationToken::new();
    let key = (owner, row.request_id.clone(), row.id.clone());
    let reservation = {
        let mut state = signals()
            .lock()
            .map_err(|_| anyhow::anyhow!("scan cancellation registry poisoned"))?;
        if native_scan::admits_claim(
            DemandStatus::Pending,
            Phase::Queued,
            state
                .inhibited
                .contains_key(&(owner, row.request_id.clone())),
        ) {
            ensure!(
                !state.active.contains_key(&key),
                "duplicate native scan reservation"
            );
            state.active.insert(
                key.clone(),
                ActiveSignal {
                    stop: stop.clone(),
                    done: done.clone(),
                },
            );
            Some(SignalGuard { key, done })
        } else {
            None
        }
    };
    let Some(reservation) = reservation else {
        tx.rollback().await?;
        return Ok(None);
    };
    sqlx::query("UPDATE catabolic_scan_demands SET phase='running',attempt=?,started_barrier=?,error=NULL WHERE id=? AND status='pending' AND phase='queued'")
    .bind(row.attempt).bind(row.barrier).bind(&row.id).execute(&mut *tx).await?;
    hint_in(&mut tx, &row.request_id).await?;
    tx.commit().await?;
    Ok(Some((row, stop, reservation)))
}
async fn finish(
    app: &App,
    claimed: &DemandRow,
    outcome: Outcome,
    result: Option<&Value>,
) -> anyhow::Result<()> {
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let row: DemandRow = sqlx::query_as(&format!("{ROWS} WHERE d.id=?"))
        .bind(&claimed.id)
        .fetch_one(&mut *tx)
        .await?;
    ensure!(
        row.attempt == claimed.attempt,
        "native scan completion belongs to another attempt"
    );
    ensure!(
        row.execution()? == claimed.execution()?,
        "native scan completion identity changed"
    );
    let (status, phase) = native_scan::finish(&row.demand()?, &row.attempt()?, outcome);
    let error = match outcome {
        Outcome::Interrupted => Some("native_scan_interrupted"),
        Outcome::Failed => Some("native_scan_failed"),
        Outcome::Complete => None,
    };
    sqlx::query("UPDATE catabolic_scan_demands SET phase=?,status=?,result_json=?,error=? WHERE id=? AND attempt=?")
    .bind(name(phase)?).bind(name(status)?).bind(result.map(Value::to_string)).bind(error).bind(&row.id).bind(row.attempt).execute(&mut *tx).await?;
    hint_in(&mut tx, &row.request_id).await?;
    tx.commit().await?;
    Ok(())
}
/// One retained-owner worker per admitted runtime. Requests are already durable
/// and scoped; no caller task or later library definition supplies new authority.
pub async fn worker(app: App, shutdown: CancellationToken) -> anyhow::Result<()> {
    // Caller loss must not release worker/cleanup custody while a native effect
    // still owns a detached blocking worker. Only the explicit shutdown token
    // requests service interruption; the owned coordinator drains it completely.
    tokio::spawn(worker_owned(app, shutdown))
        .await
        .context("native scan coordinator panicked")?
}
async fn worker_owned(app: App, shutdown: CancellationToken) -> anyhow::Result<()> {
    let owner = owner(&app)?;
    {
        let mut workers = WORKERS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("native worker registry poisoned"))?;
        ensure!(workers.insert(owner), "native scan worker already running");
    }
    let _worker = WorkerGuard(owner);
    recover(&app).await?;
    #[cfg(test)]
    crate::native_service::startup_observation::recovered(
        &app,
        crate::native_service::startup_observation::Role::Scan,
    );
    while !shutdown.is_cancelled() {
        let Some((row, stop, _signal)) = claim(&app, &shutdown).await? else {
            tokio::select! {_=shutdown.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_millis(100))=>{}};
            continue;
        };
        let mapping: SourceMapping = serde_json::from_str(&row.mapping_json)?;
        let result =
            crate::native_discovery::scan_captured(&app, mapping, row.execution()?, stop.clone())
                .await;
        let outcome = if stop.is_cancelled() {
            Outcome::Interrupted
        } else if result.is_ok() {
            Outcome::Complete
        } else {
            Outcome::Failed
        };
        if let Err(error) = &result {
            tracing::warn!(scan_id=%row.request_id,source_id=%row.source_id,error=?error,"native scan did not complete");
        }
        finish(&app, &row, outcome, result.as_ref().ok()).await?;
    }
    Ok(())
}

/// Must run inside the same Motion writer custody retained through the native
/// effect. A previously valid claim is not authority after cancel/retry.
pub(crate) async fn check_execution_in(
    db: &mut SqliteConnection,
    accepted: &native_scan::ExecutionIdentity,
) -> anyhow::Result<()> {
    let row: DemandRow = sqlx::query_as(&format!("{ROWS} WHERE d.id=?"))
        .bind(&accepted.demand_id)
        .fetch_one(&mut *db)
        .await?;
    ensure!(
        native_scan::admits_effect(
            accepted,
            &row.execution()?,
            row.demand()?.status,
            row.attempt()?.phase
        ),
        "native scan demand no longer owns this effect"
    );
    crate::native_schedules::verify_execution_in(
        db,
        &serde_json::from_str(&row.scope_json)?,
        &row.request_id,
        &row.source_id,
    )
    .await?;
    crate::native_watch::verify_execution_in(
        db,
        &serde_json::from_str(&row.scope_json)?,
        &row.request_id,
        &row.source_id,
    )
    .await?;
    Ok(())
}

pub(crate) fn check_source_identity(
    accepted: &native_scan::ExecutionIdentity,
    mapping: &SourceMapping,
) -> anyhow::Result<()> {
    ensure!(
        accepted.source_id == mapping.library_id
            && accepted.source_digest == catabolic_core::sha256(serde_json::to_vec(mapping)?),
        "native scan effect uses a different source proof"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture() -> (
        sqlx::SqlitePool,
        native_scan::ExecutionIdentity,
        SourceMapping,
    ) {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/0083_catabolic_scan_demands.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        let mapping = SourceMapping {
            library_id: "source".into(),
            profile: "default".into(),
            native_source: "source".into(),
            catalog: playscale_core::catalog_backend::Identity {
                catalog_id: "catalog".into(),
                incarnation: "incarnation".into(),
                schema_version: 39,
            },
            motion_root: "/media".into(),
            motion_root_identity: "1:2".into(),
            motion_binding_revision: 1,
            native_binding: json!({"root":"/media"}),
            exclusions: vec![],
            native_policy: json!({}),
        };
        sqlx::query(r#"INSERT INTO catabolic_scan_requests(id,library_id,requester_id,scope_json,verify,require_complete,created_at) VALUES('request','library','principal','{"execution_policy":"accepted_bounded_service_v1"}',1,1,0)"#).execute(&db).await.unwrap();
        sqlx::query("INSERT INTO catabolic_scan_demands(id,request_id,source_id,mapping_json,barrier,phase,status,attempt,started_barrier) VALUES('demand','request','source',?,1,'running','pending',1,1)").bind(serde_json::to_string(&mapping).unwrap()).execute(&db).await.unwrap();
        let accepted = native_scan::ExecutionIdentity {
            policy_version: 1,
            request_id: "request".into(),
            demand_id: "demand".into(),
            source_id: "source".into(),
            source_digest: catabolic_core::sha256(serde_json::to_vec(&mapping).unwrap()),
            attempt: 1,
        };
        (db, accepted, mapping)
    }
    #[tokio::test]
    async fn committed_cancel_revokes_scan_and_publication_custody() {
        let (db, accepted, mapping) = fixture().await;
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        check_execution_in(&mut tx, &accepted).await.unwrap();
        check_source_identity(&accepted, &mapping).unwrap();
        let cancelled = cancel_in(&mut tx, "request").await.unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert!(check_execution_in(&mut tx, &accepted).await.is_err());
        tx.commit().await.unwrap();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        assert!(check_execution_in(&mut tx, &accepted).await.is_err());
        assert_eq!(
            cancel_in(&mut tx, "request").await.unwrap()["status"],
            "cancelled"
        );
    }
    #[tokio::test]
    async fn later_attempt_and_changed_source_proof_cannot_use_old_claim() {
        let (db, accepted, mut mapping) = fixture().await;
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE catabolic_scan_demands SET attempt=2 WHERE id='demand'")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(check_execution_in(&mut tx, &accepted).await.is_err());
        mapping.motion_binding_revision += 1;
        assert!(check_source_identity(&accepted, &mapping).is_err());
        assert!(
            sqlx::query("UPDATE catabolic_scan_demands SET mapping_json='{}' WHERE id='demand'")
                .execute(&mut *tx)
                .await
                .is_err()
        );
        assert!(
            sqlx::query(
                "UPDATE catabolic_scan_requests SET requester_id='another' WHERE id='request'"
            )
            .execute(&mut *tx)
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn interrupted_unacknowledged_request_remains_pending_on_recovery() {
        let (db, accepted, _) = fixture().await;
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("UPDATE catabolic_scan_demands SET phase='queued',started_barrier=NULL,error='native_scan_interrupted' WHERE id='demand'").execute(&mut *tx).await.unwrap();
        let view = get_in(&mut tx, "request").await.unwrap();
        assert_eq!(view["status"], "queued");
        assert_eq!(view["complete"], false);
        assert!(check_execution_in(&mut tx, &accepted).await.is_err());
        assert!(
            sqlx::query("DELETE FROM catabolic_scan_requests")
                .execute(&mut *tx)
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod claim_contention_tests {
    use super::*;
    #[tokio::test]
    async fn busy_begin_acquires_no_demand_and_preserves_actual_writer() {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        let dir = tempfile::tempdir().unwrap();
        let db = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(dir.path().join("claims.sqlite3"))
                    .create_if_missing(true)
                    .busy_timeout(std::time::Duration::from_millis(10)),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE evidence(value INTEGER)")
            .execute(&db)
            .await
            .unwrap();
        let mut owner = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query("INSERT INTO evidence VALUES(7)")
            .execute(&mut *owner)
            .await
            .unwrap();
        assert!(begin_claim(&db).await.unwrap().is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT value FROM evidence")
                .fetch_one(&mut *owner)
                .await
                .unwrap(),
            7
        );
        owner.commit().await.unwrap();
        begin_claim(&db)
            .await
            .unwrap()
            .unwrap()
            .rollback()
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT value FROM evidence")
                .fetch_one(&db)
                .await
                .unwrap(),
            7
        );
        db.close().await;
    }
}
