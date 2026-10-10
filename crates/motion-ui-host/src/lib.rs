//! Explicit client-cache transport. This crate has no server, SQLite, scanner,
//! encoder, provider or catalog dependency. A13 can implement the same reader
//! port; this file adapter consumes a versioned cache manifest and event log.
use anyhow::{Context, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path as AxumPath, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cap_std::fs::Dir;
use fs2::FileExt;
use motion_ui::{
    facade::{BoxFuture, UiResult},
    offline::{CachedDownload, OfflinePresentationReader},
};
use playscale_core::{
    offline::{self, Admission, CacheScope, OfflineEvent},
    ranges::{RangeDecision, select_range},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{Read, Seek, Write},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use uuid::Uuid;

const MAX_MANIFEST: u64 = 2 * 1024 * 1024;
const MAX_LOG: u64 = 8 * 1024 * 1024;
const MAX_EVENTS: usize = 10_000;
const MAX_MEDIA: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheManifest {
    pub protocol: u8,
    pub scope: CacheScope,
    pub downloads: Vec<CachedDownload>,
}

pub struct Cache {
    directory: Dir,
    _lock: std::fs::File,
    manifest: CacheManifest,
    history: Mutex<Vec<OfflineEvent>>,
}
fn bounded(mut file: cap_std::fs::File, limit: u64) -> anyhow::Result<Vec<u8>> {
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= limit,
        "cache document is too large or not a regular file"
    );
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "cache document grew beyond its limit"
    );
    Ok(bytes)
}
impl Cache {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let directory = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let lock = directory
            .open_with(
                "events.lock",
                cap_std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true),
            )?
            .into_std();
        lock.try_lock_exclusive()
            .context("the offline cache is already open")?;
        let manifest: CacheManifest =
            serde_json::from_slice(&bounded(directory.open("manifest.json")?, MAX_MANIFEST)?)?;
        ensure!(
            manifest.protocol == 1 && manifest.downloads.len() <= 500,
            "unsupported or oversized cache manifest"
        );
        let scope = &manifest.scope;
        ensure!(
            [
                &scope.server_id,
                &scope.principal_id,
                &scope.profile_id,
                &scope.device_id
            ]
            .into_iter()
            .all(|s| offline::identifier(s)),
            "invalid cache scope"
        );
        let mut ids = HashSet::new();
        for item in &manifest.downloads {
            let m = &item.identity;
            ensure!(
                offline::identifier(&m.download_id)
                    && ids.insert(m.download_id.clone())
                    && offline::identifier(&m.timeline_id),
                "invalid or duplicate download identity"
            );
            ensure!(
                [
                    &m.timeline_revision,
                    &m.base_viewing_revision,
                    &m.base_manual_epoch
                ]
                .into_iter()
                .all(|s| offline::sequence(s).is_some()),
                "invalid causal revision"
            );
            ensure!(
                !m.source_revision.is_empty() && m.source_revision.len() <= 256,
                "invalid source revision"
            );
            ensure!(
                item.title.len() <= 1024
                    && item.duration_ms > 0
                    && item.duration_ms <= 9_007_199_254_740_991,
                "invalid download metadata"
            );
            ensure!(
                item.sha256.len() == 64
                    && item
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid media digest"
            );
            ensure!(
                item.size > 0
                    && item.size <= MAX_MEDIA
                    && matches!(
                        item.content_type.as_str(),
                        "video/mp4" | "video/webm" | "audio/mp4" | "audio/mpeg" | "audio/ogg"
                    ),
                "unsupported cache media"
            );
        }
        let history: Vec<OfflineEvent> = match directory.open("events.json") {
            Ok(file) => serde_json::from_slice(&bounded(file, MAX_LOG)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        // Old downloaded manifests may have been removed. Validate retained
        // history against its own pinned media, never rebase it onto new facts.
        for (index, event) in history.iter().enumerate() {
            ensure!(
                offline::admit(
                    &history[..index],
                    event,
                    scope,
                    &event.media,
                    9_007_199_254_740_991,
                    MAX_EVENTS
                ) == Admission::Append,
                "invalid cached event history"
            );
        }
        Ok(Self {
            directory,
            _lock: lock,
            manifest,
            history: Mutex::new(history),
        })
    }
    fn item(&self, id: &str) -> Option<&CachedDownload> {
        self.manifest
            .downloads
            .iter()
            .find(|d| d.identity.download_id == id)
    }
    fn record(&self, event: OfflineEvent) -> anyhow::Result<serde_json::Value> {
        let item = self
            .item(&event.media.download_id)
            .context("download is unavailable")?;
        let mut history = self
            .history
            .lock()
            .map_err(|_| anyhow::anyhow!("event store failed"))?;
        match offline::admit(
            &history,
            &event,
            &self.manifest.scope,
            &item.identity,
            item.duration_ms,
            MAX_EVENTS,
        ) {
            Admission::Append => {
                let mut next = history.clone();
                next.push(event.clone());
                let bytes = serde_json::to_vec(&next)?;
                ensure!(bytes.len() as u64 <= MAX_LOG, "event storage is full");
                let name = format!("events-{}.tmp", Uuid::new_v4().simple());
                let mut file = self.directory.open_with(
                    &name,
                    cap_std::fs::OpenOptions::new().write(true).create_new(true),
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(cap_std::fs::Permissions::from_std(
                        std::fs::Permissions::from_mode(0o600),
                    ))?;
                }
                file.write_all(&bytes)?;
                file.sync_all()?;
                self.directory
                    .rename(&name, &self.directory, "events.json")?;
                self.directory.try_clone()?.into_std_file().sync_all()?;
                *history = next; // Publish in memory only after the durable rename.
            }
            Admission::Duplicate => {}
            other => anyhow::bail!("offline event rejected: {other:?}"),
        }
        Ok(
            json!({"event_id":event.event_id,"device_sequence":event.device_sequence,"next_sequence":offline::next_sequence(&history).map(|n|n.to_string())}),
        )
    }
    fn snapshot(&self, id: &str) -> anyhow::Result<(CachedDownload, tempfile::NamedTempFile)> {
        let item = self.item(id).context("download is unavailable")?.clone();
        let mut source = self.directory.open(format!("blobs/{}", item.sha256))?;
        ensure!(
            source.metadata()?.is_file() && source.metadata()?.len() == item.size,
            "download size changed"
        );
        let mut copy = tempfile::NamedTempFile::new()?;
        ensure!(
            item.size <= fs2::available_space(copy.path())?.saturating_sub(512 * 1024 * 1024),
            "insufficient space for verified playback snapshot"
        );
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            ensure!(size <= item.size, "download changed during verification");
            hash.update(&buffer[..count]);
            copy.write_all(&buffer[..count])?;
        }
        ensure!(
            size == item.size && format!("{:x}", hash.finalize()) == item.sha256,
            "download checksum mismatch"
        );
        copy.flush()?;
        copy.rewind()?;
        Ok((item, copy)) // Only the verified private snapshot is served, never the source path.
    }
}
impl OfflinePresentationReader for Cache {
    fn scope(&self) -> &CacheScope {
        &self.manifest.scope
    }
    fn downloads(&self) -> BoxFuture<'_, UiResult<Vec<CachedDownload>>> {
        Box::pin(async { Ok(self.manifest.downloads.clone()) })
    }
}

struct Media {
    ticket: String,
    item: CachedDownload,
    file: tempfile::NamedTempFile,
}
#[derive(Clone)]
pub struct Host {
    cache: Arc<Cache>,
    origin: String,
    bootstrap: Arc<Mutex<Option<String>>>,
    cookie: String,
    media: Arc<Mutex<Option<Media>>>,
    opening: Arc<tokio::sync::Mutex<()>>,
    assets: Arc<std::collections::HashMap<String, axum::body::Bytes>>,
}
impl Host {
    pub fn new(cache: Arc<Cache>, origin: String, bootstrap: String) -> Self {
        Self {
            cache,
            origin,
            bootstrap: Arc::new(Mutex::new(Some(bootstrap))),
            cookie: Uuid::new_v4().simple().to_string(),
            media: Arc::new(Mutex::new(None)),
            opening: Arc::new(tokio::sync::Mutex::new(())),
            assets: Arc::new(std::collections::HashMap::new()),
        }
    }
    pub fn with_player_assets(mut self, path: &Path) -> anyhow::Result<Self> {
        let directory = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let receipt: serde_json::Value = serde_json::from_slice(&bounded(
            directory.open("playscale-package.json")?,
            MAX_MANIFEST,
        )?)?;
        let inventory = receipt["files"]
            .as_object()
            .context("missing player inventory")?;
        ensure!(
            inventory.contains_key("web/generated/player/index.js") && inventory.len() <= 4096,
            "invalid player inventory"
        );
        let mut assets = std::collections::HashMap::new();
        let mut total = 0usize;
        for (name, digest) in inventory {
            ensure!(
                !Path::new(name).is_absolute()
                    && Path::new(name)
                        .components()
                        .all(|p| matches!(p, std::path::Component::Normal(_))),
                "invalid player asset path"
            );
            let bytes = bounded(directory.open(name)?, 64 * 1024 * 1024)?;
            total += bytes.len();
            ensure!(
                total <= 256 * 1024 * 1024,
                "player assets exceed offline host budget"
            );
            ensure!(
                digest.as_str() == Some(format!("{:x}", Sha256::digest(&bytes)).as_str()),
                "player asset checksum mismatch"
            );
            assets.insert(name.clone(), bytes.into());
        }
        self.assets = Arc::new(assets);
        Ok(self)
    }
    pub fn router(self) -> Router {
        let pages = topcoat::router::tower::TowerService::new(motion_ui::offline::router(
            self.cache.clone(),
        ));
        Router::new()
            .route("/cache/bootstrap", post(bootstrap))
            .route("/cache/open/{id}", post(open_media))
            .route("/cache/media/{ticket}", get(media))
            .route("/cache/events", post(record))
            .route("/ui/{file}", get(motion_ui::ui_asset))
            .route("/assets/demuxe/{*file}", get(player_asset))
            .fallback_service(
                Router::new()
                    .fallback_service(pages)
                    .layer(middleware::map_request(motion_ui::remote_addr)),
            )
            .layer(DefaultBodyLimit::max(16 * 1024))
            .layer(middleware::from_fn_with_state(self.clone(), guard))
            .layer(middleware::from_fn(motion_ui::presentation_headers))
            .with_state(self)
    }
}
async fn player_asset(State(host): State<Host>, AxumPath(file): AxumPath<String>) -> Response {
    match host.assets.get(&file) {
        Some(bytes) => (
            [(
                header::CONTENT_TYPE,
                mime_guess::from_path(&file)
                    .first_or_octet_stream()
                    .to_string(),
            )],
            bytes.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn guard(State(host): State<Host>, request: Request, next: Next) -> Response {
    let expected = host.origin.strip_prefix("http://").unwrap_or("");
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(expected)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = request.uri().path();
    if path.starts_with("/api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.method() != axum::http::Method::GET
        && request.method() != axum::http::Method::HEAD
        && (request
            .headers()
            .get(header::ORIGIN)
            .is_some_and(|v| v.as_bytes() != host.origin.as_bytes())
            || request
                .headers()
                .get("x-motion-cache")
                .and_then(|v| v.to_str().ok())
                != Some("1"))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if path != "/cache/bootstrap" {
        let expected = format!("motion_cache={}", host.cookie);
        if !request
            .headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').any(|v| v.trim() == expected))
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    next.run(request).await
}
#[derive(Deserialize)]
struct Bootstrap {
    credential: String,
}
async fn bootstrap(State(host): State<Host>, Json(input): Json<Bootstrap>) -> Response {
    let mut bootstrap = host.bootstrap.lock().unwrap();
    if bootstrap.as_ref() != Some(&input.credential) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    *bootstrap = None;
    (
        [(
            header::SET_COOKIE,
            format!(
                "motion_cache={}; HttpOnly; SameSite=Strict; Path=/",
                host.cookie
            ),
        )],
        Json(json!({"protocol":1,"scope":host.cache.manifest.scope})),
    )
        .into_response()
}
async fn open_media(State(host): State<Host>, AxumPath(id): AxumPath<String>) -> Response {
    let Ok(_single) = host.opening.try_lock() else {
        return StatusCode::CONFLICT.into_response();
    };
    let cache = host.cache.clone();
    let download = id.clone();
    let snapshot = tokio::task::spawn_blocking(move || cache.snapshot(&download)).await;
    let Ok(Ok((item, file))) = snapshot else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let history = host.cache.history.lock().unwrap();
    let position = history
        .iter()
        .rev()
        .find(|e| e.media == item.identity)
        .map_or(0, |e| {
            if e.status == "ended" {
                0
            } else {
                e.position_ms
            }
        });
    let ticket = Uuid::new_v4().simple().to_string();
    let result = json!({"scope":host.cache.manifest.scope,"media":item.identity,"duration_ms":item.duration_ms,"position_ms":position,
        "next_sequence":offline::next_sequence(&history).map(|n|n.to_string()),"media_url":format!("/cache/media/{ticket}")});
    *host.media.lock().unwrap() = Some(Media { ticket, item, file });
    Json(result).into_response()
}
async fn record(State(host): State<Host>, Json(event): Json<OfflineEvent>) -> Response {
    match tokio::task::spawn_blocking(move || host.cache.record(event)).await {
        Ok(Ok(ack)) => Json(ack).into_response(),
        _ => StatusCode::CONFLICT.into_response(),
    }
}
async fn media(
    State(host): State<Host>,
    AxumPath(ticket): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let opened = {
        let active = host.media.lock().unwrap();
        active
            .as_ref()
            .filter(|m| m.ticket == ticket)
            .and_then(|m| m.file.reopen().ok().map(|f| (f, m.item.clone())))
    };
    let Some((file, item)) = opened else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = format!("\"{}\"", item.sha256);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(&etag)
    {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    let range = if headers
        .get(header::IF_RANGE)
        .is_none_or(|v| v.as_bytes() == etag.as_bytes())
    {
        headers.get(header::RANGE).and_then(|v| v.to_str().ok())
    } else {
        None
    };
    let (status, start, end) = match select_range(range, item.size) {
        RangeDecision::Full => (StatusCode::OK, 0, item.size - 1),
        RangeDecision::Partial { start, end } => (StatusCode::PARTIAL_CONTENT, start, end),
        RangeDecision::Unsatisfiable => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{}", item.size))],
            )
                .into_response();
        }
    };
    let mut file = tokio::fs::File::from_std(file);
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, item.content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        .header(header::CONTENT_LENGTH, (end - start + 1).to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", item.size),
        );
    }
    response
        .body(Body::from_stream(tokio_util::io::ReaderStream::new(
            file.take(end - start + 1),
        )))
        .unwrap()
}
