use crate::{
    App,
    api::{ApiError, json},
    new_id, now,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct ViewingState {
    pub profile_id: String,
    pub item_id: String,
    pub position_seconds: f64,
    pub automatic_watched: bool,
    pub manual_watched: Option<bool>,
    pub watched: bool,
    pub revision: i64,
    pub session_id: Option<String>,
}
impl ViewingState {
    pub(crate) fn state(&self) -> playscale_core::viewing::State {
        playscale_core::viewing::State {
            revision: self.revision,
            session_id: self.session_id.clone(),
            position: self.position_seconds,
            automatic_watched: self.automatic_watched,
            manual_watched: self.manual_watched,
        }
    }
}
async fn profile(app: &App, id: &str) -> Result<(), ApiError> {
    let _: String = sqlx::query_scalar("SELECT id FROM profiles WHERE id=?")
        .bind(id)
        .fetch_one(&app.db)
        .await?;
    Ok(())
}
pub(crate) async fn load(app: &App, profile: &str, item: &str) -> Result<ViewingState, ApiError> {
    Ok(sqlx::query_as("SELECT p.id AS profile_id,i.id AS item_id,coalesce(g.position_seconds,0.0) AS position_seconds,coalesce(v.automatic_watched,0) AS automatic_watched,v.manual_watched,coalesce(v.manual_watched,v.automatic_watched,0) AS watched,coalesce(v.revision,0) AS revision,v.session_id FROM profiles p CROSS JOIN items i LEFT JOIN viewing_state v ON v.profile_id=p.id AND v.item_id=i.id LEFT JOIN progress g ON g.profile_id=p.id AND g.item_id=i.id WHERE p.id=? AND i.id=?")
        .bind(profile).bind(item).fetch_one(&app.db).await?)
}
fn conflict(code: &str) -> ApiError {
    ApiError::conflict(
        code,
        "Read the current viewing state or session before retrying",
    )
}
#[utoipa::path(operation_id="get_viewing_state",get,path="/api/v1/profiles/{profile}/viewing/{item}",params(("profile"=String,Path),("item"=String,Path)),responses((status=200,description="Effective watched state, position and revision; defaults for unviewed items",body=ViewingState),(status=404,description="Unknown profile or item",body=crate::api::ErrorBody)))]
pub async fn get(
    State(app): State<App>,
    Path((p, i)): Path<(String, String)>,
) -> Result<Json<ViewingState>, ApiError> {
    Ok(Json(load(&app, &p, &i).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Watched {
    pub expected_revision: i64,
    pub watched: Option<bool>,
}
#[utoipa::path(operation_id="set_watched_override",put,path="/api/v1/profiles/{profile}/viewing/{item}",params(("profile"=String,Path),("item"=String,Path)),request_body=Watched,responses((status=200,description="Set manual watched override; null restores automatic state and invalidates the current session",body=ViewingState),(status=400,description="Invalid revision",body=crate::api::ErrorBody),(status=404,description="Unknown profile or item",body=crate::api::ErrorBody),(status=409,description="Revision conflict",body=crate::api::ErrorBody)))]
pub async fn watched(
    State(app): State<App>,
    Path((p, i)): Path<(String, String)>,
    body: Result<Json<Watched>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ViewingState>, ApiError> {
    let body = json(body)?;
    Ok(Json(
        set_watched(&app, &p, &i, body.expected_revision, body.watched, false).await?,
    ))
}
/// Set or clear the manual watched override, optionally clearing the resume
/// position, and invalidate the current session (core `override_change`).
pub(crate) async fn set_watched(
    app: &App,
    p: &str,
    i: &str,
    expected_revision: i64,
    watched: Option<bool>,
    reset_position: bool,
) -> Result<ViewingState, ApiError> {
    if expected_revision < 0 || expected_revision == i64::MAX {
        return Err(ApiError::bad("Invalid revision"));
    }
    let _guard = app.jobs.lock().await;
    let current = load(app, p, i).await?;
    let next = current
        .state()
        .override_change(expected_revision, watched)
        .map_err(|_| conflict("viewing_revision_conflict"))?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    if let Some((session, status)) = &next.close_session {
        sqlx::query("UPDATE playback_sessions SET status=?,updated_at=? WHERE id=?")
            .bind(status)
            .bind(now())
            .bind(session)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT INTO viewing_state(profile_id,item_id,manual_watched,revision) VALUES (?,?,?,?) ON CONFLICT(profile_id,item_id) DO UPDATE SET manual_watched=excluded.manual_watched,revision=excluded.revision")
        .bind(p).bind(i).bind(next.view.manual_watched).bind(next.view.revision).execute(&mut *tx).await?;
    if reset_position {
        sqlx::query(
            "UPDATE progress SET position_seconds=0,updated_at=? WHERE profile_id=? AND item_id=?",
        )
        .bind(now())
        .bind(p)
        .bind(i)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    load(app, p, i).await
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Session {
    pub id: String,
    pub profile_id: String,
    pub item_id: String,
    pub file_id: String,
    pub file_revision: String,
    pub duration_seconds: Option<f64>,
    pub sequence: i64,
    pub position_seconds: f64,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}
async fn session(app: &App, p: &str, id: &str) -> Result<Session, ApiError> {
    Ok(
        sqlx::query_as("SELECT * FROM playback_sessions WHERE id=? AND profile_id=?")
            .bind(id)
            .bind(p)
            .fetch_one(&app.db)
            .await?,
    )
}
async fn file_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: &str,
) -> Result<playscale_core::viewing::FileState, ApiError> {
    let (item, revision, available, generated, duration): (String, String, bool, bool, Option<f64>) = sqlx::query_as("SELECT item_id,revision,available,generated,duration_seconds FROM catalog_files WHERE id=?")
        .bind(id).fetch_one(&mut **tx).await?;
    let rows: Vec<(String,String,String,String)> = sqlx::query_as("SELECT r.item_id,r.file_revision,r.source_revision,s.revision FROM renditions r JOIN media_files s ON s.id=r.source_file_id WHERE r.file_id=?")
        .bind(id).fetch_all(&mut **tx).await?;
    Ok(playscale_core::viewing::FileState {
        identity: playscale_core::viewing::FileIdentity {
            id: id.into(),
            revision,
        },
        item,
        available,
        generated,
        duration,
        renditions: rows
            .into_iter()
            .map(
                |(item, output_revision, source_revision, current_source_revision)| {
                    playscale_core::viewing::RenditionBinding {
                        item,
                        output_revision,
                        source_revision,
                        current_source_revision,
                    }
                },
            )
            .collect(),
    })
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub item_id: String,
    pub file_id: String,
    pub file_revision: String,
    pub expected_revision: i64,
}
#[utoipa::path(operation_id="start_playback_session",post,path="/api/v1/profiles/{profile}/playback-sessions",params(("profile"=String,Path)),request_body=Start,responses((status=201,description="Create authoritative session for this profile/item; supersedes older sessions",body=Session),(status=400,description="Invalid revision",body=crate::api::ErrorBody),(status=404,description="Unknown profile, item or eligible file",body=crate::api::ErrorBody),(status=409,description="Viewing or source revision conflict",body=crate::api::ErrorBody)))]
pub async fn start(
    State(app): State<App>,
    Path(p): Path<String>,
    body: Result<Json<Start>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Session>), ApiError> {
    let body = json(body)?;
    let session = start_session(&app, &p, new_id(), body).await?;
    Ok((StatusCode::CREATED, Json(session)))
}
/// Create the authoritative session `id` for this profile/item, superseding
/// any earlier one, after checking the file and expected viewing revision.
pub(crate) async fn start_session(
    app: &App,
    p: &str,
    id: String,
    body: Start,
) -> Result<Session, ApiError> {
    if body.expected_revision < 0 || body.expected_revision == i64::MAX {
        return Err(ApiError::bad("Invalid viewing revision"));
    }
    let _guard = app.jobs.lock().await;
    let current = load(app, p, &body.item_id).await?;
    playscale_core::revision::advance(current.revision, body.expected_revision)
        .map_err(|_| conflict("viewing_revision_conflict"))?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let observed = file_state(&mut tx, &body.file_id).await?;
    if !observed.eligible(&body.item_id, true) {
        return Err(ApiError::not_found());
    }
    playscale_core::viewing::check_file(
        &playscale_core::viewing::FileIdentity {
            id: body.file_id.clone(),
            revision: body.file_revision.clone(),
        },
        &observed.identity.revision,
        true,
    )
    .map_err(conflict)?;
    let revision = observed.identity.revision;
    let duration = observed.duration;
    let change = current
        .state()
        .start_change(body.expected_revision, id.clone(), duration)
        .map_err(|_| conflict("viewing_revision_conflict"))?;
    if let Some((old, status)) = &change.close_session {
        sqlx::query("UPDATE playback_sessions SET status=?,updated_at=? WHERE id=?")
            .bind(status)
            .bind(now())
            .bind(old)
            .execute(&mut *tx)
            .await?;
    }
    let next = change.view;
    let position = next.position;
    sqlx::query("INSERT INTO playback_sessions VALUES (?,?,?,?,?,?,0,?,'paused',?,?)")
        .bind(&id)
        .bind(p)
        .bind(&body.item_id)
        .bind(body.file_id)
        .bind(revision)
        .bind(duration)
        .bind(position)
        .bind(now())
        .bind(now())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO viewing_state(profile_id,item_id,revision,session_id) VALUES (?,?,?,?) ON CONFLICT(profile_id,item_id) DO UPDATE SET revision=excluded.revision,session_id=excluded.session_id")
        .bind(p).bind(&body.item_id).bind(next.revision).bind(&id).execute(&mut *tx).await?;
    tx.commit().await?;
    session(app, p, &id).await
}
#[utoipa::path(operation_id="get_playback_session",get,path="/api/v1/profiles/{profile}/playback-sessions/{id}",params(("profile"=String,Path),("id"=String,Path)),responses((status=200,description="Durable playback session",body=Session),(status=404,description="Unknown session for profile",body=crate::api::ErrorBody)))]
pub async fn get_session(
    State(app): State<App>,
    Path((p, id)): Path<(String, String)>,
) -> Result<Json<Session>, ApiError> {
    Ok(Json(session(&app, &p, &id).await?))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionFile {
    pub file_id: String,
    pub file_revision: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub sequence: i64,
    pub position_seconds: f64,
    pub status: String,
    /// Atomically change the current file without ending the authoritative session.
    #[serde(default)]
    pub file: Option<SessionFile>,
}
#[utoipa::path(operation_id="update_playback_session",put,path="/api/v1/profiles/{profile}/playback-sessions/{id}",params(("profile"=String,Path),("id"=String,Path)),request_body=Event,responses((status=200,description="Accept ordered event or exact retry; ended marks automatic completion",body=Session),(status=400,description="Invalid status, sequence or position",body=crate::api::ErrorBody),(status=404,description="Unknown session for profile",body=crate::api::ErrorBody),(status=409,description="Superseded, closed, stale event or changed source",body=crate::api::ErrorBody)))]
pub async fn event(
    State(app): State<App>,
    Path((p, id)): Path<(String, String)>,
    body: Result<Json<Event>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Session>, ApiError> {
    let body = json(body)?;
    Ok(Json(record_event(&app, &p, &id, body).await?.0))
}
/// Apply one ordered event (core `viewing::event`). Returns the session and
/// whether the event was an exact retry of the one already recorded.
pub(crate) async fn record_event(
    app: &App,
    p: &str,
    id: &str,
    body: Event,
) -> Result<(Session, bool), ApiError> {
    let _guard = app.jobs.lock().await;
    let old = session(app, p, id).await?;
    let view = load(app, p, &old.item_id).await?;
    let from = playscale_core::viewing::Session {
        sequence: old.sequence,
        position: old.position_seconds,
        status: old.status.clone(),
    };
    let to = playscale_core::viewing::Session {
        sequence: body.sequence,
        position: body.position_seconds,
        status: body.status.clone(),
    };
    let old_file = playscale_core::viewing::FileIdentity {
        id: old.file_id.clone(),
        revision: old.file_revision.clone(),
    };
    let selected = body
        .file
        .as_ref()
        .map(|f| playscale_core::viewing::FileIdentity {
            id: f.file_id.clone(),
            revision: f.file_revision.clone(),
        });
    playscale_core::viewing::check_switch(&from, &old_file, &to, selected.as_ref()).map_err(
        |e| {
            if e.starts_with("invalid_") {
                ApiError::bad(e)
            } else {
                conflict(e)
            }
        },
    )?;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let mut duration = old.duration_seconds;
    if let Some(file) = &body.file
        && body.sequence > old.sequence
    {
        let observed = file_state(&mut tx, &file.file_id).await?;
        if !observed.eligible(&old.item_id, true) {
            return Err(ApiError::not_found());
        }
        playscale_core::viewing::check_file(
            selected.as_ref().expect("selected file"),
            &observed.identity.revision,
            true,
        )
        .map_err(conflict)?;
        duration = observed.duration;
    }
    let next =
        playscale_core::viewing::event(&view.state(), id, &from, &to, duration).map_err(|e| {
            if e.starts_with("invalid_") {
                ApiError::bad(e)
            } else {
                conflict(e)
            }
        })?;
    let Some((next, next_view)) = next else {
        return Ok((old, true));
    };
    if let Some(file) = &body.file {
        sqlx::query(
            "UPDATE playback_sessions SET file_id=?,file_revision=?,duration_seconds=? WHERE id=?",
        )
        .bind(&file.file_id)
        .bind(&file.file_revision)
        .bind(duration)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    } else {
        let observed = file_state(&mut tx, &old.file_id).await?;
        playscale_core::viewing::check_file(
            &old_file,
            &observed.identity.revision,
            observed.eligible(&old.item_id, false),
        )
        .map_err(conflict)?;
    }
    sqlx::query("UPDATE playback_sessions SET sequence=?,position_seconds=?,status=?,updated_at=? WHERE id=?").bind(next.sequence).bind(next.position).bind(&next.status).bind(now()).bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO progress VALUES (?,?,?,?) ON CONFLICT(profile_id,item_id) DO UPDATE SET position_seconds=excluded.position_seconds,updated_at=excluded.updated_at")
        .bind(p).bind(&old.item_id).bind(next.position).bind(now()).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE viewing_state SET automatic_watched=?,revision=? WHERE profile_id=? AND item_id=?",
    )
    .bind(next_view.automatic_watched)
    .bind(next_view.revision)
    .bind(p)
    .bind(&old.item_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((session(app, p, id).await?, false))
}

#[derive(Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    /// off, always, or foreign_audio; hints to clients, not automatic server processing.
    pub subtitle_mode: String,
    /// auto, original (strict), or convert. Planning never starts processing.
    pub quality: String,
    /// Fixed processing recipe used by Convert or proposed by Auto.
    #[serde(default = "crate::playback::default_recipe")]
    pub conversion_recipe: String,
    /// v2 preference fields; stored documents written before them read as defaults.
    #[serde(default = "enabled")]
    pub allow_client_software_decode: bool,
    #[serde(default)]
    pub autoplay: bool,
    #[serde(default = "default_completion")]
    pub completion_percent: f64,
}
fn enabled() -> bool {
    true
}
fn default_completion() -> f64 {
    90.0
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            audio_languages: vec![],
            subtitle_languages: vec![],
            subtitle_mode: "foreign_audio".into(),
            quality: "auto".into(),
            conversion_recipe: crate::playback::default_recipe(),
            allow_client_software_decode: true,
            autoplay: false,
            completion_percent: default_completion(),
        }
    }
}
#[derive(Serialize, ToSchema)]
pub struct PreferenceState {
    pub revision: i64,
    pub preferences: Preferences,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferenceUpdate {
    pub expected_revision: i64,
    pub preferences: Preferences,
}
pub(crate) async fn prefs(app: &App, p: &str) -> Result<PreferenceState, ApiError> {
    profile(app, p).await?;
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT revision,document_json FROM playback_preferences WHERE profile_id=?",
    )
    .bind(p)
    .fetch_optional(&app.db)
    .await?;
    match row {
        Some((revision, doc)) => Ok(PreferenceState {
            revision,
            preferences: serde_json::from_str(&doc).map_err(ApiError::internal)?,
        }),
        None => Ok(PreferenceState {
            revision: 0,
            preferences: Preferences::default(),
        }),
    }
}
#[utoipa::path(operation_id="get_playback_preferences",get,path="/api/v1/profiles/{profile}/playback-preferences",params(("profile"=String,Path)),responses((status=200,description="Playback preference defaults or saved values",body=PreferenceState),(status=404,description="Unknown profile",body=crate::api::ErrorBody)))]
pub async fn get_preferences(
    State(app): State<App>,
    Path(p): Path<String>,
) -> Result<Json<PreferenceState>, ApiError> {
    Ok(Json(prefs(&app, &p).await?))
}
#[utoipa::path(operation_id="update_playback_preferences",put,path="/api/v1/profiles/{profile}/playback-preferences",params(("profile"=String,Path)),request_body=PreferenceUpdate,responses((status=200,description="Preferences replaced",body=PreferenceState),(status=400,description="Invalid preferences",body=crate::api::ErrorBody),(status=404,description="Unknown profile",body=crate::api::ErrorBody),(status=409,description="Revision conflict",body=crate::api::ErrorBody)))]
pub async fn put_preferences(
    State(app): State<App>,
    Path(p): Path<String>,
    body: Result<Json<PreferenceUpdate>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<PreferenceState>, ApiError> {
    let mut body = json(body)?;
    if body.expected_revision < 0
        || body.expected_revision == i64::MAX
        || !matches!(
            body.preferences.subtitle_mode.as_str(),
            "off" | "always" | "foreign_audio"
        )
        || !matches!(
            body.preferences.quality.as_str(),
            "auto" | "original" | "convert"
        )
        || !crate::playback::valid_recipe(&body.preferences.conversion_recipe)
    {
        return Err(ApiError::bad("Invalid preference revision or mode"));
    }
    for languages in [
        &mut body.preferences.audio_languages,
        &mut body.preferences.subtitle_languages,
    ] {
        if languages.len() > 10
            || languages
                .iter()
                .any(|v| !playscale_core::viewing::valid_language(v))
        {
            return Err(ApiError::bad("Expected at most ten bounded language tags"));
        }
        for value in languages.iter_mut() {
            *value = value.to_ascii_lowercase();
        }
        let unique: std::collections::BTreeSet<_> = languages.iter().collect();
        if unique.len() != languages.len() {
            return Err(ApiError::bad("Language tags must be unique"));
        }
    }
    let _guard = app.jobs.lock().await;
    let current = prefs(&app, &p).await?;
    if playscale_core::revision::advance(current.revision, body.expected_revision).is_err() {
        return Err(conflict("preferences_revision_conflict"));
    }
    sqlx::query("INSERT INTO playback_preferences VALUES (?,?,?) ON CONFLICT(profile_id) DO UPDATE SET revision=excluded.revision,document_json=excluded.document_json")
        .bind(&p).bind(current.revision+1).bind(serde_json::to_string(&body.preferences).map_err(ApiError::internal)?).execute(&app.db).await?;
    Ok(Json(prefs(&app, &p).await?))
}

const AVAILABLE: &str = "(EXISTS(SELECT 1 FROM catalog_files f WHERE f.item_id=i.id AND f.available=1 AND f.generated=0) OR EXISTS(SELECT 1 FROM renditions r JOIN media_files f ON f.id=r.file_id JOIN media_files s ON s.id=r.source_file_id WHERE r.item_id=i.id AND f.available=1 AND f.revision=r.file_revision AND s.revision=r.source_revision))";
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct ContinueItem {
    pub item_id: String,
    pub title: String,
    pub position_seconds: f64,
    pub updated_at: i64,
    pub available: bool,
}
#[derive(Deserialize, IntoParams)]
pub struct PageQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
#[derive(Serialize, ToSchema)]
pub struct ContinuePage {
    pub items: Vec<ContinueItem>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}
#[utoipa::path(operation_id="continue_watching",get,path="/api/v1/profiles/{profile}/continue-watching",params(("profile"=String,Path),PageQuery),responses((status=200,description="Unwatched titles with positive progress, newest first; includes availability",body=ContinuePage),(status=400,description="Invalid page bounds",body=crate::api::ErrorBody),(status=404,description="Unknown profile",body=crate::api::ErrorBody)))]
pub async fn continue_watching(
    State(app): State<App>,
    Path(p): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Json<ContinuePage>, ApiError> {
    profile(&app, &p).await?;
    let limit = q.limit.unwrap_or(50);
    let offset = q.offset.unwrap_or(0);
    if !(1..=200).contains(&limit) || offset < 0 {
        return Err(ApiError::bad("Invalid page bounds"));
    }
    let from = "FROM progress g JOIN items i ON i.id=g.item_id LEFT JOIN viewing_state v ON v.profile_id=g.profile_id AND v.item_id=g.item_id WHERE g.profile_id=? AND g.position_seconds>0 AND coalesce(v.manual_watched,v.automatic_watched,0)=0";
    let mut tx = app.db.begin().await?;
    let total = sqlx::query_scalar(&format!("SELECT count(*) {from}"))
        .bind(&p)
        .fetch_one(&mut *tx)
        .await?;
    let items=sqlx::query_as(&format!("SELECT i.id AS item_id,i.title,g.position_seconds,g.updated_at,{AVAILABLE} AS available {from} ORDER BY g.updated_at DESC,i.id LIMIT ? OFFSET ?")).bind(&p).bind(limit).bind(offset).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(ContinuePage {
        items,
        total,
        limit,
        offset,
    }))
}
#[derive(Deserialize, IntoParams)]
pub struct NextQuery {
    pub include_specials: Option<bool>,
    pub skip_watched: Option<bool>,
}
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Episode {
    pub item_id: String,
    pub title: String,
    pub season_number: i64,
    pub episode_number: i64,
    pub available: bool,
}
#[derive(Serialize, ToSchema)]
pub struct NextEpisode {
    pub next: Option<Episode>,
}
#[utoipa::path(operation_id="next_episode",get,path="/api/v1/profiles/{profile}/next-episode/{item}",params(("profile"=String,Path),("item"=String,Path),NextQuery),responses((status=200,description="Next episode by season/episode number; defaults skip watched and season zero; null at end",body=NextEpisode),(status=400,description="Item is not an episode",body=crate::api::ErrorBody),(status=404,description="Unknown profile or item",body=crate::api::ErrorBody)))]
pub async fn next_episode(
    State(app): State<App>,
    Path((p, id)): Path<(String, String)>,
    Query(q): Query<NextQuery>,
) -> Result<Json<NextEpisode>, ApiError> {
    profile(&app, &p).await?;
    let _: String = sqlx::query_scalar("SELECT id FROM items WHERE id=?")
        .bind(&id)
        .fetch_one(&app.db)
        .await?;
    let mut tx = app.db.begin().await?;
    let current:Option<(String,i64,i64)>=sqlx::query_as("SELECT season.parent_id,season.number,episode.number FROM item_structure episode JOIN item_structure season ON season.item_id=episode.parent_id WHERE episode.item_id=? AND episode.media_type='episode' AND season.media_type='season'").bind(&id).fetch_optional(&mut *tx).await?;
    let Some((series, season, episode)) = current else {
        return Err(ApiError::bad("Next-episode lookup requires an episode"));
    };
    let next=sqlx::query_as(&format!("SELECT i.id AS item_id,i.title,s.number AS season_number,e.number AS episode_number,{AVAILABLE} AS available FROM item_structure e JOIN items i ON i.id=e.item_id JOIN item_structure s ON s.item_id=e.parent_id LEFT JOIN viewing_state v ON v.item_id=i.id AND v.profile_id=? WHERE e.media_type='episode' AND s.media_type='season' AND s.parent_id=? AND (s.number>? OR (s.number=? AND e.number>?)) AND (? OR s.number>0) AND (?=0 OR coalesce(v.manual_watched,v.automatic_watched,0)=0) ORDER BY s.number,e.number,i.id LIMIT 1"))
        .bind(&p).bind(series).bind(season).bind(season).bind(episode).bind(q.include_specials.unwrap_or(false)).bind(q.skip_watched.unwrap_or(true)).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(NextEpisode { next }))
}
