//! `/api/v2` timeline viewing: state, manual overrides, ordered viewing
//! sessions, continue-watching and next-in-order.
//!
//! Decisions live in `playscale_core::timeline_viewing` (authority, ordering,
//! idempotency, manual epochs, completion) and `playscale_core::playback_session`
//! (who may view through which delivery). This adapter loads exactly the facts
//! those decisions need and commits each decided change, its acknowledgement
//! and any superseded session in one SQLite write transaction.
use super::{
    Body, Page, PageQuery, Problem, Scope,
    auth::Caller,
    catalog::{self, TimelineRow},
    content::file_facts,
    etag, idempotency_key, if_match, page,
    playback::{readable_timeline, usable_profile},
};
use crate::{App, db::begin_write, delivery, new_id, now};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use playscale_core::{
    access::{self, AccessError, Idempotent, Permission, Principal},
    playback_session as session_core,
    timeline_viewing::{self as core, Error, Event, Recorded, Session, Status, View},
};
use serde::Deserialize;
use serde_json::{Value, json};

impl From<Error> for Problem {
    fn from(e: Error) -> Self {
        let conflict = |code, detail| Problem::new(StatusCode::CONFLICT, code, detail);
        match e {
            Error::RevisionConflict => AccessError::StaleRevision.into(),
            Error::Superseded => conflict(
                "superseded_session",
                "Another session or a manual change took over this timeline; create a new session",
            ),
            Error::Closed => conflict(
                "closed_session",
                "The session ended or stopped; create a new session",
            ),
            Error::SequenceGap => conflict(
                "sequence_gap",
                "Send the next consecutive sequence; read the session for its current sequence",
            ),
            Error::StaleSequence => conflict(
                "stale_sequence",
                "The sequence was already used; read the session for its current sequence",
            ),
            Error::EventConflict => conflict(
                "event_conflict",
                "The sequence or event ID was already used with different content",
            ),
            Error::InvalidEvent => Problem::invalid("invalid_event", "The event is not valid"),
            Error::InvalidPosition => Problem::invalid(
                "invalid_position",
                "The position is outside the delivery's timeline",
            ),
            Error::Exhausted => conflict("revision_exhausted", "The record cannot change further"),
        }
    }
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Playing => "playing",
        Status::Paused => "paused",
        Status::Ended => "ended",
        Status::Stopped => "stopped",
        Status::Superseded => "superseded",
    }
}

fn status_of(s: &str) -> Option<Status> {
    Some(match s {
        "playing" => Status::Playing,
        "paused" => Status::Paused,
        "ended" => Status::Ended,
        "stopped" => Status::Stopped,
        "superseded" => Status::Superseded,
        _ => return None,
    })
}

fn int(v: u64) -> i64 {
    // Core revisions and positions stay below 2^53.
    i64::try_from(v).expect("bounded value")
}

fn uint(v: i64) -> Result<u64, Problem> {
    u64::try_from(v).map_err(Problem::internal)
}

/// revision, manual_epoch, position_ms, automatic_watched, manual_watched, session_id
type ViewTuple = (i64, i64, i64, bool, Option<bool>, Option<String>);

pub(crate) async fn load_view(
    conn: &mut sqlx::SqliteConnection,
    profile: &str,
    timeline: &str,
) -> Result<View, Problem> {
    let row: Option<ViewTuple> = sqlx::query_as(
        "SELECT revision,manual_epoch,position_ms,automatic_watched,manual_watched,session_id FROM timeline_viewing WHERE profile_id=? AND timeline_id=?",
    )
    .bind(profile)
    .bind(timeline)
    .fetch_optional(conn)
    .await?;
    let Some((revision, manual_epoch, position_ms, automatic_watched, manual_watched, session)) =
        row
    else {
        return Ok(View::default());
    };
    Ok(View {
        revision: uint(revision)?,
        manual_epoch: uint(manual_epoch)?,
        position_ms: uint(position_ms)?,
        automatic_watched,
        manual_watched,
        session,
    })
}

async fn store_view(
    conn: &mut sqlx::SqliteConnection,
    profile: &str,
    timeline: &str,
    v: &View,
) -> Result<(), Problem> {
    sqlx::query(
        "INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,automatic_watched,manual_watched,session_id,updated_at) VALUES (?,?,?,?,?,?,?,?,?)
         ON CONFLICT(profile_id,timeline_id) DO UPDATE SET revision=excluded.revision,manual_epoch=excluded.manual_epoch,position_ms=excluded.position_ms,automatic_watched=excluded.automatic_watched,manual_watched=excluded.manual_watched,session_id=excluded.session_id,updated_at=excluded.updated_at",
    )
    .bind(profile)
    .bind(timeline)
    .bind(int(v.revision))
    .bind(int(v.manual_epoch))
    .bind(int(v.position_ms))
    .bind(v.automatic_watched)
    .bind(v.manual_watched)
    .bind(&v.session)
    .bind(now())
    .execute(conn)
    .await?;
    Ok(())
}

/// A stored session with the profile and timeline it belongs to.
pub(crate) struct SessionRow {
    pub session: Session,
    pub profile: String,
    pub timeline: String,
}

/// id, profile, timeline, delivery, revision, epoch, sequence, position, status, duration
type SessionTuple = (
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    String,
    Option<i64>,
);

async fn load_session(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<SessionRow>, Problem> {
    let row: Option<SessionTuple> = sqlx::query_as(
        "SELECT id,profile_id,timeline_id,delivery_id,revision,manual_epoch,sequence,position_ms,status,duration_ms FROM viewing_sessions WHERE id=?",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    let Some((
        id,
        profile,
        timeline,
        delivery,
        revision,
        epoch,
        sequence,
        position,
        status,
        duration,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(SessionRow {
        session: Session {
            id,
            revision: uint(revision)?,
            delivery,
            manual_epoch: uint(epoch)?,
            sequence: uint(sequence)?,
            position_ms: uint(position)?,
            status: status_of(&status)
                .ok_or_else(|| Problem::internal("unknown session status"))?,
            duration_ms: duration.map(uint).transpose()?,
        },
        profile,
        timeline,
    }))
}

async fn insert_session(
    conn: &mut sqlx::SqliteConnection,
    row: &SessionRow,
    principal: &str,
) -> Result<(), Problem> {
    let s = &row.session;
    sqlx::query("INSERT INTO viewing_sessions(id,profile_id,timeline_id,delivery_id,principal_id,revision,manual_epoch,sequence,position_ms,status,duration_ms,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&s.id)
        .bind(&row.profile)
        .bind(&row.timeline)
        .bind(&s.delivery)
        .bind(principal)
        .bind(int(s.revision))
        .bind(int(s.manual_epoch))
        .bind(int(s.sequence))
        .bind(int(s.position_ms))
        .bind(status_name(s.status))
        .bind(s.duration_ms.map(int))
        .bind(now())
        .bind(now())
        .execute(conn)
        .await?;
    Ok(())
}

/// Compare-and-set write of a decided session transition from `previous`.
async fn update_session(
    conn: &mut sqlx::SqliteConnection,
    previous: &Session,
    s: &Session,
) -> Result<(), Problem> {
    let updated = sqlx::query("UPDATE viewing_sessions SET delivery_id=?,revision=?,sequence=?,position_ms=?,status=?,duration_ms=?,updated_at=? WHERE id=? AND revision=?")
        .bind(&s.delivery)
        .bind(int(s.revision))
        .bind(int(s.sequence))
        .bind(int(s.position_ms))
        .bind(status_name(s.status))
        .bind(s.duration_ms.map(int))
        .bind(now())
        .bind(&s.id)
        .bind(int(previous.revision))
        .execute(conn)
        .await?;
    if updated.rows_affected() != 1 {
        // The writer lock serializes this transaction; a mismatch is a bug.
        return Err(Problem::internal(
            "viewing session changed under the writer",
        ));
    }
    Ok(())
}

/// Supersede the session that held the authority before (`core::supersede`).
async fn supersede(conn: &mut sqlx::SqliteConnection, id: Option<&str>) -> Result<(), Problem> {
    let Some(id) = id else { return Ok(()) };
    let Some(old) = load_session(conn, id).await? else {
        return Ok(());
    };
    let next = core::supersede(&old.session)?;
    if next != old.session {
        update_session(conn, &old.session, &next).await?;
    }
    Ok(())
}

pub(crate) fn view_json(profile: &str, timeline: &str, v: &View) -> Value {
    json!({
        "profile_id": profile,
        "timeline_id": timeline,
        "revision": v.revision.to_string(),
        "manual_epoch": v.manual_epoch.to_string(),
        "position_ms": v.position_ms,
        "watched": v.watched(),
        "manual_watched": v.manual_watched,
        "session_id": v.session,
    })
}

fn session_json(row: &SessionRow) -> Value {
    let s = &row.session;
    json!({
        "id": s.id,
        "revision": s.revision.to_string(),
        "profile_id": row.profile,
        "timeline_id": row.timeline,
        "delivery_id": s.delivery,
        "sequence": s.sequence.to_string(),
        "manual_epoch": s.manual_epoch.to_string(),
        "position_ms": s.position_ms,
        "status": status_name(s.status),
    })
}

fn tagged(status: StatusCode, body: Value, revision: u64) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, etag(revision));
    response
}

/// Completion threshold from the profile's preferences (default 90 %).
async fn completion_percent(
    conn: &mut sqlx::SqliteConnection,
    profile: &str,
) -> Result<f64, Problem> {
    let document: Option<String> =
        sqlx::query_scalar("SELECT document_json FROM playback_preferences WHERE profile_id=?")
            .bind(profile)
            .fetch_optional(conn)
            .await?;
    Ok(document
        .and_then(|d| serde_json::from_str::<crate::viewing::Preferences>(&d).ok())
        .map(|p| p.completion_percent)
        .filter(|p| (50.0..=100.0).contains(p))
        .unwrap_or(90.0))
}

/// Profile and timeline facts for viewing writes and reads, observed in the
/// caller's transaction: the profile is usable and the timeline readable.
async fn admit_view(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    profile: &str,
    timeline: &str,
) -> Result<TimelineRow, Problem> {
    usable_profile(conn, principal, profile).await?;
    readable_timeline(conn, principal, timeline).await
}

// ------------------------------------------------------------ viewing state

pub async fn get_viewing(
    State(app): State<App>,
    caller: Caller,
    Path((profile, timeline)): Path<(String, String)>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let mut tx = app.db.begin().await?;
    admit_view(&mut tx, &caller.principal, &profile, &timeline).await?;
    let view = load_view(&mut tx, &profile, &timeline).await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        view_json(&profile, &timeline, &view),
        view.revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchedInput {
    manual_watched: Option<bool>,
    reset_position: bool,
}

pub async fn put_viewing(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    Path((profile, timeline)): Path<(String, String)>,
    body: Body<WatchedInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ViewingWrite))
        .await?;
    admit_view(&mut tx, &principal, &profile, &timeline).await?;
    let view = load_view(&mut tx, &profile, &timeline).await?;
    let o = core::override_watched(
        &view,
        expected,
        body.value.manual_watched,
        body.value.reset_position,
    )?;
    store_view(&mut tx, &profile, &timeline, &o.view).await?;
    supersede(&mut tx, o.fenced.as_deref()).await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        view_json(&profile, &timeline, &o.view),
        o.view.revision,
    ))
}

// -------------------------------------------------------- continue watching

/// `{updated_at}.{timeline_id}` keyset over the newest-first list.
fn keyset(q: &PageQuery) -> Result<Option<(i64, String)>, Problem> {
    let after = q.after()?;
    if after.is_empty() {
        return Ok(None);
    }
    after
        .split_once('.')
        .and_then(|(at, id)| Some((at.parse().ok()?, id.to_owned())))
        .filter(|(_, id)| !id.is_empty())
        .map(Some)
        .ok_or_else(|| Problem::bad("invalid_cursor", "The page cursor is invalid"))
}

pub async fn continue_watching(
    State(app): State<App>,
    caller: Caller,
    Path(profile): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let limit = q.limit()?;
    let after = keyset(&q)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    usable_profile(&mut tx, &caller.principal, &profile).await?;
    let (clause, all, ids) = super::scope_clause(&scope, "f.library_id");
    // Resumable (positively progressed, effectively unwatched) timelines whose
    // work has an original in scope, newest first. Scope is applied before
    // paging; item-level policy (ratings) is rechecked per row below.
    let rows: Vec<(String, i64)> = sqlx::query_as(&format!(
        "SELECT v.timeline_id,v.updated_at FROM timeline_viewing v JOIN timelines t ON t.id=v.timeline_id JOIN editions e ON e.id=t.edition_id \
         WHERE v.profile_id=? AND v.position_ms>0 AND coalesce(v.manual_watched,v.automatic_watched)=0 \
         AND (? IS NULL OR v.updated_at<? OR (v.updated_at=? AND v.timeline_id>?)) \
         AND EXISTS(SELECT 1 FROM media_files f JOIN editions fe ON fe.id=f.edition_id WHERE fe.item_id=e.item_id AND f.generated=0 AND {clause}) \
         ORDER BY v.updated_at DESC,v.timeline_id LIMIT ?"
    ))
    .bind(&profile)
    .bind(after.as_ref().map(|a| a.0))
    .bind(after.as_ref().map(|a| a.0))
    .bind(after.as_ref().map(|a| a.0))
    .bind(after.as_ref().map(|a| a.1.clone()))
    .bind(all)
    .bind(ids)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let mut entries = Vec::new();
    for (timeline_id, updated_at) in rows {
        let row: TimelineRow = sqlx::query_as(
            "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
        )
        .bind(&timeline_id)
        .fetch_one(&mut *tx)
        .await?;
        let Ok((item_row, _)) = catalog::load_item(&mut tx, &scope, &row.item_id, false).await
        else {
            continue;
        };
        let view = load_view(&mut tx, &profile, &timeline_id).await?;
        let item = catalog::item_body(&mut tx, &scope, item_row).await?;
        let timeline = catalog::timeline_body(&mut tx, &scope, row).await?;
        entries.push((
            format!("{updated_at}.{timeline_id}"),
            json!({"item": item, "timeline": timeline, "viewing": view_json(&profile, &timeline_id, &view)}),
        ));
    }
    let value: Page<Value> = page(&mut tx, &caller.principal, entries, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

// ------------------------------------------------------------ next timeline

/// The next timeline in the current one's release order: the same order group
/// (which belongs to one edition) with the smallest greater position that the
/// caller may read. Unplaced timelines have no next; nothing is inferred from
/// season/episode numbers, and editions are never crossed.
pub async fn next_timeline(
    State(app): State<App>,
    caller: Caller,
    Path((profile, timeline)): Path<(String, String)>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let current = admit_view(&mut tx, &caller.principal, &profile, &timeline).await?;
    let placed: Option<(Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT order_group_id,order_position FROM timelines WHERE id=?")
            .bind(&current.id)
            .fetch_optional(&mut *tx)
            .await?;
    let mut values = Vec::new();
    if let Some((Some(group), Some(position))) = placed {
        let candidates: Vec<String> = sqlx::query_scalar(
            // The group is one release (PR #4: order_groups.edition_id is the
            // series release); its members are episode timelines of other
            // works, so they are not constrained to the current edition row.
            "SELECT id FROM timelines WHERE order_group_id=? AND order_position>? ORDER BY order_position,id LIMIT 50",
        )
        .bind(&group)
        .bind(position)
        .fetch_all(&mut *tx)
        .await?;
        for id in candidates {
            let Ok(row) = readable_timeline(&mut tx, &caller.principal, &id).await else {
                continue;
            };
            values.push((id, catalog::timeline_body(&mut tx, &scope, row).await?));
            break;
        }
    }
    let value = page(&mut tx, &caller.principal, values, 1).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

// --------------------------------------------------------- viewing sessions

/// A live, unfenced v2 delivery and its owner.
fn open_delivery(app: &App, id: &str) -> Option<(session_core::Owner, Option<u64>)> {
    let owner = delivery::live_owner(app, id)?;
    let view = delivery::live_view(app, id)?;
    let fenced = matches!(
        view.status.as_str(),
        "closing" | "closed" | "failed" | "interrupted"
    );
    (!fenced).then_some((owner, view.logical_duration_ms))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewingCreate {
    delivery_id: String,
    expected_viewing_revision: String,
}

fn decimal(value: &str) -> Option<u64> {
    (!value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0')))
    .then(|| value.parse().ok())
    .flatten()
}

fn replayed(stored: super::Stored) -> Response {
    let mut response = (stored.status, Json(stored.body.unwrap_or_default())).into_response();
    response
        .headers_mut()
        .insert("idempotent-replayed", HeaderValue::from_static("true"));
    response
}

pub async fn create_viewing(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<ViewingCreate>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    let expected = decimal(&input.expected_viewing_revision)
        .ok_or_else(|| Problem::invalid("invalid_revision", "Invalid viewing revision"))?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ViewingWrite))
        .await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createViewingSession",
        target: "-",
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        // The acknowledgement names a session: replay it only while the
        // caller may still read and record into that session.
        let stored = stored.expect("replay has a record");
        let id = stored
            .body
            .as_ref()
            .and_then(|b| b["id"].as_str())
            .ok_or_else(|| Problem::internal("viewing receipt without a session"))?
            .to_owned();
        recordable(&mut tx, &principal, &id).await?;
        return Ok(replayed(stored));
    }
    let (owner, duration) =
        open_delivery(&app, &input.delivery_id).ok_or_else(Problem::not_found)?;
    let facts = file_facts(&mut tx, &owner.file).await?;
    if !session_core::may_view(&owner, &principal, facts.as_ref()) {
        return Err(Problem::not_found());
    }
    admit_view(&mut tx, &principal, &owner.profile, &owner.timeline).await?;
    let view = load_view(&mut tx, &owner.profile, &owner.timeline).await?;
    let started = core::start(
        &view,
        expected,
        new_id(),
        input.delivery_id.clone(),
        duration,
    )
    .map_err(|e| match e {
        // A stale expected revision is a conflict, not a precondition.
        Error::RevisionConflict => Problem::new(
            StatusCode::CONFLICT,
            "viewing_revision_conflict",
            "Viewing changed; read it again before starting a session",
        ),
        e => e.into(),
    })?;
    // The view must exist before the session that references it.
    store_view(&mut tx, &owner.profile, &owner.timeline, &started.view).await?;
    supersede(&mut tx, started.superseded.as_deref()).await?;
    let row = SessionRow {
        session: started.session,
        profile: owner.profile.clone(),
        timeline: owner.timeline.clone(),
    };
    insert_session(&mut tx, &row, &principal.id).await?;
    let ack = session_json(&row);
    scope
        .save(&mut tx, &body.digest, StatusCode::CREATED, Some(&ack), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, ack, row.session.revision))
}

/// A session the principal may read or record into: viewing write, its
/// profile, and its timeline readable now. Anything else is not found.
async fn recordable(
    conn: &mut sqlx::SqliteConnection,
    principal: &Principal,
    id: &str,
) -> Result<SessionRow, Problem> {
    if !principal.allows(Permission::ViewingWrite) {
        return Err(AccessError::Forbidden(Permission::ViewingWrite).into());
    }
    let row = load_session(conn, id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let readable = match readable_timeline(conn, principal, &row.timeline).await {
        Ok(_) => true,
        Err(p) if p.status == StatusCode::NOT_FOUND => false,
        Err(p) => return Err(p),
    };
    if !session_core::may_record(principal, &row.profile, readable) {
        return Err(Problem::not_found());
    }
    Ok(row)
}

pub async fn get_viewing_session(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    let row = recordable(&mut tx, &caller.principal, &id).await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        session_json(&row),
        row.session.revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewingEvent {
    event_id: String,
    sequence: String,
    delivery_generation: String,
    position_ms: u64,
    status: String,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// sequence, event_id, delivery_generation, position_ms, status, acknowledgement_json
type EventTuple = (i64, String, i64, i64, String, String);

pub async fn record_viewing(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    body: Body<ViewingEvent>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let input = body.value;
    let invalid = || Problem::invalid("invalid_event", "The event is not valid");
    let event = Event {
        id: Some(input.event_id)
            .filter(|i| valid_id(i))
            .ok_or_else(invalid)?,
        sequence: decimal(&input.sequence).ok_or_else(invalid)?,
        generation: decimal(&input.delivery_generation).ok_or_else(invalid)?,
        position_ms: input.position_ms,
        status: status_of(&input.status)
            .filter(|s| s.reportable())
            .ok_or_else(invalid)?,
    };
    if event.sequence >= 1 << 53 || event.generation >= 1 << 53 {
        return Err(invalid());
    }
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ViewingWrite))
        .await?;
    let row = recordable(&mut tx, &principal, &id).await?;
    let receipts: Vec<EventTuple> = sqlx::query_as(
        "SELECT sequence,event_id,delivery_generation,position_ms,status,acknowledgement_json FROM viewing_events WHERE session_id=? AND (sequence=? OR event_id=?)",
    )
    .bind(&id)
    .bind(int(event.sequence))
    .bind(&event.id)
    .fetch_all(&mut *tx)
    .await?;
    let mut prior = Vec::with_capacity(receipts.len());
    for (sequence, event_id, generation, position, status, _) in &receipts {
        prior.push(Event {
            id: event_id.clone(),
            sequence: uint(*sequence)?,
            generation: uint(*generation)?,
            position_ms: uint(*position)?,
            status: status_of(status).ok_or_else(|| Problem::internal("unknown event status"))?,
        });
    }
    let view = load_view(&mut tx, &row.profile, &row.timeline).await?;
    let completion = completion_percent(&mut tx, &row.profile).await?;
    match core::record(&view, &row.session, &event, &prior, completion)? {
        Recorded::Duplicate => {
            tx.commit().await?;
            let (_, _, _, _, _, ack) = receipts
                .into_iter()
                .find(|r| r.0 == int(event.sequence))
                .ok_or_else(|| Problem::internal("duplicate without its receipt"))?;
            let mut ack: Value = serde_json::from_str(&ack).map_err(Problem::internal)?;
            ack["duplicate"] = true.into();
            let revision = ack["session"]["revision"]
                .as_str()
                .and_then(|r| r.parse().ok())
                .unwrap_or_default();
            Ok(tagged(StatusCode::OK, ack, revision))
        }
        Recorded::Accepted { view, session } => {
            store_view(&mut tx, &row.profile, &row.timeline, &view).await?;
            update_session(&mut tx, &row.session, &session).await?;
            let next = SessionRow { session, ..row };
            let ack = json!({
                "session": session_json(&next),
                "viewing": view_json(&next.profile, &next.timeline, &view),
                "duplicate": false,
            });
            sqlx::query("INSERT INTO viewing_events(session_id,sequence,event_id,delivery_generation,position_ms,status,acknowledgement_json,created_at) VALUES (?,?,?,?,?,?,?,?)")
                .bind(&id)
                .bind(int(event.sequence))
                .bind(&event.id)
                .bind(int(event.generation))
                .bind(int(event.position_ms))
                .bind(status_name(event.status))
                .bind(ack.to_string())
                .bind(now())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(tagged(StatusCode::OK, ack, next.session.revision))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewingDeliveryInput {
    delivery_id: String,
}

/// Rebind an authoritative session to another live delivery of the same
/// profile and timeline that the caller may view through.
pub async fn replace_viewing_delivery(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body<ViewingDeliveryInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let expected = if_match(&headers)?;
    let delivery_id = body.value.delivery_id;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ViewingWrite))
        .await?;
    let row = recordable(&mut tx, &principal, &id).await?;
    let (owner, duration) = open_delivery(&app, &delivery_id).ok_or_else(Problem::not_found)?;
    let facts = file_facts(&mut tx, &owner.file).await?;
    if !session_core::may_view(&owner, &principal, facts.as_ref()) {
        return Err(Problem::not_found());
    }
    if owner.profile != row.profile || owner.timeline != row.timeline {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "delivery_mismatch",
            "The delivery presents another profile or timeline; create a new session",
        ));
    }
    let view = load_view(&mut tx, &row.profile, &row.timeline).await?;
    let session = core::rebind(&view, &row.session, expected, delivery_id, duration)?;
    update_session(&mut tx, &row.session, &session).await?;
    tx.commit().await?;
    let next = SessionRow { session, ..row };
    Ok(tagged(
        StatusCode::OK,
        session_json(&next),
        next.session.revision,
    ))
}

/// Remove a profile's timeline viewing records (inside the caller's transaction).
pub(crate) async fn delete_profile(
    conn: &mut sqlx::SqliteConnection,
    profile: &str,
) -> Result<(), Problem> {
    for statement in [
        "DELETE FROM viewing_events WHERE session_id IN (SELECT id FROM viewing_sessions WHERE profile_id=?)",
        "DELETE FROM viewing_sessions WHERE profile_id=?",
        "DELETE FROM timeline_viewing WHERE profile_id=?",
    ] {
        sqlx::query(statement)
            .bind(profile)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}
