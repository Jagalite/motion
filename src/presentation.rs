//! Request-scoped Topcoat service bridge. Reads traverse the production v2
//! adapter in-process, retaining the credential and genuine ingress marker.
//! Each read therefore rechecks current credentials and catalog policy. No
//! caller registry, synthesized administrator, loopback network or mock data.
use crate::{App, api::PresentationIdentity, v2};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use motion_ui::facade::*;
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

pub fn router(app: App) -> Router {
    Router::new().fallback(render).with_state(app)
}

struct Queries {
    app: App,
    headers: HeaderMap,
    ingress: Option<v2::auth::TrustedIngress>,
}

fn error(status: StatusCode) -> UiError {
    match status {
        StatusCode::NOT_FOUND => UiError::NotFound,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => UiError::Denied,
        _ => UiError::Unavailable,
    }
}
fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().into()
}
fn availability(v: &Value) -> Availability {
    match v["availability"].as_str() {
        Some("available") => Availability::Available,
        Some("unavailable") => Availability::Unavailable,
        Some("degraded") => Availability::Degraded,
        _ => Availability::Unknown,
    }
}
fn library(v: &Value) -> LibraryCard {
    LibraryCard {
        id: text(v, "id"),
        name: text(v, "name"),
        kind: text(v, "kind"),
        availability: availability(v),
    }
}
fn item(v: &Value) -> ItemCard {
    ItemCard {
        id: text(v, "id"),
        title: text(v, "title"),
        kind: text(v, "kind"),
        availability: availability(v),
        needs_review: matches!(v["match_state"].as_str(), Some("review" | "unmatched")),
    }
}
fn segment(s: &str) -> String {
    let mut url = url::Url::parse("http://local/").unwrap();
    url.path_segments_mut().unwrap().push(s);
    url.path().trim_start_matches('/').into()
}
impl Queries {
    async fn read(&self, path: &str) -> UiResult<Value> {
        Ok(self.read_tagged(path).await?.0)
    }
    /// A read and its strong validator, for views that submit If-Match.
    async fn read_tagged(&self, path: &str) -> UiResult<(Value, Option<String>)> {
        let mut request = Request::builder()
            .uri(path)
            .body(Body::empty())
            .map_err(|_| UiError::Unavailable)?;
        *request.headers_mut() = self.headers.clone();
        if let Some(ingress) = self.ingress {
            request.extensions_mut().insert(ingress);
        }
        let response = v2::router()
            .with_state(self.app.clone())
            .oneshot(request)
            .await
            .map_err(|_| UiError::Unavailable)?;
        if !response.status().is_success() {
            return Err(error(response.status()));
        }
        let etag = response
            .headers()
            .get(axum::http::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .map_err(|_| UiError::Unavailable)?;
        Ok((
            serde_json::from_slice(&bytes).map_err(|_| UiError::Unavailable)?,
            etag,
        ))
    }
    /// The profile's viewing state for a timeline; None when unavailable
    /// (a missing service must not read as an unstarted title).
    async fn viewing(&self, who: &UiPrincipal, timeline: &str) -> Option<Value> {
        self.read(&format!(
            "/profiles/{}/timelines/{}/viewing",
            segment(&who.profile_id),
            segment(timeline)
        ))
        .await
        .ok()
    }
    /// An item aggregate must not present a truncated edition/version set as
    /// complete. Bound the work and report unavailable when paging is needed.
    async fn complete_list(&self, path: &str) -> UiResult<Vec<Value>> {
        let page = self.read(path).await?;
        if !page["next_cursor"].is_null() {
            return Err(UiError::Unavailable);
        }
        page["items"]
            .as_array()
            .cloned()
            .ok_or(UiError::Unavailable)
    }

    /// Audio streams of the timeline's first available original version that
    /// the principal may read. The version and its file binding come from the
    /// scope-filtered v2 versions read; only that bound file's stored stream
    /// list is then read (the v2 contract has no track read yet). IDs match
    /// the planner's `a{n}` ordinals for that version.
    async fn audio_tracks(&self, timeline: &str) -> (Option<String>, Vec<TrackOption>) {
        let Ok(versions) = self
            .complete_list(&format!(
                "/catalog/timelines/{}/versions?limit=200",
                segment(timeline)
            ))
            .await
        else {
            return (None, vec![]);
        };
        let Some((version, file, revision)) = versions.iter().find_map(|v| {
            let first = v["files"].as_array()?.iter().find(|b| b["part"] == 1)?;
            (v["origin"] == "original" && v["availability"] == "available").then(|| {
                (
                    text(v, "id"),
                    text(&first["file"], "file_id"),
                    text(&first["file"], "file_revision"),
                )
            })
        }) else {
            return (None, vec![]);
        };
        let tracks: Option<String> = sqlx::query_scalar(
            "SELECT tracks_json FROM media_files WHERE id=? AND revision=? AND available=1 AND generated=0",
        )
        .bind(&file)
        .bind(&revision)
        .fetch_optional(&self.app.db)
        .await
        .ok()
        .flatten();
        let streams: Vec<crate::db::Track> = tracks
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let options = streams
            .iter()
            .filter(|t| t.kind == "audio")
            .enumerate()
            .map(|(n, t)| TrackOption {
                id: format!("a{n}"),
                label: format!(
                    "Track {} ({}{})",
                    n + 1,
                    t.language
                        .as_deref()
                        .map(|l| format!("{l}, "))
                        .unwrap_or_default(),
                    t.codec
                ),
            })
            .collect();
        (Some(version), options)
    }

    async fn list(&self, path: &str) -> UiResult<Vec<Value>> {
        // Presentation is deliberately bounded; it never drains an unbounded
        // catalog. Dedicated library/search pages provide narrower reads.
        let page = self.read(path).await?;
        page["items"]
            .as_array()
            .cloned()
            .ok_or(UiError::Unavailable)
    }
}

async fn render(State(app): State<App>, mut request: Request) -> Response {
    let Ok(_permit) = app.access.renders.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "1")]).into_response();
    };
    let identity = request.extensions().get::<PresentationIdentity>().cloned();
    let queries = Arc::new(Queries {
        app: app.clone(),
        headers: request.headers().clone(),
        ingress: request
            .extensions()
            .get::<v2::auth::TrustedIngress>()
            .copied(),
    });
    let render = async {
        if let Some(PresentationIdentity(Some(caller))) = identity {
            let profiles = match queries.list("/profiles?limit=200").await {
                Ok(rows) => rows
                    .into_iter()
                    .map(|v| ProfileOption {
                        id: text(&v, "id"),
                        name: text(&v, "name"),
                    })
                    .collect::<Vec<_>>(),
                Err(e) => {
                    return match e {
                        UiError::Denied => StatusCode::UNAUTHORIZED,
                        _ => StatusCode::SERVICE_UNAVAILABLE,
                    }
                    .into_response();
                }
            };
            // This cookie is only a selection hint. The selected profile must
            // occur in the freshly authorized profile list.
            let selected = request
                .headers()
                .get("cookie")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| {
                    s.split(';')
                        .find_map(|c| c.trim().strip_prefix("motion_profile="))
                });
            let requested_profile = request.uri().query().and_then(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .find(|(key, _)| key == "profile_id")
                    .map(|(_, value)| value.into_owned())
            });
            if requested_profile
                .as_ref()
                .is_some_and(|id| !profiles.iter().any(|p| &p.id == id))
            {
                return StatusCode::FORBIDDEN.into_response();
            }
            let profile = requested_profile
                .as_deref()
                .or(selected)
                .and_then(|id| profiles.iter().find(|p| p.id == id))
                .or_else(|| profiles.first());
            let Some(profile) = profile else {
                return StatusCode::FORBIDDEN.into_response();
            };
            let permissions = playscale_core::access::Permission::ALL
                .into_iter()
                .filter(|p| caller.principal.allows(*p))
                .map(|p| {
                    serde_json::to_value(p)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect();
            request.extensions_mut().insert(UiPrincipal {
                principal_id: caller.principal.id,
                profile_id: profile.id.clone(),
                profile_name: profile.name.clone(),
                profiles,
                permissions,
                server_epoch: app.access.server_epoch.clone(),
                csrf_token: caller.csrf.unwrap_or_default(),
            });
        }
        motion_ui::mount(Router::new(), queries, None)
            .oneshot(request)
            .await
            .unwrap()
    };
    match tokio::time::timeout(Duration::from_secs(15), render).await {
        Ok(response) => response,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

impl UiQueryFacade for Queries {
    fn is_mock(&self) -> bool {
        false
    }
    fn home<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<HomeView>> {
        Box::pin(async move {
            // Continue watching is optional on the home page: its absence is
            // reported as unavailable, never as an empty history.
            let entries = self
                .list(&format!(
                    "/profiles/{}/continue-watching?limit=20",
                    segment(&who.profile_id)
                ))
                .await;
            let continue_available = entries.is_ok();
            let continue_watching = entries
                .unwrap_or_default()
                .iter()
                .map(|v| ContinueCard {
                    item: item(&v["item"]),
                    timeline_id: text(&v["timeline"], "id"),
                    position_ms: v["viewing"]["position_ms"].as_u64().unwrap_or(0),
                    duration_ms: v["timeline"]["duration_ms"].as_u64(),
                })
                .collect();
            Ok(HomeView {
                libraries: self
                    .list("/libraries?limit=200")
                    .await?
                    .iter()
                    .map(library)
                    .collect(),
                continue_watching,
                continue_available,
            })
        })
    }
    fn library<'a>(
        &'a self,
        _: &'a UiPrincipal,
        id: &'a str,
    ) -> BoxFuture<'a, UiResult<LibraryView>> {
        Box::pin(async move {
            let value = self.read(&format!("/libraries/{}", segment(id))).await?;
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("library_id", id)
                .append_pair("limit", "200")
                .finish();
            Ok(LibraryView {
                library: library(&value),
                items: self
                    .list(&format!("/catalog/items?{query}"))
                    .await?
                    .iter()
                    .map(item)
                    .collect(),
            })
        })
    }
    fn search<'a>(
        &'a self,
        _: &'a UiPrincipal,
        query: &'a str,
    ) -> BoxFuture<'a, UiResult<SearchView>> {
        Box::pin(async move {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("q", query)
                .append_pair("limit", "200")
                .finish();
            Ok(SearchView {
                query: query.into(),
                items: self
                    .list(&format!("/catalog/search?{q}"))
                    .await?
                    .iter()
                    .map(item)
                    .collect(),
            })
        })
    }
    fn item<'a>(&'a self, who: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<ItemView>> {
        Box::pin(async move {
            let path = format!("/catalog/items/{}", segment(id));
            let value = self.read(&path).await?;
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("parent_id", id)
                .append_pair("limit", "200")
                .finish();
            let children = self
                .complete_list(&format!("/catalog/items?{query}"))
                .await?
                .iter()
                .map(item)
                .collect();
            let editions = self
                .complete_list(&format!("{path}/editions?limit=200"))
                .await?;
            let rows = self
                .complete_list(&format!("{path}/timelines?limit=20"))
                .await?;
            let mut timelines = Vec::new();
            for row in rows {
                let timeline = text(&row, "id");
                let edition = editions
                    .iter()
                    .find(|e| e["id"] == row["edition_id"])
                    .ok_or(UiError::Unavailable)?;
                let versions = self
                    .complete_list(&format!(
                        "/catalog/timelines/{}/versions?limit=200",
                        segment(&timeline)
                    ))
                    .await?
                    .iter()
                    .map(|v| {
                        let label = text(v, "label");
                        VersionView {
                            id: text(v, "id"),
                            label: if label.trim().is_empty() {
                                "Unnamed version".into()
                            } else {
                                label
                            },
                            availability: availability(v),
                            summary: text(v, "origin"),
                        }
                    })
                    .collect();
                let viewing = self.viewing(who, &timeline).await.map(|v| TimelineViewing {
                    position_ms: v["position_ms"].as_u64().unwrap_or(0),
                    watched: v["watched"].as_bool().unwrap_or(false),
                });
                timelines.push(TimelineView {
                    id: timeline,
                    edition: text(edition, "label"),
                    duration_ms: row["duration_ms"].as_u64(),
                    viewing,
                    versions,
                });
            }
            // The planner decides per request; this only offers the action
            // when some version could be played at all.
            let playback_available = timelines.iter().any(|t| {
                t.versions
                    .iter()
                    .any(|v| v.availability == Availability::Available)
            });
            Ok(ItemView {
                item: item(&value),
                children,
                timelines,
                playback_available,
            })
        })
    }
    fn player<'a>(
        &'a self,
        who: &'a UiPrincipal,
        timeline_id: &'a str,
    ) -> BoxFuture<'a, UiResult<PlayerView>> {
        Box::pin(async move {
            if !who.can("playback:request") {
                return Err(UiError::Denied);
            }
            let timeline = self
                .read(&format!("/catalog/timelines/{}", segment(timeline_id)))
                .await?;
            let item_id = text(&timeline, "item_id");
            let work = self
                .read(&format!("/catalog/items/{}", segment(&item_id)))
                .await?;
            let duration_ms = timeline["duration_ms"].as_u64();
            let viewing = self
                .viewing(who, timeline_id)
                .await
                .ok_or(UiError::Unavailable)?;
            let position = viewing["position_ms"].as_u64().unwrap_or(0);
            let (audio_version, audio_tracks) = self.audio_tracks(timeline_id).await;
            // A watched title, or one stopped in its final second, starts over.
            let finished = viewing["watched"].as_bool().unwrap_or(false)
                || duration_ms.is_some_and(|d| position.saturating_add(1000) >= d);
            Ok(PlayerView {
                item_id,
                title: text(&work, "title"),
                timeline_id: timeline_id.into(),
                duration_ms,
                resume_ms: if finished { 0 } else { position },
                viewing_revision: text(&viewing, "revision"),
                viewing_manual_epoch: text(&viewing, "manual_epoch"),
                audio_tracks,
                audio_version,
            })
        })
    }
    fn profiles<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<ProfilesView>> {
        Box::pin(async move {
            let (prefs, etag) = self
                .read_tagged(&format!(
                    "/profiles/{}/preferences",
                    segment(&who.profile_id)
                ))
                .await?;
            let languages = |key: &str| -> Vec<String> {
                prefs[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Ok(ProfilesView {
                profiles: who.profiles.clone(),
                current: who.profile_id.clone(),
                preferences: PreferencesView {
                    audio_languages: languages("audio_languages"),
                    subtitle_languages: languages("subtitle_languages"),
                    subtitle_mode: text(&prefs, "subtitle_mode"),
                    quality_mode: text(&prefs, "quality_mode"),
                    allow_client_software_decode: prefs["allow_client_software_decode"]
                        .as_bool()
                        .unwrap_or(true),
                    autoplay: prefs["autoplay"].as_bool().unwrap_or(false),
                    completion_percent: prefs["completion_percent"].as_f64().unwrap_or(90.0),
                    etag: etag.ok_or(UiError::Unavailable)?,
                },
            })
        })
    }
    fn sources<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<SourcesView>> {
        Box::pin(async move {
            if !who.can("sources:manage") {
                return Err(UiError::Denied);
            }
            Ok(SourcesView {
                sources: self
                    .complete_list("/sources?limit=200")
                    .await?
                    .iter()
                    .map(|v| SourceRow {
                        id: text(v, "id"),
                        name: text(v, "name"),
                        root_path: text(v, "root_path"),
                        availability: availability(v),
                    })
                    .collect(),
                libraries: self
                    .complete_list("/libraries?limit=200")
                    .await?
                    .iter()
                    .map(library)
                    .collect(),
                scans_available: false,
                scans: vec![],
            })
        })
    }
    fn matches<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<MatchRow>>> {
        Box::pin(async move {
            self.list("/catalog/matches?limit=200")
                .await?
                .iter()
                .map(|v| {
                    let revision = text(v, "revision")
                        .parse::<u64>()
                        .map_err(|_| UiError::Unavailable)?;
                    let candidates = v["candidates"].as_array().ok_or(UiError::Unavailable)?;
                    Ok(MatchRow {
                        id: text(v, "id"),
                        subject: format!("File {}", text(&v["file"], "file_id")),
                        status: text(v, "status"),
                        etag: v2::etag(revision).to_str().unwrap().to_owned(),
                        candidates: candidates
                            .iter()
                            .map(|c| {
                                Ok(MatchCandidateView {
                                    id: text(c, "id"),
                                    title: text(c, "title"),
                                    confidence_percent: c["confidence"]
                                        .as_f64()
                                        .map(|v| (v * 100.).clamp(0., 100.) as u8),
                                    reasons: serde_json::from_value(c["reason_codes"].clone())
                                        .map_err(|_| UiError::Unavailable)?,
                                })
                            })
                            .collect::<UiResult<_>>()?,
                    })
                })
                .collect()
        })
    }
    fn jobs<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<JobRow>>> {
        Box::pin(async move {
            Ok(self
                .list("/jobs?limit=200")
                .await?
                .iter()
                .map(|v| JobRow {
                    id: text(v, "id"),
                    kind: text(v, "kind"),
                    phase: text(v, "phase"),
                    progress_percent: v["progress"]
                        .as_f64()
                        .map(|p| (p * 100.).clamp(0., 100.) as u8),
                    error_code: v["error_code"].as_str().map(str::to_owned),
                })
                .collect())
        })
    }
    fn diagnostics<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<DiagnosticsView>> {
        Box::pin(async move {
            let d = self.read("/admin/diagnostics").await?;
            let h = self.read("/system/health").await?;
            let c = self.read("/system/capabilities").await?;
            let number = |key: &str| {
                text(&d, key)
                    .parse::<u64>()
                    .map_err(|_| UiError::Unavailable)
            };
            Ok(DiagnosticsView {
                server_id: text(&h, "server_id"),
                server_version: text(&c, "server_version"),
                api_version: text(&c, "api_version"),
                schema_version: text(&c, "schema_version"),
                health: text(&h, "status"),
                uptime_seconds: number("uptime_seconds")?,
                active_deliveries: number("active_deliveries")?,
                queued_jobs: number("queued_jobs")?,
                running_jobs: number("running_jobs")?,
                worker_errors: serde_json::from_value(d["worker_errors"].clone())
                    .map_err(|_| UiError::Unavailable)?,
            })
        })
    }
}
