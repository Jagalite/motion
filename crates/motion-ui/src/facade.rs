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
    /// Server identity plus restore epoch; stable across ordinary restarts.
    pub viewing_epoch: String,
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
    /// Same-origin URL of the selected poster, when one is selected and visible.
    pub artwork_url: Option<String>,
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
pub struct TimelineViewing {
    pub position_ms: u64,
    pub watched: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineView {
    pub id: String,
    pub edition: String,
    pub duration_ms: Option<u64>,
    /// None means the viewing service is unavailable, not an unstarted title.
    pub viewing: Option<TimelineViewing>,
    pub versions: Vec<VersionView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemView {
    pub playback_available: bool,
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
    /// Manual watched epoch at render; a session start never adopts a newer one.
    pub viewing_manual_epoch: String,
    /// Audio streams of `audio_version`'s original file, by planning track ID
    /// (`a{n}`). Choosing one other than the first asks for a conversion.
    pub audio_tracks: Vec<TrackOption>,
    pub audio_version: Option<String>,
    /// The exact file revision `audio_tracks` describe; a chosen track pins it.
    pub audio_file: Option<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackOption {
    pub id: String,
    pub label: String,
}

/// Profile preferences as shown and edited on the profiles page. `etag` is the
/// strong validator the command must send back as If-Match.
#[derive(Clone, Debug, PartialEq)]
pub struct PreferencesView {
    pub audio_languages: Vec<String>,
    pub subtitle_languages: Vec<String>,
    /// off | forced | always | foreign_audio
    pub subtitle_mode: String,
    /// auto | original | convert
    pub quality_mode: String,
    pub allow_client_software_decode: bool,
    pub autoplay: bool,
    pub completion_percent: f64,
    pub etag: String,
}

#[derive(Clone, Debug, PartialEq)]
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
    /// Strong validator of the source a relocation preview is checked against.
    pub etag: String,
}

/// A library as administered on the sources page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryAdminRow {
    pub library: LibraryCard,
    pub language: String,
    pub source_ids: Vec<String>,
    /// Strong validator the edit form sends as If-Match.
    pub etag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRow {
    pub id: String,
    pub library_id: String,
    pub library_name: String,
    /// scan | metadata_refresh | preview | optimize
    pub task: String,
    pub interval_seconds: u64,
    pub enabled: bool,
    pub etag: String,
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
    /// A missing scan API must not appear as an empty scan history.
    pub scans_available: bool,
    pub sources: Vec<SourceRow>,
    pub libraries: Vec<LibraryAdminRow>,
    pub scans: Vec<ScanRow>,
    /// None when schedules cannot be read; never shown as "no schedules".
    pub schedules: Option<Vec<ScheduleRow>>,
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

/// The editable "local" metadata contribution of one item, and where each
/// effective field currently comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataEditView {
    /// Values of the local contribution only (what the form edits).
    pub title: String,
    pub description: String,
    pub release_year: String,
    /// Local values the form does not edit, as a JSON object; sent back
    /// unchanged because a save replaces the whole contribution.
    pub preserved_values: String,
    pub tags: Vec<String>,
    pub excluded_tags: Vec<String>,
    /// Effective field → contributing sources, e.g. ("title", ["local"]).
    pub field_sources: Vec<(String, Vec<String>)>,
    /// Effective values as displayed: field → JSON text.
    pub effective: Vec<(String, String)>,
    /// Validator of the whole metadata resource (If-Match for the edit).
    pub etag: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtworkRow {
    pub id: String,
    pub content_url: String,
    pub mime_type: String,
    pub selected: bool,
}

/// One media file of the item, as an exact revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub file_id: String,
    pub file_revision: String,
    pub library_id: String,
    pub label: String,
    pub availability: Availability,
    /// Audio streams as (planning track ID `a{n}`, label).
    pub audio_tracks: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeOption {
    pub id: String,
    pub name: String,
    pub backends: Vec<String>,
    /// unqualified | partial | qualified
    pub qualification: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenditionRow {
    pub id: String,
    /// pending | validating | ready | stale | failed | expired
    pub status: String,
    pub job_id: String,
    pub source_file_id: String,
}

/// A timeline's preparable originals and its existing renditions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrepareTimeline {
    pub id: String,
    pub edition: String,
    /// Original files bound to this timeline (part 1 of each original version).
    pub sources: Vec<FileRow>,
    /// None when the renditions read is unavailable (not "none prepared").
    pub renditions: Option<Vec<RenditionRow>>,
}

/// A collection or playlist of the current profile, and the exact command
/// bodies that would add or remove this item (precomputed, If-Match guarded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipRow {
    pub id: String,
    pub name: String,
    pub etag: String,
    pub contains: bool,
    /// Full replacement JSON for the toggle (adds when !contains, else removes).
    pub toggle_body: String,
}

/// A title that could be merged into the one being managed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeCandidate {
    pub id: String,
    pub title: String,
    pub revision: String,
}

/// One version of a title's timelines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionRow {
    pub id: String,
    pub label: String,
}

/// An explicit play order (aired, DVD, absolute, custom) of one edition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderGroupView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub edition: String,
    /// The group revision a placement is fenced with.
    pub revision: String,
}

/// One of the title's timelines and its current place in an order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedTimeline {
    pub id: String,
    pub edition: String,
    pub etag: String,
    pub group_id: Option<String>,
    pub position: Option<u64>,
}

/// The title's explicit play orders and where its timelines are placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderingView {
    /// (edition ID, label) an order can be created for.
    pub editions: Vec<(String, String)>,
    pub groups: Vec<OrderGroupView>,
    pub timelines: Vec<OrderedTimeline>,
}

/// Everything the title management page shows. Sections are `None` when the
/// principal lacks the permission or the read is unavailable; the page says which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemManageView {
    pub item: ItemCard,
    /// The item's catalog revision (merge and split plans are fenced by it).
    pub item_revision: String,
    /// Other readable titles with this one's title, as merge sources.
    pub merge_candidates: Option<Vec<MergeCandidate>>,
    /// Every version of the item's timelines, as split choices.
    pub versions: Option<Vec<VersionRow>>,
    pub library_ids: Vec<String>,
    pub match_state: String,
    pub item_etag: String,
    pub metadata: Option<MetadataEditView>,
    pub artwork: Option<Vec<ArtworkRow>>,
    pub files: Option<Vec<FileRow>>,
    pub recipes: Option<Vec<RecipeOption>>,
    pub timelines: Option<Vec<PrepareTimeline>>,
    pub collections: Option<Vec<MembershipRow>>,
    pub playlists: Option<Vec<MembershipRow>>,
    /// Play orders (catalog:write).
    pub ordering: Option<OrderingView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionView {
    pub id: String,
    pub name: String,
    /// manual | smart
    pub kind: String,
    /// The saved filter a smart collection follows.
    pub filter_id: Option<String>,
    pub etag: String,
    pub items: Vec<ItemCard>,
    /// Exact replacement bodies removing each listed item, in `items` order.
    pub remove_bodies: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistEntryView {
    pub entry_id: String,
    pub timeline_id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistView {
    pub id: String,
    pub name: String,
    pub etag: String,
    pub entries: Vec<PlaylistEntryView>,
    /// Exact replacement bodies per entry: (remove, move up), in `entries` order.
    pub entry_bodies: Vec<(String, Option<String>)>,
}

/// One condition of a saved filter, as the API stores it (`FilterTerm`); the
/// value is rendered as text (`true`, `1990`, `movie`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterTermView {
    pub field: String,
    pub operator: String,
    pub value: String,
}

/// A saved filter of the current profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedFilterView {
    pub id: String,
    pub name: String,
    pub etag: String,
    pub terms: Vec<FilterTermView>,
    /// A smart collection uses it (the server refuses deleting it).
    pub in_use: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrganizeView {
    pub profile_id: String,
    pub collections: Vec<CollectionView>,
    pub playlists: Vec<PlaylistView>,
    pub filters: Vec<SavedFilterView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub kind: String,
    pub phase: String,
    pub progress_percent: Option<u8>,
    pub error_code: Option<String>,
    /// Validator a retry is checked against.
    pub etag: String,
}

/// A server download demand of this device. Transfer progress is device-owned
/// and shown by the desktop host, not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRow {
    pub id: String,
    pub title: String,
    /// ready | cancelled | expired
    pub state: String,
    pub size_bytes: u64,
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
    /// Title management; requires at least one of `catalog:write`,
    /// `processing:request` or `collections:write`. Sections the principal
    /// may not use are `None`.
    fn manage_item<'a>(
        &'a self,
        who: &'a UiPrincipal,
        id: &'a str,
    ) -> BoxFuture<'a, UiResult<ItemManageView>>;
    /// The current profile's collections and playlists (`catalog:read`).
    fn organize<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<OrganizeView>>;
    fn downloads<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<Vec<DownloadRow>>>;
    /// Requires `system:admin`.
    fn diagnostics<'a>(&'a self, who: &'a UiPrincipal) -> BoxFuture<'a, UiResult<DiagnosticsView>>;
}

/// The facade as stored in Topcoat's app context.
#[derive(Clone)]
pub struct Facade(pub Arc<dyn UiQueryFacade>);
