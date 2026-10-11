//! Presentation assets: Motion's CSS and the prebuilt external bridge modules,
//! embedded in the binary and served from content-hashed URLs under `/ui/`.
//!
//! Demuxe is deliberately NOT here. It is a separate immutable asset tree
//! mounted unchanged at `DEMUXE_BASE` so its relative worker/provider imports
//! and installed-file receipts stay valid (plan section 14.5).

use sha2::{Digest, Sha256};

pub const UI_BASE: &str = "/ui/";
pub const DEMUXE_BASE: &str = "/assets/demuxe/";

pub struct Asset {
    stem: &'static str,
    extension: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

impl Asset {
    const fn new(
        stem: &'static str,
        extension: &'static str,
        content_type: &'static str,
        bytes: &'static [u8],
    ) -> Self {
        Self {
            stem,
            extension,
            content_type,
            bytes,
        }
    }

    /// `/ui/<stem>.<12 hex of sha256>.<ext>`; changes whenever the bytes do.
    pub fn url(&self) -> String {
        format!("{UI_BASE}{}", self.file_name())
    }

    pub fn file_name(&self) -> String {
        let digest = Sha256::digest(self.bytes);
        let hex: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
        format!("{}.{hex}.{}", self.stem, self.extension)
    }

    pub fn sha256(&self) -> String {
        Sha256::digest(self.bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

pub static STYLESHEET: Asset = Asset::new(
    "motion",
    "css",
    "text/css; charset=utf-8",
    include_bytes!("../../../ui-assets/motion.css"),
);
pub static BRIDGE: Asset = Asset::new(
    "bridge",
    "js",
    "text/javascript; charset=utf-8",
    include_bytes!("../../../packages/ui-bridge/bridge.js"),
);
pub static PLAYER: Asset = Asset::new(
    "player",
    "js",
    "text/javascript; charset=utf-8",
    // One prebuilt ES module: pure coordinator followed by its DOM adapter.
    concat!(
        include_str!("../../../packages/playback/viewing.mjs"),
        "\n",
        include_str!("../../../packages/playback/generation.mjs"),
        "\n",
        include_str!("../../../packages/playback/open.mjs"),
        "\n",
        include_str!("../../../packages/ui-bridge/player.js")
    )
    .as_bytes(),
);

pub static OFFLINE_PLAYER: Asset = Asset::new(
    "offline-player",
    "js",
    "text/javascript; charset=utf-8",
    include_bytes!("../../../packages/ui-bridge/offline-player.js"),
);

// Home-screen icons: regenerate with scripts/generate_motion_icons.py.
pub static TOUCH_ICON: Asset = Asset::new(
    "icon-180",
    "png",
    "image/png",
    include_bytes!("../../../ui-assets/icons/icon-180.png"),
);
pub static ICON_192: Asset = Asset::new(
    "icon-192",
    "png",
    "image/png",
    include_bytes!("../../../ui-assets/icons/icon-192.png"),
);
pub static ICON_512: Asset = Asset::new(
    "icon-512",
    "png",
    "image/png",
    include_bytes!("../../../ui-assets/icons/icon-512.png"),
);

/// Every embedded asset, for routing and the release inventory.
pub fn all() -> [&'static Asset; 7] {
    [
        &STYLESHEET,
        &BRIDGE,
        &PLAYER,
        &OFFLINE_PLAYER,
        &TOUCH_ICON,
        &ICON_192,
        &ICON_512,
    ]
}

/// The web app manifest lives at a stable URL because browsers re-check it
/// there to update an installed app; it names the hashed icons of this build.
pub const MANIFEST_URL: &str = "/ui/manifest.webmanifest";

/// Installable app metadata: launches standalone (no browser chrome) at Home.
pub fn manifest() -> String {
    let icon = |asset: &Asset, size: &str| serde_json::json!({"src": asset.url(), "sizes": size, "type": "image/png", "purpose": "any maskable"});
    serde_json::json!({
        "name": "Motion",
        "short_name": "Motion",
        "id": "/",
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "orientation": "any",
        "background_color": "#1f2326",
        "theme_color": "#191c1f",
        "icons": [icon(&ICON_192, "192x192"), icon(&ICON_512, "512x512")],
    })
    .to_string()
}

/// Resolve a request under `/ui/` to an embedded asset by exact hashed name.
pub fn lookup(file_name: &str) -> Option<&'static Asset> {
    all()
        .into_iter()
        .find(|asset| asset.file_name() == file_name)
}
