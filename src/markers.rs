//! Timeline markers adapter (`playscale_core::markers`).
use crate::{App, new_id};
use playscale_core::markers::{self as core, Kind, Marker, MarkerError, Provenance};
use serde::Serialize;
use sqlx::SqlitePool;

#[derive(Debug)]
pub enum MarkersError {
    Rejected(MarkerError),
    NotFound,
    Storage(anyhow::Error),
}
impl From<sqlx::Error> for MarkersError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            e => Self::Storage(e.into()),
        }
    }
}

fn name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn value<T: serde::de::DeserializeOwned>(name: &str) -> Result<T, MarkersError> {
    serde_json::from_value(serde_json::Value::String(name.into()))
        .map_err(|e| MarkersError::Storage(e.into()))
}

#[derive(Debug, Clone)]
pub struct NewMarker<'a> {
    pub kind: Kind,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub label: Option<&'a str>,
    pub provenance: Provenance,
}

/// Add a marker validated against the reviewed timeline revision.
pub async fn create(
    app: &App,
    timeline: &str,
    expected_timeline_revision: u64,
    marker: NewMarker<'_>,
) -> Result<String, MarkersError> {
    let NewMarker {
        kind,
        start_ms,
        end_ms,
        label,
        provenance,
    } = marker;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let (revision, duration): (i64, Option<i64>) =
        sqlx::query_as("SELECT revision,duration_ms FROM timelines WHERE id=?")
            .bind(timeline)
            .fetch_one(&mut *tx)
            .await?;
    let revision = u64::try_from(revision).map_err(|e| MarkersError::Storage(e.into()))?;
    core::validate(
        kind,
        start_ms,
        end_ms,
        label,
        expected_timeline_revision,
        revision,
        duration.and_then(|d| u64::try_from(d).ok()),
    )
    .map_err(MarkersError::Rejected)?;
    let id = new_id();
    let as_i64 = |v: u64| i64::try_from(v).map_err(|e| MarkersError::Storage(e.into()));
    sqlx::query("INSERT INTO markers (id,timeline_id,timeline_revision,kind,start_ms,end_ms,label,provenance) VALUES (?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(timeline)
        .bind(as_i64(revision)?)
        .bind(name(&kind))
        .bind(as_i64(start_ms)?)
        .bind(end_ms.map(as_i64).transpose()?)
        .bind(label.map(str::trim))
        .bind(name(&provenance))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(id)
}

pub async fn delete(app: &App, id: &str, expected_revision: u64) -> Result<(), MarkersError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM markers WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if u64::try_from(revision).ok() != Some(expected_revision) {
        return Err(MarkersError::Rejected(MarkerError::StaleTimeline));
    }
    sqlx::query("DELETE FROM markers WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// All markers of a timeline (`effective = false`) or the effective view.
pub async fn list(
    db: &SqlitePool,
    timeline: &str,
    effective: bool,
) -> Result<Vec<Marker>, MarkersError> {
    type Row = (String, String, i64, Option<i64>, Option<String>, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id,kind,start_ms,end_ms,label,provenance FROM markers WHERE timeline_id=? ORDER BY start_ms,id",
    )
    .bind(timeline)
    .fetch_all(db)
    .await?;
    let mut markers = Vec::with_capacity(rows.len());
    for (id, kind, start, end, label, provenance) in rows {
        markers.push(Marker {
            id,
            kind: value(&kind)?,
            start_ms: u64::try_from(start).map_err(|e| MarkersError::Storage(e.into()))?,
            end_ms: end.and_then(|e| u64::try_from(e).ok()),
            label,
            provenance: value(&provenance)?,
        });
    }
    Ok(if effective {
        core::effective(&markers)
    } else {
        markers
    })
}
