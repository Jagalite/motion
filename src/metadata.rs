use crate::{App, api::ApiError, now};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use playscale_core::metadata::{Contribution, resolve};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Update {
    /// Zero creates a source contribution; otherwise must equal its current revision.
    pub expected_revision: i64,
    pub external_id: Option<String>,
    pub values: BTreeMap<String, Value>,
    pub tags: BTreeSet<String>,
    #[serde(default)]
    pub excluded_tags: BTreeSet<String>,
}
#[derive(Serialize, ToSchema)]
pub struct Source {
    pub source: String,
    pub revision: i64,
    pub external_id: Option<String>,
    pub updated_at: i64,
    pub values: BTreeMap<String, Value>,
    pub tags: BTreeSet<String>,
    pub excluded_tags: BTreeSet<String>,
}
#[derive(Serialize, ToSchema)]
pub struct Metadata {
    pub values: BTreeMap<String, Value>,
    pub field_sources: BTreeMap<String, Vec<String>>,
    pub tags: BTreeSet<String>,
    pub tag_sources: BTreeMap<String, Vec<String>>,
    pub conflicts: BTreeMap<String, BTreeMap<String, Value>>,
    pub sources: Vec<Source>,
}
type Row = (String, i64, Option<String>, String, i64);
pub async fn load(db: &sqlx::SqlitePool, id: &str) -> Result<Metadata, ApiError> {
    let title: String = sqlx::query_scalar("SELECT title FROM item_origins WHERE item_id=?")
        .bind(id)
        .fetch_one(db)
        .await?;
    let rows:Vec<Row>=sqlx::query_as("SELECT source,revision,external_id,document_json,updated_at FROM metadata_documents WHERE item_id=? ORDER BY source").bind(id).fetch_all(db).await?;
    let mut docs = BTreeMap::from([(
        "scan".into(),
        Contribution {
            values: BTreeMap::from([("title".into(), Value::String(title))]),
            ..Default::default()
        },
    )]);
    let mut sources = Vec::new();
    for (source, revision, external_id, text, updated_at) in rows {
        let doc: Contribution = serde_json::from_str(&text).map_err(ApiError::internal)?;
        sources.push(Source {
            source: source.clone(),
            revision,
            external_id,
            updated_at,
            values: doc.values.clone(),
            tags: doc.tags.clone(),
            excluded_tags: doc.excluded_tags.clone(),
        });
        docs.insert(source, doc);
    }
    let resolved = resolve(&docs);
    Ok(Metadata {
        values: resolved.values,
        field_sources: resolved.field_sources,
        tags: resolved.tags,
        tag_sources: resolved.tag_sources,
        conflicts: resolved.conflicts,
        sources,
    })
}
#[utoipa::path(get,path="/api/v1/items/{id}/metadata",params(("id"=String,Path)),responses((status=200,description="Resolved metadata, tags, provenance and conflicts",body=Metadata),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Metadata>, ApiError> {
    Ok(Json(load(&app.db, &id).await?))
}

#[utoipa::path(put,path="/api/v1/items/{id}/metadata/{source}",params(("id"=String,Path),("source"=String,Path)),request_body=Update,security(("admin_token"=[])),responses((status=200,description="Source contribution replaced atomically",body=Metadata),(status=400,description="Invalid metadata",body=crate::api::ErrorBody),(status=401,description="Admin required",body=crate::api::ErrorBody),(status=409,description="Revision or external identity conflict",body=crate::api::ErrorBody),(status=404,description="Unknown item",body=crate::api::ErrorBody)))]
pub async fn put(
    State(app): State<App>,
    Path((id, source)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<Update>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Metadata>, ApiError> {
    crate::api::admin(&app, &headers)?;
    let body = crate::api::json(body)?;
    if source == "scan"
        || source.is_empty()
        || source.len() > 64
        || !source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ApiError::bad("Invalid metadata source; scan is reserved"));
    }
    if body.expected_revision < 0
        || body.expected_revision == i64::MAX
        || body.values.len() > 100
        || body.tags.len() > 100
        || body.excluded_tags.len() > 100
        || (source != "local" && !body.excluded_tags.is_empty())
        || body
            .external_id
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
    {
        return Err(ApiError::bad(
            "Invalid contribution bounds or local tag exclusions",
        ));
    }
    for (key, value) in &body.values {
        if key.is_empty() || key.len() > 100 || value.is_null() {
            return Err(ApiError::bad(
                "Fields require names and non-null values; omit a field to remove this source's contribution",
            ));
        }
        if !playscale_core::metadata::valid_field(key, value) {
            return Err(ApiError::bad(
                "Invalid conventional metadata field: title, description, release_year, release_date or cast",
            ));
        }
    }
    fn tags(values: BTreeSet<String>) -> Result<BTreeSet<String>, ApiError> {
        values
            .into_iter()
            .map(|tag| {
                let tag = tag
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                if tag.is_empty() || tag.len() > 100 {
                    Err(ApiError::bad("Tags must contain 1–100 bytes"))
                } else {
                    Ok(tag)
                }
            })
            .collect()
    }
    let contribution = Contribution {
        values: body.values,
        tags: tags(body.tags)?,
        excluded_tags: tags(body.excluded_tags)?,
    };
    let _guard = app.jobs.lock().await; // Metadata projection and scan publication share the writer boundary.
    let mut tx = crate::db::begin_write(&app.db).await?;
    // Existence check; retired (merged) works take no new contributions.
    let _origin: String = sqlx::query_scalar(
        "SELECT title FROM item_origins WHERE item_id=? AND item_id NOT IN (SELECT alias_id FROM item_aliases)",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM metadata_documents WHERE item_id=? AND source=?")
            .bind(&id)
            .bind(&source)
            .fetch_optional(&mut *tx)
            .await?;
    if !crate::matching::provider_identity_allowed(
        &mut tx,
        &id,
        &source,
        body.external_id.as_deref(),
    )
    .await
    .map_err(ApiError::internal)?
    {
        return Err(ApiError::conflict(
            "manual_identity_pinned",
            "A manual identification pins this source's external identity",
        ));
    }
    if playscale_core::revision::advance(existing.unwrap_or(0), body.expected_revision).is_err() {
        return Err(ApiError::conflict(
            "metadata_revision_conflict",
            "Read the current source revision before updating",
        ));
    }
    let text = serde_json::to_string(&contribution).map_err(ApiError::internal)?;
    let changed=sqlx::query("INSERT INTO metadata_documents VALUES (?,?,?,?,?,?) ON CONFLICT(item_id,source) DO UPDATE SET revision=excluded.revision,external_id=excluded.external_id,document_json=excluded.document_json,updated_at=excluded.updated_at")
        .bind(&id).bind(&source).bind(body.expected_revision+1).bind(body.external_id).bind(text).bind(now()).execute(&mut *tx).await;
    if let Err(error) = changed {
        if matches!(&error,sqlx::Error::Database(e) if e.is_unique_violation()) {
            return Err(ApiError::conflict(
                "external_identity_conflict",
                "External identity already belongs to another item",
            ));
        }
        return Err(error.into());
    }
    project_title(&mut tx, &id)
        .await
        .map_err(ApiError::internal)?;
    crate::search::refresh(&mut tx, 100)
        .await
        .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(load(&app.db, &id).await?))
}

/// Recompute the stored title projection from every contribution, falling
/// back to the scanned origin title. Callers hold the writer transaction.
pub(crate) async fn project_title(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> anyhow::Result<()> {
    let original: String = sqlx::query_scalar("SELECT title FROM item_origins WHERE item_id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT source,document_json FROM metadata_documents WHERE item_id=?")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?;
    let mut docs = BTreeMap::new();
    for (source, text) in rows {
        docs.insert(source, serde_json::from_str::<Contribution>(&text)?);
    }
    let resolved = resolve(&docs);
    let title = resolved
        .values
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&original);
    sqlx::query("UPDATE items SET title=? WHERE id=? AND title IS NOT ?")
        .bind(title)
        .bind(id)
        .bind(title)
        .execute(&mut *conn)
        .await?;
    Ok(())
}
