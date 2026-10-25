//! Native scan-schedule companion owner. Service launch and outer routes use
//! this same current-authority owner; no legacy scheduler fallback.
//! Claim, current authorization, exact source proof, request, occurrence and
//! cursor share a writer. Legacy queues and maintenance are never a fallback.
use crate::App;
mod crud;
mod tick;
use anyhow::{Context, ensure};
pub(crate) use crud::{ScheduleAccessError, create_in, lookup_in, replace_in, tombstone_in};
use playscale_core::{
    access::{self, Permission, Principal},
    scan_schedule::{self as policy, AdoptionDisposition, Cursor, Definition, RetainedSchedule},
};
use sqlx::SqliteConnection;
pub(crate) use tick::{tick_once, worker};

// All three fields are reserved together. A malformed schedule proof must never
// silently fall back to the manual accepted-demand contract.
fn scheduled_proof(
    scope: &serde_json::Value,
) -> anyhow::Result<
    Option<(
        policy::Occurrence,
        Vec<playscale_core::source_lifecycle::LifecycleProof>,
    )>,
> {
    let fields = [
        "schedule_contract_version",
        "schedule_occurrence",
        "schedule_source_lifecycle",
    ];
    if fields.iter().all(|key| scope.get(*key).is_none()) {
        return Ok(None);
    }
    ensure!(
        fields.iter().all(|key| scope.get(*key).is_some())
            && scope["schedule_contract_version"] == 1,
        "incomplete schedule acceptance proof"
    );
    let occurrence: policy::Occurrence =
        serde_json::from_value(scope["schedule_occurrence"].clone())?;
    let lifecycle: Vec<playscale_core::source_lifecycle::LifecycleProof> =
        serde_json::from_value(scope["schedule_source_lifecycle"].clone())?;
    ensure!(
        !occurrence.schedule_id.is_empty()
            && occurrence.definition_revision > 0
            && !lifecycle.is_empty()
            && lifecycle.len() <= 100,
        "invalid schedule acceptance proof"
    );
    let ids: std::collections::BTreeSet<_> = lifecycle.iter().map(|p| &p.source_id).collect();
    ensure!(
        ids.len() == lifecycle.len()
            && lifecycle.iter().all(|p| !p.source_id.is_empty()
                && p.state == playscale_core::source_lifecycle::State::Active
                && scope["catalog_id"] == p.catalog_id
                && scope["incarnation"] == p.incarnation),
        "schedule lifecycle proof differs from accepted scope"
    );
    Ok(Some((occurrence, lifecycle)))
}

pub(crate) async fn verify_admission_in(
    db: &mut SqliteConnection,
    scope: &serde_json::Value,
    sources: &[motion_catalog_backend::sources::SourceMapping],
) -> anyhow::Result<()> {
    let Some((_, lifecycle)) = scheduled_proof(scope)? else {
        return Ok(());
    };
    ensure!(
        lifecycle.len() == sources.len(),
        "schedule accepted source set differs"
    );
    for source in sources {
        let saved = lifecycle
            .iter()
            .find(|p| p.source_id == source.library_id)
            .context("schedule source lifecycle missing")?;
        let current =
            motion_catalog_backend::source_lifecycle::require_active_in(db, &source.library_id)
                .await?;
        ensure!(
            *saved == current
                && saved.mapping_digest
                    == motion_catalog_backend::source_lifecycle::mapping_digest(source)?,
            "schedule source lifecycle changed before admission"
        );
    }
    Ok(())
}

pub(crate) async fn verify_execution_in(
    db: &mut SqliteConnection,
    scope: &serde_json::Value,
    request_id: &str,
    source_id: &str,
) -> anyhow::Result<()> {
    let Some((occurrence, lifecycle)) = scheduled_proof(scope)? else {
        return Ok(());
    };
    let bytes: Option<i64> = sqlx::query_scalar("SELECT length(CAST(accepted_scope_json AS BLOB)) FROM catabolic_scan_schedule_occurrences WHERE schedule_id=? AND definition_revision=? AND due_at=? AND request_id=?")
        .bind(&occurrence.schedule_id).bind(i64::try_from(occurrence.definition_revision)?).bind(occurrence.due_at).bind(request_id).fetch_optional(&mut *db).await?;
    ensure!(
        bytes.is_some_and(|n| (0..=1024 * 1024).contains(&n)),
        "schedule occurrence missing or oversized"
    );
    let saved: String = sqlx::query_scalar("SELECT accepted_scope_json FROM catabolic_scan_schedule_occurrences WHERE schedule_id=? AND definition_revision=? AND due_at=? AND request_id=?")
        .bind(&occurrence.schedule_id).bind(i64::try_from(occurrence.definition_revision)?).bind(occurrence.due_at).bind(request_id).fetch_one(&mut *db).await?;
    ensure!(
        serde_json::from_str::<serde_json::Value>(&saved)? == *scope,
        "schedule occurrence differs from accepted request"
    );
    let ids: Vec<String> = sqlx::query_scalar("SELECT source_id FROM catabolic_scan_demands WHERE request_id=? ORDER BY source_id LIMIT 101").bind(request_id).fetch_all(&mut *db).await?;
    let expected: std::collections::BTreeSet<_> = lifecycle.iter().map(|p| &p.source_id).collect();
    ensure!(
        ids.len() == expected.len() && ids.iter().all(|id| expected.contains(id)),
        "schedule demand source set differs"
    );
    let saved = lifecycle
        .iter()
        .find(|p| p.source_id == source_id)
        .context("schedule executing source missing")?;
    // Completed sibling sources need not remain active. Only this exact demand
    // is about to effect work, under the original bounded service policy.
    ensure!(
        *saved
            == motion_catalog_backend::source_lifecycle::require_active_in(db, source_id).await?,
        "schedule source lifecycle changed before effect"
    );
    Ok(())
}

const LEGACY_ROW: &str = "SELECT json_object('id',id,'revision',revision,'library_id',library_id,'task',task,'interval_seconds',interval_seconds,'enabled',json(CASE enabled WHEN 1 THEN 'true' ELSE 'false' END),'next_run',next_run,'requester_id',requester_id,'credential_hash',credential_hash,'ingress_id',ingress_id) FROM api_schedules WHERE id=?";

async fn legacy_in(
    db: &mut SqliteConnection,
    id: &str,
) -> anyhow::Result<(RetainedSchedule, Option<String>)> {
    let bytes: i64 = sqlx::query_scalar("SELECT 512+6*(length(CAST(id AS BLOB))+length(CAST(library_id AS BLOB))+length(CAST(task AS BLOB))+length(CAST(requester_id AS BLOB))+COALESCE(length(CAST(credential_hash AS BLOB)),0)+COALESCE(length(CAST(ingress_id AS BLOB)),0))+COALESCE((SELECT length(CAST(after_item AS BLOB)) FROM schedule_dispatch_progress WHERE schedule_id=api_schedules.id),0) FROM api_schedules WHERE id=?")
        .bind(id).fetch_one(&mut *db).await?;
    ensure!(
        (0..=1024 * 1024).contains(&bytes),
        "retained schedule row exceeds byte bound"
    );
    let row: String = sqlx::query_scalar(LEGACY_ROW)
        .bind(id)
        .fetch_one(&mut *db)
        .await?;
    let progress =
        sqlx::query_scalar("SELECT after_item FROM schedule_dispatch_progress WHERE schedule_id=?")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?;
    Ok((serde_json::from_str(&row)?, progress))
}

fn scope_ids(scope: &access::CatalogScope) -> Option<Vec<String>> {
    match scope {
        access::CatalogScope::All => None,
        access::CatalogScope::Libraries(ids) => Some(ids.iter().cloned().collect()),
        access::CatalogScope::Nothing => Some(Vec::new()),
    }
}

async fn principal_in(
    db: &mut SqliteConnection,
    row: &RetainedSchedule,
) -> anyhow::Result<Principal> {
    let principal = crate::metadata_jobs::principal(
        db,
        &row.requester_id,
        row.credential_hash.as_deref(),
        row.ingress_id.as_deref(),
        Permission::SourcesManage,
    )
    .await
    .map_err(|e| anyhow::anyhow!("schedule requester authorization: {e:?}"))?;
    ensure!(
        principal.id == row.requester_id,
        "schedule credential/requester identity changed"
    );
    ensure!(
        access::catalog_scope(&principal).library(&row.library_id),
        "schedule library is outside current requester scope"
    );
    Ok(principal)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Adoption {
    Adopted,
    PendingUnsupportedTask,
    PendingNativeDefinition,
}

/// Explicit bounded one-row adoption. This does not dispatch work or replace
/// the legacy list with an empty native view. Absent native definition remains
/// an explicit pending row; storage/integrity errors are never classified absent.
pub(crate) async fn adopt_retained(app: &App, id: &str) -> anyhow::Result<Adoption> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native schedule owner missing")?;
    let selection = runtime.prepare_library_definitions(None).await?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let (original, progress) = legacy_in(&mut tx, id).await?;
    let plan = policy::plan_adoption(original, progress).map_err(anyhow::Error::msg)?;
    let identity = runtime.identity().await?;
    let catalog = identity["catalog_id"]
        .as_str()
        .context("catalog identity missing")?;
    let incarnation = identity["catalog_incarnation"]
        .as_str()
        .context("catalog incarnation missing")?;
    let original_json = serde_json::to_string(&plan.original)?;
    let progress_json = serde_json::to_string(&plan.dispatch_after_item)?;
    let saved_bytes: Option<i64> = sqlx::query_scalar("SELECT length(CAST(original_json AS BLOB))+length(CAST(progress_json AS BLOB))+length(CAST(catalog_id AS BLOB))+length(CAST(incarnation AS BLOB)) FROM catabolic_scan_schedule_adoptions WHERE schedule_id=?")
        .bind(id).fetch_optional(&mut *tx).await?;
    ensure!(
        saved_bytes.is_none_or(|bytes| (0..=1024 * 1024).contains(&bytes)),
        "saved schedule adoption exceeds byte bound"
    );
    type Saved = (String, String, String, String);
    let saved: Option<Saved> = sqlx::query_as("SELECT catalog_id,incarnation,original_json,progress_json FROM catabolic_scan_schedule_adoptions WHERE schedule_id=?")
        .bind(id).fetch_optional(&mut *tx).await?;
    if let Some((saved_catalog, saved_incarnation, saved_original, saved_progress)) = saved {
        ensure!(
            saved_catalog == catalog
                && saved_incarnation == incarnation
                && serde_json::from_str::<RetainedSchedule>(&saved_original)? == plan.original
                && serde_json::from_str::<Option<String>>(&saved_progress)?
                    == plan.dispatch_after_item,
            "retained schedule adoption differs from immutable original"
        );
    } else {
        sqlx::query("INSERT INTO catabolic_scan_schedule_adoptions(schedule_id,catalog_id,incarnation,original_json,progress_json,created_at) VALUES(?,?,?,?,?,?)")
            .bind(id).bind(catalog).bind(incarnation).bind(&original_json).bind(&progress_json).bind(crate::now()).execute(&mut *tx).await?;
    }
    let result = match plan.disposition {
        AdoptionDisposition::PendingUnsupportedTask => Adoption::PendingUnsupportedTask,
        AdoptionDisposition::ScanCandidate { definition, cursor } => {
            let rows = runtime
                .read_libraries(
                    &mut tx,
                    vec![selection],
                    Some(format!("motion.library:{}", definition.library_id)),
                )
                .await;
            let exists = match rows {
                Ok(rows) => {
                    ensure!(rows.len() <= 1, "ambiguous native schedule library");
                    if let Some(row) = rows.first() {
                        ensure!(row[0].as_str() == Some(format!("motion.library:{}", definition.library_id).as_str()), "native schedule library identity changed");
                    }
                    !rows.is_empty()
                }
                Err(error) if error.downcast_ref::<crate::native_catalog::ReadFailure>().is_some_and(|failure|
                    failure.0 == catabolic_store::catalog_scope::ScopeErrorCode::NotFoundOrNotVisible) => false,
                Err(error) => return Err(error),
            };
            if !exists {
                Adoption::PendingNativeDefinition
            } else {
                if let Some((current, _, _)) = load_in(&mut tx, id).await? {
                    // An adopted definition may legitimately have later revisions.
                    // Its immutable adoption link remains the original evidence.
                    let link: Option<String> = sqlx::query_scalar(
                        "SELECT adoption_id FROM catabolic_scan_schedule_definitions WHERE id=?",
                    )
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
                    ensure!(link.as_deref() == Some(id), "native schedule ID collision");
                    ensure!(
                        current.revision >= plan.original.revision,
                        "native schedule revision regressed"
                    );
                } else {
                    insert_definition_in(&mut tx, &identity, &plan.original, &cursor, Some(id))
                        .await?;
                    crud::hint_in(&mut tx, id, "changed", Some(&plan.original.library_id)).await?;
                }
                Adoption::Adopted
            }
        }
    };
    tx.commit().await?;
    Ok(result)
}

async fn insert_definition_in(
    db: &mut SqliteConnection,
    identity: &serde_json::Value,
    row: &RetainedSchedule,
    cursor: &Cursor,
    adoption: Option<&str>,
) -> anyhow::Result<()> {
    ensure!(
        row.task == "scan" && u64::try_from(row.revision)? == cursor.definition_revision,
        "invalid native schedule definition"
    );
    let catalog = identity["catalog_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("schedule catalog identity missing")?;
    let incarnation = identity["catalog_incarnation"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("schedule incarnation missing")?;
    sqlx::query("INSERT INTO catabolic_scan_schedule_definitions(id,catalog_id,incarnation,origin,revision,library_id,enabled,definition_json,adoption_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&row.id).bind(catalog).bind(incarnation).bind(if adoption.is_some(){"legacy_adoption"}else{"native_create"}).bind(row.revision).bind(&row.library_id).bind(row.enabled).bind(serde_json::to_string(row)?).bind(adoption).bind(crate::now()).bind(crate::now()).execute(&mut *db).await?;
    sqlx::query("INSERT INTO catabolic_scan_schedule_cursors(schedule_id,definition_revision,due_at,retry_at) VALUES(?,?,?,?)")
        .bind(&row.id).bind(i64::try_from(cursor.definition_revision)?).bind(cursor.due_at).bind(cursor.retry_at).execute(&mut *db).await?;
    Ok(())
}

async fn load_in(
    db: &mut SqliteConnection,
    id: &str,
) -> anyhow::Result<Option<(RetainedSchedule, Cursor, bool)>> {
    let bytes: Option<i64> = sqlx::query_scalar("SELECT length(CAST(definition_json AS BLOB))+length(CAST(id AS BLOB))+length(CAST(library_id AS BLOB)) FROM catabolic_scan_schedule_definitions WHERE id=?")
        .bind(id).fetch_optional(&mut *db).await?;
    if let Some(bytes) = bytes {
        ensure!(
            (0..=1024 * 1024).contains(&bytes),
            "native schedule row exceeds byte bound"
        );
    }
    type Row = (
        String,
        i64,
        String,
        bool,
        bool,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );
    let row: Option<Row> = sqlx::query_as("SELECT d.definition_json,d.revision,d.library_id,d.enabled,d.tombstoned,c.definition_revision,c.due_at,c.retry_at FROM catabolic_scan_schedule_definitions d LEFT JOIN catabolic_scan_schedule_cursors c ON c.schedule_id=d.id WHERE d.id=?")
        .bind(id).fetch_optional(&mut *db).await?;
    row.map(
        |(json, revision, library, enabled, tombstone, cursor_revision, due_at, retry_at)| {
            let row: RetainedSchedule = serde_json::from_str(&json)?;
            ensure!(
                row.id == id
                    && row.revision == revision
                    && row.library_id == library
                    && row.enabled == enabled
                    && row.task == "scan",
                "native schedule definition columns differ"
            );
            let cursor = Cursor {
                definition_revision: cursor_revision
                    .context("native schedule cursor missing")?
                    .try_into()?,
                due_at: due_at.context("native schedule due cursor missing")?,
                retry_at: retry_at.context("native schedule retry cursor missing")?,
            };
            ensure!(
                u64::try_from(revision)? == cursor.definition_revision,
                "native schedule cursor revision differs"
            );
            Ok((row, cursor, tombstone))
        },
    )
    .transpose()
}

fn definition(row: &RetainedSchedule) -> anyhow::Result<Definition> {
    Ok(Definition {
        id: row.id.clone(),
        revision: row.revision.try_into()?,
        library_id: row.library_id.clone(),
        interval_seconds: row.interval_seconds,
        enabled: row.enabled,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fire {
    Idle,
    CapacityBlocked,
    Admitted(String),
}

/// Explicit one-occurrence service owner, not a launched scheduler. All native
/// calls here are read/proof operations; the only effect is durable demand
/// admission. The existing scan worker owns later external execution custody.
pub(crate) async fn fire_one(app: &App, id: &str, now: i64) -> anyhow::Result<Fire> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native schedule owner missing")?;
    let _jobs = app.jobs.lock().await;
    let mut read = app.db.begin().await?;
    let Some((original, cursor, tombstone)) = load_in(&mut read, id).await? else {
        return Ok(Fire::Idle);
    };
    if tombstone
        || policy::claim(&definition(&original)?, &cursor, now)
            .map_err(anyhow::Error::msg)?
            .is_none()
    {
        return Ok(Fire::Idle);
    }
    let initial = principal_in(&mut read, &original).await?;
    read.commit().await?;
    let selection = runtime
        .prepare_library_definitions(scope_ids(&access::catalog_scope(&initial)))
        .await?;
    let Some(mut tx) = crate::native_scans::begin_claim(&app.db).await? else {
        return Ok(Fire::Idle);
    };
    let Some((current, cursor, tombstone)) = load_in(&mut tx, id).await? else {
        return Ok(Fire::Idle);
    };
    ensure!(
        current == original,
        "schedule definition changed while preparing current scope"
    );
    if tombstone {
        return Ok(Fire::Idle);
    }
    let Some(claim) =
        policy::claim(&definition(&current)?, &cursor, now).map_err(anyhow::Error::msg)?
    else {
        return Ok(Fire::Idle);
    };
    verify_origin_in(&mut tx, id, &runtime.identity().await?).await?;
    let principal = principal_in(&mut tx, &current).await?;
    ensure!(
        principal == initial,
        "schedule requester changed while preparing current scope"
    );
    let (mut proof, sources) =
        crate::native_catalog::scan_sources_in(app, &mut tx, &current.library_id, selection, None)
            .await
            .map_err(|e| anyhow::anyhow!("schedule source proof: {e:?}"))?;
    let mut lifecycle = Vec::with_capacity(sources.len());
    for source in &sources {
        lifecycle.push(
            motion_catalog_backend::source_lifecycle::require_active_in(
                &mut tx,
                &source.library_id,
            )
            .await?,
        );
    }
    proof["accepted_principal"] = serde_json::to_value(&principal)?;
    proof["schedule_contract_version"] = serde_json::json!(1);
    proof["schedule_occurrence"] = serde_json::to_value(&claim.occurrence)?;
    proof["schedule_source_lifecycle"] = serde_json::to_value(lifecycle)?;
    if !crate::native_scans::capacity_in(&mut tx, sources.len()).await? {
        advance_in(&mut tx, &claim, policy::Outcome::CapacityBlocked).await?;
        tx.commit().await?;
        return Ok(Fire::CapacityBlocked);
    }
    let request = crate::native_scans::admit_in(
        &mut tx,
        &current.library_id,
        &principal.id,
        proof,
        sources,
        false,
        false,
    )
    .await?;
    let request_id = request["id"]
        .as_str()
        .context("native schedule request identity missing")?
        .to_owned();
    // Store the owner's final accepted proof, including its service policy.
    let accepted: String =
        sqlx::query_scalar("SELECT scope_json FROM catabolic_scan_requests WHERE id=?")
            .bind(&request_id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO catabolic_scan_schedule_occurrences(schedule_id,definition_revision,due_at,request_id,accepted_scope_json,created_at) VALUES(?,?,?,?,?,?)")
        .bind(&claim.occurrence.schedule_id).bind(i64::try_from(claim.occurrence.definition_revision)?).bind(claim.occurrence.due_at).bind(&request_id).bind(accepted).bind(now).execute(&mut *tx).await?;
    advance_in(&mut tx, &claim, policy::Outcome::Admitted).await?;
    tx.commit().await?;
    Ok(Fire::Admitted(request_id))
}

async fn advance_in(
    db: &mut SqliteConnection,
    claim: &policy::Claim,
    outcome: policy::Outcome,
) -> anyhow::Result<()> {
    let next = claim.cursor_after(outcome);
    let changed = sqlx::query("UPDATE catabolic_scan_schedule_cursors SET due_at=?,retry_at=? WHERE schedule_id=? AND definition_revision=? AND due_at=? AND retry_at=?")
        .bind(next.due_at).bind(next.retry_at).bind(&claim.occurrence.schedule_id).bind(i64::try_from(claim.expected_cursor.definition_revision)?).bind(claim.expected_cursor.due_at).bind(claim.expected_cursor.retry_at).execute(db).await?.rows_affected();
    ensure!(changed == 1, "native schedule cursor CAS conflict");
    Ok(())
}

/// Verify an adopted definition against actual retained provenance, not just an
/// adoption ID. Legacy edits/deletion cannot silently leave an old native grant.
async fn verify_origin_in(
    db: &mut SqliteConnection,
    id: &str,
    identity: &serde_json::Value,
) -> anyhow::Result<bool> {
    let (catalog,incarnation,origin,adoption):(String,String,String,Option<String>)=sqlx::query_as("SELECT catalog_id,incarnation,origin,adoption_id FROM catabolic_scan_schedule_definitions WHERE id=?").bind(id).fetch_one(&mut *db).await?;
    ensure!(
        identity["catalog_id"] == catalog && identity["catalog_incarnation"] == incarnation,
        "schedule definition belongs to another catalog"
    );
    match (origin.as_str(), adoption.as_deref()) {
        ("legacy_adoption", Some(link)) if link == id => {
            verify_adoption_in(db, id, identity).await?;
            Ok(true)
        }
        ("native_create", None) => {
            let collision:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_schedules WHERE id=? UNION ALL SELECT 1 FROM catabolic_scan_schedule_adoptions WHERE schedule_id=?)").bind(id).bind(id).fetch_one(db).await?;
            ensure!(
                !collision,
                "fresh schedule collides with retained legacy evidence"
            );
            Ok(false)
        }
        _ => anyhow::bail!("schedule owner origin is invalid"),
    }
}

async fn verify_adoption_in(
    db: &mut SqliteConnection,
    id: &str,
    identity: &serde_json::Value,
) -> anyhow::Result<()> {
    type Saved = (String, String, String, String, String);
    let bytes: i64 = sqlx::query_scalar("SELECT length(CAST(a.original_json AS BLOB))+length(CAST(a.progress_json AS BLOB))+length(CAST(a.catalog_id AS BLOB))+length(CAST(a.incarnation AS BLOB)) FROM catabolic_scan_schedule_definitions d JOIN catabolic_scan_schedule_adoptions a ON a.schedule_id=d.adoption_id WHERE d.id=?")
        .bind(id).fetch_one(&mut *db).await?;
    ensure!(
        (0..=1024 * 1024).contains(&bytes),
        "schedule adoption evidence exceeds byte bound"
    );
    let saved: Saved = sqlx::query_as("SELECT a.schedule_id,a.catalog_id,a.incarnation,a.original_json,a.progress_json FROM catabolic_scan_schedule_definitions d JOIN catabolic_scan_schedule_adoptions a ON a.schedule_id=d.adoption_id WHERE d.id=?")
        .bind(id).fetch_one(&mut *db).await?;
    ensure!(
        saved.0 == id
            && identity["catalog_id"] == saved.1
            && identity["catalog_incarnation"] == saved.2,
        "schedule adoption belongs to another retained owner"
    );
    let (current_original, progress) = legacy_in(db, id).await?;
    ensure!(
        serde_json::from_str::<RetainedSchedule>(&saved.3)? == current_original
            && serde_json::from_str::<Option<String>>(&saved.4)? == progress,
        "legacy schedule changed after immutable adoption"
    );
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RetainedView {
    pub row: RetainedSchedule,
    pub progress: Option<String>,
    pub adopted: bool,
    pub native: bool,
    pub tombstoned: bool,
}

/// Administrative inventory, not a public wire/authorization grant. Callers
/// still filter with current permission and native library scope. Full bounded
/// union never suppresses a legacy row based solely on a matching link ID.
pub(crate) async fn retained_inventory_in(
    app: &App,
    db: &mut SqliteConnection,
) -> anyhow::Result<Vec<RetainedView>> {
    let runtime = app
        .native_catalog
        .as_ref()
        .context("native schedule owner missing")?;
    let identity = runtime.identity().await?;
    // Precharge all retained data before fetching IDs or decoding any JSON.
    // Legacy string escaping is conservatively bounded by six bytes per input
    // byte; duplicate original/adopted evidence is deliberately charged twice.
    let bytes: i64 = sqlx::query_scalar("SELECT COALESCE((SELECT SUM(512+6*(length(CAST(id AS BLOB))+length(CAST(library_id AS BLOB))+length(CAST(task AS BLOB))+length(CAST(requester_id AS BLOB))+COALESCE(length(CAST(credential_hash AS BLOB)),0)+COALESCE(length(CAST(ingress_id AS BLOB)),0))) FROM api_schedules),0)+COALESCE((SELECT SUM(length(CAST(after_item AS BLOB))) FROM schedule_dispatch_progress),0)+COALESCE((SELECT SUM(length(CAST(definition_json AS BLOB))+length(CAST(id AS BLOB))+length(CAST(library_id AS BLOB))) FROM catabolic_scan_schedule_definitions),0)+COALESCE((SELECT SUM(length(CAST(original_json AS BLOB))+length(CAST(progress_json AS BLOB))+length(CAST(catalog_id AS BLOB))+length(CAST(incarnation AS BLOB))) FROM catabolic_scan_schedule_adoptions),0)")
        .fetch_one(&mut *db).await?;
    ensure!(
        (0..=8 * 1024 * 1024).contains(&bytes),
        "retained schedule inventory exceeds byte bound"
    );
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM api_schedules UNION SELECT id FROM catabolic_scan_schedule_definitions ORDER BY id LIMIT 10001")
        .fetch_all(&mut *db).await?;
    ensure!(
        ids.len() <= 10000,
        "retained schedule inventory exceeds complete bound"
    );
    let mut result = Vec::with_capacity(ids.len());
    let mut bytes = 0usize;
    for id in ids {
        let view = if let Some((mut current, cursor, tombstoned)) = load_in(db, &id).await? {
            let adopted = verify_origin_in(db, &id, &identity).await?;
            current.next_run = cursor.retry_at;
            RetainedView {
                row: current,
                progress: None,
                adopted,
                native: true,
                tombstoned,
            }
        } else {
            let (row, progress) = legacy_in(db, &id).await?;
            RetainedView {
                row,
                progress,
                adopted: false,
                native: false,
                tombstoned: false,
            }
        };
        bytes = bytes
            .checked_add(serde_json::to_vec(&view.row)?.len())
            .context("schedule inventory byte count overflow")?;
        bytes = bytes
            .checked_add(view.progress.as_ref().map_or(0, String::len))
            .context("schedule progress byte count overflow")?;
        ensure!(
            bytes <= 8 * 1024 * 1024,
            "retained schedule inventory exceeds byte bound"
        );
        result.push(view);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn storage() -> sqlx::SqlitePool {
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
        sqlx::raw_sql(include_str!(
            "../migrations/0092_catabolic_scan_schedules.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        db
    }

    fn row() -> RetainedSchedule {
        RetainedSchedule {
            id: "schedule".into(),
            revision: 9,
            library_id: "library".into(),
            task: "scan".into(),
            interval_seconds: 600,
            enabled: true,
            next_run: 100,
            requester_id: "operator".into(),
            credential_hash: None,
            ingress_id: None,
        }
    }

    #[tokio::test]
    async fn missing_json_fields_and_array_scope_are_storage_errors() {
        let db = storage().await;
        let error = sqlx::query("INSERT INTO catabolic_scan_schedule_adoptions VALUES('schedule','catalog','incarnation','{}','null',0)").execute(&db).await.unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
        for key in ["id", "revision", "library_id", "enabled", "task"] {
            let mut value = serde_json::to_value(row()).unwrap();
            value.as_object_mut().unwrap().remove(key);
            let error = sqlx::query("INSERT INTO catabolic_scan_schedule_definitions(id,catalog_id,incarnation,origin,revision,library_id,enabled,definition_json,created_at,updated_at) VALUES('schedule','catalog','incarnation','native_create',9,'library',1,?,0,0)")
                .bind(value.to_string()).execute(&db).await.unwrap_err();
            assert!(
                error.to_string().contains("CHECK constraint failed"),
                "{key}: {error}"
            );
        }
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        insert_definition_in(
            &mut tx,
            &serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"}),
            &row(),
            &Cursor {
                definition_revision: 9,
                due_at: 100,
                retry_at: 100,
            },
            None,
        )
        .await
        .unwrap();
        // Storage-only references, never used as an execution grant.
        sqlx::query("INSERT INTO catabolic_scan_requests(id,library_id,requester_id,scope_json,verify,require_complete,created_at) VALUES('request','library','operator','{}',0,0,0)").execute(&mut *tx).await.unwrap();
        let error = sqlx::query("INSERT INTO catabolic_scan_schedule_occurrences VALUES('schedule',9,100,'request','[]',0)").execute(&mut *tx).await.unwrap_err();
        assert!(error.to_string().contains("CHECK constraint failed"));
        tx.rollback().await.unwrap();
    }

    #[tokio::test]
    async fn cursor_cas_and_rollback_never_acknowledge_a_partial_fire() {
        // Storage-only contract: no fabricated native source/authority or request
        // is passed to the production admission owner in this unit fixture.
        let db = storage().await;
        let row = row();
        let cursor = Cursor {
            definition_revision: 9,
            due_at: 100,
            retry_at: 100,
        };
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        insert_definition_in(
            &mut tx,
            &serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"}),
            &row,
            &cursor,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let claim = policy::claim(&definition(&row).unwrap(), &cursor, 100)
            .unwrap()
            .unwrap();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        advance_in(&mut tx, &claim, policy::Outcome::Admitted)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let (_, observed, _) = load_in(&mut *db.acquire().await.unwrap(), "schedule")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observed, cursor);
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        advance_in(&mut tx, &claim, policy::Outcome::CapacityBlocked)
            .await
            .unwrap();
        assert!(
            advance_in(&mut tx, &claim, policy::Outcome::Admitted)
                .await
                .is_err()
        );
        tx.commit().await.unwrap();
        let (_, blocked, _) = load_in(&mut *db.acquire().await.unwrap(), "schedule")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(blocked.due_at, cursor.due_at);
        assert_eq!(blocked.retry_at, 400);
        let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_scan_requests")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(requests, 0);
    }

    #[tokio::test]
    async fn adoption_evidence_and_definition_tombstones_are_retained() {
        let db = storage().await;
        let row = row();
        sqlx::query("INSERT INTO catabolic_scan_schedule_adoptions VALUES(?,?,?,?,?,?)")
            .bind(&row.id)
            .bind("catalog")
            .bind("incarnation")
            .bind(serde_json::to_string(&row).unwrap())
            .bind("null")
            .bind(0i64)
            .execute(&db)
            .await
            .unwrap();
        for statement in [
            "UPDATE catabolic_scan_schedule_adoptions SET original_json='{}'",
            "DELETE FROM catabolic_scan_schedule_adoptions",
        ] {
            assert!(sqlx::query(statement).execute(&db).await.is_err());
        }
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        insert_definition_in(
            &mut tx,
            &serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"}),
            &row,
            &Cursor {
                definition_revision: 9,
                due_at: 100,
                retry_at: 100,
            },
            Some(&row.id),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert!(
            sqlx::query("DELETE FROM catabolic_scan_schedule_definitions")
                .execute(&db)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE catabolic_scan_schedule_definitions SET revision=11")
                .execute(&db)
                .await
                .is_err()
        );
        let mut tombstone = row.clone();
        tombstone.revision = 10;
        tombstone.enabled = false;
        sqlx::query("UPDATE catabolic_scan_schedule_definitions SET revision=10,enabled=0,tombstoned=1,definition_json=? WHERE id=?")
            .bind(serde_json::to_string(&tombstone).unwrap()).bind(&row.id).execute(&db).await.unwrap();
        assert!(
            sqlx::query("UPDATE catabolic_scan_schedule_definitions SET tombstoned=0")
                .execute(&db)
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;

    #[tokio::test]
    async fn linked_original_mutation_is_not_adoption_authority() {
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
        sqlx::raw_sql(include_str!(
            "../migrations/0092_catabolic_scan_schedules.sql"
        ))
        .execute(&db)
        .await
        .unwrap();
        // Exact legacy scalar storage shape only. No native session or grant is
        // synthesized; this private test exercises immutable link comparison.
        sqlx::raw_sql("CREATE TABLE api_schedules(id TEXT PRIMARY KEY,revision INTEGER,library_id TEXT,task TEXT,interval_seconds INTEGER,enabled INTEGER,next_run INTEGER,requester_id TEXT,credential_hash TEXT,ingress_id TEXT); CREATE TABLE schedule_dispatch_progress(schedule_id TEXT PRIMARY KEY,after_item TEXT);")
            .execute(&db).await.unwrap();
        sqlx::query("INSERT INTO api_schedules VALUES('s',3,'l','scan',600,0,123,'actor','credential','ingress')").execute(&db).await.unwrap();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let (original, progress) = legacy_in(&mut tx, "s").await.unwrap();
        assert_eq!(original.next_run, 123);
        assert!(!original.enabled);
        sqlx::query("INSERT INTO catabolic_scan_schedule_adoptions VALUES('s','catalog','incarnation',?,'null',0)")
            .bind(serde_json::to_string(&original).unwrap()).execute(&mut *tx).await.unwrap();
        insert_definition_in(
            &mut tx,
            &serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"}),
            &original,
            &Cursor {
                definition_revision: 3,
                due_at: 123,
                retry_at: 123,
            },
            Some("s"),
        )
        .await
        .unwrap();
        assert!(progress.is_none());
        let identity =
            serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"});
        verify_adoption_in(&mut tx, "s", &identity).await.unwrap();
        let wrong = serde_json::json!({"catalog_id":"catalog","catalog_incarnation":"other"});
        assert!(verify_adoption_in(&mut tx, "s", &wrong).await.is_err());
        sqlx::query("UPDATE api_schedules SET requester_id='different-actor' WHERE id='s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(verify_adoption_in(&mut tx, "s", &identity).await.is_err());
        sqlx::query("UPDATE api_schedules SET requester_id='actor' WHERE id='s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO schedule_dispatch_progress VALUES('s','unexpected')")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(verify_adoption_in(&mut tx, "s", &identity).await.is_err());
        tx.rollback().await.unwrap();
    }
}

#[cfg(test)]
mod acceptance_shape_tests {
    use super::*;
    use serde_json::json;

    fn proof() -> serde_json::Value {
        json!({"catalog_id":"catalog","incarnation":"incarnation",
            "schedule_contract_version":1,
            "schedule_occurrence":{"schedule_id":"schedule","definition_revision":1,"due_at":10},
            "schedule_source_lifecycle":[{"source_id":"source","epoch":0,"catalog_id":"catalog","incarnation":"incarnation","mapping_digest":"digest","source_revision":1,"binding_revision":1,"state":"active"}]})
    }

    #[test]
    fn partial_schedule_fields_never_downgrade_to_manual() {
        assert!(
            scheduled_proof(&json!({"contract_version":1}))
                .unwrap()
                .is_none()
        );
        assert!(scheduled_proof(&proof()).unwrap().is_some());
        for key in [
            "schedule_contract_version",
            "schedule_occurrence",
            "schedule_source_lifecycle",
        ] {
            let mut value = proof();
            value.as_object_mut().unwrap().remove(key);
            assert!(scheduled_proof(&value).is_err(), "{key}");
        }
        let mut value = proof();
        let duplicate = value["schedule_source_lifecycle"][0].clone();
        value["schedule_source_lifecycle"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(scheduled_proof(&value).is_err());
        let mut value = proof();
        value["schedule_occurrence"]["unexpected"] = json!(true);
        assert!(scheduled_proof(&value).is_err());
        let mut value = proof();
        value["schedule_source_lifecycle"][0]["state"] = json!("withdrawn");
        assert!(scheduled_proof(&value).is_err());
    }

    #[tokio::test]
    async fn manual_execution_needs_no_schedule_storage_but_missing_occurrence_refuses() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut db = pool.acquire().await.unwrap();
        verify_execution_in(&mut db, &json!({"contract_version":1}), "request", "source")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/0083_catabolic_scan_demands.sql"
        ))
        .execute(&mut *db)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../migrations/0092_catabolic_scan_schedules.sql"
        ))
        .execute(&mut *db)
        .await
        .unwrap();
        let error = verify_execution_in(&mut db, &proof(), "request", "source")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("occurrence missing"));
    }
}

#[cfg(test)]
#[path = "native_schedules/owner_tests.rs"]
mod owner_tests;
