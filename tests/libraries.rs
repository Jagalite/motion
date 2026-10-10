//! Logical libraries over storage sources: registration, overlap, exclusions,
//! availability and the binding revision recorded by scan attempts.
use playscale::{App, administration, db, libraries, scan};
use playscale_core::sources::{Availability, LibraryKind, SourceError};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let app = App {
            access: Arc::new(playscale::v2::Runtime::new(
                playscale_core::access::AccessMode::TrustedHousehold,
                playscale::v2::auth::random_key(),
            )),
            health: Arc::new(playscale::operations::Health::new(false)),
            db,
            admin_token: Arc::new("test-secret-token".into()),
            origin: Arc::new("http://127.0.0.1:8787".into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(2)),
            event_streams: Arc::new(Semaphore::new(2)),
            storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
            processing: Arc::new(playscale::processing::Runtime::new(
                dir.path().join("cache"),
                Default::default(),
            )),
        };
        Self { dir, app }
    }
    fn root(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    async fn source(&self, name: &str, exclusions: &[&str]) -> libraries::SourceView {
        let candidate = administration::candidate_root(&self.app, self.root(name))
            .await
            .unwrap();
        let exclusions: Vec<String> = exclusions.iter().map(|s| s.to_string()).collect();
        libraries::register_source(&self.app, candidate, name, &exclusions)
            .await
            .unwrap()
    }
    async fn scan(&self, source: &str) -> db::JobRow {
        let row = db::enqueue_mode(&self.app, source, true).await.unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(scan::worker(self.app.clone(), stop.clone()));
        let done = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let row = db::get_job(&self.app.db, &row.id).await.unwrap();
                if !["queued", "running", "cancelling"].contains(&row.phase.as_str()) {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        task.await.unwrap().unwrap();
        done
    }
}

#[tokio::test]
async fn libraries_span_sources_and_nested_roots_are_rejected() {
    let f = Fixture::new().await;
    let movies = f.source("movies", &[]).await;
    let more = f.source("more-movies", &[]).await;
    assert_eq!(movies.availability, Availability::Unknown);
    // A root inside (or around) a registered source would observe files twice.
    let nested = administration::candidate_root(&f.app, f.root("movies/4k"))
        .await
        .unwrap();
    assert!(matches!(
        libraries::register_source(&f.app, nested, "4k", &[]).await,
        Err(libraries::LibraryError::Overlaps(_))
    ));
    let library = libraries::save_library(
        &f.app,
        None,
        "Movies",
        LibraryKind::Movies,
        "en-US",
        &[movies.id.clone(), more.id.clone()],
    )
    .await
    .unwrap();
    assert_eq!(library.source_ids.len(), 2);
    assert_eq!(library.availability, Availability::Unknown);
    assert!(matches!(
        libraries::save_library(
            &f.app,
            Some((&library.id, 0)),
            "Movies",
            LibraryKind::Movies,
            "",
            &[]
        )
        .await,
        Err(libraries::LibraryError::Rejected(
            SourceError::StaleRevision
        ))
    ));
    assert!(matches!(
        libraries::save_library(
            &f.app,
            None,
            "Bad",
            LibraryKind::Mixed,
            "",
            &["missing".into()]
        )
        .await,
        Err(libraries::LibraryError::Rejected(
            SourceError::UnknownSource(_)
        ))
    ));

    // One source scanned completely, the other never: degraded.
    std::fs::write(f.root("movies").join("a.mp4"), b"a").unwrap();
    let job = f.scan(&movies.id).await;
    assert_eq!(job.binding_revision, Some(movies.binding_revision as i64));
    let library = libraries::get_library(&f.app.db, &library.id)
        .await
        .unwrap();
    assert_eq!(library.availability, Availability::Degraded);
    f.scan(&more.id).await;
    let library = libraries::get_library(&f.app.db, &library.id)
        .await
        .unwrap();
    assert_eq!(library.availability, Availability::Available);
    let item: String = sqlx::query_scalar("SELECT item_id FROM catalog_files LIMIT 1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        libraries::libraries_of_item(&f.app.db, &item)
            .await
            .unwrap(),
        std::slice::from_ref(&library.id)
    );
}

#[tokio::test]
async fn exclusions_narrow_scope_without_inferring_absence() {
    let f = Fixture::new().await;
    let source = f.source("media", &[]).await;
    let root = f.root("media");
    std::fs::create_dir_all(root.join("Extras")).unwrap();
    std::fs::write(root.join("film.mp4"), b"film").unwrap();
    std::fs::write(root.join("Extras/trailer.mp4"), b"trailer").unwrap();
    f.scan(&source.id).await;
    let source =
        libraries::set_exclusions(&f.app, &source.id, source.revision, &["Extras/".into()])
            .await
            .unwrap();
    assert_eq!(source.exclusions, ["Extras"]);
    std::fs::write(root.join("Extras/new.mp4"), b"new extra").unwrap();
    let job = f.scan(&source.id).await;
    assert_eq!(
        job.outcome.as_deref(),
        Some("complete"),
        "excluded is not unproven"
    );
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT relative_path,available FROM media_files ORDER BY relative_path")
            .fetch_all(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            ("Extras/trailer.mp4".to_string(), false),
            ("film.mp4".to_string(), true)
        ],
        "excluded file leaves the catalog view; new excluded file is not cataloged"
    );
    assert!(matches!(
        libraries::set_exclusions(&f.app, &source.id, source.revision, &["../x".into()]).await,
        Err(libraries::LibraryError::Rejected(
            SourceError::InvalidExclusion(0)
        ))
    ));
}

#[tokio::test]
async fn legacy_registration_creates_one_library_and_source_each() {
    let f = Fixture::new().await;
    let legacy = db::add_library(&f.app.db, "Legacy", &f.root("legacy"))
        .await
        .unwrap();
    // A v1 registration through the admin path creates the paired library.
    let candidate = administration::candidate_root(&f.app, f.root("v1"))
        .await
        .unwrap();
    let v1 = administration::register_root(&f.app, candidate, Some("V1"))
        .await
        .unwrap();
    let library = libraries::get_library(&f.app.db, &v1.id).await.unwrap();
    assert_eq!(library.source_ids, std::slice::from_ref(&v1.id));
    let paired = libraries::get_library(&f.app.db, &legacy.id).await.unwrap();
    assert_eq!(paired.source_ids, std::slice::from_ref(&legacy.id));
    let sources = libraries::list_sources(&f.app.db).await.unwrap();
    assert_eq!(sources.len(), 2);
}

#[tokio::test]
async fn re_registration_keeps_library_edits_and_relocation_honors_exclusions() {
    let f = Fixture::new().await;
    let root = f.root("v1");
    let candidate = administration::candidate_root(&f.app, root.clone())
        .await
        .unwrap();
    let v1 = administration::register_root(&f.app, candidate, Some("V1"))
        .await
        .unwrap();
    let other = f.source("other", &[]).await;
    let library = libraries::get_library(&f.app.db, &v1.id).await.unwrap();
    libraries::save_library(
        &f.app,
        Some((&library.id, library.revision)),
        "V1",
        LibraryKind::Mixed,
        "",
        std::slice::from_ref(&other.id),
    )
    .await
    .unwrap();
    // Startup re-registers configured roots; the edit must survive.
    let again = administration::candidate_root(&f.app, root.clone())
        .await
        .unwrap();
    administration::register_root(&f.app, again, Some("V1"))
        .await
        .unwrap();
    let library = libraries::get_library(&f.app.db, &v1.id).await.unwrap();
    assert_eq!(library.source_ids, std::slice::from_ref(&other.id));

    // Exclude a scanned file, delete it, then relocate: the excluded path is
    // neither required at the new root nor re-enabled.
    std::fs::create_dir_all(root.join("Extras")).unwrap();
    std::fs::write(root.join("keep.mp4"), b"keep").unwrap();
    std::fs::write(root.join("Extras/gone.mp4"), b"gone").unwrap();
    f.scan(&v1.id).await;
    let source = libraries::get_source(&f.app.db, &v1.id).await.unwrap();
    // No scan after changing exclusions: relocation itself must stop
    // offering the excluded file.
    libraries::set_exclusions(&f.app, &v1.id, source.revision, &["Extras".into()])
        .await
        .unwrap();
    let moved = f.dir.path().join("v1-moved");
    std::fs::rename(&root, &moved).unwrap();
    std::fs::remove_file(moved.join("Extras/gone.mp4")).unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM libraries WHERE id=?")
        .bind(&v1.id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let response = playscale::api::router(f.app.clone(), None)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1/admin/libraries/{}/relocate", v1.id))
                .header("host", "127.0.0.1:8787")
                .header("authorization", "Bearer test-secret-token")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({"expected_revision":revision,"root":moved}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT relative_path,available FROM media_files ORDER BY relative_path")
            .fetch_all(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            ("Extras/gone.mp4".to_string(), false),
            ("keep.mp4".to_string(), true)
        ]
    );
}

#[tokio::test]
async fn v1_registration_pairs_a_source_only_root_once() {
    let f = Fixture::new().await;
    let source = f.source("shared", &[]).await;
    let candidate = administration::candidate_root(&f.app, f.root("shared"))
        .await
        .unwrap();
    let v1 = administration::register_root(&f.app, candidate, Some("Shared"))
        .await
        .unwrap();
    assert_eq!(v1.id, source.id);
    let library = libraries::get_library(&f.app.db, &v1.id).await.unwrap();
    assert_eq!(library.source_ids, std::slice::from_ref(&source.id));
}

#[tokio::test]
async fn deleting_libraries_and_sources_never_touches_originals() {
    let f = Fixture::new().await;
    let used = f.source("used", &[]).await;
    let empty = f.source("empty", &[]).await;
    std::fs::write(f.root("used").join("film.mp4"), b"film").unwrap();
    let library = libraries::save_library(
        &f.app,
        None,
        "L",
        LibraryKind::Mixed,
        "",
        std::slice::from_ref(&used.id),
    )
    .await
    .unwrap();
    // A source referenced by a library cannot be removed.
    let used_view = libraries::get_source(&f.app.db, &used.id).await.unwrap();
    assert!(matches!(
        libraries::delete_source(&f.app, &used.id, used_view.revision).await,
        Err(libraries::LibraryError::Busy)
    ));
    f.scan(&used.id).await;
    assert!(matches!(
        libraries::delete_library(&f.app, &library.id, 0).await,
        Err(libraries::LibraryError::Rejected(
            SourceError::StaleRevision
        ))
    ));
    libraries::delete_library(&f.app, &library.id, library.revision)
        .await
        .unwrap();
    assert!(
        libraries::get_library(&f.app.db, &library.id)
            .await
            .is_err()
    );
    // With cataloged files the source is disabled, not deleted.
    let used_view = libraries::get_source(&f.app.db, &used.id).await.unwrap();
    assert_eq!(
        libraries::delete_source(&f.app, &used.id, used_view.revision)
            .await
            .unwrap(),
        libraries::SourceRemoval::Disabled
    );
    assert!(
        f.root("used").join("film.mp4").exists(),
        "originals untouched"
    );
    let available: bool = sqlx::query_scalar("SELECT available FROM media_files")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert!(!available);
    // An unused source with no catalog is deleted outright.
    assert_eq!(
        libraries::delete_source(&f.app, &empty.id, empty.revision)
            .await
            .unwrap(),
        libraries::SourceRemoval::Deleted
    );
    assert!(libraries::get_source(&f.app.db, &empty.id).await.is_err());
}

#[tokio::test]
async fn relocation_is_previewed_then_committed_against_unchanged_facts() {
    let f = Fixture::new().await;
    let source = f.source("old", &[]).await;
    std::fs::write(f.root("old").join("a.mp4"), b"alpha").unwrap();
    f.scan(&source.id).await;
    let moved = f.dir.path().join("new");
    std::fs::rename(f.root("old"), &moved).unwrap();
    let candidate = administration::candidate_root(&f.app, moved.clone())
        .await
        .unwrap();
    let plan = libraries::preview_relocation(&f.app, &source.id, candidate)
        .await
        .unwrap();
    assert_eq!(plan.verified.len(), 1);
    // The file changes after preview: commit is refused.
    std::fs::write(moved.join("a.mp4"), b"alpha!").unwrap();
    assert!(matches!(
        libraries::commit_relocation(&f.app, &plan).await,
        Err(libraries::RelocationError::Conflict(
            playscale_core::sources::RelocationConflict::FileChanged(_)
        ))
    ));
    std::fs::write(moved.join("a.mp4"), b"alpha").unwrap();
    let candidate = administration::candidate_root(&f.app, moved.clone())
        .await
        .unwrap();
    let plan = libraries::preview_relocation(&f.app, &source.id, candidate)
        .await
        .unwrap();
    // A publication between preview and commit changes the reviewed catalog.
    sqlx::query("UPDATE media_files SET available=0 WHERE library_id=?")
        .bind(&source.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    assert!(matches!(
        libraries::commit_relocation(&f.app, &plan).await,
        Err(libraries::RelocationError::Conflict(
            playscale_core::sources::RelocationConflict::SourceChanged
        ))
    ));
    sqlx::query("UPDATE media_files SET available=1 WHERE library_id=?")
        .bind(&source.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let candidate = administration::candidate_root(&f.app, moved.clone())
        .await
        .unwrap();
    let plan = libraries::preview_relocation(&f.app, &source.id, candidate)
        .await
        .unwrap();
    let view = libraries::commit_relocation(&f.app, &plan).await.unwrap();
    assert_eq!(view.binding_revision, source.binding_revision + 1);
    assert!(
        matches!(
            libraries::commit_relocation(&f.app, &plan).await,
            Err(libraries::RelocationError::Conflict(
                playscale_core::sources::RelocationConflict::SourceChanged
            ))
        ),
        "a committed plan cannot be replayed"
    );
    // A candidate missing content is rejected at preview.
    let empty = f.root("empty");
    let candidate = administration::candidate_root(&f.app, empty).await.unwrap();
    assert!(matches!(
        libraries::preview_relocation(&f.app, &source.id, candidate).await,
        Err(libraries::RelocationError::CandidateMismatch(_))
    ));
}

#[tokio::test]
async fn pre_split_database_backfills_stable_logical_library_ids() {
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("migrations");
    std::fs::create_dir(&migrations).unwrap();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if name[..4].parse::<u32>().unwrap() <= 16 {
            std::fs::copy(&path, migrations.join(name)).unwrap();
        }
    }
    let path = dir.path().join("db.sqlite");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO libraries(id,name,root,root_identity) VALUES ('old-source','Old library','/fixture/source','fixture-volume')").execute(&pool).await.unwrap();
    pool.close().await;
    let upgraded = db::connect(&path).await.unwrap();
    let library = libraries::get_library(&upgraded, "old-source")
        .await
        .unwrap();
    assert_eq!(library.source_ids, ["old-source"]);
    assert_eq!(library.name, "Old library");
    let source = libraries::get_source(&upgraded, "old-source")
        .await
        .unwrap();
    assert_eq!(source.root_path, "/fixture/source");
    assert_eq!(source.volume_identity, "fixture-volume");
}

/// A real volume mounted over a scanned directory hides its files. The scan
/// must treat the mount point as a boundary and keep the hidden files rather
/// than withdrawing them (plan 7.5 "hidden mount"). macOS only: needs
/// `hdiutil`, and skips (with a note) where a disk image cannot be attached.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_volume_mounted_over_a_scanned_directory_does_not_withdraw_its_files() {
    use std::process::Command;
    let f = Fixture::new().await;
    let source = f.source("media", &[]).await;
    let root = f.root("media");
    std::fs::create_dir_all(root.join("Shows")).unwrap();
    std::fs::write(root.join("film.mp4"), b"film").unwrap();
    std::fs::write(root.join("Shows/episode.mp4"), b"episode").unwrap();
    let first = f.scan(&source.id).await;
    assert_eq!(first.phase, "completed");
    let image = f.dir.path().join("empty.dmg");
    let created = Command::new("hdiutil")
        .args([
            "create", "-quiet", "-size", "2m", "-fs", "HFS+", "-volname", "Empty",
        ])
        .arg(&image)
        .status();
    if !created.is_ok_and(|s| s.success()) {
        eprintln!("skipping: hdiutil create unavailable");
        return;
    }
    let attached = Command::new("hdiutil")
        .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
        .arg(root.join("Shows"))
        .arg(&image)
        .status();
    if !attached.is_ok_and(|s| s.success()) {
        eprintln!("skipping: hdiutil attach unavailable");
        return;
    }
    struct Detach(PathBuf);
    impl Drop for Detach {
        fn drop(&mut self) {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.0)
                .status();
        }
    }
    let _detach = Detach(root.join("Shows"));
    assert!(
        !root.join("Shows/episode.mp4").exists(),
        "the mount hides it"
    );

    let second = f.scan(&source.id).await;
    assert!(second.incomplete_directories >= 1, "{second:?}");
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT relative_path,available FROM media_files ORDER BY relative_path")
            .fetch_all(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            ("Shows/episode.mp4".to_string(), true),
            ("film.mp4".to_string(), true)
        ],
        "the hidden file is retained, not withdrawn"
    );
}

#[tokio::test]
async fn a_source_with_active_processing_cannot_be_removed() {
    let f = Fixture::new().await;
    let source = f.source("busy", &[]).await;
    std::fs::write(f.root("busy").join("film.mp4"), b"film").unwrap();
    f.scan(&source.id).await;
    let (file, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files WHERE library_id=?")
            .bind(&source.id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    sqlx::query("INSERT INTO processing_jobs (id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at) VALUES ('conv',?,?,'r','software','k','queued',1,1)")
        .bind(&file)
        .bind(&revision)
        .execute(&f.app.db)
        .await
        .unwrap();
    let view = libraries::get_source(&f.app.db, &source.id).await.unwrap();
    assert!(matches!(
        libraries::delete_source(&f.app, &source.id, view.revision).await,
        Err(libraries::LibraryError::Busy)
    ));
    let available: bool = sqlx::query_scalar("SELECT available FROM media_files WHERE id=?")
        .bind(&file)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert!(available, "the conversion's source stays available");
}
