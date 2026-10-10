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
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .map_err(|_| UiError::Unavailable)?;
        serde_json::from_slice(&bytes).map_err(|_| UiError::Unavailable)
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
    fn home<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<HomeView>> {
        Box::pin(async move {
            Ok(HomeView {
                libraries: self
                    .list("/libraries?limit=200")
                    .await?
                    .iter()
                    .map(library)
                    .collect(),
                continue_watching: vec![],
                continue_available: false,
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
    fn item<'a>(&'a self, _: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<ItemView>> {
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
                timelines.push(TimelineView {
                    id: timeline,
                    edition: text(edition, "label"),
                    duration_ms: row["duration_ms"].as_u64(),
                    viewing: None,
                    versions,
                });
            }
            Ok(ItemView {
                item: item(&value),
                children,
                timelines,
                playback_available: false,
            })
        })
    }
    fn player<'a>(&'a self, _: &'a UiPrincipal, _: &'a str) -> BoxFuture<'a, UiResult<PlayerView>> {
        Box::pin(async { Err(UiError::Unavailable) })
    }
    fn profiles<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<ProfilesView>> {
        Box::pin(async { Err(UiError::Unavailable) })
    }
    fn sources<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<SourcesView>> {
        Box::pin(async { Err(UiError::Unavailable) })
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
