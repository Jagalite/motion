//! Administrator playback monitoring: live sessions and finished history.
//! Read-only views over `playback_monitor` observations; `system:admin` only.
use super::{PageQuery, Problem, auth::Caller, page, timestamp};
use crate::{App, delivery};
use axum::{
    Json, Router,
    extract::{Query, State},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::access::Permission;
use serde::Deserialize;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// At most this many live sessions are listed.
const MAX_LIVE: usize = 500;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/admin/playback/sessions", get(sessions))
        .route("/admin/playback/history", get(history))
}

/// Names for a session's identity; any may be gone since (null then).
#[derive(sqlx::FromRow)]
struct Row {
    delivery_id: String,
    principal_id: String,
    device_name: Option<String>,
    profile_id: String,
    profile_name: Option<String>,
    item_id: Option<String>,
    item_title: Option<String>,
    item_kind: Option<String>,
    parent_title: Option<String>,
    timeline_id: String,
    version_id: String,
    version_label: Option<String>,
    mode: String,
    client_network: String,
    started_at: i64,
    last_activity_at: i64,
    ended_at: Option<i64>,
    end_reason: Option<String>,
    bytes_sent: i64,
    peak_bps: i64,
    watched_ms: i64,
    start_position_ms: Option<i64>,
    end_position_ms: Option<i64>,
    stalls: i64,
}

const SELECT: &str = "SELECT ps.delivery_id, ps.principal_id, dv.name AS device_name, ps.profile_id, p.name AS profile_name, \
    i.id AS item_id, i.title AS item_title, coalesce(s.media_type,'unclassified') AS item_kind, pi.title AS parent_title, \
    ps.timeline_id, ps.version_id, v.label AS version_label, ps.mode, ps.client_network, ps.started_at, ps.last_activity_at, \
    ps.ended_at, ps.end_reason, ps.bytes_sent, ps.peak_bps, ps.watched_ms, ps.start_position_ms, ps.end_position_ms, ps.stalls \
    FROM playback_monitor_sessions ps \
    LEFT JOIN devices dv ON dv.id=ps.principal_id \
    LEFT JOIN profiles p ON p.id=ps.profile_id \
    LEFT JOIN timelines t ON t.id=ps.timeline_id \
    LEFT JOIN editions e ON e.id=t.edition_id \
    LEFT JOIN items i ON i.id=e.item_id \
    LEFT JOIN item_structure s ON s.item_id=i.id \
    LEFT JOIN items pi ON pi.id=s.parent_id \
    LEFT JOIN media_versions v ON v.id=ps.version_id";

fn millis(ms: i64) -> String {
    timestamp(ms.div_euclid(1000))
}

fn identity(r: &Row) -> serde_json::Map<String, Value> {
    let device_name = r
        .device_name
        .clone()
        .or_else(|| (r.principal_id == "operator").then(|| "Operator".into()));
    let mut m = serde_json::Map::new();
    m.insert("delivery_id".into(), json!(r.delivery_id));
    m.insert(
        "device".into(),
        json!({"id": r.principal_id, "name": device_name}),
    );
    m.insert(
        "profile".into(),
        json!({"id": r.profile_id, "name": r.profile_name}),
    );
    m.insert(
        "item".into(),
        match &r.item_id {
            Some(id) => json!({"id": id, "title": r.item_title, "kind": r.item_kind, "parent_title": r.parent_title}),
            None => Value::Null,
        },
    );
    m.insert("timeline_id".into(), json!(r.timeline_id));
    m.insert(
        "version".into(),
        json!({"id": r.version_id, "label": r.version_label}),
    );
    m.insert("mode".into(), json!(r.mode));
    m.insert("client_network".into(), json!(r.client_network));
    m.insert("started_at".into(), json!(millis(r.started_at)));
    m
}

/// Live v2 deliveries: who, what, how, and how fast.
async fn sessions(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let mut live = delivery::monitored(&app);
    let truncated = live.len() > MAX_LIVE;
    live.truncate(MAX_LIVE);
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let mut items = Vec::with_capacity(live.len());
    for session in &live {
        let row: Option<Row> = sqlx::query_as(&format!("{SELECT} WHERE ps.delivery_id=?"))
            .bind(&session.id)
            .fetch_optional(&mut *tx)
            .await?;
        // A delivery admitted this instant may not be readable yet.
        let Some(row) = row else { continue };
        let viewing: Option<(String, i64, String, i64, i64)> =
            sqlx::query_as(crate::playback_monitor::AUTHORITATIVE_VIEWING)
                .bind(&session.id)
                .fetch_optional(&mut *tx)
                .await?;
        let viewing = viewing.map(|(id, ..)| id);
        let r = session.telemetry.read();
        let mut m = identity(&row);
        m.insert("viewing_session_id".into(), json!(viewing));
        m.insert(
            "status".into(),
            json!(crate::playback_monitor::status(Some(session.status), &r)),
        );
        m.insert("mode".into(), json!(session.mode)); // the current generation's
        m.insert(
            "last_activity_at".into(),
            json!(millis(r.last_activity_ms as i64)),
        );
        m.insert("position_ms".into(), json!(r.position_ms));
        m.insert("duration_ms".into(), json!(session.duration_ms));
        m.insert("bytes_sent".into(), json!(r.bytes_sent.to_string()));
        m.insert("throughput_bps".into(), json!(r.throughput_bps.to_string()));
        m.insert("average_bps".into(), json!(r.average_bps.to_string()));
        m.insert("peak_bps".into(), json!(r.peak_bps.to_string()));
        m.insert("watched_ms".into(), json!(r.watched_ms));
        m.insert("stalls".into(), json!(r.stalls));
        items.push(Value::Object(m));
    }
    tx.commit().await?;
    Ok(Json(json!({
        "sampled_at": timestamp(crate::now()),
        "truncated": truncated,
        "sessions": items,
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    device_id: Option<String>,
    profile_id: Option<String>,
    item_id: Option<String>,
    since: Option<String>,
}

/// History cursors name the last row returned: "<ended_at ms>:<delivery id>".
fn cursor(after: &str) -> Result<Option<(i64, &str)>, Problem> {
    if after.is_empty() {
        return Ok(None);
    }
    let invalid = || Problem::bad("invalid_cursor", "The page cursor is invalid");
    let (at, id) = after.split_once(':').ok_or_else(invalid)?;
    Ok(Some((at.parse().map_err(|_| invalid())?, id)))
}

/// Finished sessions, newest first.
async fn history(
    State(app): State<App>,
    caller: Caller,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let Query(q) = query
        .map_err(|_| Problem::bad("invalid_history_query", "Invalid playback history query"))?;
    let paging = PageQuery {
        cursor: q.cursor.clone(),
        limit: Some(q.limit.unwrap_or(50)),
    };
    let limit = paging.limit()?;
    let after = cursor(paging.after()?)?;
    let since = match &q.since {
        Some(s) => Some(
            OffsetDateTime::parse(s, &Rfc3339)
                .map_err(|_| {
                    Problem::bad(
                        "invalid_history_query",
                        "since must be an RFC 3339 timestamp",
                    )
                })?
                .unix_timestamp_nanos()
                // Whole milliseconds at or after the bound: sessions are
                // timed in milliseconds, so none before it is included.
                .saturating_add(999_999)
                .div_euclid(1_000_000)
                .clamp(0, i64::MAX as i128) as i64,
        ),
        None => None,
    };
    for value in [&q.device_id, &q.profile_id, &q.item_id]
        .into_iter()
        .flatten()
    {
        if value.is_empty() || value.len() > 256 {
            return Err(Problem::bad(
                "invalid_history_query",
                "Filter values must be 1 to 256 characters",
            ));
        }
    }
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} WHERE ps.ended_at IS NOT NULL \
         AND (?1 IS NULL OR ps.principal_id=?1) AND (?2 IS NULL OR ps.profile_id=?2) \
         AND (?3 IS NULL OR e.item_id=?3 OR s.parent_id=?3 \
              OR EXISTS (SELECT 1 FROM item_structure g WHERE g.item_id=s.parent_id AND g.parent_id=?3)) AND (?4 IS NULL OR ps.ended_at>=?4) \
         AND (?5 IS NULL OR ps.ended_at<?5 OR (ps.ended_at=?5 AND ps.delivery_id<?6)) \
         ORDER BY ps.ended_at DESC, ps.delivery_id DESC LIMIT ?7"
    ))
    .bind(&q.device_id)
    .bind(&q.profile_id)
    .bind(&q.item_id)
    .bind(since)
    .bind(after.map(|(at, _)| at))
    .bind(after.map(|(_, id)| id))
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let items = rows
        .into_iter()
        .map(|r| {
            let ended = r.ended_at.unwrap_or(r.last_activity_at);

            let mut m = identity(&r);
            m.insert("ended_at".into(), json!(millis(ended)));
            m.insert("end_reason".into(), json!(r.end_reason));
            m.insert("watched_ms".into(), json!(r.watched_ms));
            m.insert("start_position_ms".into(), json!(r.start_position_ms));
            m.insert("end_position_ms".into(), json!(r.end_position_ms));
            m.insert("bytes_sent".into(), json!(r.bytes_sent.to_string()));
            m.insert(
                "average_bps".into(),
                json!(
                    crate::playback_monitor::average_bps(
                        u64::try_from(r.bytes_sent).unwrap_or(0),
                        u64::try_from(ended - r.started_at).unwrap_or(0),
                    )
                    .to_string()
                ),
            );
            m.insert("peak_bps".into(), json!(r.peak_bps.to_string()));
            m.insert("stalls".into(), json!(r.stalls));
            (format!("{ended}:{}", r.delivery_id), Value::Object(m))
        })
        .collect();
    let result = page(&mut tx, &caller.principal, items, limit).await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn cursors_name_the_last_row() {
        assert_eq!(super::cursor("").unwrap(), None);
        assert_eq!(super::cursor("1700:abc:d").unwrap(), Some((1700, "abc:d")));
        assert!(super::cursor("x:abc").is_err());
        assert!(super::cursor("abc").is_err());
    }
}
