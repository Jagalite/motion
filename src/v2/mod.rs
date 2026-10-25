//! `/api/v2` HTTP adapter: problem responses, request identity, strict bodies,
//! preconditions, idempotency records and pages. Access decisions are made by
//! `playscale_core::access`; this module supplies stored facts and executes
//! the decided effects inside one SQLite transaction.
mod access_management;
pub mod admin;
mod admin_reads;
pub mod auth;
pub mod catalog;
pub mod content;
pub mod cors;
pub mod downloads;
pub mod events;
pub mod identity;
pub mod jobs;
pub mod libraries;
pub mod markers;
pub mod metadata;
pub mod monitor;
pub mod organization;
pub mod playback;
pub mod previews;
pub mod processing;
pub mod scans;
pub mod schedules;
pub mod signin;
pub mod system;
pub mod viewing;

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
    /// Mail routing: system DNS/WebPKI, or an explicit isolated fixture.
    pub mail: crate::mail::Runtime,
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
            mail: crate::mail::Runtime::default(),
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
            PairingDenied => (StatusCode::CONFLICT, "The pairing was denied"),
            UserDisabled => (StatusCode::CONFLICT, "The access user is disabled"),
            PolicyLimitExceeded => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "Combined device restrictions exceed 200 blocked labels; adjust the device policy or approve a new device before changing this user",
            ),
            GrantExceeded => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "The grant exceeds the user access",
            ),
            LastAdministrator => (
                StatusCode::CONFLICT,
                "Keep another usable administrator device before removing this access",
            ),
            InvitationUnavailable => (StatusCode::CONFLICT, "The invitation is unavailable"),
            PairingPending => (StatusCode::CONFLICT, "The pairing is awaiting approval"),
            PairingDecided => (StatusCode::CONFLICT, "The pairing already has a decision"),
            OwnerSetupUnavailable => (
                StatusCode::CONFLICT,
                "Users already exist; owner setup never adopts an existing user. Approve devices for an explicit user",
            ),
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

/// Native mode is a positive capability allowlist. Unknown routes must never
/// reach handlers that still read or mutate the frozen legacy catalog.
async fn native_catalog_gate(
    axum::extract::State(app): axum::extract::State<App>,
    request: Request,
    next: Next,
) -> Response {
    if app.native_catalog.is_none() {
        // A missing retained owner never turns an already-bound catalog back
        // into a legacy server. This applies even to individually supported
        // routes: their adapters would otherwise see None and take old paths.
        let bound = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM catabolic_binding WHERE id=1)",
        )
        .fetch_one(&app.db)
        .await;
        if !matches!(bound, Ok(false)) {
            return Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "native_runtime_unavailable",
                "The native catalog owner is not admitted; legacy fallback is disabled",
            )
            .into_response();
        }
    }
    if app.native_catalog.is_some()
        && !native_route_supported(request.method().as_str(), request.uri().path())
    {
        return Problem::new(StatusCode::SERVICE_UNAVAILABLE, "native_route_unavailable",
            "This operation requires a native catalog adapter that is not yet available; legacy fallback is disabled").into_response();
    }
    next.run(request).await
}
fn native_route_supported(method: &str, path: &str) -> bool {
    let path = path.strip_prefix("/api/v2").unwrap_or(path);
    if path.starts_with("/auth/") || path == "/me" {
        return true;
    }
    if matches!(
        path,
        "/system/health" | "/system/capabilities" | "/openapi.json"
    ) {
        return matches!(method, "GET" | "HEAD");
    }
    // Each hint reauthorizes under a current writer and native owner scope.
    if path == "/events" {
        return method == "GET";
    }
    if matches!(path, "/sources" | "/libraries") {
        return matches!(method, "GET" | "HEAD" | "POST");
    }
    if path
        .strip_prefix("/sources/")
        .is_some_and(catalog::valid_id)
    {
        return matches!(method, "GET" | "HEAD" | "DELETE");
    }
    if path
        .strip_prefix("/libraries/")
        .is_some_and(catalog::valid_id)
    {
        return matches!(method, "GET" | "HEAD" | "PUT");
    }
    // Saved filters store a validated, inert AST. This does not admit smart
    // collection evaluation or turn referenced strings into catalog grants.
    if path == "/catalog/filters" {
        return matches!(method, "GET" | "POST");
    }
    if path
        .strip_prefix("/catalog/filters/")
        .is_some_and(catalog::valid_id)
    {
        return matches!(method, "GET" | "PUT" | "DELETE");
    }
    // Manual collections resolve members through the retained native owner.
    // Smart evaluation and queue execution remain separately closed.
    if path == "/collections" {
        return matches!(method, "GET" | "POST");
    }
    if path
        .strip_prefix("/collections/")
        .is_some_and(catalog::valid_id)
    {
        return matches!(method, "GET" | "PUT" | "DELETE");
    }
    // Playlist membership is current typed timeline identity, not playback.
    if path == "/playlists" {
        return matches!(method, "GET" | "POST");
    }
    if path
        .strip_prefix("/playlists/")
        .is_some_and(catalog::valid_id)
    {
        return matches!(method, "GET" | "PUT" | "DELETE");
    }
    // Application-owned administration still uses fresh authorization and
    // exact native library references. Read-only diagnostics/auth routes are
    // individually selected below, never admitted by an admin prefix.
    if matches!(path, "/users" | "/access/setup") {
        return matches!(method, "GET" | "POST");
    }
    if path == "/admin/metrics" {
        return method == "GET";
    }
    if admin_reads::supported(method, path) {
        return true;
    }
    if path.strip_prefix("/users/").is_some_and(catalog::valid_id) {
        return matches!(method, "GET" | "PUT");
    }
    if path == "/devices" {
        return method == "GET";
    }
    if path == "/profiles" {
        return matches!(method, "GET" | "POST");
    }
    if let Some(id) = path.strip_prefix("/profiles/") {
        if catalog::valid_id(id) {
            return matches!(method, "GET" | "PUT" | "DELETE");
        }
    }
    if let Some(tail) = path.strip_prefix("/devices/") {
        if catalog::valid_id(tail) {
            return matches!(method, "GET" | "DELETE");
        }
        if let Some(id) = tail.strip_suffix("/user") {
            return catalog::valid_id(id) && method == "PUT";
        }
        if let Some(id) = tail.strip_suffix("/policy") {
            return catalog::valid_id(id) && matches!(method, "GET" | "PUT");
        }
    }
    // Exact scan-only schedule adapter methods, never watch or task siblings.
    if path == "/schedules" {
        return matches!(method, "GET" | "POST");
    }
    if path
        .strip_prefix("/schedules/")
        .is_some_and(catalog::valid_id)
    {
        return matches!(method, "GET" | "PUT" | "DELETE");
    }
    // Native durable scan requests retain their exact accepted source scope.
    // Explicit methods only: HEAD and sibling management routes stay closed.
    if path == "/scans" {
        return method == "GET";
    }
    if let Some(id) = path.strip_prefix("/scans/") {
        if catalog::valid_id(id) {
            return method == "GET";
        }
        if let Some(id) = id.strip_suffix("/cancel") {
            return catalog::valid_id(id) && method == "POST";
        }
    }
    if let Some(id) = path
        .strip_prefix("/libraries/")
        .and_then(|p| p.strip_suffix("/scans"))
    {
        return catalog::valid_id(id) && method == "POST";
    }
    // Each enabled physical operation retains its native identity/generation,
    // current authorization and exact occurrence fences. Do not open families.
    if path == "/processing/preparations" {
        return method == "POST";
    }
    if path == "/processing/renditions" {
        return method == "GET";
    }
    // Descriptive processing-job reads and only the bounded preparation
    // controls. The native handler independently requires retained admission;
    // a route match never adopts legacy history or another job family.
    if path == "/jobs" || path.strip_prefix("/jobs/").is_some_and(catalog::valid_id) {
        return method == "GET";
    }
    if let Some(tail) = path.strip_prefix("/jobs/") {
        if let Some(id) = tail
            .strip_suffix("/cancel")
            .or_else(|| tail.strip_suffix("/retry"))
        {
            return method == "POST" && catalog::valid_id(id);
        }
    }
    if path == "/playback/plans" {
        return method == "POST";
    }
    if matches!(
        path,
        "/playback/delivery-sessions" | "/playback/viewing-sessions" | "/content-access"
    ) {
        return method == "POST";
    }
    if let Some(tail) = path.strip_prefix("/playback/delivery-sessions/") {
        if catalog::valid_id(tail) {
            return matches!(method, "GET" | "DELETE");
        }
        if let Some(id) = tail.strip_suffix("/heartbeat") {
            return catalog::valid_id(id) && method == "POST";
        }
        if let Some(id) = tail.strip_suffix("/changes") {
            return catalog::valid_id(id) && method == "POST";
        }
        let segments: Vec<_> = tail.split('/').collect();
        if let [id, "generations", generation, "activate"] = segments.as_slice() {
            return method == "POST"
                && catalog::valid_id(id)
                && generation.bytes().all(|b| b.is_ascii_digit())
                && generation.parse::<u64>().is_ok_and(|n| n > 0);
        }
    }
    if let Some(tail) = path.strip_prefix("/playback/viewing-sessions/") {
        if catalog::valid_id(tail) {
            return method == "GET";
        }
        if let Some(id) = tail.strip_suffix("/events") {
            return catalog::valid_id(id) && method == "POST";
        }
        if let Some(id) = tail.strip_suffix("/delivery") {
            return catalog::valid_id(id) && method == "PUT";
        }
    }
    if let Some(tail) = path.strip_prefix("/streams/") {
        let segments: Vec<_> = tail.split('/').collect();
        if let [delivery, generation, "content"] = segments.as_slice() {
            return catalog::valid_id(delivery) && catalog::valid_id(generation) && method == "GET";
        }
    }
    if path == "/offline/sync" {
        return method == "POST";
    }
    if path == "/downloads" {
        return matches!(method, "GET" | "POST");
    }
    if let Some(tail) = path.strip_prefix("/downloads/") {
        if catalog::valid_id(tail) {
            return matches!(method, "GET" | "DELETE");
        }
        if let Some(id) = tail.strip_suffix("/transfer") {
            return catalog::valid_id(id) && method == "PUT";
        }
        if let Some(id) = tail.strip_suffix("/manifest") {
            return catalog::valid_id(id) && method == "GET";
        }
    }
    if let Some(tail) = path.strip_prefix("/media/files/") {
        let segments: Vec<_> = tail.split('/').collect();
        if let [id, "previews", sheet] = segments.as_slice() {
            return catalog::valid_id(id)
                && !sheet.is_empty()
                && sheet.bytes().all(|b| b.is_ascii_digit())
                && sheet.parse::<u32>().is_ok_and(|n| n > 0)
                && method == "GET";
        }
    }
    if let Some(id) = path
        .strip_prefix("/media/files/")
        .and_then(|tail| tail.strip_suffix("/content"))
    {
        return catalog::valid_id(id) && method == "GET";
    }
    if let Some(id) = path
        .strip_prefix("/media/assets/")
        .and_then(|tail| tail.strip_suffix("/content"))
    {
        return catalog::valid_id(id) && matches!(method, "GET" | "HEAD");
    }
    if let Some(tail) = path.strip_prefix("/catalog/items/") {
        let segments: Vec<_> = tail.split('/').collect();
        match segments.as_slice() {
            [id, "artwork"] if catalog::valid_id(id) => return matches!(method, "GET" | "POST"),
            [id, "artwork", "selection"] if catalog::valid_id(id) => {
                return matches!(method, "GET" | "PUT");
            }
            [id, "metadata"] if catalog::valid_id(id) => return method == "GET",
            [id, "metadata", "contributions", source]
                if catalog::valid_id(id) && catalog::valid_id(source) =>
            {
                return method == "PUT";
            }
            _ => {}
        }
    }
    if let Some(id) = path
        .strip_prefix("/catalog/items/")
        .and_then(|tail| tail.strip_suffix("/relationships"))
    {
        return catalog::valid_id(id) && matches!(method, "GET" | "POST");
    }
    // Native detail adapters independently verify complete current owners.
    if let Some(tail) = path.strip_prefix("/catalog/items/") {
        let segments: Vec<_> = tail.split('/').collect();
        if let [id, "editions" | "timelines"] = segments.as_slice() {
            return catalog::valid_id(id) && method == "GET";
        }
    }
    if let Some(tail) = path.strip_prefix("/catalog/timelines/") {
        let segments: Vec<_> = tail.split('/').collect();
        return match segments.as_slice() {
            [id] | [id, "versions"] => catalog::valid_id(id) && method == "GET",
            [id, "markers" | "previews"] => {
                catalog::valid_id(id) && matches!(method, "GET" | "POST")
            }
            _ => false,
        };
    }
    if let Some(id) = path.strip_prefix("/catalog/relationships/") {
        return catalog::valid_id(id) && matches!(method, "GET" | "DELETE");
    }
    if path == "/catalog/items" {
        return matches!(method, "GET" | "HEAD" | "POST");
    }
    if path == "/catalog/search" {
        return matches!(method, "GET" | "HEAD");
    }
    if let Some(tail) = path.strip_prefix("/profiles/") {
        let segments: Vec<_> = tail.split('/').collect();
        if let [profile, "continue-watching"] = segments.as_slice() {
            return catalog::valid_id(profile) && method == "GET";
        }
        if let [profile, "timelines", timeline, "viewing"] = segments.as_slice() {
            return catalog::valid_id(profile)
                && catalog::valid_id(timeline)
                && matches!(method, "GET" | "HEAD" | "PUT");
        }
    }
    if let Some(id) = path
        .strip_prefix("/profiles/")
        .and_then(|p| p.strip_suffix("/preferences"))
    {
        return catalog::valid_id(id) && matches!(method, "GET" | "HEAD" | "PUT");
    }
    path.strip_prefix("/catalog/items/")
        .is_some_and(|id| catalog::valid_id(id))
        && matches!(method, "GET" | "HEAD" | "PUT")
}

pub fn router(app: App) -> Router<App> {
    Router::new()
        .route("/openapi.json", get(system::openapi))
        .route("/system/health", get(system::health))
        .route("/admin/diagnostics", get(system::diagnostics))
        .route("/admin/metrics", get(system::metrics))
        .route("/system/capabilities", get(system::capabilities))
        .route("/me", get(identity::me))
        .route("/auth/methods", get(signin::methods))
        .route("/auth/oidc/{provider_id}/start", post(signin::oidc_start))
        .route("/auth/oidc/callback", get(signin::oidc_callback))
        .route("/auth/email/start", post(signin::email_start))
        .route("/auth/register/start", post(signin::register_start))
        .route(
            "/auth/email/{transaction_id}/finish",
            post(signin::email_finish),
        )
        .route(
            "/auth/transactions/{transaction_id}",
            get(signin::transaction),
        )
        .route("/auth/me", get(signin::me))
        .route("/auth/identities", get(signin::my_identities))
        .route("/auth/sessions", get(signin::my_sessions))
        .route(
            "/auth/sessions/{id}",
            axum::routing::delete(signin::revoke_session),
        )
        .route(
            "/auth/identities/{id}",
            axum::routing::delete(signin::unlink_identity),
        )
        .route("/auth/proofs", post(signin::create_proof))
        .route("/auth/proofs/{id}", get(signin::get_proof))
        .route("/auth/proofs/{id}/approve", post(signin::approve_proof))
        .route("/admin/auth/overview", get(signin::overview))
        .route(
            "/admin/auth/settings",
            axum::routing::put(signin::replace_settings),
        )
        .route(
            "/admin/auth/restore-review",
            get(signin::overview).post(signin::review_restore),
        )
        .route(
            "/admin/auth/providers",
            get(signin::list_providers).post(signin::create_provider),
        )
        .route(
            "/admin/auth/providers/{id}",
            axum::routing::put(signin::replace_provider),
        )
        .route(
            "/admin/auth/providers/{id}/test",
            post(signin::test_provider),
        )
        .route(
            "/admin/auth/email",
            get(signin::get_email).put(signin::replace_email),
        )
        .route("/admin/auth/email/test", post(signin::test_email))
        .route(
            "/admin/auth/email/tests/{id}/confirm",
            post(signin::confirm_email_test),
        )
        .route(
            "/admin/auth/email/diagnostics",
            post(signin::email_diagnostics),
        )
        .route("/admin/auth/email/activity", get(signin::email_activity))
        .route(
            "/admin/auth/registration",
            get(signin::get_registration).put(signin::replace_registration),
        )
        .route("/admin/auth/requests", get(signin::list_requests))
        .route(
            "/admin/auth/requests/{id}/approve",
            post(signin::approve_request),
        )
        .route("/admin/auth/requests/{id}/deny", post(signin::deny_request))
        .route("/admin/auth/fresh-approvals", get(signin::list_proofs))
        .route("/admin/auth/identities", get(signin::list_identities))
        .route("/admin/auth/sessions", get(signin::list_sessions))
        .route("/admin/auth/users/{id}", get(signin::user_details))
        .route("/admin/auth/users/{id}/revoke", post(signin::revoke_user))
        .route("/admin/auth/audit", get(signin::list_audit))
        .route(
            "/auth/pairings",
            post(identity::create_pairing).get(access_management::list_pairings),
        )
        .route(
            "/auth/pairings/{pairing_id}/deny",
            post(access_management::deny_pairing),
        )
        .route(
            "/users",
            get(access_management::list_users).post(access_management::create_user),
        )
        .route(
            "/access/setup",
            get(access_management::get_setup).post(access_management::setup_owner),
        )
        .route(
            "/users/{user_id}",
            get(access_management::get_user).put(access_management::replace_user),
        )
        .route(
            "/auth/invitations",
            get(access_management::list_invitations).post(access_management::create_invitation),
        )
        .route(
            "/auth/invitations/redeem",
            post(access_management::redeem_invitation),
        )
        .route(
            "/auth/invitations/{invitation_id}",
            axum::routing::delete(access_management::revoke_invitation),
        )
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
            "/devices/{device_id}/user",
            axum::routing::put(access_management::assign_device_user),
        )
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
        .merge(scans::routes())
        .merge(schedules::routes())
        .merge(jobs::routes())
        .merge(admin::routes())
        .merge(monitor::routes())
        .merge(metadata::routes())
        .merge(playback::routes())
        .merge(processing::routes())
        .merge(downloads::routes())
        .merge(previews::routes())
        .merge(markers::routes())
        .fallback(|| async { Problem::not_found() })
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(app, native_catalog_gate))
        .layer(middleware::from_fn(request_scope))
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_gate_is_a_positive_operation_allowlist() {
        for (method, path) in [
            ("GET", "/jobs"),
            ("GET", "/jobs/job-1"),
            ("GET", "/api/v2/jobs/job-1"),
            ("POST", "/jobs/job-1/cancel"),
            ("POST", "/jobs/job-1/retry"),
            ("POST", "/api/v2/jobs/job-1/cancel"),
            ("POST", "/api/v2/jobs/job-1/retry"),
            ("GET", "/playback/viewing-sessions/session-1"),
            ("POST", "/playback/delivery-sessions/delivery-1/heartbeat"),
            ("POST", "/playback/delivery-sessions/delivery-1/changes"),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/generations/2/activate",
            ),
            ("GET", "/playback/delivery-sessions/delivery-1"),
            ("GET", "/schedules"),
            ("POST", "/schedules"),
            ("GET", "/schedules/schedule-1"),
            ("PUT", "/schedules/schedule-1"),
            ("DELETE", "/schedules/schedule-1"),
            ("PUT", "/playback/viewing-sessions/session-1/delivery"),
            ("GET", "/catalog/filters"),
            ("POST", "/catalog/filters"),
            ("GET", "/catalog/filters/filter-1"),
            ("PUT", "/catalog/filters/filter-1"),
            ("DELETE", "/catalog/filters/filter-1"),
            ("GET", "/playlists"),
            ("POST", "/playlists"),
            ("GET", "/playlists/list-1"),
            ("PUT", "/playlists/list-1"),
            ("DELETE", "/playlists/list-1"),
            ("GET", "/collections"),
            ("POST", "/collections"),
            ("GET", "/collections/collection-1"),
            ("PUT", "/collections/collection-1"),
            ("DELETE", "/collections/collection-1"),
            ("POST", "/playback/plans"),
            ("POST", "/processing/preparations"),
            ("GET", "/processing/renditions"),
            ("GET", "/catalog/items"),
            ("GET", "/catalog/items/work-1/artwork"),
            ("POST", "/catalog/items/work-1/artwork"),
            ("GET", "/media/assets/asset-1/content"),
            ("HEAD", "/media/assets/asset-1/content"),
            ("GET", "/catalog/items/work-1/artwork/selection"),
            ("PUT", "/catalog/items/work-1/artwork/selection"),
            ("GET", "/catalog/items/work-1/relationships"),
            ("POST", "/catalog/items/work-1/relationships"),
            ("GET", "/catalog/relationships/edge-1"),
            ("DELETE", "/catalog/relationships/edge-1"),
            ("POST", "/api/v2/catalog/items"),
            ("GET", "/catalog/items/work-1"),
            ("GET", "/catalog/search"),
            ("GET", "/catalog/timelines/timeline-1/previews"),
            ("POST", "/api/v2/catalog/timelines/timeline-1/previews"),
            ("GET", "/media/files/file-1/previews/1"),
            ("GET", "/api/v2/media/files/file-1/previews/4294967295"),
            ("GET", "/catalog/timelines/timeline-1/markers"),
            ("POST", "/api/v2/catalog/timelines/timeline-1/markers"),
            ("GET", "/profiles/profile-1/continue-watching"),
            ("PUT", "/profiles/profile-1/preferences"),
            ("HEAD", "/profiles/profile-1/preferences"),
            ("GET", "/profiles/profile-1/timelines/timeline-1/viewing"),
            (
                "HEAD",
                "/api/v2/profiles/profile-1/timelines/timeline-1/viewing",
            ),
            ("PUT", "/profiles/profile-1/timelines/timeline-1/viewing"),
            ("GET", "/events"),
            ("GET", "/api/v2/events"),
            ("POST", "/downloads"),
            ("GET", "/api/v2/downloads"),
            ("POST", "/api/v2/offline/sync"),
            ("GET", "/api/v2/downloads/download-1"),
            ("DELETE", "/downloads/download-1"),
            ("PUT", "/downloads/download-1/transfer"),
            ("GET", "/api/v2/downloads/download-1/manifest"),
            ("GET", "/media/files/file-1/content"),
            ("DELETE", "/sources/source-1"),
            ("POST", "/libraries"),
            ("HEAD", "/libraries/library-1"),
            ("PUT", "/libraries/library-1"),
            ("POST", "/libraries/library-1/scans"),
            ("GET", "/scans"),
            ("GET", "/devices"),
            ("DELETE", "/devices/device-1"),
            ("GET", "/devices/device-1/policy"),
            ("GET", "/users"),
            ("POST", "/users"),
            ("GET", "/users/user-1"),
            ("PUT", "/users/user-1"),
            ("GET", "/access/setup"),
            ("POST", "/access/setup"),
            ("PUT", "/devices/device-1/user"),
            ("GET", "/admin/metrics"),
            ("POST", "/playback/delivery-sessions"),
            ("DELETE", "/playback/delivery-sessions/delivery-1"),
            ("POST", "/playback/viewing-sessions"),
            ("POST", "/playback/viewing-sessions/session-1/events"),
            ("POST", "/content-access"),
            ("GET", "/streams/delivery-1/generation-1/content"),
            ("GET", "/profiles"),
            ("POST", "/profiles"),
            ("GET", "/profiles/profile-1"),
            ("PUT", "/profiles/profile-1"),
            ("DELETE", "/profiles/profile-1"),
            ("GET", "/devices/device-1"),
            ("PUT", "/devices/device-1/policy"),
            ("GET", "/api/v2/scans/scan-1"),
            ("POST", "/scans/scan-1/cancel"),
        ] {
            assert!(super::native_route_supported(method, path));
        }
        for (method, path) in [
            ("HEAD", "/events"),
            ("POST", "/events"),
            ("PUT", "/events"),
            ("DELETE", "/events"),
            ("GET", "/events/replay"),
            ("GET", "/events/"),
            ("HEAD", "/schedules"),
            ("HEAD", "/schedules/schedule-1"),
            ("PATCH", "/schedules/schedule-1"),
            ("PUT", "/schedules"),
            ("POST", "/schedules/schedule-1"),
            ("GET", "/schedules/schedule-1/events"),
            ("GET", "/watch"),
            ("HEAD", "/playback/delivery-sessions/delivery-1"),
            ("GET", "/playback/delivery-sessions/delivery-1/heartbeat"),
            ("HEAD", "/playback/viewing-sessions/session-1"),
            ("GET", "/playback/viewing-sessions/session-1/delivery"),
            ("HEAD", "/collections"),
            ("HEAD", "/collections/collection-1"),
            ("PUT", "/collections"),
            ("GET", "/collections/collection-1/members"),
            ("HEAD", "/catalog/filters"),
            ("HEAD", "/catalog/filters/filter-1"),
            ("POST", "/catalog/filters/filter-1"),
            ("GET", "/catalog/filters/filter-1/evaluate"),
            ("HEAD", "/playlists"),
            ("HEAD", "/playlists/list-1"),
            ("POST", "/playlists/list-1/play"),
            ("GET", "/playback/queues"),
            ("DELETE", "/catalog/items/work-1"),
            ("POST", "/catalog/items/work-1/editions"),
            ("HEAD", "/catalog/items/work-1/artwork"),
            ("POST", "/media/assets/asset-1/content"),
            ("GET", "/media/assets/asset-1/other/content"),
            ("DELETE", "/catalog/items/work-1/artwork/selection"),
            ("GET", "/catalog/items/work-1/artwork/selection/extra"),
            ("GET", "/playback/delivery-sessions"),
            ("HEAD", "/playback/delivery-sessions/delivery-1/changes"),
            ("GET", "/playback/delivery-sessions/delivery-1/changes"),
            ("PUT", "/playback/delivery-sessions/delivery-1/changes"),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/changes/extra",
            ),
            (
                "HEAD",
                "/playback/delivery-sessions/delivery-1/generations/2/activate",
            ),
            (
                "GET",
                "/playback/delivery-sessions/delivery-1/generations/2/activate",
            ),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/generations/0/activate",
            ),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/generations/+2/activate",
            ),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/generations/18446744073709551616/activate",
            ),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/generations/2/activate/extra",
            ),
            (
                "POST",
                "/playback/delivery-sessions/delivery-1/generations/generation-1/activate",
            ),
            ("GET", "/playback/viewing-sessions/session-1/events"),
            ("POST", "/playback/viewing-sessions/session-1/delivery"),
            ("GET", "/content-access"),
            ("DELETE", "/content-access/access-1"),
            ("HEAD", "/streams/delivery-1/generation-1/content"),
            ("GET", "/streams/delivery-1/generation-1/subtitles.vtt"),
            ("GET", "/streams/delivery-1/generation-1/master.m3u8"),
            (
                "GET",
                "/streams/delivery-1/generation-1/variants/variant-1/index.m3u8",
            ),
            ("GET", "/streams/delivery-1/generation-1/segments/segment-1"),
            ("GET", "/streams/delivery-1/generation-1/extra/content"),
            ("GET", "/playback/plans"),
            ("HEAD", "/processing/renditions"),
            ("GET", "/processing/preparations"),
            ("POST", "/processing/renditions"),
            ("HEAD", "/jobs"),
            ("POST", "/jobs"),
            ("HEAD", "/jobs/job-1"),
            ("PUT", "/jobs/job-1"),
            ("DELETE", "/jobs/job-1"),
            ("GET", "/jobs/job-1/cancel"),
            ("HEAD", "/jobs/job-1/retry"),
            ("PUT", "/jobs/job-1/cancel"),
            ("DELETE", "/jobs/job-1/retry"),
            ("POST", "/jobs//cancel"),
            ("POST", "/jobs/job-1/extra/cancel"),
            ("POST", "/jobs/job-1/retry/extra"),
            ("POST", "/jobs/job-1/cancel/"),
            ("POST", "/jobs/job%2F1/cancel"),
            ("GET", "/jobs/job-1/extra"),
            ("HEAD", "/catalog/timelines/timeline-1/previews"),
            ("PUT", "/catalog/timelines/timeline-1/previews"),
            ("DELETE", "/catalog/timelines/timeline-1/previews"),
            ("GET", "/catalog/timelines/timeline-1/previews/1"),
            ("HEAD", "/media/files/file-1/previews/1"),
            ("POST", "/media/files/file-1/previews/1"),
            ("PUT", "/media/files/file-1/previews/1"),
            ("DELETE", "/media/files/file-1/previews/1"),
            ("GET", "/media/files/file-1/previews/0"),
            ("GET", "/media/files/file-1/previews/4294967296"),
            ("GET", "/media/files/file-1/previews/+1"),
            ("GET", "/media/files/file-1/previews/-1"),
            ("GET", "/media/files/file-1/previews/1/extra"),
            ("GET", "/media/files/file-1/previews"),
            ("GET", "/media/files/motion.file:one/previews/1"),
            ("HEAD", "/catalog/timelines/timeline-1/markers"),
            ("PUT", "/catalog/timelines/timeline-1/markers"),
            ("DELETE", "/catalog/timelines/timeline-1/markers"),
            ("PATCH", "/catalog/timelines/timeline-1/markers"),
            ("GET", "/catalog/timelines/timeline-1/markers/marker-1"),
            ("POST", "/catalog/timelines/timeline-1/other/markers"),
            ("GET", "/catalog/timelines/motion.timeline:one/markers"),
            ("HEAD", "/catalog/items/work-1/relationships"),
            ("HEAD", "/catalog/relationships/edge-1"),
            ("PUT", "/catalog/relationships/edge-1"),
            ("POST", "/catalog/items/work-1/other/relationships"),
            ("HEAD", "/profiles/profile-1/continue-watching"),
            ("POST", "/profiles/profile-1/continue-watching"),
            ("GET", "/profiles/profile-1/continue-watching/next"),
            ("GET", "/profiles/profile-1/next"),
            ("DELETE", "/profiles/profile-1/preferences"),
            ("GET", "/profiles/profile-1/other/preferences"),
            ("GET", "/catalog/items/work-1/files"),
            ("POST", "/playback/sessions"),
            ("DELETE", "/profiles/profile-1/timelines/timeline-1/viewing"),
            ("POST", "/profiles/profile-1/timelines/timeline-1/viewing"),
            (
                "GET",
                "/profiles/profile-1/timelines/timeline-1/viewing/events",
            ),
            (
                "GET",
                "/profiles/profile-1/timelines/motion.timeline:one/viewing",
            ),
            ("GET", "/offline/sync"),
            ("HEAD", "/offline/sync"),
            ("PUT", "/offline/sync"),
            ("DELETE", "/offline/sync"),
            ("POST", "/offline/sync/extra"),
            ("POST", "/offline/other/sync"),
            ("POST", "/offline"),
            ("HEAD", "/downloads"),
            ("HEAD", "/downloads/download-1"),
            ("PUT", "/downloads"),
            ("PUT", "/downloads/download-1"),
            ("GET", "/downloads/download-1/transfer"),
            ("POST", "/downloads/download-1/transfer"),
            ("PUT", "/downloads/download-1/transfer/extra"),
            ("POST", "/downloads/download-1/cancel"),
            ("HEAD", "/downloads/download-1/manifest"),
            ("POST", "/downloads/download-1/manifest"),
            ("PUT", "/downloads/download-1/manifest"),
            ("DELETE", "/downloads/download-1/manifest"),
            ("GET", "/downloads/download-1/manifest/extra"),
            ("GET", "/downloads/download-1/other/manifest"),
            ("GET", "/downloads/download-1/subtitles"),
            ("POST", "/downloads/download-1/sync"),
            ("HEAD", "/media/files/file-1/content"),
            ("GET", "/media/files/file-1/subtitles"),
            ("GET", "/media/files/file-1/other/content"),
            ("POST", "/admin/scans"),
            ("GET", "/admin/files"),
            ("GET", "/catalog/items/motion.edition:one"),
            ("PUT", "/sources/source-1"),
            ("PATCH", "/sources/source-1"),
            ("DELETE", "/sources"),
            ("DELETE", "/sources/source-1/settings"),
            ("POST", "/sources/source-1/reactivate"),
            ("DELETE", "/libraries/library-1"),
            ("PATCH", "/libraries/library-1"),
            ("PUT", "/libraries"),
            ("PUT", "/libraries/library-1/other"),
            ("PUT", "/libraries/library-1/scans"),
            ("HEAD", "/scans"),
            ("HEAD", "/devices/device-1"),
            ("HEAD", "/devices"),
            ("HEAD", "/users"),
            ("DELETE", "/users/user-1"),
            ("GET", "/users/user-1/devices"),
            ("HEAD", "/access/setup"),
            ("DELETE", "/access/setup"),
            ("GET", "/devices/device-1/user"),
            ("DELETE", "/devices/device-1/user"),
            ("HEAD", "/admin/metrics"),
            ("HEAD", "/admin/auth/users/user-1"),
            ("HEAD", "/admin/diagnostics"),
            ("HEAD", "/profiles"),
            ("HEAD", "/profiles/profile-1"),
            ("PUT", "/devices/device-1/other/policy"),
            ("HEAD", "/scans/scan-1"),
            ("DELETE", "/scans/scan-1"),
            ("GET", "/scans/scan-1/cancel"),
            ("POST", "/scans/scan-1/other/cancel"),
            ("GET", "/libraries/library-1/scans"),
            ("POST", "/libraries/library-1/other/scans"),
        ] {
            assert!(!super::native_route_supported(method, path));
        }
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(super::timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::timestamp(1_791_590_399), "2026-10-09T23:59:59Z");
    }
}
