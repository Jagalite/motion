//! Catalog/storage adapter (services owned by A01; search by A09): logical
//! items, editions, title search, and reviewed merge/split plans with their
//! commit. Domain decisions come from `playscale_core::catalog`,
//! `playscale_core::identity` and `playscale_core::access`; merges and splits
//! run through `crate::curation` inside the transaction that also records the
//! idempotent acknowledgement.
//!
//! Timelines and versions use the production identity tables. Catalog
//! membership comes from original files through logical library/source bindings.
//! Source/scan services are adapted separately; relationships, timeline/version
//! creation and full file-track evidence still need their service ports.
use super::{
    Body, Page, PageQuery, Problem, Scope, Stored, auth::Caller, etag, idempotency_key, if_match,
    page, scope_clause, timestamp,
};
use crate::{App, db::begin_write, new_id, now};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use playscale_core::{
    access::{self, CatalogScope, Idempotent, Permission, Revised},
    identity::{IdentityError, MergePlan, MergeRequest, SplitMove, SplitPlan, SplitRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/catalog/items", get(list_items).post(create_item))
        .route("/catalog/search", get(search))
        .route("/catalog/items/{item_id}", get(get_item).put(replace_item))
        .route(
            "/catalog/items/{item_id}/editions",
            get(list_editions).post(create_edition),
        )
        .route("/catalog/items/{item_id}/timelines", get(list_timelines))
        .route("/catalog/timelines/{timeline_id}", get(get_timeline))
        .route(
            "/catalog/timelines/{timeline_id}/versions",
            get(list_versions),
        )
        .route("/catalog/items/{item_id}/merge-plans", post(preview_merge))
        .route("/catalog/items/{item_id}/split-plans", post(preview_split))
        .route("/catalog/reconciliations", post(commit_reconciliation))
}

// ---------------------------------------------------------------------------
// Shared helpers

fn tagged<T: Serialize>(status: StatusCode, body: &T, revision: u64) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, etag(revision));
    response
}

/// Replays a stored acknowledgement without re-executing the operation.
fn replayed(stored: Stored) -> Response {
    let body = stored.body.unwrap_or_default();
    let revision = body
        .get("revision")
        .and_then(|r| r.as_str())
        .and_then(|r| r.parse().ok());
    let mut response = (stored.status, Json(body)).into_response();
    if let Some(revision) = revision {
        response.headers_mut().insert(header::ETAG, etag(revision));
    }
    response
        .headers_mut()
        .insert("idempotent-replayed", HeaderValue::from_static("true"));
    response
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_ids(field: &str, ids: &[String], max: usize) -> Result<(), Problem> {
    if ids.len() <= max && ids.iter().all(|id| valid_id(id)) {
        Ok(())
    } else {
        Err(Problem::invalid(
            "invalid_field",
            format!("{field} must contain at most {max} valid IDs"),
        ))
    }
}

/// Title-like text: 1..=max characters after trimming, no control characters.
fn text(field: &str, value: &str, max: usize) -> Result<String, Problem> {
    let trimmed = value.trim();
    let n = trimmed.chars().count();
    if n == 0 || n > max || trimmed.chars().any(char::is_control) {
        return Err(Problem::invalid(
            "invalid_field",
            format!("{field} must contain 1-{max} characters without control characters"),
        ));
    }
    Ok(trimmed.into())
}

fn revision_text(field: &str, value: &str) -> Result<u64, Problem> {
    let canonical = !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'));
    canonical
        .then(|| value.parse().ok())
        .flatten()
        .ok_or_else(|| {
            Problem::invalid(
                "invalid_field",
                format!("{field} must be unsigned decimal revisions"),
            )
        })
}

fn outside_scope() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "outside_library_scope",
        "The change touches libraries outside the principal's library policy",
    )
}

fn derived_membership() -> Problem {
    Problem::invalid(
        "derived_library_membership",
        "library_ids is derived from the item's files; send the current value",
    )
}

fn identity_problem(e: IdentityError) -> Problem {
    use IdentityError::*;
    let (status, code, detail) = match e {
        IdentityError::SharedFileSplit(_) => (
            StatusCode::CONFLICT,
            "shared_file_split",
            "A shared file cannot be split across editions",
        ),
        IdentityError::ReviewedContentChanged(_) => (
            StatusCode::CONFLICT,
            "reviewed_content_changed",
            "Reviewed file content changed; preview again",
        ),
        UnknownItem(_) => return Problem::not_found(),
        StaleRevision(_) | RevisionExhausted(_) => (
            StatusCode::CONFLICT,
            "revision_conflict",
            "A participant changed since it was reviewed; read it again and preview a new plan",
        ),
        ExternalIdentityConflict(_) => (
            StatusCode::CONFLICT,
            "external_identity_conflict",
            "The works carry different identities from the same provider",
        ),
        IncompatibleKinds(_) => (
            StatusCode::CONFLICT,
            "incompatible_kinds",
            "Only works of the same kind can be merged",
        ),
        SourceHasChildren(_) => (
            StatusCode::CONFLICT,
            "source_has_children",
            "A merged work must not own children",
        ),
        SplitWouldEmptySource => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "split_would_empty_source",
            "A split must leave at least one version on the source work",
        ),
        UnknownVersion(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_version",
            "A selected version does not belong to this work",
        ),
        EmptySelection => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "empty_selection",
            "Select at least one participant",
        ),
        DuplicateSelection(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "duplicate_selection",
            "A participant is selected more than once",
        ),
        TargetSelected => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "target_selected",
            "The target cannot also be a merge source",
        ),
        MissingExpectedRevision(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "missing_expected_revision",
            "expected_revisions must name every participant",
        ),
        InvalidTitle => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_field",
            "new_title must contain 1-500 characters",
        ),
        InvalidBindings(_)
        | UnknownEquivalenceRequiresNewTimeline
        | SharedFileNeedsKnownIntervals(_) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_selection",
            "The selection cannot be applied to this work",
        ),
    };
    Problem::new(status, code, detail)
}

fn curation_problem(e: crate::curation::CurationError) -> Problem {
    match e {
        crate::curation::CurationError::Rejected(e) => identity_problem(e),
        crate::curation::CurationError::Storage(e) => match e.downcast::<sqlx::Error>() {
            Ok(e) => e.into(),
            Err(e) => Problem::internal(e),
        },
    }
}

// URL-safe base64 without padding, for opaque cursors and plan tokens.
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 3);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let values = text
        .bytes()
        .map(|c| B64.iter().position(|&b| b == c).map(|p| p as u32))
        .collect::<Option<Vec<u32>>>()?;
    let mut out = Vec::with_capacity(values.len() * 3 / 4);
    for chunk in values.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, v)| n | v << (18 - 6 * i));
        for i in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Item facts and representation

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Movie,
    Series,
    Season,
    Episode,
    Video,
    MusicVideo,
    Artist,
    Album,
    Recording,
    Track,
    Photo,
    PhotoAlbum,
    Person,
    Collection,
    Other,
}

/// Contract kind of a stored item. Unclassified items have no logical kind;
/// they are reported by their media family.
fn media_kind(media_type: &str, kind: &str) -> &'static str {
    match media_type {
        "movie" => "movie",
        "series" => "series",
        "season" => "season",
        "episode" => "episode",
        _ if kind == "video" => "video",
        _ => "other",
    }
}

/// The stored `(media_type, items.kind)` a contract kind denotes, or `None`
/// when this catalog cannot record it. `items.kind` is `None` when any media
/// family is consistent with the kind.
fn storage_kind(kind: MediaKind) -> Option<(&'static str, Option<&'static str>)> {
    match kind {
        MediaKind::Movie => Some(("movie", None)),
        MediaKind::Series => Some(("series", None)),
        MediaKind::Season => Some(("season", None)),
        MediaKind::Episode => Some(("episode", None)),
        MediaKind::Video => Some(("unclassified", Some("video"))),
        MediaKind::Other => Some(("unclassified", Some("audio"))),
        _ => None,
    }
}

fn unsupported_kind() -> Problem {
    Problem::invalid(
        "unsupported_kind",
        "This catalog records movie, series, season, episode and unclassified video items only",
    )
}

#[derive(sqlx::FromRow)]
struct ItemRow {
    id: String,
    title: String,
    kind: String,
    media_type: String,
    parent_id: Option<String>,
    revision: i64,
    match_state: String,
}

const ITEM_SELECT: &str = "SELECT i.id,i.title,i.kind,coalesce(s.media_type,'unclassified') AS media_type,s.parent_id,i.catalog_revision AS revision,i.match_state FROM items i LEFT JOIN item_structure s ON s.item_id=i.id";

/// Library and availability of every original file of an item.
async fn files_of(
    conn: &mut sqlx::SqliteConnection,
    item: &str,
) -> Result<Vec<(String, bool)>, Problem> {
    Ok(sqlx::query_as(
        "SELECT ls.library_id,f.available FROM media_files f JOIN library_sources ls ON ls.source_id=f.library_id JOIN editions e ON e.id=f.edition_id WHERE e.item_id=? AND f.generated=0 ORDER BY f.id,ls.library_id",
    )
    .bind(item)
    .fetch_all(conn)
    .await?)
}

fn libraries(files: &[(String, bool)]) -> BTreeSet<String> {
    files.iter().map(|(l, _)| l.clone()).collect()
}

/// A live item readable under `scope`. A retired (merged) ID resolves to its
/// live work. Missing and inaccessible items are indistinguishable.
async fn load_item(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    id: &str,
    follow_alias: bool,
) -> Result<(ItemRow, BTreeSet<String>), Problem> {
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    let live: String = if follow_alias {
        sqlx::query_scalar("SELECT coalesce((SELECT item_id FROM item_aliases WHERE alias_id=?),?)")
            .bind(id)
            .bind(id)
            .fetch_one(&mut *conn)
            .await?
    } else {
        id.into()
    };
    let row: ItemRow = sqlx::query_as(&format!(
        "{ITEM_SELECT} WHERE i.id=? AND i.id NOT IN (SELECT alias_id FROM item_aliases)"
    ))
    .bind(&live)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(Problem::not_found)?;
    let libraries = libraries(&files_of(conn, &row.id).await?);
    if !scope.admits(libraries.iter()) {
        return Err(Problem::not_found());
    }
    Ok((row, libraries))
}

/// A live item the principal may change: readable (else 404) and wholly
/// inside its library scope (else 403).
async fn curatable_item(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    id: &str,
    follow_alias: bool,
) -> Result<ItemRow, Problem> {
    let (row, libraries) = load_item(conn, scope, id, follow_alias).await?;
    if !access::may_curate(scope, &libraries) {
        return Err(outside_scope());
    }
    Ok(row)
}

#[derive(Serialize)]
struct ExternalIdentity {
    provider: String,
    value: String,
    /// The provider namespace version is not recorded on this branch.
    namespace_version: String,
}

#[derive(Serialize)]
pub struct CatalogItemBody {
    id: String,
    revision: String,
    kind: &'static str,
    title: String,
    library_ids: Vec<String>,
    availability: &'static str,
    external_ids: Vec<ExternalIdentity>,
    artwork_id: Option<String>,
    default_timeline_id: Option<String>,
    match_state: String,
    parent_ids: Vec<String>,
}

/// Availability of the files the principal may read: no files is unknown.
fn availability(files: &[&(String, bool)]) -> &'static str {
    let available = files.iter().filter(|(_, a)| *a).count();
    match (files.len(), available) {
        (0, _) => "unknown",
        (n, a) if a == n => "available",
        (_, 0) => "unavailable",
        _ => "degraded",
    }
}

async fn item_body(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    row: ItemRow,
) -> Result<CatalogItemBody, Problem> {
    let files = files_of(conn, &row.id).await?;
    // Libraries and availability outside the principal's scope are not disclosed.
    let visible: Vec<&(String, bool)> = files.iter().filter(|(l, _)| scope.library(l)).collect();
    let library_ids: BTreeSet<String> = visible.iter().map(|(l, _)| l.clone()).collect();
    let external_ids = sqlx::query_as::<_, (String, String)>(
        "SELECT source,external_id FROM metadata_documents WHERE item_id=? AND external_id IS NOT NULL ORDER BY source",
    )
    .bind(&row.id)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(provider, value)| ExternalIdentity {
        provider,
        value,
        namespace_version: String::new(),
    })
    .collect();
    let pinned: Option<String> = sqlx::query_scalar(
        "SELECT asset_id FROM artwork_selections WHERE item_id=? AND role='poster'",
    )
    .bind(&row.id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    let contributions: Vec<(String, String)> = sqlx::query_as(
        "SELECT source,asset_id FROM artwork_contributions WHERE item_id=? AND role='poster'",
    )
    .bind(&row.id)
    .fetch_all(&mut *conn)
    .await?;
    let local = contributions
        .iter()
        .find(|(s, _)| s == "local")
        .map(|(_, a)| a.clone());
    let candidates = contributions.into_iter().map(|(_, a)| a).collect();
    let (artwork_id, _) = playscale_core::artwork::resolve(pinned, local, &candidates);
    // Count only visible timelines: a hidden edition must not alter the
    // viewer's default selection or disclose itself through that count.
    let (clause, all, ids) = scope_clause(scope, "f.library_id");
    let editions: Vec<String> =
        sqlx::query_scalar(&format!("SELECT t.id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE e.item_id=? AND (? OR NOT EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0) OR EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0 AND {clause})) ORDER BY t.id LIMIT 2"))
            .bind(&row.id).bind(all).bind(all).bind(ids)
            .fetch_all(&mut *conn)
            .await?;
    let default_timeline_id = match editions.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    let mut parent_ids = Vec::new();
    if let Some(parent) = &row.parent_id {
        let parent_libraries = libraries(&files_of(conn, parent).await?);
        if scope.admits(parent_libraries.iter()) {
            parent_ids.push(parent.clone());
        }
    }
    Ok(CatalogItemBody {
        kind: media_kind(&row.media_type, &row.kind),
        revision: row.revision.to_string(),
        id: row.id,
        title: row.title,
        library_ids: library_ids.into_iter().collect(),
        availability: availability(&visible),
        external_ids,
        artwork_id,
        default_timeline_id,
        match_state: row.match_state,
        parent_ids,
    })
}

async fn item_response(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    id: &str,
) -> Result<(serde_json::Value, u64), Problem> {
    let (row, _) = load_item(conn, scope, id, false).await?;
    let revision = row.revision as u64;
    let body = item_body(conn, scope, row).await?;
    Ok((
        serde_json::to_value(body).map_err(Problem::internal)?,
        revision,
    ))
}

// ---------------------------------------------------------------------------
// Browse and search

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    Title,
    RecentlyAdded,
    ReleaseDate,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    library_id: Option<String>,
    kind: Option<MediaKind>,
    parent_id: Option<String>,
    q: Option<String>,
    sort: Option<Sort>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    cursor: Option<String>,
    limit: Option<u32>,
    q: String,
    library_id: Option<String>,
}

#[derive(Default)]
struct Filter {
    library_id: Option<String>,
    kind: Option<MediaKind>,
    parent_id: Option<String>,
    q: Option<String>,
}

fn query_id(field: &str, id: &Option<String>) -> Result<(), Problem> {
    match id {
        Some(id) if !valid_id(id) => Err(Problem::bad(
            "invalid_query",
            format!("{field} must be a valid ID"),
        )),
        _ => Ok(()),
    }
}

fn query_text(q: &Option<String>) -> Result<(), Problem> {
    match q {
        Some(q) if q.is_empty() || q.chars().count() > 200 || q.chars().any(char::is_control) => {
            Err(Problem::bad(
                "invalid_query",
                "q must contain 1-200 characters",
            ))
        }
        _ => Ok(()),
    }
}

/// Keyset cursor over `(title, id)`.
fn encode_cursor(title: &str, id: &str) -> String {
    b64_encode(
        serde_json::to_string(&(title, id))
            .expect("strings serialize")
            .as_bytes(),
    )
}

fn decode_cursor(cursor: &str) -> Result<(String, String), Problem> {
    b64_decode(cursor)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| Problem::bad("invalid_cursor", "The page cursor is invalid"))
}

/// Authorization is applied inside the query, before filtering, ordering and
/// the page limit, so restricted rows never influence a page.
async fn browse(
    app: &App,
    caller: &Caller,
    filter: Filter,
    q: PageQuery,
) -> Result<Json<Page<CatalogItemBody>>, Problem> {
    caller.require(Permission::CatalogRead)?;
    let limit = q.limit()?;
    let after = match q.after()? {
        "" => None,
        cursor => Some(decode_cursor(cursor)?),
    };
    query_id("library_id", &filter.library_id)?;
    query_id("parent_id", &filter.parent_id)?;
    query_text(&filter.q)?;
    let (kind_ok, media_type, family) = match filter.kind {
        None => (true, None, None),
        Some(kind) => match storage_kind(kind) {
            Some((media_type, family)) => (true, Some(media_type), family),
            // A valid kind this catalog never records matches nothing.
            None => (false, None, None),
        },
    };
    let scope = access::catalog_scope(&caller.principal);
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    let sql = format!(
        "{ITEM_SELECT} WHERE i.id NOT IN (SELECT alias_id FROM item_aliases)
         AND (? OR EXISTS (SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=i.id AND f.generated=0 AND {clause}))
         AND (? IS NULL OR EXISTS (SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=i.id AND f.generated=0 AND f.library_id IN (SELECT source_id FROM library_sources WHERE library_id=?) AND {clause}))
         AND ? AND (? IS NULL OR coalesce(s.media_type,'unclassified')=?) AND (? IS NULL OR i.kind=?)
         AND (? IS NULL OR s.parent_id=?)
         AND (? IS NULL OR instr(lower(i.title),lower(?))>0)
         AND (? OR i.title>? OR (i.title=? AND i.id>?))
         ORDER BY i.title,i.id LIMIT ?"
    );
    let (after_title, after_id) = after.clone().unwrap_or_default();
    let mut tx = app.db.begin().await?;
    let rows: Vec<ItemRow> = sqlx::query_as(&sql)
        .bind(all)
        .bind(all)
        .bind(&ids)
        .bind(&filter.library_id)
        .bind(&filter.library_id)
        .bind(all)
        .bind(&ids)
        .bind(kind_ok)
        .bind(media_type)
        .bind(media_type)
        .bind(family)
        .bind(family)
        .bind(&filter.parent_id)
        .bind(&filter.parent_id)
        .bind(&filter.q)
        .bind(&filter.q)
        .bind(after.is_none())
        .bind(&after_title)
        .bind(&after_title)
        .bind(&after_id)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let cursor = encode_cursor(&row.title, &row.id);
        items.push((cursor, item_body(&mut tx, &scope, row).await?));
    }
    let page = page(&mut tx, &caller.principal, items, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

pub async fn list_items(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<ItemQuery>,
) -> Result<Json<Page<CatalogItemBody>>, Problem> {
    caller.require(Permission::CatalogRead)?;
    if matches!(q.sort, Some(Sort::RecentlyAdded | Sort::ReleaseDate)) {
        return Err(Problem::bad(
            "unsupported_sort",
            "This catalog records no addition or release dates; sort by title",
        ));
    }
    let filter = Filter {
        library_id: q.library_id,
        kind: q.kind,
        parent_id: q.parent_id,
        q: q.q,
    };
    let page = PageQuery {
        cursor: q.cursor,
        limit: q.limit,
    };
    browse(&app, &caller, filter, page).await
}

/// Searches the effective (metadata-resolved) title. Aliases, people and tags
/// are not indexed on this branch.
pub async fn search(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Page<CatalogItemBody>>, Problem> {
    let filter = Filter {
        library_id: q.library_id,
        q: Some(q.q),
        ..Default::default()
    };
    let page = PageQuery {
        cursor: q.cursor,
        limit: q.limit,
    };
    browse(&app, &caller, filter, page).await
}

pub async fn get_item(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let (row, _) = load_item(&mut tx, &scope, &id, true).await?;
    let revision = row.revision as u64;
    let body = item_body(&mut tx, &scope, row).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

// ---------------------------------------------------------------------------
// Item creation and replacement

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogItemInput {
    kind: MediaKind,
    title: String,
    library_ids: Vec<String>,
}

/// Facts the core hierarchy rule needs for `id` becoming `media_type` under
/// `parent` with `number`.
async fn validate_structure(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
    media_type: &str,
    parent: Option<&str>,
    number: Option<i64>,
) -> Result<(), Problem> {
    let parent_kind: Option<String> = match parent {
        Some(parent) => {
            sqlx::query_scalar("SELECT media_type FROM item_structure WHERE item_id=?")
                .bind(parent)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    let children: Vec<String> =
        sqlx::query_scalar("SELECT media_type FROM item_structure WHERE parent_id=?")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?;
    let editions: i64 = sqlx::query_scalar("SELECT count(*) FROM editions WHERE item_id=?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    playscale_core::catalog::validate_structure(
        media_type,
        parent_kind.as_deref(),
        number,
        parent == Some(id),
        &children,
        editions,
    )
    .map_err(|detail| Problem::invalid("invalid_structure", detail))
}

pub async fn create_item(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<CatalogItemInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let key = idempotency_key(&headers)?;
    let (input, digest) = (body.value, body.digest);
    let title = text("title", &input.title, 500)?;
    valid_ids("library_ids", &input.library_ids, 100)?;
    // A new item has no files, so it belongs to no library.
    if !input.library_ids.is_empty() {
        return Err(derived_membership());
    }
    let (media_type, family) = storage_kind(input.kind).ok_or_else(unsupported_kind)?;
    if family.is_some_and(|f| f != "video") {
        return Err(unsupported_kind());
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    if !access::may_curate(&scope, &BTreeSet::new()) {
        return Err(outside_scope());
    }
    let record = Scope {
        principal: &principal.id,
        operation: "createCatalogItem",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &digest, now())?
        == Idempotent::Replay
    {
        return item_receipt(&mut tx, &scope, stored.unwrap()).await;
    }
    let id = new_id();
    validate_structure(&mut tx, &id, media_type, None, None).await?;
    sqlx::query("INSERT INTO items (id,title,kind) VALUES (?,?,'video')")
        .bind(&id)
        .bind(&title)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO item_origins (item_id,title) VALUES (?,?)")
        .bind(&id)
        .bind(&title)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO item_structure (item_id,media_type,parent_id,number,revision) VALUES (?,?,NULL,NULL,1)")
        .bind(&id)
        .bind(media_type)
        .execute(&mut *tx)
        .await?;
    crate::search::refresh(&mut tx, 100)
        .await
        .map_err(Problem::internal)?;
    let (json, revision) = item_response(&mut tx, &scope, &id).await?;
    record
        .save(&mut tx, &digest, StatusCode::CREATED, Some(&json), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &json, revision))
}

/// Replaces the curated title (the origin the metadata projection falls back
/// to) and the logical kind. Hierarchy placement is kept; library membership
/// is derived from files and must be sent unchanged.
pub async fn replace_item(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<CatalogItemInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let input = body.value;
    let title = text("title", &input.title, 500)?;
    valid_ids("library_ids", &input.library_ids, 100)?;
    let (media_type, family) = storage_kind(input.kind).ok_or_else(unsupported_kind)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let row = curatable_item(&mut tx, &scope, &id, true).await?;
    let current: BTreeSet<String> = libraries(&files_of(&mut tx, &row.id).await?);
    let requested: BTreeSet<String> = input.library_ids.into_iter().collect();
    if requested != current {
        return Err(derived_membership());
    }
    if family.is_some_and(|f| f != row.kind) {
        return Err(unsupported_kind());
    }
    let (origin, number): (String, Option<i64>) = sqlx::query_as(
        "SELECT coalesce(o.title,i.title),s.number FROM items i LEFT JOIN item_origins o ON o.item_id=i.id LEFT JOIN item_structure s ON s.item_id=i.id WHERE i.id=?",
    )
    .bind(&row.id)
    .fetch_one(&mut *tx)
    .await?;
    let restructure = media_type != row.media_type;
    let retitle = origin != title;
    let current_revision = row.revision as u64;
    if let Revised::Next(next) = access::revise(current_revision, expected, restructure || retitle)?
    {
        if restructure {
            validate_structure(
                &mut tx,
                &row.id,
                media_type,
                row.parent_id.as_deref(),
                number,
            )
            .await?;
            sqlx::query("INSERT INTO item_structure (item_id,media_type,parent_id,number,revision) VALUES (?,?,?,?,1) ON CONFLICT(item_id) DO UPDATE SET media_type=excluded.media_type,revision=item_structure.revision+1")
                .bind(&row.id)
                .bind(media_type)
                .bind(&row.parent_id)
                .bind(number)
                .execute(&mut *tx)
                .await?;
        }
        if retitle {
            sqlx::query("INSERT INTO item_origins (item_id,title) VALUES (?,?) ON CONFLICT(item_id) DO UPDATE SET title=excluded.title")
                .bind(&row.id)
                .bind(&title)
                .execute(&mut *tx)
                .await?;
            crate::metadata::project_title(&mut tx, &row.id)
                .await
                .map_err(Problem::internal)?;
        }
        // Triggers advanced the revision per row change; the committed
        // revision of this replacement is exactly one past the reviewed one.
        sqlx::query("UPDATE items SET catalog_revision=? WHERE id=?")
            .bind(i64::try_from(next).map_err(Problem::internal)?)
            .bind(&row.id)
            .execute(&mut *tx)
            .await?;
        crate::search::refresh(&mut tx, 100)
            .await
            .map_err(Problem::internal)?;
    }
    let (json, revision) = item_response(&mut tx, &scope, &row.id).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &json, revision))
}

// ---------------------------------------------------------------------------
// Editions

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditionKind {
    MovieCut,
    SeriesRelease,
    Default,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditionInput {
    label: String,
    kind: EditionKind,
}

#[derive(Serialize)]
pub struct EditionBody {
    id: String,
    revision: String,
    item_id: String,
    label: String,
    /// Legacy editions record no cut/release kind.
    kind: &'static str,
    timeline_ids: Vec<String>,
}

fn edition_body(id: String, item_id: String, label: String, revision: i64) -> EditionBody {
    EditionBody {
        timeline_ids: vec![id.clone()],
        id,
        revision: revision.to_string(),
        item_id,
        label,
        kind: "default",
    }
}

pub async fn list_editions(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<EditionBody>>, Problem> {
    caller.require(Permission::CatalogRead)?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    let mut tx = app.db.begin().await?;
    let (item, _) = load_item(&mut tx, &scope, &id, true).await?;
    // An edition whose files are all outside the scope is not disclosed.
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(&format!(
        "SELECT e.id,e.item_id,e.label,e.revision FROM editions e WHERE e.item_id=? AND e.id>?
         AND (? OR NOT EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0)
              OR EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0 AND {clause}))
         ORDER BY e.id LIMIT ?"
    ))
    .bind(&item.id)
    .bind(q.after()?)
    .bind(all)
    .bind(all)
    .bind(ids)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let mut output = Vec::new();
    for (id, item_id, label, revision) in rows {
        let mut edition = edition_body(id.clone(), item_id, label, revision);
        edition.timeline_ids =
            sqlx::query_scalar("SELECT id FROM timelines WHERE edition_id=? ORDER BY id")
                .bind(&id)
                .fetch_all(&mut *tx)
                .await?;
        output.push((id, edition));
    }
    let rows = output;
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

/// Creates an empty edition under the item's aggregate ETag.
pub async fn create_edition(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<EditionInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let (input, digest) = (body.value, body.digest);
    let label = text("label", &input.label, 200)?;
    if input.kind != EditionKind::Default {
        return Err(Problem::invalid(
            "unsupported_edition_kind",
            "This catalog records untyped editions only; send kind \"default\"",
        ));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let record = Scope {
        principal: &principal.id,
        operation: "createEdition",
        target: &id,
        key: &key,
    };
    let row = curatable_item(&mut tx, &scope, &id, true).await?;
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &digest, now())?
        == Idempotent::Replay
    {
        return Ok(replayed(stored.unwrap()));
    }
    access::revise(row.revision as u64, expected, true)?;
    if !playscale_core::catalog::can_own_editions(&row.media_type) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "container_item",
            "Series and seasons cannot own editions",
        ));
    }
    let edition = edition_body(new_id(), row.id.clone(), label, 1);
    crate::curation::create_edition(&mut tx, &edition.id, &edition.item_id, &edition.label)
        .await
        .map_err(Problem::internal)?;
    let json = serde_json::to_value(&edition).map_err(Problem::internal)?;
    record
        .save(&mut tx, &digest, StatusCode::CREATED, Some(&json), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &json, 1))
}

// Saved responses do not freeze a principal's authorization. Recheck every
// existing identity named by a plan before returning its signed snapshot.
async fn authorize_plan_receipt(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    body: &Value,
) -> Result<(), Problem> {
    for id in body["affected_ids"]
        .as_array()
        .ok_or_else(|| Problem::internal("invalid plan receipt"))?
    {
        let id = id
            .as_str()
            .ok_or_else(|| Problem::internal("invalid plan identity"))?;
        let owners: Vec<String> = sqlx::query_scalar("SELECT id FROM items WHERE id=? UNION SELECT item_id FROM editions WHERE id=? UNION SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=? UNION SELECT e.item_id FROM media_versions v JOIN timelines t ON t.id=v.timeline_id JOIN editions e ON e.id=t.edition_id WHERE v.id=?")
            .bind(id).bind(id).bind(id).bind(id).fetch_all(&mut *conn).await?;
        for owner in owners {
            curatable_item(conn, scope, &owner, true).await?;
        }
    }
    Ok(())
}

async fn item_receipt(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    mut stored: Stored,
) -> Result<Response, Problem> {
    let body = stored
        .body
        .as_mut()
        .ok_or_else(|| Problem::internal("missing item receipt"))?;
    let id = body["id"]
        .as_str()
        .ok_or_else(|| Problem::internal("invalid item receipt"))?;
    let (current, _) = item_response(conn, scope, id).await?;
    for field in ["library_ids", "parent_ids"] {
        if let Some(ids) = body[field].as_array_mut() {
            ids.retain(|id| {
                current[field]
                    .as_array()
                    .is_some_and(|visible| visible.contains(id))
            });
        }
    }
    for field in ["default_timeline_id", "artwork_id"] {
        if body[field] != current[field] {
            body[field] = Value::Null;
        }
    }
    Ok(replayed(stored))
}

// ---------------------------------------------------------------------------
// Reviewed merge/split plans
//
// A plan token is the reviewed core plan, its principal and expiry, signed
// with the server credential key. It names a proposal only: commit
// re-authorizes, rechecks scope and lets the core decision recompute the plan
// against current revisions before anything is applied.

const PLAN_PREFIX: &str = "mcp_";
const PLAN_TOKEN_MAX: usize = 8192;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Reviewed {
    Merge { plan: MergePlan },
    Split { plan: SplitPlan },
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    principal: String,
    expires_at: i64,
    change: Reviewed,
}

fn plan_mac(app: &App, payload: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(&app.access.key).expect("HMAC accepts any key length");
    mac.update(b"catalog-plan:");
    mac.update(payload.as_bytes());
    format!("{:x}", mac.finalize().into_bytes())
}

fn sign_plan(app: &App, envelope: &Envelope) -> Result<String, Problem> {
    let payload = b64_encode(
        serde_json::to_string(envelope)
            .map_err(Problem::internal)?
            .as_bytes(),
    );
    let token = format!("{PLAN_PREFIX}{payload}.{}", plan_mac(app, &payload));
    if token.len() > PLAN_TOKEN_MAX {
        return Err(Problem::invalid(
            "plan_too_large",
            "The plan is too large for one token; select fewer participants",
        ));
    }
    Ok(token)
}

fn invalid_plan() -> Problem {
    Problem::invalid(
        "invalid_plan_token",
        "The plan token is not a plan issued to this principal",
    )
}

fn open_plan(app: &App, token: &str, principal: &str) -> Result<Envelope, Problem> {
    let (payload, mac) = token
        .strip_prefix(PLAN_PREFIX)
        .and_then(|t| t.split_once('.'))
        .ok_or_else(invalid_plan)?;
    // Digest comparison keeps the MAC check independent of the mismatch position.
    if Sha256::digest(plan_mac(app, payload).as_bytes()) != Sha256::digest(mac.as_bytes()) {
        return Err(invalid_plan());
    }
    let envelope: Envelope = b64_decode(payload)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(invalid_plan)?;
    if envelope.principal != principal {
        return Err(invalid_plan());
    }
    if !access::plan_token_current(envelope.expires_at, now()) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "plan_expired",
            "The plan expired; preview the change again",
        ));
    }
    Ok(envelope)
}

#[derive(Serialize)]
struct ChangePlanBody {
    plan_token: String,
    expires_at: String,
    affected_ids: Vec<String>,
    warnings: Vec<String>,
}

fn dedup(ids: impl IntoIterator<Item = String>) -> Result<Vec<String>, Problem> {
    let mut seen = BTreeSet::new();
    let ids: Vec<String> = ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();
    if ids.len() > 1000 {
        return Err(Problem::invalid(
            "plan_too_large",
            "The plan affects too many resources; select fewer participants",
        ));
    }
    Ok(ids)
}

fn merge_affected(plan: &MergePlan) -> Result<Vec<String>, Problem> {
    dedup(
        [plan.target.clone()]
            .into_iter()
            .chain(plan.retired.iter().cloned())
            .chain(plan.moved_editions.iter().map(|(e, _)| e.clone()))
            .chain(plan.repointed_aliases.iter().cloned()),
    )
}

fn split_affected(plan: &SplitPlan) -> Result<Vec<String>, Problem> {
    let mut ids = vec![plan.item.clone(), plan.new_item.clone()];
    for step in &plan.moves {
        match step {
            SplitMove::Edition { edition } => ids.push(edition.clone()),
            SplitMove::Timelines {
                from_edition,
                new_edition,
                timelines,
            } => {
                ids.extend([from_edition.clone(), new_edition.clone()]);
                ids.extend(timelines.iter().cloned());
            }
            SplitMove::Versions {
                from_timeline,
                new_edition,
                new_timeline,
                versions,
            } => {
                ids.extend([
                    from_timeline.clone(),
                    new_edition.clone(),
                    new_timeline.clone(),
                ]);
                ids.extend(versions.iter().cloned());
            }
        }
    }
    dedup(ids)
}

enum Preview {
    Merge(MergeRequest),
    Split(SplitRequest),
}

/// Scope-checks every participant, then asks the core (through the curation
/// service) for the plan against current revisions.
async fn plan(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    preview: &Preview,
) -> Result<(Reviewed, Vec<String>, Vec<String>), Problem> {
    match preview {
        Preview::Merge(request) => {
            let participants: BTreeSet<&String> =
                request.sources.iter().chain([&request.target]).collect();
            for participant in participants {
                curatable_item(conn, scope, participant, false).await?;
            }
            let plan = crate::curation::preview_merge_on(conn, request)
                .await
                .map_err(curation_problem)?;
            let affected = merge_affected(&plan)?;
            let warnings = plan.warnings.clone();
            Ok((Reviewed::Merge { plan }, affected, warnings))
        }
        Preview::Split(request) => {
            curatable_item(conn, scope, &request.item, false).await?;
            let plan = crate::curation::preview_split_on(conn, request)
                .await
                .map_err(curation_problem)?;
            let affected = split_affected(&plan)?;
            let warnings = plan.warnings.clone();
            Ok((Reviewed::Split { plan }, affected, warnings))
        }
    }
}

/// Plans are previewed inside a writer transaction so the acknowledgement
/// and the reviewed revisions come from one snapshot.
async fn issue_plan(
    app: &App,
    caller: &Caller,
    headers: &HeaderMap,
    operation: &'static str,
    target: &str,
    digest: &str,
    preview: Preview,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let key = idempotency_key(headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let record = Scope {
        principal: &principal.id,
        operation,
        target,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), digest, now())? == Idempotent::Replay
    {
        let stored = stored.unwrap();
        authorize_plan_receipt(
            &mut tx,
            &scope,
            stored
                .body
                .as_ref()
                .ok_or_else(|| Problem::internal("missing plan receipt"))?,
        )
        .await?;
        return Ok(replayed(stored));
    }
    let (change, affected_ids, warnings) = plan(&mut tx, &scope, &preview).await?;
    let expires_at = now() + access::PLAN_TOKEN_TTL_SECONDS;
    let plan_token = sign_plan(
        app,
        &Envelope {
            principal: principal.id.clone(),
            expires_at,
            change,
        },
    )?;
    let body = ChangePlanBody {
        plan_token,
        expires_at: timestamp(expires_at),
        affected_ids,
        warnings,
    };
    let json = serde_json::to_value(&body).map_err(Problem::internal)?;
    record
        .save(&mut tx, digest, StatusCode::OK, Some(&json), None)
        .await?;
    tx.commit().await?;
    Ok(Json(json).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergePreviewInput {
    source_item_ids: Vec<String>,
    target_item_id: String,
    expected_revisions: BTreeMap<String, String>,
}

pub async fn preview_merge(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<MergePreviewInput>,
) -> Result<Response, Problem> {
    let (input, digest) = (body.value, body.digest);
    valid_ids(
        "source_item_ids",
        &input.source_item_ids,
        playscale_core::identity::MAX_SELECTION,
    )?;
    let expected_ids: Vec<String> = input.expected_revisions.keys().cloned().collect();
    valid_ids(
        "expected_revisions",
        &expected_ids,
        playscale_core::identity::MAX_SELECTION + 1,
    )?;
    if !valid_id(&input.target_item_id) {
        return Err(Problem::invalid(
            "invalid_field",
            "target_item_id must be a valid ID",
        ));
    }
    if id != input.target_item_id && !input.source_item_ids.contains(&id) {
        return Err(Problem::invalid(
            "invalid_field",
            "The path item must be the merge target or one of its sources",
        ));
    }
    let mut expected = BTreeMap::new();
    for (item, revision) in &input.expected_revisions {
        expected.insert(item.clone(), revision_text("expected_revisions", revision)?);
    }
    let request = MergeRequest {
        sources: input.source_item_ids,
        target: input.target_item_id,
        expected,
    };
    issue_plan(
        &app,
        &caller,
        &headers,
        "previewMerge",
        &id,
        &digest,
        Preview::Merge(request),
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitPreviewInput {
    version_ids: Vec<String>,
    new_title: String,
    expected_revision: String,
}

pub async fn preview_split(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<SplitPreviewInput>,
) -> Result<Response, Problem> {
    let (input, digest) = (body.value, body.digest);
    valid_ids(
        "version_ids",
        &input.version_ids,
        playscale_core::identity::MAX_SELECTION,
    )?;
    let request = SplitRequest {
        item: id.clone(),
        versions: input.version_ids,
        new_title: input.new_title,
        expected_revision: revision_text("expected_revision", &input.expected_revision)?,
    };
    issue_plan(
        &app,
        &caller,
        &headers,
        "previewSplit",
        &id,
        &digest,
        Preview::Split(request),
    )
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitChange {
    plan_token: String,
}

/// Commits a reviewed plan and returns the resulting work: the merge target,
/// or the new work created by a split.
pub async fn commit_reconciliation(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<CommitChange>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let key = idempotency_key(&headers)?;
    let (input, digest) = (body.value, body.digest);
    if !(16..=PLAN_TOKEN_MAX).contains(&input.plan_token.len()) {
        return Err(invalid_plan());
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let record = Scope {
        principal: &principal.id,
        operation: "commitReconciliation",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &digest, now())?
        == Idempotent::Replay
    {
        return item_receipt(&mut tx, &scope, stored.unwrap()).await;
    }
    let envelope = open_plan(&app, &input.plan_token, &principal.id)?;
    let result = match &envelope.change {
        Reviewed::Merge { plan } => {
            let participants: BTreeSet<&String> =
                plan.retired.iter().chain([&plan.target]).collect();
            for participant in participants {
                curatable_item(&mut tx, &scope, participant, false).await?;
            }
            crate::curation::merge_in_transaction(&mut tx, plan)
                .await
                .map_err(curation_problem)?;
            plan.target.clone()
        }
        Reviewed::Split { plan } => {
            curatable_item(&mut tx, &scope, &plan.item, false).await?;
            crate::curation::split_in_transaction(&mut tx, plan)
                .await
                .map_err(curation_problem)?;
            plan.new_item.clone()
        }
    };
    let (json, revision) = item_response(&mut tx, &scope, &result).await?;
    record
        .save(&mut tx, &digest, StatusCode::OK, Some(&json), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &json, revision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips() {
        for input in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(b64_decode(&b64_encode(input)).unwrap(), input);
        }
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert!(b64_decode("a").is_none());
        assert!(b64_decode("a+").is_none());
    }

    #[test]
    fn kinds_map_both_ways() {
        assert_eq!(media_kind("unclassified", "video"), "video");
        assert_eq!(media_kind("unclassified", "audio"), "other");
        assert_eq!(media_kind("episode", "video"), "episode");
        assert_eq!(
            storage_kind(MediaKind::Video),
            Some(("unclassified", Some("video")))
        );
        assert_eq!(storage_kind(MediaKind::Album), None);
    }

    #[test]
    fn revisions_are_canonical_decimal() {
        assert_eq!(revision_text("r", "0").unwrap(), 0);
        assert_eq!(revision_text("r", "12").unwrap(), 12);
        for bad in ["", "01", "-1", "1.0", "99999999999999999999"] {
            assert!(revision_text("r", bad).is_err(), "{bad}");
        }
    }
}

// Real timeline/version reads share the item's authorized snapshot. Version
// bindings are indivisible: a restricted reader must see every bound file,
// rather than receiving an apparently complete multipart version with gaps.
#[derive(sqlx::FromRow)]
struct TimelineRow {
    id: String,
    revision: i64,
    item_id: String,
    edition_id: String,
    duration_ms: Option<i64>,
    order_group_id: Option<String>,
    order_position: Option<i64>,
}

async fn version_ids(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    timeline: &str,
    after: &str,
    limit: i64,
) -> Result<Vec<String>, Problem> {
    let (clause, all, ids) = scope_clause(scope, "f.library_id");
    Ok(sqlx::query_scalar(&format!("SELECT v.id FROM media_versions v WHERE v.timeline_id=? AND v.id>? AND (? OR (EXISTS(SELECT 1 FROM version_files b WHERE b.version_id=v.id) AND NOT EXISTS(SELECT 1 FROM version_files b JOIN media_files f ON f.id=b.file_id WHERE b.version_id=v.id AND NOT {clause}))) ORDER BY v.id LIMIT ?"))
        .bind(timeline).bind(after).bind(all).bind(all).bind(ids).bind(limit).fetch_all(conn).await?)
}

async fn timeline_body(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    row: TimelineRow,
) -> Result<serde_json::Value, Problem> {
    let versions = version_ids(conn, scope, &row.id, "", 101).await?;
    if versions.len() > 100 {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "timeline_too_large",
            "Timeline exceeds the public representation bound",
        ));
    }
    Ok(
        serde_json::json!({"id":row.id,"revision":row.revision.to_string(),"item_id":row.item_id,"edition_id":row.edition_id,"duration_ms":row.duration_ms,"version_ids":versions,"order_group_id":row.order_group_id,"order_position":row.order_position}),
    )
}

pub async fn list_timelines(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let (item, _) = load_item(&mut tx, &scope, &id, true).await?;
    let limit = q.limit()?;
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    let rows:Vec<TimelineRow>=sqlx::query_as(&format!("SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE e.item_id=? AND t.id>? AND (? OR NOT EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0) OR EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0 AND {clause})) ORDER BY t.id LIMIT ?"))
        .bind(item.id).bind(q.after()?).bind(all).bind(all).bind(ids).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let mut values = Vec::new();
    for row in rows {
        values.push((row.id.clone(), timeline_body(&mut tx, &scope, row).await?));
    }
    let value = page(&mut tx, &caller.principal, values, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

pub async fn get_timeline(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let row: TimelineRow = sqlx::query_as(
        "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Problem::not_found)?;
    load_item(&mut tx, &scope, &row.item_id, false).await?;
    require_edition(&mut tx, &scope, &row.edition_id).await?;
    let revision = row.revision as u64;
    let body = timeline_body(&mut tx, &scope, row).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

pub async fn list_versions(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let item: String = sqlx::query_scalar(
        "SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Problem::not_found)?;
    load_item(&mut tx, &scope, &item, false).await?;
    let edition: String = sqlx::query_scalar("SELECT edition_id FROM timelines WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    require_edition(&mut tx, &scope, &edition).await?;
    let limit = q.limit()?;
    let ids = version_ids(&mut tx, &scope, &id, q.after()?, i64::from(limit) + 1).await?;
    let mut values = Vec::new();
    for version in ids {
        let (revision, label, origin, equivalence): (i64, String, String, String) = sqlx::query_as(
            "SELECT revision,label,origin,equivalence FROM media_versions WHERE id=?",
        )
        .bind(&version)
        .fetch_one(&mut *tx)
        .await?;
        type BindingRow = (String, String, i64, Option<i64>, Option<i64>, bool, String);
        let bindings: Vec<BindingRow>=sqlx::query_as("SELECT b.file_id,b.file_revision,b.part,b.start_ms,b.end_ms,f.available,f.revision FROM version_files b JOIN media_files f ON f.id=b.file_id WHERE b.version_id=? ORDER BY b.part LIMIT 101")
            .bind(&version).fetch_all(&mut *tx).await?;
        if bindings.len() > 100 {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "version_too_large",
                "Version exceeds the public representation bound",
            ));
        }
        let core_bindings: Vec<_> = bindings
            .iter()
            .map(
                |(file, revision, part, start, end, _, _)| playscale_core::identity::Binding {
                    file_id: file.clone(),
                    revision: revision.clone(),
                    part: *part as u32,
                    start_ms: start.map(|v| v as u64),
                    end_ms: end.map(|v| v as u64),
                },
            )
            .collect();
        let mut occurrences: Vec<_> = bindings
            .iter()
            .map(
                |(file, _, _, _, _, available, current)| playscale_core::identity::Occurrence {
                    file_id: file.clone(),
                    revision: current.clone(),
                    available: *available,
                },
            )
            .collect();
        let (clause, all, scope_ids) = scope_clause(&scope, "f.library_id");
        for binding in &core_bindings {
            let copy:Option<(String,String)>=sqlx::query_as(&format!("SELECT f.id,f.revision FROM media_files f WHERE f.available=1 AND f.revision=? AND {clause} ORDER BY f.id LIMIT 1"))
                .bind(&binding.revision).bind(all).bind(&scope_ids).fetch_optional(&mut *tx).await?;
            if let Some((file_id, revision)) = copy {
                occurrences.push(playscale_core::identity::Occurrence {
                    file_id,
                    revision,
                    available: true,
                });
            }
        }
        let available = !core_bindings.is_empty()
            && playscale_core::identity::version_availability(&core_bindings, &occurrences)
                == playscale_core::identity::Availability::Available;
        let files:Vec<_>=bindings.into_iter().map(|(file,revision,part,start,end,_,_)|serde_json::json!({"file":{"file_id":file,"file_revision":revision},"part":part,"start_ms":start,"end_ms":end})).collect();
        values.push((version.clone(),serde_json::json!({"id":version,"revision":revision.to_string(),"timeline_id":id,"label":label,"origin":origin,"files":files,"timeline_equivalence":equivalence,"availability":if available {"available"} else {"unavailable"}})));
    }
    let value = page(&mut tx, &caller.principal, values, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

async fn require_edition(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    id: &str,
) -> Result<(), Problem> {
    let (clause, all, ids) = scope_clause(scope, "f.library_id");
    let visible:bool=sqlx::query_scalar(&format!("SELECT (? OR NOT EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=? AND f.generated=0) OR EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=? AND f.generated=0 AND {clause}))"))
 .bind(all).bind(id).bind(id).bind(all).bind(ids).fetch_one(conn).await?;
    if visible {
        Ok(())
    } else {
        Err(Problem::not_found())
    }
}
