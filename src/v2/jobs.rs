//! Durable job transport. Ownership and phase changes use production core
//! policies; current authorization, transition and replay receipt are atomic.
use super::{PageQuery, Problem, Scope, auth::Caller, etag, idempotency_key, page};
use crate::{App, db::begin_write};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use playscale_core::{
    access::{self, Idempotent, Permission},
    jobs::{Input, Job, transition},
};
use serde_json::{Value, json};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/jobs", get(list))
        .route("/jobs/{id}", get(get_job))
        .route("/jobs/{id}/cancel", post(cancel))
        .route("/jobs/{id}/retry", post(retry))
}
#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    revision: i64,
    kind: String,
    phase: String,
    attempt: i64,
    error: Option<String>,
    result_id: Option<String>,
    requester_id: String,
}
impl Row {
    fn wire(&self) -> Value {
        json!({"id":self.id,"revision":self.revision.to_string(),"kind":self.kind,"phase":self.phase,"attempt_generation":self.attempt.to_string(),"progress":Value::Null,"error_code":self.error.as_ref().map(|_|"worker_failed"),"result_ids":self.result_id.iter().collect::<Vec<_>>(),"requester_id":self.requester_id})
    }
    fn response(&self) -> Response {
        let mut r = Json(self.wire()).into_response();
        r.headers_mut()
            .insert(header::ETAG, etag(self.revision as u64));
        r
    }
}
async fn row(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    principal: &access::Principal,
) -> Result<Row, Problem> {
    let row: Row = sqlx::query_as("SELECT * FROM api_jobs WHERE id=?")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(Problem::not_found)?;
    if !access::owns_job(principal, &row.requester_id) {
        return Err(Problem::not_found());
    }
    Ok(row)
}
pub async fn list(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    let limit = q.limit()?;
    let rows:Vec<Row>=sqlx::query_as("SELECT * FROM api_jobs WHERE id>? AND (? OR (requester_id!='legacy' AND requester_id=?)) ORDER BY id LIMIT ?")
 .bind(q.after()?).bind(caller.principal.is_admin()).bind(&caller.principal.id).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let rows = rows.into_iter().map(|r| (r.id.clone(), r.wire())).collect();
    let body = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(body).into_response())
}
pub async fn get_job(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    let row = row(&mut tx, &id, &caller.principal).await?;
    tx.commit().await?;
    Ok(row.response())
}
pub async fn cancel(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    control(app, caller, id, headers, false).await
}
pub async fn retry(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    control(app, caller, id, headers, true).await
}
async fn control(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
    retry: bool,
) -> Result<Response, Problem> {
    caller.require(Permission::ProcessingRequest)?;
    let key = idempotency_key(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProcessingRequest))
        .await?;
    let old = row(&mut tx, &id, &principal).await?;
    let record = Scope {
        principal: &principal.id,
        operation: if retry { "retryJob" } else { "cancelJob" },
        target: &id,
        key: &key,
    };
    let digest = super::sha256(b"");
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|r| &r.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        let stored = stored.unwrap();
        let body = stored.body.unwrap_or_default();
        let mut r = (stored.status, Json(body.clone())).into_response();
        if let Some(rev) = body["revision"].as_str().and_then(|s| s.parse().ok()) {
            r.headers_mut().insert(header::ETAG, etag(rev));
        }
        r.headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(r);
    }
    let state = Job {
        phase: serde_json::from_value(json!(old.phase)).map_err(Problem::internal)?,
        attempt: old.attempt.try_into().map_err(Problem::internal)?,
    };
    if retry && !state.retryable() {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "not_retryable",
            "Only failed or cancelled jobs can retry",
        ));
    }
    if retry && old.kind == "scan" {
        let active:i64=sqlx::query_scalar("SELECT count(*) FROM jobs WHERE library_id=(SELECT library_id FROM jobs WHERE id=?) AND phase IN ('queued','running','cancelling')")
            .bind(&id).fetch_one(&mut *tx).await?;
        playscale_core::catalog::library_idle(active).map_err(|_| {
            Problem::new(
                StatusCode::CONFLICT,
                "source_busy",
                "A newer scan is still active for this source",
            )
        })?;
    }
    let (next, _) = transition(&state, if retry { Input::Retry } else { Input::Cancel });
    if next != state {
        if old.kind == "scan" {
            sqlx::query("UPDATE jobs SET phase=?,error=NULL WHERE id=?")
                .bind(crate::db::phase_name(next.phase))
                .bind(&id)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("UPDATE processing_jobs SET phase=?,error=NULL,cache_cleaned=0,updated_at=? WHERE id=?").bind(crate::db::phase_name(next.phase)).bind(crate::now()).bind(&id).execute(&mut *tx).await?;
        }
    }
    let updated = row(&mut tx, &id, &principal).await?;
    record
        .save(
            &mut tx,
            &digest,
            StatusCode::OK,
            Some(&updated.wire()),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(updated.response())
}
