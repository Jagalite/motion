//! Shared scan demands over real attempts: joining, freshness follow-ups,
//! cancellation isolation, disabled sources and crash recovery.
use playscale::{App, administration, db, libraries, scan, scans};
use playscale_core::{demands::DemandStatus, sources::LibraryKind};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration};
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
    async fn source(&self, name: &str) -> String {
        let root = self.root(name);
        std::fs::write(root.join("file.mp4"), name.as_bytes()).unwrap();
        let candidate = administration::candidate_root(&self.app, root)
            .await
            .unwrap();
        libraries::register_source(&self.app, candidate, name, &[])
            .await
            .unwrap()
            .id
    }
    async fn library(&self, sources: &[String]) -> String {
        libraries::save_library(&self.app, None, "Lib", LibraryKind::Mixed, "", sources)
            .await
            .unwrap()
            .id
    }
    /// Probe that blocks until `probe.release` exists, so a test can act while
    /// an attempt is running.
    fn gated_probe(&mut self) {
        let executable = self.dir.path().join("gated-probe");
        std::fs::write(
            &executable,
            "#!/bin/sh\ntouch \"${0%/*}/probe.started\"\nwhile [ ! -f \"${0%/*}/probe.release\" ]; do sleep 0.05; done\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        self.app.ffprobe = Arc::new(executable);
    }
    async fn until(&self, what: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !what() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn settle(&self, scan: &str) -> scans::ScanView {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let view = scans::get(&self.app.db, scan).await.unwrap();
                if view.status != DemandStatus::Pending {
                    break view;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn jobs(&self, source: &str) -> Vec<(String, String)> {
        sqlx::query_as("SELECT id,phase FROM jobs WHERE library_id=? ORDER BY created_at,rowid")
            .bind(source)
            .fetch_all(&self.app.db)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn concurrent_requests_share_one_attempt_per_source() {
    let f = Fixture::new().await;
    let a = f.source("a").await;
    let b = f.source("b").await;
    let library = f.library(&[a.clone(), b.clone()]).await;
    let first = scans::request(&f.app, &library, None, false, true)
        .await
        .unwrap();
    let second = scans::request(
        &f.app,
        &library,
        Some(std::slice::from_ref(&a)),
        false,
        false,
    )
    .await
    .unwrap();
    assert_eq!(first.sources.len(), 2);
    assert_eq!(
        f.jobs(&a).await.len(),
        1,
        "second request joined the queued attempt"
    );
    let stop = CancellationToken::new();
    let worker = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    let first = f.settle(&first.id).await;
    let second = f.settle(&second.id).await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(first.status, DemandStatus::Complete);
    assert!(first.complete);
    assert_eq!(second.status, DemandStatus::Complete);
    let first_a = first.sources.iter().find(|s| s.source_id == a).unwrap();
    assert_eq!(
        first_a.job_id, second.sources[0].job_id,
        "one attempt served both"
    );
}

#[tokio::test]
async fn a_request_during_a_running_scan_gets_a_fresh_follow_up() {
    let mut f = Fixture::new().await;
    let a = f.source("a").await;
    let library = f.library(std::slice::from_ref(&a)).await;
    f.gated_probe();
    let early = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    let started = f.dir.path().join("probe.started");
    f.until(|| started.exists()).await;
    let late = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    let jobs = f.jobs(&a).await;
    assert_eq!(
        jobs.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(),
        ["running", "queued"],
        "the running attempt started before the late request"
    );
    std::fs::write(f.dir.path().join("probe.release"), b"").unwrap();
    let early = f.settle(&early.id).await;
    let late = f.settle(&late.id).await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(early.status, DemandStatus::Complete);
    assert_eq!(late.status, DemandStatus::Complete);
    assert_ne!(
        early.sources[0].job_id, late.sources[0].job_id,
        "never reported stale as fresh"
    );
}

#[tokio::test]
async fn cancelling_one_request_keeps_the_attempt_another_needs() {
    let f = Fixture::new().await;
    let a = f.source("a").await;
    let library = f.library(std::slice::from_ref(&a)).await;
    let one = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    let two = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    let one = scans::cancel(&f.app, &one.id).await.unwrap();
    assert_eq!(one.status, DemandStatus::Cancelled);
    assert_eq!(
        f.jobs(&a).await[0].1,
        "queued",
        "still needed by the second request"
    );
    let two = scans::cancel(&f.app, &two.id).await.unwrap();
    assert_eq!(two.status, DemandStatus::Cancelled);
    assert_eq!(f.jobs(&a).await[0].1, "cancelled");
}

#[tokio::test]
async fn disabled_sources_fail_their_demand_and_verify_upgrades_queued_work() {
    let f = Fixture::new().await;
    let a = f.source("a").await;
    let b = f.source("b").await;
    sqlx::query("UPDATE libraries SET enabled=0 WHERE id=?")
        .bind(&b)
        .execute(&f.app.db)
        .await
        .unwrap();
    let library = f.library(&[a.clone(), b.clone()]).await;
    let view = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    let disabled = view.sources.iter().find(|s| s.source_id == b).unwrap();
    assert_eq!(disabled.status, DemandStatus::Failed);
    assert_eq!(disabled.error.as_deref(), Some("source_disabled"));
    scans::request(
        &f.app,
        &library,
        Some(std::slice::from_ref(&a)),
        true,
        false,
    )
    .await
    .unwrap();
    let verify: bool = sqlx::query_scalar("SELECT full_scan FROM jobs WHERE library_id=?")
        .bind(&a)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert!(verify, "the queued incremental attempt was upgraded");
    assert!(matches!(
        scans::request(&f.app, &library, Some(&["other".into()]), false, false).await,
        Err(scans::ScanError::UnknownSource(_))
    ));
}

#[tokio::test]
async fn recovery_merges_an_interrupted_attempt_into_its_queued_follow_up() {
    let f = Fixture::new().await;
    let a = f.source("a").await;
    let library = f.library(std::slice::from_ref(&a)).await;
    let early = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    // Simulate a crash mid-traversal: the attempt is running at barrier 1.
    sqlx::query("UPDATE jobs SET phase='running',attempt=1,started_barrier=1 WHERE library_id=?")
        .bind(&a)
        .execute(&f.app.db)
        .await
        .unwrap();
    let late = scans::request(&f.app, &library, None, true, false)
        .await
        .unwrap();
    assert_eq!(f.jobs(&a).await.len(), 2);
    db::recover(&f.app.db).await.unwrap();
    let jobs = f.jobs(&a).await;
    assert_eq!(
        jobs.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(),
        ["cancelled", "queued"]
    );
    // Both demands are still pending and will be served by the queued attempt.
    let stop = CancellationToken::new();
    let worker = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    assert_eq!(f.settle(&early.id).await.status, DemandStatus::Complete);
    assert_eq!(f.settle(&late.id).await.status, DemandStatus::Complete);
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn direct_requests_are_respected_and_empty_requests_rejected() {
    let f = Fixture::new().await;
    let a = f.source("a").await;
    let library = f.library(std::slice::from_ref(&a)).await;
    // A v1/scheduled request owns the queued attempt; a demand joins it.
    let direct = db::enqueue(&f.app, &a).await.unwrap();
    let demand = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    scans::cancel(&f.app, &demand.id).await.unwrap();
    assert_eq!(
        db::get_job(&f.app.db, &direct.id).await.unwrap().phase,
        "queued"
    );
    // Cancelling the direct job itself re-queues work for a pending demand.
    let other = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    db::cancel(&f.app, &direct.id).await.unwrap();
    let jobs = f.jobs(&a).await;
    assert_eq!(
        jobs.iter().filter(|(_, p)| p == "queued").count(),
        1,
        "follow-up queued"
    );
    let stop = CancellationToken::new();
    let worker = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    let other = f.settle(&other.id).await;
    stop.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(other.status, DemandStatus::Complete);
    // Coverage survives pruning of the answering job.
    let counts = other.sources[0].complete_directories;
    assert!(counts >= 1);
    sqlx::query("DELETE FROM jobs WHERE id=?")
        .bind(other.sources[0].job_id.as_deref().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let retained = scans::get(&f.app.db, &other.id).await.unwrap();
    assert_eq!(retained.sources[0].complete_directories, counts);
    // No sources: rejected rather than pending forever.
    let empty = f.library(&[]).await;
    assert!(matches!(
        scans::request(&f.app, &empty, None, false, false).await,
        Err(scans::ScanError::NoSources)
    ));
    assert!(matches!(
        scans::request(&f.app, &library, Some(&[]), false, false).await,
        Err(scans::ScanError::NoSources)
    ));
}

#[tokio::test]
async fn recovery_keeps_direct_ownership_when_merging_attempts() {
    let f = Fixture::new().await;
    let a = f.source("a").await;
    let library = f.library(std::slice::from_ref(&a)).await;
    // A direct (v1) attempt is running when a demand queues a follow-up.
    let direct = db::enqueue(&f.app, &a).await.unwrap();
    sqlx::query("UPDATE jobs SET phase='running',attempt=1,started_barrier=0 WHERE id=?")
        .bind(&direct.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let demand = scans::request(&f.app, &library, None, false, false)
        .await
        .unwrap();
    db::recover(&f.app.db).await.unwrap();
    // Cancelling the demand must not cancel the direct requester's work.
    scans::cancel(&f.app, &demand.id).await.unwrap();
    let jobs = f.jobs(&a).await;
    assert_eq!(
        jobs.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(),
        ["cancelled", "queued"]
    );
}

#[tokio::test]
async fn a_filesystem_change_requests_a_scan_that_publishes_it() {
    let f = Fixture::new().await;
    let source = f.source("watched").await;
    f.library(std::slice::from_ref(&source)).await;
    let stop = CancellationToken::new();
    let scanner = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    let watcher = tokio::spawn(playscale::watch::worker(
        f.app.clone(),
        stop.clone(),
        playscale_core::watch::Debounce {
            quiet_ms: 300,
            max_delay_ms: 3_000,
        },
    ));
    // Native watchers start asynchronously; give the first sync a moment.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let root = f.root("watched");
    std::fs::write(root.join(".DS_Store"), b"ignored").unwrap();
    std::fs::write(root.join("new.mkv"), b"new film").unwrap();
    let db = f.app.db.clone();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let published: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM media_files WHERE relative_path='new.mkv'",
            )
            .fetch_one(&db)
            .await
            .unwrap();
            if published == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the change was scanned and published");
    let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM scan_requests")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert!(requests >= 1, "the watcher went through the demand rules");
    stop.cancel();
    scanner.await.unwrap().unwrap();
    watcher.await.unwrap().unwrap();
}
