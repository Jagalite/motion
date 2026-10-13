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
        artwork_url: v["artwork_id"]
            .as_str()
            .map(|id| format!("/api/v2/media/assets/{}/content?revision=1", segment(id))),
    }
}
/// The strong validator of a resource read from a list (`"r-N"`).
fn tag(v: &Value) -> UiResult<String> {
    let revision = text(v, "revision")
        .parse::<u64>()
        .map_err(|_| UiError::Unavailable)?;
    Ok(v2::etag(revision).to_str().unwrap().to_owned())
}
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
fn count(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}
/// Human size, e.g. "1.4 GB".
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = "bytes";
    for next in UNITS {
        if value < 1000.0 {
            break;
        }
        value /= 1000.0;
        unit = next;
    }
    if unit == "bytes" {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {unit}")
    }
}
/// A file of an item, labelled without its server path.
/// A version as a person can tell it apart: its label (or origin when it
/// has none), the size and length of its first file, and a short ID, which
/// is always distinct.
fn version_label(edition: &str, v: &Value, item_files: &[Value]) -> String {
    let name = match text(v, "label") {
        label if !label.is_empty() => label,
        _ => format!("{} version", text(v, "origin")),
    };
    let mut parts = vec![edition.to_owned(), name];
    let first = v["files"][0]["file"]["file_id"].as_str();
    if let Some(file) = item_files
        .iter()
        .find(|f| first.is_some_and(|id| f["id"] == id))
    {
        if let Ok(bytes) = text(file, "size_bytes").parse::<f64>() {
            parts.push(format!("{:.1} GB", bytes / 1e9));
        }
        if let Some(ms) = file["duration_ms"]
            .as_u64()
            .or_else(|| text(file, "duration_ms").parse().ok())
        {
            parts.push(format!(
                "{}:{:02}:{:02}",
                ms / 3_600_000,
                ms / 60_000 % 60,
                ms / 1000 % 60
            ));
        }
    }
    let id = text(v, "id");
    parts.push(format!("#{}", &id[..id.len().min(8)]));
    parts.join(" · ")
}

/// A prepared version in words: its recipe and the tracks it kept.
fn prepared_label(r: &Value) -> String {
    let recipe = match r["recipe_id"].as_str().unwrap_or("") {
        "remux_mp4" => "MP4 copy",
        "audio_aac" => "AAC audio",
        "h264720p" => "H.264 720p",
        "video_profile" => "Custom profile",
        other => other,
    };
    let ordinal = |key: &str, prefix: char| {
        r["tracks"][key]
            .as_str()
            .and_then(|id| id.strip_prefix(prefix)?.parse::<u32>().ok())
    };
    let mut parts = vec![recipe.to_owned()];
    parts.extend(ordinal("audio_track_id", 'a').map(|n| format!("audio {}", n + 1)));
    parts.extend(ordinal("subtitle_track_id", 's').map(|n| format!("subtitle {}", n + 1)));
    parts.join(" · ")
}

/// A file's text subtitle streams, as (track ID `s{n}`, label): what a
/// preparation can keep, and what a download can carry as WebVTT. `n`
/// counts every subtitle stream of the revision.
fn text_subtitle_tracks(v: &Value) -> Vec<(String, String)> {
    v["tracks"]
        .as_array()
        .map(|tracks| {
            tracks
                .iter()
                .filter(|t| t["kind"] == "subtitle")
                .enumerate()
                .filter(|(_, t)| {
                    t["codec"]
                        .as_str()
                        .is_some_and(playscale_core::delivery::sidecar_subtitle)
                })
                .map(|(n, t)| {
                    let mut label = vec![format!("Subtitle {}", n + 1)];
                    label.extend(t["language"].as_str().map(str::to_owned));
                    label.extend(t["title"].as_str().map(str::to_owned));
                    (format!("s{n}"), label.join(" · "))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn file_row(v: &Value, library_id: String, n: usize) -> FileRow {
    let audio_tracks = v["tracks"]
        .as_array()
        .map(|tracks| {
            tracks
                .iter()
                .filter(|t| t["kind"] == "audio")
                .enumerate()
                .map(|(n, t)| {
                    let mut label = vec![format!("Track {}", n + 1)];
                    label.extend(t["language"].as_str().map(str::to_owned));
                    label.extend(t["codec"].as_str().map(str::to_owned));
                    label.extend(t["channels"].as_u64().map(|c| format!("{c}ch")));
                    (format!("a{n}"), label.join(" · "))
                })
                .collect()
        })
        .unwrap_or_default();
    // Only text subtitles can be kept by a preparation (as mov_text).
    let subtitle_tracks = text_subtitle_tracks(v);
    let mut label = format!("File {} · {}", n + 1, size(count(&v["size_bytes"])));
    if let Some(ms) = v["duration_ms"].as_u64() {
        label.push_str(&format!(" · {} min", ms.div_ceil(60_000)));
    }
    FileRow {
        file_id: text(v, "id"),
        file_revision: text(v, "revision"),
        library_id,
        label,
        availability: availability(v),
        audio_tracks,
        subtitle_tracks,
    }
}
/// Members of a smart collection shown on the organize page.
const SMART_PREVIEW: usize = 24;

/// Bounds on the per-title reads of the organize page: distinct playlist
/// timelines, and titles a batched read did not return.
const ORGANIZE_TIMELINE_READS: usize = 500;
const ORGANIZE_FALLBACK_READS: usize = 100;

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
    async fn audio_tracks(
        &self,
        timeline: &str,
    ) -> (Option<(String, String, String)>, Vec<TrackOption>) {
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
        (Some((version, file, revision)), options)
    }

    /// Every row of a paged list, bounded to `pages` pages of 200. Callers
    /// use server filters and orderings to keep the read narrow; beyond the
    /// bound this reports unavailable rather than a partial view.
    /// Every page of a timeline's effective markers (a timeline holds at
    /// most `MAX_MARKERS`). Pages are bound to one snapshot: a change between
    /// pages is refused, and the read starts again a few times before showing
    /// none, never a mix of two effective sets.
    async fn effective_markers(&self, timeline_id: &str) -> Vec<Value> {
        let path = format!(
            "/catalog/timelines/{}/markers?effective=true",
            segment(timeline_id)
        );
        for _ in 0..3 {
            if let Ok(markers) = self.list_all(&path, 5).await {
                return markers;
            }
        }
        vec![]
    }

    /// Up to `pages` pages of a listing: the rows read, and whether that
    /// was all of them (the rows are kept either way).
    async fn list_up_to(&self, path: &str, pages: usize) -> UiResult<(Vec<Value>, bool)> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut rows = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..pages {
            let url = match &cursor {
                Some(c) => {
                    let q = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("cursor", c)
                        .finish();
                    format!("{path}{separator}limit=200&{q}")
                }
                None => format!("{path}{separator}limit=200"),
            };
            let page = self.read(&url).await?;
            rows.extend(
                page["items"]
                    .as_array()
                    .ok_or(UiError::Unavailable)?
                    .clone(),
            );
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok((rows, true)),
            }
        }
        Ok((rows, false))
    }

    async fn list_all(&self, path: &str, pages: usize) -> UiResult<Vec<Value>> {
        let separator = if path.contains('?') { '&' } else { '?' };
        let mut rows = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..pages {
            let url = match &cursor {
                Some(c) => {
                    let q = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("cursor", c)
                        .finish();
                    format!("{path}{separator}limit=200&{q}")
                }
                None => format!("{path}{separator}limit=200"),
            };
            let page = self.read(&url).await?;
            rows.extend(
                page["items"]
                    .as_array()
                    .ok_or(UiError::Unavailable)?
                    .clone(),
            );
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => return Ok(rows),
            }
        }
        Err(UiError::Unavailable)
    }

    /// Title cards for `ids`, read 100 at a time through the catalog's
    /// batched `ids` filter. An ID a batch does not return (a merged alias,
    /// or a hidden or removed title) gets one individual read, which resolves
    /// aliases and maps hidden titles to `None`, up to a bound; past it the
    /// ID is left out (not loaded).
    async fn cards(
        &self,
        ids: &[String],
    ) -> UiResult<std::collections::HashMap<String, Option<ItemCard>>> {
        let mut unique: Vec<&String> = Vec::new();
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        let mut cards = std::collections::HashMap::new();
        for chunk in unique.chunks(100) {
            let list = chunk
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("ids", &list)
                .finish();
            for v in self.list(&format!("/catalog/items?limit=100&{q}")).await? {
                cards.insert(text(&v, "id"), Some(item(&v)));
            }
        }
        let mut fallback = ORGANIZE_FALLBACK_READS;
        for id in unique {
            if cards.contains_key(id) || fallback == 0 {
                continue;
            }
            fallback -= 1;
            let card = match self.read(&format!("/catalog/items/{}", segment(id))).await {
                Ok(v) => Some(item(&v)),
                Err(UiError::NotFound) => None,
                Err(e) => return Err(e),
            };
            cards.insert(id.clone(), card);
        }
        Ok(cards)
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
            let (server_id, restore_epoch) = match crate::v2::system::server_identity(&app).await {
                Ok(identity) => identity,
                Err(error) => return error.into_response(),
            };
            request.extensions_mut().insert(UiPrincipal {
                principal_id: caller.principal.id,
                profile_id: profile.id.clone(),
                profile_name: profile.name.clone(),
                profiles,
                permissions,
                server_epoch: app.access.server_epoch.clone(),
                viewing_epoch: format!("{server_id}:{restore_epoch}"),
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
            // The item's files, for the subtitles a download may carry.
            let item_files = if who.can("downloads:manage") {
                self.complete_list(&format!("{path}/files?limit=200"))
                    .await
                    .unwrap_or_default()
            } else {
                vec![]
            };
            let mut timelines = Vec::new();
            for row in rows {
                let timeline = text(&row, "id");
                let edition = editions
                    .iter()
                    .find(|e| e["id"] == row["edition_id"])
                    .ok_or(UiError::Unavailable)?;
                let version_rows = self
                    .complete_list(&format!(
                        "/catalog/timelines/{}/versions?limit=200",
                        segment(&timeline)
                    ))
                    .await?;
                // Whole single-file originals with text subtitles: a download
                // may carry one of them, pinned to that exact version.
                let subtitle_downloads = version_rows
                    .iter()
                    .filter(|v| v["origin"] == "original")
                    .filter_map(|v| {
                        let [binding] = v["files"].as_array()?.as_slice() else {
                            return None;
                        };
                        if binding["part"] != 1
                            || !binding["start_ms"].is_null()
                            || !binding["end_ms"].is_null()
                        {
                            return None;
                        }
                        let file = item_files.iter().find(|f| {
                            f["id"] == binding["file"]["file_id"]
                                && f["revision"] == binding["file"]["file_revision"]
                        })?;
                        let tracks = text_subtitle_tracks(file);
                        let label = text(v, "label");
                        (!tracks.is_empty()).then(|| SubtitleDownload {
                            version_id: text(v, "id"),
                            label: if label.trim().is_empty() {
                                "Unnamed version".into()
                            } else {
                                label
                            },
                            tracks,
                        })
                    })
                    .collect();
                let versions = version_rows
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
                // Ready prepared versions, when this caller may save and read
                // them; each is offered with exactly the selection it kept.
                let mut prepared_incomplete = false;
                let prepared_downloads =
                    if who.can("downloads:manage") && who.can("processing:request") {
                        // Every page: ready versions may follow pending ones.
                        let query = url::form_urlencoded::Serializer::new(String::new())
                            .append_pair("timeline_id", &timeline)
                            .finish();
                        // The ready ones among those read are kept even when
                        // more remain (`prepared_incomplete` says so).
                        let (rows, complete) = self
                            .list_up_to(&format!("/processing/renditions?{query}"), 5)
                            .await
                            .unwrap_or((vec![], true));
                        prepared_incomplete = !complete;
                        rows.iter()
                            .filter(|r| {
                                r["status"] == "ready"
                                    && r["tracks"].is_object()
                                    && r["version_id"].is_string()
                            })
                            .map(|r| PreparedDownload {
                                label: prepared_label(r),
                                recipe_id: text(r, "recipe_id"),
                                tracks_json: r["tracks"].to_string(),
                                version_id: text(r, "version_id"),
                            })
                            .collect()
                    } else {
                        vec![]
                    };
                timelines.push(TimelineView {
                    id: timeline,
                    edition: text(edition, "label"),
                    duration_ms: row["duration_ms"].as_u64(),
                    viewing,
                    versions,
                    prepared_downloads,
                    prepared_incomplete,
                    subtitle_downloads,
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
            let (audio_source, audio_tracks) = self.audio_tracks(timeline_id).await;
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
                audio_version: audio_source.as_ref().map(|s| s.0.clone()),
                audio_file: audio_source.map(|(_, file, revision)| (file, revision)),
                // Chapters and skip markers in effect; none if unreadable.
                markers: self
                    .effective_markers(timeline_id)
                    .await
                    .iter()
                    .map(|m| MarkerView {
                        kind: text(m, "kind"),
                        start_ms: m["start_ms"].as_u64().unwrap_or(0),
                        end_ms: m["end_ms"].as_u64(),
                        label: m["label"].as_str().map(str::to_owned),
                        detected: m["provenance"] == "detected",
                    })
                    .collect(),
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
                    max_bitrate_bps: prefs["max_bitrate_bps"]
                        .as_str()
                        .and_then(|b| b.parse().ok()),
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
            let source_rows = self.complete_list("/sources?limit=200").await?;
            let library_rows = self.complete_list("/libraries?limit=200").await?;
            let source_name = |id: &str| {
                source_rows
                    .iter()
                    .find(|s| s["id"] == id)
                    .map(|s| text(s, "name"))
                    .unwrap_or_else(|| "Removed source".into())
            };
            let library_name = |id: &str| {
                library_rows
                    .iter()
                    .find(|l| l["id"] == id)
                    .map(|l| text(l, "name"))
                    .unwrap_or_else(|| "Removed library".into())
            };
            // Recent scans, newest first. A scan read failure is shown as
            // unavailable, never as an empty history.
            let scans = match self.list("/scans?order=recent&limit=20").await {
                Ok(rows) => Some(
                    rows.iter()
                        .map(|v| ScanRow {
                            id: text(v, "id"),
                            library_name: library_name(v["library_id"].as_str().unwrap_or("")),
                            status: text(v, "status"),
                            started: text(v, "captured_after"),
                            sources: v["sources"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|r| ScanSourceRow {
                                    source_name: source_name(r["source_id"].as_str().unwrap_or("")),
                                    status: text(r, "status"),
                                    observed_files: count(&r["observed_files"]),
                                    complete_directories: count(&r["complete_directories"]),
                                    incomplete_directories: count(&r["incomplete_directories"]),
                                    error_codes: strings(&r["error_codes"]),
                                })
                                .collect(),
                        })
                        .collect(),
                ),
                Err(_) => None,
            };
            let schedules = match self.list_all("/schedules", 5).await {
                Ok(rows) => rows
                    .iter()
                    .map(|v| {
                        Ok(ScheduleRow {
                            id: text(v, "id"),
                            library_name: library_name(v["library_id"].as_str().unwrap_or("")),
                            library_id: text(v, "library_id"),
                            task: text(v, "task"),
                            interval_seconds: count(&v["interval_seconds"]),
                            enabled: v["enabled"].as_bool().unwrap_or(false),
                            etag: tag(v)?,
                        })
                    })
                    .collect::<UiResult<Vec<_>>>()
                    .ok(),
                Err(_) => None,
            };
            Ok(SourcesView {
                sources: source_rows
                    .iter()
                    .map(|v| {
                        Ok(SourceRow {
                            id: text(v, "id"),
                            name: text(v, "name"),
                            root_path: text(v, "root_path"),
                            availability: availability(v),
                            etag: tag(v)?,
                        })
                    })
                    .collect::<UiResult<_>>()?,
                libraries: library_rows
                    .iter()
                    .map(|v| {
                        Ok(LibraryAdminRow {
                            library: library(v),
                            language: text(v, "language"),
                            source_ids: strings(&v["source_ids"]),
                            etag: tag(v)?,
                        })
                    })
                    .collect::<UiResult<_>>()?,
                scans_available: scans.is_some(),
                scans: scans.unwrap_or_default(),
                schedules,
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
                        // The title the file is cataloged under, when
                        // readable; otherwise its file name.
                        subject: match v["subject"]["title"].as_str() {
                            Some(title) => {
                                format!("{title} ({})", text(&v["subject"], "file_name"))
                            }
                            None => match v["subject"]["file_name"].as_str() {
                                Some(name) if !name.is_empty() => name.to_owned(),
                                _ => format!("File {}", text(&v["file"], "file_id")),
                            },
                        },
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
            self.list("/jobs?limit=200")
                .await?
                .iter()
                .map(|v| {
                    Ok(JobRow {
                        id: text(v, "id"),
                        kind: text(v, "kind"),
                        phase: text(v, "phase"),
                        progress_percent: v["progress"]
                            .as_f64()
                            .map(|p| (p * 100.).clamp(0., 100.) as u8),
                        error_code: v["error_code"].as_str().map(str::to_owned),
                        etag: tag(v)?,
                    })
                })
                .collect()
        })
    }
    fn downloads<'a>(&'a self, _: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<DownloadRow>>> {
        Box::pin(async move {
            let mut rows = Vec::new();
            // Every record, not the first page; past the bound the page
            // reports unavailable rather than a partial list. Cancelled
            // records need no action and are not shown.
            for v in self
                .list_all("/downloads", 25)
                .await?
                .into_iter()
                .filter(|v| v["state"] != "cancelled")
            {
                // Titles come from the same scoped catalog reads; a timeline
                // no longer readable is shown without one.
                let timeline = text(&v, "timeline_id");
                let title = match self
                    .read(&format!("/catalog/timelines/{}", segment(&timeline)))
                    .await
                {
                    Ok(t) => self
                        .read(&format!("/catalog/items/{}", segment(&text(&t, "item_id"))))
                        .await
                        .map(|i| text(&i, "title"))
                        .unwrap_or_default(),
                    Err(_) => String::new(),
                };
                rows.push(DownloadRow {
                    id: text(&v, "id"),
                    title,
                    state: text(&v, "state"),
                    size_bytes: text(&v, "size_bytes").parse().unwrap_or(0),
                    device_transfer: v["device_transfer"].is_object().then(|| {
                        (
                            text(&v["device_transfer"], "state"),
                            text(&v["device_transfer"], "bytes_received")
                                .parse()
                                .unwrap_or(0),
                        )
                    }),
                });
            }
            Ok(rows)
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
    fn manage_item<'a>(
        &'a self,
        who: &'a UiPrincipal,
        id: &'a str,
    ) -> BoxFuture<'a, UiResult<ItemManageView>> {
        Box::pin(async move {
            let write = who.can("catalog:write");
            let prepare = who.can("processing:request");
            let organize = who.can("collections:write");
            if !(write || prepare || organize) {
                return Err(UiError::Denied);
            }
            let (value, item_etag) = self
                .read_tagged(&format!("/catalog/items/{}", segment(id)))
                .await?;
            // A retired (merged) ID resolves to its live work. Section reads
            // (metadata, artwork) reject aliases, so they use the live ID.
            let item_id = text(&value, "id");
            let path = format!("/catalog/items/{}", segment(&item_id));
            let card = item(&value);
            let library_ids = strings(&value["library_ids"]);
            // Timelines with their editions and the exact files bound to their
            // original versions: (timeline, edition, whole-file originals).
            // Generated renditions are never identification or preparation sources.
            let structure = if write || prepare {
                async {
                    let editions = self
                        .complete_list(&format!("{path}/editions?limit=200"))
                        .await?;
                    let rows = self
                        .complete_list(&format!("{path}/timelines?limit=20"))
                        .await?;
                    let mut originals = std::collections::HashSet::new();
                    let mut out = Vec::new();
                    let mut version_rows = Vec::new();
                    // File facts to tell unlabeled versions apart (size, length).
                    let item_files = self
                        .complete_list(&format!("{path}/files?limit=200"))
                        .await
                        .unwrap_or_default();
                    for row in rows {
                        let timeline = text(&row, "id");
                        let edition = editions
                            .iter()
                            .find(|e| e["id"] == row["edition_id"])
                            .map(|e| text(e, "label"))
                            .unwrap_or_default();
                        let versions = self
                            .complete_list(&format!(
                                "/catalog/timelines/{}/versions?limit=200",
                                segment(&timeline)
                            ))
                            .await?;
                        for v in &versions {
                            version_rows.push(VersionRow {
                                id: text(v, "id"),
                                label: version_label(&edition, v, &item_files),
                            });
                        }
                        let mut whole = Vec::new();
                        for v in versions.iter().filter(|v| v["origin"] == "original") {
                            let bindings = v["files"].as_array().cloned().unwrap_or_default();
                            for b in &bindings {
                                originals.insert((
                                    text(&b["file"], "file_id"),
                                    text(&b["file"], "file_revision"),
                                ));
                            }
                            // Only an available original bound as one whole file is preparable.
                            // Interval bindings (e.g. double episodes) are not whole files.
                            if let [only] = bindings.as_slice()
                                && v["availability"] == "available"
                                && only["start_ms"].is_null()
                                && only["end_ms"].is_null()
                            {
                                whole.push((
                                    text(&only["file"], "file_id"),
                                    text(&only["file"], "file_revision"),
                                ));
                            }
                        }
                        out.push((timeline, edition, whole));
                    }
                    UiResult::Ok((out, originals, version_rows))
                }
                .await
                .ok()
            } else {
                None
            };
            // Original files, with the item library that holds each file's
            // source (a match proposal is scoped to that library).
            let files = match &structure {
                Some((_, originals, _)) => {
                    let libraries = self.complete_list("/libraries?limit=200").await.ok();
                    self.complete_list(&format!("{path}/files?limit=200"))
                        .await
                        .ok()
                        .map(|rows| {
                            rows.iter()
                                .filter(|f| {
                                    originals.contains(&(text(f, "id"), text(f, "revision")))
                                })
                                .enumerate()
                                .map(|(n, f)| {
                                    let library = libraries
                                        .iter()
                                        .flatten()
                                        .find(|l| {
                                            library_ids.iter().any(|id| l["id"] == id.as_str())
                                                && l["source_ids"]
                                                    .as_array()
                                                    .is_some_and(|s| s.contains(&f["source_id"]))
                                        })
                                        .map(|l| text(l, "id"))
                                        .unwrap_or_default();
                                    file_row(f, library, n)
                                })
                                .collect::<Vec<_>>()
                        })
                }
                None => None,
            };
            let metadata = if write {
                match self.read_tagged(&format!("{path}/metadata")).await {
                    Ok((m, Some(etag))) => {
                        let local = m["contributions"]
                            .as_array()
                            .and_then(|c| c.iter().find(|c| c["source"] == "local"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        let show = |v: &Value| match v {
                            Value::String(s) => s.clone(),
                            Value::Array(a) => a
                                .iter()
                                .map(|x| {
                                    x.as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| x.to_string())
                                })
                                .collect::<Vec<_>>()
                                .join(", "),
                            other => other.to_string(),
                        };
                        // The form replaces the whole local contribution; carry
                        // the local fields it does not show (release_date, cast).
                        let mut preserved =
                            local["values"].as_object().cloned().unwrap_or_default();
                        for shown in ["title", "description", "release_year"] {
                            preserved.remove(shown);
                        }
                        Some(MetadataEditView {
                            title: local["values"]["title"].as_str().unwrap_or_default().into(),
                            description: local["values"]["description"]
                                .as_str()
                                .unwrap_or_default()
                                .into(),
                            release_year: local["values"]["release_year"]
                                .as_i64()
                                .map(|y| y.to_string())
                                .unwrap_or_default(),
                            preserved_values: Value::Object(preserved).to_string(),
                            tags: strings(&local["tags"]),
                            excluded_tags: strings(&local["excluded_tags"]),
                            field_sources: m["field_sources"]
                                .as_object()
                                .map(|o| o.iter().map(|(k, v)| (k.clone(), strings(v))).collect())
                                .unwrap_or_default(),
                            effective: m["values"]
                                .as_object()
                                .map(|o| o.iter().map(|(k, v)| (k.clone(), show(v))).collect())
                                .unwrap_or_default(),
                            etag,
                        })
                    }
                    _ => None,
                }
            } else {
                None
            };
            let artwork = if write {
                self.complete_list(&format!("{path}/artwork?limit=200"))
                    .await
                    .ok()
                    .map(|rows| {
                        rows.iter()
                            .filter(|a| a["kind"] == "artwork")
                            .map(|a| ArtworkRow {
                                id: text(a, "id"),
                                content_url: text(a, "content_url"),
                                mime_type: text(a, "mime_type"),
                                selected: value["artwork_id"] == a["id"],
                            })
                            .collect()
                    })
            } else {
                None
            };
            let (recipes, timelines) = if prepare {
                // `video_profile` is offered by its own custom-profile form.
                let recipes = self
                    .complete_list("/processing/recipes?limit=200")
                    .await
                    .ok()
                    .map(|rows| {
                        rows.iter()
                            .filter(|r| r["purpose"] == "rendition")
                            .map(|r| RecipeOption {
                                id: text(r, "id"),
                                name: text(r, "name"),
                                backends: strings(&r["allowed_backends"]),
                                qualification: text(r, "qualification"),
                            })
                            .collect()
                    });
                // Each timeline's renditions are read through the server's
                // timeline filter. Past the read bound (or on a failed read)
                // they are reported unknown while preparing stays available.
                let mut renditions = std::collections::HashMap::new();
                for (timeline, _, _) in structure.iter().flat_map(|(rows, _, _)| rows) {
                    let rows = self
                        .list_all(
                            &format!("/processing/renditions?timeline_id={}", segment(timeline)),
                            5,
                        )
                        .await
                        .ok();
                    renditions.insert(timeline.clone(), rows);
                }
                let timelines = structure.as_ref().map(|(rows, _, _)| {
                    rows.iter()
                        .map(|(timeline, edition, whole)| PrepareTimeline {
                            renditions: renditions.get(timeline).cloned().flatten().map(|all| {
                                all.iter()
                                    .map(|r| RenditionRow {
                                        id: text(r, "id"),
                                        status: text(r, "status"),
                                        job_id: text(r, "job_id"),
                                        source_file_id: text(&r["source"], "file_id"),
                                    })
                                    .collect()
                            }),
                            sources: whole
                                .iter()
                                .filter_map(|(file, revision)| {
                                    files.iter().flatten().find(|row| {
                                        &row.file_id == file && &row.file_revision == revision
                                    })
                                })
                                .cloned()
                                .collect(),
                            id: timeline.clone(),
                            edition: edition.clone(),
                        })
                        .collect()
                });
                (recipes, timelines)
            } else {
                (None, None)
            };
            let (collections, playlists) = if organize {
                let mine = |v: &&Value| v["profile_id"] == who.profile_id.as_str();
                let collections = self.list_all("/collections", 5).await.ok().map(|rows| {
                    rows.iter()
                        .filter(mine)
                        .filter(|c| c["kind"] == "manual")
                        .filter_map(|c| {
                            let ids = strings(&c["item_ids"]);
                            let contains = ids.contains(&item_id);
                            let item_ids: Vec<String> = if contains {
                                ids.into_iter().filter(|i| *i != item_id).collect()
                            } else {
                                ids.into_iter().chain([item_id.clone()]).collect()
                            };
                            Some(MembershipRow {
                                id: text(c, "id"),
                                name: text(c, "name"),
                                etag: tag(c).ok()?,
                                contains,
                                toggle_body: serde_json::json!({
                                    "name": c["name"], "profile_id": c["profile_id"],
                                    "kind": "manual", "item_ids": item_ids, "filter_id": null,
                                })
                                .to_string(),
                            })
                        })
                        .collect()
                });
                // A playlist entry is a timeline; offer the title's default one.
                let playlists = match value["default_timeline_id"].as_str() {
                    None => Some(vec![]),
                    Some(timeline) => self.list_all("/playlists", 5).await.ok().map(|rows| {
                        rows.iter()
                            .filter(mine)
                            .filter_map(|p| {
                                let entries = p["entries"].as_array()?.clone();
                                let contains = entries.iter().any(|e| e["timeline_id"] == timeline);
                                let entries: Vec<Value> = if contains {
                                    entries
                                        .into_iter()
                                        .filter(|e| e["timeline_id"] != timeline)
                                        .collect()
                                } else {
                                    let revision = text(p, "revision");
                                    let mut entry_id = format!("e{revision}-{}", entries.len());
                                    while entries.iter().any(|e| e["entry_id"] == entry_id.as_str()) {
                                        entry_id.push('x');
                                    }
                                    entries
                                        .into_iter()
                                        .chain([serde_json::json!({"entry_id": entry_id, "timeline_id": timeline})])
                                        .collect()
                                };
                                Some(MembershipRow {
                                    id: text(p, "id"),
                                    name: text(p, "name"),
                                    etag: tag(p).ok()?,
                                    contains,
                                    toggle_body: serde_json::json!({
                                        "name": p["name"], "profile_id": p["profile_id"], "entries": entries,
                                    })
                                    .to_string(),
                                })
                            })
                            .collect()
                    }),
                };
                (collections, playlists)
            } else {
                (None, None)
            };
            // Play orders of this title's editions and its timelines' places.
            let ordering = if write {
                async {
                    let editions = self
                        .complete_list(&format!("{path}/editions?limit=200"))
                        .await?;
                    let label = |id: &Value| {
                        editions
                            .iter()
                            .find(|e| e["id"] == *id)
                            .map(|e| text(e, "label"))
                            .unwrap_or_default()
                    };
                    let groups = self
                        .complete_list(&format!("{path}/order-groups?limit=200"))
                        .await?
                        .iter()
                        .map(|g| OrderGroupView {
                            id: text(g, "id"),
                            name: text(g, "name"),
                            kind: text(g, "kind"),
                            edition: label(&g["edition_id"]),
                            revision: text(g, "revision"),
                        })
                        .collect();
                    let timelines = self
                        .complete_list(&format!("{path}/timelines?limit=20"))
                        .await?
                        .iter()
                        .map(|t| {
                            Ok(OrderedTimeline {
                                id: text(t, "id"),
                                edition: label(&t["edition_id"]),
                                etag: tag(t)?,
                                group_id: t["order_group_id"].as_str().map(str::to_owned),
                                position: t["order_position"].as_u64(),
                            })
                        })
                        .collect::<UiResult<Vec<_>>>()?;
                    UiResult::Ok(OrderingView {
                        editions: editions
                            .iter()
                            .map(|e| (text(e, "id"), text(e, "label")))
                            .collect(),
                        groups,
                        timelines,
                    })
                }
                .await
                .ok()
            } else {
                None
            };
            // Merge candidates: other readable titles found by this one's
            // title, each with the exact revisions a merge plan is fenced by.
            let item_revision = text(&value, "revision");
            let merge_candidates = if write {
                let q = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("q", &card.title)
                    .finish();
                self.list(&format!("/catalog/search?limit=20&{q}"))
                    .await
                    .ok()
                    .map(|rows| {
                        rows.iter()
                            .filter(|r| r["id"] != item_id.as_str())
                            .map(|r| MergeCandidate {
                                id: text(r, "id"),
                                title: text(r, "title"),
                                revision: text(r, "revision"),
                            })
                            .collect()
                    })
            } else {
                None
            };
            let versions = if write {
                structure.as_ref().map(|(_, _, versions)| versions.clone())
            } else {
                None
            };
            // Chapters and skip markers of each timeline, all of them (stale
            // and uncertain ones shown as such), for correction.
            let markers = if write {
                async {
                    let editions = self
                        .complete_list(&format!("{path}/editions?limit=200"))
                        .await?;
                    let mut out = Vec::new();
                    for t in self
                        .complete_list(&format!("{path}/timelines?limit=20"))
                        .await?
                    {
                        let revision = text(&t, "revision");
                        // Every page; past the bound the section is
                        // reported unavailable, never as a shorter list.
                        let rows = self
                            .list_all(
                                &format!("/catalog/timelines/{}/markers", segment(&text(&t, "id"))),
                                5,
                            )
                            .await?;
                        out.push(TimelineMarkersView {
                            timeline_id: text(&t, "id"),
                            edition: editions
                                .iter()
                                .find(|e| e["id"] == t["edition_id"])
                                .map(|e| text(e, "label"))
                                .unwrap_or_default(),
                            markers: rows
                                .iter()
                                .map(|m| StoredMarkerView {
                                    kind: text(m, "kind"),
                                    start_ms: m["start_ms"].as_u64().unwrap_or(0),
                                    end_ms: m["end_ms"].as_u64(),
                                    label: m["label"].as_str().map(str::to_owned),
                                    provenance: text(m, "provenance"),
                                    current: text(m, "timeline_revision") == revision,
                                })
                                .collect(),
                            revision,
                        });
                    }
                    UiResult::Ok(out)
                }
                .await
                .ok()
            } else {
                None
            };
            Ok(ItemManageView {
                item: card,
                markers,
                item_revision,
                merge_candidates,
                versions,
                library_ids,
                match_state: text(&value, "match_state"),
                item_etag: item_etag.ok_or(UiError::Unavailable)?,
                metadata,
                artwork,
                files: if write { files } else { None },
                recipes,
                timelines,
                collections,
                playlists,
                ordering,
            })
        })
    }

    fn organize<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<OrganizeView>> {
        Box::pin(async move {
            if !who.can("collections:write") {
                return Err(UiError::Denied);
            }
            let mine = |v: &&Value| v["profile_id"] == who.profile_id.as_str();
            let collection_rows: Vec<Value> = self
                .list_all("/collections", 5)
                .await?
                .into_iter()
                .filter(|v| mine(&v))
                .collect();
            let playlist_rows: Vec<Value> = self
                .list_all("/playlists", 5)
                .await?
                .into_iter()
                .filter(|v| mine(&v))
                .collect();
            // Playlist entries name timelines: one read per distinct timeline,
            // bounded. Past the bound an entry stays listed and removable
            // with its title not loaded, rather than failing the page.
            let mut timeline_items: std::collections::HashMap<String, Option<String>> =
                Default::default();
            let mut timeline_reads = ORGANIZE_TIMELINE_READS;
            for p in &playlist_rows {
                for e in p["entries"].as_array().ok_or(UiError::Unavailable)? {
                    let timeline = text(e, "timeline_id");
                    if timeline_items.contains_key(&timeline) || timeline_reads == 0 {
                        continue;
                    }
                    timeline_reads -= 1;
                    // Playlists hide entries by title visibility; the timeline
                    // read also requires its edition. An entry readable only in
                    // part stays listed (removable) without disclosing a title.
                    let owner = match self
                        .read(&format!("/catalog/timelines/{}", segment(&timeline)))
                        .await
                    {
                        Ok(t) => Some(text(&t, "item_id")),
                        Err(UiError::NotFound) => None,
                        Err(e) => return Err(e),
                    };
                    timeline_items.insert(timeline, owner);
                }
            }
            // Every title the page shows, read in batches. A smart collection's
            // members follow its filter (the server evaluates it): the first
            // few are shown, read-only.
            let shown = |c: &Value| {
                let ids = strings(&c["item_ids"]);
                if c["kind"] == "smart" {
                    ids.into_iter().take(SMART_PREVIEW).collect()
                } else {
                    ids
                }
            };
            let mut wanted: Vec<String> = Vec::new();
            for c in &collection_rows {
                wanted.extend(shown(c));
            }
            wanted.extend(timeline_items.values().flatten().cloned());
            let titles = self.cards(&wanted).await?;
            let mut collections = Vec::new();
            for c in &collection_rows {
                let smart = c["kind"] == "smart";
                let ids = shown(c);
                let mut items = Vec::new();
                let mut remove_bodies = Vec::new();
                for item_id in &ids {
                    if let Some(Some(card)) = titles.get(item_id).cloned() {
                        if smart {
                            items.push(card);
                            continue;
                        }
                        let rest: Vec<&String> = ids.iter().filter(|i| *i != item_id).collect();
                        remove_bodies.push(
                            serde_json::json!({
                                "name": c["name"], "profile_id": c["profile_id"], "kind": c["kind"],
                                "item_ids": rest, "filter_id": c["filter_id"],
                            })
                            .to_string(),
                        );
                        items.push(card);
                    }
                }
                collections.push(CollectionView {
                    id: text(c, "id"),
                    name: text(c, "name"),
                    kind: text(c, "kind"),
                    filter_id: c["filter_id"].as_str().map(str::to_owned),
                    etag: tag(c)?,
                    member_count: c["item_ids"].as_array().map_or(0, Vec::len),
                    items,
                    remove_bodies,
                });
            }
            let mut playlists = Vec::new();
            for p in &playlist_rows {
                let entries = p["entries"].as_array().ok_or(UiError::Unavailable)?.clone();
                let mut views = Vec::new();
                let mut bodies = Vec::new();
                for (n, e) in entries.iter().enumerate() {
                    let timeline = text(e, "timeline_id");
                    let title = match timeline_items.get(&timeline) {
                        None => "Title not loaded".into(),
                        Some(None) => "Unavailable title".into(),
                        Some(Some(item)) => match titles.get(item) {
                            Some(Some(card)) => card.title.clone(),
                            Some(None) => "Unavailable title".into(),
                            None => "Title not loaded".into(),
                        },
                    };
                    let body = |entries: Vec<&Value>| {
                        serde_json::json!({"name": p["name"], "profile_id": p["profile_id"], "entries": entries})
                            .to_string()
                    };
                    let remove = body(
                        entries
                            .iter()
                            .filter(|x| x["entry_id"] != e["entry_id"])
                            .collect(),
                    );
                    let up = (n > 0).then(|| {
                        let mut order: Vec<&Value> = entries.iter().collect();
                        order.swap(n - 1, n);
                        body(order)
                    });
                    bodies.push((remove, up));
                    views.push(PlaylistEntryView {
                        entry_id: text(e, "entry_id"),
                        title,
                        timeline_id: timeline,
                    });
                }
                playlists.push(PlaylistView {
                    id: text(p, "id"),
                    name: text(p, "name"),
                    etag: tag(p)?,
                    entries: views,
                    entry_bodies: bodies,
                });
            }
            // The profile's saved filters, with whether a smart collection
            // still uses each (deleting one in use is refused).
            let mut filters = Vec::new();
            for f in self
                .list_all("/catalog/filters", 5)
                .await?
                .iter()
                .filter(mine)
            {
                let id = text(f, "id");
                filters.push(SavedFilterView {
                    in_use: collections
                        .iter()
                        .any(|c| c.filter_id.as_deref() == Some(id.as_str())),
                    id,
                    name: text(f, "name"),
                    etag: tag(f)?,
                    terms: f["all"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|t| FilterTermView {
                            field: text(t, "field"),
                            operator: text(t, "operator"),
                            value: match &t["value"] {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            },
                        })
                        .collect(),
                });
            }
            Ok(OrganizeView {
                profile_id: who.profile_id.clone(),
                collections,
                playlists,
                filters,
            })
        })
    }
}

#[cfg(test)]
mod version_label_tests {
    use serde_json::json;

    #[test]
    fn unlabeled_versions_are_told_apart() {
        let files = [json!({"id":"f1","size_bytes":"4200000000","duration_ms":6130000})];
        let unlabeled = |id: &str| json!({"id":id,"label":"","origin":"original","files":[{"file":{"file_id":"f1"}}]});
        let a = super::version_label("Original", &unlabeled("aaaaaaaa-1"), &files);
        let b = super::version_label("Original", &unlabeled("bbbbbbbb-2"), &files);
        assert_eq!(
            a,
            "Original · original version · 4.2 GB · 1:42:10 · #aaaaaaaa"
        );
        assert_ne!(a, b);
        let labeled = json!({"id":"c1","label":"Director's cut","origin":"original","files":[]});
        assert_eq!(
            super::version_label("Original", &labeled, &[]),
            "Original · Director's cut · #c1"
        );
    }
}
