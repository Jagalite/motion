//! Ordered app-owned references, with current typed native identity and work
//! visibility. A playlist is not a readiness, file, equivalence or byte grant.
use super::native_collections::{Context, unavailable};
use super::*;
use serde_json::json;

struct Timelines {
    ctx: Context,
    reader: crate::native_catalog::timeline_visibility::Reader,
}
impl Timelines {
    async fn open(app: &App, caller: &Caller) -> Result<Self, Problem> {
        let mut ctx = Context::open(app, caller).await?;
        let reader = crate::native_catalog::timeline_visibility::Reader::new(
            ctx.runtime.clone(),
            &mut ctx.tx,
            ctx.selection.iter().cloned().collect(),
        )
        .await
        .map_err(super::super::catalog::native_read_error)?;
        Ok(Self { ctx, reader })
    }
    async fn visible(&mut self, id: &str, stored: bool) -> Result<bool, Problem> {
        use crate::native_catalog::timeline_visibility::Visibility;
        match self
            .reader
            .read(&mut self.ctx.tx, id)
            .await
            .map_err(super::super::catalog::native_read_error)?
        {
            Visibility::Visible { .. } => Ok(true),
            Visibility::Hidden | Visibility::Retired => Ok(false),
            Visibility::Missing if !stored => Ok(false),
            Visibility::Missing => Err(unavailable()),
        }
    }
    async fn hidden(&mut self, entries: &[Entry]) -> Result<BTreeSet<String>, Problem> {
        org::validate_entries(entries).map_err(OrgError::from)?;
        let mut hidden = BTreeSet::new();
        for entry in entries {
            if !self.visible(&entry.timeline_id, true).await? {
                hidden.insert(entry.entry_id.clone());
            }
        }
        Ok(hidden)
    }
    async fn requested(&mut self, entries: &[Entry]) -> Result<(), Problem> {
        org::validate_entries(entries).map_err(OrgError::from)?;
        for entry in entries {
            if !self.visible(&entry.timeline_id, false).await? {
                return Err(OrgError::InvalidReference(String::new()).into());
            }
        }
        Ok(())
    }
    async fn body(&mut self, p: service::Playlist) -> Result<PlaylistBody, Problem> {
        let hidden = self.hidden(&p.entries).await?;
        Ok(PlaylistBody {
            id: p.id,
            revision: p.revision.to_string(),
            name: p.name,
            profile_id: p.profile_id,
            entries: entries_out(&p.entries, &hidden),
        })
    }
}
async fn load(conn: &mut SqliteConnection, id: &str) -> Result<service::Playlist, Problem> {
    // Check the app aggregate size before its existing loader allocates entries.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM playlist_entries WHERE playlist_id=?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    if count > org::MAX_MEMBERS as i64 {
        return Err(unavailable());
    }
    service::load_playlist(conn, id)
        .await?
        .ok_or_else(Problem::not_found)
}
pub(super) async fn list(
    app: App,
    caller: Caller,
    q: PageQuery,
) -> Result<Json<Page<PlaylistBody>>, Problem> {
    let limit = q.limit()?;
    let mut t = Timelines::open(&app, &caller).await?;
    let mut rows = Vec::new();
    for id in list_ids(&mut t.ctx.tx, "playlists", &t.ctx.principal, &q, limit).await? {
        let row = load(&mut t.ctx.tx, &id).await?;
        owned(&t.ctx.principal, &row.profile_id)?;
        rows.push((id, t.body(row).await?));
    }
    let page = page(&mut t.ctx.tx, &t.ctx.principal, rows, limit).await?;
    t.ctx.tx.commit().await?;
    Ok(Json(page))
}
pub(super) async fn get(app: App, caller: Caller, id: String) -> Result<Response, Problem> {
    let mut t = Timelines::open(&app, &caller).await?;
    let row = load(&mut t.ctx.tx, &id).await?;
    owned(&t.ctx.principal, &row.profile_id)?;
    let revision = row.revision;
    let body = t.body(row).await?;
    t.ctx.tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}
pub(super) async fn create(
    app: App,
    caller: Caller,
    headers: HeaderMap,
    body: Body<PlaylistInput>,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    ids("profile_id", [&body.value.profile_id])?;
    let entries = entries_in(body.value.entries)?;
    org::valid_name(&body.value.name).map_err(OrgError::from)?;
    org::validate_entries(&entries).map_err(OrgError::from)?;
    let mut t = Timelines::open(&app, &caller).await?;
    target_profile(&mut t.ctx.tx, &t.ctx.principal, &body.value.profile_id).await?;
    let principal = t.ctx.principal.id.clone();
    let scope = Scope {
        principal: &principal,
        operation: "createPlaylist",
        target: "-",
        key: &key,
    };
    let stored = scope.load(&mut t.ctx.tx).await?;
    if access::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        let mut stored = stored.ok_or_else(unavailable)?;
        if let Some(value) = stored.body.as_mut() {
            if value["profile_id"] != body.value.profile_id {
                return Err(unavailable());
            }
            let replay: Vec<EntryBody> =
                serde_json::from_value(value["entries"].clone()).map_err(|_| unavailable())?;
            let replay = entries_in(replay)?;
            let hidden = t.hidden(&replay).await?;
            value["entries"] = json!(entries_out(&replay, &hidden));
        }
        t.ctx.tx.commit().await?;
        return Ok(replayed(stored));
    }
    t.requested(&entries).await?;
    let row = service::save_playlist_in(
        &mut t.ctx.tx,
        &body.value.profile_id,
        None,
        &body.value.name,
        entries,
    )
    .await?;
    let revision = row.revision;
    let response = t.body(row).await?;
    acknowledge(t.ctx.tx, &scope, &body.digest, &response, revision).await
}
pub(super) async fn replace(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
    body: Body<PlaylistInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    ids("profile_id", [&body.value.profile_id])?;
    let requested = entries_in(body.value.entries)?;
    let mut t = Timelines::open(&app, &caller).await?;
    let current = load(&mut t.ctx.tx, &id).await?;
    owned(&t.ctx.principal, &current.profile_id)?;
    org::same_owner(&current.profile_id, &body.value.profile_id).map_err(OrgError::from)?;
    org::advance(current.revision, expected).map_err(OrgError::from)?;
    t.requested(&requested).await?;
    let hidden = t.hidden(&current.entries).await?;
    let entries =
        org::keep_hidden_entries(requested, &current.entries, &hidden).map_err(OrgError::from)?;
    let row = service::save_playlist_in(
        &mut t.ctx.tx,
        &current.profile_id,
        Some((&id, expected)),
        &body.value.name,
        entries,
    )
    .await?;
    let revision = row.revision;
    let response = t.body(row).await?;
    t.ctx.tx.commit().await?;
    Ok(tagged(StatusCode::OK, &response, revision))
}
pub(super) async fn delete(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut t = Timelines::open(&app, &caller).await?;
    let current = load(&mut t.ctx.tx, &id).await?;
    owned(&t.ctx.principal, &current.profile_id)?;
    service::delete_owned_in(&mut t.ctx.tx, Owned::Playlist, &id, expected).await?;
    t.ctx.tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
