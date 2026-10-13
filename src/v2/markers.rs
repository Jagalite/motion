//! Public timeline markers: chapters and intro/credits/recap skip markers.
//! Validation and the effective view are decided by `playscale_core::markers`;
//! a manual marker is a correction that outranks embedded, imported and
//! detected ones (`Provenance::precedence`) without erasing them.
use super::{Body, PageQuery, Problem, Scope, auth::Caller, etag, idempotency_key, page};
use crate::{App, db::begin_write, new_id, now};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::access::{self, Idempotent, Permission};
use playscale_core::markers::{self as core, Kind, Marker, MarkerError, Provenance};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn routes() -> Router<App> {
    Router::new().route(
        "/catalog/timelines/{timeline_id}/markers",
        get(list).post(create),
    )
}

#[derive(Serialize)]
struct MarkerBody {
    id: String,
    revision: String,
    timeline_id: String,
    timeline_revision: String,
    kind: Kind,
    start_ms: u64,
    end_ms: Option<u64>,
    label: Option<String>,
    provenance: Provenance,
}

fn body(timeline: &str, m: Marker, revision: u64) -> MarkerBody {
    MarkerBody {
        id: m.id,
        revision: revision.to_string(),
        timeline_id: timeline.to_owned(),
        timeline_revision: m.timeline_revision.to_string(),
        kind: m.kind,
        start_ms: m.start_ms,
        end_ms: m.end_ms,
        label: m.label,
        provenance: m.provenance,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    /// Only the markers in effect for the timeline's current revision
    /// (`core::effective`); otherwise every marker, current or not.
    #[serde(default)]
    effective: bool,
}

/// Markers of a readable timeline, ordered by start then ID. Each says the
/// timeline revision it was validated against and its provenance, so a
/// client can show stale or detected (uncertain) markers as such.
async fn list(
    State(app): State<App>,
    caller: Caller,
    Path(timeline): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Json<super::Page<MarkerBody>>, Problem> {
    caller.require(Permission::CatalogRead)?;
    let q = PageQuery {
        cursor: query.cursor,
        limit: query.limit,
    };
    let limit = q.limit()?;
    let after = q.after()?.to_owned();
    let mut tx = app.db.begin().await?;
    super::playback::readable_timeline(&mut tx, &caller.principal, &timeline).await?;
    let current: i64 = sqlx::query_scalar("SELECT revision FROM timelines WHERE id=?")
        .bind(&timeline)
        .fetch_one(&mut *tx)
        .await?;
    let markers = load(&mut tx, &timeline).await?;
    let mut markers = if query.effective {
        core::effective(
            &markers.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>(),
            current as u64,
        )
        .into_iter()
        .map(|m| {
            let revision = markers
                .iter()
                .find(|(x, _)| x.id == m.id)
                .map_or(1, |(_, r)| *r);
            (m, revision)
        })
        .collect()
    } else {
        markers
    };
    markers.sort_by(|(a, _), (b, _)| (a.start_ms, &a.id).cmp(&(b.start_ms, &b.id)));
    // Keyset over (start, id), encoded as "<start>:<id>".
    let after = match after.as_str() {
        "" => None,
        cursor => Some(
            cursor
                .split_once(':')
                .and_then(|(s, id)| Some((s.parse::<u64>().ok()?, id.to_owned())))
                .ok_or_else(|| Problem::bad("invalid_cursor", "The page cursor is invalid"))?,
        ),
    };
    let rows: Vec<(String, MarkerBody)> = markers
        .into_iter()
        .filter(|(m, _)| {
            after
                .as_ref()
                .is_none_or(|(s, id)| (m.start_ms, &m.id) > (*s, id))
        })
        .take(limit as usize + 1)
        .map(|(m, revision)| {
            (
                format!("{}:{}", m.start_ms, m.id),
                body(&timeline, m, revision),
            )
        })
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Every stored marker of the timeline with its own revision.
async fn load(
    conn: &mut sqlx::SqliteConnection,
    timeline: &str,
) -> Result<Vec<(Marker, u64)>, Problem> {
    type Row = (
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
        String,
        i64,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id,kind,start_ms,end_ms,label,provenance,timeline_revision,revision FROM markers WHERE timeline_id=? ORDER BY start_ms,id LIMIT ?",
    )
    .bind(timeline)
    .bind(core::MAX_MARKERS as i64 + 1)
    .fetch_all(conn)
    .await?;
    // Creation keeps a timeline within the bound, so every marker is read
    // and the effective view sees all of them; never a silent prefix.
    if rows.len() > core::MAX_MARKERS {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "marker_limit",
            "This timeline holds more markers than can be read at once",
        ));
    }
    rows.into_iter()
        .map(
            |(id, kind, start, end, label, provenance, recorded, revision)| {
                Ok((
                    Marker {
                        id,
                        kind: serde_json::from_value(Value::String(kind))
                            .map_err(Problem::internal)?,
                        start_ms: start.max(0) as u64,
                        end_ms: end.map(|e| e.max(0) as u64),
                        label,
                        provenance: serde_json::from_value(Value::String(provenance))
                            .map_err(Problem::internal)?,
                        timeline_revision: recorded.max(0) as u64,
                    },
                    revision.max(1) as u64,
                ))
            },
        )
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkerInput {
    timeline_revision: String,
    kind: Kind,
    start_ms: u64,
    end_ms: Option<u64>,
    label: Option<String>,
}

fn rejected(e: MarkerError) -> Problem {
    match e {
        MarkerError::StaleTimeline => Problem::new(
            StatusCode::CONFLICT,
            "timeline_changed",
            "The timeline changed; read it again",
        ),
        MarkerError::InvalidInterval => Problem::invalid(
            "invalid_interval",
            "Intro, credits and recap markers need an end after their start",
        ),
        MarkerError::OutsideTimeline => {
            Problem::invalid("outside_timeline", "The marker lies outside the timeline")
        }
        MarkerError::InvalidLabel => {
            Problem::invalid("invalid_label", "Labels have 1-200 characters")
        }
        MarkerError::TooManyMarkers => Problem::new(
            StatusCode::CONFLICT,
            "marker_limit",
            "This timeline already holds the most markers it can",
        ),
    }
}

/// Add a manual marker against the timeline revision the caller reviewed.
async fn create(
    State(app): State<App>,
    caller: Caller,
    Path(timeline): Path<String>,
    headers: HeaderMap,
    body_in: Body<MarkerInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let key = idempotency_key(&headers)?;
    let input = body_in.value;
    let expected = super::viewing::decimal(&input.timeline_revision).ok_or_else(|| {
        Problem::invalid(
            "invalid_body",
            "timeline_revision must be a decimal revision",
        )
    })?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let row = super::playback::readable_timeline(&mut tx, &principal, &timeline).await?;
    // Changing a title's markers is curation: the whole work must be in scope.
    super::catalog::curatable_item(
        &mut tx,
        &access::catalog_scope(&principal),
        &row.item_id,
        false,
    )
    .await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createMarker",
        target: &timeline,
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body_in.digest, now())?
        == Idempotent::Replay
        && let Some(stored) = stored
    {
        let value = stored.body.unwrap_or_default();
        let mut response = (stored.status, Json(value)).into_response();
        response.headers_mut().insert(header::ETAG, etag(1));
        return Ok(response);
    }
    let (revision, duration): (i64, Option<i64>) =
        sqlx::query_as("SELECT revision,duration_ms FROM timelines WHERE id=?")
            .bind(&timeline)
            .fetch_one(&mut *tx)
            .await?;
    let label = input.label.as_deref().map(str::trim);
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM markers WHERE timeline_id=?")
        .bind(&timeline)
        .fetch_one(&mut *tx)
        .await?;
    core::admits_another(existing.max(0) as usize).map_err(rejected)?;
    core::validate(
        input.kind,
        input.start_ms,
        input.end_ms,
        input.label.as_deref(),
        expected,
        revision as u64,
        duration.map(|d| d.max(0) as u64),
    )
    .map_err(rejected)?;
    let id = new_id();
    sqlx::query("INSERT INTO markers(id,timeline_id,timeline_revision,kind,start_ms,end_ms,label,provenance) VALUES (?,?,?,?,?,?,?,'manual')")
        .bind(&id)
        .bind(&timeline)
        .bind(revision)
        .bind(serde_json::to_value(input.kind).map_err(Problem::internal)?.as_str())
        .bind(input.start_ms as i64)
        .bind(input.end_ms.map(|e| e as i64))
        .bind(label)
        .execute(&mut *tx)
        .await?;
    let created = body(
        &timeline,
        Marker {
            id,
            kind: input.kind,
            start_ms: input.start_ms,
            end_ms: input.end_ms,
            label: label.map(str::to_owned),
            provenance: Provenance::Manual,
            timeline_revision: revision as u64,
        },
        1,
    );
    let json = serde_json::to_value(&created).map_err(Problem::internal)?;
    scope
        .save(
            &mut tx,
            &body_in.digest,
            StatusCode::CREATED,
            Some(&json),
            None,
        )
        .await?;
    tx.commit().await?;
    let mut response = (StatusCode::CREATED, Json(json)).into_response();
    response.headers_mut().insert(header::ETAG, etag(1));
    Ok(response)
}
