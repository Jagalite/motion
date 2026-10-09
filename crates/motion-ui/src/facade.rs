//! `UiQueryFacade`: the only data source for Topcoat pages (plan section 4.1).
//!
//! Pages receive bounded, presentation-safe view models for the request's
//! authenticated principal and profile. The facade must delegate to the same
//! authorized read use cases as the public API; it never exposes SQL, service
//! handles, source paths or hidden metadata, and loading a view has no
//! persistent side effects (rendering may be repeated or prefetched).

use std::{future::Future, pin::Pin, sync::Arc};

use serde::Serialize;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The authenticated request identity, inserted into request extensions by the
/// host's authentication layer before Topcoat dispatch. Topcoat never creates it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiPrincipal {
    pub principal_id: String,
    pub profile_id: String,
    pub profile_name: String,
    pub profiles: Vec<ProfileOption>,
    pub permissions: Vec<String>,
    /// Server runtime epoch; caches keyed without it must not survive a restart/restore.
    pub server_epoch: String,
    /// CSRF token for this browser session, embedded for the external command module.
    pub csrf_token: String,
}

impl UiPrincipal {
    pub fn can(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileOption {
    pub id: String,
    pub name: String,
}

/// Why a view could not be produced. Not-visible and nonexistent are the same
/// error so that restricted titles are not disclosed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiError {
    NotFound,
    Denied,
    Unavailable,
}

pub type UiResult<T> = Result<T, UiError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable,
    Unknown,
    Degraded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryCard {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemCard {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub availability: Availability,
    pub needs_review: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinueCard {
    pub item: ItemCard,
    pub timeline_id: String,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeView {
    pub continue_watching: Vec<ContinueCard>,
    pub libraries: Vec<LibraryCard>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryView {
    pub library: LibraryCard,
    pub items: Vec<ItemCard>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionView {
    pub id: String,
    pub label: String,
    pub availability: Availability,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineView {
    pub id: String,
    pub edition: String,
    pub duration_ms: Option<u64>,
    pub position_ms: u64,
    pub watched: bool,
    pub versions: Vec<VersionView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemView {
    pub item: ItemCard,
    pub children: Vec<ItemCard>,
    pub timelines: Vec<TimelineView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchView {
    pub query: String,
    pub items: Vec<ItemCard>,
}

/// Everything the player page needs to emit a stable player host. Playback
/// itself (plan, admission, authority, progress) is the external coordinator's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerView {
    pub item_id: String,
    pub title: String,
    pub timeline_id: String,
    pub duration_ms: Option<u64>,
    pub resume_ms: u64,
    pub viewing_revision: String,
}

/// Read-only, principal-scoped presentation queries.
pub trait UiQueryFacade: Send + Sync + 'static {
    /// True for development/test doubles; pages then label themselves.
    fn is_mock(&self) -> bool;
    fn home<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<HomeView>>;
    fn library<'a>(&'a self, who: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<LibraryView>>;
    fn item<'a>(&'a self, who: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<ItemView>>;
    fn search<'a>(&'a self, who: &'a UiPrincipal, query: &'a str) -> BoxFuture<'a, UiResult<SearchView>>;
    fn player<'a>(&'a self, who: &'a UiPrincipal, timeline_id: &'a str) -> BoxFuture<'a, UiResult<PlayerView>>;
}

/// The facade as stored in Topcoat's app context.
#[derive(Clone)]
pub struct Facade(pub Arc<dyn UiQueryFacade>);
