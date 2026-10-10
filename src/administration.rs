//! Catalog administration never moves or deletes source media.
use crate::{
    App,
    api::{ApiError, admin, json},
    db,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub root: String,
    pub revision: i64,
    pub enabled: bool,
    pub available_files: i64,
    pub missing_files: i64,
}
const LIBRARIES: &str = "SELECT l.id,l.name,l.root,l.revision,l.enabled,(SELECT count(*) FROM media_files f WHERE f.library_id=l.id AND f.available=1) AS available_files,(SELECT count(*) FROM media_files f WHERE f.library_id=l.id AND f.available=0) AS missing_files FROM libraries l WHERE l.managed=0";
async fn load(app: &App, id: &str) -> Result<Library, ApiError> {
    Ok(sqlx::query_as(&format!("{LIBRARIES} AND l.id=?"))
        .bind(id)
        .fetch_one(&app.db)
        .await?)
}
#[utoipa::path(operation_id="admin_libraries",get,path="/api/v1/admin/libraries",security(("admin_token"=[])),responses((status=200,description="Library roots, revisions and available/missing counts, including detached libraries",body=Vec<Library>)))]
pub async fn libraries(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<Vec<Library>>, ApiError> {
    admin(&app, &headers)?;
    Ok(Json(
        sqlx::query_as(&format!("{LIBRARIES} ORDER BY l.name,l.id"))
            .fetch_all(&app.db)
            .await?,
    ))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    pub expected_revision: i64,
    pub name: String,
}
fn valid_name(name: &str) -> Result<(), ApiError> {
    if name.trim().is_empty() || name.len() > 200 {
        Err(ApiError::bad("Name must contain 1..200 bytes"))
    } else {
        Ok(())
    }
}
fn version(current: i64, expected: i64) -> Result<(), ApiError> {
    if playscale_core::revision::advance(current, expected).is_err() {
        Err(ApiError::conflict(
            "revision_conflict",
            "Refresh the resource revision",
        ))
    } else {
        Ok(())
    }
}
#[utoipa::path(operation_id="rename_library",put,path="/api/v1/admin/libraries/{id}",params(("id"=String,Path)),request_body=Rename,security(("admin_token"=[])),responses((status=200,description="Library renamed",body=Library),(status=409,description="Stale revision",body=crate::api::ErrorBody)))]
pub async fn rename_library(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Rename>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Library>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    valid_name(&r.name)?;
    let _lock = app.jobs.lock().await;
    let old = load(&app, &id).await?;
    version(old.revision, r.expected_revision)?;
    sqlx::query("UPDATE libraries SET name=?,revision=revision+1 WHERE id=?")
        .bind(r.name.trim())
        .bind(&id)
        .execute(&app.db)
        .await?;
    Ok(Json(load(&app, &id).await?))
}
#[derive(Deserialize, ToSchema, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub expected_revision: i64,
}
async fn idle(app: &App, id: &str) -> Result<(), ApiError> {
    let active:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM jobs WHERE library_id=? AND phase IN ('queued','running','cancelling'))+(SELECT count(*) FROM processing_jobs p JOIN media_files f ON f.id=p.source_file_id WHERE f.library_id=? AND p.phase IN ('queued','running','cancelling'))").bind(id).bind(id).fetch_one(&app.db).await?;
    if playscale_core::catalog::library_idle(active).is_err() {
        Err(ApiError::conflict(
            "library_busy",
            "Wait for or cancel active scans and processing first",
        ))
    } else {
        Ok(())
    }
}
#[utoipa::path(operation_id="detach_library",post,path="/api/v1/admin/libraries/{id}/detach",params(("id"=String,Path)),request_body=Revision,security(("admin_token"=[])),responses((status=200,description="Root disabled and originals unavailable; source bytes, metadata and history retained",body=Library),(status=409,description="Stale revision or active work",body=crate::api::ErrorBody)))]
pub async fn detach(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Revision>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Library>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let _lock = app.jobs.lock().await;
    let old = load(&app, &id).await?;
    version(old.revision, r.expected_revision)?;
    idle(&app, &id).await?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    sqlx::query("UPDATE libraries SET enabled=0,revision=revision+1 WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM scan_schedules WHERE library_id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE media_files SET available=0 WHERE library_id=? AND available=1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(load(&app, &id).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Relocate {
    pub expected_revision: i64,
    pub root: String,
}
#[derive(Clone, PartialEq, sqlx::FromRow)]
struct File {
    id: String,
    relative_path: String,
    revision: String,
    fingerprint: String,
    available: bool,
}
#[utoipa::path(operation_id="relocate_library",post,path="/api/v1/admin/libraries/{id}/relocate",params(("id"=String,Path)),request_body=Relocate,security(("admin_token"=[])),responses((status=200,description="Verified root switch/reactivation preserving IDs and revisions; no files moved",body=Library),(status=400,description="Candidate missing or changed files",body=crate::api::ErrorBody),(status=409,description="Library changed or busy",body=crate::api::ErrorBody),(status=503,description="Another filesystem inspection is active",body=crate::api::ErrorBody)))]
pub async fn relocate(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Relocate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Library>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    let permit = app
        .storage
        .library_io
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "library_io_busy",
                "Library inspection is busy",
            )
        })?;
    let old = load(&app, &id).await?;
    version(old.revision, r.expected_revision)?;
    let files:Vec<File>=sqlx::query_as("SELECT id,relative_path,revision,fingerprint,available FROM media_files WHERE library_id=? ORDER BY id").bind(&id).fetch_all(&app.db).await?;
    let expected = files.clone();
    let data_root = app.storage.root.clone();
    let (root, identity, stamps) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        use std::io::Read;
        use sha2::{Digest, Sha256};
        let _hold = permit;
        let root = std::fs::canonicalize(r.root)?;
        let metadata = std::fs::metadata(&root)?;
        anyhow::ensure!(metadata.is_dir(), "not a directory");
        anyhow::ensure!(
            !root.starts_with(&data_root) && !data_root.starts_with(&root),
            "library cannot overlap server data"
        );
        let identity = db::root_identity(&metadata);
        let mut stamps = Vec::new();
        for file in &expected {
            // Reactivation checks every retained path; an enabled root can retain known missing files.
            if old.enabled && !file.available { continue; }
            let mut opened = crate::scan::open_file(&root, std::path::Path::new(&file.relative_path))?;
            let before = crate::scan::fingerprint(&opened.metadata()?);
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 128 * 1024];
            loop {
                let n = opened.read(&mut buffer)?;
                if n == 0 { break; }
                hash.update(&buffer[..n]);
            }
            anyhow::ensure!(
                format!("{:x}", hash.finalize()) == file.revision
                    && before == crate::scan::fingerprint(&opened.metadata()?),
                "candidate differs from cataloged revision"
            );
            stamps.push((file.id.clone(), before));
        }
        anyhow::ensure!(db::root_identity(&std::fs::metadata(&root)?) == identity,
            "root changed during verification");
        Ok((root.to_string_lossy().into_owned(), identity, stamps))
    }).await.map_err(ApiError::internal)?.map_err(|e| {
        tracing::warn!(%e, "relocation rejected");
        ApiError::bad("Candidate root must contain unchanged cataloged files and must not overlap server data")
    })?;
    let _lock = app.jobs.lock().await;
    let current = load(&app, &id).await?;
    version(current.revision, r.expected_revision)?;
    idle(&app, &id).await?;
    let current:Vec<File>=sqlx::query_as("SELECT id,relative_path,revision,fingerprint,available FROM media_files WHERE library_id=? ORDER BY id").bind(&id).fetch_all(&app.db).await?;
    // Another registration of the same root, or one nested with it, owns it.
    let others: Vec<String> = sqlx::query_scalar("SELECT root FROM libraries WHERE id<>?")
        .bind(&id)
        .fetch_all(&app.db)
        .await?;
    let refs: Vec<&str> = others.iter().map(String::as_str).collect();
    let other = i64::from(
        refs.contains(&root.as_str())
            || playscale_core::sources::overlapping(&root, &refs).is_some(),
    );
    playscale_core::catalog::relocation(&files, &current, other).map_err(|code| {
        ApiError::conflict(
            code,
            "Catalog changed during verification or root belongs to another library",
        )
    })?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    sqlx::query(
        "UPDATE libraries SET root=?,root_identity=?,enabled=1,revision=revision+1,binding_revision=binding_revision+1 WHERE id=?",
    )
    .bind(root)
    .bind(identity)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    for (file, stamp) in stamps {
        sqlx::query("UPDATE media_files SET fingerprint=?,available=1 WHERE id=?")
            .bind(stamp)
            .bind(file)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(load(&app, &id).await?))
}
#[utoipa::path(operation_id="rename_profile",put,path="/api/v1/admin/profiles/{id}",params(("id"=String,Path)),request_body=Rename,security(("admin_token"=[])),responses((status=200,description="Profile renamed; viewing state preserved",body=crate::db::Profile),(status=409,description="Stale revision",body=crate::api::ErrorBody)))]
pub async fn rename_profile(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Rename>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<db::Profile>, ApiError> {
    admin(&app, &headers)?;
    let r = json(body)?;
    valid_name(&r.name)?;
    let _lock = app.jobs.lock().await;
    let old: db::Profile = sqlx::query_as("SELECT * FROM profiles WHERE id=?")
        .bind(&id)
        .fetch_one(&app.db)
        .await?;
    version(old.revision, r.expected_revision)?;
    sqlx::query("UPDATE profiles SET name=?,revision=revision+1 WHERE id=?")
        .bind(r.name.trim())
        .bind(&id)
        .execute(&app.db)
        .await?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM profiles WHERE id=?")
            .bind(id)
            .fetch_one(&app.db)
            .await?,
    ))
}
#[utoipa::path(operation_id="remove_profile",delete,path="/api/v1/admin/profiles/{id}",params(("id"=String,Path),Revision),security(("admin_token"=[])),responses((status=204,description="Nondefault profile and its viewing data removed"),(status=409,description="Default profile or stale revision",body=crate::api::ErrorBody)))]
pub async fn remove_profile(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(r): Query<Revision>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    admin(&app, &headers)?;
    if !playscale_core::catalog::removable_profile(&id) {
        return Err(ApiError::conflict(
            "default_profile",
            "The default profile cannot be removed",
        ));
    }
    let _lock = app.jobs.lock().await;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM profiles WHERE id=?")
        .bind(&id)
        .fetch_one(&app.db)
        .await?;
    version(revision, r.expected_revision)?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    for table in [
        "viewing_state",
        "playback_sessions",
        "progress",
        "playback_preferences",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE profile_id=?"))
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    crate::v2::viewing::delete_profile(&mut tx, &id)
        .await
        .map_err(|p| ApiError::new(p.status, p.code, &p.detail))?;
    sqlx::query("DELETE FROM profiles WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub struct VerifiedRoot {
    pub(crate) root: String,
    pub(crate) identity: String,
    _permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
}

pub async fn candidate_root(app: &App, root: std::path::PathBuf) -> anyhow::Result<VerifiedRoot> {
    let permit = std::sync::Arc::new(app.storage.library_io.clone().try_acquire_owned()?);
    let data = app.storage.root.clone();
    tokio::task::spawn_blocking(move || {
        let root = std::fs::canonicalize(root)?;
        let metadata = std::fs::metadata(&root)?;
        anyhow::ensure!(metadata.is_dir(), "library root must be a directory");
        anyhow::ensure!(
            !root.starts_with(&data) && !data.starts_with(&root),
            "library overlaps server data"
        );
        Ok(VerifiedRoot {
            root: root
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("library root must be UTF-8"))?
                .into(),
            identity: db::root_identity(&metadata),
            _permit: permit,
        })
    })
    .await?
}

pub async fn register_root(
    app: &App,
    candidate: VerifiedRoot,
    name: Option<&str>,
) -> anyhow::Result<db::Library> {
    let name = name.unwrap_or_else(|| {
        std::path::Path::new(&candidate.root)
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("Media")
    });
    let _lock = app.jobs.lock().await;
    let roots: Vec<String> = sqlx::query_scalar("SELECT root FROM libraries")
        .fetch_all(&app.db)
        .await?;
    let refs: Vec<&str> = roots.iter().map(String::as_str).collect();
    if let Some(other) = playscale_core::sources::overlapping(&candidate.root, &refs) {
        anyhow::bail!("library root overlaps registered source {other}");
    }
    // No second path resolution: a replacement is detected by scan/serving's
    // root identity checks, rather than silently registering a different root.
    let library =
        db::add_verified_library(&app.db, name, &candidate.root, &candidate.identity).await?;
    // A v1 root is both a source and a mixed library with the same ID.
    sqlx::query("INSERT OR IGNORE INTO catalog_libraries (id,name,kind) VALUES (?,?,'mixed')")
        .bind(&library.id)
        .bind(&library.name)
        .execute(&app.db)
        .await?;
    sqlx::query("INSERT OR IGNORE INTO library_sources VALUES (?,?)")
        .bind(&library.id)
        .bind(&library.id)
        .execute(&app.db)
        .await?;
    Ok(library)
}
