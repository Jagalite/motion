//! `/api/v2` HTTP adapter: problem responses, request identity, strict bodies,
//! preconditions, idempotency records and pages. Access decisions are made by
//! `playscale_core::access`; this module supplies stored facts and executes
//! the decided effects inside one SQLite transaction.
pub mod auth;
pub mod catalog;
pub mod content;
pub mod cors;
pub mod events;
pub mod identity;
pub mod jobs;
pub mod libraries;
pub mod metadata;
pub mod organization;
pub mod system;

use crate::App;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRequest, Request},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use playscale_core::access::{
    AccessError, AccessMode, EventCursor, IDEMPOTENCY_RETENTION_SECONDS, IdempotencyRecord,
    Principal,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
    time::Instant,
};

pub const BODY_LIMIT: usize = 256 * 1024;
pub const PAGE_DEFAULT: u32 = 50;
pub const PAGE_MAX: u32 = 200;
/// How long a desktop bootstrap secret may be exchanged after startup.
pub const BOOTSTRAP_SECONDS: u64 = 60;
/// The local desktop device a bootstrap exchange authenticates as.
pub const DESKTOP_DEVICE: &str = "desktop-local";
/// Pairing creation is unauthenticated: bound it globally per minute.
const PAIRINGS_PER_MINUTE: usize = 30;

/// Deployment settings of the v2 adapter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ApiSettings {
    /// Exact origins (scheme://host[:port]) allowed to call the API with
    /// bearer tokens from a browser. Empty: third-party CORS disabled.
    pub approved_origins: Vec<String>,
    /// Requests per minute per authenticated principal; 0 disables.
    pub requests_per_minute: u32,
    /// Verified private ingress (e.g. `tailscale serve unix:<socket>`).
    pub trusted_ingress: Option<IngressSettings>,
}

/// Identity headers are trusted only on this dedicated Unix-socket listener,
/// which only the configured ingress proxy can reach. A missing or unmapped
/// login fails closed; mapped logins act as their device, never as admin.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IngressSettings {
    pub socket: std::path::PathBuf,
    #[serde(default = "default_login_header")]
    pub login_header: String,
    /// Verified login → paired device ID whose grant and policy apply.
    pub logins: std::collections::BTreeMap<String, String>,
}

fn default_login_header() -> String {
    "tailscale-user-login".into()
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            approved_origins: vec![],
            requests_per_minute: 1_200,
            trusted_ingress: None,
        }
    }
}

impl ApiSettings {
    pub fn validate(&mut self) -> anyhow::Result<()> {
        for origin in &mut self.approved_origins {
            *origin = crate::config::canonical_origin(origin)?;
        }
        if let Some(ingress) = &mut self.trusted_ingress {
            anyhow::ensure!(
                ingress.socket.is_absolute(),
                "trusted_ingress.socket must be an absolute path"
            );
            ingress.login_header = ingress.login_header.to_ascii_lowercase();
            axum::http::HeaderName::from_bytes(ingress.login_header.as_bytes())?;
        }
        Ok(())
    }
}

/// Process-lifetime state of the v2 adapter.
pub struct Runtime {
    pub(crate) renders: std::sync::Arc<tokio::sync::Semaphore>,
    pub settings: ApiSettings,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
    /// Hash and deadline of the one-use desktop bootstrap secret.
    bootstrap: Mutex<Option<(String, Instant)>>,
    pub mode: AccessMode,
    /// Changes on every process start; distinct from the database restore epoch.
    pub server_epoch: String,
    pairing_window: Mutex<VecDeque<Instant>>,
    pub(crate) changes: events::Notifier,
    pub(crate) server_hash: tokio::sync::OnceCell<Option<String>>,
    /// Credential-derivation key; see `auth`.
    pub(crate) key: [u8; 32],
}

impl Runtime {
    pub fn new(mode: AccessMode, key: [u8; 32]) -> Self {
        Self {
            renders: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            settings: ApiSettings::default(),
            buckets: Mutex::new(HashMap::new()),
            bootstrap: Mutex::new(None),
            key,
            mode,
            server_epoch: crate::new_id(),
            pairing_window: Mutex::new(VecDeque::new()),
            changes: events::Notifier::default(),
            server_hash: tokio::sync::OnceCell::new(),
        }
    }
    /// Accept a desktop bootstrap secret for `BOOTSTRAP_SECONDS`, once.
    pub fn set_bootstrap(&self, secret: &str) -> anyhow::Result<()> {
        anyhow::ensure!(secret.len() >= 32, "bootstrap secret is too short");
        *self.bootstrap.lock().unwrap() = Some((
            auth::token_hash(secret),
            Instant::now() + std::time::Duration::from_secs(BOOTSTRAP_SECONDS),
        ));
        Ok(())
    }

    /// Consume the bootstrap secret if `secret` is it and it is still fresh.
    /// Any presentation, right or wrong, after the deadline clears it.
    pub(crate) fn take_bootstrap(&self, secret: &str) -> bool {
        let mut slot = self.bootstrap.lock().unwrap();
        match slot.as_ref() {
            Some((_, deadline)) if Instant::now() >= *deadline => {
                *slot = None;
                false
            }
            Some((hash, _)) if *hash == auth::token_hash(secret) => {
                *slot = None;
                true
            }
            _ => false,
        }
    }

    pub fn with_settings(mut self, settings: ApiSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Token bucket per principal: capacity and refill per minute.
    pub(crate) fn admit_request(&self, principal: &str) -> Result<(), Problem> {
        let rate = f64::from(self.settings.requests_per_minute);
        if rate == 0.0 {
            return Ok(());
        }
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        if buckets.len() > 10_000 {
            // Bound memory: forget principals idle for a full refill period.
            buckets.retain(|_, (_, at)| now.duration_since(*at).as_secs() < 60);
        }
        let (tokens, at) = buckets.entry(principal.to_owned()).or_insert((rate, now));
        *tokens = (*tokens + now.duration_since(*at).as_secs_f64() * rate / 60.0).min(rate);
        *at = now;
        if *tokens < 1.0 {
            let wait = ((1.0 - *tokens) * 60.0 / rate).ceil() as u64;
            return Err(Problem::rate_limited(wait));
        }
        *tokens -= 1.0;
        Ok(())
    }

    fn admit_pairing_request(&self) -> Result<(), Problem> {
        let mut window = self.pairing_window.lock().unwrap();
        let now = Instant::now();
        while window
            .front()
            .is_some_and(|t| now.duration_since(*t).as_secs() >= 60)
        {
            window.pop_front();
        }
        if window.len() >= PAIRINGS_PER_MINUTE {
            return Err(Problem::rate_limited(60));
        }
        window.push_back(now);
        Ok(())
    }
}

tokio::task_local! {
    static REQUEST_ID: String;
}

/// RFC 9457 problem with a stable code. Details are redacted and actionable.
#[derive(Debug)]
pub struct Problem {
    pub status: StatusCode,
    pub code: &'static str,
    pub detail: String,
    pub retryable: bool,
    pub retry_after: Option<u64>,
}

#[derive(Serialize)]
struct ProblemBody<'a> {
    #[serde(rename = "type")]
    kind: String,
    title: &'a str,
    status: u16,
    code: &'a str,
    detail: &'a str,
    request_id: String,
    retryable: bool,
}

impl Problem {
    pub fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            retryable: status == StatusCode::SERVICE_UNAVAILABLE
                || status == StatusCode::TOO_MANY_REQUESTS,
            retry_after: None,
        }
    }
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "The resource does not exist or is not accessible",
        )
    }
    pub fn invalid(code: &'static str, detail: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, detail)
    }
    pub fn bad(code: &'static str, detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, detail)
    }
    pub fn rate_limited(seconds: u64) -> Self {
        Self {
            retry_after: Some(seconds.max(1)),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many requests; retry later",
            )
        }
    }
    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "v2 request failed");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The request could not be completed",
        )
    }
}

impl From<sqlx::Error> for Problem {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => Self::not_found(),
            sqlx::Error::Database(d) if d.message().contains("database is locked") => {
                system::DB_BUSY_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Self {
                    retry_after: Some(1),
                    ..Self::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "database_busy",
                        "The database is busy; retry the request",
                    )
                }
            }
            _ => Self::internal(e),
        }
    }
}

impl From<AccessError> for Problem {
    fn from(e: AccessError) -> Self {
        use AccessError::*;
        let (status, detail) = match &e {
            Unauthenticated => (StatusCode::UNAUTHORIZED, "Authentication is required"),
            CredentialRevoked | CredentialSuperseded => (
                StatusCode::UNAUTHORIZED,
                "The credential is no longer valid; pair the device again",
            ),
            CredentialExpired => (StatusCode::UNAUTHORIZED, "The credential has expired"),
            Forbidden(_) => (
                StatusCode::FORBIDDEN,
                "The principal lacks the required permission",
            ),
            ParentCredentialRequired => (
                StatusCode::FORBIDDEN,
                "A device credential is required for this operation",
            ),
            PairingExpired => (
                StatusCode::CONFLICT,
                "The pairing expired; start a new pairing",
            ),
            PairingPending => (StatusCode::CONFLICT, "The pairing is awaiting approval"),
            PairingDecided => (StatusCode::CONFLICT, "The pairing was already approved"),
            UserCodeMismatch => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "The user code does not match the pairing",
            ),
            UnknownProfile => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "A referenced profile does not exist",
            ),
            UnknownLibrary => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "A referenced library does not exist",
            ),
            InvalidTtl => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "ttl_seconds must be between 60 and 3600",
            ),
            StaleRevision => (
                StatusCode::PRECONDITION_FAILED,
                "The resource changed; read it again and retry with the new ETag",
            ),
            DeviceRevoked => (StatusCode::CONFLICT, "The device is revoked"),
            IdempotencyMismatch => (
                StatusCode::CONFLICT,
                "The Idempotency-Key was used with a different request",
            ),
            ReplayUnavailable => (
                StatusCode::CONFLICT,
                "The original result was revoked; retry with a new Idempotency-Key",
            ),
            InvalidCursor => (StatusCode::BAD_REQUEST, "The event cursor is invalid"),
        };
        let mut problem = Self::new(status, e.code(), detail);
        problem.retryable = e == PairingPending;
        if e == PairingPending {
            problem.retry_after = Some(playscale_core::access::PAIRING_POLL_SECONDS as u64);
        }
        problem
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let title = self.status.canonical_reason().unwrap_or("Error");
        let body = ProblemBody {
            kind: format!("/problems/{}", self.code),
            title,
            status: self.status.as_u16(),
            code: self.code,
            detail: &self.detail,
            request_id: REQUEST_ID
                .try_with(Clone::clone)
                .unwrap_or_else(|_| crate::new_id()),
            retryable: self.retryable,
        };
        let mut response = (self.status, Json(body)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

fn is_problem(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v == "application/problem+json")
}

fn generic_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::PAYLOAD_TOO_LARGE => "body_too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        s if s.is_server_error() => "internal_error",
        _ => "invalid_request",
    }
}

fn stamp(response: &mut Response, id: &str) {
    let headers = response.headers_mut();
    headers.insert("x-request-id", HeaderValue::from_str(id).unwrap());
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// A v2 problem produced outside the v2 router (host/origin boundary or
/// router-level fallbacks), with its own request identity.
pub fn boundary_problem(status: StatusCode, code: Option<&'static str>, detail: &str) -> Response {
    let id = crate::new_id();
    let problem = Problem::new(status, code.unwrap_or(generic_code(status)), detail);
    let mut response = REQUEST_ID.sync_scope(id.clone(), || problem.into_response());
    stamp(&mut response, &id);
    response
}

pub fn is_problem_response(response: &Response) -> bool {
    is_problem(response)
}

/// Assigns the request ID and converts framework rejections into problems.
async fn request_scope(request: Request, next: Next) -> Response {
    let id = crate::new_id();
    // Byte routes answer 304/412/416 with protocol headers (Content-Range,
    // ETag) and no body; those responses pass through unchanged.
    let bytes = request.uri().path().starts_with("/api/v2/media/")
        || request.uri().path().starts_with("/media/");
    REQUEST_ID
        .scope(id.clone(), async move {
            let mut response = next.run(request).await;
            let status = response.status();
            if (status.is_client_error() || status.is_server_error())
                && !is_problem(&response)
                && !(bytes
                    && matches!(
                        status,
                        StatusCode::PRECONDITION_FAILED | StatusCode::RANGE_NOT_SATISFIABLE
                    ))
            {
                response = Problem::new(
                    status,
                    generic_code(status),
                    "The request could not be accepted",
                )
                .into_response();
            }
            stamp(&mut response, &id);
            response
        })
        .await
}

/// Strict JSON body with its canonical digest for idempotency comparison.
pub struct Body<T> {
    pub value: T,
    pub digest: String,
}

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = Problem;
    async fn from_request(request: Request, state: &S) -> Result<Self, Problem> {
        let json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return Err(Problem::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Send application/json",
            ));
        }
        let bytes = axum::body::Bytes::from_request(request, state)
            .await
            .map_err(|e| {
                if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    Problem::new(
                        e.status(),
                        "body_too_large",
                        "The request body is too large",
                    )
                } else {
                    Problem::bad("invalid_request", "The request body could not be read")
                }
            })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| Problem::bad("invalid_json", "The request body is not valid JSON"))?;
        // serde_json maps are ordered, so this serialization is canonical.
        let digest = sha256(value.to_string().as_bytes());
        let value = T::deserialize(value)
            .map_err(|e| Problem::invalid("invalid_body", format!("Invalid request body: {e}")))?;
        Ok(Self { value, digest })
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn etag(revision: u64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"r-{revision}\"")).unwrap()
}

/// The revision named by a strong `If-Match`. Missing: 428. A value that names
/// no revision cannot match the current one: 412.
pub fn if_match(headers: &HeaderMap) -> Result<u64, Problem> {
    let value = headers.get(header::IF_MATCH).ok_or_else(|| {
        Problem::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "Send If-Match with the ETag from the current resource",
        )
    })?;
    let value = value.to_str().unwrap_or_default().trim();
    if value == "*" {
        return Err(Problem::bad(
            "wildcard_precondition",
            "Wildcard If-Match is not accepted",
        ));
    }
    value
        .strip_prefix("\"r-")
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| AccessError::StaleRevision.into())
}

pub fn idempotency_key(headers: &HeaderMap) -> Result<String, Problem> {
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            Problem::bad("idempotency_key_required", "Send an Idempotency-Key header")
        })?;
    if !(16..=128).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Problem::bad(
            "invalid_idempotency_key",
            "Idempotency-Key must be 16-128 visible ASCII characters",
        ));
    }
    Ok(key.into())
}

/// A stored acknowledgement, scoped by principal, operation, target and key.
pub struct Stored {
    pub record: IdempotencyRecord,
    pub status: StatusCode,
    pub body: Option<serde_json::Value>,
    pub issued_nonce: Option<String>,
}

/// digest, status, body, issued_nonce, expires_at
type RecordTuple = (String, i64, Option<String>, Option<String>, i64);

pub struct Scope<'a> {
    pub principal: &'a str,
    pub operation: &'static str,
    pub target: &'a str,
    pub key: &'a str,
}

impl Scope<'_> {
    pub async fn load(&self, tx: &mut sqlx::SqliteConnection) -> Result<Option<Stored>, Problem> {
        let row: Option<RecordTuple> = sqlx::query_as(
            "SELECT digest,status,body,issued_nonce,expires_at FROM idempotency_records WHERE principal_id=? AND operation=? AND target=? AND key=?",
        )
        .bind(self.principal)
        .bind(self.operation)
        .bind(self.target)
        .bind(self.key)
        .fetch_optional(&mut *tx)
        .await?;
        row.map(|(digest, status, body, issued_nonce, expires_at)| {
            Ok(Stored {
                record: IdempotencyRecord { digest, expires_at },
                status: StatusCode::from_u16(status as u16).map_err(Problem::internal)?,
                body: body
                    .map(|b| serde_json::from_str(&b))
                    .transpose()
                    .map_err(Problem::internal)?,
                issued_nonce,
            })
        })
        .transpose()
    }
    pub async fn save(
        &self,
        tx: &mut sqlx::SqliteConnection,
        digest: &str,
        status: StatusCode,
        body: Option<&serde_json::Value>,
        issued_nonce: Option<&str>,
    ) -> Result<(), Problem> {
        sqlx::query(
            "INSERT INTO idempotency_records(principal_id,operation,target,key,digest,status,body,issued_nonce,expires_at) VALUES (?,?,?,?,?,?,?,?,?)
             ON CONFLICT(principal_id,operation,target,key) DO UPDATE SET digest=excluded.digest,status=excluded.status,body=excluded.body,issued_nonce=excluded.issued_nonce,expires_at=excluded.expires_at",
        )
        .bind(self.principal)
        .bind(self.operation)
        .bind(self.target)
        .bind(self.key)
        .bind(digest)
        .bind(status.as_u16() as i64)
        .bind(body.map(|b| b.to_string()))
        .bind(issued_nonce)
        .bind(crate::now() + IDEMPOTENCY_RETENTION_SECONDS)
        .execute(&mut *tx)
        .await?;
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

impl PageQuery {
    pub fn limit(&self) -> Result<u32, Problem> {
        match self.limit.unwrap_or(PAGE_DEFAULT) {
            n @ 1..=PAGE_MAX => Ok(n),
            _ => Err(Problem::bad(
                "invalid_limit",
                "limit must be between 1 and 200",
            )),
        }
    }
    pub fn after(&self) -> Result<&str, Problem> {
        match self.cursor.as_deref() {
            None => Ok(""),
            Some(c) if !c.is_empty() && c.len() <= 2048 => Ok(c),
            Some(_) => Err(Problem::bad("invalid_cursor", "The page cursor is invalid")),
        }
    }
}

#[derive(Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub read_revision: String,
    pub event_cursor: String,
}

/// Rows were fetched with `limit + 1`; the extra row only signals another page.
/// Call inside the read transaction so the event cursor matches the snapshot.
pub async fn page<T>(
    tx: &mut sqlx::SqliteConnection,
    principal: &Principal,
    mut rows: Vec<(String, T)>,
    limit: u32,
) -> Result<Page<T>, Problem> {
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = more
        .then(|| rows.last().map(|(id, _)| id.clone()))
        .flatten();
    let (position, epoch) = events::snapshot(tx).await?;
    Ok(Page {
        items: rows.into_iter().map(|(_, v)| v).collect(),
        next_cursor,
        read_revision: position.to_string(),
        event_cursor: EventCursor {
            epoch,
            principal: principal.id.clone(),
            policy_revision: principal.policy_revision,
            position,
        }
        .encode(),
    })
}

/// Bind the trusted-ingress Unix socket (mode 0600). A stale socket from a
/// previous run is replaced; any other file at the path is an error.
pub fn ingress_listener(path: &std::path::Path) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => anyhow::bail!("{} exists and is not a socket", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// A SQL condition admitting rows whose library column is readable under
/// `scope`. Bind the two returned values in order. Use it inside the read
/// query, before counting, filtering or paginating.
pub fn scope_clause(
    scope: &playscale_core::access::CatalogScope,
    column: &str,
) -> (String, bool, String) {
    use playscale_core::access::CatalogScope::*;
    let (all, ids) = match scope {
        All => (true, "[]".to_string()),
        Libraries(ids) => (false, serde_json::to_string(ids).expect("ids serialize")),
        Nothing => (false, "[]".to_string()),
    };
    (
        format!(
            "(? OR {column} IN (SELECT source_id FROM library_sources WHERE library_id IN (SELECT value FROM json_each(?))))"
        ),
        all,
        ids,
    )
}

/// UTC RFC 3339 from Unix seconds.
pub fn timestamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

pub fn router() -> Router<App> {
    Router::new()
        .route("/openapi.json", get(system::openapi))
        .route("/system/health", get(system::health))
        .route("/admin/diagnostics", get(system::diagnostics))
        .route("/system/capabilities", get(system::capabilities))
        .route("/me", get(identity::me))
        .route("/auth/pairings", post(identity::create_pairing))
        .route(
            "/auth/pairings/{pairing_id}/approve",
            post(identity::approve_pairing),
        )
        .route(
            "/auth/pairings/{pairing_id}/claim",
            post(identity::claim_pairing),
        )
        .route(
            "/auth/session",
            post(identity::create_session)
                .get(identity::get_session)
                .delete(identity::delete_session),
        )
        .route("/auth/access-tokens", post(identity::issue_access_token))
        .route("/devices", get(identity::list_devices))
        .route(
            "/devices/{device_id}",
            get(identity::get_device).delete(identity::revoke_device),
        )
        .route(
            "/devices/{device_id}/policy",
            get(identity::get_policy).put(identity::replace_policy),
        )
        .route(
            "/profiles",
            get(identity::list_profiles).post(identity::create_profile),
        )
        .route(
            "/profiles/{profile_id}",
            get(identity::get_profile)
                .put(identity::replace_profile)
                .delete(identity::delete_profile),
        )
        .route("/events", get(events::subscribe))
        .route("/content-access", post(content::create))
        .route(
            "/content-access/{access_id}",
            axum::routing::delete(content::revoke),
        )
        .route(
            "/media/files/{file_id}/content",
            get(content::file_content).head(content::file_content),
        )
        .merge(organization::routes())
        .merge(catalog::routes())
        .merge(libraries::routes())
        .merge(jobs::routes())
        .merge(metadata::routes())
        .fallback(|| async { Problem::not_found() })
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn(request_scope))
}

#[cfg(test)]
mod tests {
    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(super::timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::timestamp(1_791_590_399), "2026-10-09T23:59:59Z");
    }
}
