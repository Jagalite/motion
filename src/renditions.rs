use crate::{
    App,
    api::{ApiError, admin, json},
    db::{Item, ItemRow},
    new_id, now,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub file_id: String,
    pub file_revision: String,
    pub source_file_id: String,
    pub source_revision: String,
    pub label: String,
    /// Informational external recipe/tool provenance, never executable commands.
    pub recipe: Value,
}
#[derive(Serialize, ToSchema)]
pub struct Rendition {
    pub id: String,
    pub source: String,
    pub external_id: String,
    pub label: String,
    pub file_id: String,
    pub file_revision: String,
    pub source_file_id: String,
    pub source_revision: String,
    pub recipe: Value,
    pub available: bool,
    pub media_url: String,
    pub bytes: i64,
    pub duration_seconds: Option<f64>,
    /// Measured file average; not peak bandwidth or an encoder target.
    pub average_bitrate: Option<u64>,
    pub tracks: Vec<crate::db::Track>,
}
#[derive(Serialize, ToSchema)]
pub struct Choices {
    pub originals: Vec<Item>,
    pub renditions: Vec<Rendition>,
}

#[utoipa::path(get,path="/api/v1/items/{id}/playback-options",params(("id"=String,Path)),responses((status=200,description="Originals and registered renditions; availability is based on cataloged revisions",body=Choices),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn choices(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Choices>, ApiError> {
    let _: String = sqlx::query_scalar("SELECT id FROM items WHERE id=?")
        .bind(&id)
        .fetch_one(&app.db)
        .await?;
    let rows: Vec<ItemRow> = sqlx::query_as(
        "SELECT * FROM catalog_files WHERE item_id=? AND generated=0 ORDER BY edition_id,id",
    )
    .bind(&id)
    .fetch_all(&app.db)
    .await?;
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        source: String,
        external_id: String,
        label: String,
        file_id: String,
        file_revision: String,
        source_file_id: String,
        source_revision: String,
        recipe_json: String,
        available: bool,
        observed_output: String,
        observed_source: String,
        bytes: i64,
        duration_seconds: Option<f64>,
        tracks_json: String,
    }
    let variants:Vec<Row>=sqlx::query_as("SELECT r.*, f.bytes,f.duration_seconds,f.tracks_json, f.available AS available,f.revision AS observed_output,s.revision AS observed_source FROM renditions r JOIN media_files f ON f.id=r.file_id JOIN media_files s ON s.id=r.source_file_id WHERE r.item_id=? ORDER BY r.source,r.external_id")
        .bind(id).fetch_all(&app.db).await?;
    let renditions = variants
        .into_iter()
        .map(|r| {
            let available = playscale_core::renditions::available(
                r.available,
                &r.observed_output,
                &r.file_revision,
                &r.observed_source,
                &r.source_revision,
            );
            Ok(Rendition {
                media_url: format!("/media/{}?revision={}", r.file_id, r.file_revision),
                id: r.id,
                source: r.source,
                external_id: r.external_id,
                label: r.label,
                file_id: r.file_id,
                file_revision: r.file_revision,
                source_file_id: r.source_file_id,
                source_revision: r.source_revision,
                recipe: serde_json::from_str(&r.recipe_json).map_err(ApiError::internal)?,
                available,
                bytes: r.bytes,
                duration_seconds: r.duration_seconds,
                average_bitrate: r
                    .duration_seconds
                    .filter(|d| d.is_finite() && *d > 0.0)
                    .map(|d| (r.bytes.max(0) as f64 * 8.0 / d).ceil() as u64),
                tracks: serde_json::from_str(&r.tracks_json).map_err(ApiError::internal)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(Choices {
        originals: rows.into_iter().map(ItemRow::public).collect(),
        renditions,
    }))
}

#[utoipa::path(put,path="/api/v1/items/{id}/renditions/{source}/{external_id}",params(("id"=String,Path),("source"=String,Path),("external_id"=String,Path)),request_body=Register,security(("admin_token"=[])),responses((status=200,description="Existing rendition registered; no processing started",body=Choices),(status=400,description="Invalid rendition",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown item or file",body=crate::api::ErrorBody),(status=409,description="Revision or identity conflict",body=crate::api::ErrorBody)))]
pub async fn register(
    State(app): State<App>,
    Path((id, source, external_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Result<Json<Register>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Choices>, ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    if source.is_empty()
        || source.len() > 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || external_id.is_empty()
        || external_id.len() > 256
        || body.label.trim().is_empty()
        || body.label.len() > 200
        || !body.recipe.is_object()
    {
        return Err(ApiError::bad("Invalid rendition identity, label or recipe"));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let (source_item, source_revision): (String, String) =
        sqlx::query_as("SELECT item_id,revision FROM catalog_files WHERE id=?")
            .bind(&body.source_file_id)
            .fetch_one(&mut *tx)
            .await?;
    let revision: String =
        sqlx::query_scalar("SELECT revision FROM media_files WHERE id=? AND available=1")
            .bind(&body.file_id)
            .fetch_one(&mut *tx)
            .await?;
    let existing:Option<(String,String,String,String,String,String)>=sqlx::query_as("SELECT item_id,file_id,file_revision,source_revision,source_file_id,recipe_json FROM renditions WHERE source=? AND external_id=?").bind(&source).bind(&external_id).fetch_optional(&mut *tx).await?;
    use playscale_core::renditions::Identity;
    let previous = existing
        .map(|r| -> Result<_, ApiError> {
            Ok(Identity {
                item: r.0,
                file: r.1,
                file_revision: r.2,
                source_revision: r.3,
                source_file: r.4,
                recipe: serde_json::from_str(&r.5).map_err(ApiError::internal)?,
            })
        })
        .transpose()?;
    Identity {
        item: id.clone(),
        file: body.file_id.clone(),
        file_revision: body.file_revision.clone(),
        source_file: body.source_file_id.clone(),
        source_revision: body.source_revision.clone(),
        recipe: body.recipe.clone(),
    }
    .register(previous.as_ref(), &source_item, &source_revision, &revision)
    .map_err(|code| {
        ApiError::conflict(
            code,
            "Rendition identity and revisions must match the current catalog and registration",
        )
    })?;
    sqlx::query("INSERT INTO renditions VALUES (?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(source,external_id) DO UPDATE SET label=excluded.label,recipe_json=excluded.recipe_json,updated_at=excluded.updated_at")
        .bind(new_id()).bind(&id).bind(source).bind(external_id).bind(body.file_id).bind(body.file_revision).bind(body.source_file_id).bind(body.source_revision).bind(body.label).bind(serde_json::to_string(&body.recipe).map_err(ApiError::internal)?).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    choices(State(app.clone()), Path(id)).await
}
