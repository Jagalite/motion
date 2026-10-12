//! Content tickets and the v2 original-file byte route.
//!
//! A ticket is an alternative credential on media routes only. It pins one
//! file revision and purpose, never outlives the credential that requested
//! it, and is re-validated against its issuing principal on every request
//! (`access::admit_file_ticket`). Generation tickets additionally require the
//! current owner and reducer serving state. Download resources remain unavailable.
use super::{
    Body, Problem, Scope,
    auth::{self, Caller, Purpose},
    idempotency_key, timestamp,
};
use crate::{App, db::begin_write, media, new_id, now};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
};
use playscale_core::access::{
    self as core, AccessError, FileFacts, FileTicket, Idempotent, Principal, TicketError,
    TicketPurpose,
};
use serde::Deserialize;

impl From<TicketError> for Problem {
    fn from(e: TicketError) -> Self {
        match e {
            TicketError::Forbidden(p) => AccessError::Forbidden(p).into(),
            TicketError::NotFound => Problem::not_found(),
            TicketError::SourceChanged => Problem::new(
                StatusCode::CONFLICT,
                "source_revision_changed",
                "The file changed; read the item again for its current revision",
            ),
            TicketError::CastUnavailable => Problem::new(
                StatusCode::FORBIDDEN,
                "cast_unavailable",
                "Casting requires an approved receiver policy, which this server does not provide",
            ),
            TicketError::InvalidTtl => {
                Problem::invalid("invalid_ttl", "ttl_seconds must be between 1 and 3600")
            }
            TicketError::TicketInvalid => Problem::new(
                StatusCode::UNAUTHORIZED,
                "ticket_invalid",
                "The ticket is expired, revoked or not valid for this resource",
            ),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRef {
    file_id: String,
    file_revision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentAccessInput {
    purpose: TicketPurpose,
    delivery_id: Option<String>,
    generation: Option<String>,
    file: Option<FileRef>,
    download_id: Option<String>,
    ttl_seconds: i64,
}

pub(crate) async fn file_facts(
    conn: &mut sqlx::SqliteConnection,
    file_id: &str,
) -> Result<Option<FileFacts>, Problem> {
    let revision: Option<String> =
        sqlx::query_scalar("SELECT revision FROM media_files WHERE id=? AND available=1")
            .bind(file_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(revision) = revision else {
        return Ok(None);
    };
    // A generated rendition lives in the managed output store, which is no
    // logical library: it is in the libraries of the source it renders, while
    // its registration still matches both current revisions. A stale or
    // orphaned rendition is in no library and so readable by no grant.
    let library_ids = sqlx::query_scalar::<_, String>(
        "SELECT ls.library_id FROM library_sources ls JOIN media_files f ON f.library_id=ls.source_id WHERE f.id=? AND f.generated=0 \
         UNION SELECT ls.library_id FROM renditions r JOIN media_files g ON g.id=r.file_id AND g.revision=r.file_revision \
         JOIN media_files s ON s.id=r.source_file_id AND s.revision=r.source_revision AND s.generated=0 \
         JOIN library_sources ls ON ls.source_id=s.library_id WHERE r.file_id=? AND g.generated=1"
    ).bind(file_id).bind(file_id).fetch_all(conn).await?.into_iter().collect();
    Ok(Some(FileFacts {
        library_ids,
        revision,
    }))
}

fn content_url(file_id: &str, revision: &str, token: &str) -> String {
    let mut url = url::Url::parse("http://motion.invalid/api/v2/media/files/").unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .extend([file_id, "content"]);
    url.query_pairs_mut()
        .append_pair("revision", revision)
        .append_pair("ticket", token);
    // A path and query relative to the server origin the client already uses.
    format!("{}?{}", url.path(), url.query().unwrap_or_default())
}

fn ticket_token(app: &App, nonce: &str, delivery: Option<(&str, &str)>) -> String {
    let secret = auth::derive_secret(&app.access.key, Purpose::Ticket, nonce);
    match delivery {
        Some((id, generation)) => format!("{secret}.delivery.{id}.{generation}"),
        None => secret,
    }
}

fn with_ticket(path: &str, token: &str) -> String {
    format!(
        "{path}{}ticket={token}",
        if path.contains('?') { "&" } else { "?" }
    )
}

/// The complete opaque token is hashed in SQLite. Altering its scope suffix
/// invalidates the lookup; a generation token is never a file credential.
pub(crate) async fn delivery_ticket(
    app: &App,
    token: &str,
    id: &str,
    generation: &str,
) -> Result<Principal, Problem> {
    let suffix = format!(".delivery.{id}.{generation}");
    if !token.ends_with(&suffix) || !crate::delivery::serves_generation(app, id, generation) {
        return Err(TicketError::TicketInvalid.into());
    }
    let mut conn = app.db.acquire().await?;
    let row: Option<TicketRow> = sqlx::query_as(
        "SELECT principal_id,file_id,file_revision,purpose,credential_hash,expires_at,revoked FROM content_tickets WHERE token_hash=?",
    ).bind(auth::token_hash(token)).fetch_optional(&mut *conn).await?;
    let (issuer, file, revision, purpose, credential, expires_at, revoked) =
        row.ok_or(TicketError::TicketInvalid)?;
    if purpose != "playback" {
        return Err(TicketError::TicketInvalid.into());
    }
    let principal = ticket_principal(&mut conn, &issuer, credential.as_deref()).await;
    let facts = file_facts(&mut conn, &file).await?;
    core::admit_file_ticket(
        &FileTicket {
            file_id: file.clone(),
            revision,
            purpose: TicketPurpose::Playback,
            expires_at,
            revoked,
        },
        &file,
        None,
        &principal,
        facts.as_ref(),
        now(),
    )?;
    let principal = principal.map_err(Problem::from)?;
    super::playback::controlled_by(app, &mut conn, &principal, id).await?;
    Ok(principal)
}

pub async fn create(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<ContentAccessInput>,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    let (input, digest) = (body.value, body.digest);
    let families = [
        input.delivery_id.is_some() || input.generation.is_some(),
        input.file.is_some(),
        input.download_id.is_some(),
    ];
    if families.iter().filter(|f| **f).count() != 1
        || (input.delivery_id.is_some() != input.generation.is_some())
        || (input.download_id.is_some() && input.purpose != TicketPurpose::Download)
    {
        return Err(Problem::invalid(
            "invalid_resource_family",
            "Select exactly one resource family: delivery_id with generation, file, or download_id (download purpose)",
        ));
    }
    let now = now();
    let mut tx = begin_write(&app.db).await?;
    let principal = caller.reauthorize(&mut tx, None).await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createContentAccess",
        target: "-",
        key: &key,
    };
    let delivery = input
        .delivery_id
        .as_deref()
        .zip(input.generation.as_deref());
    let (file, resource_url) = if let Some((id, generation)) = delivery {
        if input.purpose != TicketPurpose::Playback {
            return Err(Problem::invalid(
                "unsupported_ticket_purpose",
                "Delivery tickets support playback only; receiver and download grants are unavailable",
            ));
        }
        let owner = super::playback::controlled_by(&app, &mut tx, &principal, id).await?;
        if !crate::delivery::serves_generation(&app, id, generation) {
            return Err(TicketError::TicketInvalid.into());
        }
        let view = crate::delivery::live_view(&app, id).ok_or(TicketError::TicketInvalid)?;
        let g = [view.active.as_ref(), view.pending.as_ref()]
            .into_iter()
            .flatten()
            .find(|g| g.generation == generation)
            .ok_or(TicketError::TicketInvalid)?;
        let endpoint = if g.transport == "hls" {
            "master.m3u8"
        } else {
            "content"
        };
        (
            FileRef {
                file_id: owner.file,
                file_revision: owner.file_revision,
            },
            Some(format!("/api/v2/streams/{id}/{generation}/{endpoint}")),
        )
    } else {
        (input.file.ok_or_else(Problem::not_found)?, None)
    };
    // Reauthorize exact issuance retries as well as first admissions.
    let facts = file_facts(&mut tx, &file.file_id).await?;
    let parent_expiry = caller.credential.as_ref().map(|(_, c)| c.expires_at);
    // Ingress without a durable parent credential cannot mint a bearer grant.
    if caller.credential.is_none() && principal.id != "operator" {
        return Err(Problem::invalid(
            "ticket_parent_required",
            "Use a durable credential to issue content access",
        ));
    }
    let expires_at = core::grant_file_ticket(
        &principal,
        input.purpose,
        &file.file_revision,
        facts.as_ref(),
        input.ttl_seconds,
        parent_expiry,
        now,
    )?;
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &digest, now)? == Idempotent::Replay {
        let stored = stored.unwrap();
        let nonce = stored.issued_nonce.clone().unwrap_or_default();
        let token = ticket_token(&app, &nonce, delivery);
        let live: Option<(String, String)> = sqlx::query_as(
            "SELECT file_id,file_revision FROM content_tickets WHERE token_hash=? AND revoked=0 AND expires_at>?",
        )
        .bind(auth::token_hash(&token))
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let (file_id, revision) = live.ok_or(AccessError::ReplayUnavailable)?;
        let mut body = stored.body.clone().unwrap_or_default();
        body["url"] = resource_url
            .as_ref()
            .map(|path| with_ticket(path, &token))
            .unwrap_or_else(|| content_url(&file_id, &revision, &token))
            .into();
        return Ok((stored.status, Json(body)).into_response());
    }
    let id = new_id();
    let nonce = auth::nonce();
    let token = ticket_token(&app, &nonce, delivery);
    sqlx::query("INSERT INTO content_tickets(id,token_hash,principal_id,credential_hash,file_id,file_revision,purpose,expires_at) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(auth::token_hash(&token))
        .bind(&principal.id)
        .bind(caller.credential.as_ref().map(|(h, _)| h.clone()))
        .bind(&file.file_id)
        .bind(&file.file_revision)
        .bind(match input.purpose {
            TicketPurpose::Download => "download",
            _ => "playback",
        })
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
    // The acknowledgement is stored without the secret; replay re-derives it.
    let mut ack = serde_json::json!({"id": id, "expires_at": timestamp(expires_at)});
    scope
        .save(
            &mut tx,
            &digest,
            StatusCode::CREATED,
            Some(&ack),
            Some(&nonce),
        )
        .await?;
    tx.commit().await?;
    ack["url"] = resource_url
        .as_ref()
        .map(|path| with_ticket(path, &token))
        .unwrap_or_else(|| content_url(&file.file_id, &file.file_revision, &token))
        .into();
    Ok((StatusCode::CREATED, Json(ack)).into_response())
}

/// Only the issuing principal (or an administrator) may revoke a ticket;
/// any other caller sees it as missing.
pub async fn revoke(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let mut tx = begin_write(&app.db).await?;
    let principal = caller.reauthorize(&mut tx, None).await?;
    let updated =
        sqlx::query("UPDATE content_tickets SET revoked=1 WHERE id=? AND (principal_id=? OR ?)")
            .bind(&id)
            .bind(&principal.id)
            .bind(principal.is_admin())
            .execute(&mut *tx)
            .await?;
    if updated.rows_affected() != 1 {
        return Err(Problem::not_found());
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub struct MediaParams {
    revision: Option<String>,
    ticket: Option<String>,
    /// The delivery a direct-play URL belongs to: attribution for
    /// administrator monitoring only, never authorization.
    delivery: Option<String>,
}

type TicketRow = (String, String, String, String, Option<String>, i64, bool);

/// The principal a ticket speaks for, re-derived from current state.
pub(crate) async fn ticket_principal(
    conn: &mut sqlx::SqliteConnection,
    principal_id: &str,
    credential_hash: Option<&str>,
) -> Result<Principal, AccessError> {
    match credential_hash {
        // Operator tickets carry no credential; the operator is static.
        None if principal_id == "operator" => Ok(Principal::operator()),
        None => Err(AccessError::CredentialRevoked),
        Some(hash) => {
            let stored = auth::load_credential(conn, hash)
                .await
                .map_err(|_| AccessError::Unauthenticated)?
                .ok_or(AccessError::CredentialRevoked)?;
            auth::authenticate_stored(conn, &stored, now())
                .await
                .map(|(p, _)| p)
                .map_err(|_| AccessError::CredentialRevoked)
        }
    }
}

/// GET and HEAD share one authorization path: existence is disclosed only
/// after the caller (bearer, cookie, or ticket) is admitted for the file.
pub async fn file_content(
    State(app): State<App>,
    Path(file_id): Path<String>,
    Query(params): Query<MediaParams>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let mut conn = app.db.acquire().await?;
    let facts = file_facts(&mut conn, &file_id).await?;
    // Who is reading, to attribute direct-play bytes to their live delivery.
    let reader: Option<String>;
    if let Some(token) = &params.ticket {
        if token.contains(".delivery.") {
            return Err(TicketError::TicketInvalid.into());
        }
        let row: Option<TicketRow> = sqlx::query_as(
            "SELECT principal_id,file_id,file_revision,purpose,credential_hash,expires_at,revoked FROM content_tickets WHERE token_hash=?",
        )
        .bind(auth::token_hash(token))
        .fetch_optional(&mut *conn)
        .await?;
        let (principal_id, ticket_file, revision, purpose, credential_hash, expires_at, revoked) =
            row.ok_or(TicketError::TicketInvalid)?;
        let ticket = FileTicket {
            file_id: ticket_file,
            revision,
            purpose: if purpose == "download" {
                TicketPurpose::Download
            } else {
                TicketPurpose::Playback
            },
            expires_at,
            revoked,
        };
        let principal =
            ticket_principal(&mut conn, &principal_id, credential_hash.as_deref()).await;
        reader = Some(principal_id.clone());
        core::admit_file_ticket(
            &ticket,
            &file_id,
            params.revision.as_deref(),
            &principal,
            facts.as_ref(),
            now(),
        )?;
    } else {
        drop(conn);
        let caller = auth::resolve(&app, &method, &headers).await?;
        core::admit_file_bytes(&caller.principal, facts.as_ref())?;
        reader = Some(caller.principal.id.clone());
    }
    // Only reads pinned to a revision are attributed: a delivery's media URL
    // always pins its own, and serving refuses any other. Reserved before
    // serving, so a close meanwhile waits for this response.
    let transfer = match (&reader, &params.revision) {
        (Some(principal), Some(revision)) => crate::delivery::file_transfer(
            &app,
            principal,
            &file_id,
            revision,
            params.delivery.as_deref(),
        ),
        _ => None,
    };
    let query = media::MediaQuery {
        revision: params.revision,
    };
    media::serve(
        State(app.clone()),
        Path(file_id),
        Query(query),
        method,
        headers,
    )
    .await
    .map(|response| crate::delivery::count(transfer, response))
    .map_err(|e| {
        let (status, code, _) = e.parts();
        let code = match code {
            "source_revision_changed" => "source_revision_changed",
            "source_unavailable" => "source_unavailable",
            "stream_limit" => "stream_limit",
            "not_found" => "not_found",
            _ => "media_unavailable",
        };
        let mut problem = Problem::new(status, code, "The media could not be served");
        if status == StatusCode::SERVICE_UNAVAILABLE {
            problem.retry_after = Some(1);
        }
        problem
    })
}
