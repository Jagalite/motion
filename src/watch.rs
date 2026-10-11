//! Filesystem watcher adapter. It turns native change notifications into scan
//! demands; it never publishes catalog changes itself. Relevance and debounce
//! are `playscale_core::watch`; freshness and coverage stay with the scan
//! demand rules (a source marked dirty requires a traversal that starts
//! after the change, so a scan already running when the change happened is
//! followed by a fresh one).
//!
//! Watching is best effort: roots that cannot be watched (unsupported or
//! network filesystems, permission errors) are logged and left to scheduled
//! and requested scans. Lost events count as a hint for the whole source.
use crate::App;
use notify::{
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind},
};
use playscale_core::watch::{self as core, Debounce, Hint};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How often the watched set is reconciled with registered sources.
const RESYNC: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Watched {
    /// The registered root, as watched and unwatched.
    root: PathBuf,
    /// Its canonical form: native events may report resolved paths (macOS
    /// FSEvents reports `/private/var/...` for `/var/...`).
    canonical: PathBuf,
    exclusions: Vec<String>,
}
impl Watched {
    fn relative<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.canonical)
            .or_else(|_| path.strip_prefix(&self.root))
            .ok()
    }
}

enum Observed {
    /// Paths with whether the event itself named a directory or a rename or
    /// removal of unknown kind (a removed path can no longer be inspected).
    Paths(Vec<PathBuf>, bool),
    Lost(Option<PathBuf>),
}

fn names_container(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(CreateKind::Folder)
            | EventKind::Remove(RemoveKind::Folder | RemoveKind::Any | RemoveKind::Other)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn identity(root: &Path) -> Option<String> {
    std::fs::metadata(root)
        .ok()
        .map(|m| crate::db::root_identity(&m))
}

/// Run until `shutdown`. `policy` is injectable for tests.
pub async fn worker(app: App, shutdown: CancellationToken, policy: Debounce) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Observed>();
    let mut watcher = match notify::recommended_watcher(
        move |result: notify::Result<notify::Event>| {
            let observed = match result {
                Ok(event) if event.need_rescan() => Observed::Lost(event.paths.into_iter().next()),
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
                Ok(event) => {
                    let container = names_container(&event.kind);
                    Observed::Paths(event.paths, container)
                }
                Err(error) => Observed::Lost(error.paths.into_iter().next()),
            };
            let _ = tx.send(observed);
        },
    ) {
        Ok(watcher) => watcher,
        Err(error) => {
            tracing::warn!(%error, "filesystem watching unavailable; relying on scheduled scans");
            shutdown.cancelled().await;
            return Ok(());
        }
    };
    let started = Instant::now();
    let now_ms = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut watched: BTreeMap<String, Watched> = BTreeMap::new();
    let mut registry = core::Registry::default();
    let mut pending = BTreeMap::new();
    let mut resync = tokio::time::interval(RESYNC);
    let mut tick = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = resync.tick() => {
                match sync(&app, &mut watcher, &mut registry, &mut watched).await {
                    // Changes made while a root was unwatched were not seen.
                    Ok(due) => {
                        for source in due {
                            core::record(&mut pending, &source, now_ms());
                        }
                    }
                    Err(error) => tracing::warn!(%error, "could not refresh watched sources"),
                }
                let ids: Vec<String> = watched.keys().cloned().collect();
                core::retain_watched(&mut pending, &ids);
            }
            Some(observed) = rx.recv() => {
                for (source, hint) in attribute(&watched, observed) {
                    if core::relevant(&hint, &watched[&source].exclusions) {
                        core::record(&mut pending, &source, now_ms());
                    }
                }
            }
            _ = tick.tick() => {
                for source in core::take_due(&mut pending, now_ms(), policy) {
                    if let Err(error) = demand(&app, &source).await {
                        // Keep the hint until a scan is admitted; it is due
                        // again after the quiet period.
                        tracing::warn!(%error, source, "could not request a scan after a change; will retry");
                        core::record(&mut pending, &source, now_ms());
                    }
                }
            }
        }
    }
}

/// Map native paths to `(source, hint)`. A path under no watched root is
/// ignored; a lost event without a path applies to every source.
fn attribute(watched: &BTreeMap<String, Watched>, observed: Observed) -> Vec<(String, Hint)> {
    let owner = |path: &Path| {
        watched
            .iter()
            .filter_map(|(id, w)| Some((id, w, w.relative(path)?)))
            .max_by_key(|(_, w, _)| w.canonical.components().count())
            .map(|(id, w, relative)| (id.clone(), w, relative.to_path_buf()))
    };
    match observed {
        Observed::Lost(None) => watched.keys().map(|id| (id.clone(), Hint::Lost)).collect(),
        Observed::Lost(Some(path)) => owner(&path)
            .map(|(id, ..)| (id, Hint::Lost))
            .into_iter()
            .collect(),
        Observed::Paths(paths, named_container) => paths
            .iter()
            .filter_map(|path| {
                let (id, _, relative) = owner(path)?;
                let parts: Option<Vec<&str>> = relative
                    .components()
                    .map(|c| c.as_os_str().to_str())
                    .collect();
                // A non-UTF-8 path is not catalogable, but its directory may
                // be: treat it as a hint for the source.
                let container = named_container || path.is_dir();
                Some((
                    id,
                    parts.map_or(Hint::Lost, |p| Hint::Path {
                        path: p.join("/"),
                        container,
                    }),
                ))
            })
            .collect(),
    }
}

/// Reconcile watches with enabled, non-managed source roots through the core
/// registry, which decides what to (un)watch and which sources are owed a
/// catch-up scan after a lost watch. Returns the sources whose catch-up is
/// due now (their watch was just re-established).
async fn sync(
    app: &App,
    watcher: &mut RecommendedWatcher,
    registry: &mut core::Registry,
    watched: &mut BTreeMap<String, Watched>,
) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,root_path,exclusions_json FROM sources WHERE enabled=1 ORDER BY id",
    )
    .fetch_all(&app.db)
    .await?;
    let mut details = BTreeMap::new();
    let mut wanted = BTreeMap::new();
    for (id, root, exclusions) in rows {
        let exclusions: Vec<String> = serde_json::from_str(&exclusions)?;
        let path = PathBuf::from(&root);
        let current = identity(&path);
        wanted.insert(id.clone(), (root, current.clone()));
        details.insert(id, (path, exclusions, current));
    }
    let mut due = Vec::new();
    for step in registry.plan(&wanted) {
        match step {
            core::Step::Unwatch { id, root } => {
                let _ = watcher.unwatch(Path::new(&root));
                watched.remove(&id);
            }
            core::Step::Watch { id, root } => {
                let (path, exclusions, current) = details[&id].clone();
                let result = watcher.watch(&path, RecursiveMode::Recursive);
                if let Err(error) = &result {
                    tracing::warn!(%error, source = id, "source root cannot be watched; will retry");
                }
                if registry.registered(&id, &root, current, result.is_ok()) {
                    due.push(id.clone());
                }
                if result.is_ok() {
                    let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                    watched.insert(
                        id,
                        Watched {
                            root: path,
                            canonical,
                            exclusions,
                        },
                    );
                }
            }
        }
    }
    // Exclusions can change without re-registration.
    for (id, (_, exclusions, _)) in details {
        if let Some(entry) = watched.get_mut(&id) {
            entry.exclusions = exclusions;
        }
    }
    Ok(due)
}

/// Mark the source dirty and request a scan of it through the demand rules,
/// on behalf of the first library that includes it. A source in no library
/// has nobody to serve; it is only marked dirty.
async fn demand(app: &App, source: &str) -> anyhow::Result<()> {
    crate::scans::mark_dirty(app, source)
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let library: Option<String> = sqlx::query_scalar(
        "SELECT library_id FROM library_sources WHERE source_id=? ORDER BY library_id LIMIT 1",
    )
    .bind(source)
    .fetch_optional(&app.db)
    .await?;
    if let Some(library) = library {
        crate::scans::request(app, &library, Some(&[source.to_string()]), false, false)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    }
    Ok(())
}
