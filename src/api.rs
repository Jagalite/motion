use crate::{
    App,
    db::{self, Item, ItemRow, JobRow, Library, Profile, Progress},
    media, now,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{
    router::{OpenApiRouter, UtoipaMethodRouterExt},
    routes,
};

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}
impl ApiError {
    /// Status and stable code, for adapters that re-express legacy errors.
    pub fn parts(&self) -> (StatusCode, &str, &str) {
        (self.status, &self.body.code, &self.body.message)
    }
    pub fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            body: ErrorBody {
                code: code.into(),
                message: message.into(),
            },
        }
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Resource not found")
    }
    pub fn bad(message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }
    pub fn conflict(code: &str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Request could not be completed",
        )
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        if matches!(e, sqlx::Error::RowNotFound) {
            Self::not_found()
        } else {
            Self::internal(e)
        }
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}
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
    let root = crate::administration::candidate_root(&app, body.root.into())
        .await
        .map_err(|_| ApiError::bad("Root unavailable, busy, or overlaps server data"))?;
    let row = crate::administration::register_root(&app, root, Some(&body.name))
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
        id: crate::new_id(),
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
    let current = crate::viewing::load(&app, &profile, &item).await?;
    let next = current
        .state()
        .legacy(body.position_seconds)
        .map_err(|code| {
            ApiError::conflict(
                code,
                "Use the playback-session API or refresh viewing state",
            )
        })?;
    let mut tx = crate::db::begin_write(&app.db).await?;
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

async fn boundary(State(app): State<App>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let v2 = request.uri().path() == "/api/v2" || request.uri().path().starts_with("/api/v2/");
    let reject = |status, code: &'static str, message: &str| {
        if v2 {
            crate::v2::boundary_problem(status, Some(code), message)
        } else {
            ApiError::new(status, code, message).into_response()
        }
    };
    if host != Some(app.authority.as_str()) {
        return reject(
            StatusCode::FORBIDDEN,
            "invalid_host",
            "Unexpected Host header",
        );
    }
    let cors_origin = v2
        .then(|| {
            crate::v2::cors::approved(&app.access.settings.approved_origins, request.headers())
                .map(str::to_owned)
        })
        .flatten();
    if let Some(origin) = &cors_origin
        && crate::v2::cors::is_preflight(request.method(), request.headers())
    {
        return crate::v2::cors::preflight(origin);
    }
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    ) {
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        let cross_site = request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site");
        // Approved third-party origins may mutate only with a bearer token.
        let approved = v2
            && crate::v2::cors::approved(&app.access.settings.approved_origins, request.headers())
                .is_some()
            && crate::v2::cors::bearer_only(request.headers());
        if !approved && (cross_site || origin.is_some_and(|v| v != app.origin.as_str())) {
            return reject(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "Cross-origin mutations are not allowed",
            );
        }
    }
    let legacy = {
        let path = request.uri().path();
        path.starts_with("/api/v1/") || path.starts_with("/media/")
    };
    // v1 responses are not library-scoped, so in restricted mode only a
    // principal whose v2 scope is the whole catalog (an administrator) may
    // read them; v1 mutations still require the operator token.
    let admin = legacy
        && app.access.mode == playscale_core::access::AccessMode::Restricted
        && crate::v2::auth::resolve(&app, request.method(), request.headers())
            .await
            .is_ok_and(|caller| caller.principal.is_admin());
    let path = request.uri().path();
    if legacy && !playscale_core::access::legacy_allowed(app.access.mode, admin) {
        // Restricted mode: the legacy surface cannot bypass v2 grants.
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "restricted_mode",
            "This server requires the v2 API with paired credentials",
        )
        .into_response();
    }
    let is_api = path.starts_with("/api/");
    // Byte routes answer 412/416 with protocol headers and no problem body.
    let bytes = path.starts_with("/api/v2/media/");
    let mut response = next.run(request).await;
    let failed = response.status().is_client_error() || response.status().is_server_error();
    let protocol = matches!(
        response.status(),
        StatusCode::PRECONDITION_FAILED | StatusCode::RANGE_NOT_SATISFIABLE
    ) && bytes;
    if v2 && failed && !protocol && !crate::v2::is_problem_response(&response) {
        // Router-level fallbacks (e.g. 405) run outside the v2 layer.
        response = crate::v2::boundary_problem(
            response.status(),
            None,
            "The request could not be accepted",
        );
    } else if is_api
        && failed
        && !protocol
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_none_or(|v| v != "application/json" && v != "application/problem+json")
    {
        response = ApiError::new(
            response.status(),
            "invalid_request",
            "Request could not be accepted",
        )
        .into_response();
    }
    if let Some(origin) = &cors_origin {
        crate::v2::cors::decorate(&mut response, origin);
    }
    if is_api {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
    }
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-embedder-policy", "require-corp"),
        ("cross-origin-resource-policy", "same-origin"),
        ("referrer-policy", "no-referrer"),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

pub fn router(app: App, assets: Option<std::path::PathBuf>) -> Router {
    router_with(app, assets, None)
}

/// The request identity offered to presentation (Topcoat) renders: the
/// verified session, bearer or trusted-ingress caller, or `None` for anonymous
/// requests and invalid credentials. Protected views must authorize this caller.
#[derive(Clone)]
pub struct PresentationIdentity(pub Option<crate::v2::auth::Caller>);

async fn presentation_identity(
    axum::extract::State(app): axum::extract::State<App>,
    mut request: Request,
    next: Next,
) -> Response {
    let ingress = request
        .extensions()
        .get::<crate::v2::auth::TrustedIngress>()
        .copied();
    let caller =
        match crate::v2::auth::resolve_with(&app, request.method(), request.headers(), ingress)
            .await
        {
            Ok(caller) => {
                if let Err(error) = app.access.admit_request(&caller.principal.id) {
                    return error.into_response();
                }
                Some(caller)
            }
            Err(error) if error.status == StatusCode::UNAUTHORIZED => None,
            // CSRF rejection and storage failures must not become anonymous renders.
            Err(error) => return error.into_response(),
        };
    request
        .extensions_mut()
        .insert(PresentationIdentity(caller));
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    // Even anonymous output varies with current credential/revocation state.
    headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    headers.append(
        header::VARY,
        axum::http::HeaderValue::from_static("cookie, authorization"),
    );
    response
}

/// Unknown API and media paths are JSON errors, never presentation HTML.
async fn api_paths_stay_json(request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api"
        || path.starts_with("/api/")
        || path == "/media"
        || path.starts_with("/media/")
    {
        return ApiError::not_found().into_response();
    }
    next.run(request).await
}

/// Compose the API with an optional presentation router (plan §11.7). The
/// host/origin boundary and default extractor body limit wrap everything, presentation
/// included. API, media and SSE paths keep their JSON errors and are never
/// answered by presentation; only otherwise-unrouted paths reach it, with
/// the request's verified identity in `PresentationIdentity`.
pub fn router_with(
    app: App,
    assets: Option<std::path::PathBuf>,
    presentation: Option<Router>,
) -> Router {
    let (router, mut spec) = OpenApiRouter::<App>::new()
        .routes(routes!(libraries, add_library))
        .routes(routes!(crate::operations::ready))
        .routes(routes!(crate::operations::diagnostics))
        .routes(routes!(crate::processing::submit, crate::processing::list))
        .routes(routes!(crate::processing::get_job))
        .routes(routes!(crate::processing::capabilities))
        .routes(routes!(crate::processing::control))
        .routes(routes!(crate::events::subscribe))
        .routes(routes!(
            crate::maintenance::schedule,
            crate::maintenance::schedules
        ))
        .routes(routes!(
            crate::maintenance::cache_status,
            crate::maintenance::clean_cache
        ))
        .routes(routes!(crate::storage::report, crate::storage::backup_now))
        .routes(routes!(crate::administration::libraries))
        .routes(routes!(crate::administration::rename_library))
        .routes(routes!(crate::administration::detach))
        .routes(routes!(crate::administration::relocate))
        .routes(routes!(
            crate::administration::rename_profile,
            crate::administration::remove_profile
        ))
        .routes(routes!(items))
        .routes(routes!(item))
        .routes(routes!(scan))
        .routes(routes!(job))
        .routes(routes!(cancel))
        .routes(routes!(profiles, add_profile))
        .routes(routes!(admin_files))
        .routes(routes!(save_progress, read_progress))
        .routes(routes!(crate::metadata::get))
        .routes(routes!(crate::metadata::put))
        .routes(routes!(crate::renditions::choices))
        .routes(routes!(crate::playback::plan))
        .routes(routes!(crate::renditions::register))
        .routes(routes!(crate::catalog::browse, crate::catalog::create))
        .routes(routes!(crate::catalog::get))
        .routes(routes!(crate::catalog::put))
        .routes(routes!(
            crate::catalog::editions,
            crate::catalog::add_edition
        ))
        .routes(routes!(crate::catalog::assign))
        .routes(routes!(crate::catalog::rename_edition))
        .routes(routes!(crate::artwork::get))
        .routes(routes!(crate::artwork::upload).layer(DefaultBodyLimit::max(8 * 1024 * 1024)))
        .routes(routes!(crate::artwork::select))
        .routes(routes!(crate::artwork::content))
        .routes(routes!(crate::viewing::get, crate::viewing::watched))
        .routes(routes!(crate::viewing::start))
        .routes(routes!(crate::viewing::get_session, crate::viewing::event))
        .routes(routes!(
            crate::viewing::get_preferences,
            crate::viewing::put_preferences
        ))
        .routes(routes!(crate::viewing::continue_watching))
        .routes(routes!(crate::viewing::next_episode))
        .split_for_parts();
    spec.info.title = "Playscale core API".into();
    spec.info.version = "1".into();
    if let Some(components) = &mut spec.components {
        components.add_security_scheme(
            "admin_token",
            utoipa::openapi::security::SecurityScheme::Http(utoipa::openapi::security::Http::new(
                utoipa::openapi::security::HttpAuthScheme::Bearer,
            )),
        );
    }
    let mut spec = serde_json::to_value(spec).expect("OpenAPI serialization");
    spec["paths"]["/api/v1/items/{id}/artwork/{role}/{source}"]["put"]["requestBody"]["content"]
        ["application/octet-stream"]["schema"] =
        serde_json::json!({"type":"string","format":"binary"});
    spec["paths"]["/api/v1/artwork/{id}/content"]["get"]["responses"]["200"]["content"] = serde_json::json!({
        "image/png":{"schema":{"type":"string","format":"binary"}},
        "image/jpeg":{"schema":{"type":"string","format":"binary"}},
        "image/webp":{"schema":{"type":"string","format":"binary"}}
    });
    // Binary endpoints share the public contract but have no JSON handler response type.
    spec["paths"]["/media/{id}"] = serde_json::json!({
        "parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"string"}},{"name":"revision","in":"query","schema":{"type":"string"}}],
        "get":{"operationId":"getMedia","responses":{"200":{"description":"Complete original file"},"206":{"description":"Single byte range"},"304":{"description":"Unmodified"},"409":{"description":"Source revision changed"},"412":{"description":"Precondition failed"},"416":{"description":"Unsatisfiable range"},"404":{"description":"Unavailable file"},"503":{"description":"Stream capacity exhausted"}}},
        "head":{"operationId":"headMedia","responses":{"200":{"description":"Full representation headers; Range ignored"},"304":{"description":"Unmodified"},"409":{"description":"Source revision changed"},"412":{"description":"Precondition failed"},"404":{"description":"Unavailable file"}}}
    });
    for path in spec["paths"].as_object_mut().unwrap().values_mut() {
        for method in ["get", "post", "put", "head", "delete"] {
            if let Some(operation) = path.get_mut(method) {
                let protected = operation.get("security").is_some();
                let responses = operation["responses"].as_object_mut().unwrap();
                if protected {
                    responses.entry("401").or_insert_with(||serde_json::json!({"description":"Admin token required","content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorBody"}}}}));
                }
                for (status, description) in [
                    ("403", "Host or browser-origin policy rejected the request"),
                    ("413", "Request body exceeds 16 KiB"),
                    ("422", "Request body shape is invalid"),
                    ("500", "Internal failure"),
                ] {
                    responses.entry(status).or_insert_with(||serde_json::json!({"description":description,"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ErrorBody"}}}}));
                }
            }
        }
    }
    for method in ["get", "head"] {
        let parameters = spec["paths"]["/media/{id}"][method]
            .as_object_mut()
            .unwrap()
            .entry("parameters")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .unwrap();
        for name in [
            "Range",
            "If-Range",
            "If-Match",
            "If-None-Match",
            "If-Modified-Since",
            "If-Unmodified-Since",
        ] {
            parameters
                .push(serde_json::json!({"name":name,"in":"header","schema":{"type":"string"}}));
        }
    }
    for status in ["200", "206"] {
        let response = &mut spec["paths"]["/media/{id}"]["get"]["responses"][status];
        response["content"] = serde_json::json!({"application/octet-stream":{"schema":{"type":"string","format":"binary"}}});
        response["headers"] = serde_json::json!({"ETag":{"schema":{"type":"string"}},"Content-Range":{"schema":{"type":"string"}},"Content-Length":{"schema":{"type":"integer","format":"int64"}},"Accept-Ranges":{"schema":{"type":"string"}}});
    }
    let mut router = router
        .nest("/api/v2", crate::v2::router())
        .route(
            "/api/v1/openapi.json",
            get(move || async move { Json(spec) }),
        )
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({"status":"ok"})) }),
        )
        .route("/media/{id}", get(media::serve).head(media::serve))
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/demuxe.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/demuxe.js"),
                )
            }),
        )
        .route(
            "/playback.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/playback.js"),
                )
            }),
        )
        .route(
            "/session.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../web/session.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../web/style.css"),
                )
            }),
        );
    if presentation.is_none() {
        router = router.route(
            "/",
            get(|| async { axum::response::Html(include_str!("../web/index.html")) }),
        );
    }
    if let Some(assets) = assets {
        router = router.nest_service(
            "/assets/demuxe",
            tower_http::services::ServeDir::new(assets),
        );
    }
    let router = match presentation {
        None => router.fallback(|| async { ApiError::not_found() }),
        Some(presentation) => router.fallback_service(
            presentation
                .layer(middleware::from_fn_with_state(
                    app.clone(),
                    presentation_identity,
                ))
                .layer(middleware::from_fn(api_paths_stay_json)),
        ),
    };
    router
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Method not allowed",
            )
        })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), boundary))
        .with_state(app)
}
