use crate::{
    App,
    api::{ApiError, admin, json},
    new_id,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct CatalogItem {
    pub id: String,
    pub title: String,
    pub media_type: String,
    pub parent_id: Option<String>,
    pub number: Option<i64>,
    pub revision: i64,
}
const SELECT: &str = "SELECT i.id,i.title,coalesce(s.media_type,'unclassified') AS media_type,s.parent_id,s.number,coalesce(s.revision,0) AS revision FROM items i LEFT JOIN item_structure s ON s.item_id=i.id";
/// Retired (merged) IDs resolve to their live work so old links stay explainable.
async fn load(app: &App, id: &str) -> Result<CatalogItem, ApiError> {
    let id = match crate::curation::resolve(&app.db, id)
        .await
        .map_err(ApiError::internal)?
    {
        Some(playscale_core::identity::Resolved::Live(id))
        | Some(playscale_core::identity::Resolved::Alias { to: id, .. }) => id,
        None => return Err(ApiError::not_found()),
    };
    Ok(sqlx::query_as(&format!("{SELECT} WHERE i.id=?"))
        .bind(id)
        .fetch_one(&app.db)
        .await?)
}
#[derive(Deserialize, IntoParams)]
pub struct Browse {
    pub source: Option<String>,
    pub external_id: Option<String>,
    pub parent_id: Option<String>,
    pub media_type: Option<String>,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}
#[derive(Serialize, ToSchema)]
pub struct Page {
    pub items: Vec<CatalogItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}
#[utoipa::path(operation_id="browse_catalog",get,path="/api/v1/catalog/items",params(Browse),responses((status=200,description="Logical and discovered items; filter by parent or media type",body=Page),(status=400,description="Invalid query",body=crate::api::ErrorBody)))]
pub async fn browse(
    State(app): State<App>,
    Query(q): Query<Browse>,
) -> Result<Json<Page>, ApiError> {
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(50);
    if q.source.is_some() != q.external_id.is_some()
        || q.source
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 64)
        || q.external_id
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
        || offset < 0
        || !(1..=200).contains(&limit)
        || q.media_type
            .as_deref()
            .is_some_and(|k| !["movie", "series", "season", "episode", "unclassified"].contains(&k))
    {
        return Err(ApiError::bad("Invalid catalog filter or page bounds"));
    }
    let filter = " WHERE i.id NOT IN (SELECT alias_id FROM item_aliases) AND (? IS NULL OR s.parent_id=?) AND (? IS NULL OR coalesce(s.media_type,'unclassified')=?) AND (? IS NULL OR EXISTS (SELECT 1 FROM metadata_documents m WHERE m.item_id=i.id AND m.source=? AND m.external_id=?))";
    let mut tx = app.db.begin().await?;
    let total = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM items i LEFT JOIN item_structure s ON s.item_id=i.id {filter}"
    ))
    .bind(&q.parent_id)
    .bind(&q.parent_id)
    .bind(&q.media_type)
    .bind(&q.media_type)
    .bind(&q.source)
    .bind(&q.source)
    .bind(&q.external_id)
    .fetch_one(&mut *tx)
    .await?;
    let items = sqlx::query_as(&format!(
        "{SELECT}{filter} ORDER BY s.number,i.title,i.id LIMIT ? OFFSET ?"
    ))
    .bind(&q.parent_id)
    .bind(&q.parent_id)
    .bind(&q.media_type)
    .bind(&q.media_type)
    .bind(&q.source)
    .bind(&q.source)
    .bind(&q.external_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(Page {
        items,
        total,
        offset,
        limit,
    }))
}
#[utoipa::path(operation_id="get_catalog_item",get,path="/api/v1/catalog/items/{id}",params(("id"=String,Path)),responses((status=200,description="Logical item structure",body=CatalogItem),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<CatalogItem>, ApiError> {
    Ok(Json(load(&app, &id).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub title: String,
    pub media_type: String,
    pub parent_id: Option<String>,
    pub number: Option<i64>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Structure {
    pub expected_revision: i64,
    pub media_type: String,
    pub parent_id: Option<String>,
    pub number: Option<i64>,
}
fn conflict() -> ApiError {
    ApiError::conflict(
        "catalog_conflict",
        "Structure revision or sibling number conflicts with the current catalog",
    )
}
async fn validate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    kind: &str,
    parent: &Option<String>,
    number: Option<i64>,
) -> Result<(), ApiError> {
    if parent.as_deref() == Some(id) {
        return Err(ApiError::bad("An item cannot parent itself"));
    }
    let parent_kind: Option<String> = if let Some(parent) = parent {
        Some(
            sqlx::query_scalar("SELECT media_type FROM item_structure WHERE item_id=?")
                .bind(parent)
                .fetch_one(&mut **tx)
                .await?,
        )
    } else {
        None
    };
    let child_types: Vec<String> =
        sqlx::query_scalar("SELECT media_type FROM item_structure WHERE parent_id=?")
            .bind(id)
            .fetch_all(&mut **tx)
            .await?;
    let files: i64 = sqlx::query_scalar("SELECT count(*) FROM editions WHERE item_id=?")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    playscale_core::catalog::validate_structure(
        kind,
        parent_kind.as_deref(),
        number,
        parent.as_deref() == Some(id),
        &child_types,
        files,
    )
    .map_err(ApiError::bad)
}
async fn write(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
    body: &Structure,
) -> Result<(), ApiError> {
    let result=sqlx::query("INSERT INTO item_structure VALUES (?,?,?,?,?) ON CONFLICT(item_id) DO UPDATE SET media_type=excluded.media_type,parent_id=excluded.parent_id,number=excluded.number,revision=excluded.revision")
        .bind(id).bind(&body.media_type).bind(&body.parent_id).bind(body.number).bind(body.expected_revision+1).execute(&mut **tx).await;
    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(conflict()),
        Err(e) => Err(e.into()),
        Ok(_) => Ok(()),
    }
}
#[utoipa::path(operation_id="create_catalog_item",post,path="/api/v1/catalog/items",request_body=Create,security(("admin_token"=[])),responses((status=201,description="Logical item created",body=CatalogItem),(status=400,description="Invalid hierarchy",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown parent",body=crate::api::ErrorBody),(status=409,description="Sibling conflict",body=crate::api::ErrorBody)))]
pub async fn create(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<Create>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<CatalogItem>), ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    if body.title.trim().is_empty() || body.title.len() > 500 {
        return Err(ApiError::bad("Title requires 1–500 bytes"));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let id = new_id();
    validate(&mut tx, &id, &body.media_type, &body.parent_id, body.number).await?;
    sqlx::query("INSERT INTO items (id,title,kind) VALUES (?,?,'video')")
        .bind(&id)
        .bind(body.title.trim())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO item_origins VALUES (?,?)")
        .bind(&id)
        .bind(body.title.trim())
        .execute(&mut *tx)
        .await?;
    write(
        &mut tx,
        &id,
        &Structure {
            expected_revision: 0,
            media_type: body.media_type,
            parent_id: body.parent_id,
            number: body.number,
        },
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(load(&app, &id).await?)))
}
#[utoipa::path(operation_id="update_item_structure",put,path="/api/v1/items/{id}/structure",params(("id"=String,Path)),request_body=Structure,security(("admin_token"=[])),responses((status=200,description="Structure replaced; metadata and progress preserved",body=CatalogItem),(status=400,description="Invalid hierarchy",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown item or parent",body=crate::api::ErrorBody),(status=409,description="Revision or sibling conflict",body=crate::api::ErrorBody)))]
pub async fn put(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Structure>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<CatalogItem>, ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    if body.expected_revision < 0 || body.expected_revision == i64::MAX {
        return Err(ApiError::bad("Invalid revision"));
    }
    let _guard = app.jobs.lock().await;
    // Operate on the canonical work: a retired ID resolves to its live target.
    let current = load(&app, &id).await?;
    let id = current.id.clone();
    if playscale_core::revision::advance(current.revision, body.expected_revision).is_err() {
        return Err(conflict());
    }
    let mut tx = crate::db::begin_write(&app.db).await?;
    validate(&mut tx, &id, &body.media_type, &body.parent_id, body.number).await?;
    write(&mut tx, &id, &body).await?;
    tx.commit().await?;
    Ok(Json(load(&app, &id).await?))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Edition {
    pub id: String,
    pub item_id: String,
    pub label: String,
    pub revision: i64,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditionInput {
    pub label: String,
}
#[utoipa::path(operation_id="list_editions",get,path="/api/v1/items/{id}/editions",params(("id"=String,Path)),responses((status=200,description="Editions including empty editions",body=Vec<Edition>),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn editions(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Edition>>, ApiError> {
    let id = load(&app, &id).await?.id;
    Ok(Json(
        sqlx::query_as("SELECT * FROM editions WHERE item_id=? ORDER BY label,id")
            .bind(id)
            .fetch_all(&app.db)
            .await?,
    ))
}
#[utoipa::path(operation_id="create_edition",post,path="/api/v1/items/{id}/editions",params(("id"=String,Path)),request_body=EditionInput,security(("admin_token"=[])),responses((status=201,description="Edition created",body=Edition),(status=400,description="Invalid label or container",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn add_edition(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<EditionInput>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Edition>), ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    if body.label.trim().is_empty() || body.label.len() > 200 {
        return Err(ApiError::bad("Invalid edition label"));
    }
    let _guard = app.jobs.lock().await;
    let item = load(&app, &id).await?;
    let id = item.id.clone();
    if !playscale_core::catalog::can_own_editions(&item.media_type) {
        return Err(ApiError::bad("Container items cannot own editions"));
    }
    let edition = Edition {
        id: new_id(),
        item_id: id,
        label: body.label.trim().into(),
        revision: 1,
    };
    sqlx::query("INSERT INTO editions (id,item_id,label,revision) VALUES (?,?,?,?)")
        .bind(&edition.id)
        .bind(&edition.item_id)
        .bind(&edition.label)
        .bind(edition.revision)
        .execute(&app.db)
        .await?;
    Ok((StatusCode::CREATED, Json(edition)))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Assign {
    pub expected_edition_id: String,
    pub edition_id: String,
}
#[utoipa::path(operation_id="assign_file_edition",put,path="/api/v1/files/{id}/edition",params(("id"=String,Path)),request_body=Assign,security(("admin_token"=[])),responses((status=200,description="File assigned within its existing item",body=Edition),(status=400,description="Cross-item reassignment requires an explicit merge workflow",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown file or edition",body=crate::api::ErrorBody),(status=409,description="Current edition changed",body=crate::api::ErrorBody)))]
pub async fn assign(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Assign>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Edition>, ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let (edition, item): (String, String) =
        sqlx::query_as("SELECT edition_id,item_id FROM catalog_files WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
    if edition != body.expected_edition_id {
        return Err(conflict());
    }
    let target: Edition = sqlx::query_as("SELECT * FROM editions WHERE id=?")
        .bind(body.edition_id)
        .fetch_one(&mut *tx)
        .await?;
    if target.item_id != item {
        return Err(ApiError::bad("Cross-item reassignment is not supported"));
    }
    sqlx::query("UPDATE media_files SET edition_id=? WHERE id=?")
        .bind(&target.id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(target))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameEdition {
    pub expected_revision: i64,
    pub label: String,
}
#[utoipa::path(operation_id="rename_edition",put,path="/api/v1/editions/{id}",params(("id"=String,Path)),request_body=RenameEdition,security(("admin_token"=[])),responses((status=200,description="Edition renamed without changing file identity",body=Edition),(status=400,description="Invalid label or revision",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown edition",body=crate::api::ErrorBody),(status=409,description="Revision conflict",body=crate::api::ErrorBody)))]
pub async fn rename_edition(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<RenameEdition>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Edition>, ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    if body.expected_revision < 1
        || body.expected_revision == i64::MAX
        || body.label.trim().is_empty()
        || body.label.len() > 200
    {
        return Err(ApiError::bad("Invalid label or revision"));
    }
    let _guard = app.jobs.lock().await;
    let mut edition: Edition = sqlx::query_as("SELECT * FROM editions WHERE id=?")
        .bind(&id)
        .fetch_one(&app.db)
        .await?;
    if playscale_core::revision::advance(edition.revision, body.expected_revision).is_err() {
        return Err(conflict());
    }
    edition.label = body.label.trim().into();
    edition.revision += 1;
    sqlx::query("UPDATE editions SET label=?,revision=? WHERE id=?")
        .bind(&edition.label)
        .bind(edition.revision)
        .bind(id)
        .execute(&app.db)
        .await?;
    Ok(Json(edition))
}
