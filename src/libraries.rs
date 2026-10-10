//! Logical libraries and storage sources. Validation, overlap, scope and
//! availability come from `playscale_core::sources`; this adapter reads the
//! facts and applies changes under the shared writer boundary.
use crate::{App, administration::VerifiedRoot, new_id};
use playscale_core::sources::{self as core, Availability, LibraryKind, SourceError};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeSet;

#[derive(Debug)]
pub enum LibraryError {
    Rejected(SourceError),
    /// The root contains or lies inside another registered source.
    Overlaps(String),
    /// A scan or processing job is active for the source.
    Busy,
    NotFound,
    Storage(anyhow::Error),
}
impl From<SourceError> for LibraryError {
    fn from(e: SourceError) -> Self {
        Self::Rejected(e)
    }
}
impl From<sqlx::Error> for LibraryError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            e => Self::Storage(e.into()),
        }
    }
}
impl From<anyhow::Error> for LibraryError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<serde_json::Error> for LibraryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.into())
    }
}

fn enum_value<T: serde::de::DeserializeOwned>(name: &str) -> anyhow::Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        name.into(),
    ))?)
}
fn enum_name<T: Serialize>(value: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_value(value)?
        .as_str()
        .unwrap_or_default()
        .into())
}
fn revision(value: i64) -> Result<u64, LibraryError> {
    u64::try_from(value).map_err(|e| LibraryError::Storage(e.into()))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SourceView {
    pub id: String,
    pub revision: u64,
    pub binding_revision: u64,
    pub name: String,
    pub root_path: String,
    pub volume_identity: String,
    pub availability: Availability,
    pub exclusions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LibraryView {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub kind: LibraryKind,
    pub language: String,
    pub source_ids: Vec<String>,
    pub availability: Availability,
}

async fn source_availability(
    conn: &mut SqliteConnection,
    source: &str,
    enabled: bool,
) -> anyhow::Result<Availability> {
    let published: Option<String> = sqlx::query_scalar(
        "SELECT outcome FROM jobs WHERE library_id=? AND phase='completed' AND outcome IS NOT NULL ORDER BY created_at DESC,rowid DESC LIMIT 1",
    )
    .bind(source)
    .fetch_optional(&mut *conn)
    .await?;
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT phase FROM jobs WHERE library_id=? AND phase IN ('completed','failed') ORDER BY created_at DESC,rowid DESC LIMIT 1",
    )
    .bind(source)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(core::source_availability(
        enabled,
        published.map(|o| o == "complete"),
        latest.as_deref() == Some("failed"),
    ))
}

type SourceRow = (String, i64, i64, String, String, String, bool, String);
const SOURCE_COLUMNS: &str = "id,revision,binding_revision,name,root_path,volume_identity,enabled,exclusions_json FROM sources";

async fn source_view(
    conn: &mut SqliteConnection,
    row: SourceRow,
) -> Result<SourceView, LibraryError> {
    let (id, rev, binding, name, root_path, volume_identity, enabled, exclusions) = row;
    Ok(SourceView {
        availability: source_availability(conn, &id, enabled).await?,
        id,
        revision: revision(rev)?,
        binding_revision: revision(binding)?,
        name,
        root_path,
        volume_identity,
        exclusions: serde_json::from_str(&exclusions)?,
    })
}

pub async fn get_source(db: &SqlitePool, id: &str) -> Result<SourceView, LibraryError> {
    let mut tx = db.begin().await?;
    let row: SourceRow = sqlx::query_as(&format!("SELECT {SOURCE_COLUMNS} WHERE id=?"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let view = source_view(&mut tx, row).await?;
    tx.commit().await?;
    Ok(view)
}

pub async fn list_sources(db: &SqlitePool) -> Result<Vec<SourceView>, LibraryError> {
    let mut tx = db.begin().await?;
    let rows: Vec<SourceRow> = sqlx::query_as(&format!("SELECT {SOURCE_COLUMNS} ORDER BY name,id"))
        .fetch_all(&mut *tx)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(source_view(&mut tx, row).await?);
    }
    tx.commit().await?;
    Ok(out)
}

/// Register a verified root as a source without creating a library. Overlap
/// with any other registered root (including managed output roots) is rejected.
pub async fn register_source(
    app: &App,
    candidate: VerifiedRoot,
    name: &str,
    exclusions: &[String],
) -> Result<SourceView, LibraryError> {
    core::valid_name(name)?;
    let exclusions = core::normalize_exclusions(exclusions)?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let roots: Vec<String> = sqlx::query_scalar("SELECT root FROM libraries")
        .fetch_all(&mut *tx)
        .await?;
    let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    if let Some(other) = core::overlapping(&candidate.root, &refs) {
        return Err(LibraryError::Overlaps(other.into()));
    }
    let existing: Option<(String, String)> =
        sqlx::query_as("SELECT id,root_identity FROM libraries WHERE root=?")
            .bind(&candidate.root)
            .fetch_optional(&mut *tx)
            .await?;
    let id = match existing {
        // Re-registering the same root with a different volume is a rebind,
        // which needs the relocation workflow, never a silent acceptance.
        Some((_, identity)) if identity != candidate.identity => {
            return Err(LibraryError::Storage(anyhow::anyhow!(
                "registered root now has a different volume identity"
            )));
        }
        Some((id, _)) => id,
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO libraries (id,name,root,root_identity,exclusions_json) VALUES (?,?,?,?,?)")
                .bind(&id)
                .bind(name.trim())
                .bind(&candidate.root)
                .bind(&candidate.identity)
                .bind(serde_json::to_string(&exclusions)?)
                .execute(&mut *tx)
                .await?;
            id
        }
    };
    tx.commit().await?;
    get_source(&app.db, &id).await
}

/// Change a source's exclusions. The source must be idle so no running scan
/// observes two scopes.
pub async fn set_exclusions(
    app: &App,
    id: &str,
    expected_revision: u64,
    exclusions: &[String],
) -> Result<SourceView, LibraryError> {
    let exclusions = core::normalize_exclusions(exclusions)?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let current: i64 = sqlx::query_scalar("SELECT revision FROM sources WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let next = core::advance(revision(current)?, expected_revision)?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE library_id=? AND phase IN ('queued','running','cancelling')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if active > 0 {
        return Err(LibraryError::Busy);
    }
    sqlx::query("UPDATE libraries SET exclusions_json=?,revision=? WHERE id=?")
        .bind(serde_json::to_string(&exclusions)?)
        .bind(i64::try_from(next).map_err(|e| LibraryError::Storage(e.into()))?)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    get_source(&app.db, id).await
}

async fn registered(conn: &mut SqliteConnection) -> anyhow::Result<BTreeSet<String>> {
    Ok(sqlx::query_scalar::<_, String>("SELECT id FROM sources")
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect())
}

async fn library_view(conn: &mut SqliteConnection, id: &str) -> Result<LibraryView, LibraryError> {
    let (rev, name, kind, language): (i64, String, String, String) =
        sqlx::query_as("SELECT revision,name,kind,language FROM catalog_libraries WHERE id=?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let sources: Vec<(String, bool)> = sqlx::query_as(
        "SELECT s.id,s.enabled FROM library_sources l JOIN sources s ON s.id=l.source_id WHERE l.library_id=? ORDER BY s.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut availability = Vec::new();
    for (source, enabled) in &sources {
        availability.push(source_availability(conn, source, *enabled).await?);
    }
    Ok(LibraryView {
        id: id.into(),
        revision: revision(rev)?,
        name,
        kind: enum_value(&kind)?,
        language,
        source_ids: sources.into_iter().map(|(s, _)| s).collect(),
        availability: core::library_availability(&availability),
    })
}

pub async fn get_library(db: &SqlitePool, id: &str) -> Result<LibraryView, LibraryError> {
    let mut tx = db.begin().await?;
    let view = library_view(&mut tx, id).await?;
    tx.commit().await?;
    Ok(view)
}

pub async fn list_libraries(db: &SqlitePool) -> Result<Vec<LibraryView>, LibraryError> {
    let mut tx = db.begin().await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM catalog_libraries ORDER BY name,id")
        .fetch_all(&mut *tx)
        .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(library_view(&mut tx, &id).await?);
    }
    tx.commit().await?;
    Ok(out)
}

/// Create (`existing: None`) or revision-checked replace a library definition.
pub async fn save_library(
    app: &App,
    existing: Option<(&str, u64)>,
    name: &str,
    kind: LibraryKind,
    language: &str,
    source_ids: &[String],
) -> Result<LibraryView, LibraryError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    core::validate_library(name, language, source_ids, &registered(&mut tx).await?)?;
    let kind = enum_name(&kind)?;
    let id = match existing {
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO catalog_libraries (id,name,kind,language) VALUES (?,?,?,?)")
                .bind(&id)
                .bind(name.trim())
                .bind(&kind)
                .bind(language)
                .execute(&mut *tx)
                .await?;
            id
        }
        Some((id, expected)) => {
            let current: i64 =
                sqlx::query_scalar("SELECT revision FROM catalog_libraries WHERE id=?")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            let next = core::advance(revision(current)?, expected)?;
            sqlx::query(
                "UPDATE catalog_libraries SET revision=?,name=?,kind=?,language=? WHERE id=?",
            )
            .bind(i64::try_from(next).map_err(|e| LibraryError::Storage(e.into()))?)
            .bind(name.trim())
            .bind(&kind)
            .bind(language)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM library_sources WHERE library_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            id.to_string()
        }
    };
    for source in source_ids {
        sqlx::query("INSERT INTO library_sources VALUES (?,?)")
            .bind(&id)
            .bind(source)
            .execute(&mut *tx)
            .await?;
    }
    let view = library_view(&mut tx, &id).await?;
    tx.commit().await?;
    Ok(view)
}

/// Libraries a work belongs to: those whose sources hold any of its files.
/// Derived, never stored; access policy is applied by the caller.
pub async fn libraries_of_item(db: &SqlitePool, item: &str) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT l.library_id FROM library_sources l JOIN media_files f ON f.library_id=l.source_id JOIN editions e ON e.id=f.edition_id WHERE e.item_id=? ORDER BY l.library_id",
    )
    .bind(item)
    .fetch_all(db)
    .await?)
}
