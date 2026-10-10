//! Identity/system operations owned by A08: principal, pairing, browser
//! sessions, access tokens, devices, device policy and profiles.
use super::{
    Body, Page, PageQuery, Problem, Scope, Stored,
    auth::{self, Caller, DeviceRow, Purpose},
    etag, idempotency_key, if_match, page, timestamp,
};
use crate::{App, db::begin_write, new_id, now};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use playscale_core::access::{
    self as core, AccessError, Claim, CredentialKind, Device, Grant, Idempotent, Mode,
    PAIRING_POLL_SECONDS, PAIRING_TTL_SECONDS, Pairing, PairingPhase, Permission, Policy,
    Principal, Revised, SESSION_TTL_SECONDS,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

fn bounded(field: &str, value: &str, min: usize, max: usize) -> Result<(), Problem> {
    let n = value.chars().count();
    if n < min || n > max || value.trim() != value || value.chars().any(char::is_control) {
        return Err(Problem::invalid(
            "invalid_field",
            format!("{field} must be {min}-{max} characters without surrounding spaces"),
        ));
    }
    Ok(())
}

fn valid_ids(field: &str, ids: &[String]) -> Result<(), Problem> {
    let ok = ids.len() <= 200
        && ids.iter().all(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
    if ok {
        Ok(())
    } else {
        Err(Problem::invalid(
            "invalid_field",
            format!("{field} must contain at most 200 valid IDs"),
        ))
    }
}

/// A JSON response with a strong ETag for the returned revision.
fn tagged<T: Serialize>(status: StatusCode, body: &T, revision: u64) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, etag(revision));
    response
}

/// Replays a stored acknowledgement without re-executing the operation.
fn replayed(stored: Stored, body: serde_json::Value) -> Response {
    let revision = body
        .get("revision")
        .and_then(|r| r.as_str())
        .and_then(|r| r.parse().ok());
    let mut response = (stored.status, Json(body)).into_response();
    if let Some(revision) = revision {
        response.headers_mut().insert(header::ETAG, etag(revision));
    }
    response
        .headers_mut()
        .insert("idempotent-replayed", HeaderValue::from_static("true"));
    response
}

async fn existing_profiles(
    conn: &mut sqlx::SqliteConnection,
    ids: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Problem> {
    let ids = serde_json::to_string(ids).map_err(Problem::internal)?;
    Ok(sqlx::query_scalar(
        "SELECT id FROM profiles WHERE id IN (SELECT value FROM json_each(?)) ORDER BY id",
    )
    .bind(ids)
    .fetch_all(conn)
    .await?
    .into_iter()
    .collect())
}

#[derive(Serialize)]
pub struct PrincipalBody {
    id: String,
    device_id: Option<String>,
    profile_ids: Vec<String>,
    permissions: Vec<Permission>,
    policy_revision: String,
    mode: Mode,
}

async fn principal_body(
    conn: &mut sqlx::SqliteConnection,
    p: &Principal,
) -> Result<PrincipalBody, Problem> {
    let profile_ids = if p.is_admin() {
        sqlx::query_scalar("SELECT id FROM profiles ORDER BY id LIMIT 200")
            .fetch_all(&mut *conn)
            .await?
    } else {
        existing_profiles(conn, &p.grant.profile_ids)
            .await?
            .into_iter()
            .collect()
    };
    let permissions = if p.is_admin() {
        Permission::ALL.to_vec()
    } else {
        p.grant.permissions.iter().copied().collect()
    };
    Ok(PrincipalBody {
        id: p.id.clone(),
        device_id: p.device_id.clone(),
        profile_ids,
        permissions,
        policy_revision: p.policy_revision.to_string(),
        mode: p.mode,
    })
}

pub async fn me(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    let mut conn = app.db.acquire().await?;
    let body = principal_body(&mut conn, &caller.principal).await?;
    Ok(tagged(
        StatusCode::OK,
        &body,
        caller.principal.policy_revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingRequest {
    device_name: String,
    client_name: String,
}

#[derive(Serialize)]
struct PairingBody {
    id: String,
    device_code: String,
    user_code: String,
    expires_at: String,
    poll_interval_seconds: i64,
}

/// Eight characters from an alphabet without 0/O/1/I, displayed as XXXX-XXXX.
fn user_code() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let chars: String = bytes[..8]
        .iter()
        .map(|b| ALPHABET[usize::from(b & 31)] as char)
        .collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

pub async fn create_pairing(
    State(app): State<App>,
    body: Body<PairingRequest>,
) -> Result<Response, Problem> {
    let request = body.value;
    bounded("device_name", &request.device_name, 1, 100)?;
    bounded("client_name", &request.client_name, 1, 100)?;
    app.access.admit_pairing_request()?;
    let now = now();
    let mut tx = begin_write(&app.db).await?;
    sqlx::query("DELETE FROM pairings WHERE expires_at<=?")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pairings WHERE phase='pending'")
        .fetch_one(&mut *tx)
        .await?;
    if !core::admit_pairing(pending as usize) {
        return Err(Problem::rate_limited(60));
    }
    let id = new_id();
    let device_code = auth::derive_secret(&app.access.key, Purpose::PairingCode, &auth::nonce());
    let user_code = user_code();
    let expires_at = now + PAIRING_TTL_SECONDS;
    sqlx::query("INSERT INTO pairings(id,device_code_hash,user_code,device_name,client_name,expires_at,phase) VALUES (?,?,?,?,?,?,'pending')")
        .bind(&id)
        .bind(auth::token_hash(&device_code))
        .bind(&user_code)
        .bind(&request.device_name)
        .bind(&request.client_name)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(PairingBody {
            id,
            device_code,
            user_code,
            expires_at: timestamp(expires_at),
            poll_interval_seconds: PAIRING_POLL_SECONDS,
        }),
    )
        .into_response())
}

type PairingRow = (
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    String,
    String,
    Option<i64>,
);

async fn load_pairing(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<PairingRow>, Problem> {
    Ok(sqlx::query_as(
        "SELECT device_code_hash,user_code,expires_at,phase,device_id,last_claim_at,credential_nonce,device_name,client_name,claimed_generation FROM pairings WHERE id=?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

fn pairing(id: &str, row: &PairingRow) -> Pairing {
    let device_id = row.4.clone().unwrap_or_default();
    Pairing {
        id: id.into(),
        user_code: row.1.clone(),
        expires_at: row.2,
        phase: match row.3.as_str() {
            "pending" => PairingPhase::Pending,
            "approved" => PairingPhase::Approved { device_id },
            _ => PairingPhase::Claimed {
                device_id,
                generation: row.9.unwrap_or_default() as u64,
            },
        },
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingApproval {
    user_code: String,
    profile_ids: Vec<String>,
    permissions: Vec<Permission>,
}

#[derive(Serialize)]
pub struct DeviceBody {
    id: String,
    name: String,
    revision: String,
    profile_ids: Vec<String>,
    permissions: Vec<Permission>,
    revoked: bool,
}

fn device_body(row: &DeviceRow) -> DeviceBody {
    let d = &row.device;
    DeviceBody {
        id: d.id.clone(),
        name: row.name.clone(),
        revision: d.revision.to_string(),
        profile_ids: d.grant.profile_ids.iter().cloned().collect(),
        permissions: d.grant.permissions.iter().copied().collect(),
        revoked: d.revoked,
    }
}

pub async fn approve_pairing(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<PairingApproval>,
) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let key = idempotency_key(&headers)?;
    let (approval, digest) = (body.value, body.digest);
    if approval.user_code.len() > 32 || approval.permissions.len() > 32 {
        return Err(Problem::invalid("invalid_field", "Approval is too large"));
    }
    valid_ids("profile_ids", &approval.profile_ids)?;
    let now = now();
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "approvePairing",
        target: &id,
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &digest, now)? == Idempotent::Replay {
        let stored = stored.unwrap();
        let body = stored.body.clone().unwrap_or_default();
        return Ok(replayed(stored, body));
    }
    let row = load_pairing(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let grant = Grant {
        profile_ids: approval.profile_ids.into_iter().collect(),
        permissions: approval.permissions.into_iter().collect(),
    };
    let known = existing_profiles(&mut tx, &grant.profile_ids).await?;
    core::approve(
        &pairing(&id, &row),
        now,
        &principal,
        &approval.user_code,
        &grant,
        &known,
    )?;
    let device = DeviceRow {
        device: Device::approved(new_id(), grant),
        name: row.7.clone(),
        client_name: row.8.clone(),
    };
    auth::insert_device(&mut tx, &device).await?;
    let updated = sqlx::query(
        "UPDATE pairings SET phase='approved',device_id=? WHERE id=? AND phase='pending'",
    )
    .bind(&device.device.id)
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AccessError::PairingDecided.into());
    }
    let body = serde_json::to_value(device_body(&device)).map_err(Problem::internal)?;
    scope
        .save(&mut tx, &digest, StatusCode::OK, Some(&body), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, device.device.revision))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingClaim {
    device_code: String,
}

#[derive(Serialize)]
struct CredentialBody {
    device_id: String,
    access_token: String,
    expires_at: String,
}

pub async fn claim_pairing(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Body<PairingClaim>,
) -> Result<Response, Problem> {
    let now = now();
    let mut tx = begin_write(&app.db).await?;
    // An unknown pairing and a wrong device code are indistinguishable.
    let row = load_pairing(&mut tx, &id)
        .await?
        .filter(|r| r.0 == auth::token_hash(&body.value.device_code))
        .ok_or_else(Problem::not_found)?;
    if let Err(wait) = core::claim_poll(row.5, now) {
        return Err(Problem {
            code: "slow_down",
            ..Problem::rate_limited(wait as u64)
        });
    }
    sqlx::query("UPDATE pairings SET last_claim_at=? WHERE id=?")
        .bind(now)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let device = match &row.4 {
        Some(device_id) => auth::load_device(&mut tx, device_id).await?,
        None => None,
    };
    let issued = match &row.6 {
        Some(nonce) => {
            let token = auth::derive_secret(&app.access.key, Purpose::Device, nonce);
            auth::load_credential(&mut tx, &auth::token_hash(&token))
                .await?
                .map(|s| s.credential)
        }
        None => None,
    };
    let decision = core::claim(
        &pairing(&id, &row),
        device.as_ref().map(|d| &d.device),
        issued.as_ref(),
        now,
    );
    let (issue, nonce) = match decision {
        Err(e) => {
            // Persist the poll time so the claim rate limit holds.
            tx.commit().await?;
            return Err(e.into());
        }
        Ok(Claim::Replay(issue)) => (issue, row.6.clone().unwrap()),
        Ok(Claim::Issue(issue)) => {
            let previous = &device.as_ref().unwrap().device;
            let next = core::apply_issue(previous, &issue)?;
            let nonce = auth::nonce();
            let token = auth::derive_secret(&app.access.key, Purpose::Device, &nonce);
            // Earlier generations are superseded; children cascade with them.
            sqlx::query("DELETE FROM credentials WHERE device_id=?")
                .bind(&issue.device_id)
                .execute(&mut *tx)
                .await?;
            auth::update_device(&mut tx, previous, &next).await?;
            auth::insert_credential(
                &mut tx,
                &auth::token_hash(&token),
                &core::Credential {
                    device_id: issue.device_id.clone(),
                    kind: CredentialKind::Device,
                    generation: issue.generation,
                    expires_at: issue.expires_at,
                },
                None,
                None,
            )
            .await?;
            sqlx::query("UPDATE pairings SET phase='claimed',credential_nonce=?,claimed_generation=? WHERE id=?")
                .bind(&nonce)
                .bind(issue.generation as i64)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            (issue, nonce)
        }
    };
    tx.commit().await?;
    Ok(Json(CredentialBody {
        access_token: auth::derive_secret(&app.access.key, Purpose::Device, &nonce),
        device_id: issue.device_id,
        expires_at: timestamp(issue.expires_at),
    })
    .into_response())
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionExchange {
    Credential { credential: String },
    // A struct variant, so unknown fields are rejected like everywhere else.
    TrustedPrivate {},
}

#[derive(Serialize)]
struct SessionBody {
    principal: PrincipalBody,
    csrf_token: String,
    expires_at: String,
}

fn session_cookie(app: &App, token: &str, max_age: i64) -> HeaderValue {
    let secure = if app.origin.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{}={token}; Path=/api/v2; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}",
        auth::SESSION_COOKIE
    ))
    .expect("cookie characters are ASCII")
}

pub async fn create_session(
    State(app): State<App>,
    ingress: Option<axum::Extension<auth::TrustedIngress>>,
    headers: HeaderMap,
    body: Body<SessionExchange>,
) -> Result<Response, Problem> {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if origin != Some(app.origin.as_str()) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "Browser sessions require the exact server origin",
        ));
    }
    let credential = match body.value {
        SessionExchange::TrustedPrivate {} => {
            // Identity stays ambient on the ingress listener; the response
            // carries the principal and the CSRF token unsafe requests need.
            // It never creates an administrator.
            let Some(axum::Extension(marker)) = ingress else {
                return Err(Problem::new(
                    StatusCode::FORBIDDEN,
                    "trusted_private_unavailable",
                    "Verified private ingress is not available on this connection",
                ));
            };
            let caller =
                auth::resolve_with(&app, &axum::http::Method::GET, &headers, Some(marker)).await?;
            if caller.ingress.is_none() {
                return Err(AccessError::Unauthenticated.into());
            }
            let mut conn = app.db.acquire().await?;
            return Ok(Json(SessionBody {
                principal: principal_body(&mut conn, &caller.principal).await?,
                csrf_token: caller.csrf.unwrap_or_default(),
                expires_at: timestamp(now() + SESSION_TTL_SECONDS),
            })
            .into_response());
        }
        SessionExchange::Credential { credential } => credential,
    };
    if auth::is_operator(&app, &credential) {
        return Err(AccessError::ParentCredentialRequired.into());
    }
    let now = now();
    let mut tx = begin_write(&app.db).await?;
    let credential = if app.access.take_bootstrap(&credential) {
        bootstrap_device(&app, &mut tx, now).await?
    } else {
        credential
    };
    let parent_hash = auth::token_hash(&credential);
    let stored = auth::load_credential(&mut tx, &parent_hash)
        .await?
        .filter(|s| s.credential.kind == CredentialKind::Device)
        .ok_or(AccessError::Unauthenticated)?;
    let (principal, device) = auth::authenticate_stored(&mut tx, &stored, now).await?;
    let child = core::derive(
        &stored.credential,
        &device.unwrap().device,
        CredentialKind::Session,
        SESSION_TTL_SECONDS,
        now,
    )?;
    let token = auth::derive_secret(&app.access.key, Purpose::Session, &auth::nonce());
    let csrf = format!("csrf_{}", auth::nonce());
    auth::insert_credential(
        &mut tx,
        &auth::token_hash(&token),
        &child,
        Some(&parent_hash),
        Some(&csrf),
    )
    .await?;
    let principal = principal_body(&mut tx, &principal).await?;
    tx.commit().await?;
    let mut response = Json(SessionBody {
        principal,
        csrf_token: csrf,
        expires_at: timestamp(child.expires_at),
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&app, &token, child.expires_at - now),
    );
    Ok(response)
}

/// The desktop that launched this server proved it with the one-use
/// bootstrap secret. It acts as the local desktop device (created on first
/// use with every permission, as the server's owner); each bootstrap issues a
/// new device credential generation, superseding the previous desktop run.
/// An administrator who revokes that device disables bootstrap until it is
/// deleted, and the result is never a credential the caller sees.
async fn bootstrap_device(
    app: &App,
    tx: &mut sqlx::SqliteConnection,
    now: i64,
) -> Result<String, Problem> {
    let existing = auth::load_device(tx, super::DESKTOP_DEVICE).await?;
    let device = match existing {
        Some(row) => row,
        None => {
            let row = DeviceRow {
                device: Device::approved(
                    super::DESKTOP_DEVICE.into(),
                    Grant {
                        profile_ids: BTreeSet::new(),
                        permissions: Permission::ALL.into_iter().collect(),
                    },
                ),
                name: "Desktop".into(),
                client_name: "motion-desktop".into(),
            };
            auth::insert_device(tx, &row).await?;
            row
        }
    };
    if device.device.revoked {
        return Err(AccessError::DeviceRevoked.into());
    }
    let issue = core::Issue {
        device_id: device.device.id.clone(),
        generation: device.device.generation + 1,
        expires_at: now + core::DEVICE_CREDENTIAL_TTL_SECONDS,
    };
    let next = core::apply_issue(&device.device, &issue)?;
    sqlx::query("DELETE FROM credentials WHERE device_id=?")
        .bind(&issue.device_id)
        .execute(&mut *tx)
        .await?;
    auth::update_device(tx, &device.device, &next).await?;
    let token = auth::derive_secret(&app.access.key, Purpose::Device, &auth::nonce());
    auth::insert_credential(
        tx,
        &auth::token_hash(&token),
        &core::Credential {
            device_id: issue.device_id,
            kind: CredentialKind::Device,
            generation: issue.generation,
            expires_at: issue.expires_at,
        },
        None,
        None,
    )
    .await?;
    Ok(token)
}

fn cookie_session(caller: &Caller) -> Result<(&str, &core::Credential, &str), Problem> {
    match (&caller.credential, &caller.csrf) {
        (Some((hash, credential)), Some(csrf)) => Ok((hash, credential, csrf)),
        _ => Err(Problem::not_found()),
    }
}

pub async fn get_session(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    let (_, credential, csrf) = cookie_session(&caller)?;
    let mut conn = app.db.acquire().await?;
    Ok(Json(SessionBody {
        principal: principal_body(&mut conn, &caller.principal).await?,
        csrf_token: csrf.into(),
        expires_at: timestamp(credential.expires_at),
    })
    .into_response())
}

pub async fn delete_session(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    let (hash, _, _) = cookie_session(&caller)?;
    sqlx::query("DELETE FROM credentials WHERE token_hash=?")
        .bind(hash)
        .execute(&app.db)
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&app, "", 0));
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessTokenInput {
    ttl_seconds: i64,
}

pub async fn issue_access_token(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<AccessTokenInput>,
) -> Result<Response, Problem> {
    let Some((parent_hash, parent)) = caller
        .credential
        .clone()
        .filter(|(_, c)| c.kind == CredentialKind::Device && caller.csrf.is_none())
    else {
        return Err(AccessError::ParentCredentialRequired.into());
    };
    let key = idempotency_key(&headers)?;
    let now = now();
    let mut tx = begin_write(&app.db).await?;
    caller.reauthorize(&mut tx, None).await?;
    let scope = Scope {
        principal: &caller.principal.id,
        operation: "issueApiAccessToken",
        target: "-",
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    let decision = core::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now)?;
    if decision == Idempotent::Replay {
        let stored = stored.unwrap();
        let nonce = stored.issued_nonce.clone().unwrap_or_default();
        let token = auth::derive_secret(&app.access.key, Purpose::Access, &nonce);
        let row = auth::load_credential(&mut tx, &auth::token_hash(&token))
            .await?
            .ok_or(AccessError::ReplayUnavailable)?;
        auth::authenticate_stored(&mut tx, &row, now)
            .await
            .map_err(|_| Problem::from(AccessError::ReplayUnavailable))?;
        let mut body = stored.body.clone().unwrap_or_default();
        body["access_token"] = token.into();
        return Ok(replayed(stored, body));
    }
    let device = auth::load_device(&mut tx, &parent.device_id)
        .await?
        .ok_or(AccessError::CredentialRevoked)?;
    let child = core::derive(
        &parent,
        &device.device,
        CredentialKind::Access,
        body.value.ttl_seconds,
        now,
    )?;
    let nonce = auth::nonce();
    let token = auth::derive_secret(&app.access.key, Purpose::Access, &nonce);
    auth::insert_credential(
        &mut tx,
        &auth::token_hash(&token),
        &child,
        Some(&parent_hash),
        None,
    )
    .await?;
    // The stored acknowledgement omits the secret; replay re-derives it.
    let mut ack = serde_json::json!({
        "device_id": child.device_id,
        "expires_at": timestamp(child.expires_at),
    });
    scope
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&ack),
            Some(&nonce),
        )
        .await?;
    tx.commit().await?;
    ack["access_token"] = token.into();
    Ok((StatusCode::CREATED, Json(ack)).into_response())
}

pub async fn list_devices(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<DeviceBody>>, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let rows = auth::list_devices(&mut tx, q.after()?, limit).await?;
    let rows = rows
        .iter()
        .map(|r| (r.device.id.clone(), device_body(r)))
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn admin_device(app: &App, caller: &Caller, id: &str) -> Result<DeviceRow, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let mut conn = app.db.acquire().await?;
    auth::load_device(&mut conn, id)
        .await?
        .ok_or_else(Problem::not_found)
}

pub async fn get_device(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let row = admin_device(&app, &caller, &id).await?;
    Ok(tagged(
        StatusCode::OK,
        &device_body(&row),
        row.device.revision,
    ))
}

/// Revocation deletes every credential of the device in the same transaction,
/// so the next request, session or stream recheck fails.
pub async fn revoke_device(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = auth::load_device(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    if let Some(next) = core::revoke(&row.device, expected)? {
        auth::update_device(&mut tx, &row.device, &next).await?;
        sqlx::query("DELETE FROM credentials WHERE device_id=?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Serialize)]
pub struct PolicyBody {
    revision: String,
    library_ids: Vec<String>,
    allow_unrated: bool,
    allowed_ratings: Vec<String>,
    blocked_labels: Vec<String>,
    permissions: Vec<Permission>,
}

fn policy_body(d: &Device) -> PolicyBody {
    PolicyBody {
        revision: d.revision.to_string(),
        library_ids: d.policy.library_ids.iter().cloned().collect(),
        allow_unrated: d.policy.allow_unrated,
        allowed_ratings: d.policy.allowed_ratings.iter().cloned().collect(),
        blocked_labels: d.policy.blocked_labels.iter().cloned().collect(),
        permissions: d.grant.permissions.iter().copied().collect(),
    }
}

pub async fn get_policy(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let row = admin_device(&app, &caller, &id).await?;
    Ok(tagged(
        StatusCode::OK,
        &policy_body(&row.device),
        row.device.revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicyInput {
    library_ids: Vec<String>,
    allow_unrated: bool,
    allowed_ratings: Vec<String>,
    blocked_labels: Vec<String>,
    permissions: Vec<Permission>,
}

pub async fn replace_policy(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<AccessPolicyInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let expected = if_match(&headers)?;
    let input = body.value;
    valid_ids("library_ids", &input.library_ids)?;
    let text_ok = |v: &String, max: usize| {
        (1..=max).contains(&v.chars().count()) && !v.chars().any(char::is_control)
    };
    let labels_ok = input.allowed_ratings.len() <= 200
        && input.blocked_labels.len() <= 200
        && input.permissions.len() <= 32
        && input.allowed_ratings.iter().all(|r| text_ok(r, 64))
        && input.blocked_labels.iter().all(|l| text_ok(l, 100));
    if !labels_ok {
        return Err(Problem::invalid(
            "invalid_field",
            "Ratings, labels or permissions exceed their limits or contain control characters",
        ));
    }
    let policy = Policy {
        library_ids: input.library_ids.into_iter().collect(),
        allow_unrated: input.allow_unrated,
        allowed_ratings: input.allowed_ratings.into_iter().collect(),
        blocked_labels: input.blocked_labels.into_iter().collect(),
    };
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let ids = serde_json::to_string(&policy.library_ids).map_err(Problem::internal)?;
    let known: BTreeSet<String> =
        sqlx::query_scalar("SELECT id FROM libraries WHERE id IN (SELECT value FROM json_each(?))")
            .bind(ids)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
    let row = auth::load_device(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let permissions = input.permissions.into_iter().collect();
    let device = match core::replace_policy(&row.device, expected, permissions, policy, &known)? {
        Some(next) => {
            auth::update_device(&mut tx, &row.device, &next).await?;
            next
        }
        None => row.device,
    };
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        &policy_body(&device),
        device.revision,
    ))
}

#[derive(Serialize)]
pub struct ProfileBody {
    id: String,
    name: String,
    revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileInput {
    name: String,
}

pub async fn list_profiles(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<ProfileBody>>, Problem> {
    let limit = q.limit()?;
    let granted =
        serde_json::to_string(&caller.principal.grant.profile_ids).map_err(Problem::internal)?;
    let mut tx = app.db.begin().await?;
    // Authorization is applied in the query, before limit and cursor.
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id,name,revision FROM profiles WHERE id>? AND (? OR id IN (SELECT value FROM json_each(?))) ORDER BY id LIMIT ?",
    )
    .bind(q.after()?)
    .bind(caller.principal.is_admin())
    .bind(granted)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let rows = rows
        .into_iter()
        .map(|(id, name, revision)| {
            (
                id.clone(),
                ProfileBody {
                    id,
                    name,
                    revision: revision.to_string(),
                },
            )
        })
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Inaccessible profiles are indistinguishable from missing ones.
async fn load_profile(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    id: &str,
) -> Result<(String, u64), Problem> {
    if !principal.may_use_profile(id) {
        return Err(Problem::not_found());
    }
    let (name, revision): (String, i64) =
        sqlx::query_as("SELECT name,revision FROM profiles WHERE id=?")
            .bind(id)
            .fetch_optional(conn)
            .await?
            .ok_or_else(Problem::not_found)?;
    Ok((name, revision as u64))
}

fn profile(id: String, name: String, revision: u64) -> (ProfileBody, u64) {
    (
        ProfileBody {
            id,
            name,
            revision: revision.to_string(),
        },
        revision,
    )
}

pub async fn get_profile(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let mut conn = app.db.acquire().await?;
    let (name, revision) = load_profile(&mut conn, &caller.principal, &id).await?;
    let (body, revision) = profile(id, name, revision);
    Ok(tagged(StatusCode::OK, &body, revision))
}

pub async fn create_profile(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<ProfileInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ProfilesManage)?;
    let key = idempotency_key(&headers)?;
    bounded("name", &body.value.name, 1, 100)?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::ProfilesManage))
        .await?;
    let scope = Scope {
        principal: &caller.principal.id,
        operation: "createProfile",
        target: "-",
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        let stored = stored.unwrap();
        let body = stored.body.clone().unwrap_or_default();
        return Ok(replayed(stored, body));
    }
    let id = new_id();
    sqlx::query("INSERT INTO profiles(id,name,revision) VALUES (?,?,1)")
        .bind(&id)
        .bind(&body.value.name)
        .execute(&mut *tx)
        .await?;
    let (created, revision) = profile(id, body.value.name, 1);
    let json = serde_json::to_value(&created).map_err(Problem::internal)?;
    scope
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&json),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &created, revision))
}

pub async fn replace_profile(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<ProfileInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ProfilesManage)?;
    let expected = if_match(&headers)?;
    bounded("name", &body.value.name, 1, 100)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProfilesManage))
        .await?;
    let (name, current) = load_profile(&mut tx, &principal, &id).await?;
    let revision = match core::revise(current, expected, name != body.value.name)? {
        Revised::Unchanged => current,
        Revised::Next(next) => {
            sqlx::query("UPDATE profiles SET name=?,revision=? WHERE id=? AND revision=?")
                .bind(&body.value.name)
                .bind(next as i64)
                .bind(&id)
                .bind(current as i64)
                .execute(&mut *tx)
                .await?;
            next
        }
    };
    tx.commit().await?;
    let (body, revision) = profile(id, body.value.name, revision);
    Ok(tagged(StatusCode::OK, &body, revision))
}

/// Removes the profile and its viewing state; original media is untouched.
pub async fn delete_profile(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::ProfilesManage)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProfilesManage))
        .await?;
    let (_, revision) = load_profile(&mut tx, &principal, &id).await?;
    core::revise(revision, expected, true)?;
    if !playscale_core::catalog::removable_profile(&id) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "default_profile",
            "The default profile cannot be removed",
        ));
    }
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
    sqlx::query("DELETE FROM profiles WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
