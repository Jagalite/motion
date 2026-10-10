//! Metadata (A04) adapter owned by A08: resolved metadata and attributed
//! contributions, identification proposals and decisions, item artwork
//! listing and authorized asset bytes.
//!
//! Resolution, revisions, proposal decisions and artwork selection rules live
//! in `playscale_core::{metadata, matching, artwork}` and the services in
//! `crate::{metadata, matching}`. This module supplies the caller, the
//! catalog scope and the stored facts, and executes inside one transaction.
use super::{
    Body, Page, PageQuery, Problem, Scope, Stored, auth::Caller, etag, idempotency_key, if_match,
    page, scope_clause,
};
use crate::{
    App,
    db::begin_write,
    matching::{self as service, MatchingError},
    metadata::{self as metadata_service, ContributionError},
};
use axum::{
    Json, Router,
    body::Body as HttpBody,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use playscale_core::{
    access::{self as core, AccessError, CatalogScope, Idempotent, Permission, Revised},
    matching::{Decision, MatchError, Proposal, Status},
    ranges::{RangeDecision, select_range},
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/catalog/items/{item_id}/metadata", get(get_metadata))
        .route(
            "/catalog/items/{item_id}/metadata/contributions/{source_name}",
            put(replace_contribution),
        )
        .route("/catalog/matches", get(list_matches).post(create_match))
        .route("/catalog/matches/{match_id}", get(get_match))
        .route("/catalog/matches/{match_id}/decision", put(decide_match))
        .route("/catalog/items/{item_id}/artwork", get(list_artwork))
        .route(
            "/media/assets/{asset_id}/content",
            get(asset_content).head(asset_content),
        )
}

fn tagged<T: Serialize>(status: StatusCode, body: &T, revision: u64) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(header::ETAG, etag(revision));
    response
}

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

fn conflict(code: &'static str, detail: &str) -> Problem {
    Problem::new(StatusCode::CONFLICT, code, detail)
}

/// Contract `Id`: 1-128 of `[A-Za-z0-9_-]`.
fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A required member whose value may be null (`Option` alone would also
/// accept an absent member).
fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::deserialize(d)
}

/// Whether a live (not merge-retired) item exists and is readable under
/// `scope`. Administrators also see items with no files left; everyone else
/// needs a file in an admitted library. Inaccessible is reported as missing.
async fn visible_item(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    item_id: &str,
) -> Result<bool, Problem> {
    let (clause, all, ids) = scope_clause(scope, "f.library_id");
    let found: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM items i WHERE i.id=? AND i.id NOT IN (SELECT alias_id FROM item_aliases) AND (? OR EXISTS(SELECT 1 FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=i.id AND {clause})))"
    ))
    .bind(item_id)
    .bind(all)
    .bind(all)
    .bind(ids)
    .fetch_one(conn)
    .await?;
    Ok(found)
}

// ---------------------------------------------------------------- metadata

#[derive(Serialize)]
struct ContributionBody {
    source: String,
    revision: String,
    values: BTreeMap<String, Value>,
    tags: Vec<String>,
    excluded_tags: Vec<String>,
    /// Field locking is not modelled by the metadata service; none are locked.
    locked_fields: Vec<String>,
}

#[derive(Serialize)]
struct MetadataBody {
    item_id: String,
    revision: String,
    values: BTreeMap<String, Value>,
    field_sources: BTreeMap<String, Vec<String>>,
    contributions: Vec<ContributionBody>,
}

/// The item's stored documents, its resolved metadata and resource revision.
async fn read_metadata(
    conn: &mut sqlx::SqliteConnection,
    item_id: &str,
) -> Result<(metadata_service::Documents, MetadataBody, u64), Problem> {
    let documents = metadata_service::documents(conn, item_id)
        .await
        .map_err(|e| match e.downcast_ref::<sqlx::Error>() {
            Some(sqlx::Error::RowNotFound) => Problem::not_found(),
            _ => Problem::internal(e),
        })?;
    let revision = playscale_core::metadata::item_revision(
        documents.sources.iter().map(|s| s.revision.max(0) as u64),
    )
    .ok_or_else(|| Problem::internal("metadata revision overflow"))?;
    let resolved = playscale_core::metadata::resolve(&documents.docs);
    let body = MetadataBody {
        item_id: item_id.into(),
        revision: revision.to_string(),
        values: resolved.values,
        field_sources: resolved.field_sources,
        contributions: documents
            .sources
            .iter()
            .map(|s| ContributionBody {
                source: s.source.clone(),
                revision: s.revision.to_string(),
                values: s.values.clone(),
                tags: s.tags.iter().cloned().collect(),
                excluded_tags: s.excluded_tags.iter().cloned().collect(),
                locked_fields: vec![],
            })
            .collect(),
    };
    Ok((documents, body, revision))
}

async fn get_metadata(
    State(app): State<App>,
    caller: Caller,
    Path(item_id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let scope = core::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    if !visible_item(&mut tx, &scope, &item_id).await? {
        return Err(Problem::not_found());
    }
    let (_, body, revision) = read_metadata(&mut tx, &item_id).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataInput {
    values: BTreeMap<String, Value>,
    tags: Vec<String>,
    excluded_tags: Vec<String>,
    locked_fields: Vec<String>,
}

/// Replace one attributed contribution. The precondition names the whole
/// metadata resource; the service then advances that source's own revision.
async fn replace_contribution(
    State(app): State<App>,
    caller: Caller,
    Path((item_id, source)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body<MetadataInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let input = body.value;
    if !metadata_service::valid_source(&source) {
        return Err(Problem::invalid(
            "invalid_source",
            "source_name must be 1-64 lowercase letters, digits or '-'; scan is reserved",
        ));
    }
    if !input.locked_fields.is_empty() {
        return Err(Problem::invalid(
            "locked_fields_unsupported",
            "Field locking is not supported yet; send an empty locked_fields",
        ));
    }
    if input.values.values().any(Value::is_null) {
        return Err(Problem::invalid(
            "null_value_unsupported",
            "Intentional blanks are not supported yet; omit the field instead",
        ));
    }
    if input.tags.len() > 100 || input.excluded_tags.len() > 100 {
        return Err(Problem::invalid("invalid_field", "At most 100 tags"));
    }
    let contribution = metadata_service::validate(
        &source,
        input.values,
        input.tags.into_iter().collect(),
        input.excluded_tags.into_iter().collect(),
    )
    .map_err(|reason| Problem::invalid("invalid_metadata", reason))?;
    // Metadata projection and scan publication share the writer boundary.
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = core::catalog_scope(&principal);
    if !visible_item(&mut tx, &scope, &item_id).await? {
        return Err(Problem::not_found());
    }
    let libraries = sqlx::query_scalar::<_, String>("SELECT DISTINCT ls.library_id FROM library_sources ls JOIN media_files f ON f.library_id=ls.source_id JOIN editions e ON e.id=f.edition_id WHERE e.item_id=? AND f.generated=0")
        .bind(&item_id).fetch_all(&mut *tx).await?.into_iter().collect();
    if !core::may_curate(&scope, &libraries) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "outside_scope",
            "Changing shared metadata requires every containing library",
        ));
    }
    let (documents, current, revision) = read_metadata(&mut tx, &item_id).await?;
    let stored = documents.sources.iter().find(|s| s.source == source);
    let changed = documents.docs.get(&source) != Some(&contribution);
    let (body, revision) = match core::revise(revision, expected, changed)? {
        Revised::Unchanged => (current, revision),
        Revised::Next(_) => {
            metadata_service::replace(
                &mut tx,
                &item_id,
                &source,
                &contribution,
                // Identity is managed by identification; a contribution keeps it.
                stored.and_then(|s| s.external_id.as_deref()),
                stored.map_or(0, |s| s.revision),
            )
            .await
            .map_err(|e| match e {
                ContributionError::NotFound => Problem::not_found(),
                ContributionError::RevisionConflict => AccessError::StaleRevision.into(),
                ContributionError::IdentityPinned => conflict(
                    "manual_identity_pinned",
                    "A manual identification pins this source's external identity",
                ),
                ContributionError::IdentityConflict => conflict(
                    "external_identity_conflict",
                    "External identity already belongs to another item",
                ),
                ContributionError::Storage(e) => Problem::internal(e),
            })?;
            let (_, body, revision) = read_metadata(&mut tx, &item_id).await?;
            (body, revision)
        }
    };
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

// ----------------------------------------------------------------- matches

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRef {
    file_id: String,
    file_revision: String,
}

#[derive(Serialize)]
struct ExternalIdentity {
    provider: String,
    value: String,
    namespace_version: String,
}

#[derive(Serialize)]
struct CandidateBody {
    id: String,
    item_id: Option<String>,
    title: String,
    external_id: Option<ExternalIdentity>,
    reason_codes: Vec<String>,
    /// Candidates carry reason codes, not scores.
    confidence: Option<f64>,
}

#[derive(Serialize)]
struct MatchBody {
    id: String,
    revision: String,
    file: FileRef,
    status: Status,
    candidates: Vec<CandidateBody>,
}

const PROPOSAL: &str = "p.id,p.revision,p.file_id,p.file_revision,p.status,p.candidates_json,p.decision_json FROM match_proposals p JOIN media_files f ON f.id=p.file_id";

/// Candidate works the principal may read. Candidates naming no local work
/// are evidence without catalog content and stay visible.
async fn readable_items(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    proposals: &[Proposal],
) -> Result<BTreeSet<String>, Problem> {
    let wanted: BTreeSet<&str> = proposals
        .iter()
        .flat_map(|p| p.candidates.iter().filter_map(|c| c.item_id.as_deref()))
        .collect();
    let mut readable = BTreeSet::new();
    for item in wanted {
        if visible_item(conn, scope, item).await? {
            readable.insert(item.to_string());
        }
    }
    Ok(readable)
}

fn match_body(p: Proposal, readable: &BTreeSet<String>) -> MatchBody {
    MatchBody {
        id: p.id,
        revision: p.revision.to_string(),
        file: FileRef {
            file_id: p.file_id,
            file_revision: p.file_revision,
        },
        status: p.status,
        candidates: p
            .candidates
            .into_iter()
            .filter(|c| c.item_id.as_ref().is_none_or(|i| readable.contains(i)))
            .map(|c| CandidateBody {
                id: c.id,
                item_id: c.item_id,
                title: c.title,
                // The namespace is stored whole; its version is not modelled.
                external_id: c.external_id.map(|(provider, value)| ExternalIdentity {
                    provider,
                    value,
                    namespace_version: String::new(),
                }),
                reason_codes: c.reason_codes,
                confidence: None,
            })
            .collect(),
    }
}

/// A proposal whose file lies in a library readable under `scope`.
async fn load_match(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    id: &str,
) -> Result<Proposal, Problem> {
    let (clause, all, ids) = scope_clause(scope, "f.library_id");
    let row: service::Row = sqlx::query_as(&format!("SELECT {PROPOSAL} WHERE p.id=? AND {clause}"))
        .bind(id)
        .bind(all)
        .bind(ids)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(Problem::not_found)?;
    service::decode(row).map_err(Problem::internal)
}

async fn respond_match(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    proposal: Proposal,
) -> Result<(MatchBody, u64), Problem> {
    let readable = readable_items(conn, scope, std::slice::from_ref(&proposal)).await?;
    let revision = proposal.revision;
    Ok((match_body(proposal, &readable), revision))
}

fn matching_problem(e: MatchingError) -> Problem {
    match e {
        MatchingError::Rejected(e) => match e {
            MatchError::StaleProposal => conflict(
                "match_stale",
                "The proposal is stale; raise a new proposal for the current file",
            ),
            MatchError::FileChanged => conflict(
                "file_changed",
                "The file changed since the proposal was raised",
            ),
            MatchError::AlreadyDecided => {
                conflict("match_already_decided", "The proposal was already decided")
            }
            MatchError::UnknownCandidate(_) => Problem::invalid(
                "unknown_candidate",
                "candidate_id is not a candidate of this proposal",
            ),
            MatchError::RevisionExhausted => {
                conflict("revision_exhausted", "The proposal cannot change further")
            }
            MatchError::TooManyCandidates => conflict(
                "too_many_candidates",
                "The proposal has too many candidates",
            ),
        },
        MatchingError::SplitRequired => conflict(
            "split_required",
            "The file shares its work with other versions; split it first",
        ),
        MatchingError::IdentityTaken => conflict(
            "external_identity_conflict",
            "Another work already holds the provider identity",
        ),
        MatchingError::Curation(crate::curation::CurationError::Rejected(e)) => {
            tracing::info!(error=?e, "identification merge rejected");
            conflict(
                "merge_rejected",
                "The works cannot be merged in their current state",
            )
        }
        MatchingError::Curation(crate::curation::CurationError::Storage(e)) => Problem::internal(e),
        MatchingError::NotFound => Problem::not_found(),
        MatchingError::Storage(e) => Problem::internal(e),
    }
}

async fn list_matches(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<MatchBody>>, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let limit = q.limit()?;
    let scope = core::catalog_scope(&caller.principal);
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    let mut tx = app.db.begin().await?;
    // Scope applies in the query, before the cursor and limit.
    let rows: Vec<service::Row> = sqlx::query_as(&format!(
        "SELECT {PROPOSAL} WHERE p.id>? AND {clause} ORDER BY p.id LIMIT ?"
    ))
    .bind(q.after()?)
    .bind(all)
    .bind(ids)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let proposals = rows
        .into_iter()
        .map(service::decode)
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(Problem::internal)?;
    let readable = readable_items(&mut tx, &scope, &proposals).await?;
    let rows = proposals
        .into_iter()
        .map(|p| (p.id.clone(), match_body(p, &readable)))
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

async fn get_match(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let scope = core::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    let proposal = load_match(&mut tx, &scope, &id).await?;
    let (body, revision) = respond_match(&mut tx, &scope, proposal).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchInput {
    file: FileRef,
    library_id: String,
    #[serde(deserialize_with = "nullable")]
    provider: Option<String>,
}

/// Raise or refresh the proposal for the file's current revision.
async fn create_match(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<MatchInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let key = idempotency_key(&headers)?;
    let (input, digest) = (body.value, body.digest);
    if !valid_id(&input.file.file_id)
        || !valid_id(&input.file.file_revision)
        || !valid_id(&input.library_id)
        || input.provider.as_ref().is_some_and(|p| p.len() > 64)
    {
        return Err(Problem::invalid(
            "invalid_field",
            "Invalid file or library ID",
        ));
    }
    if input.provider.is_some() {
        return Err(Problem::invalid(
            "provider_unavailable",
            "No metadata provider is configured; send provider null for local matching",
        ));
    }
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createMatch",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        let scope = core::catalog_scope(&principal);
        let mut stored = stored.unwrap();
        let body = stored
            .body
            .as_mut()
            .ok_or_else(|| Problem::internal("missing match receipt"))?;
        let id = body["id"]
            .as_str()
            .ok_or_else(|| Problem::internal("invalid match receipt"))?;
        // A saved acknowledgement is not an authorization snapshot. Keep its
        // revision/outcome, but hide candidates no longer visible to this caller.
        load_match(&mut tx, &scope, id).await?;
        let candidates = body["candidates"]
            .as_array_mut()
            .ok_or_else(|| Problem::internal("invalid match candidates"))?;
        let mut visible = Vec::new();
        for candidate in candidates.drain(..) {
            if let Some(item) = candidate["item_id"].as_str()
                && !visible_item(&mut tx, &scope, item).await?
            {
                continue;
            }
            visible.push(candidate);
        }
        *candidates = visible;
        return Ok(replayed(stored));
    }
    let scope = core::catalog_scope(&principal);
    let file: Option<(String, String)> =
        sqlx::query_as("SELECT f.revision,ls.library_id FROM media_files f JOIN library_sources ls ON ls.source_id=f.library_id WHERE f.id=? AND f.generated=0 AND ls.library_id=?")
            .bind(&input.file.file_id).bind(&input.library_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (revision, _) = file
        .filter(|(_, library)| *library == input.library_id && scope.library(library))
        .ok_or_else(Problem::not_found)?;
    if revision != input.file.file_revision {
        return Err(conflict(
            "file_changed",
            "The file revision changed; read the file again",
        ));
    }
    require_curatable_file(&mut tx, &scope, &input.file.file_id).await?;
    let proposal = service::propose_in_transaction(&mut tx, &input.file.file_id)
        .await
        .map_err(matching_problem)?;
    let (body, revision) = respond_match(&mut tx, &scope, proposal).await?;
    let json = serde_json::to_value(&body).map_err(Problem::internal)?;
    record
        .save(&mut tx, &digest, StatusCode::ACCEPTED, Some(&json), None)
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::ACCEPTED, &json, revision))
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DecisionKind {
    Accept,
    Reject,
    Defer,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchDecision {
    decision: DecisionKind,
    #[serde(deserialize_with = "nullable")]
    candidate_id: Option<String>,
}

async fn decide_match(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<MatchDecision>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogWrite)?;
    let expected = if_match(&headers)?;
    let input = body.value;
    let decision = match (input.decision, input.candidate_id) {
        (DecisionKind::Accept, Some(candidate_id)) if candidate_id.len() <= 256 => {
            Decision::Accept { candidate_id }
        }
        (DecisionKind::Reject, None) => Decision::Reject,
        (DecisionKind::Defer, None) => Decision::Defer,
        _ => {
            return Err(Problem::invalid(
                "invalid_decision",
                "candidate_id is required for accept and must be null otherwise",
            ));
        }
    };
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    let scope = core::catalog_scope(&principal);
    let proposal = load_match(&mut tx, &scope, &id).await?;
    require_curatable_file(&mut tx, &scope, &proposal.file_id).await?;
    if let Decision::Accept { candidate_id } = &decision {
        // A candidate work the principal cannot read is indistinguishable
        // from one that is not a candidate.
        let hidden = match proposal.candidates.iter().find(|c| &c.id == candidate_id) {
            Some(c) => match &c.item_id {
                Some(item) => !visible_item(&mut tx, &scope, item).await?,
                None => false,
            },
            None => false,
        };
        if hidden {
            return Err(matching_problem(MatchingError::Rejected(
                MatchError::UnknownCandidate(candidate_id.clone()),
            )));
        }
    }
    let decided = service::decide_in_transaction(&mut tx, &id, expected, decision)
        .await
        .map_err(|e| match e {
            // The stated precondition names another revision of the proposal.
            MatchingError::Rejected(MatchError::StaleProposal) if proposal.revision != expected => {
                AccessError::StaleRevision.into()
            }
            e => matching_problem(e),
        })?;
    let (body, revision) = respond_match(&mut tx, &scope, decided.proposal).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

// ----------------------------------------------------------------- artwork

/// Artwork assets are immutable and content-addressed by their SHA-256.
const ASSET_REVISION: u64 = 1;

#[derive(Serialize)]
struct AssetBody {
    id: String,
    revision: String,
    digest: String,
    kind: &'static str,
    mime_type: String,
    size_bytes: String,
    content_url: String,
    source_revision: Option<String>,
}

fn asset_body(id: String, mime: String, size: i64) -> AssetBody {
    AssetBody {
        digest: format!("sha256:{id}"),
        revision: ASSET_REVISION.to_string(),
        kind: "artwork",
        mime_type: mime,
        size_bytes: size.max(0).to_string(),
        content_url: format!("/api/v2/media/assets/{id}/content?revision={ASSET_REVISION}"),
        source_revision: None,
        id,
    }
}

/// Assets contributed to the item, in any role, by any source.
async fn list_artwork(
    State(app): State<App>,
    caller: Caller,
    Path(item_id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<AssetBody>>, Problem> {
    caller.require(Permission::CatalogRead)?;
    let limit = q.limit()?;
    let scope = core::catalog_scope(&caller.principal);
    let mut tx = app.db.begin().await?;
    if !visible_item(&mut tx, &scope, &item_id).await? {
        return Err(Problem::not_found());
    }
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT a.id,a.mime,length(a.bytes) FROM artwork_assets a WHERE a.id IN (SELECT asset_id FROM artwork_contributions WHERE item_id=?) AND a.id>? ORDER BY a.id LIMIT ?",
    )
    .bind(&item_id)
    .bind(q.after()?)
    .bind(i64::from(limit) + 1)
    .fetch_all(&mut *tx)
    .await?;
    let rows = rows
        .into_iter()
        .map(|(id, mime, size)| (id.clone(), asset_body(id, mime, size)))
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}

#[derive(Deserialize)]
struct AssetQuery {
    revision: Option<String>,
}

fn etag_matches(list: &str, tag: &str, weak: bool) -> bool {
    list.split(',').any(|candidate| {
        let candidate = candidate.trim();
        let candidate = if weak {
            candidate.strip_prefix("W/").unwrap_or(candidate)
        } else {
            candidate
        };
        candidate == "*" || candidate == tag
    })
}

/// Asset bytes. Existence is disclosed only when an owning item is readable
/// under the caller's catalog scope; the revision is checked after that.
async fn asset_content(
    State(app): State<App>,
    caller: Caller,
    method: Method,
    Path(asset_id): Path<String>,
    query: Result<Query<AssetQuery>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let Query(query) =
        query.map_err(|_| Problem::bad("invalid_query", "The query parameters are invalid"))?;
    let scope = core::catalog_scope(&caller.principal);
    let (clause, all, ids) = scope_clause(&scope, "f.library_id");
    let mut conn = app.db.acquire().await?;
    let asset: Option<(String, Vec<u8>)> = sqlx::query_as(&format!(
        "SELECT a.mime,a.bytes FROM artwork_assets a WHERE a.id=? AND (? OR EXISTS(SELECT 1 FROM artwork_contributions c JOIN editions e ON e.item_id=c.item_id JOIN media_files f ON f.edition_id=e.id WHERE c.asset_id=a.id AND c.item_id NOT IN (SELECT alias_id FROM item_aliases) AND {clause}))"
    ))
    .bind(&asset_id)
    .bind(all)
    .bind(all)
    .bind(ids)
    .fetch_optional(&mut *conn)
    .await?;
    drop(conn);
    let (mime, bytes) = asset.ok_or_else(Problem::not_found)?;
    let revision = query
        .revision
        .ok_or_else(|| Problem::bad("revision_required", "Send the asset revision"))?;
    if revision != ASSET_REVISION.to_string() {
        return Err(conflict(
            "revision_mismatch",
            "The asset revision does not exist; read the asset again",
        ));
    }
    let tag = etag(ASSET_REVISION);
    let tag_text = tag.to_str().unwrap_or_default().to_owned();
    let get = |name| {
        headers
            .get(name)
            .and_then(|v: &HeaderValue| v.to_str().ok())
    };
    let builder = || {
        Response::builder()
            .header(header::ETAG, tag.clone())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_TYPE, mime.as_str())
            .header("x-content-type-options", "nosniff")
    };
    if get(header::IF_MATCH).is_some_and(|v| !etag_matches(v, &tag_text, false)) {
        return Err(AccessError::StaleRevision.into());
    }
    if get(header::IF_NONE_MATCH).is_some_and(|v| etag_matches(v, &tag_text, true)) {
        return builder()
            .status(StatusCode::NOT_MODIFIED)
            .body(HttpBody::empty())
            .map_err(Problem::internal);
    }
    let len = bytes.len() as u64;
    // Only a strong entity tag admits If-Range; anything else sends the whole.
    let use_range = method == Method::GET && get(header::IF_RANGE).is_none_or(|v| v == tag_text);
    let decision = select_range(if use_range { get(header::RANGE) } else { None }, len);
    let (status, start, end) = match decision {
        RangeDecision::Unsatisfiable => {
            let mut response = Problem::new(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                "The requested range lies outside the asset",
            )
            .into_response();
            let range =
                HeaderValue::from_str(&format!("bytes */{len}")).map_err(Problem::internal)?;
            response.headers_mut().insert(header::CONTENT_RANGE, range);
            return Ok(response);
        }
        RangeDecision::Full => (StatusCode::OK, 0, len),
        RangeDecision::Partial { start, end } => (StatusCode::PARTIAL_CONTENT, start, end + 1),
    };
    let mut response = builder()
        .status(status)
        .header(header::CONTENT_LENGTH, end - start);
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{len}", end - 1),
        );
    }
    let body = if method == Method::HEAD {
        HttpBody::empty()
    } else {
        HttpBody::from(bytes[start as usize..end as usize].to_vec())
    };
    response.body(body).map_err(Problem::internal)
}

async fn require_curatable_file(
    conn: &mut sqlx::SqliteConnection,
    scope: &CatalogScope,
    file: &str,
) -> Result<(), Problem> {
    let libraries:std::collections::BTreeSet<String>=sqlx::query_scalar::<_,String>("SELECT ls.library_id FROM library_sources ls JOIN media_files f ON f.library_id=ls.source_id WHERE f.id=?")
 .bind(file).fetch_all(conn).await?.into_iter().collect();
    if core::may_curate(scope, &libraries) {
        Ok(())
    } else {
        Err(Problem::new(
            StatusCode::FORBIDDEN,
            "outside_scope",
            "Changing this shared file requires every containing library",
        ))
    }
}
