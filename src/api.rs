use crate::App;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use utoipa::ToSchema;

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

/// Shared host/origin transport boundary; service tests reuse this policy.
pub async fn boundary(State(app): State<App>, request: Request, next: Next) -> Response {
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
    let path = request.uri().path();
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
    let presentation = crate::presentation::router(app.clone());
    router_with(app, assets, Some(presentation))
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
                if let Err(error) = app
                    .access
                    .admit_request(&caller.rate_key(request.extensions()))
                {
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
        || matches!(
            path,
            "/app.js" | "/demuxe.js" | "/playback.js" | "/session.js" | "/style.css"
        )
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
    let mut router = Router::<App>::new()
        .nest("/api/v2", crate::v2::router())
        .route("/ready", get(crate::operations::ready))
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({"status":"ok"})) }),
        );
    if presentation.is_some() {
        router = router.merge(motion_ui::asset_router());
    }
    if let Some(assets) = assets {
        router = router.merge(motion_ui::demuxe_asset_router(assets));
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
        .layer(middleware::from_fn_with_state(
            app.health.metrics.clone(),
            crate::metrics::track,
        ))
        .with_state(app)
}
