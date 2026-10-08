use crate::{
    App,
    api::{ApiError, admin, json},
};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::{IntoParams, ToSchema};
static DECODERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
fn role_valid(role: &str) -> bool {
    matches!(role, "poster" | "backdrop" | "thumbnail")
}
async fn exists(app: &App, id: &str) -> Result<(), ApiError> {
    let _: String = sqlx::query_scalar("SELECT id FROM items WHERE id=?")
        .bind(id)
        .fetch_one(&app.db)
        .await?;
    Ok(())
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Contribution {
    pub role: String,
    pub source: String,
    pub asset_id: String,
    pub revision: i64,
    pub mime: String,
    pub width: i64,
    pub height: i64,
}
#[derive(Serialize, ToSchema)]
pub struct Selection {
    pub role: String,
    pub asset_id: Option<String>,
    pub revision: i64,
    pub conflict: bool,
}
#[derive(Serialize, ToSchema)]
pub struct Artwork {
    pub contributions: Vec<Contribution>,
    pub selections: Vec<Selection>,
}
async fn load(app: &App, id: &str) -> Result<Artwork, ApiError> {
    exists(app, id).await?;
    let mut tx = app.db.begin().await?;
    let contributions:Vec<Contribution>=sqlx::query_as("SELECT c.role,c.source,c.asset_id,c.revision,a.mime,a.width,a.height FROM artwork_contributions c JOIN artwork_assets a ON a.id=c.asset_id WHERE c.item_id=? ORDER BY c.role,c.source").bind(id).fetch_all(&mut *tx).await?;
    let mut selections = Vec::new();
    for role in ["poster", "backdrop", "thumbnail"] {
        let explicit: Option<(Option<String>, i64)> = sqlx::query_as(
            "SELECT asset_id,revision FROM artwork_selections WHERE item_id=? AND role=?",
        )
        .bind(id)
        .bind(role)
        .fetch_optional(&mut *tx)
        .await?;
        let (chosen, revision) = explicit.unwrap_or((None, 0));
        let local = contributions
            .iter()
            .find(|c| c.role == role && c.source == "local");
        let candidates: std::collections::BTreeSet<_> = contributions
            .iter()
            .filter(|c| c.role == role)
            .map(|c| c.asset_id.clone())
            .collect();
        let (asset_id, conflict) = playscale_core::artwork::resolve(
            chosen,
            local.map(|c| c.asset_id.clone()),
            &candidates,
        );
        selections.push(Selection {
            role: role.into(),
            asset_id,
            revision,
            conflict,
        });
    }
    tx.commit().await?;
    Ok(Artwork {
        contributions,
        selections,
    })
}
#[utoipa::path(operation_id="get_artwork",get,path="/api/v1/items/{id}/artwork",params(("id"=String,Path)),responses((status=200,description="Source artwork and resolved role selections; content URL is /api/v1/artwork/{asset_id}/content",body=Artwork),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Artwork>, ApiError> {
    Ok(Json(load(&app, &id).await?))
}
#[derive(Deserialize, IntoParams)]
pub struct UploadQuery {
    pub expected_revision: i64,
}
#[utoipa::path(operation_id="upload_artwork",put,path="/api/v1/items/{id}/artwork/{role}/{source}",params(("id"=String,Path),("role"=String,Path),("source"=String,Path),UploadQuery),request_body(content=Vec<u8>,content_type="application/octet-stream"),security(("admin_token"=[])),responses((status=200,description="Validated PNG, JPEG or WebP contribution replaced",body=Artwork),(status=400,description="Invalid image or bounds",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown item",body=crate::api::ErrorBody),(status=409,description="Source revision conflict",body=crate::api::ErrorBody),(status=413,description="Image exceeds 8 MiB",body=crate::api::ErrorBody),(status=503,description="Image decoder capacity exhausted",body=crate::api::ErrorBody)))]
pub async fn upload(
    State(app): State<App>,
    Path((id, role, source)): Path<(String, String, String)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    bytes: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<Json<Artwork>, ApiError> {
    admin(&app, &headers)?;
    if !role_valid(&role)
        || source.is_empty()
        || source.len() > 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || query.expected_revision < 0
        || query.expected_revision == i64::MAX
    {
        return Err(ApiError::bad("Invalid role, source or revision"));
    }
    exists(&app, &id).await?;
    let bytes = bytes.map_err(|e| {
        ApiError::new(
            e.status(),
            "invalid_image_body",
            "Image body could not be read",
        )
    })?;
    let permit = DECODERS.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "decoder_busy",
            "Image decoder capacity exhausted",
        )
    })?;
    let (asset, mime, width, height, bytes) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(ApiError::internal)?;
        let mime = match reader.format() {
            Some(image::ImageFormat::Png) => "image/png",
            Some(image::ImageFormat::Jpeg) => "image/jpeg",
            Some(image::ImageFormat::WebP) => "image/webp",
            _ => return Err(ApiError::bad("Expected PNG, JPEG or WebP image")),
        };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader.decode().map_err(|_| {
            ApiError::bad("Image decoding failed or exceeds dimension/memory limits")
        })?;
        let asset = format!("{:x}", Sha256::digest(&bytes));
        Ok((asset, mime, decoded.width(), decoded.height(), bytes))
    })
    .await
    .map_err(ApiError::internal)??;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let revision: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM artwork_contributions WHERE item_id=? AND role=? AND source=?",
    )
    .bind(&id)
    .bind(&role)
    .bind(&source)
    .fetch_optional(&mut *tx)
    .await?;
    if playscale_core::revision::advance(revision.unwrap_or(0), query.expected_revision).is_err() {
        return Err(ApiError::conflict(
            "artwork_revision_conflict",
            "Read the current contribution before updating",
        ));
    }
    sqlx::query("INSERT OR IGNORE INTO artwork_assets VALUES (?,?,?,?,?)")
        .bind(&asset)
        .bind(mime)
        .bind(i64::from(width))
        .bind(i64::from(height))
        .bind(bytes.as_ref())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO artwork_contributions VALUES (?,?,?,?,?) ON CONFLICT(item_id,role,source) DO UPDATE SET asset_id=excluded.asset_id,revision=excluded.revision")
        .bind(&id).bind(role).bind(source).bind(asset).bind(query.expected_revision+1).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(load(&app, &id).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Select {
    pub expected_revision: i64,
    pub asset_id: Option<String>,
}
#[utoipa::path(operation_id="select_artwork",put,path="/api/v1/items/{id}/artwork-selection/{role}",params(("id"=String,Path),("role"=String,Path)),request_body=Select,security(("admin_token"=[])),responses((status=200,description="Pin a contributed asset; null restores automatic selection",body=Artwork),(status=400,description="Invalid selection",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=404,description="Unknown item",body=crate::api::ErrorBody),(status=409,description="Selection revision conflict",body=crate::api::ErrorBody)))]
pub async fn select(
    State(app): State<App>,
    Path((id, role)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<Select>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Artwork>, ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    if !role_valid(&role) || body.expected_revision < 0 || body.expected_revision == i64::MAX {
        return Err(ApiError::bad("Invalid role or revision"));
    }
    let _guard = app.jobs.lock().await;
    exists(&app, &id).await?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let revision: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM artwork_selections WHERE item_id=? AND role=?")
            .bind(&id)
            .bind(&role)
            .fetch_optional(&mut *tx)
            .await?;
    let contributions: Vec<String> =
        sqlx::query_scalar("SELECT asset_id FROM artwork_contributions WHERE item_id=? AND role=?")
            .bind(&id)
            .bind(&role)
            .fetch_all(&mut *tx)
            .await?;
    let next_revision = playscale_core::artwork::select(
        revision.unwrap_or(0),
        body.expected_revision,
        body.asset_id.as_deref(),
        &contributions,
    )
    .map_err(|code| {
        if code == "invalid_artwork_selection" {
            ApiError::bad("Asset must be a current contribution for this item and role")
        } else {
            ApiError::conflict(code, "Read the current selection before updating")
        }
    })?;
    sqlx::query("INSERT INTO artwork_selections VALUES (?,?,?,?) ON CONFLICT(item_id,role) DO UPDATE SET asset_id=excluded.asset_id,revision=excluded.revision")
        .bind(&id).bind(role).bind(body.asset_id).bind(next_revision).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(load(&app, &id).await?))
}
#[utoipa::path(operation_id="get_artwork_content",get,path="/api/v1/artwork/{id}/content",params(("id"=String,Path)),responses((status=200,description="Immutable image bytes",content_type="application/octet-stream",body=Vec<u8>),(status=304,description="ETag matches"),(status=404,description="Unknown asset",body=crate::api::ErrorBody)))]
pub async fn content(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (mime, bytes): (String, Vec<u8>) =
        sqlx::query_as("SELECT mime,bytes FROM artwork_assets WHERE id=?")
            .bind(&id)
            .fetch_one(&app.db)
            .await?;
    let etag = format!("\"{id}\"");
    if headers
        .get("if-none-match")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|v| {
                let v = v.trim();
                v == "*" || v.trim_start_matches("W/") == etag
            })
        })
    {
        return Ok((StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response());
    }
    Ok(([("content-type", mime), ("etag", etag)], bytes).into_response())
}
