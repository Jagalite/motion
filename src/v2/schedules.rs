//! Logical-library schedules; claim and durable demand creation share a writer.
use super::{Body, PageQuery, Problem, Scope, auth::Caller, etag, idempotency_key, if_match, page};
use crate::{App, db::begin_write};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::access::{self, Idempotent, Permission};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/schedules", get(list).post(create))
        .route("/schedules/{id}", get(read).put(replace).delete(delete))
}
#[derive(Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Input {
    library_id: String,
    task: String,
    interval_seconds: i64,
    enabled: bool,
}
#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    revision: i64,
    library_id: String,
    task: String,
    interval_seconds: i64,
    enabled: bool,
    next_run: i64,
    requester_id: String,
    credential_hash: Option<String>,
    ingress_id: Option<String>,
}
impl Row {
    fn wire(&self) -> Value {
        json!({"id":self.id,"revision":self.revision.to_string(),"library_id":self.library_id,"task":self.task,"interval_seconds":self.interval_seconds,"enabled":self.enabled})
    }
}
fn tagged(value: Value, status: StatusCode) -> Response {
    let revision = value["revision"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let mut r = (status, Json(value)).into_response();
    r.headers_mut().insert(header::ETAG, etag(revision));
    r
}
async fn library(
    conn: &mut sqlx::SqliteConnection,
    p: &access::Principal,
    id: &str,
) -> Result<(), Problem> {
    if !access::catalog_scope(p).library(id) {
        return Err(Problem::not_found());
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catalog_libraries WHERE id=?)")
            .bind(id)
            .fetch_one(conn)
            .await?;
    if !exists {
        return Err(Problem::not_found());
    }
    Ok(())
}
fn validate(i: &Input, p: &access::Principal) -> Result<(), Problem> {
    playscale_core::maintenance::Schedule {
        next_run: 0,
        interval: i.interval_seconds,
    }
    .claim(0)
    .map_err(|code| Problem::invalid(code, "Interval must be 60 through 31536000 seconds"))?;
    match i.task.as_str() {
        "scan" => {}
        "metadata_refresh" => {
            if !p.allows(Permission::CatalogWrite) {
                return Err(access::AccessError::Forbidden(Permission::CatalogWrite).into());
            }
        }
        "preview" | "optimize" => {
            return Err(Problem::invalid(
                "schedule_task_unavailable",
                "This server has no scheduled preview or optimization service",
            ));
        }
        _ => {
            return Err(Problem::invalid(
                "invalid_schedule_task",
                "Unknown schedule task",
            ));
        }
    }
    Ok(())
}
async fn load(
    conn: &mut sqlx::SqliteConnection,
    p: &access::Principal,
    id: &str,
) -> Result<Row, Problem> {
    let row: Row = sqlx::query_as("SELECT * FROM api_schedules WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(Problem::not_found)?;
    library(conn, p, &row.library_id).await?;
    Ok(row)
}
async fn list(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let all = matches!(scope, access::CatalogScope::All);
    let ids = match scope {
        access::CatalogScope::Libraries(ids) => {
            serde_json::to_string(&ids).map_err(Problem::internal)?
        }
        _ => "[]".into(),
    };
    let mut tx = app.db.begin().await?;
    let rows:Vec<Row>=sqlx::query_as("SELECT * FROM api_schedules WHERE id>? AND (? OR library_id IN (SELECT value FROM json_each(?))) ORDER BY id LIMIT ?").bind(q.after()?).bind(all).bind(ids).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let result = page(
        &mut tx,
        &caller.principal,
        rows.into_iter().map(|r| (r.id.clone(), r.wire())).collect(),
        limit,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}
async fn read(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let mut tx = app.db.begin().await?;
    let row = load(&mut tx, &caller.principal, &id).await?;
    tx.commit().await?;
    Ok(tagged(row.wire(), StatusCode::OK))
}
async fn create(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let i = body.value;
    validate(&i, &p)?;
    library(&mut tx, &p, &i.library_id).await?;
    let record = Scope {
        principal: &p.id,
        operation: "createSchedule",
        target: "",
        key: &key,
    };
    let old = record.load(&mut tx).await?;
    if access::idempotency(old.as_ref().map(|s| &s.record), &body.digest, crate::now())?
        == Idempotent::Replay
    {
        let old = old.unwrap();
        let mut r = tagged(old.body.unwrap_or_default(), old.status);
        r.headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(r);
    }
    let id = crate::new_id();
    sqlx::query("INSERT INTO api_schedules(id,library_id,task,interval_seconds,enabled,next_run,requester_id,credential_hash,ingress_id) VALUES (?,?,?,?,?,?,?,?,?)").bind(&id).bind(i.library_id).bind(i.task).bind(i.interval_seconds).bind(i.enabled).bind(crate::now()+i.interval_seconds).bind(&p.id).bind(caller.credential.as_ref().map(|(h,_)|h)).bind(&caller.ingress).execute(&mut *tx).await?;
    let value = load(&mut tx, &p, &id).await?.wire();
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(value, StatusCode::CREATED))
}
async fn replace(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let old = load(&mut tx, &p, &id).await?;
    let i = body.value;
    validate(&i, &p)?;
    library(&mut tx, &p, &i.library_id).await?;
    let changed = old.library_id != i.library_id
        || old.task != i.task
        || old.interval_seconds != i.interval_seconds
        || old.enabled != i.enabled;
    if let access::Revised::Next(rev) = access::revise(old.revision as u64, expected, changed)? {
        sqlx::query("UPDATE api_schedules SET revision=?,library_id=?,task=?,interval_seconds=?,enabled=?,next_run=?,requester_id=?,credential_hash=?,ingress_id=? WHERE id=?").bind(rev as i64).bind(i.library_id).bind(i.task).bind(i.interval_seconds).bind(i.enabled).bind(crate::now()+i.interval_seconds).bind(&p.id).bind(caller.credential.as_ref().map(|(h,_)|h)).bind(&caller.ingress).bind(&id).execute(&mut *tx).await?;
    }
    let value = load(&mut tx, &p, &id).await?.wire();
    tx.commit().await?;
    Ok(tagged(value, StatusCode::OK))
}
async fn delete(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let row = load(&mut tx, &p, &id).await?;
    access::revise(row.revision as u64, expected, true)?;
    sqlx::query("DELETE FROM api_schedules WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(crate) async fn tick(app: &App) -> anyhow::Result<()> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM api_schedules WHERE enabled=1 AND next_run<=? ORDER BY next_run,id LIMIT 100").bind(crate::now()).fetch_all(&app.db).await?;
    for id in ids {
        let _guard = app.jobs.lock().await;
        let mut tx = begin_write(&app.db).await?;
        let row: Option<Row> =
            sqlx::query_as("SELECT * FROM api_schedules WHERE id=? AND enabled=1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(row) = row else { continue };
        let Some(next) = (playscale_core::maintenance::Schedule {
            next_run: row.next_run,
            interval: row.interval_seconds,
        })
        .claim(crate::now())
        .map_err(anyhow::Error::msg)?
        else {
            continue;
        };
        let p = crate::metadata_jobs::principal(
            &mut tx,
            &row.requester_id,
            row.credential_hash.as_deref(),
            row.ingress_id.as_deref(),
            Permission::SourcesManage,
        )
        .await;
        let p = match p {
            Ok(p) if access::catalog_scope(&p).library(&row.library_id) => p,
            _ => {
                sqlx::query("UPDATE api_schedules SET enabled=0,revision=revision+1 WHERE id=?")
                    .bind(&row.id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                continue;
            }
        };
        let mut more = false;
        match row.task.as_str() {
            "scan" => {
                match crate::scans::request_in(&mut tx, &row.library_id, None, false, false).await {
                    Ok(_) => {}
                    Err(
                        crate::scans::ScanError::NoSources
                        | crate::scans::ScanError::NotFound
                        | crate::scans::ScanError::UnknownSource(_),
                    ) => {
                        sqlx::query(
                            "UPDATE api_schedules SET enabled=0,revision=revision+1 WHERE id=?",
                        )
                        .bind(&row.id)
                        .execute(&mut *tx)
                        .await?;
                    }
                    Err(crate::scans::ScanError::Storage(error)) => return Err(error),
                }
            }
            "metadata_refresh" if p.allows(Permission::CatalogWrite) => {
                let after: Option<String> = sqlx::query_scalar(
                    "SELECT after_item FROM schedule_dispatch_progress WHERE schedule_id=?",
                )
                .bind(&row.id)
                .fetch_optional(&mut *tx)
                .await?;
                let mut items:Vec<String>=sqlx::query_scalar("SELECT DISTINCT e.item_id FROM editions e JOIN media_files f ON f.edition_id=e.id JOIN library_sources ls ON ls.source_id=f.library_id WHERE ls.library_id=? AND f.available=1 AND f.generated=0 AND e.item_id>? ORDER BY e.item_id LIMIT 201").bind(&row.library_id).bind(after.unwrap_or_default()).fetch_all(&mut *tx).await?;
                more = items.len() > 200;
                items.truncate(200);
                let last = items.last().cloned();
                for item in items {
                    // Shared-item mutation requires every containing library.
                    if super::catalog::curatable_item(
                        &mut tx,
                        &access::catalog_scope(&p),
                        &item,
                        false,
                    )
                    .await
                    .is_err()
                    {
                        continue;
                    }
                    sqlx::query("INSERT INTO metadata_jobs(id,item_id,requester_id,credential_hash,ingress_id,phase,created_at) SELECT ?,?,?,?,?,'queued',? WHERE NOT EXISTS(SELECT 1 FROM metadata_jobs WHERE item_id=? AND phase IN ('queued','running','cancelling'))").bind(crate::new_id()).bind(&item).bind(&p.id).bind(&row.credential_hash).bind(&row.ingress_id).bind(crate::now()).bind(&item).execute(&mut *tx).await?;
                }
                if more {
                    sqlx::query("INSERT INTO schedule_dispatch_progress(schedule_id,after_item) VALUES (?,?) ON CONFLICT(schedule_id) DO UPDATE SET after_item=excluded.after_item").bind(&row.id).bind(last).execute(&mut *tx).await?;
                } else {
                    sqlx::query("DELETE FROM schedule_dispatch_progress WHERE schedule_id=?")
                        .bind(&row.id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            _ => {
                sqlx::query("UPDATE api_schedules SET enabled=0,revision=revision+1 WHERE id=?")
                    .bind(&row.id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE api_schedules SET next_run=? WHERE id=?")
            .bind(if more { row.next_run } else { next.next_run })
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}
