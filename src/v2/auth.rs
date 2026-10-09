//! Credential derivation, lookup and device persistence.
//!
//! A credential secret is `HMAC(key, purpose:nonce)`, where the key lives in a
//! 0600 file outside the database and the nonce is stored. Rows keep only the
//! SHA-256 of the secret, so a lost acknowledgement can be replayed with the
//! same secret without the database ever holding it. Every request
//! re-authenticates against the current device and parent rows.
use super::Problem;
use crate::App;
use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, Method, StatusCode, header, request::Parts},
};
use hmac::{Hmac, Mac};
use playscale_core::access::{
    AccessError, Credential, CredentialKind, Device, Grant, Permission, Principal, authenticate,
};
use sha2::{Digest, Sha256};
use std::io::Write;

pub const SESSION_COOKIE: &str = "motion_session";

/// Load or create the credential-derivation key. Like the operator token, it
/// is created once with mode 0600 and never logged.
pub fn load_or_create_key(path: &std::path::Path) -> anyhow::Result<[u8; 32]> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(bytes.len() == 32, "invalid credential key file");
        return Ok(bytes.try_into().unwrap());
    }
    let key = random_key();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&key)?;
    file.sync_all()?;
    Ok(key)
}

pub fn random_key() -> [u8; 32] {
    let mut key = [0; 32];
    key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    key
}

pub fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Typed prefixes let leak scanners recognize Motion secrets.
#[derive(Clone, Copy)]
pub enum Purpose {
    Device,
    Access,
    Session,
    PairingCode,
    Ticket,
}

impl Purpose {
    fn prefix(self) -> &'static str {
        match self {
            Self::Device => "mdv",
            Self::Access => "mat",
            Self::Session => "mss",
            Self::PairingCode => "mdc",
            Self::Ticket => "mtk",
        }
    }
}

pub fn derive_secret(key: &[u8; 32], purpose: Purpose, nonce: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(purpose.prefix().as_bytes());
    mac.update(b":");
    mac.update(nonce.as_bytes());
    format!("{}_{:x}", purpose.prefix(), mac.finalize().into_bytes())
}

pub fn token_hash(token: &str) -> String {
    super::sha256(token.as_bytes())
}

#[derive(Clone, Debug)]
pub struct DeviceRow {
    pub device: Device,
    pub name: String,
    pub client_name: String,
}

type DeviceTuple = (
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    bool,
    i64,
    Option<i64>,
);

fn decode(row: DeviceTuple) -> Result<DeviceRow, Problem> {
    let (id, name, client_name, revision, profiles, permissions, policy, revoked, generation, exp) =
        row;
    Ok(DeviceRow {
        device: Device {
            id,
            revision: revision as u64,
            grant: Grant {
                profile_ids: serde_json::from_str(&profiles).map_err(Problem::internal)?,
                permissions: serde_json::from_str(&permissions).map_err(Problem::internal)?,
            },
            policy: serde_json::from_str(&policy).map_err(Problem::internal)?,
            revoked,
            generation: generation as u64,
            credential_expires_at: exp,
        },
        name,
        client_name,
    })
}

const DEVICE_COLUMNS: &str = "id,name,client_name,revision,profile_ids,permissions,policy,revoked,generation,credential_expires_at";

pub async fn load_device(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<DeviceRow>, Problem> {
    let row: Option<DeviceTuple> =
        sqlx::query_as(&format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE id=?"))
            .bind(id)
            .fetch_optional(conn)
            .await?;
    row.map(decode).transpose()
}

pub async fn list_devices(
    conn: &mut sqlx::SqliteConnection,
    after: &str,
    limit: u32,
) -> Result<Vec<DeviceRow>, Problem> {
    let rows: Vec<DeviceTuple> = sqlx::query_as(&format!(
        "SELECT {DEVICE_COLUMNS} FROM devices WHERE id>? ORDER BY id LIMIT ?"
    ))
    .bind(after)
    .bind(i64::from(limit) + 1)
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(decode).collect()
}

fn json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("serializable access state")
}

pub async fn insert_device(
    conn: &mut sqlx::SqliteConnection,
    row: &DeviceRow,
) -> Result<(), Problem> {
    let d = &row.device;
    sqlx::query("INSERT INTO devices(id,name,client_name,revision,profile_ids,permissions,policy,revoked,generation,credential_expires_at,created_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&d.id)
        .bind(&row.name)
        .bind(&row.client_name)
        .bind(d.revision as i64)
        .bind(json(&d.grant.profile_ids))
        .bind(json(&d.grant.permissions))
        .bind(json(&d.policy))
        .bind(d.revoked)
        .bind(d.generation as i64)
        .bind(d.credential_expires_at)
        .bind(crate::now())
        .execute(conn)
        .await?;
    Ok(())
}

/// Compare-and-set write of a decided transition from `previous`.
pub async fn update_device(
    conn: &mut sqlx::SqliteConnection,
    previous: &Device,
    next: &Device,
) -> Result<(), Problem> {
    let updated = sqlx::query("UPDATE devices SET revision=?,profile_ids=?,permissions=?,policy=?,revoked=?,generation=?,credential_expires_at=? WHERE id=? AND revision=? AND generation=?")
        .bind(next.revision as i64)
        .bind(json(&next.grant.profile_ids))
        .bind(json(&next.grant.permissions))
        .bind(json(&next.policy))
        .bind(next.revoked)
        .bind(next.generation as i64)
        .bind(next.credential_expires_at)
        .bind(&previous.id)
        .bind(previous.revision as i64)
        .bind(previous.generation as i64)
        .execute(conn)
        .await?;
    if updated.rows_affected() != 1 {
        return Err(AccessError::StaleRevision.into());
    }
    Ok(())
}

pub async fn insert_credential(
    conn: &mut sqlx::SqliteConnection,
    hash: &str,
    credential: &Credential,
    parent_hash: Option<&str>,
    csrf: Option<&str>,
) -> Result<(), Problem> {
    let kind = match credential.kind {
        CredentialKind::Device => "device",
        CredentialKind::Access => "access",
        CredentialKind::Session => "session",
    };
    sqlx::query("INSERT INTO credentials(token_hash,device_id,kind,generation,expires_at,parent_hash,csrf_token) VALUES (?,?,?,?,?,?,?)")
        .bind(hash)
        .bind(&credential.device_id)
        .bind(kind)
        .bind(credential.generation as i64)
        .bind(credential.expires_at)
        .bind(parent_hash)
        .bind(csrf)
        .execute(conn)
        .await?;
    Ok(())
}

/// device_id, kind, generation, expires_at, parent_hash, csrf_token
type CredentialTuple = (String, String, i64, i64, Option<String>, Option<String>);

pub struct Stored {
    pub credential: Credential,
    pub parent_hash: Option<String>,
    pub csrf: Option<String>,
}

pub async fn load_credential(
    conn: &mut sqlx::SqliteConnection,
    hash: &str,
) -> Result<Option<Stored>, Problem> {
    let row: Option<CredentialTuple> = sqlx::query_as(
        "SELECT device_id,kind,generation,expires_at,parent_hash,csrf_token FROM credentials WHERE token_hash=?",
    )
    .bind(hash)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(
        |(device_id, kind, generation, expires_at, parent_hash, csrf)| Stored {
            credential: Credential {
                device_id,
                kind: match kind.as_str() {
                    "device" => CredentialKind::Device,
                    "access" => CredentialKind::Access,
                    _ => CredentialKind::Session,
                },
                generation: generation as u64,
                expires_at,
            },
            parent_hash,
            csrf,
        },
    ))
}

/// Authenticate a stored credential against its current device and parent rows.
pub async fn authenticate_stored(
    conn: &mut sqlx::SqliteConnection,
    stored: &Stored,
    now: i64,
) -> Result<(Principal, Option<DeviceRow>), Problem> {
    let parent = match &stored.parent_hash {
        Some(hash) => load_credential(conn, hash).await?.map(|p| p.credential),
        None => None,
    };
    let device = load_device(conn, &stored.credential.device_id).await?;
    let principal = authenticate(
        &stored.credential,
        parent.as_ref(),
        device.as_ref().map(|d| &d.device),
        now,
    )?;
    Ok((principal, device))
}

/// The authenticated caller of a v2 request.
#[derive(Clone, Debug)]
pub struct Caller {
    pub principal: Principal,
    /// Hash and row of the presented credential; absent for the operator.
    pub credential: Option<(String, Credential)>,
    /// Present only for cookie sessions.
    pub csrf: Option<String>,
}

impl Caller {
    pub fn require(&self, permission: Permission) -> Result<(), Problem> {
        if self.principal.allows(permission) {
            Ok(())
        } else {
            Err(AccessError::Forbidden(permission).into())
        }
    }

    /// Re-derive the principal inside the write transaction, so a revocation
    /// or downgrade committed after the request was admitted (for example
    /// while its body was uploading) applies before any effect or replay.
    pub async fn reauthorize(
        &self,
        conn: &mut sqlx::SqliteConnection,
        permission: Option<Permission>,
    ) -> Result<Principal, Problem> {
        let principal = match &self.credential {
            None => Principal::operator(),
            Some((hash, _)) => {
                let stored = load_credential(conn, hash)
                    .await?
                    .ok_or(AccessError::CredentialRevoked)?;
                authenticate_stored(conn, &stored, crate::now()).await?.0
            }
        };
        match permission {
            Some(p) if !principal.allows(p) => Err(AccessError::Forbidden(p).into()),
            _ => Ok(principal),
        }
    }
}

pub fn bearer(headers: &HeaderMap) -> Result<Option<&str>, Problem> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty())
        .map(Some)
        .ok_or_else(|| AccessError::Unauthenticated.into())
}

pub fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

/// Digest comparison avoids leaking the operator token through timing.
pub fn is_operator(app: &App, token: &str) -> bool {
    Sha256::digest(token.as_bytes()) == Sha256::digest(app.admin_token.as_bytes())
}

/// Resolve the caller from current stored state.
pub async fn resolve(app: &App, method: &Method, headers: &HeaderMap) -> Result<Caller, Problem> {
    let now = crate::now();
    let mut conn = app.db.acquire().await?;
    if let Some(token) = bearer(headers)? {
        if is_operator(app, token) {
            return Ok(Caller {
                principal: Principal::operator(),
                credential: None,
                csrf: None,
            });
        }
        let hash = token_hash(token);
        let stored = load_credential(&mut conn, &hash)
            .await?
            .filter(|s| s.credential.kind != CredentialKind::Session)
            .ok_or(AccessError::Unauthenticated)?;
        let (principal, _) = authenticate_stored(&mut conn, &stored, now).await?;
        return Ok(Caller {
            principal,
            credential: Some((hash, stored.credential)),
            csrf: None,
        });
    }
    let token = session_cookie(headers).ok_or(AccessError::Unauthenticated)?;
    let hash = token_hash(token);
    let stored = load_credential(&mut conn, &hash)
        .await?
        .filter(|s| s.credential.kind == CredentialKind::Session)
        .ok_or(AccessError::Unauthenticated)?;
    let (principal, _) = authenticate_stored(&mut conn, &stored, now).await?;
    let csrf = stored.csrf.clone().unwrap_or_default();
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        let presented = headers
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if csrf.is_empty()
            || Sha256::digest(presented.as_bytes()) != Sha256::digest(csrf.as_bytes())
        {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                "csrf_required",
                "Cookie-authenticated unsafe requests require X-CSRF-Token",
            ));
        }
    }
    Ok(Caller {
        principal,
        credential: Some((hash, stored.credential)),
        csrf: Some(csrf),
    })
}

impl FromRequestParts<App> for Caller {
    type Rejection = Problem;
    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Problem> {
        resolve(app, &parts.method, &parts.headers).await
    }
}
