//! Public scan demands. Domain admission/cancellation stay in core::demands;
//! authorization, durable effects and retry acknowledgement share one writer.
use super::{Body, PageQuery, Problem, Scope, auth::Caller, etag, idempotency_key, page};
use crate::{App, scans as service};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use playscale_core::access::{self, Idempotent, Permission};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/libraries/{id}/scans", post(create))
        .route("/scans", get(list))
        .route("/scans/{id}", get(read))
        .route("/scans/{id}/cancel", post(cancel))
}
fn error(e: service::ScanError) -> Problem {
    match e {
        service::ScanError::NotFound => Problem::not_found(),
        service::ScanError::UnknownSource(_) | service::ScanError::NoSources => Problem::invalid(
            "invalid_sources",
            "Select nonempty sources belonging to this library",
        ),
        service::ScanError::Storage(e) => Problem::internal(e),
    }
}
fn tagged(status: StatusCode, value: Value) -> Response {
    let rev = value["revision"].as_str().unwrap().parse().unwrap();
    let mut r = (status, Json(value)).into_response();
    r.headers_mut().insert(header::ETAG, etag(rev));
    r
}
async fn visible(
    tx: &mut sqlx::SqliteConnection,
    principal: &access::Principal,
    id: &str,
) -> Result<(), Problem> {
    let library: Option<String> =
        sqlx::query_scalar("SELECT library_id FROM scan_requests WHERE id=?")
            .bind(id)
            .fetch_optional(tx)
            .await?;
    if !library.is_some_and(|id| access::catalog_scope(principal).library(&id)) {
        return Err(Problem::not_found());
    }
    Ok(())
}
async fn wire(
    tx: &mut sqlx::SqliteConnection,
    id: &str,
    principal: &access::Principal,
) -> Result<Value, Problem> {
    let view = service::get_in(tx, id).await.map_err(error)?;
    let (revision, created): (i64, i64) =
        sqlx::query_as("SELECT revision,created_at FROM scan_requests WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let mut sources = Vec::new();
    let mut jobs = Vec::new();
    let mut running = false;
    for s in view.sources {
        let job: Option<(String, i64, String, i64, i64)> = if let Some(id) = s.job_id.as_ref() {
            sqlx::query_as(
                "SELECT id,attempt,phase,reused_files,inspected_files FROM jobs WHERE id=?",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        } else if s.status == playscale_core::demands::DemandStatus::Pending {
            let (barrier, verify): (i64, bool) = sqlx::query_as(
                "SELECT barrier,verify FROM scan_demands WHERE request_id=? AND source_id=?",
            )
            .bind(id)
            .bind(&s.source_id)
            .fetch_one(&mut *tx)
            .await?;
            let demand = playscale_core::demands::Demand {
                id: String::new(),
                barrier: barrier as u64,
                verify,
                require_complete: view.require_complete,
                status: s.status,
            };
            type AttemptRow = (String, i64, String, i64, i64, bool, Option<i64>, bool);
            let candidates:Vec<AttemptRow>=sqlx::query_as("SELECT id,attempt,phase,reused_files,inspected_files,full_scan,started_barrier,direct_request FROM jobs WHERE library_id=? AND phase IN ('queued','running','cancelling') ORDER BY created_at,id").bind(&s.source_id).fetch_all(&mut *tx).await?;
            let mut selected = None;
            for (id, generation, phase, reused, inspected, verify, barrier, direct) in candidates {
                let attempt = playscale_core::demands::Attempt {
                    job: id.clone(),
                    phase: serde_json::from_value(json!(phase)).map_err(Problem::internal)?,
                    verify,
                    started_barrier: barrier.map(|b| b as u64),
                    direct,
                };
                if playscale_core::demands::can_satisfy(&attempt, &demand) {
                    selected = Some((id, generation, phase, reused, inspected));
                    break;
                }
            }
            selected
        } else {
            None
        };
        let (generation, phase, observed, verified) = job
            .as_ref()
            .map(|(id, a, p, r, i)| {
                jobs.push(id.clone());
                (a.to_string(), p.as_str(), r + i, *i)
            })
            .unwrap_or(("0".into(), "queued", 0, 0));
        let status = match s.status {
            playscale_core::demands::DemandStatus::Pending => {
                if phase == "running" || phase == "cancelling" {
                    running = true;
                    "running"
                } else {
                    "queued"
                }
            }
            playscale_core::demands::DemandStatus::Complete => "complete",
            playscale_core::demands::DemandStatus::Partial => "partial",
            playscale_core::demands::DemandStatus::Failed => "failed",
            playscale_core::demands::DemandStatus::Cancelled => "cancelled",
        };
        sources.push(json!({"source_id":s.source_id,"attempt_generation":generation,"status":status,"complete_directories":s.complete_directories.to_string(),"incomplete_directories":s.incomplete_directories.to_string(),"observed_files":observed.to_string(),"verified_files":verified.to_string(),"error_codes":s.error.into_iter().collect::<Vec<_>>()}));
    }
    // Shared physical attempts have no single requester. Only expose job IDs
    // the jobs API authorizes; restricted clients manage their own demand.
    if !principal.is_admin() {
        jobs.clear();
    }
    jobs.sort();
    jobs.dedup();
    let status = match view.status {
        playscale_core::demands::DemandStatus::Pending => {
            if running {
                "running"
            } else {
                "queued"
            }
        }
        playscale_core::demands::DemandStatus::Complete => "complete",
        playscale_core::demands::DemandStatus::Partial => "partial",
        playscale_core::demands::DemandStatus::Failed => "failed",
        playscale_core::demands::DemandStatus::Cancelled => "cancelled",
    };
    Ok(
        json!({"id":view.id,"revision":revision.to_string(),"library_id":view.library_id,"status":status,"require_complete":view.require_complete,"captured_after":super::timestamp(created),"sources":sources,"job_ids":jobs,"complete":view.complete}),
    )
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Incremental,
    Verify,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    mode: Mode,
    source_ids: Option<Vec<String>>,
    require_complete: bool,
}
async fn create(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    if let Some(ids) = &body.value.source_ids
        && (ids.is_empty()
            || ids.len() > 100
            || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len())
    {
        return Err(Problem::invalid(
            "invalid_sources",
            "Select 1-100 distinct sources",
        ));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if !access::catalog_scope(&principal).library(&id) {
        return Err(Problem::not_found());
    }
    let record = Scope {
        principal: &principal.id,
        operation: "createScan",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|r| &r.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let s = stored.unwrap();
        return Ok(tagged(s.status, s.body.unwrap()));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM library_sources WHERE library_id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    if body.value.source_ids.is_none() && count > 100 {
        return Err(Problem::invalid(
            "too_many_sources",
            "Select at most 100 sources",
        ));
    }
    let view = service::request_in(
        &mut tx,
        &id,
        body.value.source_ids.as_deref(),
        matches!(body.value.mode, Mode::Verify),
        body.value.require_complete,
    )
    .await
    .map_err(error)?;
    let value = wire(&mut tx, &view.id, &principal).await?;
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::ACCEPTED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::ACCEPTED, value))
}
async fn read(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let mut tx = app.db.begin().await?;
    visible(&mut tx, &caller.principal, &id).await?;
    let value = wire(&mut tx, &id, &caller.principal).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, value))
}
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
enum Order {
    #[default]
    Id,
    Recent,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    #[serde(default)]
    order: Order,
    library_id: Option<String>,
}
/// Keyset cursor for newest-first pages: the admission sequence (SQLite
/// rowid) of the last row. Rowids grow with every admission, unlike capture
/// times (whole seconds, possibly stepping back with the clock).
fn recent_cursor(sequence: i64) -> String {
    super::catalog::b64_encode(sequence.to_string().as_bytes())
}
fn decode_recent(cursor: &str) -> Result<i64, Problem> {
    super::catalog::b64_decode(cursor)
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Problem::bad("invalid_cursor", "The page cursor is invalid"))
}
async fn list(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<ListQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    if query
        .library_id
        .as_deref()
        .is_some_and(|id| !super::catalog::valid_id(id))
    {
        return Err(Problem::bad(
            "invalid_query",
            "library_id must be a valid ID",
        ));
    }
    let q = PageQuery {
        cursor: query.cursor,
        limit: query.limit,
    };
    let mut tx = app.db.begin().await?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let all = matches!(scope, access::CatalogScope::All);
    let ids = match scope {
        access::CatalogScope::Libraries(ids) => {
            serde_json::to_string(&ids).map_err(Problem::internal)?
        }
        _ => "[]".into(),
    };
    // Authorization and the optional library filter apply before ordering
    // and the limit, so hidden scans never shape a page. Optional predicates
    // are emitted only when present, so SQLite can seek its indexes.
    let scoped = "(? OR library_id IN (SELECT value FROM json_each(?)))";
    let library = if query.library_id.is_some() {
        " AND library_id=?"
    } else {
        ""
    };
    let keyed: Vec<(String, String)> = match query.order {
        Order::Id => {
            let sql = format!(
                "SELECT id FROM scan_requests WHERE id>? AND {scoped}{library} ORDER BY id LIMIT ?"
            );
            let mut rows = sqlx::query_scalar::<_, String>(&sql)
                .bind(q.after()?)
                .bind(all)
                .bind(&ids);
            if let Some(library) = &query.library_id {
                rows = rows.bind(library);
            }
            rows.bind(i64::from(limit) + 1)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|id| (id.clone(), id))
                .collect()
        }
        Order::Recent => {
            // Newest admission first. A scan admitted mid-read has a larger
            // sequence than every cursor, so it never shifts a later page.
            let after = match q.after()? {
                "" => None,
                cursor => Some(decode_recent(cursor)?),
            };
            let before = if after.is_some() { " AND rowid<?" } else { "" };
            let sql = format!(
                "SELECT rowid,id FROM scan_requests WHERE {scoped}{library}{before} ORDER BY rowid DESC LIMIT ?"
            );
            let mut rows = sqlx::query_as::<_, (i64, String)>(&sql)
                .bind(all)
                .bind(&ids);
            if let Some(library) = &query.library_id {
                rows = rows.bind(library);
            }
            if let Some(after) = after {
                rows = rows.bind(after);
            }
            rows.bind(i64::from(limit) + 1)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(sequence, id)| (recent_cursor(sequence), id))
                .collect()
        }
    };
    let mut rows = Vec::new();
    for (cursor, id) in keyed {
        rows.push((cursor, wire(&mut tx, &id, &caller.principal).await?));
    }
    let value = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}
async fn cancel(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let digest = super::sha256(b"");
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    visible(&mut tx, &principal, &id).await?;
    let record = Scope {
        principal: &principal.id,
        operation: "cancelScan",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|r| &r.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        let s = stored.unwrap();
        return Ok(tagged(s.status, s.body.unwrap()));
    }
    service::cancel_in(&mut tx, &id).await.map_err(error)?;
    let value = wire(&mut tx, &id, &principal).await?;
    record
        .save(&mut tx, &digest, StatusCode::OK, Some(&value), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, value))
}
