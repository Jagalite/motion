//! Accessibility audit support (completion 6D): serve the presentation with
//! the self-identifying mock facade as an administrator, so an external
//! auditor (`scripts/a11y/audit.mjs`, axe-core in Electron) can load every
//! page with its real stylesheet. Does nothing unless `MOTION_A11Y_ADDR`
//! names a loopback address; serves for `MOTION_A11Y_SECONDS` (default 300).

use std::sync::Arc;

use axum::{Router, extract::Request, middleware::Next};
use motion_ui::{UiPrincipal, facade::ProfileOption, mock::MockUiQueryFacade};

#[tokio::test]
async fn serve_for_accessibility_audit() {
    let Ok(addr) = std::env::var("MOTION_A11Y_ADDR") else {
        return;
    };
    assert!(addr.starts_with("127.0.0.1:"), "loopback only");
    let seconds: u64 = std::env::var("MOTION_A11Y_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let who = UiPrincipal {
        principal_id: "p1".into(),
        profile_id: "everyone".into(),
        profile_name: "Everyone".into(),
        profiles: vec![ProfileOption {
            id: "everyone".into(),
            name: "Everyone".into(),
        }],
        permissions: [
            "catalog:read",
            "catalog:write",
            "sources:manage",
            "profiles:manage",
            "viewing:write",
            "playback:request",
            "processing:request",
            "collections:write",
            "downloads:manage",
            "system:admin",
        ]
        .iter()
        .map(|p| p.to_string())
        .collect(),
        server_epoch: "epoch1".into(),
        viewing_epoch: "server:restore".into(),
        csrf_token: "csrf-abc".into(),
    };
    let app = motion_ui::mount(Router::new(), Arc::new(MockUiQueryFacade), None).layer(
        axum::middleware::from_fn(move |mut request: Request, next: Next| {
            let who = who.clone();
            async move {
                request.extensions_mut().insert(who);
                next.run(request).await
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    // The offline pages too, over one saved title, when asked.
    let offline = std::env::var("MOTION_A11Y_OFFLINE_ADDR").ok().map(|addr| {
        assert!(addr.starts_with("127.0.0.1:"), "loopback only");
        let pages = topcoat::router::tower::TowerService::new(motion_ui::offline::router(
            Arc::new(SavedTitle(scope())),
        ));
        let app = Router::new()
            .route("/ui/{file}", axum::routing::get(motion_ui::ui_asset))
            .fallback_service(
                Router::new()
                    .fallback_service(pages)
                    .layer(axum::middleware::map_request(motion_ui::remote_addr)),
            )
            .layer(axum::middleware::from_fn(motion_ui::presentation_headers));
        async move {
            let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
            axum::serve(listener, app).await
        }
    });
    let serve = async move {
        match offline {
            Some(offline) => {
                let _ = tokio::join!(axum::serve(listener, app), offline);
            }
            None => {
                let _ = axum::serve(listener, app).await;
            }
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(seconds), serve).await;
}

fn scope() -> playscale_core::offline::CacheScope {
    playscale_core::offline::CacheScope {
        server_id: "server".into(),
        principal_id: "device".into(),
        profile_id: "everyone".into(),
        device_id: "device".into(),
    }
}

/// One saved title with its poster and metadata.
struct SavedTitle(playscale_core::offline::CacheScope);
impl motion_ui::offline::OfflinePresentationReader for SavedTitle {
    fn scope(&self) -> &playscale_core::offline::CacheScope {
        &self.0
    }
    fn downloads(
        &self,
    ) -> motion_ui::facade::BoxFuture<
        '_,
        motion_ui::facade::UiResult<Vec<motion_ui::offline::CachedDownload>>,
    > {
        Box::pin(async {
            Ok(vec![motion_ui::offline::CachedDownload {
                identity: playscale_core::offline::DownloadIdentity {
                    download_id: "saved".into(),
                    timeline_id: "timeline".into(),
                    timeline_revision: "1".into(),
                    source_revision: "0".repeat(64),
                    base_viewing_revision: "0".into(),
                    base_manual_epoch: "0".into(),
                    viewing_epoch: Some("server:restore".into()),
                },
                title: "Arrival of the Night Train".into(),
                duration_ms: 5_400_000,
                sha256: "0".repeat(64),
                size: 1,
                content_type: "video/mp4".into(),
                resume_position_ms: None,
                artwork: Some(motion_ui::offline::CachedArtwork {
                    sha256: "1".repeat(64),
                    size: 1,
                    content_type: "image/png".into(),
                }),
                metadata: Some(playscale_core::offline::OfflineMetadata {
                    release_year: Some(1999),
                    description: Some("A night train arrives somewhere unexpected.".into()),
                    genres: vec!["Drama".into()],
                }),
            }])
        })
    }
}
