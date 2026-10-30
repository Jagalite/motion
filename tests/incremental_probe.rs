//! Real scanner probe calls for incremental evidence reuse.
use playscale::{App, db, scan};
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn scanner_uses_core_probe_decision_for_cached_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("media");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("film.mkv"), b"first").unwrap();
    let probe = dir.path().join("probe");
    let log = dir.path().join("probe.log");
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in\n *chapter=*) printf 'chapter\\n' >> '{}' ; printf '%s' '{{\"chapters\":[]}}' ;;\n *) printf 'full\\n' >> '{}' ; printf '%s' '{{\"format\":{{\"duration\":\"120\"}},\"streams\":[{{\"index\":0,\"codec_type\":\"video\",\"codec_name\":\"h264\"}}]}}' ;;\nesac\n",
        log.display(),
        log.display()
    );
    std::fs::write(&probe, script).unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let pool = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
    let library = db::add_library(&pool, "Library", &root).await.unwrap().id;
    let app = App {
        health: Arc::new(playscale::operations::Health::new(false)),
        db: pool,
        admin_token: Arc::new("test-secret-token".into()),
        origin: Arc::new("http://127.0.0.1:8787".into()),
        authority: Arc::new("127.0.0.1:8787".into()),
        ffprobe: Arc::new(probe),
        jobs: Arc::new(Mutex::new(())),
        streams: Arc::new(Semaphore::new(2)),
        event_streams: Arc::new(Semaphore::new(2)),
        storage: Arc::new(playscale::storage::Runtime::new(
            dir.path().join("state"),
            Default::default(),
        )),
        processing: Arc::new(playscale::processing::Runtime::new(
            dir.path().join("cache"),
            Default::default(),
        )),
        access: Arc::new(playscale::v2::Runtime::new(
            playscale_core::access::AccessMode::Restricted,
            playscale::v2::auth::random_key(),
        )),
    };
    async fn run(app: &App, library: &str, full: bool) {
        let job = db::enqueue_mode(app, library, full).await.unwrap();
        let stop = CancellationToken::new();
        let worker = tokio::spawn(scan::worker(app.clone(), stop.clone()));
        let done = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let row = db::get_job(&app.db, &job.id).await.unwrap();
                if !["queued", "running", "cancelling"].contains(&row.phase.as_str()) {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        worker.await.unwrap().unwrap();
        assert_eq!(done.phase, "completed");
    }
    run(&app, &library, true).await;
    std::fs::write(&log, "").unwrap();
    run(&app, &library, false).await;
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
    sqlx::query("UPDATE media_files SET chapters_json=NULL")
        .execute(&app.db)
        .await
        .unwrap();
    run(&app, &library, false).await;
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "chapter\n");
    std::fs::write(&log, "").unwrap();
    sqlx::query("UPDATE media_files SET tracks_json=json_set(tracks_json,'$[0].probe_version',1)")
        .execute(&app.db)
        .await
        .unwrap();
    run(&app, &library, false).await;
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "full\nchapter\n");
    std::fs::write(&log, "").unwrap();
    std::fs::write(root.join("film.mkv"), b"changed content").unwrap();
    run(&app, &library, false).await;
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "full\nchapter\n");
}
