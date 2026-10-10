//! Organization adapter (A09 service, A08 adapter): saved filters, collections,
//! playlists and play queues under `/api/v2`.
//!
//! Every aggregate belongs to one profile: a caller must be allowed that
//! profile, otherwise the aggregate is indistinguishable from a missing one.
//! Membership confers no media permission: members (items) and entries
//! (timelines) outside the caller's catalog scope are never accepted from, nor
//! disclosed to, that caller, and a replacement by that caller keeps them
//! (`organization::keep_hidden_*`). Validation, revisions, removal and queue
//! rules are decided in `playscale_core::organization`; persistence is
//! `crate::organization`, run inside this adapter's write transaction so the
//! authorization recheck, the effect and the idempotency record commit
//! together.
//!
//! Timeline IDs: on this branch the organization service stores a timeline ID
//! as its legacy edition ID (migration 0014, `curation.rs`); visibility of an
//! entry is the visibility of the work holding that stored timeline.
use super::{
    Body, Page, PageQuery, Problem, Scope, Stored, auth::Caller, etag, idempotency_key, if_match,
    page, scope_clause,
};
use crate::{
    App,
    db::begin_write,
    now,
    organization::{self as service, OrgError, Owned, QueueChange},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::{
    access::{self as access, CatalogScope, Idempotent, Permission, Principal},
    organization::{self as org, CollectionKind, Entry, OrganizationError, Repeat, Term},
};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::SqliteConnection;
use std::collections::BTreeSet;

pub fn routes() -> Router<App> {
    Router::new()
        .route(
            "/collections",
            get(list_collections).post(create_collection),
        )
        .route(
            "/collections/{collection_id}",
            get(get_collection)
                .put(replace_collection)
                .delete(delete_collection),
        )
        .route("/catalog/filters", get(list_filters).post(create_filter))
        .route(
            "/catalog/filters/{filter_id}",
            get(get_filter).put(replace_filter).delete(delete_filter),
        )
        .route("/playlists", get(list_playlists).post(create_playlist))
        .route(
            "/playlists/{playlist_id}",
            get(get_playlist)
                .put(replace_playlist)
                .delete(delete_playlist),
        )
        .route("/playback/queues", get(list_queues).post(create_queue))
        .route(
            "/playback/queues/{queue_id}",
            get(get_queue).put(replace_queue).delete(delete_queue),
        )
}

// ---------------------------------------------------------------------------
// Shared adapter helpers

/// A required JSON field whose value may be `null`.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn ids<'a>(field: &str, values: impl IntoIterator<Item = &'a String>) -> Result<(), Problem> {
    if values.into_iter().all(|v| is_id(v)) {
        Ok(())
    } else {
        Err(Problem::invalid(
            "invalid_field",
            format!("{field} must contain valid IDs"),
        ))
    }
}

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

fn org_problem(e: OrgError) -> Problem {
    use OrganizationError::*;
    match e {
        OrgError::NotFound => Problem::not_found(),
        OrgError::InvalidReference(_) => Problem::invalid(
            "invalid_reference",
            "A referenced item, timeline or filter does not exist or is not accessible",
        ),
        OrgError::Storage(e) => match e.downcast::<sqlx::Error>() {
            Ok(e) => e.into(),
            Err(e) => Problem::internal(e),
        },
        OrgError::Rejected(e) => match e {
            StaleRevision => access::AccessError::StaleRevision.into(),
            RevisionExhausted => Problem::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "The resource can no longer be revised",
            ),
            OwnerChanged => Problem::new(
                StatusCode::CONFLICT,
                "profile_immutable",
                "profile_id cannot change; create a new resource for another profile",
            ),
            FilterInUse => Problem::new(
                StatusCode::CONFLICT,
                "filter_in_use",
                "The filter defines a smart collection; change or delete the collection first",
            ),
            InvalidName => Problem::invalid(
                "invalid_name",
                "name must be 1-200 bytes and not only whitespace",
            ),
            TooManyTerms | InvalidTerm(_) => Problem::invalid(
                "invalid_filter",
                "Filter terms must be at most 32 compatible field/operator/value terms",
            ),
            ManualCollectionHasFilter | SmartCollectionNeedsFilter | SmartCollectionHasMembers => {
                Problem::invalid(
                    "invalid_collection",
                    "manual collections take item_ids and a null filter_id; smart collections take a filter_id and no item_ids",
                )
            }
            TooManyMembers => Problem::invalid("too_many_members", "At most 1000 members"),
            DuplicateMember(_) => {
                Problem::invalid("duplicate_member", "Members and entry IDs must be unique")
            }
            UnknownEntry(_) => Problem::invalid("unknown_entry", "The entry is not in the queue"),
        },
    }
}

impl From<OrgError> for Problem {
    fn from(e: OrgError) -> Self {
        org_problem(e)
    }
}

/// An aggregate owned by a profile the caller may not use is missing to it.
fn owned(principal: &Principal, profile: &str) -> Result<(), Problem> {
    if principal.may_use_profile(profile) {
        Ok(())
    } else {
        Err(Problem::not_found())
    }
}

/// The caller must be allowed the target profile, and it must exist.
async fn target_profile(
    conn: &mut SqliteConnection,
    principal: &Principal,
    profile: &str,
) -> Result<(), Problem> {
    owned(principal, profile)?;
    let known: Option<String> = sqlx::query_scalar("SELECT id FROM profiles WHERE id=?")
        .bind(profile)
        .fetch_optional(conn)
        .await?;
    known.map(|_| ()).ok_or_else(Problem::not_found)
}

/// The works among `ids` the scope can read: those with a file in a readable
/// library (everything for an unrestricted scope).
async fn visible_items(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    ids: &[String],
) -> Result<BTreeSet<String>, Problem> {
    if ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let (clause, all, libraries) = scope_clause(scope, "f.library_id");
    Ok(sqlx::query_scalar(&format!(
        "SELECT i.id FROM items i WHERE i.id IN (SELECT value FROM json_each(?)) AND (? OR EXISTS(SELECT 1 FROM editions e JOIN media_files f ON f.edition_id=e.id WHERE e.item_id=i.id AND {clause}))"
    ))
    .bind(serde_json::to_string(ids).map_err(Problem::internal)?)
    .bind(all)
    .bind(all)
    .bind(libraries)
    .fetch_all(conn)
    .await?
    .into_iter()
    .collect())
}

/// The stored timelines among `ids` whose work the scope can read.
async fn visible_timelines(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    ids: &BTreeSet<&String>,
) -> Result<BTreeSet<String>, Problem> {
    if ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let (clause, all, libraries) = scope_clause(scope, "f.library_id");
    Ok(sqlx::query_scalar(&format!(
        "SELECT t.id FROM editions t WHERE t.id IN (SELECT value FROM json_each(?)) AND (? OR EXISTS(SELECT 1 FROM editions e JOIN media_files f ON f.edition_id=e.id WHERE e.item_id=t.item_id AND {clause}))"
    ))
    .bind(serde_json::to_string(ids).map_err(Problem::internal)?)
    .bind(all)
    .bind(all)
    .bind(libraries)
    .fetch_all(conn)
    .await?
    .into_iter()
    .collect())
}

/// Requested members must all be readable; unreadable ones are reported
/// exactly like missing ones.
async fn require_visible_items(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    requested: &[String],
) -> Result<(), Problem> {
    if *scope == CatalogScope::All {
        return Ok(());
    }
    let visible = visible_items(conn, scope, requested).await?;
    if requested.iter().all(|id| visible.contains(id)) {
        Ok(())
    } else {
        Err(OrgError::InvalidReference(String::new()).into())
    }
}

async fn require_visible_entries(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    requested: &[Entry],
) -> Result<(), Problem> {
    if *scope == CatalogScope::All {
        return Ok(());
    }
    let wanted = requested.iter().map(|e| &e.timeline_id).collect();
    let visible = visible_timelines(conn, scope, &wanted).await?;
    if requested.iter().all(|e| visible.contains(&e.timeline_id)) {
        Ok(())
    } else {
        Err(OrgError::InvalidReference(String::new()).into())
    }
}

/// Entry IDs of `entries` the scope cannot read.
async fn hidden_entries(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    entries: &[Entry],
) -> Result<BTreeSet<String>, Problem> {
    if *scope == CatalogScope::All {
        return Ok(BTreeSet::new());
    }
    let wanted = entries.iter().map(|e| &e.timeline_id).collect();
    let visible = visible_timelines(conn, scope, &wanted).await?;
    Ok(entries
        .iter()
        .filter(|e| !visible.contains(&e.timeline_id))
        .map(|e| e.entry_id.clone())
        .collect())
}

async fn hidden_items(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    items: &[String],
) -> Result<BTreeSet<String>, Problem> {
    if *scope == CatalogScope::All {
        return Ok(BTreeSet::new());
    }
    let visible = visible_items(conn, scope, items).await?;
    Ok(items
        .iter()
        .filter(|id| !visible.contains(*id))
        .cloned()
        .collect())
}

/// Profiles a list may show, applied in SQL before the cursor and limit.
fn profile_filter(principal: &Principal) -> Result<(bool, String), Problem> {
    Ok((
        principal.is_admin(),
        serde_json::to_string(&principal.grant.profile_ids).map_err(Problem::internal)?,
    ))
}

async fn list_ids(
    conn: &mut SqliteConnection,
    table: &str,
    principal: &Principal,
    q: &PageQuery,
    limit: u32,
) -> Result<Vec<String>, Problem> {
    let (admin, granted) = profile_filter(principal)?;
    Ok(sqlx::query_scalar(&format!(
        "SELECT id FROM {table} WHERE id>? AND (? OR profile_id IN (SELECT value FROM json_each(?))) ORDER BY id LIMIT ?"
    ))
    .bind(q.after()?)
    .bind(admin)
    .bind(granted)
    .bind(i64::from(limit) + 1)
    .fetch_all(conn)
    .await?)
}

/// Opens the write transaction for a mutation and re-derives the principal in
/// it. The service's writer lock is taken first, in the service's order.
async fn begin<'a>(
    app: &'a App,
    caller: &Caller,
    permission: Permission,
) -> Result<
    (
        tokio::sync::MutexGuard<'a, ()>,
        sqlx::Transaction<'a, sqlx::Sqlite>,
        Principal,
    ),
    Problem,
> {
    let guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller.reauthorize(&mut tx, Some(permission)).await?;
    Ok((guard, tx, principal))
}

/// Loads an idempotency record; `Some` is the response to replay. The stored
/// acknowledgement is redacted for the caller's current catalog scope, so a
/// replay after a policy downgrade discloses no member it can no longer see.
async fn replay(
    tx: &mut SqliteConnection,
    principal: &Principal,
    scope: &Scope<'_>,
    digest: &str,
) -> Result<Option<Response>, Problem> {
    let stored = scope.load(tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), digest, now())? != Idempotent::Replay
    {
        return Ok(None);
    }
    let Some(mut stored) = stored else {
        return Ok(None);
    };
    if let Some(body) = stored.body.as_mut() {
        redact(tx, &access::catalog_scope(principal), body).await?;
    }
    Ok(Some(replayed(stored)))
}

/// Removes members and entries `scope` cannot read from a stored body, and a
/// current entry that was removed.
async fn redact(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    body: &mut serde_json::Value,
) -> Result<(), Problem> {
    if *scope == CatalogScope::All {
        return Ok(());
    }
    let strings = |v: Option<&serde_json::Value>, key: &str| -> Vec<String> {
        v.and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|e| {
                if key.is_empty() {
                    e.as_str()
                } else {
                    e[key].as_str()
                }
            })
            .map(str::to_owned)
            .collect()
    };
    let items = strings(body.get("item_ids"), "");
    if !items.is_empty() {
        let visible = visible_items(conn, scope, &items).await?;
        body["item_ids"] = items
            .into_iter()
            .filter(|id| visible.contains(id))
            .collect::<Vec<_>>()
            .into();
    }
    let timelines = strings(body.get("entries"), "timeline_id");
    if !timelines.is_empty() {
        let visible = visible_timelines(conn, scope, &timelines.iter().collect()).await?;
        let entries: Vec<serde_json::Value> = body["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| {
                e["timeline_id"]
                    .as_str()
                    .is_some_and(|t| visible.contains(t))
            })
            .collect();
        if let Some(current) = body.get("current_entry_id").and_then(|c| c.as_str())
            && !entries.iter().any(|e| e["entry_id"] == current)
        {
            body["current_entry_id"] = serde_json::Value::Null;
        }
        body["entries"] = entries.into();
    }
    Ok(())
}

async fn acknowledge<T: Serialize>(
    mut tx: sqlx::Transaction<'_, sqlx::Sqlite>,
    scope: &Scope<'_>,
    digest: &str,
    body: &T,
    revision: u64,
) -> Result<Response, Problem> {
    let json = serde_json::to_value(body).map_err(Problem::internal)?;
    scope
        .save(&mut tx, digest, StatusCode::CREATED, Some(&json), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &json, revision))
}

// ---------------------------------------------------------------------------
// Saved filters

#[derive(Serialize)]
struct FilterBody {
    id: String,
    revision: String,
    name: String,
    profile_id: String,
    all: Vec<Term>,
}

fn filter_body(f: service::SavedFilter) -> (FilterBody, u64) {
    (
        FilterBody {
            id: f.id,
            revision: f.revision.to_string(),
            name: f.name,
            profile_id: f.profile_id,
            all: f.all,
        },
        f.revision,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterInput {
    name: String,
    profile_id: String,
    all: Vec<Term>,
}

impl FilterInput {
    fn check(&self) -> Result<(), Problem> {
        ids("profile_id", [&self.profile_id])
    }
}

async fn list_filters(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<FilterBody>>, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let mut rows = Vec::new();
    for id in list_ids(&mut tx, "saved_filters", &caller.principal, &q, limit).await? {
        if let Some(f) = service::load_filter(&mut tx, &id).await? {
            rows.push((id, filter_body(f).0));
        }
    }
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn get_filter(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let mut conn = app.db.acquire().await?;
    let f = service::load_filter(&mut conn, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&caller.principal, &f.profile_id)?;
    let (body, revision) = filter_body(f);
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn create_filter(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<FilterInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let key = idempotency_key(&headers)?;
    body.value.check()?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    let input = body.value;
    target_profile(&mut tx, &principal, &input.profile_id).await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createSavedFilter",
        target: "-",
        key: &key,
    };
    if let Some(response) = replay(&mut tx, &principal, &scope, &body.digest).await? {
        return Ok(response);
    }
    let saved =
        service::save_filter_in(&mut tx, &input.profile_id, None, &input.name, input.all).await?;
    let (created, revision) = filter_body(saved);
    acknowledge(tx, &scope, &body.digest, &created, revision).await
}

async fn replace_filter(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<FilterInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let expected = if_match(&headers)?;
    body.value.check()?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    let current = service::load_filter(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&principal, &current.profile_id)?;
    org::same_owner(&current.profile_id, &body.value.profile_id).map_err(OrgError::from)?;
    let input = body.value;
    let saved = service::save_filter_in(
        &mut tx,
        &current.profile_id,
        Some((&id, expected)),
        &input.name,
        input.all,
    )
    .await?;
    tx.commit().await?;
    let (body, revision) = filter_body(saved);
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn delete_filter(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let expected = if_match(&headers)?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    let current = service::load_filter(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&principal, &current.profile_id)?;
    service::delete_filter_in(&mut tx, &id, expected).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// Collections

#[derive(Serialize)]
struct CollectionBody {
    id: String,
    revision: String,
    name: String,
    profile_id: String,
    kind: CollectionKind,
    item_ids: Vec<String>,
    filter_id: Option<String>,
}

/// The collection as `scope` may see it: unreadable members are omitted.
async fn collection_body(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    c: service::Collection,
) -> Result<(CollectionBody, u64), Problem> {
    let hidden = hidden_items(conn, scope, &c.item_ids).await?;
    Ok((
        CollectionBody {
            id: c.id,
            revision: c.revision.to_string(),
            name: c.name,
            profile_id: c.profile_id,
            kind: c.kind,
            item_ids: c
                .item_ids
                .into_iter()
                .filter(|id| !hidden.contains(id))
                .collect(),
            filter_id: c.filter_id,
        },
        c.revision,
    ))
}

/// Responses list members in the service's canonical `(title, id)` order.
async fn reload_collection(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<service::Collection, Problem> {
    service::load_collection(conn, id)
        .await?
        .ok_or_else(|| Problem::internal("saved collection is missing"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionInput {
    name: String,
    profile_id: String,
    kind: CollectionKind,
    item_ids: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    filter_id: Option<String>,
}

impl CollectionInput {
    fn check(&self) -> Result<(), Problem> {
        ids("profile_id", [&self.profile_id])?;
        ids("filter_id", &self.filter_id)?;
        if self.item_ids.len() > org::MAX_MEMBERS {
            return Err(OrgError::from(OrganizationError::TooManyMembers).into());
        }
        ids("item_ids", &self.item_ids)
    }
}

async fn list_collections(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<CollectionBody>>, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let mut rows = Vec::new();
    for id in list_ids(&mut tx, "collections", &caller.principal, &q, limit).await? {
        if let Some(c) = service::load_collection(&mut tx, &id).await? {
            rows.push((id, collection_body(&mut tx, &scope, c).await?.0));
        }
    }
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn get_collection(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let c = service::load_collection(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&caller.principal, &c.profile_id)?;
    let (body, revision) = collection_body(&mut tx, &scope, c).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn create_collection(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<CollectionInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let key = idempotency_key(&headers)?;
    body.value.check()?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    let input = body.value;
    target_profile(&mut tx, &principal, &input.profile_id).await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createCollection",
        target: "-",
        key: &key,
    };
    if let Some(response) = replay(&mut tx, &principal, &scope, &body.digest).await? {
        return Ok(response);
    }
    let catalog = access::catalog_scope(&principal);
    require_visible_items(&mut tx, &catalog, &input.item_ids).await?;
    let saved = service::save_collection_in(
        &mut tx,
        &input.profile_id,
        None,
        &input.name,
        input.kind,
        input.item_ids,
        input.filter_id,
    )
    .await?;
    let saved = reload_collection(&mut tx, &saved.id).await?;
    let (created, revision) = collection_body(&mut tx, &catalog, saved).await?;
    acknowledge(tx, &scope, &body.digest, &created, revision).await
}

async fn replace_collection(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<CollectionInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let expected = if_match(&headers)?;
    body.value.check()?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    let current = service::load_collection(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&principal, &current.profile_id)?;
    let input = body.value;
    org::same_owner(&current.profile_id, &input.profile_id).map_err(OrgError::from)?;
    org::advance(current.revision, expected).map_err(OrgError::from)?;
    let catalog = access::catalog_scope(&principal);
    require_visible_items(&mut tx, &catalog, &input.item_ids).await?;
    // Members this caller cannot see stay in a manual collection it rewrites.
    // Changing the kind replaces the whole membership, like deleting the
    // collection, which any user of the profile may do.
    let item_ids = match input.kind {
        CollectionKind::Manual => {
            let hidden = hidden_items(&mut tx, &catalog, &current.item_ids).await?;
            org::keep_hidden_members(input.item_ids, &current.item_ids, &hidden)
        }
        CollectionKind::Smart => input.item_ids,
    };
    let saved = service::save_collection_in(
        &mut tx,
        &current.profile_id,
        Some((&id, expected)),
        &input.name,
        input.kind,
        item_ids,
        input.filter_id,
    )
    .await?;
    let saved = reload_collection(&mut tx, &saved.id).await?;
    let (body, revision) = collection_body(&mut tx, &catalog, saved).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn delete_owned(
    app: &App,
    caller: &Caller,
    permission: Permission,
    kind: Owned,
    id: &str,
    headers: &HeaderMap,
) -> Result<Response, Problem> {
    caller.require(permission)?;
    let expected = if_match(headers)?;
    let (_guard, mut tx, principal) = begin(app, caller, permission).await?;
    let table = kind.table();
    let profile: String = sqlx::query_scalar(&format!("SELECT profile_id FROM {table} WHERE id=?"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&principal, &profile)?;
    service::delete_owned_in(&mut tx, kind, id, expected).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn delete_collection(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    delete_owned(
        &app,
        &caller,
        Permission::CollectionsWrite,
        Owned::Collection,
        &id,
        &headers,
    )
    .await
}

// ---------------------------------------------------------------------------
// Playlists

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EntryBody {
    entry_id: String,
    timeline_id: String,
}

fn entries_in(entries: Vec<EntryBody>) -> Result<Vec<Entry>, Problem> {
    if entries.len() > org::MAX_MEMBERS {
        return Err(OrgError::from(OrganizationError::TooManyMembers).into());
    }
    ids(
        "entries",
        entries.iter().flat_map(|e| [&e.entry_id, &e.timeline_id]),
    )?;
    Ok(entries
        .into_iter()
        .map(|e| Entry {
            entry_id: e.entry_id,
            timeline_id: e.timeline_id,
        })
        .collect())
}

fn entries_out(entries: &[Entry], hidden: &BTreeSet<String>) -> Vec<EntryBody> {
    entries
        .iter()
        .filter(|e| !hidden.contains(&e.entry_id))
        .map(|e| EntryBody {
            entry_id: e.entry_id.clone(),
            timeline_id: e.timeline_id.clone(),
        })
        .collect()
}

#[derive(Serialize)]
struct PlaylistBody {
    id: String,
    revision: String,
    name: String,
    profile_id: String,
    entries: Vec<EntryBody>,
}

async fn playlist_body(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    p: service::Playlist,
) -> Result<(PlaylistBody, u64), Problem> {
    let hidden = hidden_entries(conn, scope, &p.entries).await?;
    Ok((
        PlaylistBody {
            entries: entries_out(&p.entries, &hidden),
            id: p.id,
            revision: p.revision.to_string(),
            name: p.name,
            profile_id: p.profile_id,
        },
        p.revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaylistInput {
    name: String,
    profile_id: String,
    entries: Vec<EntryBody>,
}

async fn list_playlists(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<PlaylistBody>>, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let mut rows = Vec::new();
    for id in list_ids(&mut tx, "playlists", &caller.principal, &q, limit).await? {
        if let Some(p) = service::load_playlist(&mut tx, &id).await? {
            rows.push((id, playlist_body(&mut tx, &scope, p).await?.0));
        }
    }
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn get_playlist(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let p = service::load_playlist(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&caller.principal, &p.profile_id)?;
    let (body, revision) = playlist_body(&mut tx, &scope, p).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn create_playlist(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<PlaylistInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let key = idempotency_key(&headers)?;
    ids("profile_id", [&body.value.profile_id])?;
    let input = body.value;
    let entries = entries_in(input.entries)?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    target_profile(&mut tx, &principal, &input.profile_id).await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createPlaylist",
        target: "-",
        key: &key,
    };
    if let Some(response) = replay(&mut tx, &principal, &scope, &body.digest).await? {
        return Ok(response);
    }
    let catalog = access::catalog_scope(&principal);
    require_visible_entries(&mut tx, &catalog, &entries).await?;
    let saved =
        service::save_playlist_in(&mut tx, &input.profile_id, None, &input.name, entries).await?;
    let (created, revision) = playlist_body(&mut tx, &catalog, saved).await?;
    acknowledge(tx, &scope, &body.digest, &created, revision).await
}

async fn replace_playlist(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<PlaylistInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CollectionsWrite)?;
    let expected = if_match(&headers)?;
    ids("profile_id", [&body.value.profile_id])?;
    let input = body.value;
    let requested = entries_in(input.entries)?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::CollectionsWrite).await?;
    let current = service::load_playlist(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&principal, &current.profile_id)?;
    org::same_owner(&current.profile_id, &input.profile_id).map_err(OrgError::from)?;
    org::advance(current.revision, expected).map_err(OrgError::from)?;
    let catalog = access::catalog_scope(&principal);
    require_visible_entries(&mut tx, &catalog, &requested).await?;
    let hidden = hidden_entries(&mut tx, &catalog, &current.entries).await?;
    let entries =
        org::keep_hidden_entries(requested, &current.entries, &hidden).map_err(OrgError::from)?;
    let saved = service::save_playlist_in(
        &mut tx,
        &current.profile_id,
        Some((&id, expected)),
        &input.name,
        entries,
    )
    .await?;
    let (body, revision) = playlist_body(&mut tx, &catalog, saved).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn delete_playlist(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    delete_owned(
        &app,
        &caller,
        Permission::CollectionsWrite,
        Owned::Playlist,
        &id,
        &headers,
    )
    .await
}

// ---------------------------------------------------------------------------
// Play queues

#[derive(Serialize)]
struct QueueBody {
    id: String,
    revision: String,
    profile_id: String,
    entries: Vec<EntryBody>,
    current_entry_id: Option<String>,
    repeat: Repeat,
    shuffle_seed: Option<String>,
}

/// The queue as `scope` may see it. A current entry the caller cannot see is
/// reported as no current entry.
async fn queue_body(
    conn: &mut SqliteConnection,
    scope: &CatalogScope,
    id: String,
    profile_id: String,
    q: org::Queue,
) -> Result<(QueueBody, u64), Problem> {
    let hidden = hidden_entries(conn, scope, &q.entries).await?;
    Ok((
        QueueBody {
            id,
            revision: q.revision.to_string(),
            profile_id,
            entries: entries_out(&q.entries, &hidden),
            current_entry_id: q.current.filter(|c| !hidden.contains(c)),
            repeat: q.repeat,
            shuffle_seed: q.shuffle_seed.map(|s| s.to_string()),
        },
        q.revision,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueInput {
    profile_id: String,
    entries: Vec<EntryBody>,
    repeat: Repeat,
    #[serde(deserialize_with = "nullable")]
    shuffle_seed: Option<String>,
}

/// `UInt64`: an unsigned decimal string without leading zeros.
fn seed(value: Option<&str>) -> Result<Option<u64>, Problem> {
    let Some(value) = value else {
        return Ok(None);
    };
    let canonical = value == "0" || (!value.starts_with('0') && value.len() <= 20);
    match value.parse::<u64>() {
        Ok(seed) if canonical && value.bytes().all(|b| b.is_ascii_digit()) => Ok(Some(seed)),
        _ => Err(Problem::invalid(
            "invalid_field",
            "shuffle_seed must be an unsigned 64-bit decimal string or null",
        )),
    }
}

async fn list_queues(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<QueueBody>>, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let mut rows = Vec::new();
    for id in list_ids(&mut tx, "queues", &caller.principal, &q, limit).await? {
        if let Some((profile, queue)) = service::load_queue_by_id(&mut tx, &id).await? {
            let body = queue_body(&mut tx, &scope, id.clone(), profile, queue).await?;
            rows.push((id, body.0));
        }
    }
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn get_queue(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let scope = access::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let (profile, queue) = service::load_queue_by_id(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&caller.principal, &profile)?;
    let (body, revision) = queue_body(&mut tx, &scope, id, profile, queue).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn create_queue(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<QueueInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let key = idempotency_key(&headers)?;
    ids("profile_id", [&body.value.profile_id])?;
    let input = body.value;
    let shuffle_seed = seed(input.shuffle_seed.as_deref())?;
    let entries = entries_in(input.entries)?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::ViewingWrite).await?;
    target_profile(&mut tx, &principal, &input.profile_id).await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createQueue",
        target: "-",
        key: &key,
    };
    if let Some(response) = replay(&mut tx, &principal, &scope, &body.digest).await? {
        return Ok(response);
    }
    let catalog = access::catalog_scope(&principal);
    require_visible_entries(&mut tx, &catalog, &entries).await?;
    let (id, queue) = service::create_queue_in(
        &mut tx,
        &input.profile_id,
        entries,
        input.repeat,
        shuffle_seed,
    )
    .await?;
    let (created, revision) = queue_body(&mut tx, &catalog, id, input.profile_id, queue).await?;
    acknowledge(tx, &scope, &body.digest, &created, revision).await
}

async fn replace_queue(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<QueueInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::ViewingWrite)?;
    let expected = if_match(&headers)?;
    ids("profile_id", [&body.value.profile_id])?;
    let input = body.value;
    let shuffle_seed = seed(input.shuffle_seed.as_deref())?;
    let requested = entries_in(input.entries)?;
    let (_guard, mut tx, principal) = begin(&app, &caller, Permission::ViewingWrite).await?;
    let (profile, current) = service::load_queue_by_id(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    owned(&principal, &profile)?;
    org::same_owner(&profile, &input.profile_id).map_err(OrgError::from)?;
    org::advance(current.revision, expected).map_err(OrgError::from)?;
    let catalog = access::catalog_scope(&principal);
    require_visible_entries(&mut tx, &catalog, &requested).await?;
    let hidden = hidden_entries(&mut tx, &catalog, &current.entries).await?;
    let entries =
        org::keep_hidden_entries(requested, &current.entries, &hidden).map_err(OrgError::from)?;
    let next = service::change_queue_in(
        &mut tx,
        &profile,
        &id,
        expected,
        QueueChange::Edit {
            entries,
            repeat: input.repeat,
            shuffle_seed,
        },
    )
    .await?;
    let (body, revision) = queue_body(&mut tx, &catalog, id, profile, next).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

async fn delete_queue(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    delete_owned(
        &app,
        &caller,
        Permission::ViewingWrite,
        Owned::Queue,
        &id,
        &headers,
    )
    .await
}
