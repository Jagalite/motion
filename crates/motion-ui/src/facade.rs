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
    pub continue_available: bool,
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

/// Profile preferences as shown and edited on the profiles page. `etag` is the
/// strong validator the command must send back as If-Match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreferencesView {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    /// off | forced | always | foreign_audio
    pub subtitle_mode: String,
    /// auto | original | convert
    pub quality_mode: String,
    pub allow_client_software_decode: bool,
    pub autoplay: bool,
    pub completion_percent: u8,
    pub etag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilesView {
    pub profiles: Vec<ProfileOption>,
    pub current: String,
    pub preferences: PreferencesView,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    /// Host path of an administrative source; only shown to `sources:manage`.
    pub root_path: String,
    pub availability: Availability,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanSourceRow {
    pub source_name: String,
    /// queued | running | complete | partial | unavailable | stale | cancelled | failed
    pub status: String,
    pub observed_files: u64,
    pub complete_directories: u64,
    pub incomplete_directories: u64,
    pub error_codes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRow {
    pub id: String,
    pub library_name: String,
    /// queued | running | complete | partial | unavailable | cancelled | failed
    pub status: String,
    pub started: String,
    pub sources: Vec<ScanSourceRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcesView {
    pub sources: Vec<SourceRow>,
    pub libraries: Vec<LibraryCard>,
    pub scans: Vec<ScanRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchCandidateView {
    pub id: String,
    pub title: String,
    pub confidence_percent: Option<u8>,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchRow {
    pub id: String,
    pub subject: String,
    /// pending | review | accepted | rejected | deferred | stale
    pub status: String,
    pub etag: String,
    pub candidates: Vec<MatchCandidateView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub kind: String,
    pub phase: String,
    pub progress_percent: Option<u8>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticsView {
    pub server_id: String,
    pub server_version: String,
    pub api_version: String,
    pub schema_version: String,
    pub health: String,
    pub uptime_seconds: u64,
    pub active_deliveries: u64,
    pub queued_jobs: u64,
    pub running_jobs: u64,
    pub worker_errors: Vec<String>,
}

/// Read-only, principal-scoped presentation queries.
pub trait UiQueryFacade: Send + Sync + 'static {
    /// True for development/test doubles; pages then label themselves.
    fn is_mock(&self) -> bool;
    fn home<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<HomeView>>;
    fn library<'a>(
        &'a self,
        who: &'a UiPrincipal,
        id: &'a str,
    ) -> BoxFuture<'a, UiResult<LibraryView>>;
    fn item<'a>(&'a self, who: &'a UiPrincipal, id: &'a str) -> BoxFuture<'a, UiResult<ItemView>>;
    fn search<'a>(
        &'a self,
        who: &'a UiPrincipal,
        query: &'a str,
    ) -> BoxFuture<'a, UiResult<SearchView>>;
    fn player<'a>(
        &'a self,
        who: &'a UiPrincipal,
        timeline_id: &'a str,
    ) -> BoxFuture<'a, UiResult<PlayerView>>;
    fn profiles<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<ProfilesView>>;
    /// Requires `sources:manage`.
    fn sources<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<SourcesView>>;
    /// Requires `catalog:write`.
    fn matches<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<MatchRow>>>;
    /// Requires `processing:request`.
    fn jobs<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<JobRow>>>;
    /// Requires `system:admin`.
    fn diagnostics<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<DiagnosticsView>>;
}

/// The facade as stored in Topcoat's app context.
#[derive(Clone)]
pub struct Facade(pub Arc<dyn UiQueryFacade>);
