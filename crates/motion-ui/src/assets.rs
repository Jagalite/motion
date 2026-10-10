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
    include_bytes!("../../../packages/ui-bridge/player.js"),
);

/// Every embedded asset, for routing and the release inventory.
pub fn all() -> [&'static Asset; 3] {
    [&STYLESHEET, &BRIDGE, &PLAYER]
}

/// Resolve a request under `/ui/` to an embedded asset by exact hashed name.
pub fn lookup(file_name: &str) -> Option<&'static Asset> {
    all()
        .into_iter()
        .find(|asset| asset.file_name() == file_name)
}
