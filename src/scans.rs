//! Scan requests and shared demands. Admission, satisfaction, cancellation and
//! follow-up decisions come from `playscale_core::demands`; this adapter loads
//! per-source attempts and demands inside the writer transaction and applies
//! the decisions. Physical attempts remain `jobs` rows run by the scan worker.
use crate::{App, new_id, now};
use playscale_core::{
    demands::{self as core, Admission, Attempt, Demand, DemandStatus, FollowUp},
    jobs::{Input, Job, Phase, transition},
};
use serde::Serialize;
use sqlx::SqliteConnection;

#[derive(Debug)]
pub enum ScanError {
    NotFound,
    /// A requested source is not part of the library.
    UnknownSource(String),
    /// The library has no sources, or an empty subset was requested.
    NoSources,
    Storage(anyhow::Error),
}
impl From<sqlx::Error> for ScanError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            e => Self::Storage(e.into()),
        }
    }
}
impl From<anyhow::Error> for ScanError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}

fn status_value(name: &str) -> anyhow::Result<DemandStatus> {
    Ok(serde_json::from_value(serde_json::Value::String(
        name.into(),
    ))?)
}
fn status_name(status: DemandStatus) -> &'static str {
    match status {
        DemandStatus::Pending => "pending",
        DemandStatus::Complete => "complete",
        DemandStatus::Partial => "partial",
        DemandStatus::Failed => "failed",
        DemandStatus::Cancelled => "cancelled",
    }
}

async fn attempts(
    conn: &mut SqliteConnection,
    source: &str,
) -> anyhow::Result<Vec<(Attempt, Job)>> {
    let rows: Vec<(String, String, i64, bool, Option<i64>, bool)> = sqlx::query_as(
        "SELECT id,phase,attempt,full_scan,started_barrier,direct_request FROM jobs WHERE library_id=? AND phase IN ('queued','running','cancelling') ORDER BY created_at,id",
    )
    .bind(source)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|(id, phase, attempt, verify, started, direct)| {
            let phase: Phase = serde_json::from_value(serde_json::Value::String(phase))?;
            Ok((
                Attempt {
                    job: id,
                    phase,
                    verify,
                    started_barrier: started.map(u64::try_from).transpose()?,
                    direct,
                },
                Job {
                    phase,
                    attempt: u32::try_from(attempt)?,
                },
            ))
        })
        .collect()
}

async fn demands(conn: &mut SqliteConnection, source: &str) -> anyhow::Result<Vec<Demand>> {
    let rows: Vec<(String, i64, bool, String)> = sqlx::query_as(
        "SELECT id,barrier,verify,status FROM scan_demands WHERE source_id=? AND status='pending' ORDER BY rowid",
    )
    .bind(source)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|(id, barrier, verify, status)| {
            Ok(Demand {
                id,
                barrier: u64::try_from(barrier)?,
                verify,
                require_complete: false,
                status: status_value(&status)?,
            })
        })
        .collect()
}

async fn enqueue(conn: &mut SqliteConnection, source: &str, verify: bool) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO jobs (id,library_id,phase,created_at,full_scan,direct_request) VALUES (?,?,'queued',?,?,0)",
    )
    .bind(new_id())
    .bind(source)
    .bind(now())
    .bind(verify)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn upgrade(conn: &mut SqliteConnection, job: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE jobs SET full_scan=1 WHERE id=? AND phase='queued'")
        .bind(job)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Apply the follow-up decision for one source.
async fn follow_up(conn: &mut SqliteConnection, source: &str) -> anyhow::Result<()> {
    let pending = demands(conn, source).await?;
    let attempts: Vec<Attempt> = attempts(conn, source)
        .await?
        .into_iter()
        .map(|(a, _)| a)
        .collect();
    match core::follow_up(&pending, &attempts) {
        FollowUp::Nothing => {}
        FollowUp::Enqueue { verify } => enqueue(conn, source, verify).await?,
        FollowUp::UpgradeQueued { job } => upgrade(conn, &job).await?,
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SourceResult {
    pub source_id: String,
    pub status: DemandStatus,
    pub job_id: Option<String>,
    pub complete_directories: i64,
    pub incomplete_directories: i64,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScanView {
    pub id: String,
    pub library_id: String,
    pub status: DemandStatus,
    pub require_complete: bool,
    pub sources: Vec<SourceResult>,
    /// Coverage proven complete for every source.
    pub complete: bool,
}

/// Request a scan of a library (or a subset of its sources). Each source gets a
/// demand at a fresh barrier, joined to an attempt that will satisfy it, an
/// upgraded queued attempt, or a newly queued one.
pub async fn request(
    app: &App,
    library: &str,
    sources: Option<&[String]>,
    verify: bool,
    require_complete: bool,
) -> Result<ScanView, ScanError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let members: Vec<(String, bool)> = sqlx::query_as(
        "SELECT s.id,s.enabled FROM library_sources l JOIN sources s ON s.id=l.source_id WHERE l.library_id=? ORDER BY s.id",
    )
    .bind(library)
    .fetch_all(&mut *tx)
    .await?;
    let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog_libraries WHERE id=?")
        .bind(library)
        .fetch_one(&mut *tx)
        .await?;
    if exists == 0 {
        return Err(ScanError::NotFound);
    }
    let selected: Vec<(String, bool)> = match sources {
        None => members,
        Some(ids) => {
            let mut out = Vec::new();
            for id in ids {
                match members.iter().find(|(m, _)| m == id) {
                    Some(m) => out.push(m.clone()),
                    None => return Err(ScanError::UnknownSource(id.clone())),
                }
            }
            out
        }
    };
    if selected.is_empty() {
        return Err(ScanError::NoSources);
    }
    let id = new_id();
    sqlx::query("INSERT INTO scan_requests VALUES (?,?,?,?)")
        .bind(&id)
        .bind(library)
        .bind(require_complete)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    for (source, enabled) in selected {
        let demand_id = new_id();
        if !enabled {
            sqlx::query("INSERT INTO scan_demands (id,request_id,source_id,barrier,verify,status,error) VALUES (?,?,?,0,?,'failed','source_disabled')")
                .bind(&demand_id).bind(&id).bind(&source).bind(verify).execute(&mut *tx).await?;
            continue;
        }
        let barrier: i64 = sqlx::query_scalar(
            "UPDATE libraries SET scan_barrier=scan_barrier+1 WHERE id=? RETURNING scan_barrier",
        )
        .bind(&source)
        .fetch_one(&mut *tx)
        .await?;
        let demand = Demand {
            id: demand_id.clone(),
            barrier: u64::try_from(barrier).map_err(anyhow::Error::from)?,
            verify,
            require_complete,
            status: DemandStatus::Pending,
        };
        let current: Vec<Attempt> = attempts(&mut tx, &source)
            .await?
            .into_iter()
            .map(|(a, _)| a)
            .collect();
        match core::admit(&demand, &current) {
            Admission::Join { .. } => {}
            Admission::UpgradeQueued { job } => upgrade(&mut tx, &job).await?,
            Admission::Enqueue => enqueue(&mut tx, &source, verify).await?,
        }
        sqlx::query("INSERT INTO scan_demands (id,request_id,source_id,barrier,verify,status) VALUES (?,?,?,?,?,'pending')")
            .bind(&demand_id).bind(&id).bind(&source).bind(barrier).bind(verify).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    get(&app.db, &id).await
}

/// A filesystem change hint: later demands require a newer traversal.
pub async fn mark_dirty(app: &App, source: &str) -> Result<(), ScanError> {
    let _guard = app.jobs.lock().await;
    let changed = sqlx::query("UPDATE libraries SET scan_barrier=scan_barrier+1 WHERE id=?")
        .bind(source)
        .execute(&app.db)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ScanError::NotFound);
    }
    Ok(())
}

/// Cancel a request's pending demands. An attempt is stopped only if no other
/// pending demand still needs it.
pub async fn cancel(app: &App, id: &str) -> Result<ScanView, ScanError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let mine: Vec<(String, String)> = sqlx::query_as(
        "SELECT id,source_id FROM scan_demands WHERE request_id=? AND status='pending'",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    for (demand_id, source) in mine {
        let pending = demands(&mut tx, &source).await?;
        let Some(demand) = pending.iter().find(|d| d.id == demand_id).cloned() else {
            continue;
        };
        let current = attempts(&mut tx, &source).await?;
        let plain: Vec<Attempt> = current.iter().map(|(a, _)| a.clone()).collect();
        let (status, stop) = core::cancel(&demand, &pending, &plain);
        sqlx::query("UPDATE scan_demands SET status=? WHERE id=?")
            .bind(status_name(status))
            .bind(&demand_id)
            .execute(&mut *tx)
            .await?;
        for (attempt, job) in current.iter().filter(|(a, _)| stop.contains(&a.job)) {
            let (next, _) = transition(job, Input::Cancel);
            sqlx::query("UPDATE jobs SET phase=? WHERE id=?")
                .bind(crate::db::phase_name(next.phase))
                .bind(&attempt.job)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    get(&app.db, id).await
}

/// Inside the attempt's terminal transaction: answer the demands it satisfies
/// (completed or failed; a cancelled attempt answers none) and queue a
/// follow-up for any demand still unserved.
pub(crate) async fn after_attempt(conn: &mut SqliteConnection, job: &str) -> anyhow::Result<()> {
    let (source, phase, verify, started, outcome): (
        String,
        String,
        bool,
        Option<i64>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT library_id,phase,full_scan,started_barrier,outcome FROM jobs WHERE id=?",
    )
    .bind(job)
    .fetch_one(&mut *conn)
    .await?;
    let phase: Phase = serde_json::from_value(serde_json::Value::String(phase))?;
    let result = match phase {
        Phase::Completed => Some(Some(outcome.as_deref() == Some("complete"))),
        Phase::Failed => Some(None),
        _ => None,
    };
    if let Some(result) = result {
        let attempt = Attempt {
            job: job.into(),
            phase: Phase::Running,
            verify,
            started_barrier: started.map(u64::try_from).transpose()?,
            direct: false,
        };
        let pending = demands(conn, &source).await?;
        for (id, status) in core::resolve(&attempt, result, &pending) {
            // The coverage summary is copied so it survives job history pruning.
            sqlx::query("UPDATE scan_demands SET status=?,job_id=?,complete_directories=(SELECT complete_directories FROM jobs WHERE id=?),incomplete_directories=(SELECT incomplete_directories FROM jobs WHERE id=?) WHERE id=?")
                .bind(status_name(status))
                .bind(job)
                .bind(job)
                .bind(job)
                .bind(id)
                .execute(&mut *conn)
                .await?;
        }
    }
    follow_up(conn, &source).await
}

/// After crash recovery: every source with pending demands gets the attempt
/// it needs.
pub(crate) async fn reconcile_all(conn: &mut SqliteConnection) -> anyhow::Result<()> {
    let sources: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT source_id FROM scan_demands WHERE status='pending'")
            .fetch_all(&mut *conn)
            .await?;
    for source in sources {
        follow_up(conn, &source).await?;
    }
    Ok(())
}

pub async fn get(db: &sqlx::SqlitePool, id: &str) -> Result<ScanView, ScanError> {
    let mut tx = db.begin().await?;
    let (library_id, require_complete): (String, bool) =
        sqlx::query_as("SELECT library_id,require_complete FROM scan_requests WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    type Row = (String, String, Option<String>, Option<String>, i64, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT source_id,status,job_id,error,complete_directories,incomplete_directories FROM scan_demands WHERE request_id=? ORDER BY source_id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut sources = Vec::with_capacity(rows.len());
    for (source_id, status, job_id, error, complete, incomplete) in rows {
        sources.push(SourceResult {
            source_id,
            status: status_value(&status)?,
            job_id,
            complete_directories: complete,
            incomplete_directories: incomplete,
            error,
        });
    }
    let statuses: Vec<DemandStatus> = sources.iter().map(|s| s.status).collect();
    Ok(ScanView {
        id: id.into(),
        library_id,
        status: core::request_status(&statuses),
        require_complete,
        complete: !statuses.is_empty() && statuses.iter().all(|s| *s == DemandStatus::Complete),
        sources,
    })
}

/// Recent scan requests, newest first, optionally for one library.
pub async fn list(
    db: &sqlx::SqlitePool,
    library: Option<&str>,
    limit: i64,
) -> Result<Vec<ScanView>, ScanError> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM scan_requests WHERE (?1 IS NULL OR library_id=?1) ORDER BY created_at DESC,rowid DESC LIMIT ?2",
    )
    .bind(library)
    .bind(limit.clamp(1, 200))
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(get(db, &id).await?);
    }
    Ok(out)
}
