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
/// The stored inputs of an item's metadata: every source document, including
/// the scanned origin under the reserved `scan` source, and the stored rows.
pub(crate) struct Documents {
    pub docs: BTreeMap<String, Contribution>,
    pub sources: Vec<Source>,
}
pub(crate) async fn documents(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> anyhow::Result<Documents> {
    let title: String = sqlx::query_scalar("SELECT title FROM item_origins WHERE item_id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let rows:Vec<Row>=sqlx::query_as("SELECT source,revision,external_id,document_json,updated_at FROM metadata_documents WHERE item_id=? ORDER BY source").bind(id).fetch_all(&mut *conn).await?;
    let mut docs = BTreeMap::from([(
        "scan".into(),
        Contribution {
            values: BTreeMap::from([("title".into(), Value::String(title))]),
            ..Default::default()
        },
    )]);
    let mut sources = Vec::new();
    for (source, revision, external_id, text, updated_at) in rows {
        let doc: Contribution = serde_json::from_str(&text)?;
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
    Ok(Documents { docs, sources })
}
pub async fn load(db: &sqlx::SqlitePool, id: &str) -> Result<Metadata, ApiError> {
    let mut conn = db.acquire().await?;
    let Documents { docs, sources } = documents(&mut conn, id).await.map_err(|e| match e
        .downcast_ref::<sqlx::Error>()
    {
        Some(sqlx::Error::RowNotFound) => ApiError::not_found(),
        _ => ApiError::internal(e),
    })?;
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
    if !valid_source(&source) {
        return Err(ApiError::bad("Invalid metadata source; scan is reserved"));
    }
    if body.expected_revision < 0
        || body.expected_revision == i64::MAX
        || body
            .external_id
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
    {
        return Err(ApiError::bad(
            "Invalid contribution bounds or local tag exclusions",
        ));
    }
    let contribution =
        validate(&source, body.values, body.tags, body.excluded_tags).map_err(ApiError::bad)?;
    let _guard = app.jobs.lock().await; // Metadata projection and scan publication share the writer boundary.
    let mut tx = crate::db::begin_write(&app.db).await?;
    replace(
        &mut tx,
        &id,
        &source,
        &contribution,
        body.external_id.as_deref(),
        body.expected_revision,
    )
    .await
    .map_err(|e| match e {
        ContributionError::NotFound => ApiError::not_found(),
        ContributionError::IdentityPinned => ApiError::conflict(
            "manual_identity_pinned",
            "A manual identification pins this source's external identity",
        ),
        ContributionError::RevisionConflict => ApiError::conflict(
            "metadata_revision_conflict",
            "Read the current source revision before updating",
        ),
        ContributionError::IdentityConflict => ApiError::conflict(
            "external_identity_conflict",
            "External identity already belongs to another item",
        ),
        ContributionError::Storage(e) => ApiError::internal(e),
    })?;
    tx.commit().await?;
    Ok(Json(load(&app.db, &id).await?))
}

/// Source names are lowercase slugs; `scan` is reserved for the scanned origin.
pub fn valid_source(source: &str) -> bool {
    source != "scan"
        && !source.is_empty()
        && source.len() <= 64
        && source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Validate and normalize one source's contribution. Err carries the reason.
pub fn validate(
    source: &str,
    values: BTreeMap<String, Value>,
    tags: BTreeSet<String>,
    excluded_tags: BTreeSet<String>,
) -> Result<Contribution, &'static str> {
    if values.len() > 100
        || tags.len() > 100
        || excluded_tags.len() > 100
        || (source != "local" && !excluded_tags.is_empty())
    {
        return Err("Invalid contribution bounds or local tag exclusions");
    }
    for (key, value) in &values {
        if key.is_empty() || key.len() > 100 || value.is_null() {
            return Err(
                "Fields require names and non-null values; omit a field to remove this source's contribution",
            );
        }
        if !playscale_core::metadata::valid_field(key, value) {
            return Err(
                "Invalid conventional metadata field: title, description, release_year, release_date or cast",
            );
        }
    }
    fn normalize(values: BTreeSet<String>) -> Result<BTreeSet<String>, &'static str> {
        values
            .into_iter()
            .map(|tag| {
                let tag = tag
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase();
                if tag.is_empty() || tag.len() > 100 {
                    Err("Tags must contain 1–100 bytes")
                } else {
                    Ok(tag)
                }
            })
            .collect()
    }
    Ok(Contribution {
        values,
        tags: normalize(tags)?,
        excluded_tags: normalize(excluded_tags)?,
    })
}

#[derive(Debug)]
pub enum ContributionError {
    /// Unknown item, or a work retired by a merge.
    NotFound,
    IdentityPinned,
    RevisionConflict,
    IdentityConflict,
    Storage(anyhow::Error),
}
impl From<sqlx::Error> for ContributionError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            e => Self::Storage(e.into()),
        }
    }
}

/// Replace one source's contribution against its current revision (zero
/// creates it) and reproject the title. The caller holds `App.jobs` and the
/// writer transaction, and commits.
pub(crate) async fn replace(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    source: &str,
    contribution: &Contribution,
    external_id: Option<&str>,
    expected_revision: i64,
) -> Result<(), ContributionError> {
    // Existence check; retired (merged) works take no new contributions.
    let _origin: String = sqlx::query_scalar(
        "SELECT title FROM item_origins WHERE item_id=? AND item_id NOT IN (SELECT alias_id FROM item_aliases)",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM metadata_documents WHERE item_id=? AND source=?")
            .bind(id)
            .bind(source)
            .fetch_optional(&mut *conn)
            .await?;
    if !crate::matching::provider_identity_allowed(&mut *conn, id, source, external_id)
        .await
        .map_err(ContributionError::Storage)?
    {
        return Err(ContributionError::IdentityPinned);
    }
    if playscale_core::revision::advance(existing.unwrap_or(0), expected_revision).is_err() {
        return Err(ContributionError::RevisionConflict);
    }
    let text =
        serde_json::to_string(contribution).map_err(|e| ContributionError::Storage(e.into()))?;
    let changed=sqlx::query("INSERT INTO metadata_documents VALUES (?,?,?,?,?,?) ON CONFLICT(item_id,source) DO UPDATE SET revision=excluded.revision,external_id=excluded.external_id,document_json=excluded.document_json,updated_at=excluded.updated_at")
        .bind(id).bind(source).bind(expected_revision+1).bind(external_id).bind(text).bind(now()).execute(&mut *conn).await;
    if let Err(error) = changed {
        if matches!(&error,sqlx::Error::Database(e) if e.is_unique_violation()) {
            return Err(ContributionError::IdentityConflict);
        }
        return Err(error.into());
    }
    project_title(conn, id)
        .await
        .map_err(ContributionError::Storage)?;
    Ok(())
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
