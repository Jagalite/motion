//! Logical-library/source transport. The service applies core source policies;
//! reauthorization, writes and retry acknowledgements share a reserved writer.
use super::{Body, PageQuery, Problem, Scope, auth::Caller, etag, idempotency_key, if_match, page};
use crate::{App, db::begin_write, libraries as service};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::{
    access::{self, Idempotent, Permission},
    sources::{LibraryKind, SourceError},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/libraries", get(list_libraries).post(create_library))
        .route(
            "/libraries/{id}",
            get(get_library).put(replace_library).delete(delete_library),
        )
        .route("/sources", get(list_sources).post(create_source))
        .route("/sources/{id}", get(get_source).delete(delete_source))
}

fn error(e: service::LibraryError) -> Problem {
    match e {
        service::LibraryError::NotFound => Problem::not_found(),
        service::LibraryError::Rejected(SourceError::StaleRevision) => {
            access::AccessError::StaleRevision.into()
        }
        service::LibraryError::Rejected(SourceError::UnknownSource(_)) => Problem::not_found(),
        service::LibraryError::Rejected(_) => Problem::invalid(
            "invalid_source_configuration",
            "Invalid library or source configuration",
        ),
        service::LibraryError::Overlaps(_) => Problem::new(
            StatusCode::CONFLICT,
            "source_overlap",
            "Root overlaps a registered source",
        ),
        service::LibraryError::Busy => Problem::new(
            StatusCode::CONFLICT,
            "source_busy",
            "Source is still in use",
        ),
        service::LibraryError::Storage(e) => Problem::internal(e),
    }
}

fn wire<T: Serialize>(value: T) -> Result<Value, Problem> {
    let mut value = serde_json::to_value(value).map_err(Problem::internal)?;
    for field in ["revision", "binding_revision"] {
        if let Some(revision) = value.get(field).and_then(Value::as_u64) {
            value[field] = Value::String(revision.to_string());
        }
    }
    Ok(value)
}

fn tagged(status: StatusCode, body: Value) -> Response {
    let revision = body["revision"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, etag(revision));
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryInput {
    name: String,
    kind: LibraryKind,
    language: String,
    source_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInput {
    name: String,
    root_path: String,
    exclusions: Vec<String>,
}

pub async fn list_libraries(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = access::catalog_scope(&caller.principal);
    let all = matches!(scope, access::CatalogScope::All);
    let ids = match &scope {
        access::CatalogScope::Libraries(ids) => {
            serde_json::to_string(ids).map_err(Problem::internal)?
        }
        _ => "[]".into(),
    };
    let mut tx = app.db.begin().await?;
    let limit = q.limit()?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM catalog_libraries WHERE id>? AND (? OR id IN (SELECT value FROM json_each(?))) ORDER BY id LIMIT ?")
        .bind(q.after()?).bind(all).bind(ids).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let mut rows = Vec::new();
    for id in ids {
        rows.push((
            id.clone(),
            wire(service::library_view(&mut tx, &id).await.map_err(error)?)?,
        ));
    }
    let result = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}

pub async fn get_library(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    if !access::catalog_scope(&caller.principal).library(&id) {
        return Err(Problem::not_found());
    }
    Ok(tagged(
        StatusCode::OK,
        wire(service::get_library(&app.db, &id).await.map_err(error)?)?,
    ))
}

pub async fn create_library(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<LibraryInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createLibrary",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|r| &r.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let stored = stored.unwrap();
        return Ok(tagged(stored.status, stored.body.unwrap_or_default()));
    }
    let input = body.value;
    let value = wire(
        service::save_library_in(
            &mut tx,
            None,
            &input.name,
            input.kind,
            &input.language,
            &input.source_ids,
        )
        .await
        .map_err(error)?,
    )?;
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
    Ok(tagged(StatusCode::CREATED, value))
}

pub async fn replace_library(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<LibraryInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if !access::catalog_scope(&principal).library(&id) {
        return Err(Problem::not_found());
    }
    let input = body.value;
    let value = wire(
        service::save_library_in(
            &mut tx,
            Some((&id, expected)),
            &input.name,
            input.kind,
            &input.language,
            &input.source_ids,
        )
        .await
        .map_err(error)?,
    )?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, value))
}

pub async fn delete_library(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if !access::catalog_scope(&principal).library(&id) {
        return Err(Problem::not_found());
    }
    let library = service::library_view(&mut tx, &id).await.map_err(error)?;
    access::revise(library.revision, expected, true)?;
    // Only the logical definition is removed. Storage sources and media stay owned by their service.
    sqlx::query("DELETE FROM catalog_libraries WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn list_sources(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let mut tx = app.db.begin().await?;
    let limit = q.limit()?;
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM sources WHERE id>? ORDER BY id LIMIT ?")
            .bind(q.after()?)
            .bind(i64::from(limit) + 1)
            .fetch_all(&mut *tx)
            .await?;
    let mut rows = Vec::new();
    for id in ids {
        rows.push((
            id.clone(),
            wire(service::source_in(&mut tx, &id).await.map_err(error)?)?,
        ));
    }
    let result = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}

pub async fn get_source(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    Ok(tagged(
        StatusCode::OK,
        wire(service::get_source(&app.db, &id).await.map_err(error)?)?,
    ))
}

pub async fn create_source(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<SourceInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    // Replay before filesystem observation; an acknowledged source can be offline.
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createSource",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|r| &r.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let stored = stored.unwrap();
        return Ok(tagged(stored.status, stored.body.unwrap_or_default()));
    }
    tx.rollback().await?;
    let input = body.value;
    if input.root_path.is_empty() || input.root_path.len() > 4096 {
        return Err(Problem::invalid("invalid_root", "Invalid source root"));
    }
    let candidate = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::administration::candidate_root(&app, input.root_path.into()),
    )
    .await
    .map_err(|_| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "source_timeout",
            "Source verification timed out",
        )
    })?
    .map_err(|_| Problem::invalid("invalid_root", "Source root is unavailable or invalid"))?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createSource",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|r| &r.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let stored = stored.unwrap();
        return Ok(tagged(stored.status, stored.body.unwrap_or_default()));
    }
    let value = wire(
        service::register_source_in(&mut tx, candidate, &input.name, &input.exclusions)
            .await
            .map_err(error)?,
    )?;
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
    Ok(tagged(StatusCode::CREATED, value))
}

pub async fn delete_source(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let source = service::source_in(&mut tx, &id).await.map_err(error)?;
    access::revise(source.revision, expected, true)?;
    let uses: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM library_sources WHERE source_id=?1)+(SELECT count(*) FROM media_files WHERE library_id=?1)+(SELECT count(*) FROM jobs WHERE library_id=?1 AND phase IN ('queued','running','cancelling'))").bind(&id).fetch_one(&mut *tx).await?;
    playscale_core::catalog::library_idle(uses).map_err(|_| error(service::LibraryError::Busy))?;
    sqlx::query("DELETE FROM libraries WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
