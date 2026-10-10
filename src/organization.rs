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
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let saved = save_filter_in(&mut tx, profile, existing, name, terms).await?;
    tx.commit().await?;
    Ok(saved)
}

/// `save_filter` inside the caller's write transaction (the v2 adapter commits
/// its idempotency record and authorization recheck in the same transaction).
pub async fn save_filter_in(
    conn: &mut SqliteConnection,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    terms: Vec<Term>,
) -> Result<SavedFilter, OrgError> {
    org::valid_name(name)?;
    org::validate_filter(&terms)?;
    let json = serde_json::to_string(&terms)?;
    let (id, next) = match existing {
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO saved_filters VALUES (?,1,?,?,?)")
                .bind(&id)
                .bind(profile)
                .bind(name.trim())
                .bind(json)
                .execute(&mut *conn)
                .await?;
            (id, 1)
        }
        Some((id, expected)) => {
            let current: i64 = sqlx::query_scalar(
                "SELECT revision FROM saved_filters WHERE id=? AND profile_id=?",
            )
            .bind(id)
            .bind(profile)
            .fetch_one(&mut *conn)
            .await?;
            let next = org::advance(revision(current)?, expected)?;
            sqlx::query("UPDATE saved_filters SET revision=?,name=?,terms_json=? WHERE id=?")
                .bind(stored(next)?)
                .bind(name.trim())
                .bind(json)
                .bind(id)
                .execute(&mut *conn)
                .await?;
            (id.to_string(), next)
        }
    };
    Ok(SavedFilter {
        id,
        revision: next,
        profile_id: profile.into(),
        name: name.trim().into(),
        all: terms,
    })
}

/// A filter by ID with its owning profile, for adapters that authorize the
/// owner before disclosing it.
pub async fn load_filter(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<SavedFilter>, OrgError> {
    let row: Option<(i64, String, String, String)> =
        sqlx::query_as("SELECT revision,profile_id,name,terms_json FROM saved_filters WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    row.map(|(revision_value, profile_id, name, terms)| {
        Ok(SavedFilter {
            id: id.into(),
            revision: revision(revision_value)?,
            profile_id,
            name,
            all: serde_json::from_str(&terms)?,
        })
    })
    .transpose()
}

/// Revision-checked removal. A filter that defines a smart collection stays.
pub async fn delete_filter_in(
    conn: &mut SqliteConnection,
    id: &str,
    expected: u64,
) -> Result<(), OrgError> {
    let current: i64 = sqlx::query_scalar("SELECT revision FROM saved_filters WHERE id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM collections WHERE filter_id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    org::removable(
        revision(current)?,
        expected,
        usize::try_from(users).unwrap_or(usize::MAX),
    )?;
    sqlx::query("DELETE FROM saved_filters WHERE id=?")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Revision-checked removal of a collection, playlist or queue (entries and
/// members cascade).
pub async fn delete_owned_in(
    conn: &mut SqliteConnection,
    table: Owned,
    id: &str,
    expected: u64,
) -> Result<(), OrgError> {
    let table = table.table();
    let current: i64 = sqlx::query_scalar(&format!("SELECT revision FROM {table} WHERE id=?"))
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    org::removable(revision(current)?, expected, 0)?;
    // Delete hints (queues name their profile) come from triggers (0029).
    sqlx::query(&format!("DELETE FROM {table} WHERE id=?"))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Profile-owned aggregates without dependants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owned {
    Collection,
    Playlist,
    Queue,
}
impl Owned {
    pub fn table(self) -> &'static str {
        match self {
            Self::Collection => "collections",
            Self::Playlist => "playlists",
            Self::Queue => "queues",
        }
    }
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
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let saved =
        save_collection_in(&mut tx, profile, existing, name, kind, item_ids, filter_id).await?;
    tx.commit().await?;
    Ok(saved)
}

/// `save_collection` inside the caller's write transaction.
pub async fn save_collection_in(
    conn: &mut SqliteConnection,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    kind: CollectionKind,
    item_ids: Vec<String>,
    filter_id: Option<String>,
) -> Result<Collection, OrgError> {
    org::validate_collection(name, kind, &item_ids, filter_id.as_ref())?;
    live_items(&mut *conn, &item_ids).await?;
    if let Some(filter) = &filter_id {
        let own: i64 =
            sqlx::query_scalar("SELECT count(*) FROM saved_filters WHERE id=? AND profile_id=?")
                .bind(filter)
                .bind(profile)
                .fetch_one(&mut *conn)
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
                .execute(&mut *conn)
                .await?;
            (id, 1)
        }
        Some((id, expected)) => {
            let current: i64 =
                sqlx::query_scalar("SELECT revision FROM collections WHERE id=? AND profile_id=?")
                    .bind(id)
                    .bind(profile)
                    .fetch_one(&mut *conn)
                    .await?;
            let next = org::advance(revision(current)?, expected)?;
            sqlx::query("UPDATE collections SET revision=?,name=?,kind=?,filter_id=? WHERE id=?")
                .bind(stored(next)?)
                .bind(name.trim())
                .bind(kind_name)
                .bind(&filter_id)
                .bind(id)
                .execute(&mut *conn)
                .await?;
            sqlx::query("DELETE FROM collection_items WHERE collection_id=?")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            (id.to_string(), next)
        }
    };
    for item in &item_ids {
        sqlx::query("INSERT INTO collection_items VALUES (?,?)")
            .bind(&id)
            .bind(item)
            .execute(&mut *conn)
            .await?;
    }
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

/// Live members of a manual collection in deterministic `(title, id)` order;
/// members merged into another work resolve to that work once.
pub async fn manual_members(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Vec<String>, OrgError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT i.title,i.id FROM collection_items c LEFT JOIN item_aliases a ON a.alias_id=c.item_id JOIN items i ON i.id=coalesce(a.item_id,c.item_id) WHERE c.collection_id=? ORDER BY i.title,i.id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut seen = BTreeSet::new();
    Ok(rows
        .into_iter()
        .filter(|(_, id)| seen.insert(id.clone()))
        .map(|(_, id)| id)
        .collect())
}

/// A collection by ID with its owning profile and live manual members.
pub async fn load_collection(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<Collection>, OrgError> {
    let row: Option<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT revision,profile_id,name,kind,filter_id FROM collections WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((revision_value, profile_id, name, kind, filter_id)) = row else {
        return Ok(None);
    };
    let kind = if kind == "manual" {
        CollectionKind::Manual
    } else {
        CollectionKind::Smart
    };
    let item_ids = match kind {
        CollectionKind::Manual => manual_members(conn, id).await?,
        CollectionKind::Smart => Vec::new(),
    };
    Ok(Some(Collection {
        id: id.into(),
        revision: revision(revision_value)?,
        profile_id,
        name,
        kind,
        item_ids,
        filter_id,
    }))
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
    if kind == "manual" {
        let mut conn = db.acquire().await?;
        return manual_members(&mut conn, id).await;
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
        let known: i64 = sqlx::query_scalar("SELECT count(*) FROM timelines WHERE id=?")
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
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let saved = save_playlist_in(&mut tx, profile, existing, name, entries).await?;
    tx.commit().await?;
    Ok(saved)
}

/// `save_playlist` inside the caller's write transaction.
pub async fn save_playlist_in(
    conn: &mut SqliteConnection,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    entries: Vec<Entry>,
) -> Result<Playlist, OrgError> {
    org::valid_name(name)?;
    org::validate_entries(&entries)?;
    valid_timelines(&mut *conn, &entries).await?;
    let (id, next) = match existing {
        None => {
            let id = new_id();
            sqlx::query("INSERT INTO playlists VALUES (?,1,?,?)")
                .bind(&id)
                .bind(profile)
                .bind(name.trim())
                .execute(&mut *conn)
                .await?;
            (id, 1)
        }
        Some((id, expected)) => {
            let current: i64 =
                sqlx::query_scalar("SELECT revision FROM playlists WHERE id=? AND profile_id=?")
                    .bind(id)
                    .bind(profile)
                    .fetch_one(&mut *conn)
                    .await?;
            let next = org::advance(revision(current)?, expected)?;
            sqlx::query("UPDATE playlists SET revision=?,name=? WHERE id=?")
                .bind(stored(next)?)
                .bind(name.trim())
                .bind(id)
                .execute(&mut *conn)
                .await?;
            (id.to_string(), next)
        }
    };
    store_entries(&mut *conn, "playlist_entries", &id, &entries).await?;
    Ok(Playlist {
        id,
        revision: next,
        profile_id: profile.into(),
        name: name.trim().into(),
        entries,
    })
}

/// A playlist by ID with its owning profile and ordered entries.
pub async fn load_playlist(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<Playlist>, OrgError> {
    let row: Option<(i64, String, String)> =
        sqlx::query_as("SELECT revision,profile_id,name FROM playlists WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((revision_value, profile_id, name)) = row else {
        return Ok(None);
    };
    Ok(Some(Playlist {
        id: id.into(),
        revision: revision(revision_value)?,
        profile_id,
        name,
        entries: load_entries(conn, "playlist_entries", id).await?,
    }))
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

/// The owning profile and state of a queue, read in the caller's transaction.
pub async fn load_queue_by_id(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<(String, Queue)>, OrgError> {
    let profile: Option<String> = sqlx::query_scalar("SELECT profile_id FROM queues WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    match profile {
        None => Ok(None),
        Some(profile) => {
            let queue = load_queue(conn, &profile, id).await?;
            Ok(Some((profile, queue)))
        }
    }
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
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let created = create_queue_in(&mut tx, profile, entries, repeat, shuffle_seed).await?;
    tx.commit().await?;
    Ok(created)
}

/// `create_queue` inside the caller's write transaction.
pub async fn create_queue_in(
    conn: &mut SqliteConnection,
    profile: &str,
    entries: Vec<Entry>,
    repeat: Repeat,
    shuffle_seed: Option<u64>,
) -> Result<(String, Queue), OrgError> {
    org::validate_entries(&entries)?;
    valid_timelines(&mut *conn, &entries).await?;
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
        .execute(&mut *conn)
        .await?;
    store_entries(&mut *conn, "queue_entries", &id, &queue.entries).await?;
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
    let next = change_queue_in(&mut tx, profile, id, expected_revision, change).await?;
    tx.commit().await?;
    Ok(next)
}

/// `change_queue` inside the caller's write transaction.
pub async fn change_queue_in(
    conn: &mut SqliteConnection,
    profile: &str,
    id: &str,
    expected_revision: u64,
    change: QueueChange,
) -> Result<Queue, OrgError> {
    let current = load_queue(&mut *conn, profile, id).await?;
    let next = match change {
        QueueChange::Step(step) => org::advance_queue(&current, expected_revision, step)?,
        QueueChange::Select(entry) => org::select(&current, expected_revision, &entry)?,
        QueueChange::Edit {
            entries,
            repeat,
            shuffle_seed,
        } => {
            valid_timelines(&mut *conn, &entries).await?;
            let next = org::edit_queue(&current, expected_revision, entries, repeat, shuffle_seed)?;
            store_entries(&mut *conn, "queue_entries", id, &next.entries).await?;
            next
        }
    };
    store_queue(&mut *conn, id, &next).await?;
    Ok(next)
}

// ---------------------------------------------------------------------------
// Reads and deletes (profile-scoped; deletions are revision-checked)

async fn delete_owned(
    app: &App,
    table: &str,
    profile: &str,
    id: &str,
    expected_revision: u64,
) -> Result<(), OrgError> {
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let current: i64 = sqlx::query_scalar(&format!(
        "SELECT revision FROM {table} WHERE id=? AND profile_id=?"
    ))
    .bind(id)
    .bind(profile)
    .fetch_one(&mut *tx)
    .await?;
    org::advance(revision(current)?, expected_revision)?;
    let result = sqlx::query(&format!("DELETE FROM {table} WHERE id=?"))
        .bind(id)
        .execute(&mut *tx)
        .await;
    match result {
        // A saved filter still used by a smart collection cannot disappear.
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => {
            return Err(OrgError::InvalidReference(id.into()));
        }
        other => {
            other?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete_filter(
    app: &App,
    profile: &str,
    id: &str,
    expected: u64,
) -> Result<(), OrgError> {
    delete_owned(app, "saved_filters", profile, id, expected).await
}
pub async fn delete_collection(
    app: &App,
    profile: &str,
    id: &str,
    expected: u64,
) -> Result<(), OrgError> {
    delete_owned(app, "collections", profile, id, expected).await
}
pub async fn delete_playlist(
    app: &App,
    profile: &str,
    id: &str,
    expected: u64,
) -> Result<(), OrgError> {
    delete_owned(app, "playlists", profile, id, expected).await
}
pub async fn delete_queue(
    app: &App,
    profile: &str,
    id: &str,
    expected: u64,
) -> Result<(), OrgError> {
    delete_owned(app, "queues", profile, id, expected).await
}

async fn ids(db: &SqlitePool, table: &str, profile: &str) -> Result<Vec<String>, OrgError> {
    let order = if table == "queues" { "id" } else { "name,id" };
    Ok(sqlx::query_scalar(&format!(
        "SELECT id FROM {table} WHERE profile_id=? ORDER BY {order}"
    ))
    .bind(profile)
    .fetch_all(db)
    .await?)
}

pub async fn list_filters(db: &SqlitePool, profile: &str) -> Result<Vec<SavedFilter>, OrgError> {
    let mut out = Vec::new();
    for id in ids(db, "saved_filters", profile).await? {
        out.push(get_filter(db, profile, &id).await?);
    }
    Ok(out)
}

pub async fn get_collection(
    db: &SqlitePool,
    profile: &str,
    id: &str,
) -> Result<Collection, OrgError> {
    let mut tx = db.begin().await?;
    let (rev, name, kind, filter_id): (i64, String, String, Option<String>) = sqlx::query_as(
        "SELECT revision,name,kind,filter_id FROM collections WHERE id=? AND profile_id=?",
    )
    .bind(id)
    .bind(profile)
    .fetch_one(&mut *tx)
    .await?;
    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT item_id FROM collection_items WHERE collection_id=? ORDER BY item_id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Collection {
        id: id.into(),
        revision: revision(rev)?,
        profile_id: profile.into(),
        name,
        kind: serde_json::from_value(serde_json::Value::String(kind))?,
        item_ids,
        filter_id,
    })
}

pub async fn list_collections(db: &SqlitePool, profile: &str) -> Result<Vec<Collection>, OrgError> {
    let mut out = Vec::new();
    for id in ids(db, "collections", profile).await? {
        out.push(get_collection(db, profile, &id).await?);
    }
    Ok(out)
}

pub async fn get_playlist(db: &SqlitePool, profile: &str, id: &str) -> Result<Playlist, OrgError> {
    let mut tx = db.begin().await?;
    let (rev, name): (i64, String) =
        sqlx::query_as("SELECT revision,name FROM playlists WHERE id=? AND profile_id=?")
            .bind(id)
            .bind(profile)
            .fetch_one(&mut *tx)
            .await?;
    let entries = load_entries(&mut tx, "playlist_entries", id).await?;
    tx.commit().await?;
    Ok(Playlist {
        id: id.into(),
        revision: revision(rev)?,
        profile_id: profile.into(),
        name,
        entries,
    })
}

pub async fn list_playlists(db: &SqlitePool, profile: &str) -> Result<Vec<Playlist>, OrgError> {
    let mut out = Vec::new();
    for id in ids(db, "playlists", profile).await? {
        out.push(get_playlist(db, profile, &id).await?);
    }
    Ok(out)
}

pub async fn list_queues(db: &SqlitePool, profile: &str) -> Result<Vec<(String, Queue)>, OrgError> {
    let mut out = Vec::new();
    for id in ids(db, "queues", profile).await? {
        let queue = get_queue(db, profile, &id).await?;
        out.push((id, queue));
    }
    Ok(out)
}
