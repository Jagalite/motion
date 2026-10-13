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
    let mut markers = load(&mut tx, &timeline).await?;
    markers.extend(
        embedded(&mut tx, &caller.principal, &timeline, current as u64)
            .await?
            .into_iter()
            .map(|m| (m, 1)),
    );
    // The effective set depends on every marker and the current revision, so
    // its pages are bound to the snapshot they were read from: a cursor names
    // that snapshot, and a change between pages is refused rather than
    // joining pages of two different effective sets.
    let snapshot = query.effective.then(|| snapshot(current, &markers));
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
    // Keyset over (start, id), encoded as "<start>:<id>"; for the effective
    // view prefixed with its snapshot as "<snapshot>.<start>:<id>".
    let after = match (after.as_str(), &snapshot) {
        ("", _) => None,
        (cursor, Some(snapshot)) => match cursor.split_once('.') {
            Some((read, rest)) if read == snapshot => Some(rest),
            Some(_) => {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "markers_changed",
                    "The markers changed since the first page; read them again",
                ));
            }
            None => {
                return Err(Problem::bad("invalid_cursor", "The page cursor is invalid"));
            }
        },
        (cursor, None) => Some(cursor),
    };
    let after = match after {
        None => None,
        Some(cursor) => Some(
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
                match &snapshot {
                    Some(snapshot) => format!("{snapshot}.{}:{}", m.start_ms, m.id),
                    None => format!("{}:{}", m.start_ms, m.id),
                },
                body(&timeline, m, revision),
            )
        })
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// The chapters the timeline's whole, single-file original records
/// (`markers::embedded_chapters`), at its current revision: file facts, so
/// they follow the timeline. Their identities name the file revision. Only
/// an original the caller may read is a source (as for playback), so a
/// copy in another library never discloses its chapters.
async fn embedded(
    conn: &mut sqlx::SqliteConnection,
    principal: &access::Principal,
    timeline: &str,
    revision: u64,
) -> Result<Vec<Marker>, Problem> {
    type Row = (String, String, Option<String>, Option<i64>, Option<f64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT f.id,f.revision,f.chapters_json,t.duration_ms,f.duration_seconds FROM timelines t JOIN media_versions v ON v.timeline_id=t.id \
         JOIN version_files b ON b.version_id=v.id AND b.part=1 JOIN media_files f ON f.id=b.file_id \
         WHERE t.id=? AND v.origin='original' AND f.generated=0 AND b.file_revision=f.revision \
         AND b.start_ms IS NULL AND b.end_ms IS NULL \
         AND NOT EXISTS(SELECT 1 FROM version_files o WHERE o.version_id=v.id AND o.part<>1) \
         AND f.library_id IN (SELECT id FROM libraries WHERE enabled=1) \
         AND f.chapters_json IS NOT NULL AND f.chapters_json<>'[]' ORDER BY v.id,f.id LIMIT 100",
    )
    .bind(timeline)
    .fetch_all(&mut *conn)
    .await?;
    for (file, file_revision, chapters, timeline_ms, file_seconds) in rows {
        if !playscale_core::playback_session::readable(
            principal,
            super::content::file_facts(conn, &file).await?.as_ref(),
        ) {
            continue;
        }
        let chapters: Vec<core::FileChapter> =
            serde_json::from_str(chapters.as_deref().unwrap_or("[]")).unwrap_or_default();
        // The timeline's duration, else the whole file's (scan-created
        // timelines record none), so chapters past the media's end drop.
        let duration = timeline_ms.and_then(|d| u64::try_from(d).ok()).or_else(|| {
            file_seconds
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| (s * 1000.0).floor() as u64)
        });
        return Ok(core::embedded_chapters(
            &chapters,
            duration,
            revision,
            &format!("embedded-{}", &file_revision[..file_revision.len().min(16)]),
        ));
    }
    Ok(vec![])
}

/// An identity of the facts the effective view is decided from: the
/// timeline revision and every marker with its revision.
fn snapshot(timeline_revision: i64, markers: &[(Marker, u64)]) -> String {
    use sha2::{Digest, Sha256};
    let mut ids: Vec<(&str, u64)> = markers.iter().map(|(m, r)| (m.id.as_str(), *r)).collect();
    ids.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(timeline_revision.to_string());
    for (id, revision) in ids {
        digest.update(format!("\n{id}/{revision}"));
    }
    format!("{:x}", digest.finalize())[..32].to_owned()
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
    crate::markers::admission(&mut tx, &timeline)
        .await?
        .map_err(rejected)?;
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
