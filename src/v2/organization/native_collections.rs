//! Manual organization remains Motion-owned; native works alone determine
//! references, aliases, ordering and current disclosure. No collection grants
//! catalog access and no legacy alias/member join supplies authority.
use super::*;
use crate::native_catalog::Runtime;
use catabolic_store::catalog_scope::TrustedSelection;
use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use std::collections::BTreeMap;

pub(super) fn unavailable() -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_organization_unavailable",
        "This organization shape requires an additional native adapter",
    )
}
fn invalid_reference() -> Problem {
    OrgError::InvalidReference(String::new()).into()
}
pub(super) struct Context {
    pub(super) tx: Transaction<'static, Sqlite>,
    pub(super) principal: Principal,
    pub(super) runtime: std::sync::Arc<Runtime>,
    pub(super) selection: Option<TrustedSelection>,
    pub(super) generation: i64,
    pub(super) catalog: String,
    pub(super) incarnation: String,
    named: BTreeMap<String, Value>,
    named_bytes: usize,
    visible: BTreeMap<String, Option<String>>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}
impl Context {
    pub(super) async fn open(app: &App, caller: &Caller) -> Result<Self, Problem> {
        caller.require(Permission::CollectionsWrite)?;
        let runtime = app.native_catalog.as_ref().ok_or_else(unavailable)?.clone();
        let mut first = app.db.begin().await?;
        let principal = caller
            .reauthorize(&mut first, Some(Permission::CollectionsWrite))
            .await?;
        let scope = access::catalog_scope(&principal);
        first.commit().await?;
        let scoped = match &scope {
            CatalogScope::Libraries(ids) => Some(
                runtime
                    .prepare_library_read_scope(ids.clone())
                    .await
                    .map_err(super::super::catalog::native_read_error)?,
            ),
            _ => None,
        };
        let selection = match (&scope, &scoped) {
            (CatalogScope::Nothing, _) => None,
            (_, Some(s)) => Some(s.selection.clone()),
            _ => Some(
                runtime
                    .prepare_all_scope()
                    .await
                    .map_err(super::super::catalog::native_read_error)?,
            ),
        };
        runtime
            .rebuild_projection()
            .await
            .map_err(super::super::catalog::native_read_error)?;
        let guard = app.jobs.clone().lock_owned().await;
        let mut tx: Transaction<'static, Sqlite> = app.db.begin_with("BEGIN IMMEDIATE").await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::CollectionsWrite))
            .await?;
        if access::catalog_scope(&principal) != scope {
            return Err(unavailable());
        }
        runtime
            .validate_projection(&mut tx)
            .await
            .map_err(super::super::catalog::native_read_error)?;
        if let Some(s) = &scoped {
            runtime
                .validate_library_read_scope(&mut tx, s)
                .await
                .map_err(super::super::catalog::native_read_error)?;
        }
        let(catalog,incarnation,generation)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(&mut *tx).await?;
        Ok(Self {
            tx,
            principal,
            runtime,
            selection,
            generation,
            catalog,
            incarnation,
            named: BTreeMap::new(),
            named_bytes: 0,
            visible: BTreeMap::new(),
            _guard: guard,
        })
    }
    async fn load_named(&mut self, ids: &[String]) -> Result<(), Problem> {
        let missing: Vec<_> = ids
            .iter()
            .filter(|id| !self.named.contains_key(*id))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if self.named.len().saturating_add(missing.len()) > 10000 {
            return Err(unavailable());
        }
        for chunk in missing.chunks(200) {
            let names = chunk.to_vec();
            let generation = self.generation;
            let facts = self
                .runtime
                .with_host(move |host| {
                    host.projection_named_items(generation, &names)
                        .map_err(Into::into)
                })
                .await
                .map_err(super::super::catalog::native_read_error)?;
            if facts["catalog_id"] != self.catalog
                || facts["incarnation"] != self.incarnation
                || facts["generation"] != self.generation
            {
                return Err(unavailable());
            }
            let rows = facts["items"].as_array().ok_or_else(unavailable)?;
            let absent = facts["missing_ids"].as_array().ok_or_else(unavailable)?;
            let mut seen = BTreeSet::new();
            for row in rows {
                let id = row["item_id"].as_str().ok_or_else(unavailable)?;
                if !chunk.iter().any(|v| v == id) || !seen.insert(id.to_owned()) {
                    return Err(unavailable());
                }
                if !matches!(row["kind"].as_str(), Some("video" | "custom:motion_audio"))
                    || row["metadata"]["motion_catalog"]["item_id"] != id
                    || row["metadata"]["motion_catalog"]["contract_version"] != 1
                {
                    return Err(unavailable());
                }
                self.named_bytes = self
                    .named_bytes
                    .checked_add(serde_json::to_vec(row).map_err(|_| unavailable())?.len())
                    .filter(|total| *total <= 8 * 1024 * 1024)
                    .ok_or_else(unavailable)?;
                self.named.insert(id.into(), row.clone());
            }
            for id in absent {
                let id = id.as_str().ok_or_else(unavailable)?;
                if !chunk.iter().any(|v| v == id) || !seen.insert(id.to_owned()) {
                    return Err(unavailable());
                }
                self.named.insert(id.into(), Value::Null);
            }
            if seen.len() != chunk.len() {
                return Err(unavailable());
            }
        }
        Ok(())
    }
    async fn resolve(
        &mut self,
        ids: &[String],
        stored: bool,
    ) -> Result<BTreeMap<String, Option<String>>, Problem> {
        if ids.len() > org::MAX_MEMBERS {
            return Err(unavailable());
        }
        self.load_named(ids).await?;
        let mut result = BTreeMap::new();
        for id in ids {
            let mut current = id.clone();
            let mut visited = BTreeSet::new();
            loop {
                if visited.len() >= 32 || !visited.insert(current.clone()) {
                    return Err(unavailable());
                }
                self.load_named(&[current.clone()]).await?;
                let row = self.named.get(&current).ok_or_else(unavailable)?;
                if row.is_null() {
                    if stored {
                        return Err(unavailable());
                    } else {
                        result.insert(id.clone(), None);
                        break;
                    }
                }
                let retirement = &row["retirement"];
                if retirement.is_null() {
                    result.insert(id.clone(), Some(current));
                    break;
                }
                match retirement["kind"].as_str() {
                    Some("merge") => {
                        current = retirement["survivor_item_id"]
                            .as_str()
                            .filter(|v| is_id(v))
                            .ok_or_else(unavailable)?
                            .into()
                    }
                    Some("split") => {
                        result.insert(id.clone(), None);
                        break;
                    }
                    _ => return Err(unavailable()),
                }
            }
        }
        Ok(result)
    }
    pub(super) async fn title(&mut self, id: &str) -> Result<Option<String>, Problem> {
        if let Some(v) = self.visible.get(id) {
            return Ok(v.clone());
        }
        let Some(selection) = &self.selection else {
            return Ok(None);
        };
        let rows = match self
            .runtime
            .read_items(&mut self.tx, vec![selection.clone()], Some(id.into()))
            .await
        {
            Ok(rows) => rows,
            Err(e)
                if e.downcast_ref::<crate::native_catalog::ReadFailure>()
                    .is_some_and(|e| {
                        e.0 == catabolic_store::catalog_scope::ScopeErrorCode::NotFoundOrNotVisible
                    }) =>
            {
                Vec::new()
            }
            Err(e) => return Err(super::super::catalog::native_read_error(e)),
        };
        if rows.len() > 1 {
            return Err(unavailable());
        }
        let title = if let Some(row) = rows.first() {
            if row[0] != id {
                return Err(unavailable());
            }
            let metadata: Value = serde_json::from_str(row[2].as_str().ok_or_else(unavailable)?)
                .map_err(|_| unavailable())?;
            Some(
                metadata["title"]
                    .as_str()
                    .ok_or_else(unavailable)?
                    .to_owned(),
            )
        } else {
            None
        };
        self.visible.insert(id.into(), title.clone());
        Ok(title)
    }
    async fn visible_members(
        &mut self,
        ids: &[String],
        stored: bool,
    ) -> Result<(Vec<String>, BTreeSet<String>), Problem> {
        let resolved = self.resolve(ids, stored).await?;
        let mut sorted = BTreeSet::new();
        let mut hidden = BTreeSet::new();
        for raw in ids {
            match resolved.get(raw).and_then(Option::as_ref) {
                Some(id) => match self.title(id).await? {
                    Some(title) => {
                        sorted.insert((title, id.clone()));
                    }
                    None => {
                        hidden.insert(raw.clone());
                    }
                },
                None => {
                    hidden.insert(raw.clone());
                }
            }
        }
        Ok((sorted.into_iter().map(|(_, id)| id).collect(), hidden))
    }
    async fn require_requested(&mut self, ids: &[String]) -> Result<(), Problem> {
        let resolved = self.resolve(ids, false).await?;
        for id in ids {
            if resolved.get(id).and_then(Option::as_ref) != Some(id)
                || self.title(id).await?.is_none()
            {
                return Err(invalid_reference());
            }
        }
        Ok(())
    }
    async fn body(&mut self, mut row: service::Collection) -> Result<CollectionBody, Problem> {
        if row.kind != CollectionKind::Manual || row.filter_id.is_some() {
            return Err(unavailable());
        }
        row.item_ids = self.visible_members(&row.item_ids, true).await?.0;
        Ok(CollectionBody {
            id: row.id,
            revision: row.revision.to_string(),
            name: row.name,
            profile_id: row.profile_id,
            kind: row.kind,
            item_ids: row.item_ids,
            filter_id: None,
        })
    }
}

/// Read only Motion-owned aggregate rows, not compatibility catalog joins.
async fn load(conn: &mut SqliteConnection, id: &str) -> Result<service::Collection, Problem> {
    let row: Option<(i64, String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT revision,profile_id,name,kind,filter_id FROM collections WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let (revision, profile_id, name, kind, filter_id) = row.ok_or_else(Problem::not_found)?;
    if kind != "manual" || filter_id.is_some() {
        return Err(unavailable());
    }
    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT item_id FROM collection_items WHERE collection_id=? ORDER BY item_id LIMIT 1001",
    )
    .bind(id)
    .fetch_all(conn)
    .await?;
    if item_ids.len() > org::MAX_MEMBERS {
        return Err(unavailable());
    }
    Ok(service::Collection {
        id: id.into(),
        revision: u64::try_from(revision).map_err(|_| unavailable())?,
        profile_id,
        name,
        kind: CollectionKind::Manual,
        item_ids,
        filter_id,
    })
}
async fn persist(
    conn: &mut SqliteConnection,
    profile: &str,
    existing: Option<(&str, u64)>,
    name: &str,
    members: Vec<String>,
) -> Result<service::Collection, Problem> {
    org::validate_collection(name, CollectionKind::Manual, &members, None)
        .map_err(OrgError::from)?;
    let (id, revision) = if let Some((id, expected)) = existing {
        let current: i64 =
            sqlx::query_scalar("SELECT revision FROM collections WHERE id=? AND profile_id=?")
                .bind(id)
                .bind(profile)
                .fetch_one(&mut *conn)
                .await?;
        let next = org::advance(u64::try_from(current).map_err(|_| unavailable())?, expected)
            .map_err(OrgError::from)?;
        let stored = i64::try_from(next).map_err(|_| unavailable())?;
        sqlx::query("UPDATE collections SET revision=?,name=? WHERE id=?")
            .bind(stored)
            .bind(name.trim())
            .bind(id)
            .execute(&mut *conn)
            .await?;
        sqlx::query("DELETE FROM collection_items WHERE collection_id=?")
            .bind(id)
            .execute(&mut *conn)
            .await?;
        (id.into(), next)
    } else {
        let id = crate::new_id();
        sqlx::query("INSERT INTO collections(id,revision,profile_id,name,kind,filter_id) VALUES(?,1,?,?,'manual',NULL)").bind(&id).bind(profile).bind(name.trim()).execute(&mut *conn).await?;
        (id, 1)
    };
    for item in &members {
        sqlx::query("INSERT INTO collection_items(collection_id,item_id) VALUES(?,?)")
            .bind(&id)
            .bind(item)
            .execute(&mut *conn)
            .await?;
    }
    Ok(service::Collection {
        id,
        revision,
        profile_id: profile.into(),
        name: name.trim().into(),
        kind: CollectionKind::Manual,
        item_ids: members,
        filter_id: None,
    })
}
fn manual(input: &CollectionInput) -> Result<(), Problem> {
    input.check()?;
    if input.kind != CollectionKind::Manual || input.filter_id.is_some() {
        return Err(unavailable());
    }
    org::validate_collection(&input.name, input.kind, &input.item_ids, None)
        .map_err(OrgError::from)?;
    Ok(())
}

pub(super) async fn list(
    app: App,
    caller: Caller,
    q: PageQuery,
) -> Result<Json<Page<CollectionBody>>, Problem> {
    let limit = q.limit()?;
    let mut ctx = Context::open(&app, &caller).await?;
    let mut rows = Vec::new();
    for id in list_ids(&mut ctx.tx, "collections", &ctx.principal, &q, limit).await? {
        let row = load(&mut ctx.tx, &id).await?;
        owned(&ctx.principal, &row.profile_id)?;
        rows.push((id, ctx.body(row).await?));
    }
    let page = page(&mut ctx.tx, &ctx.principal, rows, limit).await?;
    ctx.tx.commit().await?;
    Ok(Json(page))
}
pub(super) async fn get(app: App, caller: Caller, id: String) -> Result<Response, Problem> {
    let mut ctx = Context::open(&app, &caller).await?;
    let row = load(&mut ctx.tx, &id).await?;
    owned(&ctx.principal, &row.profile_id)?;
    let revision = row.revision;
    let body = ctx.body(row).await?;
    ctx.tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}
pub(super) async fn create(
    app: App,
    caller: Caller,
    headers: HeaderMap,
    body: Body<CollectionInput>,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    manual(&body.value)?;
    let mut ctx = Context::open(&app, &caller).await?;
    target_profile(&mut ctx.tx, &ctx.principal, &body.value.profile_id).await?;
    let principal = ctx.principal.id.clone();
    let scope = Scope {
        principal: &principal,
        operation: "createCollection",
        target: "-",
        key: &key,
    };
    let stored = scope.load(&mut ctx.tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        let mut stored = stored.ok_or_else(unavailable)?;
        if let Some(value) = stored.body.as_mut() {
            if value["kind"] != "manual" || value["profile_id"] != body.value.profile_id {
                return Err(unavailable());
            }
            let ids: Vec<String> =
                serde_json::from_value(value["item_ids"].clone()).map_err(|_| unavailable())?;
            value["item_ids"] = json!(ctx.visible_members(&ids, true).await?.0);
        }
        ctx.tx.commit().await?;
        return Ok(replayed(stored));
    }
    ctx.require_requested(&body.value.item_ids).await?;
    let row = persist(
        &mut ctx.tx,
        &body.value.profile_id,
        None,
        &body.value.name,
        body.value.item_ids,
    )
    .await?;
    let revision = row.revision;
    let response = ctx.body(row).await?;
    acknowledge(ctx.tx, &scope, &body.digest, &response, revision).await
}
pub(super) async fn replace(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
    body: Body<CollectionInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    manual(&body.value)?;
    let mut ctx = Context::open(&app, &caller).await?;
    let current = load(&mut ctx.tx, &id).await?;
    owned(&ctx.principal, &current.profile_id)?;
    org::same_owner(&current.profile_id, &body.value.profile_id).map_err(OrgError::from)?;
    org::advance(current.revision, expected).map_err(OrgError::from)?;
    ctx.require_requested(&body.value.item_ids).await?;
    let hidden = ctx.visible_members(&current.item_ids, true).await?.1;
    let members = org::keep_hidden_members(body.value.item_ids, &current.item_ids, &hidden);
    let row = persist(
        &mut ctx.tx,
        &current.profile_id,
        Some((&id, expected)),
        &body.value.name,
        members,
    )
    .await?;
    let revision = row.revision;
    let response = ctx.body(row).await?;
    ctx.tx.commit().await?;
    Ok(tagged(StatusCode::OK, &response, revision))
}
pub(super) async fn delete(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut ctx = Context::open(&app, &caller).await?;
    let current = load(&mut ctx.tx, &id).await?;
    owned(&ctx.principal, &current.profile_id)?;
    service::delete_owned_in(&mut ctx.tx, Owned::Collection, &id, expected).await?;
    ctx.tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
