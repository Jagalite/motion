//! Historical transport scaffolding for shared service/failure regressions.
//! This module is compiled only into tests. It is never linked into the server,
//! selected by configuration or used by current-client acceptance. Public v2
//! authorization, canonical history and retired-route absence use api::router.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware,
    routing::get,
};
use playscale::{
    App,
    db::{self, Item, ItemRow, JobRow, Library, Profile, Progress},
    media, now,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

use playscale::api::{ApiError, ErrorBody};

pub(crate) fn admin(app: &App, headers: &HeaderMap) -> Result<(), ApiError> {
    let expected = format!("Bearer {}", app.admin_token);
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        != Some(&expected)
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "admin_required",
            "A local admin token is required",
        ));
    }
    Ok(())
}
pub(crate) fn json<T>(
    value: Result<Json<T>, axum::extract::rejection::JsonRejection>,
) -> Result<T, ApiError> {
    value.map(|Json(v)| v).map_err(|e| {
        ApiError::new(
            e.status(),
            "invalid_json",
            "Invalid JSON body or content type",
        )
    })
}

#[utoipa::path(get, path="/api/v1/libraries", responses((status=200, description="Configured libraries", body=Vec<Library>)))]
async fn libraries(State(app): State<App>) -> Result<Json<Vec<Library>>, ApiError> {
    Ok(Json(
        sqlx::query_as(
            "SELECT id,name FROM libraries WHERE managed=0 AND enabled=1 ORDER BY name,id",
        )
        .fetch_all(&app.db)
        .await?,
    ))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddLibrary {
    name: String,
    root: String,
}
#[utoipa::path(post, path="/api/v1/libraries", request_body=AddLibrary, security(("admin_token"=[])), responses((status=201,description="Library registered",body=Library),(status=400,description="Invalid root or name",body=ErrorBody),(status=401,description="Admin required",body=ErrorBody)))]
async fn add_library(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<AddLibrary>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Library>), ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    let root = playscale::administration::candidate_root(&app, body.root.into())
        .await
        .map_err(|_| ApiError::bad("Root unavailable, busy, or overlaps server data"))?;
    let row = playscale::administration::register_root(&app, root, Some(&body.name))
        .await
        .map_err(|e| {
            tracing::warn!(%e, "library rejected");
            ApiError::bad("Library name or directory is invalid")
        })?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize, IntoParams)]
pub struct Browse {
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    pub q: Option<String>,
    pub library_id: Option<String>,
}
#[derive(Serialize, ToSchema)]
pub struct ItemPage {
    pub items: Vec<Item>,
    pub offset: i64,
    pub limit: i64,
    pub total: i64,
}
#[utoipa::path(get,path="/api/v1/items",params(Browse),responses((status=200,description="Catalog page, including unavailable items",body=ItemPage),(status=400,description="Invalid query",body=ErrorBody)))]
async fn items(
    State(app): State<App>,
    query: Result<Query<Browse>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<ItemPage>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::bad("Invalid query parameters"))?;
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if offset < 0 || !(1..=200).contains(&limit) || query.q.as_ref().is_some_and(|s| s.len() > 200)
    {
        return Err(ApiError::bad(
            "offset must be nonnegative, limit 1–200, query at most 200 bytes",
        ));
    }
    let search = query.q.unwrap_or_default();
    let mut tx = app.db.begin().await?;
    let total = sqlx::query_scalar("SELECT count(*) FROM catalog_files WHERE generated=0 AND instr(lower(title),lower(?))>0 AND (? IS NULL OR library_id=?)")
        .bind(&search).bind(&query.library_id).bind(&query.library_id).fetch_one(&mut *tx).await?;
    let rows: Vec<ItemRow> = sqlx::query_as("SELECT * FROM catalog_files WHERE generated=0 AND instr(lower(title),lower(?))>0 AND (? IS NULL OR library_id=?) ORDER BY title,item_id,id LIMIT ? OFFSET ?")
        .bind(search).bind(&query.library_id).bind(&query.library_id).bind(limit).bind(offset).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(ItemPage {
        items: rows.into_iter().map(ItemRow::public).collect(),
        offset,
        limit,
        total,
    }))
}
#[utoipa::path(get,path="/api/v1/items/{id}",params(("id"=String,Path)),responses((status=200,description="Catalog item",body=Item),(status=404,description="Unknown item",body=ErrorBody)))]
async fn item(State(app): State<App>, Path(id): Path<String>) -> Result<Json<Item>, ApiError> {
    let row: ItemRow = sqlx::query_as(
        "SELECT * FROM catalog_files WHERE item_id=? AND generated=0 ORDER BY available DESC,id LIMIT 1",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await?;
    Ok(Json(row.public()))
}
#[derive(Deserialize, utoipa::IntoParams)]
pub struct ScanMode {
    pub full: Option<bool>,
}
#[utoipa::path(post,path="/api/v1/libraries/{id}/scans",params(("id"=String,Path),ScanMode),security(("admin_token"=[])),responses((status=202,description="Queued or existing active scan",body=JobRow),(status=401,description="Admin required",body=ErrorBody),(status=404,description="Unknown library",body=ErrorBody)))]
async fn scan(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(mode): Query<ScanMode>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<JobRow>), ApiError> {
    admin(&app, &headers)?;
    let exists: i64 =
        sqlx::query_scalar("SELECT count(*) FROM libraries WHERE id=? AND managed=0 AND enabled=1")
            .bind(&id)
            .fetch_one(&app.db)
            .await?;
    if exists == 0 {
        return Err(ApiError::not_found());
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(
            db::enqueue_mode(&app, &id, mode.full.unwrap_or(false))
                .await
                .map_err(|e| ApiError::conflict("scan_conflict", &e.to_string()))?,
        ),
    ))
}
#[utoipa::path(get,path="/api/v1/jobs/{id}",params(("id"=String,Path)),responses((status=200,description="Scan state",body=JobRow),(status=404,description="Unknown job",body=ErrorBody)))]
async fn job(State(app): State<App>, Path(id): Path<String>) -> Result<Json<JobRow>, ApiError> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM jobs WHERE id=?")
            .bind(id)
            .fetch_one(&app.db)
            .await?,
    ))
}
#[utoipa::path(post,path="/api/v1/jobs/{id}/cancel",params(("id"=String,Path)),security(("admin_token"=[])),responses((status=200,description="Current state after cancellation request",body=JobRow),(status=401,description="Admin required",body=ErrorBody),(status=404,description="Unknown job",body=ErrorBody)))]
async fn cancel(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<JobRow>, ApiError> {
    admin(&app, &headers)?;
    let _: JobRow = sqlx::query_as("SELECT * FROM jobs WHERE id=?")
        .bind(&id)
        .fetch_one(&app.db)
        .await?;
    Ok(Json(
        db::cancel(&app, &id).await.map_err(ApiError::internal)?,
    ))
}
#[utoipa::path(get,path="/api/v1/profiles",responses((status=200,description="Viewing profiles",body=Vec<Profile>)))]
async fn profiles(State(app): State<App>) -> Result<Json<Vec<Profile>>, ApiError> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM profiles ORDER BY id")
            .fetch_all(&app.db)
            .await?,
    ))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddProfile {
    name: String,
}
#[utoipa::path(post,path="/api/v1/profiles",request_body=AddProfile,security(("admin_token"=[])),responses((status=201,description="Selectable profile created; not an authentication identity",body=Profile),(status=400,description="Invalid name",body=ErrorBody),(status=401,description="Admin required",body=ErrorBody)))]
async fn add_profile(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<AddProfile>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Profile>), ApiError> {
    admin(&app, &headers)?;
    let body = json(body)?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(ApiError::bad("Profile name requires 1–100 bytes"));
    }
    let profile = Profile {
        id: playscale::new_id(),
        name: name.into(),
        revision: 0,
    };
    sqlx::query("INSERT INTO profiles(id,name) VALUES (?,?)")
        .bind(&profile.id)
        .bind(&profile.name)
        .execute(&app.db)
        .await?;
    Ok((StatusCode::CREATED, Json(profile)))
}
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AdminFile {
    id: String,
    item_id: String,
    library_id: String,
    relative_path: String,
    revision: String,
    available: bool,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct FileBrowse {
    limit: Option<i64>,
    offset: Option<i64>,
    library_id: Option<String>,
    available: Option<bool>,
}
#[utoipa::path(get,path="/api/v1/admin/files",params(FileBrowse),security(("admin_token"=[])),responses((status=200,description="File mapping for importers; bounded page ordered by file ID",body=Vec<AdminFile>),(status=400,description="Invalid pagination",body=ErrorBody),(status=401,description="Admin required",body=ErrorBody)))]
async fn admin_files(
    State(app): State<App>,
    headers: HeaderMap,
    query: Result<Query<FileBrowse>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Vec<AdminFile>>, ApiError> {
    admin(&app, &headers)?;
    let Query(q) = query.map_err(|_| ApiError::bad("Invalid query"))?;
    let limit = q.limit.unwrap_or(100);
    let offset = q.offset.unwrap_or(0);
    if !(1..=200).contains(&limit) || offset < 0 {
        return Err(ApiError::bad(
            "Use limit 1–200, nonnegative offset and optional library_id and available",
        ));
    }
    Ok(Json(sqlx::query_as("SELECT id,item_id,library_id,relative_path,revision,available FROM catalog_files WHERE (? IS NULL OR library_id=?) AND (? IS NULL OR available=?) ORDER BY id LIMIT ? OFFSET ?").bind(&q.library_id).bind(&q.library_id).bind(q.available).bind(q.available).bind(limit).bind(offset).fetch_all(&app.db).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveProgress {
    position_seconds: f64,
}
#[utoipa::path(put,path="/api/v1/profiles/{profile}/progress/{item}",params(("profile"=String,Path),("item"=String,Path)),request_body=SaveProgress,responses((status=409,description="Playback session required",body=ErrorBody),(status=200,description="Legacy position update; rejected once playback sessions are used for this profile/item",body=Progress),(status=400,description="Invalid position",body=ErrorBody),(status=404,description="Unknown profile or item",body=ErrorBody)))]
async fn save_progress(
    State(app): State<App>,
    Path((profile, item)): Path<(String, String)>,
    body: Result<Json<SaveProgress>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Progress>, ApiError> {
    let body = json(body)?;
    if !body.position_seconds.is_finite() || !(0.0..=315_360_000.0).contains(&body.position_seconds)
    {
        return Err(ApiError::bad("Invalid playback position"));
    }
    let _guard = app.jobs.lock().await;
    let Json(current) =
        playscale::viewing::get(State(app.clone()), Path((profile.clone(), item.clone()))).await?;
    let next = playscale_core::viewing::State {
        revision: current.revision,
        session_id: current.session_id,
        position: current.position_seconds,
        automatic_watched: current.automatic_watched,
        manual_watched: current.manual_watched,
    }
    .legacy(body.position_seconds)
    .map_err(|code| {
        ApiError::conflict(
            code,
            "Use the playback-session API or refresh viewing state",
        )
    })?;
    let mut tx = playscale::db::begin_write(&app.db).await?;
    sqlx::query("INSERT INTO progress VALUES (?,?,?,?) ON CONFLICT(profile_id,item_id) DO UPDATE SET position_seconds=excluded.position_seconds,updated_at=excluded.updated_at")
        .bind(&profile).bind(&item).bind(body.position_seconds).bind(now()).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO viewing_state(profile_id,item_id,revision) VALUES (?,?,?) ON CONFLICT(profile_id,item_id) DO UPDATE SET revision=excluded.revision").bind(&profile).bind(&item).bind(next.revision).execute(&mut *tx).await?;
    tx.commit().await?;
    read_progress(State(app.clone()), Path((profile, item))).await
}
#[utoipa::path(get,path="/api/v1/profiles/{profile}/progress/{item}",params(("profile"=String,Path),("item"=String,Path)),responses((status=200,description="Saved position",body=Progress),(status=404,description="No saved position",body=ErrorBody)))]
async fn read_progress(
    State(app): State<App>,
    Path((profile, item)): Path<(String, String)>,
) -> Result<Json<Progress>, ApiError> {
    Ok(Json(
        sqlx::query_as("SELECT * FROM progress WHERE profile_id=? AND item_id=?")
            .bind(profile)
            .bind(item)
            .fetch_one(&app.db)
            .await?,
    ))
}

pub fn router(app: App) -> Router {
    let (router, _) = OpenApiRouter::<App>::new()
        .routes(routes!(libraries, add_library))
        .routes(routes!(playscale::operations::ready))
        .routes(routes!(playscale::operations::diagnostics))
        .routes(routes!(
            playscale::processing::submit,
            playscale::processing::list
        ))
        .routes(routes!(playscale::processing::get_job))
        .routes(routes!(playscale::processing::capabilities))
        .routes(routes!(playscale::processing::control))
        .routes(routes!(playscale::events::subscribe))
        .routes(routes!(
            playscale::maintenance::schedule,
            playscale::maintenance::schedules
        ))
        .routes(routes!(
            playscale::maintenance::cache_status,
            playscale::maintenance::clean_cache
        ))
        .routes(routes!(
            playscale::storage::report,
            playscale::storage::backup_now
        ))
        .routes(routes!(playscale::administration::libraries))
        .routes(routes!(playscale::administration::rename_library))
        .routes(routes!(playscale::administration::detach))
        .routes(routes!(playscale::administration::relocate))
        .routes(routes!(
            playscale::administration::rename_profile,
            playscale::administration::remove_profile
        ))
        .routes(routes!(items))
        .routes(routes!(item))
        .routes(routes!(scan))
        .routes(routes!(job))
        .routes(routes!(cancel))
        .routes(routes!(profiles, add_profile))
        .routes(routes!(admin_files))
        .routes(routes!(save_progress, read_progress))
        .routes(routes!(playscale::metadata::get))
        .routes(routes!(playscale::metadata::put))
        .routes(routes!(playscale::renditions::choices))
        .routes(routes!(playscale::playback::plan))
        .routes(routes!(playscale::renditions::register))
        .routes(routes!(
            playscale::catalog::browse,
            playscale::catalog::create
        ))
        .routes(routes!(playscale::catalog::get))
        .routes(routes!(playscale::catalog::put))
        .routes(routes!(
            playscale::catalog::editions,
            playscale::catalog::add_edition
        ))
        .routes(routes!(playscale::catalog::assign))
        .routes(routes!(playscale::catalog::rename_edition))
        .routes(routes!(playscale::artwork::get))
        .routes(routes!(playscale::artwork::upload).layer(DefaultBodyLimit::max(8 * 1024 * 1024)))
        .routes(routes!(playscale::artwork::select))
        .routes(routes!(playscale::artwork::content))
        .routes(routes!(
            playscale::viewing::get,
            playscale::viewing::watched
        ))
        .routes(routes!(playscale::viewing::start))
        .routes(routes!(
            playscale::viewing::get_session,
            playscale::viewing::event
        ))
        .routes(routes!(
            playscale::viewing::get_preferences,
            playscale::viewing::put_preferences
        ))
        .routes(routes!(playscale::viewing::continue_watching))
        .routes(routes!(playscale::viewing::next_episode))
        .routes(routes!(playscale::delivery::create))
        .routes(routes!(
            playscale::delivery::get,
            playscale::delivery::close
        ))
        .routes(routes!(playscale::delivery::heartbeat))
        .routes(routes!(playscale::delivery::change))
        .routes(routes!(playscale::delivery::activate))
        .split_for_parts();
    router
        .route("/media/{id}", get(media::serve).head(media::serve))
        .route(
            "/api/v1/streams/{id}/{generation}/index.m3u8",
            get(playscale::delivery::playlist),
        )
        .route(
            "/api/v1/streams/{id}/{generation}/init.mp4",
            get(playscale::delivery::init),
        )
        .route(
            "/api/v1/streams/{id}/{generation}/segments/{segment}",
            get(playscale::delivery::segment),
        )
        .route(
            "/api/v1/streams/{id}/{generation}/subtitles.vtt",
            get(playscale::delivery::subtitles),
        )
        .fallback(|| async { ApiError::not_found() })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(
            app.clone(),
            playscale::api::boundary,
        ))
        .with_state(app)
}
