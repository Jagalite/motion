//! Content tickets and the v2 original-file byte route.
//!
//! A ticket is an alternative credential on media routes only. It pins one
//! file revision and purpose, never outlives the credential that requested
//! it, and is re-validated against its issuing principal on every request
//! (`access::admit_file_ticket`). Delivery-generation and download tickets
//! need the delivery (A07) and download (A13) services; until those exist
//! those resource families are unknown (404), never a broader grant.
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

async fn file_facts(
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
    let library_ids = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT ls.library_id FROM library_sources ls JOIN media_files f ON f.library_id=ls.source_id WHERE f.id=?"
    ).bind(file_id).fetch_all(conn).await?.into_iter().collect();
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
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &digest, now)? == Idempotent::Replay {
        let stored = stored.unwrap();
        let nonce = stored.issued_nonce.clone().unwrap_or_default();
        let token = auth::derive_secret(&app.access.key, Purpose::Ticket, &nonce);
        let live: Option<(String, String)> = sqlx::query_as(
            "SELECT file_id,file_revision FROM content_tickets WHERE token_hash=? AND revoked=0 AND expires_at>?",
        )
        .bind(auth::token_hash(&token))
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let (file_id, revision) = live.ok_or(AccessError::ReplayUnavailable)?;
        let mut body = stored.body.clone().unwrap_or_default();
        body["url"] = content_url(&file_id, &revision, &token).into();
        return Ok((stored.status, Json(body)).into_response());
    }
    let Some(file) = input.file else {
        // Delivery and download tickets need services this server lacks.
        return Err(Problem::not_found());
    };
    let facts = file_facts(&mut tx, &file.file_id).await?;
    let parent_expiry = caller.credential.as_ref().map(|(_, c)| c.expires_at);
    let expires_at = core::grant_file_ticket(
        &principal,
        input.purpose,
        &file.file_revision,
        facts.as_ref(),
        input.ttl_seconds,
        parent_expiry,
        now,
    )?;
    let id = new_id();
    let nonce = auth::nonce();
    let token = auth::derive_secret(&app.access.key, Purpose::Ticket, &nonce);
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
    ack["url"] = content_url(&file.file_id, &file.file_revision, &token).into();
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
}

type TicketRow = (String, String, String, String, Option<String>, i64, bool);

/// The principal a ticket speaks for, re-derived from current state.
async fn ticket_principal(
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
    if let Some(token) = &params.ticket {
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
    }
    let query = media::MediaQuery {
        revision: params.revision,
    };
    media::serve(State(app), Path(file_id), Query(query), method, headers)
        .await
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
