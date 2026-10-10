//! A13 cache port and reusable Topcoat download views. The reader supplies
//! verified cache metadata only; no catalog service or database is reachable.
use crate::{
    assets,
    facade::{BoxFuture, UiResult},
};
use playscale_core::offline::{CacheScope, DownloadIdentity};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{Router, Slot, layout, page, path_param},
    view::{View, view},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedDownload {
    pub identity: DownloadIdentity,
    pub title: String,
    pub duration_ms: u64,
    pub sha256: String,
    pub size: u64,
    pub content_type: String,
}

pub trait OfflinePresentationReader: Send + Sync + 'static {
    fn scope(&self) -> &CacheScope;
    fn downloads(&self) -> BoxFuture<'_, UiResult<Vec<CachedDownload>>>;
}

#[derive(Clone)]
pub struct OfflineFacade(pub Arc<dyn OfflinePresentationReader>);

pub fn router(reader: Arc<dyn OfflinePresentationReader>) -> Router {
    Router::builder()
        .app_context(OfflineFacade(reader))
        .layout(shell)
        .page(downloads)
        .page(player)
        .build()
}

path_param!(download_id);

#[layout("/")]
async fn shell(slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
        <title>"Motion downloads"</title><link rel="stylesheet" href=(assets::STYLESHEET.url())><script type="module" src=(assets::OFFLINE_PLAYER.url())></script></head>
        <body><a class="skip-link" href="#main">"Skip to content"</a><header class="app-header"><a class="brand" href="/">"Downloads"</a><span>"Offline"</span></header>
        <main id="main" tabindex="-1">(slot)</main></body></html>
    })
}

#[page("/")]
async fn downloads(cx: &Cx) -> Result<impl View> {
    let reader = &app_context::<OfflineFacade>(cx).0;
    let downloads = reader.downloads().await.map_err(crate::app::ui_error)?;
    let scope = reader.scope();
    Ok(view! {
        <h1>"Downloaded titles"</h1><p>"Progress stays on this device until it is reconciled with your server. A later manual change on the server may take precedence."</p>
        <p>"Server: " (scope.server_id.as_str()) " · Profile: " (scope.profile_id.as_str())</p>
        if downloads.is_empty() { <p class="status-panel">"No verified downloads are available for this profile."</p> }
        <ul class="grid" aria-label="Downloads">
        for item in downloads { <li class="card"><a href=(format!("/download/{}",item.identity.download_id))><span class="card-title">(item.title)</span><span class="card-meta">"Saved on this device"</span></a></li> }
        </ul>
    })
}

#[page("/download/{download_id}")]
async fn player(cx: &Cx) -> Result<impl View> {
    let id = path_param::<DownloadId>(cx);
    let item = app_context::<OfflineFacade>(cx)
        .0
        .downloads()
        .await
        .map_err(crate::app::ui_error)?
        .into_iter()
        .find(|item| item.identity.download_id == id)
        .ok_or_else(topcoat::router::error::not_found)?;
    Ok(view! {
        <h1>(item.title)</h1>
        <div id="motion-offline-player" class="player-host" data-download-id=(item.identity.download_id) data-demuxe-base=(assets::DEMUXE_BASE)>
            <p class="status-panel" role="status">"Verifying saved media…"</p>
        </div>
        <p id="offline-progress" role="status">"Playback progress is saved locally."</p>
        <p><a href="/">"Back to downloads"</a></p>
    })
}
