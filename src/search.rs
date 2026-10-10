//! Catalog search over the rebuildable FTS5 projection. Document text and
//! query syntax come from `playscale_core::search`; canonical rows mark works
//! dirty (migration 0021) and `refresh` rebuilds their documents. Writers call
//! `refresh` inside their own transactions; maintenance drains the rest, and
//! results report `stale` while any work awaits reindexing.
use playscale_core::{
    metadata::{Contribution, resolve},
    search::{self as core, Document},
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeMap;

pub const MAX_LIMIT: i64 = 200;

async fn document(conn: &mut SqliteConnection, item: &str) -> anyhow::Result<Option<Document>> {
    let title: Option<String> = sqlx::query_scalar(
        "SELECT title FROM items WHERE id=? AND id NOT IN (SELECT alias_id FROM item_aliases)",
    )
    .bind(item)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(title) = title else {
        return Ok(None);
    };
    // Retired works contribute both their last display title and their
    // scanned origin title.
    let mut aliases: Vec<String> = sqlx::query_scalar(
        "SELECT i.title FROM item_aliases a JOIN items i ON i.id=a.alias_id WHERE a.item_id=? UNION ALL SELECT o.title FROM item_aliases a JOIN item_origins o ON o.item_id=a.alias_id WHERE a.item_id=?",
    )
    .bind(item)
    .bind(item)
    .fetch_all(&mut *conn)
    .await?;
    let origin: Option<String> =
        sqlx::query_scalar("SELECT title FROM item_origins WHERE item_id=?")
            .bind(item)
            .fetch_optional(&mut *conn)
            .await?;
    aliases.extend(origin);
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT source,document_json FROM metadata_documents WHERE item_id=?")
            .bind(item)
            .fetch_all(&mut *conn)
            .await?;
    let mut docs = BTreeMap::new();
    for (source, text) in rows {
        docs.insert(source, serde_json::from_str::<Contribution>(&text)?);
    }
    let resolved = resolve(&docs);
    let people = resolved
        .values
        .get("cast")
        .and_then(Value::as_array)
        .map(|cast| {
            cast.iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(Document {
        title,
        aliases,
        people,
        tags: resolved.tags.into_iter().collect(),
    }))
}

/// Rebuild up to `limit` dirty documents in the caller's writer transaction.
/// Returns how many were processed.
pub async fn refresh(conn: &mut SqliteConnection, limit: i64) -> anyhow::Result<usize> {
    let dirty: Vec<String> = sqlx::query_scalar("SELECT item_id FROM search_dirty LIMIT ?")
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
    for item in &dirty {
        // Delete through the indexed row map, never by scanning search_fts.
        let previous: Option<i64> =
            sqlx::query_scalar("DELETE FROM search_rows WHERE item_id=? RETURNING fts_rowid")
                .bind(item)
                .fetch_optional(&mut *conn)
                .await?;
        if let Some(rowid) = previous {
            sqlx::query("DELETE FROM search_fts WHERE rowid=?")
                .bind(rowid)
                .execute(&mut *conn)
                .await?;
        }
        if let Some(doc) = document(conn, item).await? {
            let rowid = sqlx::query("INSERT INTO search_fts (item_id,body) VALUES (?,?)")
                .bind(item)
                .bind(core::body(&doc))
                .execute(&mut *conn)
                .await?
                .last_insert_rowid();
            sqlx::query("INSERT INTO search_rows VALUES (?,?)")
                .bind(item)
                .bind(rowid)
                .execute(&mut *conn)
                .await?;
        }
        sqlx::query("DELETE FROM search_dirty WHERE item_id=?")
            .bind(item)
            .execute(&mut *conn)
            .await?;
    }
    Ok(dirty.len())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hit {
    pub item_id: String,
    pub title: String,
}
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Page {
    pub items: Vec<Hit>,
    pub next_cursor: Option<String>,
    /// Matches within the caller's scope (not merely this page).
    pub total: i64,
    /// Some works await reindexing; results may lag recent changes.
    pub stale: bool,
}

#[derive(Debug)]
pub enum SearchError {
    InvalidCursor,
    InvalidLimit,
    Storage(anyhow::Error),
}
impl From<sqlx::Error> for SearchError {
    fn from(e: sqlx::Error) -> Self {
        Self::Storage(e.into())
    }
}

fn encode_cursor(score: f64, id: &str) -> String {
    format!("{:016x}{id}", score.to_bits())
}
fn decode_cursor(cursor: &str) -> Option<(f64, String)> {
    let bits = u64::from_str_radix(cursor.get(..16)?, 16).ok()?;
    let id = cursor.get(16..)?;
    (!id.is_empty()).then(|| (f64::from_bits(bits), id.to_string()))
}

/// Ranked search. `libraries` limits results to works with files in those
/// libraries' sources (the caller passes the viewer's visible set; `None`
/// means unrestricted). Ordering is bm25 rank with an item ID tie-breaker,
/// paged by keyset cursor. Read-only, one snapshot.
pub async fn search(
    db: &SqlitePool,
    query: &str,
    libraries: Option<&[String]>,
    limit: i64,
    cursor: Option<&str>,
) -> Result<Page, SearchError> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(SearchError::InvalidLimit);
    }
    let after = match cursor {
        Some(c) => Some(decode_cursor(c).ok_or(SearchError::InvalidCursor)?),
        None => None,
    };
    let mut tx = db.begin().await?;
    let stale: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM search_dirty)")
        .fetch_one(&mut *tx)
        .await?;
    let Some(expression) = core::fts_query(&core::terms(query)) else {
        tx.commit().await?;
        return Ok(Page {
            items: vec![],
            next_cursor: None,
            total: 0,
            stale,
        });
    };
    let scope = libraries.map(|l| serde_json::to_string(l).unwrap_or_default());
    let filter = "FROM (SELECT item_id,bm25(search_fts) AS score FROM search_fts WHERE search_fts MATCH ?1) s \
        JOIN items i ON i.id=s.item_id \
        WHERE i.id NOT IN (SELECT alias_id FROM item_aliases) \
        AND (?2 IS NULL OR EXISTS (SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id \
             JOIN library_sources l ON l.source_id=f.library_id \
             WHERE e.item_id=i.id AND l.library_id IN (SELECT value FROM json_each(?2))))";
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) {filter}"))
        .bind(&expression)
        .bind(&scope)
        .fetch_one(&mut *tx)
        .await?;
    let (after_score, after_id) = after.unwrap_or((f64::NEG_INFINITY, String::new()));
    let rows: Vec<(String, String, f64)> = sqlx::query_as(&format!(
        "SELECT i.id,i.title,s.score {filter} AND (s.score>?3 OR (s.score=?3 AND i.id>?4)) ORDER BY s.score,i.id LIMIT ?5"
    ))
    .bind(&expression)
    .bind(&scope)
    .bind(after_score)
    .bind(&after_id)
    .bind(limit + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let more = rows.len() as i64 > limit;
    let page: Vec<(String, String, f64)> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = more
        .then(|| page.last().map(|(id, _, score)| encode_cursor(*score, id)))
        .flatten();
    Ok(Page {
        items: page
            .into_iter()
            .map(|(item_id, title, _)| Hit { item_id, title })
            .collect(),
        next_cursor,
        total,
        stale,
    })
}
