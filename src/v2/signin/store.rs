use super::*;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub(super) fn keyed(app: &App, purpose: &str, value: &str) -> String {
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(&app.access.key).unwrap();
    h.update(b"motion-signin-v1:");
    h.update(purpose.as_bytes());
    h.update(b":");
    h.update(value.as_bytes());
    format!("{:x}", h.finalize().into_bytes())
}
/// Verify keyed challenge digests using the MAC implementation's constant-time
/// comparison. Malformed persisted digests fail closed.
pub(super) fn verify_keyed(app: &App, purpose: &str, value: &str, expected: &str) -> bool {
    let Some(expected) = unhex(expected) else {
        return false;
    };
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(&app.access.key).unwrap();
    h.update(b"motion-signin-v1:");
    h.update(purpose.as_bytes());
    h.update(b":");
    h.update(value.as_bytes());
    h.verify_slice(&expected).is_ok()
}

fn cipher(app: &App) -> XChaCha20Poly1305 {
    let key = keyed(app, "encryption-key", "");
    XChaCha20Poly1305::new_from_slice(&unhex(&key).unwrap()).unwrap()
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
pub(super) fn seal(app: &App, aad: &str, value: &str) -> Result<String, Problem> {
    let random = auth::random_key();
    let nonce = &random[..24];
    let bytes = cipher(app)
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: value.as_bytes(),
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| unavailable())?;
    Ok(format!("{}{}", hex(nonce), hex(&bytes)))
}
pub(super) fn open(app: &App, aad: &str, value: &str) -> Result<String, Problem> {
    let bytes = unhex(value)
        .filter(|b| b.len() >= 40)
        .ok_or_else(unavailable)?;
    let clear = cipher(app)
        .decrypt(
            XNonce::from_slice(&bytes[..24]),
            Payload {
                msg: &bytes[24..],
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| unavailable())?;
    String::from_utf8(clear).map_err(|_| unavailable())
}
pub(super) async fn epoch(c: &mut sqlx::SqliteConnection) -> Result<String, Problem> {
    Ok(
        sqlx::query_scalar("SELECT restore_epoch FROM server_identity")
            .fetch_one(c)
            .await?,
    )
}
pub(super) async fn hold(c: &mut sqlx::SqliteConnection) -> Result<bool, Problem> {
    Ok(sqlx::query_scalar(
        "SELECT reviewed_epoch != restore_epoch FROM signin_settings,server_identity",
    )
    .fetch_one(c)
    .await?)
}
pub(crate) async fn audit(
    c: &mut sqlx::SqliteConnection,
    actor: &str,
    target: &str,
    action: &str,
    outcome: &str,
) -> Result<(), Problem> {
    sqlx::query("INSERT INTO signin_audit(actor,target,action,outcome,at) VALUES(?,?,?,?,?)")
        .bind(actor)
        .bind(target)
        .bind(action)
        .bind(outcome)
        .bind(now())
        .execute(c)
        .await?;
    Ok(())
}
/// A separate committed writer reservation makes failed requests count too.
pub(super) async fn limit(app: &App, bucket: &str, max: i64, window: i64) -> Result<(), Problem> {
    let mut tx = begin_write(&app.db).await?;
    sqlx::query("DELETE FROM signin_limits WHERE window_start<?")
        .bind(now() - 86400)
        .execute(&mut *tx)
        .await?;
    let key = keyed(app, "rate", bucket);
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT window_start,attempts FROM signin_limits WHERE bucket=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
    let (start, attempts) = row
        .filter(|(start, _)| now() - start < window)
        .unwrap_or((now(), 0));
    if attempts >= max {
        return Err(Problem::rate_limited((start + window - now()).max(1) as u64));
    }
    sqlx::query("INSERT INTO signin_limits(bucket,window_start,attempts) VALUES(?,?,?) ON CONFLICT(bucket) DO UPDATE SET window_start=excluded.window_start,attempts=excluded.attempts").bind(key).bind(start).bind(attempts+1).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
pub(super) async fn owned_user(
    c: &mut sqlx::SqliteConnection,
    caller: &Caller,
) -> Result<Option<User>, Problem> {
    let id: Option<String> = sqlx::query_scalar("SELECT user_id FROM devices WHERE id=?")
        .bind(&caller.principal.id)
        .fetch_optional(&mut *c)
        .await?
        .flatten();
    match id {
        Some(id) => access_management::load_user(c, &id).await,
        None => Ok(None),
    }
}
pub(super) async fn actor_valid(
    c: &mut sqlx::SqliteConnection,
    id: &str,
    hash: Option<&str>,
) -> Result<(), Problem> {
    if id == "operator" && hash.is_none() {
        return Ok(());
    }
    let hash = hash.ok_or_else(unavailable)?;
    let stored = auth::load_credential(c, hash)
        .await?
        .ok_or_else(unavailable)?;
    let (p, _) = auth::authenticate_stored(c, &stored, now()).await?;
    if p.id != id {
        return Err(unavailable());
    }
    Ok(())
}
pub(super) async fn fresh_until(
    c: &mut sqlx::SqliteConnection,
    caller: &Caller,
) -> Result<Option<i64>, Problem> {
    if caller.is_local_operator() {
        return Ok(Some(now() + core::FRESH_SECONDS));
    }
    let at: Option<i64> = sqlx::query_scalar(
        "SELECT authenticated_at FROM signin_families WHERE device_id=? AND revoked=0",
    )
    .bind(&caller.principal.id)
    .fetch_optional(c)
    .await?;
    Ok(at
        .filter(|at| core::fresh(*at, now()))
        .map(|at| at + core::FRESH_SECONDS))
}
/// Consume action-bound proof in the same transaction as the protected write.
pub(crate) async fn require_fresh(
    c: &mut sqlx::SqliteConnection,
    caller: &Caller,
    headers: &HeaderMap,
    action: &str,
    target: &str,
) -> Result<i64, Problem> {
    caller.reauthenticate(c).await?;
    if let Some(until) = fresh_until(c, caller).await? {
        return Ok(until);
    }
    let id = headers
        .get("x-motion-action-proof")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| problem(core::Error::RecentProofRequired))?;
    let row = sqlx::query("SELECT * FROM signin_proofs WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(|| problem(core::Error::RecentProofRequired))?;
    let actor: String = row.get("actor_id");
    let a: String = row.get("action");
    let t: String = row.get("target");
    let e: String = row.get("epoch");
    let approver: Option<String> = row.get("approved_by");
    let p = core::Proof {
        actor: &actor,
        action: &a,
        target: &t,
        epoch: &e,
        expires_at: row.get("expires_at"),
        approved_by: approver.as_deref(),
        consumed: row.get("consumed"),
    };
    if !core::proof_valid(
        &p,
        &caller.principal.id,
        action,
        target,
        &epoch(c).await?,
        now(),
    ) {
        return Err(problem(core::Error::RecentProofRequired));
    }
    let actor_hash: Option<String> = row.get("actor_credential_hash");
    if actor_hash.as_deref() != caller.credential().map(|x| x.0) {
        return Err(problem(core::Error::RecentProofRequired));
    }
    let approver_hash: Option<String> = row.get("approver_credential_hash");
    actor_valid(c, approver.as_deref().unwrap(), approver_hash.as_deref()).await?;
    if approver.as_deref() != Some("operator") {
        let approver_device = auth::load_device(c, approver.as_deref().unwrap())
            .await?
            .ok_or_else(unavailable)?;
        let actor_user = owned_user(c, caller).await?;
        if !approver_device
            .device
            .grant
            .permissions
            .contains(&Permission::SystemAdmin)
            && (actor_user.is_none()
                || approver_device.user_id.as_deref() != actor_user.as_ref().map(|u| u.id.as_str()))
        {
            return Err(problem(core::Error::RecentProofRequired));
        }
    }
    sqlx::query("UPDATE signin_proofs SET consumed=1 WHERE id=?")
        .bind(id)
        .execute(&mut *c)
        .await?;
    audit(
        c,
        &caller.principal.id,
        target,
        action,
        "fresh_proof_consumed",
    )
    .await?;
    Ok(row.get("expires_at"))
}
#[derive(Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct EmailConfig {
    pub enabled: bool,
    /// Relay delivery (the existing default) or native direct delivery.
    pub mode: playscale_core::mail::Mode,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub from: String,
    pub loopback_test: bool,
    pub direct: DirectConfig,
}
#[derive(Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct DirectConfig {
    /// EHLO name and Message-ID domain.
    pub hostname: String,
    /// Envelope sender (return path) for bounces, managed outside Motion.
    pub envelope_from: String,
    pub selector: String,
    /// Explicit isolated qualification route: plaintext to 127.0.0.1:port.
    pub loopback_port: Option<u16>,
}
/// Loaded delivery configuration, with secrets opened only in memory.
pub(super) struct Mail {
    pub cfg: EmailConfig,
    pub revision: i64,
    pub password: Option<String>,
    /// PKCS#8 DER and base64 SPKI of the DKIM key, if configured.
    pub dkim: Option<(Vec<u8>, String)>,
}
impl Mail {
    /// Whether messages can be composed at all. Checked before any recipient
    /// eligibility so an unusable configuration is uniformly unavailable.
    pub fn deliverable(&self) -> bool {
        crate::mail::message::parse_mailbox(&self.cfg.from).is_some()
            && (self.cfg.mode == playscale_core::mail::Mode::Relay
                || (self.dkim.is_some()
                    && !self.cfg.direct.hostname.is_empty()
                    && !self.cfg.direct.envelope_from.is_empty()))
    }
    /// The signing-key identity fenced into each direct-mode message.
    pub fn key_id(&self) -> Option<String> {
        (self.cfg.mode == playscale_core::mail::Mode::Direct)
            .then(|| {
                self.dkim
                    .as_ref()
                    .map(|(_, public)| crate::mail::dkim::key_id(public))
            })
            .flatten()
    }
    pub fn relay(&self) -> crate::mail::Relay {
        crate::mail::Relay {
            host: self.cfg.host.clone(),
            port: self.cfg.port,
            username: self.cfg.username.clone(),
            password: self.password.clone(),
            loopback_test: self.cfg.loopback_test,
        }
    }
}
pub(super) async fn mail(c: &mut sqlx::SqliteConnection, app: &App) -> Result<Mail, Problem> {
    let (json, secret, revision, dkim, public): (
        String,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT email_json,email_secret,email_revision,dkim_secret,dkim_public FROM signin_settings",
    )
    .fetch_one(c)
    .await?;
    let cfg: EmailConfig = serde_json::from_str(&json).map_err(Problem::internal)?;
    let password = secret.map(|s| open(app, "mail-secret", &s)).transpose()?;
    let dkim = match (dkim, public) {
        (Some(sealed), Some(public)) => {
            let der = unhex(&open(app, "dkim-key", &sealed)?).ok_or_else(unavailable)?;
            Some((der, public))
        }
        _ => None,
    };
    Ok(Mail {
        cfg,
        revision,
        password,
        dkim,
    })
}
pub(super) async fn provider(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    id: &str,
) -> Result<(i64, bool, crate::signin_protocol::OidcConfig), Problem> {
    let row = sqlx::query("SELECT * FROM signin_providers WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(Problem::not_found)?;
    let origin: Option<String> = sqlx::query_scalar("SELECT public_origin FROM signin_settings")
        .fetch_one(c)
        .await?;
    let secret: Option<String> = row.get("secret");
    Ok((
        row.get("revision"),
        row.get("enabled"),
        crate::signin_protocol::OidcConfig {
            issuer: row.get("issuer"),
            client_id: row.get("client_id"),
            client_secret: secret
                .map(|s| open(app, &format!("provider:{id}"), &s))
                .transpose()?,
            redirect_uri: format!(
                "{}/api/v2/auth/oidc/callback",
                origin.ok_or_else(unavailable)?
            ),
            destinations: serde_json::from_str(row.get::<&str, _>("destinations"))
                .map_err(Problem::internal)?,
        },
    ))
}
pub(super) async fn family_valid(
    c: &mut sqlx::SqliteConnection,
    device: &str,
) -> Result<bool, Problem> {
    let row=sqlx::query("SELECT f.*,i.enabled AS identity_enabled,i.revision AS current_identity_revision,i.kind,u.disabled,p.enabled AS provider_enabled,p.revision AS provider_revision,s.email_json,s.email_revision,s.reviewed_epoch,x.restore_epoch FROM signin_families f JOIN signin_identities i ON i.id=f.identity_id JOIN access_users u ON u.id=f.user_id LEFT JOIN signin_providers p ON p.id=i.provider_id CROSS JOIN signin_settings s CROSS JOIN server_identity x WHERE f.device_id=?").bind(device).fetch_optional(c).await?;
    let Some(r) = row else { return Ok(true) };
    let is_email = r.get::<String, _>("kind") == "email";
    let email: EmailConfig =
        serde_json::from_str(r.get("email_json")).map_err(Problem::internal)?;
    let old: String = r.get("epoch");
    let current: String = r.get("restore_epoch");
    Ok(core::family_valid(
        &core::Family {
            revoked: r.get("revoked"),
            epoch: &old,
            current_epoch: &current,
            expires_at: r.get("expires_at"),
            user_enabled: !r.get::<bool, _>("disabled"),
            identity_enabled: r.get("identity_enabled"),
            identity_revision: r.get::<i64, _>("identity_revision") as u64,
            current_identity_revision: r.get::<i64, _>("current_identity_revision") as u64,
            method_enabled: if is_email {
                email.enabled
            } else {
                r.get::<Option<bool>, _>("provider_enabled")
                    .unwrap_or(false)
            } && r.get::<String, _>("reviewed_epoch") == current,
            method_revision: r.get::<i64, _>("method_revision") as u64,
            current_method_revision: if is_email {
                r.get::<i64, _>("email_revision")
            } else {
                r.get::<Option<i64>, _>("provider_revision").unwrap_or(0)
            } as u64,
        },
        now(),
    ))
}
pub(super) async fn revoke_families(
    c: &mut sqlx::SqliteConnection,
    ids: &[String],
    actor: &str,
) -> Result<(), Problem> {
    let mut changes = Vec::new();
    for id in ids {
        let device: Option<String> =
            sqlx::query_scalar("SELECT device_id FROM signin_families WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *c)
                .await?;
        if let Some(device) = device
            && let Some(row) = auth::load_device(c, &device).await?
        {
            let mut next = row.device.clone();
            next.revoked = true;
            next.revision += 1;
            changes.push((row.device, next));
        }
    }
    access_management::guard_admin_change(c, &changes).await?;
    for (old, next) in changes {
        auth::update_device(c, &old, &next).await?;
        sqlx::query("DELETE FROM credentials WHERE device_id=?")
            .bind(&old.id)
            .execute(&mut *c)
            .await?;
    }
    for id in ids {
        sqlx::query("UPDATE signin_families SET revoked=1,revision=revision+1 WHERE id=?")
            .bind(id)
            .execute(&mut *c)
            .await?;
        audit(c, actor, id, "session.revoke", "ok").await?;
    }
    Ok(())
}
