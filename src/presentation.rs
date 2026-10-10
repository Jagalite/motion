//! Production SSR adapter over the authenticated public read API, dispatched
//! in-process (no loopback socket). Each request owns its facade and headers;
//! there is no cross-principal cache or second authorization implementation.
use crate::{App, api::PresentationIdentity};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use motion_ui::facade::*;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

#[derive(Clone)]
struct Context {
    api: Router,
    app: App,
}
struct ApiFacade {
    api: Router,
    headers: HeaderMap,
    reads: AtomicUsize,
    trusted_ingress: bool,
    deadline: tokio::time::Instant,
}
impl ApiFacade {
    fn new(api: Router, headers: HeaderMap) -> Self {
        Self {
            api,
            headers,
            reads: AtomicUsize::new(0),
            trusted_ingress: false,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(5),
        }
    }
    async fn get(&self, path: &str) -> UiResult<(Value, String)> {
        if self.reads.fetch_add(1, Ordering::Relaxed) >= 32 {
            return Err(UiError::Unavailable);
        }
        let mut request = Request::builder()
            .uri(path)
            .body(Body::empty())
            .map_err(|_| UiError::Unavailable)?;
        *request.headers_mut() = self.headers.clone();
        if self.trusted_ingress {
            request
                .extensions_mut()
                .insert(crate::v2::auth::TrustedIngress);
        }
        let response = tokio::time::timeout_at(self.deadline, self.api.clone().oneshot(request))
            .await
            .map_err(|_| UiError::Unavailable)?
            .map_err(|_| UiError::Unavailable)?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(UiError::Denied),
            StatusCode::NOT_FOUND => return Err(UiError::NotFound),
            _ => return Err(UiError::Unavailable),
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes =
            tokio::time::timeout_at(self.deadline, to_bytes(response.into_body(), 512 * 1024))
                .await
                .map_err(|_| UiError::Unavailable)?
                .map_err(|_| UiError::Unavailable)?;
        Ok((
            serde_json::from_slice(&bytes).map_err(|_| UiError::Unavailable)?,
            etag,
        ))
    }
    async fn list(&self, path: &str) -> UiResult<Vec<Value>> {
        let (value, _) = self.get(path).await.map_err(|e| {
            if e == UiError::NotFound {
                UiError::Unavailable
            } else {
                e
            }
        })?;
        value["items"]
            .as_array()
            .cloned()
            .ok_or(UiError::Unavailable)
    }
}
fn string(value: &Value, key: &str) -> UiResult<String> {
    value[key]
        .as_str()
        .map(str::to_owned)
        .ok_or(UiError::Unavailable)
}
fn segment(value: &str) -> UiResult<&str> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(UiError::NotFound);
    }
    Ok(value)
}
fn decimal(value: &Value, key: &str) -> UiResult<u64> {
    string(value, key)?
        .parse()
        .map_err(|_| UiError::Unavailable)
}
fn strings(value: &Value, key: &str) -> UiResult<Vec<String>> {
    value[key]
        .as_array()
        .ok_or(UiError::Unavailable)?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or(UiError::Unavailable))
        .collect()
}
fn availability(value: &Value) -> UiResult<Availability> {
    match value["availability"].as_str() {
        Some("available") => Ok(Availability::Available),
        Some("unavailable") => Ok(Availability::Unavailable),
        Some("degraded") => Ok(Availability::Degraded),
        Some("unknown") => Ok(Availability::Unknown),
        _ => Err(UiError::Unavailable),
    }
}
fn library(value: &Value) -> UiResult<LibraryCard> {
    Ok(LibraryCard {
        id: string(value, "id")?,
        name: string(value, "name")?,
        kind: string(value, "kind")?,
        availability: availability(value)?,
    })
}
fn card(value: &Value) -> UiResult<ItemCard> {
    Ok(ItemCard {
        id: string(value, "id")?,
        title: string(value, "title")?,
        kind: string(value, "kind")?,
        availability: availability(value)?,
        needs_review: matches!(
            value["match_state"].as_str(),
            Some("unmatched" | "ambiguous")
        ),
    })
}

impl UiQueryFacade for ApiFacade {
    fn is_mock(&self) -> bool {
        false
    }
    fn home<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<HomeView>> {
        Box::pin(async move {
            let libraries = self
                .list("/api/v2/libraries?limit=100")
                .await?
                .iter()
                .map(library)
                .collect::<UiResult<_>>()?;
            let rows = self
                .list(&format!(
                    "/api/v2/profiles/{}/continue-watching?limit=20",
                    segment(&who.profile_id)?
                ))
                .await?;
            let mut continue_watching = Vec::new();
            for row in rows {
                continue_watching.push(ContinueCard {
                    item: card(&row["item"])?,
                    timeline_id: string(&row["timeline"], "id")?,
                    position_ms: row["viewing"]["position_ms"]
                        .as_u64()
                        .ok_or(UiError::Unavailable)?,
                    duration_ms: row["timeline"]["duration_ms"].as_u64(),
                });
            }
            Ok(HomeView {
                libraries,
                continue_watching,
            })
        })
    }
    fn library<'a>(
        &'a self,
        _: &'a UiPrincipal,
        id: &'a str,
    ) -> BoxFuture<'a, UiResult<LibraryView>> {
        Box::pin(async move {
            let id = segment(id)?;
            let library = library(&self.get(&format!("/api/v2/libraries/{id}")).await?.0)?;
            let items = self
                .list(&format!("/api/v2/catalog/items?library_id={id}&limit=100"))
                .await?
                .iter()
                .map(card)
                .collect::<UiResult<_>>()?;
            Ok(LibraryView { library, items })
        })
    }
    fn search<'a>(
        &'a self,
        _: &'a UiPrincipal,
        query: &'a str,
    ) -> BoxFuture<'a, UiResult<SearchView>> {
        Box::pin(async move {
            let encoded = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("q", query)
                .append_pair("limit", "100")
                .finish();
            let items = self
                .list(&format!("/api/v2/catalog/search?{encoded}"))
                .await?
                .iter()
                .map(card)
                .collect::<UiResult<_>>()?;
            Ok(SearchView {
                query: query.into(),
                items,
            })
        })
    }
    fn profiles<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<ProfilesView>> {
        Box::pin(async move {
            let (value, etag) = self
                .get(&format!(
                    "/api/v2/profiles/{}/preferences",
                    segment(&who.profile_id)?
                ))
                .await?;
            let strings = |key: &str| -> UiResult<Vec<String>> {
                value[key]
                    .as_array()
                    .ok_or(UiError::Unavailable)?
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned).ok_or(UiError::Unavailable))
                    .collect()
            };
            Ok(ProfilesView {
                profiles: who.profiles.clone(),
                current: who.profile_id.clone(),
                preferences: PreferencesView {
                    audio_languages: strings("audio_languages")?,
                    subtitle_languages: strings("subtitle_languages")?,
                    subtitle_mode: string(&value, "subtitle_mode")?,
                    quality_mode: string(&value, "quality_mode")?,
                    allow_client_software_decode: value["allow_client_software_decode"]
                        .as_bool()
                        .ok_or(UiError::Unavailable)?,
                    autoplay: value["autoplay"].as_bool().ok_or(UiError::Unavailable)?,
                    completion_percent: value["completion_percent"]
                        .as_f64()
                        .filter(|v| (50.0..=100.0).contains(v))
                        .ok_or(UiError::Unavailable)?,
                    etag,
                },
            })
        })
    }
    fn item<'a>(&'a self, who: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<ItemView>> {
        Box::pin(async move {
            let id = segment(id)?;
            let item = card(&self.get(&format!("/api/v2/catalog/items/{id}")).await?.0)?;
            let children = self
                .list(&format!("/api/v2/catalog/items?parent_id={id}&limit=100"))
                .await?
                .iter()
                .map(card)
                .collect::<UiResult<_>>()?;
            let editions = self
                .list(&format!("/api/v2/catalog/items/{id}/editions?limit=100"))
                .await?;
            let rows = self
                .list(&format!("/api/v2/catalog/items/{id}/timelines?limit=100"))
                .await?;
            let mut timelines = Vec::new();
            for row in rows {
                let timeline = string(&row, "id")?;
                let viewing = self
                    .get(&format!(
                        "/api/v2/profiles/{}/timelines/{}/viewing",
                        segment(&who.profile_id)?,
                        segment(&timeline)?
                    ))
                    .await?
                    .0;
                let versions = self
                    .list(&format!(
                        "/api/v2/catalog/timelines/{}/versions?limit=100",
                        segment(&timeline)?
                    ))
                    .await?
                    .iter()
                    .map(|v| {
                        Ok(VersionView {
                            id: string(v, "id")?,
                            label: string(v, "label")?,
                            availability: availability(v)?,
                            summary: format!(
                                "{} · {}",
                                string(v, "origin")?,
                                string(v, "timeline_equivalence")?
                            ),
                        })
                    })
                    .collect::<UiResult<_>>()?;
                let edition = editions
                    .iter()
                    .find(|e| e["id"] == row["edition_id"])
                    .ok_or(UiError::Unavailable)?;
                timelines.push(TimelineView {
                    id: timeline,
                    edition: string(edition, "label")?,
                    duration_ms: row["duration_ms"].as_u64(),
                    position_ms: viewing["position_ms"]
                        .as_u64()
                        .ok_or(UiError::Unavailable)?,
                    watched: viewing["watched"].as_bool().ok_or(UiError::Unavailable)?,
                    versions,
                });
            }
            Ok(ItemView {
                item,
                children,
                timelines,
            })
        })
    }
    fn player<'a>(
        &'a self,
        who: &'a UiPrincipal,
        timeline: &'a str,
    ) -> BoxFuture<'a, UiResult<PlayerView>> {
        Box::pin(async move {
            let timeline = segment(timeline)?;
            let row = self
                .get(&format!("/api/v2/catalog/timelines/{timeline}"))
                .await?
                .0;
            let item_id = string(&row, "item_id")?;
            let item = self
                .get(&format!("/api/v2/catalog/items/{}", segment(&item_id)?))
                .await?
                .0;
            let viewing = self
                .get(&format!(
                    "/api/v2/profiles/{}/timelines/{timeline}/viewing",
                    segment(&who.profile_id)?
                ))
                .await?
                .0;
            Ok(PlayerView {
                item_id,
                title: string(&item, "title")?,
                timeline_id: timeline.into(),
                duration_ms: row["duration_ms"].as_u64(),
                resume_ms: viewing["position_ms"]
                    .as_u64()
                    .ok_or(UiError::Unavailable)?,
                viewing_revision: string(&viewing, "revision")?,
            })
        })
    }
    fn sources<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<SourcesView>> {
        Box::pin(async move {
            let sources = self
                .list("/api/v2/sources?limit=100")
                .await?
                .iter()
                .map(|v| {
                    Ok(SourceRow {
                        id: string(v, "id")?,
                        name: string(v, "name")?,
                        root_path: string(v, "root_path")?,
                        availability: availability(v)?,
                    })
                })
                .collect::<UiResult<Vec<_>>>()?;
            let libraries = self
                .list("/api/v2/libraries?limit=100")
                .await?
                .iter()
                .map(library)
                .collect::<UiResult<Vec<_>>>()?;
            let scans = self
                .list("/api/v2/scans?limit=100")
                .await?
                .iter()
                .map(|v| {
                    let library = libraries
                        .iter()
                        .find(|l| Some(l.id.as_str()) == v["library_id"].as_str())
                        .ok_or(UiError::Unavailable)?;
                    let rows = v["sources"]
                        .as_array()
                        .ok_or(UiError::Unavailable)?
                        .iter()
                        .map(|r| {
                            let source = sources
                                .iter()
                                .find(|s| Some(s.id.as_str()) == r["source_id"].as_str())
                                .ok_or(UiError::Unavailable)?;
                            Ok(ScanSourceRow {
                                source_name: source.name.clone(),
                                status: string(r, "status")?,
                                observed_files: decimal(r, "observed_files")?,
                                complete_directories: decimal(r, "complete_directories")?,
                                incomplete_directories: decimal(r, "incomplete_directories")?,
                                error_codes: strings(r, "error_codes")?,
                            })
                        })
                        .collect::<UiResult<_>>()?;
                    Ok(ScanRow {
                        id: string(v, "id")?,
                        library_name: library.name.clone(),
                        status: string(v, "status")?,
                        started: string(v, "captured_after")?,
                        sources: rows,
                    })
                })
                .collect::<UiResult<_>>()?;
            Ok(SourcesView {
                sources,
                libraries,
                scans,
            })
        })
    }
    fn matches<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<MatchRow>>> {
        Box::pin(async move {
            self.list("/api/v2/catalog/matches?limit=100")
                .await?
                .iter()
                .map(|v| {
                    let candidates = v["candidates"]
                        .as_array()
                        .ok_or(UiError::Unavailable)?
                        .iter()
                        .map(|c| {
                            Ok(MatchCandidateView {
                                id: string(c, "id")?,
                                title: string(c, "title")?,
                                confidence_percent: c["confidence"]
                                    .as_f64()
                                    .map(|v| (v * 100.0).round() as u8),
                                reasons: strings(c, "reason_codes")?,
                            })
                        })
                        .collect::<UiResult<_>>()?;
                    Ok(MatchRow {
                        id: string(v, "id")?,
                        subject: string(&v["file"], "file_id")?,
                        status: string(v, "status")?,
                        etag: format!("\"{}\"", string(v, "revision")?),
                        candidates,
                    })
                })
                .collect()
        })
    }
    fn jobs<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<JobRow>>> {
        Box::pin(async move {
            self.list("/api/v2/jobs?limit=100")
                .await?
                .iter()
                .map(|v| {
                    Ok(JobRow {
                        id: string(v, "id")?,
                        kind: string(v, "kind")?,
                        phase: string(v, "phase")?,
                        progress_percent: v["progress"].as_f64().map(|v| (v * 100.0).round() as u8),
                        error_code: v["error_code"].as_str().map(str::to_owned),
                    })
                })
                .collect()
        })
    }
    fn diagnostics<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<DiagnosticsView>> {
        Box::pin(async move {
            let capability = self.get("/api/v2/system/capabilities").await?.0;
            let health = self.get("/api/v2/system/health").await?.0;
            let value = self.get("/api/v2/admin/diagnostics").await?.0;
            Ok(DiagnosticsView {
                server_id: string(&capability, "server_id")?,
                server_version: string(&capability, "server_version")?,
                api_version: string(&capability, "api_version")?,
                schema_version: string(&capability, "schema_version")?,
                health: string(&health, "status")?,
                uptime_seconds: decimal(&value, "uptime_seconds")?,
                active_deliveries: decimal(&value, "active_deliveries")?,
                queued_jobs: decimal(&value, "queued_jobs")?,
                running_jobs: decimal(&value, "running_jobs")?,
                worker_errors: strings(&value, "worker_errors")?,
            })
        })
    }
}

async fn identity(State(context): State<Context>, mut request: Request, next: Next) -> Response {
    let mut scoped = ApiFacade::new(context.api.clone(), request.headers().clone());
    scoped.trusted_ingress = request
        .extensions()
        .get::<crate::v2::auth::TrustedIngress>()
        .is_some();
    let facade = Arc::new(scoped);
    if let Some(PresentationIdentity(Some(caller))) =
        request.extensions().get::<PresentationIdentity>().cloned()
    {
        let profiles = match facade.list("/api/v2/profiles?limit=200").await {
            Ok(rows) => rows
                .iter()
                .map(|r| {
                    Ok(ProfileOption {
                        id: string(r, "id")?,
                        name: string(r, "name")?,
                    })
                })
                .collect::<UiResult<Vec<_>>>(),
            Err(error) => Err(error),
        };
        let Ok(profiles) = profiles else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let selected = request
            .headers()
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.split(';')
                    .find_map(|p| p.trim().strip_prefix("motion_profile="))
            });
        let selected = profiles
            .iter()
            .find(|p| Some(p.id.as_str()) == selected)
            .or_else(|| profiles.first());
        if let Some(profile) = selected {
            let permissions = playscale_core::access::Permission::ALL
                .into_iter()
                .filter(|p| caller.principal.allows(*p))
                .filter_map(|p| serde_json::to_value(p).ok()?.as_str().map(str::to_owned))
                .collect();
            request.extensions_mut().insert(UiPrincipal {
                principal_id: caller.principal.id.clone(),
                profile_id: profile.id.clone(),
                profile_name: profile.name.clone(),
                profiles,
                permissions,
                server_epoch: context.app.access.server_epoch.clone(),
                csrf_token: caller
                    .csrf
                    .clone()
                    .or_else(|| {
                        caller
                            .ingress
                            .as_ref()
                            .map(|login| crate::v2::auth::ingress_csrf(&context.app, login))
                    })
                    .unwrap_or_default(),
            });
        }
    }
    request.extensions_mut().insert(Facade(facade));
    next.run(request).await
}

pub fn router(app: App, assets: Option<std::path::PathBuf>) -> Router {
    let api = crate::api::router(app.clone(), None);
    let facade = Arc::new(ApiFacade::new(api.clone(), HeaderMap::new()));
    let ui = motion_ui::mount(Router::new(), facade, assets).layer(middleware::from_fn_with_state(
        Context {
            api,
            app: app.clone(),
        },
        identity,
    ));
    crate::api::router_with(app, None, Some(ui))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, routing::get};
    use serde_json::json;
    fn who() -> UiPrincipal {
        UiPrincipal {
            principal_id: "p".into(),
            profile_id: "profile".into(),
            profile_name: "Profile".into(),
            profiles: vec![],
            permissions: vec![],
            server_epoch: "epoch".into(),
            csrf_token: "csrf".into(),
        }
    }
    #[tokio::test]
    async fn facade_reads_the_api_under_its_own_request_identity() {
        let router=Router::new().route("/api/v2/jobs",get(|headers: HeaderMap| async move {
            let identity=headers["authorization"].to_str().unwrap();
            Json(json!({"items":[{"id":identity,"kind":"scan","phase":"running","progress":0.5,"error_code":null}]}))
        }));
        let mut first = HeaderMap::new();
        first.insert("authorization", "first".parse().unwrap());
        let mut second = HeaderMap::new();
        second.insert("authorization", "second".parse().unwrap());
        let a = ApiFacade::new(router.clone(), first);
        let b = ApiFacade::new(router, second);
        let who = who();
        let (a, b) = tokio::join!(a.jobs(&who), b.jobs(&who));
        assert_eq!(a.unwrap()[0].id, "first");
        let b = b.unwrap();
        assert_eq!(b[0].id, "second");
        assert_eq!(b[0].progress_percent, Some(50));
    }
    #[tokio::test]
    async fn unavailable_lists_are_not_rendered_as_empty_and_work_is_bounded() {
        let missing = ApiFacade::new(Router::new(), HeaderMap::new());
        assert_eq!(missing.list("/missing").await, Err(UiError::Unavailable));
        let router = Router::new().route("/read", get(|| async { Json(json!({"items":[]})) }));
        let facade = ApiFacade::new(router, HeaderMap::new());
        for _ in 0..32 {
            assert_eq!(facade.list("/read").await.unwrap().len(), 0);
        }
        assert_eq!(facade.list("/read").await, Err(UiError::Unavailable));
    }
    #[tokio::test]
    async fn preference_projection_preserves_fractional_completion_and_exact_etag() {
        let router=Router::new().route("/api/v2/profiles/profile/preferences",get(||async {
            ([("etag","\"prefs-7\"")],Json(json!({"audio_languages":["en"],"subtitle_languages":[],"subtitle_mode":"forced","quality_mode":"original","allow_client_software_decode":false,"autoplay":false,"completion_percent":95.5})))
        }));
        let facade = ApiFacade::new(router, HeaderMap::new());
        let result = facade.profiles(&who()).await.unwrap();
        assert_eq!(result.preferences.completion_percent, 95.5);
        assert_eq!(result.preferences.etag, "\"prefs-7\"");
        assert_eq!(result.preferences.subtitle_mode, "forced");
    }
}
