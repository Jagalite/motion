//! Durable transactional-mail outbox shared by relay and direct delivery.
//!
//! Every message is composed (and DKIM-signed in direct mode) once, sealed,
//! and stored with its configuration revision, signing-key identity, restore
//! epoch and owner. A worker reserves an attempt in one committed write before
//! any network work, rechecks eligibility immediately before DATA, and records
//! the adapter's observation through `playscale_core::mail` in a second write.
//! Terminal states erase the sealed payload in the same write.
use super::*;
use crate::mail::{self as adapter, Content};
use base64::{Engine, engine::general_purpose::STANDARD};
use playscale_core::mail::{self as policy, Decision, Message, Mode, Observation, Phase, Purpose};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
struct Payload {
    recipient: String,
    envelope_from: String,
    bytes: String,
}

fn outgoing(
    mail: &Mail,
    id: &str,
    recipient: &str,
    content: &Content<'_>,
) -> Option<(adapter::Outgoing, String)> {
    let from = adapter::message::parse_mailbox(&mail.cfg.from)?;
    let direct = mail.cfg.mode == Mode::Direct;
    let (id_domain, envelope_from) = if direct {
        (
            mail.cfg.direct.hostname.clone(),
            mail.cfg.direct.envelope_from.clone(),
        )
    } else {
        (from.domain().to_owned(), from.address.clone())
    };
    let signing = if direct {
        let (pkcs8, _) = mail.dkim.as_ref()?;
        Some(adapter::Signing {
            selector: &mail.cfg.direct.selector,
            pkcs8,
        })
    } else {
        None
    };
    let rendered =
        adapter::render(content, &from, recipient, id, &id_domain, now(), signing).ok()?;
    Some((rendered, envelope_from))
}

/// Compose (and sign) a throwaway message exactly as a real one would be.
/// Callers run it before evaluating recipient eligibility, and once more for
/// recipients that receive nothing, so an unusable configuration is uniformly
/// unavailable and the response path does equal work for every address.
pub(super) fn rehearse(mail: &Mail) -> bool {
    outgoing(
        mail,
        &new_id(),
        "rehearsal@motion.invalid",
        &Content::RegistrationCode {
            code: "00000000",
            origin: "https://motion.invalid",
        },
    )
    .is_some()
}

/// Compose, sign, seal and insert one message. Runs inside the caller's
/// reserved writer so the message commits with the state that requires it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn enqueue(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    mail: &Mail,
    purpose: Purpose,
    owner: &str,
    transaction: Option<&str>,
    recipient: &str,
    content: Content<'_>,
    expires_at: i64,
) -> Result<String, Problem> {
    let id = new_id();
    let (outgoing, envelope_from) =
        outgoing(mail, &id, recipient, &content).ok_or_else(unavailable)?;
    let payload = seal(
        app,
        &format!("mail:{id}"),
        &jsonstr(&Payload {
            recipient: recipient.to_owned(),
            envelope_from,
            bytes: STANDARD.encode(&outgoing.bytes),
        }),
    )?;
    let domain = recipient.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    sqlx::query("INSERT INTO signin_outbox(id,purpose,transaction_id,owner_id,payload,phase,next_attempt,expires_at,mode,config_revision,key_id,epoch,recipient_domain,destination_hash,message_hash,created_at,updated_at) VALUES(?,?,?,?,?,'queued',?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(purpose.as_str())
        .bind(transaction)
        .bind(owner)
        .bind(payload)
        .bind(now())
        .bind(expires_at)
        .bind(mail.cfg.mode.as_str())
        .bind(mail.revision)
        .bind(mail.key_id())
        .bind(epoch(c).await?)
        .bind(domain)
        .bind(keyed(app, "email-address", recipient))
        .bind(hex(&Sha256::digest(&outgoing.bytes)))
        .bind(now())
        .bind(now())
        .execute(&mut *c)
        .await?;
    Ok(id)
}

fn message(row: &sqlx::sqlite::SqliteRow) -> Result<Message, Problem> {
    let token: Option<String> = row.get("lease_token");
    Ok(Message {
        id: row.get("id"),
        purpose: Purpose::parse(row.get("purpose")).ok_or_else(unavailable)?,
        owner: row.get("owner_id"),
        mode: if row.get::<String, _>("mode") == "direct" {
            Mode::Direct
        } else {
            Mode::Relay
        },
        config_revision: row.get::<i64, _>("config_revision") as u64,
        key_id: row.get("key_id"),
        epoch: row.get("epoch"),
        expires_at: row.get("expires_at"),
        phase: Phase::parse(row.get("phase")).ok_or_else(unavailable)?,
        attempts: row.get::<i64, _>("attempts") as u32,
        lease: token.map(|token| policy::Lease {
            attempt: row.get::<Option<i64>, _>("lease_attempt").unwrap_or(0) as u32,
            token,
            until: row.get::<Option<i64>, _>("lease_until").unwrap_or(0),
        }),
        next_attempt: row.get("next_attempt"),
        ambiguous: row.get("ambiguous"),
    })
}

/// Is the owning challenge, invitation or test still waiting for this message?
async fn owner_live(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    m: &Message,
) -> Result<bool, Problem> {
    Ok(match m.purpose {
        Purpose::SigninCode | Purpose::RegistrationCode => {
            let state: Option<String> =
                sqlx::query_scalar("SELECT state_json FROM signin_transactions WHERE id=?")
                    .bind(&m.owner)
                    .fetch_optional(&mut *c)
                    .await?;
            match state.map(|s| serde_json::from_str::<core::Transaction>(&s)) {
                Some(Ok(tx)) => {
                    tx.phase == core::Phase::Pending
                        && tx.server_epoch == app.access.server_epoch
                        && tx.epoch == m.epoch
                }
                _ => false,
            }
        }
        Purpose::Invitation => sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM access_invitations WHERE id=? AND revoked=0 AND device_id IS NULL AND identity_id IS NULL AND expires_at>?",
        )
        .bind(&m.owner)
        .bind(now())
        .fetch_one(&mut *c)
        .await?
            > 0,
        // A notice has no proof to protect; it lives until its deadline.
        Purpose::AccountNotice => true,
        Purpose::Test => sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM signin_mail_tests WHERE id=? AND expires_at>?",
        )
        .bind(&m.owner)
        .bind(now())
        .fetch_one(&mut *c)
        .await?
            > 0,
    })
}

struct Facts {
    enabled: bool,
    mode: Mode,
    revision: u64,
    key_id: Option<String>,
    epoch: String,
    hold: bool,
    owner_live: bool,
}

impl Facts {
    fn current(&self) -> policy::Current<'_> {
        policy::Current {
            now: now(),
            enabled: self.enabled,
            mode: self.mode,
            config_revision: self.revision,
            key_id: self.key_id.as_deref(),
            epoch: &self.epoch,
            restore_hold: self.hold,
            owner_live: self.owner_live,
        }
    }
}

async fn observe_facts(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    m: &Message,
) -> Result<(Facts, Mail), Problem> {
    let mail = mail(c, app).await?;
    Ok((
        Facts {
            // An explicit delivery test may run before email sign-in is enabled.
            enabled: mail.cfg.enabled || m.purpose == Purpose::Test,
            mode: mail.cfg.mode,
            revision: mail.revision as u64,
            key_id: mail.key_id(),
            epoch: epoch(c).await?,
            hold: hold(c).await?,
            owner_live: owner_live(c, app, m).await?,
        },
        mail,
    ))
}

async fn persist(
    c: &mut sqlx::SqliteConnection,
    m: &Message,
    detail: Option<&str>,
) -> Result<(), Problem> {
    let terminal = m.phase.terminal();
    sqlx::query("UPDATE signin_outbox SET phase=?,attempts=?,next_attempt=?,ambiguous=?,lease_token=?,lease_attempt=?,lease_until=?,payload=CASE WHEN ? THEN NULL ELSE payload END,last_error=COALESCE(?,last_error),accepted_at=CASE WHEN ?='accepted' THEN ? ELSE accepted_at END,updated_at=? WHERE id=?")
        .bind(m.phase.as_str())
        .bind(m.attempts as i64)
        .bind(m.next_attempt)
        .bind(m.ambiguous)
        .bind(m.lease.as_ref().map(|l| l.token.as_str()))
        .bind(m.lease.as_ref().map(|l| l.attempt as i64))
        .bind(m.lease.as_ref().map(|l| l.until))
        .bind(terminal)
        .bind(detail)
        .bind(m.phase.as_str())
        .bind(now())
        .bind(now())
        .bind(&m.id)
        .execute(&mut *c)
        .await?;
    if m.purpose == Purpose::Test && terminal {
        // Transport acceptance of an explicit test is recorded for the tested
        // revision only. It is not proof of receipt (see confirm_test).
        sqlx::query(
            "UPDATE signin_settings SET email_test_at=?,email_test_ok=? WHERE email_revision=?",
        )
        .bind(now())
        .bind(m.phase == Phase::Accepted)
        .bind(m.config_revision as i64)
        .execute(&mut *c)
        .await?;
    }
    Ok(())
}

async fn load(c: &mut sqlx::SqliteConnection, id: &str) -> Result<Option<Message>, Problem> {
    match sqlx::query("SELECT * FROM signin_outbox WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
    {
        Some(row) => Ok(Some(message(&row)?)),
        None => Ok(None),
    }
}

async fn recheck(app: &App, id: &str, token: &str) -> bool {
    async {
        let mut c = app.db.acquire().await?;
        let Some(m) = load(&mut c, id).await? else {
            return Ok::<_, Problem>(false);
        };
        let (facts, _) = observe_facts(&mut c, app, &m).await?;
        Ok(policy::may_transmit(&m, token, &facts.current()))
    }
    .await
    .unwrap_or(false)
}

/// Run one durable step for message `id`. Returns the resulting phase.
pub(super) async fn process(app: &App, id: &str) -> Result<Phase, Problem> {
    let token = auth::nonce();
    let mut tx = begin_write(&app.db).await?;
    let m = load(&mut tx, id).await?.ok_or_else(Problem::not_found)?;
    let (facts, mail) = observe_facts(&mut tx, app, &m).await?;
    let attempt = match policy::claim(&m, &facts.current(), &token) {
        Decision::Wait => {
            tx.commit().await?;
            return Ok(m.phase);
        }
        Decision::Finish(next) => {
            persist(&mut tx, &next, None).await?;
            tx.commit().await?;
            return Ok(next.phase);
        }
        Decision::Attempt(next) => next,
    };
    let sealed: Option<String> = sqlx::query_scalar("SELECT payload FROM signin_outbox WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let payload: Option<Payload> = sealed
        .and_then(|sealed| open(app, &format!("mail:{id}"), &sealed).ok())
        .and_then(|clear| serde_json::from_str(&clear).ok());
    let Some(payload) = payload else {
        // Missing or undecryptable payload: nothing can ever be sent.
        let failed = Message {
            phase: Phase::Failed,
            lease: None,
            ..m
        };
        persist(&mut tx, &failed, Some("payload unavailable")).await?;
        tx.commit().await?;
        return Ok(Phase::Failed);
    };
    persist(&mut tx, &attempt, None).await?;
    sqlx::query("UPDATE signin_outbox SET last_attempt_at=? WHERE id=?")
        .bind(now())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let bytes = STANDARD.decode(&payload.bytes).unwrap_or_default();
    let (observation, detail) = if bytes.is_empty() {
        (
            Observation::Permanent(policy::Stage::Message),
            "message bytes unavailable".to_owned(),
        )
    } else if attempt.mode == Mode::Direct {
        adapter::deliver_direct(
            &app.access.mail.routing(),
            &adapter::Direct {
                helo: &mail.cfg.direct.hostname,
                envelope_from: &payload.envelope_from,
                recipient: &payload.recipient,
                message: &bytes,
                loopback_port: mail.cfg.direct.loopback_port,
            },
            || recheck(app, id, &token),
        )
        .await
    } else {
        adapter::deliver_relay(
            &mail.relay(),
            &payload.envelope_from,
            &payload.recipient,
            &bytes,
            recheck(app, id, &token),
        )
        .await
    };

    let mut tx = begin_write(&app.db).await?;
    let current = load(&mut tx, id).await?.ok_or_else(Problem::not_found)?;
    let (facts, _) = observe_facts(&mut tx, app, &current).await?;
    let phase = match policy::observe(&current, &token, observation, &facts.current()) {
        Ok(next) => {
            persist(&mut tx, &next, Some(&redact(&detail))).await?;
            next.phase
        }
        Err(policy::Error::StaleAttempt) => current.phase,
    };
    audit(
        &mut tx,
        "mail-worker",
        &current.owner,
        "email.delivery",
        phase.as_str(),
    )
    .await?;
    tx.commit().await?;
    Ok(phase)
}

/// Bounded, address-free diagnostic text.
fn redact(detail: &str) -> String {
    detail
        .chars()
        .filter(char::is_ascii)
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .take(300)
        .collect::<String>()
        .split_whitespace()
        .filter(|word| !word.contains('@'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Process the next due message, if any.
pub async fn deliver_mail_once(app: &App) -> Result<bool, Problem> {
    let mut tx = begin_write(&app.db).await?;
    sqlx::query(
        "DELETE FROM signin_outbox WHERE phase NOT IN ('queued','sending') AND updated_at<?",
    )
    .bind(now() - policy::RETENTION_SECONDS)
    .execute(&mut *tx)
    .await?;
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM signin_outbox WHERE (phase='queued' AND (next_attempt<=? OR expires_at<=?)) OR (phase='sending' AND lease_until<=?) ORDER BY next_attempt,created_at LIMIT 1",
    )
    .bind(now())
    .bind(now())
    .bind(now())
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    let Some(id) = id else { return Ok(false) };
    process(app, &id).await?;
    Ok(true)
}

/// Cancel queued messages owned by `owner`. In-flight attempts resolve through
/// their own observation against the invalidated owner.
pub(super) async fn cancel_owner(
    c: &mut sqlx::SqliteConnection,
    owner: &str,
) -> Result<(), Problem> {
    sqlx::query("UPDATE signin_outbox SET phase='cancelled',payload=NULL,updated_at=? WHERE owner_id=? AND phase='queued'")
        .bind(now())
        .bind(owner)
        .execute(c)
        .await?;
    Ok(())
}

/// Cancel every queued message (configuration, origin or restore changes).
pub(super) async fn cancel_all(c: &mut sqlx::SqliteConnection) -> Result<(), Problem> {
    sqlx::query(
        "UPDATE signin_outbox SET phase='cancelled',payload=NULL,updated_at=? WHERE phase='queued'",
    )
    .bind(now())
    .execute(c)
    .await?;
    Ok(())
}

pub(super) fn status(phase: &str, attempts: i64) -> &'static str {
    match phase {
        "queued" if attempts > 0 => "retry_pending",
        "queued" => "queued",
        "sending" => "sending",
        "accepted" => "accepted",
        "failed" => "failed",
        "unknown" => "acceptance_unknown",
        "expired" => "expired",
        _ => "cancelled",
    }
}

pub(super) fn activity_value(row: &sqlx::sqlite::SqliteRow) -> Value {
    let phase: String = row.get("phase");
    let attempts: i64 = row.get("attempts");
    json!({
        "id": row.get::<String,_>("id"),
        "cursor": format!("{}:{}", row.get::<i64,_>("created_at"), row.get::<String,_>("id")),
        "purpose": row.get::<String,_>("purpose"),
        "status": status(&phase, attempts),
        "attempts": attempts,
        "max_attempts": policy::MAX_ATTEMPTS,
        "possible_duplicate": row.get::<bool,_>("ambiguous"),
        "mode": row.get::<String,_>("mode"),
        "configuration_revision": row.get::<i64,_>("config_revision").to_string(),
        "recipient_domain": row.get::<String,_>("recipient_domain"),
        "created_at": row.get::<i64,_>("created_at"),
        "last_attempt_at": row.get::<Option<i64>,_>("last_attempt_at"),
        "next_attempt_at": if phase == "queued" { Some(row.get::<i64,_>("next_attempt")) } else { None },
        "accepted_at": row.get::<Option<i64>,_>("accepted_at"),
        "expires_at": row.get::<i64,_>("expires_at"),
        "detail": row.get::<Option<String>,_>("last_error"),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn redaction_drops_addresses_and_controls() {
        assert_eq!(
            super::redact("mx.example: rcpt_to 550 user@example.com\r\nx"),
            "mx.example: rcpt_to 550 x"
        );
    }
}
