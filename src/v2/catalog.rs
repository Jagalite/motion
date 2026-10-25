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
mod native_details;
mod native_editions;
mod native_relationship_writes;
mod native_relationships;

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
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/catalog/items", get(list_items).post(create_item))
        .route("/catalog/search", get(search))
        .route("/catalog/items/{item_id}/files", get(list_item_files))
        .route("/admin/files", get(list_admin_files))
        .route("/catalog/items/{item_id}", get(get_item).put(replace_item))
        .route(
            "/catalog/items/{item_id}/editions",
            get(list_editions).post(create_edition),
        )
        .route(
            "/catalog/items/{item_id}/timelines",
            get(list_timelines).post(create_timeline),
        )
        .route(
            "/catalog/items/{item_id}/order-groups",
            get(list_order_groups).post(create_order_group),
        )
        .route("/catalog/timelines/{timeline_id}", get(get_timeline))
        .route(
            "/catalog/timelines/{timeline_id}/order",
            axum::routing::put(place_timeline),
        )
        .route(
            "/catalog/timelines/{timeline_id}/versions",
            get(list_versions).post(create_version),
        )
        .route("/catalog/items/{item_id}/merge-plans", post(preview_merge))
        .route("/catalog/items/{item_id}/split-plans", post(preview_split))
        .route("/catalog/reconciliations", post(commit_reconciliation))
        .route(
            "/catalog/items/{item_id}/relationships",
            get(list_relationships).post(create_relationship),
        )
        .route(
            "/catalog/relationships/{id}",
            get(get_relationship).delete(delete_relationship),
        )
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

pub(super) fn valid_id(id: &str) -> bool {
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

pub(super) fn b64_encode(bytes: &[u8]) -> String {
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

pub(super) fn b64_decode(text: &str) -> Option<Vec<u8>> {
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
pub(crate) struct ItemRow {
    pub(crate) id: String,
    title: String,
    kind: String,
    media_type: String,
    parent_id: Option<String>,
    pub(crate) revision: i64,
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
pub(crate) async fn load_item(
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
pub(crate) async fn curatable_item(
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

/// Every work whose relationships or order membership a merge can mutate
/// must be writable. Call under the same writer reservation as the mutation.
pub(crate) async fn authorize_merge_scope(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    sources: &[String],
    target: &str,
) -> Result<(), Problem> {
    let mut affected: BTreeSet<String> = sources.iter().cloned().collect();
    affected.insert(target.to_owned());
    for source in sources.iter().filter(|source| source.as_str() != target) {
        let owners: Vec<String> = sqlx::query_scalar(
            "SELECT source_item_id FROM item_relationships WHERE source_item_id=? OR target_item_id=? UNION SELECT target_item_id FROM item_relationships WHERE source_item_id=? OR target_item_id=? UNION SELECT owner.item_id FROM timelines t JOIN editions moved ON moved.id=t.edition_id JOIN order_groups g ON g.id=t.order_group_id JOIN editions owner ON owner.id=g.edition_id WHERE moved.item_id=?",
        )
        .bind(source).bind(source).bind(source).bind(source).bind(source)
        .fetch_all(&mut *conn).await?;
        affected.extend(owners);
    }
    for item in affected {
        curatable_item(conn, scope, &item, false).await?;
    }
    Ok(())
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
impl CatalogItemBody {
    pub(super) fn library_ids(&self) -> &[String] {
        &self.library_ids
    }
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

pub(crate) async fn item_body(
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
    let artwork_id = super::metadata::resolved_artwork(conn, &row.id, "poster")
        .await?
        .asset_id;
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
    /// Comma-separated live item IDs (at most 100): a batched read. Merged
    /// aliases and unreadable or unknown IDs match nothing.
    ids: Option<String>,
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
    /// A JSON array of item IDs, when filtering by identity.
    ids: Option<String>,
}

/// The `ids` query: 1-100 distinct valid IDs, as a JSON array for `json_each`.
fn id_list(ids: &Option<String>) -> Result<Option<String>, Problem> {
    let Some(ids) = ids else { return Ok(None) };
    let list: Vec<String> = ids.split(',').map(str::to_owned).collect();
    if list.len() > 100
        || list.iter().any(|id| !valid_id(id))
        || list.iter().collect::<std::collections::BTreeSet<_>>().len() != list.len()
    {
        return Err(Problem::bad(
            "invalid_query",
            "ids must list 1-100 distinct valid IDs",
        ));
    }
    Ok(Some(
        serde_json::to_string(&list).map_err(Problem::internal)?,
    ))
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
    if app.native_catalog.is_some() {
        return browse_native(app, caller, filter, q).await;
    }
    let scope = access::catalog_scope(&caller.principal);
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    // The batched ID filter is emitted only when given, as a direct predicate
    // SQLite resolves by primary-key lookups (an optional `OR` would scan).
    let by_id = if filter.ids.is_some() {
        "i.id IN (SELECT value FROM json_each(?))"
    } else {
        "? IS NULL"
    };
    let sql = format!(
        "{ITEM_SELECT} WHERE i.id NOT IN (SELECT alias_id FROM item_aliases)
         AND (? OR EXISTS (SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=i.id AND f.generated=0 AND {clause}))
         AND (? IS NULL OR EXISTS (SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=i.id AND f.generated=0 AND f.library_id IN (SELECT source_id FROM library_sources WHERE library_id=?) AND {clause}))
         AND ? AND (? IS NULL OR coalesce(s.media_type,'unclassified')=?) AND (? IS NULL OR i.kind=?)
         AND (? IS NULL OR s.parent_id=?)
         AND (? IS NULL OR instr(lower(i.title),lower(?))>0)
         AND {by_id}
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
        .bind(&filter.ids)
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
        ids: id_list(&q.ids)?,
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
    if app.native_catalog.is_some() {
        return get_native_item(&app, &caller, &id).await;
    }
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let (row, _) = load_item(&mut tx, &scope, &id, true).await?;
    let revision = row.revision as u64;
    let body = item_body(&mut tx, &scope, row).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

#[track_caller]
pub(super) fn native_unavailable() -> Problem {
    tracing::warn!(location = %std::panic::Location::caller(), "native wire projection refused unsupported or inconsistent facts");
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_projection_unavailable",
        "This catalog scope needs a richer native projection; no legacy catalog data was used",
    )
}
pub(super) fn native_read_error(error: anyhow::Error) -> Problem {
    if error
        .downcast_ref::<crate::native_catalog::ReadFailure>()
        .is_some_and(|e| {
            e.0 == catabolic_store::catalog_scope::ScopeErrorCode::NotFoundOrNotVisible
        })
    {
        Problem::not_found()
    } else {
        tracing::warn!(%error,"native scoped read unavailable");
        native_unavailable()
    }
}
/// Disposable wire projection of a native no-file work with explicit wire
/// identity evidence. Richer records require their separately qualified adapter.
fn plain_native_work(row: &Value) -> Result<CatalogItemBody, Problem> {
    let id = row[0].as_str().ok_or_else(native_unavailable)?;
    let metadata: Value = match row[2].as_str() {
        Some(text) => serde_json::from_str(text).map_err(|_| native_unavailable())?,
        None => row[2].clone(),
    };
    let projection = &metadata["motion_catalog"];
    if projection["contract_version"] != 1
        || projection["item_id"] != id
        || (projection["creation_request_digest"].as_str().is_none()
            && !projection["external_ids"].is_array())
        || !projection["parent_id"].is_null()
    {
        return Err(native_unavailable());
    }
    let mut external_ids = Vec::new();
    if let Some(ids) = projection["external_ids"].as_array() {
        for identity in ids {
            external_ids.push(ExternalIdentity {
                provider: identity["provider"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .into(),
                value: identity["value"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .into(),
                namespace_version: identity["namespace_version"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .into(),
            });
        }
    }
    let kind =
        playscale_core::catalog_kind::from_native(row[1].as_str().ok_or_else(native_unavailable)?)
            .ok_or_else(native_unavailable)?;
    if projection["kind"] != kind {
        return Err(native_unavailable());
    }
    let media_type = projection["media_type"]
        .as_str()
        .ok_or_else(native_unavailable)?;
    let revision = projection["catalog_revision"]
        .as_u64()
        .ok_or_else(native_unavailable)?;
    Ok(CatalogItemBody {
        id: id.into(),
        revision: revision.to_string(),
        kind: media_kind(media_type, kind),
        title: metadata["title"]
            .as_str()
            .ok_or_else(native_unavailable)?
            .into(),
        library_ids: vec![],
        availability: "unknown",
        external_ids,
        artwork_id: None,
        default_timeline_id: None,
        match_state: projection["match_state"]
            .as_str()
            .ok_or_else(native_unavailable)?
            .into(),
        parent_ids: vec![],
    })
}
/// Fields are assembled from native scoped work/library records and current
/// generation-tagged exact physical maps. Legacy metadata, library membership,
/// and aliases are never consulted as authority here.
pub(super) async fn projected_native_work(
    runtime: &crate::native_catalog::Runtime,
    conn: &mut sqlx::SqliteConnection,
    row: &Value,
    native_libraries: &[Value],
) -> Result<CatalogItemBody, Problem> {
    let mut body = plain_native_work(row)?;
    // read_scoped_records independently admitted the exact current native source
    // before permitting this protected field through the projection shape gate.
    let metadata: Value = match row[2].as_str() {
        Some(text) => serde_json::from_str(text).map_err(|_| native_unavailable())?,
        None => row[2].clone(),
    };
    if let Some(value) = metadata.get("motion_artwork") {
        let state = playscale_core::artwork::native::ReferenceState::parse_json(
            &value.to_string(),
            &body.id,
        )
        .map_err(|_| native_unavailable())?;
        body.artwork_id = playscale_core::artwork::native::resolve_role(&state, "poster")
            .map_err(|_| native_unavailable())?
            .asset_id;
    }
    let shape = &row[3];
    if shape["requires_rich_projection"] == false || shape["logical_work_projection"] == true {
        return Ok(body);
    }
    if shape["physical_work_projection"] != true {
        return Err(native_unavailable());
    }
    let native_editions: Vec<String> =
        serde_json::from_value(shape["edition_ids"].clone()).map_err(|_| native_unavailable())?;
    let editions: Vec<String> = native_editions
        .iter()
        .map(|id| {
            id.strip_prefix("motion.edition:")
                .filter(|id| valid_id(id))
                .map(str::to_owned)
                .ok_or_else(native_unavailable)
        })
        .collect::<Result<_, _>>()?;
    if editions.is_empty() {
        return Err(native_unavailable());
    }
    let encoded = serde_json::to_string(&editions).map_err(Problem::internal)?;
    let matching: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM editions WHERE item_id=? AND id IN(SELECT value FROM json_each(?))",
    )
    .bind(&body.id)
    .bind(&encoded)
    .fetch_one(&mut *conn)
    .await?;
    if matching != editions.len() as i64 {
        return Err(native_unavailable());
    }
    let (catalog,incarnation,generation):(String,String,i64)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(&mut *conn).await?;
    let native_versions: Vec<String> =
        serde_json::from_value(shape["version_ids"].clone()).map_err(|_| native_unavailable())?;
    let version_ids: Vec<String> = native_versions
        .iter()
        .map(|id| {
            id.strip_prefix("motion.version:")
                .filter(|id| valid_id(id))
                .map(str::to_owned)
                .ok_or_else(native_unavailable)
        })
        .collect::<Result<_, _>>()?;
    if version_ids.is_empty() {
        return native_details::empty_work(
            runtime,
            conn,
            body,
            shape,
            generation,
            &catalog,
            &incarnation,
        )
        .await;
    }
    let encoded_versions = serde_json::to_string(&version_ids).map_err(Problem::internal)?;
    let manifests:Vec<(String,String,String)>=sqlx::query_as("SELECT version_id,timeline_id,manifest_json FROM catabolic_projection_versions WHERE generation=? AND native_timeline_revision=native_current_timeline_revision AND version_id IN(SELECT value FROM json_each(?)) ORDER BY version_id")
        .bind(generation).bind(&encoded_versions).fetch_all(&mut *conn).await?;
    let current_ids: BTreeSet<_> = manifests.iter().map(|row| row.0.clone()).collect();
    if current_ids.len() != manifests.len() {
        return Err(native_unavailable());
    }
    let missing_ids: Vec<_> = version_ids
        .iter()
        .filter(|id| !current_ids.contains(*id))
        .cloned()
        .collect();
    let unavailable_facts = if !missing_ids.is_empty() {
        let ids = missing_ids.clone();
        Some(
            runtime
                .with_host(move |host| {
                    host.projection_unavailable_versions(generation, &ids)
                        .map_err(|e| anyhow::anyhow!("unavailable native facts: {e:?}"))
                })
                .await
                .map_err(native_read_error)?,
        )
    } else {
        None
    };
    let unavailable_dependencies = if unavailable_facts.is_some() {
        let work = body.id.clone();
        Some(
            runtime
                .with_host(move |host| {
                    let capture = host.capture_work_dependencies(generation, &work)?;
                    Ok(capture.dependencies().clone())
                })
                .await
                .map_err(native_read_error)?,
        )
    } else {
        None
    };
    let mut exact_files = BTreeSet::new();
    let mut generated_files = BTreeSet::new();
    let mut timelines = BTreeSet::new();
    if let Some(facts) = &unavailable_facts {
        if facts["contract_version"] != 1
            || facts["catalog_id"] != catalog
            || facts["incarnation"] != incarnation
            || facts["generation"] != generation
        {
            return Err(native_unavailable());
        }
        let versions = facts["versions"]
            .as_array()
            .ok_or_else(native_unavailable)?;
        if versions.len() != missing_ids.len() {
            return Err(native_unavailable());
        }
        for version in versions {
            let id = version["version_id"]
                .as_str()
                .ok_or_else(native_unavailable)?;
            if !missing_ids.iter().any(|v| v == id)
                || version["association_item_id"] != format!("motion.version:{id}")
                || !native_editions
                    .iter()
                    .any(|v| version["edition_item_id"] == *v)
                || !version["version_revision"].as_i64().is_some_and(|n| n > 0)
                || !version["timeline_revision"].as_i64().is_some_and(|n| n > 0)
                || version["timeline_revision"] != version["current_timeline_revision"]
            {
                return Err(native_unavailable());
            }
            let deps = unavailable_dependencies
                .as_ref()
                .ok_or_else(native_unavailable)?;
            let owner = deps["versions"]
                .as_array()
                .ok_or_else(native_unavailable)?
                .iter()
                .find(|v| v["id"] == id)
                .ok_or_else(native_unavailable)?;
            let head = deps["version_heads"]
                .as_array()
                .ok_or_else(native_unavailable)?
                .iter()
                .find(|v| v["version_id"] == id && v["revision"] == owner["current_revision"])
                .ok_or_else(native_unavailable)?;
            if owner["current_revision"] != version["version_revision"]
                || owner["association_item_id"] != version["association_item_id"]
                || head["timeline_id"] != version["timeline_id"]
                || head["timeline_revision"] != version["timeline_revision"]
            {
                return Err(native_unavailable());
            }
            timelines.insert(
                version["timeline_id"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .to_owned(),
            );
            for binding in version["bindings"]
                .as_array()
                .ok_or_else(native_unavailable)?
            {
                exact_files.insert((
                    binding["file"]["file_id"]
                        .as_str()
                        .ok_or_else(native_unavailable)?
                        .to_owned(),
                    binding["file"]["revision"]
                        .as_str()
                        .ok_or_else(native_unavailable)?
                        .to_owned(),
                ));
            }
        }
    }
    for (version_id, timeline_id, raw) in manifests {
        let envelope: Value = serde_json::from_str(&raw).map_err(|_| native_unavailable())?;
        if envelope["catalog_id"] != catalog
            || envelope["incarnation"] != incarnation
            || envelope["generation"] != generation
        {
            return Err(native_unavailable());
        }
        let candidates = if envelope.get("selection").is_some() {
            &envelope["selection"]["selected_versions"]
        } else {
            &envelope["manifest"]["versions"]
        };
        let candidate = candidates
            .as_array()
            .ok_or_else(native_unavailable)?
            .iter()
            .find(|v| v["version"]["version_id"] == version_id)
            .ok_or_else(native_unavailable)?;
        let edition = candidate["version"]["edition_item_id"]
            .as_str()
            .ok_or_else(native_unavailable)?;
        if !native_editions.iter().any(|v| v == edition) {
            return Err(native_unavailable());
        }
        let origin = motion_catalog_backend::version_proof::origin_in(
            conn,
            &version_id,
            &catalog,
            &incarnation,
            generation,
        )
        .await
        .map_err(native_read_error)?;
        let projected_origin: String =
            sqlx::query_scalar("SELECT origin FROM media_versions WHERE id=?")
                .bind(&version_id)
                .fetch_one(&mut *conn)
                .await?;
        if !matches!(
            (origin, projected_origin.as_str()),
            (
                motion_catalog_backend::version_proof::ProofOrigin::Original,
                "original"
            ) | (
                motion_catalog_backend::version_proof::ProofOrigin::ReceiptVerifiedGenerated,
                "generated"
            )
        ) {
            return Err(native_unavailable());
        }
        let proof = motion_catalog_backend::version_proof::read(
            &envelope,
            &catalog,
            &incarnation,
            generation,
            &catabolic_store::catalog_projection::VersionLineageRequest {
                work_item_id: body.id.clone(),
                edition_item_id: edition.into(),
                version_item_id: format!("motion.version:{version_id}"),
                version_id: version_id.clone(),
            },
            origin,
        )
        .map_err(native_read_error)?;
        let contract = &proof.version["version"];
        if contract["association_item_id"] != format!("motion.version:{version_id}")
            || contract["timeline_id"] != timeline_id
            || !native_editions
                .iter()
                .any(|id| contract["edition_item_id"] == *id)
        {
            return Err(native_unavailable());
        }
        timelines.insert(timeline_id);
        for binding in contract["bindings"]
            .as_array()
            .ok_or_else(native_unavailable)?
        {
            if origin
                == motion_catalog_backend::version_proof::ProofOrigin::ReceiptVerifiedGenerated
            {
                generated_files.insert((
                    binding["file"]["file_id"]
                        .as_str()
                        .ok_or_else(native_unavailable)?
                        .to_owned(),
                    binding["file"]["revision"]
                        .as_str()
                        .ok_or_else(native_unavailable)?
                        .to_owned(),
                ));
            }
            exact_files.insert((
                binding["file"]["file_id"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .to_owned(),
                binding["file"]["revision"]
                    .as_str()
                    .ok_or_else(native_unavailable)?
                    .to_owned(),
            ));
        }
    }
    if exact_files.is_empty() {
        return Err(native_unavailable());
    }
    let files:Vec<(String,String,String,bool,String)>=sqlx::query_as("SELECT f.id,f.library_id,f.edition_id,p.available,p.mapping_json FROM catabolic_projection_files p JOIN media_files f ON f.id=p.file_id WHERE p.generation=? AND f.edition_id IN(SELECT value FROM json_each(?)) ORDER BY f.id")
        .bind(generation).bind(&encoded).fetch_all(&mut *conn).await?;
    if files.is_empty() {
        return Err(native_unavailable());
    }
    let mut source_ids = BTreeSet::new();
    let mut available = 0;
    let mut seen = BTreeSet::new();
    for (id, source, edition, present, raw) in &files {
        let mapping: motion_catalog_backend::occurrence_bridge::OccurrenceMapping =
            serde_json::from_str(raw).map_err(|_| native_unavailable())?;
        if mapping.motion_file["id"] != *id
            || mapping.motion_file["edition_id"] != *edition
            || mapping.source.library_id != *source
        {
            return Err(native_unavailable());
        }
        let exact = (
            mapping.physical.exact.file_id.clone(),
            mapping.physical.exact.revision.clone(),
        );
        if !exact_files.contains(&exact) {
            continue;
        }
        if let Some(facts) = &unavailable_facts {
            for version in facts["versions"]
                .as_array()
                .ok_or_else(native_unavailable)?
            {
                for binding in version["bindings"]
                    .as_array()
                    .ok_or_else(native_unavailable)?
                {
                    if binding["file"]["file_id"] != exact.0
                        || binding["file"]["revision"] != exact.1
                    {
                        continue;
                    }
                    let observed = &binding["observation"]["snapshot"];
                    let status = binding["observation"]["state"]
                        .as_str()
                        .ok_or_else(native_unavailable)?;
                    if binding["profile"] != mapping.source.profile
                        || observed["location"] != mapping.physical.location
                        || observed["path"] != mapping.physical.path
                        || (status == "present") != *present
                        || !matches!(status, "present" | "missing" | "replacement")
                        || !mapping.version_files.iter().any(|v| {
                            v["version_id"] == version["version_id"]
                                && v["part"] == binding["part"]
                                && v["file_id"] == *id
                                && v["file_revision"] == mapping.motion_file["revision"]
                        })
                    {
                        return Err(native_unavailable());
                    }
                }
            }
        }
        // A present native observation is not current local deployment
        // admission. Withdrawal does not advance native generation. Generated
        // output uses its independently receipt-verified private source instead.
        let admitted = if generated_files.contains(&exact) {
            true
        } else if mapping.source.library_id == motion_catalog_backend::generated_source::SOURCE_ID {
            if *present {
                return Err(native_unavailable());
            }
            false
        } else {
            motion_catalog_backend::source_lifecycle::current_in(conn, source)
                .await
                .map_err(native_read_error)?
                .state
                == motion_catalog_backend::source_lifecycle::State::Active
        };
        if !seen.insert(exact) {
            return Err(native_unavailable());
        }
        if admitted {
            source_ids.insert(source.clone());
        }
        available += usize::from(*present && admitted);
    }
    if seen != exact_files {
        return Err(native_unavailable());
    }
    body.availability = if available == seen.len() {
        "available"
    } else if available == 0 {
        "unavailable"
    } else {
        "degraded"
    };
    for library in native_libraries {
        let metadata: Value =
            serde_json::from_str(library[2].as_str().ok_or_else(native_unavailable)?)
                .map_err(|_| native_unavailable())?;
        let definition = &metadata["motion_library"];
        let id = definition["id"].as_str().ok_or_else(native_unavailable)?;
        if definition["contract_version"] != 1 || library[0] != format!("motion.library:{id}") {
            return Err(native_unavailable());
        }
        let ids: Vec<String> = serde_json::from_value(definition["source_ids"].clone())
            .map_err(|_| native_unavailable())?;
        if ids.iter().any(|id| source_ids.contains(id)) {
            body.library_ids.push(id.to_owned());
        }
    }
    body.library_ids.sort();
    body.library_ids.dedup();
    let all_timelines: Vec<String> =
        serde_json::from_value(shape["timeline_ids"].clone()).map_err(|_| native_unavailable())?;
    if all_timelines.iter().any(|id| !valid_id(id))
        || timelines.iter().any(|id| !all_timelines.contains(id))
    {
        return Err(native_unavailable());
    }
    body.default_timeline_id = match all_timelines.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    Ok(body)
}

pub(super) async fn get_native_item(
    app: &App,
    caller: &Caller,
    id: &str,
) -> Result<Response, Problem> {
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let mut authorization = app.db.begin().await?;
    let initial = caller
        .reauthorize(&mut authorization, Some(Permission::CatalogRead))
        .await?;
    let prepared_scope = access::catalog_scope(&initial);
    if prepared_scope == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    authorization.commit().await?;
    let scoped = match &prepared_scope {
        CatalogScope::Libraries(ids) => Some(
            runtime
                .prepare_library_read_scope(ids.clone())
                .await
                .map_err(native_read_error)?,
        ),
        _ => None,
    };
    let selection = match &scoped {
        Some(scope) => scope.selection.clone(),
        None => runtime
            .prepare_all_scope()
            .await
            .map_err(native_read_error)?,
    };
    let library_selection = match &scoped {
        Some(scope) => scope.definitions.clone(),
        None => runtime
            .prepare_library_definitions(None)
            .await
            .map_err(native_read_error)?,
    };
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    let current_scope = access::catalog_scope(&principal);
    if current_scope == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    if current_scope != prepared_scope {
        return Err(native_unavailable());
    }
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(native_read_error)?;
    if access::catalog_scope(&principal) != prepared_scope {
        return Err(native_unavailable());
    }
    let native_libraries = match &scoped {
        Some(scope) => runtime
            .validate_library_read_scope(&mut tx, scope)
            .await
            .map_err(native_read_error)?,
        None if prepared_scope == CatalogScope::Nothing => Vec::new(),
        None => runtime
            .read_libraries(&mut tx, vec![library_selection], None)
            .await
            .map_err(native_read_error)?,
    };
    let rows = runtime
        .read_projectable_items(&mut tx, vec![selection], Some(id.into()))
        .await
        .map_err(native_read_error)?;
    let mut body = projected_native_work(
        runtime,
        &mut tx,
        rows.first().ok_or_else(Problem::not_found)?,
        &native_libraries,
    )
    .await?;
    let issued = runtime
        .issue_work_revisions_and_commit(tx, vec![id.into()])
        .await
        .map_err(native_read_error)?;
    let revision = issued.get(id).ok_or_else(native_unavailable)?.revision;
    body.revision = revision.to_string();
    Ok(tagged(StatusCode::OK, &body, revision))
}
async fn browse_native(
    app: &App,
    caller: &Caller,
    filter: Filter,
    q: PageQuery,
) -> Result<Json<Page<CatalogItemBody>>, Problem> {
    let limit = q.limit()?;
    let after = match q.after()? {
        "" => None,
        cursor => Some(decode_cursor(cursor)?),
    };
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let mut authorization = app.db.begin().await?;
    let initial = caller
        .reauthorize(&mut authorization, Some(Permission::CatalogRead))
        .await?;
    let prepared_scope = access::catalog_scope(&initial);
    if prepared_scope == CatalogScope::Nothing {
        let empty = page(
            &mut authorization,
            &initial,
            Vec::<(String, CatalogItemBody)>::new(),
            limit,
        )
        .await?;
        authorization.commit().await?;
        return Ok(Json(empty));
    }
    authorization.commit().await?;
    let scoped = match &prepared_scope {
        CatalogScope::Libraries(ids) => Some(
            runtime
                .prepare_library_read_scope(ids.clone())
                .await
                .map_err(native_read_error)?,
        ),
        _ => None,
    };
    let selection = match &scoped {
        Some(scope) => scope.selection.clone(),
        None => runtime
            .prepare_all_scope()
            .await
            .map_err(native_read_error)?,
    };
    let library_selection = match &scoped {
        Some(scope) => scope.definitions.clone(),
        None => runtime
            .prepare_library_definitions(None)
            .await
            .map_err(native_read_error)?,
    };
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(native_read_error)?;
    if access::catalog_scope(&principal) != prepared_scope {
        return Err(native_unavailable());
    }
    let native_libraries = match &scoped {
        Some(scope) => runtime
            .validate_library_read_scope(&mut tx, scope)
            .await
            .map_err(native_read_error)?,
        None if prepared_scope == CatalogScope::Nothing => Vec::new(),
        None => runtime
            .read_libraries(&mut tx, vec![library_selection], None)
            .await
            .map_err(native_read_error)?,
    };
    let rows = match access::catalog_scope(&principal) {
        CatalogScope::Nothing => vec![],
        CatalogScope::Libraries(_) | CatalogScope::All => runtime
            .read_projectable_items(&mut tx, vec![selection], None)
            .await
            .map_err(native_read_error)?,
    };
    let ids: Option<Vec<String>> = filter
        .ids
        .as_ref()
        .map(|ids| serde_json::from_str(ids))
        .transpose()
        .map_err(Problem::internal)?;
    let mut items = Vec::new();
    for row in rows {
        let body = projected_native_work(runtime, &mut tx, &row, &native_libraries).await?;
        if filter
            .library_id
            .as_ref()
            .is_some_and(|id| !body.library_ids.contains(id))
            || filter.parent_id.is_some()
        {
            continue;
        }
        if filter.kind.is_some_and(|kind| {
            storage_kind(kind).is_none_or(|(media, family)| {
                media_kind(media, family.unwrap_or("video")) != body.kind
            })
        }) {
            continue;
        }
        if filter
            .q
            .as_ref()
            .is_some_and(|q| !body.title.to_lowercase().contains(&q.to_lowercase()))
        {
            continue;
        }
        if ids.as_ref().is_some_and(|ids| !ids.contains(&body.id)) {
            continue;
        }
        if after
            .as_ref()
            .is_some_and(|(title, id)| (&body.title, &body.id) <= (title, id))
        {
            continue;
        }
        items.push(body);
    }
    items.sort_by(|a, b| (&a.title, &a.id).cmp(&(&b.title, &b.id)));
    let rows = items
        .into_iter()
        .take(limit as usize + 1)
        .map(|body| (encode_cursor(&body.title, &body.id), body))
        .collect();
    let mut page = page(&mut tx, &principal, rows, limit).await?;
    let ids = page
        .items
        .iter()
        .map(|body: &CatalogItemBody| body.id.clone())
        .collect();
    let issued = runtime
        .issue_work_revisions_and_commit(tx, ids)
        .await
        .map_err(native_read_error)?;
    for body in &mut page.items {
        body.revision = issued
            .get(&body.id)
            .ok_or_else(native_unavailable)?
            .revision
            .to_string();
    }
    Ok(Json(page))
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
    if app.native_catalog.is_some() {
        return create_native_work(&app, &caller, &key, &digest, title, media_type).await;
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

/// First native mutation slice. No catalog SQL writes or fallback are allowed.
/// The native intent commits before dispatch; current Motion authorization is
/// reserved again across the native effect and Motion acknowledgment.
async fn create_native_work(
    app: &App,
    caller: &Caller,
    key: &str,
    digest: &str,
    title: String,
    media_type: &str,
) -> Result<Response, Problem> {
    playscale_core::catalog::validate_structure(media_type, None, None, false, &[], 0)
        .map_err(|detail| Problem::invalid("invalid_structure", detail))?;
    let runtime = app
        .native_catalog
        .as_ref()
        .ok_or_else(|| Problem::internal("native runtime missing"))?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    if !matches!(access::catalog_scope(&principal), CatalogScope::All) {
        return Err(outside_scope());
    }
    let record = Scope {
        principal: &principal.id,
        operation: "createCatalogItem",
        target: "-",
        key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), digest, now())? == Idempotent::Replay
    {
        // This receipt is a new, unmapped work with no library/parent/artwork
        // disclosure; current unrestricted authority remains mandatory.
        let response = replayed(stored.unwrap());
        tx.commit().await?;
        runtime
            .rebuild_projection()
            .await
            .map_err(native_read_error)?;
        return Ok(response);
    }
    let operation = format!(
        "server-create-work:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(principal.id.as_str(), key)).map_err(Problem::internal)?
        )
    );
    let id = format!("{:x}", Sha256::digest(operation.as_bytes()));
    tx.commit().await?;
    let command = runtime
        .prepare_work(
            operation,
            id.clone(),
            title.clone(),
            media_type.to_owned(),
            digest.to_owned(),
        )
        .await
        .map_err(Problem::internal)?;
    let (outcome, mut tx, principal, revision_intent) = loop {
        let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
            app.db.begin_with("BEGIN IMMEDIATE").await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::CatalogWrite))
            .await?;
        if !matches!(access::catalog_scope(&principal), CatalogScope::All) {
            return Err(outside_scope());
        }
        let record = Scope {
            principal: &principal.id,
            operation: "createCatalogItem",
            target: "-",
            key,
        };
        let stored = record.load(&mut tx).await?;
        if access::idempotency(stored.as_ref().map(|s| &s.record), digest, now())?
            == Idempotent::Replay
        {
            let response = replayed(stored.unwrap());
            tx.commit().await?;
            runtime
                .rebuild_projection()
                .await
                .map_err(native_read_error)?;
            return Ok(response);
        }
        use catabolic_store::catalog_commands::CommandOutcome;
        // Transfer the authorization reservation into the blocking native worker.
        // Cancellation cannot release revocation custody while dispatch is queued.
        let (_, revision_intent) =
            motion_catalog_backend::command_attempts::latest_in(&mut tx, &command)
                .await
                .map_err(Problem::internal)?;
        let (outcome, witness, mut tx) = runtime
            .execute_work_observed(revision_intent.clone(), tx)
            .await
            .map_err(Problem::internal)?;
        if let Some(witness) = witness {
            motion_catalog_backend::command_attempts::record_next_in(
                &mut tx,
                &command,
                &revision_intent,
                witness,
            )
            .await
            .map_err(Problem::internal)?;
            tx.commit().await?;
            continue;
        }
        match &outcome {
            CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. } => {}
            outcome => {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "native_command_conflict",
                    format!("Native command requires reconciliation: {}", outcome.code()),
                ));
            }
        }
        break (outcome, tx, principal, revision_intent);
    };
    let record = Scope {
        principal: &principal.id,
        operation: "createCatalogItem",
        target: "-",
        key,
    };
    motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
        &mut tx,
        &id,
        &revision_intent,
        &outcome,
    )
    .await
    .map_err(Problem::internal)?;
    let body = CatalogItemBody {
        id,
        revision: "1".into(),
        kind: media_kind(media_type, "video"),
        title,
        library_ids: vec![],
        availability: "unknown",
        external_ids: vec![],
        artwork_id: None,
        default_timeline_id: None,
        match_state: "unmatched".into(),
        parent_ids: vec![],
    };
    let json = serde_json::to_value(&body).map_err(Problem::internal)?;
    sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('catalog',?)")
        .bind(&body.id)
        .execute(&mut *tx)
        .await?;
    record
        .save(&mut tx, digest, StatusCode::CREATED, Some(&json), None)
        .await?;
    tx.commit().await?;
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    Ok(tagged(StatusCode::CREATED, &body, 1))
}

async fn replace_native_work(
    app: &App,
    caller: &Caller,
    id: &str,
    expected: u64,
    title: String,
    media_type: &str,
    digest: &str,
) -> Result<Response, Problem> {
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    playscale_core::catalog::validate_structure(media_type, None, None, false, &[], 0)
        .map_err(|detail| Problem::invalid("invalid_structure", detail))?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _guard = app.jobs.lock().await;
    let selection = runtime
        .prepare_all_scope()
        .await
        .map_err(native_read_error)?;
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    if !matches!(access::catalog_scope(&principal), CatalogScope::All) {
        return Err(outside_scope());
    }
    let operation = format!(
        "server-replace-work:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(principal.id.as_str(), id, expected, digest))
                .map_err(Problem::internal)?
        )
    );
    // Resolve an immutable intent before looking at a potentially newer native
    // head. Receipt absence never authorizes replacing the saved payload/CAS.
    let saved: Option<String> =
        sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
            .bind(&operation)
            .fetch_optional(&mut *tx)
            .await?;
    let recorded = saved
        .as_deref()
        .map(motion_catalog_backend::command_attempts::decode)
        .transpose()
        .map_err(Problem::internal)?;
    if let Some(original) = &recorded {
        let payload = original.payload();
        let mutation = if payload["operation"] == "metadata_initialize_and_patch" {
            payload
        } else if payload["operation"] == "metadata" {
            &payload["mutation"]["change"]
        } else {
            return Err(native_unavailable());
        };
        let evidence: Value = serde_json::from_str(
            mutation["evidence_json"]
                .as_str()
                .ok_or_else(native_unavailable)?,
        )
        .map_err(Problem::internal)?;
        if original.key().operation_id() != operation
            || mutation["subject"] != id
            || evidence["request_digest"] != digest
        {
            return Err(native_unavailable());
        }
        let (_, sent) = motion_catalog_backend::command_attempts::latest_in(&mut tx, original)
            .await
            .map_err(Problem::internal)?;
        let (outcome, returned) = runtime
            .with_host(move |host| Ok((host.reconcile_command_receipt(&sent)?, tx)))
            .await
            .map_err(native_read_error)?;
        tx = returned;
        if let Some(outcome) = outcome {
            if !matches!(
                outcome,
                catabolic_store::catalog_commands::CommandOutcome::Replay { .. }
            ) {
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    "native_command_conflict",
                    format!("Native command requires reconciliation: {}", outcome.code()),
                ));
            }
            tx.commit().await?;
            runtime
                .rebuild_projection()
                .await
                .map_err(native_read_error)?;
            return get_native_item(app, caller, id).await;
        }
    }
    let rows = runtime
        .read_items(&mut tx, vec![selection.clone()], Some(id.into()))
        .await
        .map_err(native_read_error)?;
    let row = rows.first().ok_or_else(Problem::not_found)?;
    let current = plain_native_work(row)?;
    let revision: u64 = current.revision.parse().map_err(Problem::internal)?;
    let mut metadata: Value = serde_json::from_str(row[2].as_str().ok_or_else(native_unavailable)?)
        .map_err(Problem::internal)?;
    let changed = current.title != title || metadata["motion_catalog"]["media_type"] != media_type;
    let checked = runtime
        .guard_work_and_commit(tx, id.into(), expected, |_, _, _| Ok(()))
        .await
        .map_err(native_read_error)?;
    let aggregate = match checked {
        crate::native_catalog::Guarded::Mismatch(_) => {
            return Err(Problem::new(
                StatusCode::PRECONDITION_FAILED,
                "precondition_failed",
                "The resource revision has changed",
            ));
        }
        crate::native_catalog::Guarded::Applied { revision, .. } => revision.revision,
    };
    if !changed {
        let mut current = current;
        current.revision = aggregate.to_string();
        return Ok(tagged(StatusCode::OK, &current, aggregate));
    }
    let next = revision.checked_add(1).ok_or_else(native_unavailable)?;
    metadata["motion_catalog"]["catalog_revision"] = json!(next);
    if metadata["motion_catalog"]["media_type"] != media_type {
        let previous = metadata["motion_catalog"]["structure_revision"]
            .as_u64()
            .ok_or_else(native_unavailable)?;
        metadata["motion_catalog"]["structure_revision"] =
            json!(previous.checked_add(1).ok_or_else(native_unavailable)?);
    }
    metadata["motion_catalog"]["media_type"] = json!(media_type);
    let command = if let Some(recorded) = recorded {
        recorded
    } else {
        runtime
            .prepare_work_update(
                operation,
                id.into(),
                title,
                metadata["motion_catalog"].clone(),
                digest.into(),
            )
            .await
            .map_err(native_read_error)?
    };
    loop {
        let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
            app.db.begin_with("BEGIN IMMEDIATE").await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::CatalogWrite))
            .await?;
        if !matches!(access::catalog_scope(&principal), CatalogScope::All) {
            return Err(outside_scope());
        }
        let rows = runtime
            .read_items(&mut tx, vec![selection.clone()], Some(id.into()))
            .await
            .map_err(native_read_error)?;
        if rows.is_empty() {
            return Err(Problem::not_found());
        }
        let original = command.clone();
        let subject = id.to_owned();
        let handle = tokio::runtime::Handle::current();
        let dispatch_caller = caller.clone();
        let guarded = runtime
            .guard_work_and_commit(tx, id.into(), expected, move |host, tx, _| {
                handle
                    .block_on(
                        dispatch_caller.reauthorize(&mut **tx, Some(Permission::CatalogWrite)),
                    )
                    .map_err(|e| anyhow::anyhow!(e.detail))?;
                let (_, attempt) = handle.block_on(
                    motion_catalog_backend::command_attempts::latest_in(tx, &original),
                )?;
                let (outcome, witness) =
                    crate::native_catalog::Runtime::execute_metadata_with_host(
                        host,
                        &attempt,
                        subject.clone(),
                    )?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        tx, &original, &attempt, witness,
                    ))?;
                    return Ok(true);
                }
                anyhow::ensure!(
                    matches!(
                        outcome,
                        catabolic_store::catalog_commands::CommandOutcome::Committed { .. }
                            | catabolic_store::catalog_commands::CommandOutcome::Replay { .. }
                    ),
                    "native metadata command requires reconciliation: {}",
                    outcome.code()
                );
                handle.block_on(
                    sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('catalog',?)")
                        .bind(&subject)
                        .execute(&mut **tx),
                )?;
                Ok(false)
            })
            .await
            .map_err(native_read_error)?;
        match guarded {
            crate::native_catalog::Guarded::Mismatch(_) => {
                return Err(Problem::new(
                    StatusCode::PRECONDITION_FAILED,
                    "precondition_failed",
                    "The resource revision has changed",
                ));
            }
            crate::native_catalog::Guarded::Applied { value: true, .. } => continue,
            crate::native_catalog::Guarded::Applied { value: false, .. } => break,
        }
    }
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    get_native_item(app, caller, id).await
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
    let request_digest = body.digest;
    let input = body.value;
    let title = text("title", &input.title, 500)?;
    valid_ids("library_ids", &input.library_ids, 100)?;
    let (media_type, family) = storage_kind(input.kind).ok_or_else(unsupported_kind)?;
    if app.native_catalog.is_some() {
        if !input.library_ids.is_empty() {
            return Err(derived_membership());
        }
        if family.is_some_and(|kind| kind != "video") {
            return Err(unsupported_kind());
        }
        return replace_native_work(
            &app,
            &caller,
            &id,
            expected,
            title,
            media_type,
            &request_digest,
        )
        .await;
    }
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
    if app.native_catalog.is_some() {
        return native_details::editions(&app, &caller, &id, q).await;
    }
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
    if app.native_catalog.is_some() {
        return native_editions::create(State(app), caller, Path(id), headers, body).await;
    }
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
            authorize_merge_scope(conn, scope, &request.sources, &request.target).await?;
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
            authorize_merge_scope(&mut tx, &scope, &plan.retired, &plan.target).await?;
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
pub(crate) struct TimelineRow {
    pub(crate) id: String,
    revision: i64,
    pub(crate) item_id: String,
    pub(crate) edition_id: String,
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

pub(crate) async fn timeline_body(
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
    if app.native_catalog.is_some() {
        return native_details::timelines(&app, &caller, &id, q).await;
    }
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
    if app.native_catalog.is_some() {
        return native_details::timeline(&app, &caller, &id).await;
    }
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
    if app.native_catalog.is_some() {
        return native_details::versions(&app, &caller, &id, q).await;
    }
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
        values.push((
            version.clone(),
            version_body(&mut tx, &scope, &version, &id).await?,
        ));
    }
    let value = page(&mut tx, &caller.principal, values, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

pub(crate) async fn require_edition(
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

async fn version_body(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    version: &str,
    id: &str,
) -> Result<Value, Problem> {
    let (revision, label, origin, equivalence): (i64, String, String, String) =
        sqlx::query_as("SELECT revision,label,origin,equivalence FROM media_versions WHERE id=?")
            .bind(version)
            .fetch_one(&mut *conn)
            .await?;
    type BindingRow = (String, String, i64, Option<i64>, Option<i64>, bool, String);
    let bindings: Vec<BindingRow>=sqlx::query_as("SELECT b.file_id,b.file_revision,b.part,b.start_ms,b.end_ms,f.available,f.revision FROM version_files b JOIN media_files f ON f.id=b.file_id WHERE b.version_id=? ORDER BY b.part LIMIT 101")
            .bind(version).fetch_all(&mut *conn).await?;
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
    let (clause, all, scope_ids) = scope_clause(scope, "f.library_id");
    for binding in &core_bindings {
        let copy:Option<(String,String)>=sqlx::query_as(&format!("SELECT f.id,f.revision FROM media_files f WHERE f.available=1 AND f.revision=? AND {clause} ORDER BY f.id LIMIT 1"))
                .bind(&binding.revision).bind(all).bind(&scope_ids).fetch_optional(&mut *conn).await?;
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

    Ok(
        serde_json::json!({"id":version,"revision":revision.to_string(),"timeline_id":id,"label":label,"origin":origin,"files":files,"timeline_equivalence":equivalence,"availability":if available {"available"} else {"unavailable"}}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderGroupInput {
    edition_id: String,
    name: String,
    kind: crate::curation::OrderKind,
}

pub async fn list_order_groups(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<Value>>, Problem> {
    caller.require(Permission::CatalogRead)?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    let mut tx = app.db.begin().await?;
    let (item, _) = load_item(&mut tx, &scope, &id, true).await?;
    let id = item.id;
    let rows: Vec<(String,String,String,String,i64)> = sqlx::query_as(&format!(
        "SELECT g.id,g.edition_id,g.name,g.kind,g.revision FROM order_groups g JOIN editions e ON e.id=g.edition_id WHERE e.item_id=? AND g.id>? AND (? OR NOT EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0) OR EXISTS (SELECT 1 FROM media_files f WHERE f.edition_id=e.id AND f.generated=0 AND {clause})) ORDER BY g.id LIMIT ?"
    )).bind(&id).bind(q.after()?).bind(all).bind(all).bind(ids).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let rows = rows.into_iter().map(|(group,edition,name,kind,revision)| {
        let value = serde_json::json!({"id":group,"item_id":id,"edition_id":edition,"name":name,"kind":kind,"revision":revision.to_string()});
        (group,value)
    }).collect();
    let result = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(result))
}

/// Revision fencing, authorization and replay decisions use the production core;
/// the adapter commits the group, aggregate revision and receipt atomically.
pub async fn create_order_group(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<OrderGroupInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    let name = text("name", &input.name, 200)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let row = curatable_item(&mut tx, &scope, &id, true).await?;
    let id = row.id;
    let edition: Option<String> =
        sqlx::query_scalar("SELECT id FROM editions WHERE id=? AND item_id=?")
            .bind(&input.edition_id)
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    if edition.is_none() {
        return Err(Problem::not_found());
    }
    let record = Scope {
        principal: &principal.id,
        operation: "createOrderGroup",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        return Ok(replayed(stored.unwrap()));
    }
    access::revise(row.revision as u64, expected, true)?;
    let group =
        crate::curation::create_order_group_in(&mut tx, &input.edition_id, &name, input.kind)
            .await
            .map_err(curation_problem)?;
    let value = serde_json::json!({"id":group,"item_id":id,"edition_id":input.edition_id,"name":name,"kind":input.kind,"revision":"1"});
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &value, 1))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineInput {
    edition_id: String,
    duration_ms: Option<u64>,
    order_group_id: Option<String>,
    order_position: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimelineOrderInput {
    order_group_id: String,
    order_position: u32,
    expected_group_revision: String,
}

/// Place an existing playable timeline without changing its media bindings or
/// history identity. The writer transaction fences both timeline and group and
/// supplies current ancestry/occupancy to the production core placement policy.
pub async fn place_timeline(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<TimelineOrderInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let input = body.value;
    let expected_group = revision_text("expected_group_revision", &input.expected_group_revision)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let timeline: TimelineRow = sqlx::query_as(
        "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Problem::not_found)?;
    curatable_item(&mut tx, &scope, &timeline.item_id, false).await?;
    // Leaving a group also mutates its revision; scope every affected owner.
    for group in std::iter::once(&input.order_group_id).chain(timeline.order_group_id.iter()) {
        let owner: String = sqlx::query_scalar(
            "SELECT e.item_id FROM order_groups g JOIN editions e ON e.id=g.edition_id WHERE g.id=?",
        ).bind(group).fetch_optional(&mut *tx).await?.ok_or_else(Problem::not_found)?;
        curatable_item(&mut tx, &scope, &owner, false).await?;
    }
    access::revise(timeline.revision as u64, expected, true)?;
    crate::curation::place_timeline_in(
        &mut tx,
        &id,
        &input.order_group_id,
        input.order_position,
        expected_group,
    )
    .await
    .map_err(curation_problem)?;
    let row: TimelineRow = sqlx::query_as(
        "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    let revision = row.revision as u64;
    let value = timeline_body(&mut tx, &scope, row).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &value, revision))
}

pub async fn create_timeline(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<TimelineInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    if input.duration_ms.is_some_and(|n| n > 9_007_199_254_740_991)
        || input.order_group_id.is_some() != input.order_position.is_some()
    {
        return Err(Problem::invalid(
            "invalid_timeline",
            "Invalid duration or incomplete ordering",
        ));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let row = curatable_item(&mut tx, &scope, &id, false).await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createTimeline",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        return Ok(replayed(stored.unwrap()));
    }
    access::revise(row.revision as u64, expected, true)?;
    let timeline = crate::curation::create_timeline_in(
        &mut tx,
        &id,
        &input.edition_id,
        expected,
        input.duration_ms,
    )
    .await
    .map_err(curation_problem)?;
    if let (Some(group), Some(position)) = (input.order_group_id, input.order_position) {
        let (revision,owner):(i64,String)=sqlx::query_as("SELECT g.revision,e.item_id FROM order_groups g JOIN editions e ON e.id=g.edition_id WHERE g.id=?").bind(&group).fetch_optional(&mut *tx).await?.ok_or_else(Problem::not_found)?;
        curatable_item(&mut tx, &scope, &owner, false).await?;
        crate::curation::place_timeline_in(&mut tx, &timeline, &group, position, revision as u64)
            .await
            .map_err(curation_problem)?;
    }
    let row: TimelineRow = sqlx::query_as(
        "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(&timeline)
    .fetch_one(&mut *tx)
    .await?;
    let revision = row.revision as u64;
    let value = timeline_body(&mut tx, &scope, row).await?;
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &value, revision))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundFile {
    file: VersionFileRef,
    part: u32,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionFileRef {
    file_id: String,
    file_revision: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionInput {
    timeline_id: String,
    label: String,
    files: Vec<BoundFile>,
    timeline_equivalence: playscale_core::identity::Equivalence,
}
pub async fn create_version(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<VersionInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    if input.timeline_id != id
        || input.files.is_empty()
        || input.files.len() > 100
        || input.label.len() > 200
    {
        return Err(Problem::invalid(
            "invalid_version",
            "Invalid target, label or bindings",
        ));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let timeline: TimelineRow = sqlx::query_as(
        "SELECT t.*,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(Problem::not_found)?;
    let item = curatable_item(&mut tx, &scope, &timeline.item_id, false).await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createVersion",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        return Ok(replayed(stored.unwrap()));
    }
    access::revise(timeline.revision as u64, expected, true)?;
    let mut bindings = Vec::new();
    for binding in input.files {
        let owner: String = sqlx::query_scalar(
            "SELECT e.item_id FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE f.id=?",
        )
        .bind(&binding.file.file_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
        curatable_item(&mut tx, &scope, &owner, false).await?;
        if binding
            .start_ms
            .into_iter()
            .chain(binding.end_ms)
            .any(|n| n > 9_007_199_254_740_991)
        {
            return Err(Problem::invalid(
                "invalid_interval",
                "Interval exceeds safe integer bound",
            ));
        }
        bindings.push(playscale_core::identity::Binding {
            file_id: binding.file.file_id,
            revision: binding.file.file_revision,
            part: binding.part,
            start_ms: binding.start_ms,
            end_ms: binding.end_ms,
        });
    }
    let request = playscale_core::identity::VersionRequest {
        bindings,
        equivalence: input.timeline_equivalence,
    };
    let version = crate::curation::create_version_explicit_in(
        &mut tx,
        &id,
        &input.label,
        &request,
        item.revision as u64,
    )
    .await
    .map_err(curation_problem)?;
    let value = version_body(&mut tx, &scope, &version, &id).await?;
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &value, 1))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationshipInput {
    source_item_id: String,
    target_item_id: String,
    kind: playscale_core::identity::RelationshipKind,
    position: Option<u32>,
}
async fn relationship(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    id: &str,
) -> Result<Value, Problem> {
    let (revision,source,target,kind,position):(i64,String,String,String,Option<i64>)=sqlx::query_as("SELECT revision,source_item_id,target_item_id,kind,position FROM item_relationships WHERE id=?").bind(id).fetch_optional(&mut *conn).await?.ok_or_else(Problem::not_found)?;
    load_item(conn, scope, &source, false).await?;
    load_item(conn, scope, &target, false).await?;
    Ok(
        serde_json::json!({"id":id,"revision":revision.to_string(),"source_item_id":source,"target_item_id":target,"kind":kind,"position":position}),
    )
}
pub async fn create_relationship(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<RelationshipInput>,
) -> Result<Response, Problem> {
    if app.native_catalog.is_some() {
        return native_relationship_writes::create(State(app), caller, Path(id), headers, body)
            .await;
    }
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let key = idempotency_key(&headers)?;
    let input = body.value;
    if input.source_item_id != id {
        return Err(Problem::invalid(
            "source_mismatch",
            "Path must name source_item_id",
        ));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let row = curatable_item(&mut tx, &scope, &id, false).await?;
    curatable_item(&mut tx, &scope, &input.target_item_id, false).await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createRelationship",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        return Ok(replayed(stored.unwrap()));
    }
    access::revise(row.revision as u64, expected, true)?;
    let relation = crate::curation::create_relationship_in(
        &mut tx,
        &id,
        &input.target_item_id,
        input.kind,
        input.position,
    )
    .await
    .map_err(curation_problem)?;
    let value = relationship(&mut tx, &scope, &relation.id).await?;
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &value, 1))
}
pub async fn get_relationship(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    if app.native_catalog.is_some() {
        return native_relationships::get(State(app), caller, Path(id)).await;
    }
    caller.require(Permission::CatalogRead)?;
    let mut tx = app.db.begin().await?;
    let value = relationship(&mut tx, &access::catalog_scope(&caller.principal), &id).await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        &value,
        value["revision"].as_str().unwrap().parse().unwrap(),
    ))
}
pub async fn delete_relationship(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    if app.native_catalog.is_some() {
        return native_relationship_writes::delete(State(app), caller, Path(id), headers).await;
    }
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = access::catalog_scope(&principal);
    let value = relationship(&mut tx, &scope, &id).await?;
    for field in ["source_item_id", "target_item_id"] {
        curatable_item(&mut tx, &scope, value[field].as_str().unwrap(), false).await?;
    }
    access::revise(
        value["revision"].as_str().unwrap().parse().unwrap(),
        expected,
        true,
    )?;
    crate::curation::delete_relationship_in(&mut tx, &id, expected)
        .await
        .map_err(curation_problem)?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
pub async fn list_relationships(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    if app.native_catalog.is_some() {
        return native_relationships::list(State(app), caller, Path(id), Query(q)).await;
    }
    caller.require(Permission::CatalogRead)?;
    let mut tx = app.db.begin().await?;
    let scope = access::catalog_scope(&caller.principal);
    load_item(&mut tx, &scope, &id, false).await?;
    let limit = q.limit()?;
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM item_relationships WHERE (source_item_id=? OR target_item_id=?) AND id>? ORDER BY id").bind(&id).bind(&id).bind(q.after()?).fetch_all(&mut *tx).await?;
    let mut rows = Vec::new();
    for id in ids {
        match relationship(&mut tx, &scope, &id).await {
            Ok(value) => rows.push((id, value)),
            Err(e) if e.status == StatusCode::NOT_FOUND => continue,
            Err(e) => return Err(e),
        }
        if rows.len() > limit as usize {
            break;
        }
    }
    let value = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

pub async fn list_item_files(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    files_page(app, caller, Some(id), q).await
}
pub async fn list_admin_files(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    files_page(app, caller, None, q).await
}
async fn files_page(
    app: App,
    caller: Caller,
    mut item: Option<String>,
    q: PageQuery,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let mut tx = app.db.begin().await?;
    let scope = access::catalog_scope(&caller.principal);
    if let Some(id) = &item {
        item = Some(load_item(&mut tx, &scope, id, true).await?.0.id);
    }
    let admin = item.is_none();
    let limit = q.limit()?;
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    type FileRow = (
        String,
        String,
        String,
        i64,
        Option<f64>,
        bool,
        String,
        String,
        i64,
    );
    let rows:Vec<FileRow>=sqlx::query_as(&format!("SELECT f.id,f.revision,f.library_id,f.bytes,f.duration_seconds,f.available,f.tracks_json,f.relative_path,l.binding_revision FROM media_files f JOIN libraries l ON l.id=f.library_id JOIN editions e ON e.id=f.edition_id WHERE (? IS NULL OR e.item_id=?) AND f.id>? AND {clause} ORDER BY f.id LIMIT ?"))
        .bind(&item).bind(&item).bind(q.after()?).bind(all).bind(ids).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let mut values = Vec::new();
    for (
        id,
        revision,
        source,
        bytes,
        duration,
        available,
        tracks,
        relative_path,
        binding_revision,
    ) in rows
    {
        let tracks: Vec<crate::db::Track> =
            serde_json::from_str(&tracks).map_err(Problem::internal)?;
        if tracks.len() > 256 {
            return Err(Problem::invalid(
                "too_many_tracks",
                "File exceeds public track bound",
            ));
        }
        let mut indices = BTreeMap::new();
        let tracks:Vec<_>=tracks.into_iter().map(|t|{
            let (kind,prefix)=match t.kind.as_str(){"audio"=>("audio","a"),"subtitle"=>("subtitle","s"),"video"=>("video","v"),"attachment"=>("attachment","x"),_=>("other","o")};
            let index=indices.entry(prefix).or_insert(0);let id=format!("{prefix}{index}");*index+=1;
            serde_json::json!({"id":id,"component_id":Value::Null,"kind":kind,"codec":t.codec,"language":t.language,"title":t.title,"forced":t.forced.unwrap_or(false),"default":t.default_track.unwrap_or(false),"sdh":t.hearing_impaired.unwrap_or(false),"channels":Value::Null,"channel_layout":Value::Null,"width":t.width.filter(|n|*n>0),"height":t.height.filter(|n|*n>0),"bit_depth":Value::Null,"transfer":t.color_transfer})
        }).collect();
        let digest = if revision.len() == 64 && revision.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(format!("sha256:{revision}"))
        } else {
            None
        };
        let value = serde_json::json!({"id":id,"revision":revision,"source_id":source,"size_bytes":bytes.to_string(),"verification":if available {"observed"}else{"stale"},"digest":digest,"duration_ms":duration.filter(|d|d.is_finite()&&*d>=0.0).map(|d|(d*1000.0).round() as u64),"availability":if available {"available"}else{"unavailable"},"tracks":tracks});
        let value = if admin {
            serde_json::json!({"file":value,"relative_path":relative_path,"binding_revision":binding_revision.to_string()})
        } else {
            value
        };
        values.push((id, value));
    }
    let value = page(&mut tx, &caller.principal, values, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}

#[cfg(test)]
mod native_projection_tests {
    use super::*;
    #[test]
    fn imported_or_richer_records_never_become_empty_plain_dtos() {
        let metadata = serde_json::json!({"title":"Native", "motion_catalog":{
            "contract_version":1,"item_id":"work","kind":"video","catalog_revision":1,"media_type":"movie",
            "parent_id":null,"match_state":"unmatched"
        }});
        let row = serde_json::json!(["work", "video", metadata.to_string()]);
        assert!(plain_native_work(&row).is_err());
        let mut metadata = metadata;
        metadata["motion_catalog"]["creation_request_digest"] = Value::String("digest".into());
        let row = serde_json::json!(["work", "video", metadata.to_string()]);
        let body = plain_native_work(&row).unwrap();
        assert_eq!(body.id, "work");
        assert_eq!(body.title, "Native");
        assert_eq!(body.availability, "unknown");
        assert!(body.library_ids.is_empty());
        metadata["motion_catalog"]["parent_id"] = Value::String("parent".into());
        assert!(
            plain_native_work(&serde_json::json!(["work", "video", metadata.to_string()])).is_err()
        );
    }
}
