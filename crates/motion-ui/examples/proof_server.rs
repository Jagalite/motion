//! Wave 0 proof server: the real `motion_ui::mount` composition with a mock
//! facade, a mock browser-session exchange and mock playback routes.
//!
//! NOT a Motion server. Every API response carries `X-Motion-Mock: 1`. It
//! exists so the desktop proof can load Topcoat HTML, the UI bridge and
//! Demuxe from one loopback origin under the production CSP.
//!
//! stdout protocol (one JSON object per line):
//!   {"listening":"http://127.0.0.1:PORT","bootstrap":"<one-use code>"}
//!   {"request":{"method":..,"path":..,"range":..,"status":..}}

use std::{
    collections::HashMap,
    io::Read,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use motion_ui::{UiPrincipal, facade::ProfileOption, mock::MockUiQueryFacade};
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::services::ServeFile;

#[derive(Default)]
struct Sessions {
    /// One-use bootstrap capability and the instant it stops being valid.
    bootstrap: Option<(String, std::time::Instant)>,
    listening: String,
    sessions: HashMap<String, String>,
    deliveries: HashMap<String, bool>,
    idempotency: HashMap<String, Value>,
    /// Match revision; a decision bumps it, so a stale If-Match gets 412.
    match_revision: u64,
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<Mutex<Sessions>>,
    media: PathBuf,
}

fn random_token() -> String {
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("urandom");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mock(status: StatusCode, body: Value) -> Response {
    (
        status,
        [(header::HeaderName::from_static("x-motion-mock"), "1")],
        Json(body),
    )
        .into_response()
}

fn problem(status: StatusCode, code: &str) -> Response {
    mock(
        status,
        json!({"type": "about:blank", "title": code, "status": status.as_u16(), "code": code, "request_id": "proof", "retryable": false}),
    )
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == "motion_session").then(|| value.to_string())
        })
}

/// The proof's stand-in for the host authentication layer.
async fn authenticate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let token = session_cookie(request.headers());
    let csrf = token.and_then(|t| state.sessions.lock().unwrap().sessions.get(&t).cloned());
    if let Some(csrf) = csrf {
        let unsafe_method = !matches!(request.method().as_str(), "GET" | "HEAD" | "OPTIONS");
        if unsafe_method
            && request.uri().path().starts_with("/api/")
            && request
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                != Some(&csrf)
        {
            return problem(StatusCode::FORBIDDEN, "csrf_required");
        }
        request.extensions_mut().insert(UiPrincipal {
            principal_id: "proof-principal".into(),
            profile_id: "everyone".into(),
            profile_name: "Everyone".into(),
            profiles: vec![ProfileOption {
                id: "everyone".into(),
                name: "Everyone".into(),
            }],
            permissions: [
                "catalog:read",
                "catalog:write",
                "sources:manage",
                "playback:request",
                "processing:request",
                "viewing:write",
            ]
            .map(String::from)
            .to_vec(),
            server_epoch: "proof-epoch".into(),
            csrf_token: csrf,
        });
    } else if request.uri().path().starts_with("/api/v2/")
        && request.uri().path() != "/api/v2/auth/session"
    {
        return problem(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    next.run(request).await
}

async fn log_requests(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let response = next.run(request).await;
    println!(
        "{}",
        json!({"request": {"method": method, "path": path, "range": range, "status": response.status().as_u16()}})
    );
    response
}

/// One-use bootstrap capability exchanged for an HttpOnly session cookie.
async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let credential = body
        .get("credential")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut sessions = state.sessions.lock().unwrap();
    // Exact Host, and Origin when a browser sends one (plan 12.3).
    let expected_host = sessions.listening.trim_start_matches("http://").to_string();
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(expected_host.as_str()) {
        return problem(StatusCode::FORBIDDEN, "host_not_allowed");
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && origin != sessions.listening
    {
        return problem(StatusCode::FORBIDDEN, "origin_not_allowed");
    }
    let valid = matches!(&sessions.bootstrap, Some((code, expires)) if *code == credential && std::time::Instant::now() < *expires);
    if body.get("kind").and_then(Value::as_str) != Some("credential") || !valid {
        return problem(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    sessions.bootstrap = None;
    let token = random_token();
    let csrf = random_token();
    sessions.sessions.insert(token.clone(), csrf.clone());
    let mut response = mock(
        StatusCode::OK,
        json!({"principal": {"id": "proof-principal"}, "csrf_token": csrf, "expires_at": "2099-01-01T00:00:00Z"}),
    );
    let cookie = format!("motion_session={token}; Path=/; HttpOnly; SameSite=Strict");
    response
        .headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
}

/// Mock scan request: requires an idempotency key; logs exactly what arrived.
async fn create_scan(
    State(state): State<AppState>,
    Path(library): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(key) = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return problem(StatusCode::BAD_REQUEST, "idempotency_key_required");
    };
    let mode_ok = matches!(
        body.get("mode").and_then(Value::as_str),
        Some("incremental" | "verify")
    );
    if !mode_ok || !body.get("require_complete").is_some_and(Value::is_boolean) {
        return problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");
    }
    let mut sessions = state.sessions.lock().unwrap();
    let replay = sessions.idempotency.contains_key(&key);
    let scan = sessions
        .idempotency
        .entry(key.clone())
        .or_insert_with(|| json!({"id": "scan-proof", "library_id": library, "status": "queued"}))
        .clone();
    println!(
        "{}",
        json!({"command": {"operation": "createScan", "library": library, "key": key, "body": body, "replay": replay}})
    );
    mock(StatusCode::CREATED, scan)
}

/// Mock match decision: requires If-Match for the current revision.
async fn decide_match(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let mut sessions = state.sessions.lock().unwrap();
    let current = format!("\"r{}\"", sessions.match_revision + 1);
    let condition = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let outcome = match condition.as_deref() {
        None => problem(StatusCode::PRECONDITION_REQUIRED, "precondition_required"),
        Some(c) if c != current => problem(StatusCode::PRECONDITION_FAILED, "precondition_failed"),
        Some(_) => {
            sessions.match_revision += 1;
            mock(StatusCode::OK, json!({"id": id, "status": "accepted"}))
        }
    };
    println!(
        "{}",
        json!({"command": {"operation": "decideMatch", "if_match": condition, "body": body, "status": outcome.status().as_u16()}})
    );
    outcome
}

async fn plan(Json(body): Json<Value>) -> Response {
    let timeline = body
        .get("timeline_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    mock(
        StatusCode::OK,
        json!({
            "status": "ready", "plan_token": format!("proof-plan-{timeline}-0000000000"), "expires_at": "2099-01-01T00:00:00Z",
            "candidate_id": "proof-original", "profile_id": "everyone", "timeline_id": timeline, "version_id": "proof-version",
            "source": {"file_id": "proof", "file_revision": "r1"}, "transport": "http_range", "operation": "original",
            "reason_codes": [], "warnings": ["proof server: no media was evaluated"],
            "tracks": body.get("tracks").cloned().unwrap_or(Value::Null),
        }),
    )
}

async fn admit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(key) = headers.get("idempotency-key").and_then(|v| v.to_str().ok()) else {
        return problem(StatusCode::BAD_REQUEST, "idempotency_key_required");
    };
    let mut sessions = state.sessions.lock().unwrap();
    if let Some(saved) = sessions.idempotency.get(key) {
        return mock(StatusCode::CREATED, saved.clone());
    }
    let id = format!("del{}", sessions.deliveries.len() + 1);
    sessions.deliveries.insert(id.clone(), true);
    let start = body.get("start_ms").and_then(Value::as_u64).unwrap_or(0);
    // Admitted while the first generation is still starting; it becomes
    // active on the next read (exercises the bridge's wait-for-ready path).
    let delivery = delivery_json(&id, start, false);
    sessions
        .idempotency
        .insert(key.to_string(), delivery.clone());
    println!("{}", json!({"delivery_admitted": id}));
    mock(StatusCode::CREATED, delivery)
}

fn delivery_json(id: &str, start: u64, ready: bool) -> Value {
    let generation = json!({"generation": "1", "status": if ready { "active" } else { "starting" },
        "media_url": "/api/v2/media/files/proof/content", "manifest_url": null,
        "media_time_origin_ms": 0, "requested_start_ms": start, "available_start_ms": 0, "available_end_ms": 12000,
        "transport": "http_range", "operation": "original", "error_code": null});
    json!({
        "id": id, "revision": if ready { "2" } else { "1" }, "replacement_mode": "none", "profile_id": "everyone", "timeline_id": "proof",
        "source": {"file_id": "proof", "file_revision": "r1"}, "status": if ready { "ready" } else { "starting" },
        "active": if ready { generation.clone() } else { Value::Null }, "pending": if ready { Value::Null } else { generation },
        "lease_expires_at": "2099-01-01T00:00:00Z", "heartbeat_interval_seconds": 15, "logical_duration_ms": 12000,
    })
}

async fn get_delivery(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.sessions.lock().unwrap().deliveries.get(&id) {
        Some(true) => mock(StatusCode::OK, delivery_json(&id, 0, true)),
        Some(false) => problem(StatusCode::GONE, "delivery_closed"),
        None => problem(StatusCode::NOT_FOUND, "not_found"),
    }
}

async fn close_delivery(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.sessions.lock().unwrap().deliveries.get_mut(&id) {
        Some(open) => {
            // Idempotent close; only the first one changes state.
            if *open {
                println!("{}", json!({"delivery_closed": id}));
            }
            *open = false;
            StatusCode::NO_CONTENT.into_response()
        }
        None => problem(StatusCode::NOT_FOUND, "not_found"),
    }
}

/// Revision-pinned original bytes: single range, conditional requests, same origin.
async fn serve_media(State(state): State<AppState>, request: Request) -> Response {
    let mut response = ServeFile::new(&state.media)
        .oneshot(request)
        .await
        .unwrap()
        .map(Body::new);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-transform"),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    response.headers_mut().insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let listen: SocketAddr = arg("--listen")
        .unwrap_or_else(|| "127.0.0.1:0".into())
        .parse()
        .expect("--listen");
    assert!(
        listen.ip().is_loopback(),
        "the proof server binds to loopback only"
    );
    let media = PathBuf::from(arg("--media").expect("--media <mp4>"));
    let demuxe = arg("--demuxe-dir").map(PathBuf::from);

    let bootstrap = random_token();
    let expires = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let state = AppState {
        sessions: Arc::new(Mutex::new(Sessions {
            bootstrap: Some((bootstrap.clone(), expires)),
            ..Default::default()
        })),
        media,
    };
    let api = Router::new()
        .route("/api/v2/auth/session", post(create_session))
        .route("/api/v2/libraries/{id}/scans", post(create_scan))
        .route(
            "/api/v2/catalog/matches/{id}/decision",
            axum::routing::put(decide_match),
        )
        .route("/api/v2/playback/plans", post(plan))
        .route("/api/v2/playback/delivery-sessions", post(admit))
        .route(
            "/api/v2/playback/delivery-sessions/{id}",
            delete(close_delivery).get(get_delivery),
        )
        .route("/api/v2/media/files/proof/content", get(serve_media))
        .with_state(state.clone());
    let app = motion_ui::mount(api, Arc::new(MockUiQueryFacade), demuxe)
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(middleware::from_fn(log_requests));

    let listener = tokio::net::TcpListener::bind(listen).await.expect("bind");
    let address = listener.local_addr().unwrap();
    state.sessions.lock().unwrap().listening = format!("http://{address}");
    println!(
        "{}",
        json!({"listening": format!("http://{address}"), "bootstrap": bootstrap})
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
