//! Logical libraries over storage sources: registration, overlap, exclusions,
//! availability and the binding revision recorded by scan attempts.
use playscale::{App, administration, db, libraries, scan};
use playscale_core::sources::{Availability, LibraryKind, SourceError};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

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
