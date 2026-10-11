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
        .route(
            "/sources/{id}/relocation-plans",
            axum::routing::post(preview_relocation),
        )
        .route(
            "/sources/{id}/relocation",
            axum::routing::post(commit_relocation),
        )
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
    // Only the logical definition (and its finished scan history) is removed;
    // the service refuses while scan demands are pending.
    service::delete_library_in(&mut tx, &id, library.revision)
        .await
        .map_err(error)?;
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
    // A source still holding cataloged files is disabled rather than deleted
    // (originals and history are retained) and its current view returned.
    let outcome = service::delete_source_in(&mut tx, &id, source.revision)
        .await
        .map_err(error)?;
    if outcome == service::SourceRemoval::Disabled {
        let value = wire(service::source_in(&mut tx, &id).await.map_err(error)?)?;
        tx.commit().await?;
        return Ok(tagged(StatusCode::OK, value));
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelocationInput {
    new_root_path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelocationCommit {
    plan_token: String,
}
fn relocation_error(e: service::RelocationError) -> Problem {
    match e {
        service::RelocationError::Library(e) => error(e),
        service::RelocationError::CandidateMismatch(_) => Problem::new(
            StatusCode::CONFLICT,
            "candidate_mismatch",
            "Candidate does not contain unchanged source media",
        ),
        service::RelocationError::Conflict(_) => Problem::new(
            StatusCode::CONFLICT,
            "relocation_changed",
            "Source or candidate changed; preview again",
        ),
    }
}
pub async fn preview_relocation(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<RelocationInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let expected = if_match(&headers)?;
    // Exact retry is independent of whether the candidate is currently mounted.
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "previewSourceRelocation",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|s| &s.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        return Ok((StatusCode::OK, Json(stored.unwrap().body)).into_response());
    }
    tx.rollback().await?;
    if body.value.new_root_path.is_empty() || body.value.new_root_path.len() > 4096 {
        return Err(Problem::invalid("invalid_root", "Invalid root path"));
    }
    let candidate = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::administration::candidate_root(&app, body.value.new_root_path.into()),
    )
    .await
    .map_err(|_| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "source_timeout",
            "Source observation timed out",
        )
    })?
    .map_err(|_| Problem::invalid("invalid_root", "Candidate root unavailable"))?;
    let current = service::get_source(&app.db, &id).await.map_err(error)?;
    access::revise(current.revision, expected, true)?;
    let plan = service::preview_relocation(&app, &id, candidate)
        .await
        .map_err(relocation_error)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "previewSourceRelocation",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|s| &s.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        return Ok((StatusCode::OK, Json(stored.unwrap().body)).into_response());
    }
    let current = service::source_in(&mut tx, &id).await.map_err(error)?;
    access::revise(current.revision, plan.expected_revision, true)?;
    access::revise(plan.expected_revision, expected, true)?;
    let token = crate::new_id();
    let expires = crate::now() + 300;
    sqlx::query("INSERT INTO catalog_receipts(id,kind,created_at,document_json) VALUES (?,'source:relocation_plan',?,?)").bind(&token).bind(crate::now()).bind(serde_json::json!({"principal":principal.id,"expires":expires,"plan":plan}).to_string()).execute(&mut *tx).await?;
    let value = serde_json::json!({"plan_token":token,"expires_at":super::timestamp(expires),"affected_ids":[id],"warnings":[]});
    record
        .save(&mut tx, &body.digest, StatusCode::OK, Some(&value), None)
        .await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}
pub async fn commit_relocation(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<RelocationCommit>,
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
        operation: "commitSourceRelocation",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|s| &s.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let s = stored.unwrap();
        return Ok(tagged(s.status, s.body.unwrap()));
    }
    let document: Option<String> = sqlx::query_scalar(
        "SELECT document_json FROM catalog_receipts WHERE id=? AND kind='source:relocation_plan'",
    )
    .bind(&body.value.plan_token)
    .fetch_optional(&mut *tx)
    .await?;
    let document: Value = serde_json::from_str(
        &document.ok_or_else(|| Problem::invalid("invalid_plan", "Unknown relocation plan"))?,
    )
    .map_err(Problem::internal)?;
    if document["principal"] != principal.id
        || document["expires"]
            .as_i64()
            .is_none_or(|e| e <= crate::now())
    {
        return Err(Problem::invalid(
            "invalid_plan",
            "Relocation plan expired or belongs to another principal",
        ));
    }
    let plan: playscale_core::sources::RelocationPlan =
        serde_json::from_value(document["plan"].clone()).map_err(Problem::internal)?;
    if plan.source != id {
        return Err(access::AccessError::StaleRevision.into());
    }
    tx.rollback().await?;
    let facts = service::observe_relocation(&app, &plan)
        .await
        .map_err(relocation_error)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "commitSourceRelocation",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|s| &s.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let s = stored.unwrap();
        return Ok(tagged(s.status, s.body.unwrap()));
    }
    if document["expires"]
        .as_i64()
        .is_none_or(|e| e <= crate::now())
    {
        return Err(Problem::invalid("invalid_plan", "Relocation plan expired"));
    }
    let value = wire(
        service::commit_relocation_in(&mut tx, &plan, facts)
            .await
            .map_err(relocation_error)?,
    )?;
    record
        .save(&mut tx, &body.digest, StatusCode::OK, Some(&value), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, value))
}
