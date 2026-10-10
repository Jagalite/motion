//! Authorized, replayable invalidation hints over SSE.
//!
//! One shared task watches the change log and wakes subscribers; streams do
//! not poll SQLite independently while idle. Immediately before every hint,
//! the subscriber is re-authenticated (`access::recheck`) and disclosure is
//! decided by `access::disclose` from current facts, so revocation ends the
//! stream, a policy change resets it, and a resource that left the
//! subscriber's scope while the stream was suspended is never named. No
//! database connection is held while a hint waits for the client.
use super::{
    Problem,
    auth::{self, Caller},
};
use crate::App;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use playscale_core::access::{
    AccessError, Disclosure, EventCursor, Permission, Principal, Recheck, ResetReason, Resource,
    Resume, disclose, recheck, resume,
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Once, time::Duration};
use tokio::sync::watch;

const BATCH: i64 = 100;
const RECHECK: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(250);

pub struct Notifier {
    latest: watch::Sender<i64>,
    started: Once,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            latest: watch::channel(0).0,
            started: Once::new(),
        }
    }
}

impl Notifier {
    fn subscribe(&self, app: &App) -> watch::Receiver<i64> {
        self.started.call_once(|| {
            let app = app.clone();
            tokio::spawn(async move {
                while !app
                    .health
                    .shutting_down
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    if let Ok(max) = sqlx::query_scalar::<_, i64>(
                        "SELECT coalesce(max(id),0) FROM change_events",
                    )
                    .fetch_one(&app.db)
                    .await
                    {
                        app.access.changes.latest.send_if_modified(|v| {
                            let changed = *v != max;
                            *v = max;
                            changed
                        });
                    }
                    tokio::time::sleep(POLL).await;
                }
            });
        });
        self.latest.subscribe()
    }
}

/// Newest change position and restore epoch, read in the caller's transaction.
pub async fn snapshot(conn: &mut sqlx::SqliteConnection) -> Result<(i64, String), Problem> {
    Ok(sqlx::query_as(
        "SELECT (SELECT coalesce(max(id),0) FROM change_events),restore_epoch FROM server_identity WHERE singleton=1",
    )
    .fetch_one(conn)
    .await?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct After {
    after: Option<String>,
}

#[derive(Serialize)]
struct Hint<'a> {
    cursor: String,
    kind: &'a str,
    resource_type: &'a str,
    resource_id: Option<&'a str>,
    resource_revision: Option<String>,
    policy_revision: String,
    reason: Option<&'a str>,
}

fn resource_type(topic: &str) -> &str {
    match topic {
        "libraries" => "library",
        "catalog" => "catalog_item",
        "metadata" => "item_metadata",
        "artwork" => "item_artwork",
        "profiles" => "profile",
        "viewing" => "profile_viewing",
        "devices" => "device",
        "schedules" => "scan_schedule",
        "organization" => "organization",
        "queues" => "profile_queue",
        "matches" => "match",
        other => other,
    }
}

/// Facts needed to authorize one hint. Library membership is read only for
/// principals whose visibility depends on it. Catalog hints name items (file
/// triggers are normalized to their item by migration 0011); a deleted item
/// has no remaining membership.
async fn classify(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    row: &Row,
) -> Result<Resource, sqlx::Error> {
    let (_, topic, id, _, scope) = row;
    let prior = scope.iter().cloned().collect();
    Ok(match topic.as_str() {
        "libraries" => Resource::Library(id.clone()),
        "catalog" | "metadata" | "artwork" if !principal.is_admin() => Resource::Item {
            libraries: sqlx::query_scalar(
                "SELECT DISTINCT f.library_id FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=?",
            )
            .bind(id)
            .fetch_all(conn)
            .await?
            .into_iter()
            .collect(),
            prior,
        },
        "catalog" | "metadata" | "artwork" => Resource::Item {
            libraries: Default::default(),
            prior,
        },
        "profiles" | "viewing" | "queues" => Resource::Profile(id.clone()),
        // Collections, saved filters and playlists belong to a profile.
        "organization" => {
            let owner: Option<String> = sqlx::query_scalar(
                "SELECT profile_id FROM collections WHERE id=?1 UNION ALL SELECT profile_id FROM saved_filters WHERE id=?1 UNION ALL SELECT profile_id FROM playlists WHERE id=?1 LIMIT 1",
            )
            .bind(id)
            .fetch_optional(conn)
            .await?;
            owner.map_or(Resource::Administrative, Resource::Profile)
        }
        "devices" => Resource::Device(id.clone()),
        _ => Resource::Administrative,
    })
}

async fn reauthenticate(app: &App, caller: &Caller) -> Result<Principal, AccessError> {
    let Some((hash, _)) = &caller.credential else {
        let Some(device_id) = &caller.ingress else {
            return Ok(Principal::operator());
        };
        let mut conn = app
            .db
            .acquire()
            .await
            .map_err(|_| AccessError::Unauthenticated)?;
        let device = auth::load_device(&mut conn, device_id)
            .await
            .map_err(|_| AccessError::Unauthenticated)?;
        return playscale_core::access::authenticate_ingress(device.as_ref().map(|d| &d.device));
    };
    let now = crate::now();
    let mut conn = app
        .db
        .acquire()
        .await
        .map_err(|_| AccessError::Unauthenticated)?;
    let stored = auth::load_credential(&mut conn, hash)
        .await
        .map_err(|_| AccessError::Unauthenticated)?
        .ok_or(AccessError::CredentialRevoked)?;
    match auth::authenticate_stored(&mut conn, &stored, now).await {
        Ok((principal, _)) => Ok(principal),
        Err(_) => Err(AccessError::CredentialRevoked),
    }
}

fn reset(epoch: &str, principal: &Principal, position: i64, reason: ResetReason) -> Event {
    let cursor = EventCursor {
        epoch: epoch.into(),
        principal: principal.id.clone(),
        policy_revision: principal.policy_revision,
        position,
    }
    .encode();
    let hint = Hint {
        cursor: cursor.clone(),
        kind: "reset",
        resource_type: "stream",
        resource_id: None,
        resource_revision: None,
        policy_revision: principal.policy_revision.to_string(),
        reason: Some(reason.as_str()),
    };
    Event::default()
        .event("reset")
        .id(cursor)
        .data(serde_json::to_string(&hint).unwrap())
}

/// id, topic, resource_id, kind, scope_library
type Row = (i64, String, String, String, Option<String>);
type Batch = (i64, i64, String, Vec<Row>);

async fn batch(app: &App, after: i64) -> Result<Batch, sqlx::Error> {
    let mut tx = app.db.begin().await?;
    let (oldest, newest): (i64, i64) =
        sqlx::query_as("SELECT coalesce(min(id),0),coalesce(max(id),0) FROM change_events")
            .fetch_one(&mut *tx)
            .await?;
    let epoch: String =
        sqlx::query_scalar("SELECT restore_epoch FROM server_identity WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    let rows = sqlx::query_as(
        "SELECT id,topic,resource_id,kind,scope_library FROM change_events WHERE id>? ORDER BY id LIMIT ?",
    )
    .bind(after)
    .bind(BATCH)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((oldest, newest, epoch, rows))
}

pub async fn subscribe(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<After>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>>, Problem> {
    caller.require(Permission::EventsRead)?;
    // Last-Event-ID takes precedence over `after`.
    let requested = match headers.get("last-event-id") {
        Some(h) => Some(
            h.to_str()
                .map_err(|_| Problem::from(AccessError::InvalidCursor))?
                .to_owned(),
        ),
        None => q.after,
    };
    let requested = requested.map(|c| EventCursor::parse(&c)).transpose()?;
    let permit = app.event_streams.clone().try_acquire_owned().map_err(|_| {
        let mut problem = Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "event_limit",
            "Too many event subscribers",
        );
        problem.retry_after = Some(5);
        problem
    })?;
    let (oldest, newest, epoch, _) = batch(&app, i64::MAX).await?;
    let start = resume(
        requested.as_ref(),
        &epoch,
        &caller.principal,
        oldest,
        newest,
    );
    let mut wake = app.access.changes.subscribe(&app);
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut principal = caller.principal.clone();
        let mut epoch = epoch;
        let mut position = match start {
            Resume::Continue(position) => position,
            Resume::Reset { position, reason } => {
                yield Ok(reset(&epoch, &principal, position, reason));
                position
            }
        };
        'stream: loop {
            if app.health.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match recheck(principal.policy_revision, &reauthenticate(&app, &caller).await) {
                Recheck::Close(_) => break,
                Recheck::Reset { .. } => {
                    let Ok(current) = reauthenticate(&app, &caller).await else { break };
                    principal = current;
                    let Ok((_, newest, e, _)) = batch(&app, i64::MAX).await else { break };
                    epoch = e;
                    position = newest;
                    yield Ok(reset(&epoch, &principal, position, ResetReason::PolicyChanged));
                    continue;
                }
                Recheck::Continue => {}
            }
            let Ok((oldest, newest, current_epoch, rows)) = batch(&app, position).await else { break };
            if current_epoch != epoch {
                epoch = current_epoch;
                position = newest;
                yield Ok(reset(&epoch, &principal, position, ResetReason::RestoreEpoch));
                continue;
            }
            if playscale_core::events::reset_cursor(position, oldest, newest).is_some() {
                position = newest;
                yield Ok(reset(&epoch, &principal, position, ResetReason::CursorExpired));
                continue;
            }
            let full = rows.len() as i64 == BATCH;
            for row in rows {
                // The client may have left the stream suspended since the
                // previous hint: authorize the principal and decide
                // disclosure from current facts immediately before each
                // hint, holding no connection across the yield.
                if recheck(principal.policy_revision, &reauthenticate(&app, &caller).await)
                    != Recheck::Continue
                {
                    continue 'stream;
                }
                let disclosure = {
                    let Ok(mut conn) = app.db.acquire().await else { break 'stream };
                    let Ok(resource) = classify(&mut conn, &principal, &row).await else { break 'stream };
                    disclose(&principal, &resource)
                };
                let (id, topic, resource_id, kind, _) = row;
                position = id;
                match disclosure {
                    Disclosure::Withhold => continue,
                    Disclosure::Reset => {
                        yield Ok(reset(&epoch, &principal, position, ResetReason::ScopeUnknown));
                        continue;
                    }
                    Disclosure::Deliver => {}
                }
                let cursor = EventCursor {
                    epoch: epoch.clone(),
                    principal: principal.id.clone(),
                    policy_revision: principal.policy_revision,
                    position: id,
                }
                .encode();
                let hint = Hint {
                    cursor: cursor.clone(),
                    kind: if kind == "deleted" { "deleted" } else { "changed" },
                    resource_type: resource_type(&topic),
                    resource_id: Some(&resource_id),
                    resource_revision: None,
                    policy_revision: principal.policy_revision.to_string(),
                    reason: None,
                };
                yield Ok(Event::default()
                    .event(hint.kind)
                    .id(cursor)
                    .data(serde_json::to_string(&hint).unwrap()));
            }
            if full {
                continue;
            }
            let _ = tokio::time::timeout(RECHECK, wake.changed()).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
