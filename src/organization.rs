//! Organization persistence. Validation, revisions, queue navigation and filter
//! membership are decided by `playscale_core::organization`; this adapter loads
//! facts and stores results under the shared writer boundary.
//!
//! Every read is scoped to one profile. Membership never grants access: callers
//! must still apply the viewer's library policy to returned items (A08).
use crate::{App, new_id};
use playscale_core::organization::{
    self as org, CollectionKind, Entry, Facts, OrganizationError, Queue, Repeat, Step, Term,
};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::BTreeSet;

#[derive(Debug)]
pub enum OrgError {
    Rejected(OrganizationError),
    NotFound,
    /// A referenced work, timeline or filter does not exist or is not the
    /// profile's own.
    InvalidReference(String),
    Storage(anyhow::Error),
}
impl From<OrganizationError> for OrgError {
    fn from(e: OrganizationError) -> Self {
        Self::Rejected(e)
    }
}
impl From<sqlx::Error> for OrgError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            e => Self::Storage(e.into()),
        }
    }
}
impl From<anyhow::Error> for OrgError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<serde_json::Error> for OrgError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.into())
    }
}

fn revision(value: i64) -> Result<u64, OrgError> {
    u64::try_from(value).map_err(|e| OrgError::Storage(e.into()))
}
fn stored(value: u64) -> Result<i64, OrgError> {
    i64::try_from(value).map_err(|_| OrgError::Rejected(OrganizationError::RevisionExhausted))
}

// ---------------------------------------------------------------------------
// Saved filters

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SavedFilter {
    pub id: String,
    pub revision: u64,
    pub profile_id: String,
    pub name: String,
    pub all: Vec<Term>,
}

pub async fn get_filter(db: &SqlitePool, profile: &str, id: &str) -> Result<SavedFilter, OrgError> {
    let (revision_value, name, terms): (i64, String, String) = sqlx::query_as(
        "SELECT revision,name,terms_json FROM saved_filters WHERE id=? AND profile_id=?",
    )
    .bind(id)
    .bind(profile)
    .fetch_one(db)
    .await?;
    Ok(SavedFilter {
        id: id.into(),
        revision: revision(revision_value)?,
        profile_id: profile.into(),
        name,
        all: serde_json::from_str(&terms)?,
    })
}

pub async fn save_filter(
    app: &App,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    terms: Vec<Term>,
) -> Result<SavedFilter, OrgError> {
    org::valid_name(name)?;
    org::validate_filter(&terms)?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let json = serde_json::to_string(&terms)?;
    let (id, next) = match existing {
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO saved_filters VALUES (?,1,?,?,?)")
                .bind(&id)
                .bind(profile)
                .bind(name.trim())
                .bind(json)
                .execute(&mut *tx)
                .await?;
            (id, 1)
        }
        Some((id, expected)) => {
            let current: i64 = sqlx::query_scalar(
                "SELECT revision FROM saved_filters WHERE id=? AND profile_id=?",
            )
            .bind(id)
            .bind(profile)
            .fetch_one(&mut *tx)
            .await?;
            let next = org::advance(revision(current)?, expected)?;
            sqlx::query("UPDATE saved_filters SET revision=?,name=?,terms_json=? WHERE id=?")
                .bind(stored(next)?)
                .bind(name.trim())
                .bind(json)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            (id.to_string(), next)
        }
    };
    tx.commit().await?;
    Ok(SavedFilter {
        id,
        revision: next,
        profile_id: profile.into(),
        name: name.trim().into(),
        all: terms,
    })
}

// ---------------------------------------------------------------------------
// Collections

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Collection {
    pub id: String,
    pub revision: u64,
    pub profile_id: String,
    pub name: String,
    pub kind: CollectionKind,
    /// Stored members of a manual collection (empty for smart collections).
    pub item_ids: Vec<String>,
    pub filter_id: Option<String>,
}

async fn live_items(conn: &mut SqliteConnection, ids: &[String]) -> Result<(), OrgError> {
    for id in ids {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM items WHERE id=? AND id NOT IN (SELECT alias_id FROM item_aliases)",
        )
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
        if live == 0 {
            return Err(OrgError::InvalidReference(id.clone()));
        }
    }
    Ok(())
}

pub async fn save_collection(
    app: &App,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    kind: CollectionKind,
    item_ids: Vec<String>,
    filter_id: Option<String>,
) -> Result<Collection, OrgError> {
    org::validate_collection(name, kind, &item_ids, filter_id.as_ref())?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    live_items(&mut tx, &item_ids).await?;
    if let Some(filter) = &filter_id {
        let own: i64 =
            sqlx::query_scalar("SELECT count(*) FROM saved_filters WHERE id=? AND profile_id=?")
                .bind(filter)
                .bind(profile)
                .fetch_one(&mut *tx)
                .await?;
        if own == 0 {
            return Err(OrgError::InvalidReference(filter.clone()));
        }
    }
    let kind_name = match kind {
        CollectionKind::Manual => "manual",
        CollectionKind::Smart => "smart",
    };
    let (id, next) = match existing {
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO collections VALUES (?,1,?,?,?,?)")
                .bind(&id)
                .bind(profile)
                .bind(name.trim())
                .bind(kind_name)
                .bind(&filter_id)
                .execute(&mut *tx)
                .await?;
            (id, 1)
        }
        Some((id, expected)) => {
            let current: i64 =
                sqlx::query_scalar("SELECT revision FROM collections WHERE id=? AND profile_id=?")
                    .bind(id)
                    .bind(profile)
                    .fetch_one(&mut *tx)
                    .await?;
            let next = org::advance(revision(current)?, expected)?;
            sqlx::query("UPDATE collections SET revision=?,name=?,kind=?,filter_id=? WHERE id=?")
                .bind(stored(next)?)
                .bind(name.trim())
                .bind(kind_name)
                .bind(&filter_id)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM collection_items WHERE collection_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            (id.to_string(), next)
        }
    };
    for item in &item_ids {
        sqlx::query("INSERT INTO collection_items VALUES (?,?)")
            .bind(&id)
            .bind(item)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Collection {
        id,
        revision: next,
        profile_id: profile.into(),
        name: name.trim().into(),
        kind,
        item_ids,
        filter_id,
    })
}

/// Facts a filter may observe, from effective metadata and the profile's
/// viewing state.
async fn facts(db: &SqlitePool, profile: &str, item: &str) -> Result<Facts, OrgError> {
    let (kind, title, available): (String, String, bool) = sqlx::query_as(
        "SELECT coalesce(s.media_type,'unclassified'),i.title,EXISTS(SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=i.id AND f.available=1 AND f.generated=0) FROM items i LEFT JOIN item_structure s ON s.item_id=i.id WHERE i.id=?",
    )
    .bind(item)
    .fetch_one(db)
    .await?;
    let metadata = crate::metadata::load(db, item)
        .await
        .map_err(|_| OrgError::Storage(anyhow::anyhow!("metadata unavailable")))?;
    let watched: Option<(bool, Option<bool>)> = sqlx::query_as(
        "SELECT automatic_watched,manual_watched FROM viewing_state WHERE profile_id=? AND item_id=?",
    )
    .bind(profile)
    .bind(item)
    .fetch_optional(db)
    .await?;
    let strings = |key: &str| -> Vec<String> {
        metadata
            .values
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(Facts {
        kind,
        title,
        year: metadata.values.get("release_year").and_then(|v| v.as_i64()),
        genres: strings("genres"),
        person_ids: strings("person_ids"),
        tags: metadata.tags.into_iter().collect(),
        available,
        watched: watched.is_some_and(|(automatic, manual)| manual.unwrap_or(automatic)),
    })
}

/// Members in deterministic `(title, id)` order. Manual members that were later
/// merged resolve to their live work once; smart members are evaluated now.
pub async fn members(db: &SqlitePool, profile: &str, id: &str) -> Result<Vec<String>, OrgError> {
    let (kind, filter): (String, Option<String>) =
        sqlx::query_as("SELECT kind,filter_id FROM collections WHERE id=? AND profile_id=?")
            .bind(id)
            .bind(profile)
            .fetch_one(db)
            .await?;
    let ordered = |rows: Vec<(String, String)>| {
        let mut seen = BTreeSet::new();
        rows.into_iter()
            .filter(|(_, id)| seen.insert(id.clone()))
            .map(|(_, id)| id)
            .collect::<Vec<_>>()
    };
    if kind == "manual" {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT i.title,i.id FROM collection_items c LEFT JOIN item_aliases a ON a.alias_id=c.item_id JOIN items i ON i.id=coalesce(a.item_id,c.item_id) WHERE c.collection_id=? ORDER BY i.title,i.id",
        )
        .bind(id)
        .fetch_all(db)
        .await?;
        return Ok(ordered(rows));
    }
    let filter = get_filter(db, profile, filter.as_deref().unwrap_or_default()).await?;
    let candidates: Vec<(String, String)> = sqlx::query_as(
        "SELECT title,id FROM items WHERE id NOT IN (SELECT alias_id FROM item_aliases) ORDER BY title,id",
    )
    .fetch_all(db)
    .await?;
    let mut out = Vec::new();
    for (_, item) in candidates {
        if org::matches(&filter.all, &facts(db, profile, &item).await?) {
            out.push(item);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Playlists and queues

async fn valid_timelines(conn: &mut SqliteConnection, entries: &[Entry]) -> Result<(), OrgError> {
    for entry in entries {
        let known: i64 = sqlx::query_scalar("SELECT count(*) FROM editions WHERE id=?")
            .bind(&entry.timeline_id)
            .fetch_one(&mut *conn)
            .await?;
        if known == 0 {
            return Err(OrgError::InvalidReference(entry.timeline_id.clone()));
        }
    }
    Ok(())
}

async fn store_entries(
    conn: &mut SqliteConnection,
    table: &str,
    owner: &str,
    entries: &[Entry],
) -> Result<(), OrgError> {
    let column = if table == "playlist_entries" {
        "playlist_id"
    } else {
        "queue_id"
    };
    sqlx::query(&format!("DELETE FROM {table} WHERE {column}=?"))
        .bind(owner)
        .execute(&mut *conn)
        .await?;
    for (position, entry) in entries.iter().enumerate() {
        sqlx::query(&format!("INSERT INTO {table} VALUES (?,?,?,?)"))
            .bind(owner)
            .bind(i64::try_from(position).map_err(|e| OrgError::Storage(e.into()))?)
            .bind(&entry.entry_id)
            .bind(&entry.timeline_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

async fn load_entries(
    conn: &mut SqliteConnection,
    table: &str,
    owner: &str,
) -> Result<Vec<Entry>, OrgError> {
    let column = if table == "playlist_entries" {
        "playlist_id"
    } else {
        "queue_id"
    };
    let rows: Vec<(String, String)> = sqlx::query_as(&format!(
        "SELECT entry_id,timeline_id FROM {table} WHERE {column}=? ORDER BY position"
    ))
    .bind(owner)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(entry_id, timeline_id)| Entry {
            entry_id,
            timeline_id,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub revision: u64,
    pub profile_id: String,
    pub name: String,
    pub entries: Vec<Entry>,
}

pub async fn save_playlist(
    app: &App,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    entries: Vec<Entry>,
) -> Result<Playlist, OrgError> {
    org::valid_name(name)?;
    org::validate_entries(&entries)?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    valid_timelines(&mut tx, &entries).await?;
    let (id, next) = match existing {
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO playlists VALUES (?,1,?,?)")
                .bind(&id)
                .bind(profile)
                .bind(name.trim())
                .execute(&mut *tx)
                .await?;
            (id, 1)
        }
        Some((id, expected)) => {
            let current: i64 =
                sqlx::query_scalar("SELECT revision FROM playlists WHERE id=? AND profile_id=?")
                    .bind(id)
                    .bind(profile)
                    .fetch_one(&mut *tx)
                    .await?;
            let next = org::advance(revision(current)?, expected)?;
            sqlx::query("UPDATE playlists SET revision=?,name=? WHERE id=?")
                .bind(stored(next)?)
                .bind(name.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            (id.to_string(), next)
        }
    };
    store_entries(&mut tx, "playlist_entries", &id, &entries).await?;
    tx.commit().await?;
    Ok(Playlist {
        id,
        revision: next,
        profile_id: profile.into(),
        name: name.trim().into(),
        entries,
    })
}

fn repeat_name(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "off",
        Repeat::One => "one",
        Repeat::All => "all",
    }
}

async fn load_queue(
    conn: &mut SqliteConnection,
    profile: &str,
    id: &str,
) -> Result<Queue, OrgError> {
    let (revision_value, repeat, seed, current): (i64, String, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT revision,repeat,shuffle_seed,current_entry_id FROM queues WHERE id=? AND profile_id=?",
        )
        .bind(id)
        .bind(profile)
        .fetch_one(&mut *conn)
        .await?;
    Ok(Queue {
        revision: revision(revision_value)?,
        entries: load_entries(conn, "queue_entries", id).await?,
        current,
        repeat: serde_json::from_value(serde_json::Value::String(repeat))?,
        shuffle_seed: seed
            .map(|s| s.parse::<u64>())
            .transpose()
            .map_err(|e| OrgError::Storage(e.into()))?,
    })
}

async fn store_queue(conn: &mut SqliteConnection, id: &str, queue: &Queue) -> Result<(), OrgError> {
    sqlx::query(
        "UPDATE queues SET revision=?,repeat=?,shuffle_seed=?,current_entry_id=? WHERE id=?",
    )
    .bind(stored(queue.revision)?)
    .bind(repeat_name(queue.repeat))
    .bind(queue.shuffle_seed.map(|s| s.to_string()))
    .bind(&queue.current)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Header and entries come from one read snapshot so `current` is always a
/// member of the returned entries.
pub async fn get_queue(db: &SqlitePool, profile: &str, id: &str) -> Result<Queue, OrgError> {
    let mut tx = db.begin().await?;
    let queue = load_queue(&mut tx, profile, id).await?;
    tx.commit().await?;
    Ok(queue)
}

pub async fn create_queue(
    app: &App,
    profile: &str,
    entries: Vec<Entry>,
    repeat: Repeat,
    shuffle_seed: Option<u64>,
) -> Result<(String, Queue), OrgError> {
    org::validate_entries(&entries)?;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    valid_timelines(&mut tx, &entries).await?;
    let id = new_id();
    let queue = Queue {
        revision: 1,
        entries,
        current: None,
        repeat,
        shuffle_seed,
    };
    sqlx::query("INSERT INTO queues VALUES (?,1,?,?,?,NULL)")
        .bind(&id)
        .bind(profile)
        .bind(repeat_name(repeat))
        .bind(shuffle_seed.map(|s| s.to_string()))
        .execute(&mut *tx)
        .await?;
    store_entries(&mut tx, "queue_entries", &id, &queue.entries).await?;
    tx.commit().await?;
    Ok((id, queue))
}

pub enum QueueChange {
    Step(Step),
    Select(String),
    Edit {
        entries: Vec<Entry>,
        repeat: Repeat,
        shuffle_seed: Option<u64>,
    },
}

/// Apply one revision-checked queue change for its owning profile.
pub async fn change_queue(
    app: &App,
    profile: &str,
    id: &str,
    expected_revision: u64,
    change: QueueChange,
) -> Result<Queue, OrgError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let current = load_queue(&mut tx, profile, id).await?;
    let next = match change {
        QueueChange::Step(step) => org::advance_queue(&current, expected_revision, step)?,
        QueueChange::Select(entry) => org::select(&current, expected_revision, &entry)?,
        QueueChange::Edit {
            entries,
            repeat,
            shuffle_seed,
        } => {
            valid_timelines(&mut tx, &entries).await?;
            let next = org::edit_queue(&current, expected_revision, entries, repeat, shuffle_seed)?;
            store_entries(&mut tx, "queue_entries", id, &next.entries).await?;
            next
        }
    };
    store_queue(&mut tx, id, &next).await?;
    tx.commit().await?;
    Ok(next)
}
