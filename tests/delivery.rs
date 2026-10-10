//! Live HLS delivery against real FFmpeg/FFprobe from PATH. Skips (with a message)
//! when either tool is missing.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db, scan};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
    library: String,
    stop: CancellationToken,
}

fn tools_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

impl Fixture {
    async fn new() -> Self {
        Self::with_storage(Default::default()).await
    }
    async fn with_storage(storage: playscale::storage::Settings) -> Self {
        Self::with_realtime(storage, false).await
    }
    async fn with_realtime(storage: playscale::storage::Settings, realtime: bool) -> Self {
        Self::with_options(storage, realtime, false).await
    }
    async fn with_options(
        storage: playscale::storage::Settings,
        realtime: bool,
        copy_routes: bool,
    ) -> Self {
        Self::configured(storage, realtime, copy_routes, |_, _| {}).await
    }
    /// `configure` adjusts the processing runtime (in the fixture directory).
    async fn configured(
        storage: playscale::storage::Settings,
        realtime: bool,
        copy_routes: bool,
        configure: impl FnOnce(&mut playscale::processing::Runtime, &std::path::Path),
    ) -> Self {
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_env_filter("playscale=debug")
            .try_init();
        // The first launch of a freshly linked executable can be delayed by the OS
        // (macOS assessment) far beyond a delivery lease. Warm it up untimed.
        let warm = Command::new(env!("CARGO_BIN_EXE_playscale"))
            .args(["--internal-ffmpeg-supervisor", "/usr/bin/true"])
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(warm.code().is_some());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let library = db::add_library(&db, "Test library", &root)
            .await
            .unwrap()
            .id;
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let mut processing =
            playscale::processing::Runtime::new(dir.path().join("cache"), Default::default());
        processing.supervisor = PathBuf::from(env!("CARGO_BIN_EXE_playscale"));
        processing.settings.experimental_copy_routes = copy_routes;
        configure(&mut processing, dir.path());
        if realtime {
            // A survivor test must own an encoder that cannot finish the entire
            // fixture before the restart assertions reach it.
            let wrapper = dir.path().join("realtime-ffmpeg");
            std::fs::write(&wrapper, "#!/bin/sh\nexec ffmpeg -re \"$@\"\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            processing.settings.ffmpeg = wrapper;
        }
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            db,
            admin_token: Arc::new("test-secret-token".into()),
            origin: Arc::new("http://127.0.0.1:8787".into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(4)),
            event_streams: Arc::new(Semaphore::new(2)),
            storage: Arc::new(playscale::storage::Runtime::new(state, storage)),
            processing: Arc::new(processing),
            access: Arc::new(playscale::v2::Runtime::new(
                playscale_core::access::AccessMode::TrustedHousehold,
                playscale::v2::auth::random_key(),
            )),
        };
        playscale::delivery::recover(&app).await.unwrap();
        let stop = CancellationToken::new();
        tokio::spawn(playscale::delivery::worker(app.clone(), stop.clone()));
        Self {
            dir,
            app,
            library,
            stop,
        }
    }
    async fn scan(&self) {
        let row = db::enqueue_mode(&self.app, &self.library, false)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(scan::worker(self.app.clone(), stop.clone()));
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let row = db::get_job(&self.app.db, &row.id).await.unwrap();
                if !["queued", "running", "cancelling"].contains(&row.phase.as_str()) {
                    assert_eq!(row.phase, "completed");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        task.await.unwrap().unwrap();
    }
    async fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
        raw_on(&self.app, method, path, body).await
    }
    async fn json(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        json_on(&self.app, method, path, body).await
    }
    /// Poll the delivery until `done` accepts it, renewing the lease like a client.
    async fn until(&self, id: &str, done: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        for _ in 0..600 {
            let (status, body) = self
                .json("GET", &format!("/api/v1/deliveries/{id}"), None)
                .await;
            if status == StatusCode::OK && done(&body) {
                return body;
            }
            let current = body["active"]["generation"]
                .as_str()
                .or(body["pending"]["generation"].as_str());
            if let Some(generation) = current {
                self.json(
                    "POST",
                    &format!("/api/v1/deliveries/{id}/heartbeat"),
                    Some(json!({"active_generation": generation})),
                )
                .await;
            }
            last = json!({"status": status.as_u16(), "body": body});
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let files: Vec<_> = walkdir::WalkDir::new(&self.app.processing.deliveries.root)
            .into_iter()
            .flatten()
            .map(|e| e.path().display().to_string())
            .collect();
        panic!("delivery condition not reached; last: {last}; files: {files:?}");
    }
}

async fn raw_on(app: &App, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
    raw_on_key(app, method, path, body, None).await
}

async fn raw_on_key(
    app: &App,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:8787")
        .header("authorization", "Bearer test-secret-token");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let response = api::router(app.clone(), None)
        .oneshot(
            request
                .body(
                    body.map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn json_on(app: &App, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (status, bytes) = raw_on(app, method, path, body).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn probe(path: &std::path::Path) -> Value {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hls_plays_before_completion_switches_generations_and_releases_workers() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // Two audio tracks distinguished by sample rate, so the output proves which
    // track was encoded.
    let source = f.dir.path().join("media/clip.mkv");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=880:sample_rate=22050",
            "-t",
            "240",
            "-map",
            "0",
            "-map",
            "1",
            "-map",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();

    let (status, _) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(json!({"file_id":file_id,"file_revision":"stale","start_ms":0,"audio_track":1})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":2})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, created) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":1})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["status"], "starting");

    // Playback starts before the encoder completes the 240-second timeline.
    let ready = f.until(&id, |d| d["active"]["status"] == "active").await;
    assert_eq!(ready["active"]["complete"], false, "{ready}");
    assert_eq!(ready["active"]["audio_track"], 1);
    let base = format!("/api/v1/streams/{id}/1");
    let (status, playlist) = f.raw("GET", &format!("{base}/index.m3u8"), None).await;
    assert_eq!(status, StatusCode::OK);
    let playlist = String::from_utf8(playlist).unwrap();
    assert!(playlist.contains("#EXT-X-TARGETDURATION:6\n"));
    assert!(playlist.contains("segments/0.m4s"));
    assert!(!playlist.contains("#EXT-X-ENDLIST") && !playlist.contains("PLAYLIST-TYPE"));
    let (status, init) = f.raw("GET", &format!("{base}/init.mp4"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, first) = f.raw("GET", &format!("{base}/segments/0.m4s"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = f
        .raw("GET", &format!("{base}/segments/999.m4s"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let fragment = f.dir.path().join("fragment.mp4");
    std::fs::write(&fragment, [init, first].concat()).unwrap();
    let streams = probe(&fragment)["streams"].as_array().unwrap().clone();
    assert!(streams.iter().any(|s| s["codec_name"] == "h264"));
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    assert_eq!(audio.len(), 1);
    assert_eq!(audio[0]["sample_rate"], "22050", "selected audio track");

    // Encode-ahead is bounded by the playhead: with the client at 0 the encoder
    // pauses about 45 s ahead and stays there; an advancing playhead resumes it.
    f.until(&id, |d| d["active"]["paused"] == true).await;
    // Segments finished between the pause decision and the stop signal may still
    // be published; after that the output is stable.
    let heartbeat = format!("/api/v1/deliveries/{id}/heartbeat");
    let beat = |position: u64| {
        f.json(
            "POST",
            &heartbeat,
            Some(json!({"active_generation":"1","position_ms":position})),
        )
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    let end = beat(0).await.1["active"]["available_end_ms"]
        .as_u64()
        .unwrap();
    assert!(
        (45_000..120_000).contains(&end),
        "encode-ahead not bounded: {end}"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (_, still) = beat(0).await;
    assert_eq!(still["active"]["paused"], true);
    assert_eq!(
        still["active"]["available_end_ms"], end,
        "encoder kept running"
    );
    let (status, resumed) = beat(end - 20_000).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resumed["active"]["paused"], false, "{resumed}");
    f.until(&id, |d| {
        d["active"]["available_end_ms"].as_u64().unwrap_or(0) > end
    })
    .await;

    // Seek far ahead with an overlapping generation; the old one keeps serving.
    let (status, changed) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/changes"),
            Some(json!({"expected_generation":"1","position_ms":180000})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{changed}");
    assert_eq!(changed["replacement_mode"], "overlap");
    assert_eq!(changed["pending"]["generation"], "2");
    let staged = f.until(&id, |d| d["pending"]["status"] == "ready").await;
    assert_eq!(staged["pending"]["media_time_origin_ms"], 180000);
    assert_eq!(staged["pending"]["audio_track"], 1, "seek keeps tracks");
    assert_eq!(
        f.raw("GET", &format!("{base}/index.m3u8"), None).await.0,
        StatusCode::OK
    );
    let (status, conflict) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/generations/2/activate"),
            Some(json!({"expected_active_generation":"2"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    let (status, activated) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/generations/2/activate"),
            Some(json!({"expected_active_generation":"1"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{activated}");
    assert_eq!(activated["active"]["generation"], "2");
    // The replaced generation is fenced immediately, even while its files drain.
    assert_eq!(
        f.raw("GET", &format!("{base}/index.m3u8"), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.raw("GET", &format!("{base}/segments/0.m4s"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.raw("GET", &format!("/api/v1/streams/{id}/2/index.m3u8"), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, _) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/changes"),
            Some(json!({"expected_generation":"1","position_ms":0})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/heartbeat"),
            Some(json!({"active_generation":"2"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // An explicit null removes audio (a replan), unlike an omitted field (a seek).
    let (status, silent) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/changes"),
            Some(json!({"expected_generation":"2","position_ms":0,"audio_track":null})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{silent}");
    assert_eq!(silent["pending"]["generation"], "3");
    assert_eq!(silent["pending"]["audio_track"], Value::Null);

    // Closing stops the encoders; capacity returns only after their exit.
    assert_eq!(
        f.raw("DELETE", &format!("/api/v1/deliveries/{id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.raw("GET", &format!("/api/v1/streams/{id}/2/index.m3u8"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            // Forgotten in memory, the delivery reads back as its recorded state.
            let (status, body) = f
                .json("GET", &format!("/api/v1/deliveries/{id}"), None)
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let gone = body["status"] == "closed" && !f.app.processing.deliveries.is_live(&id);
            let snapshot = f.app.processing.execution.snapshot();
            let removed = !f.app.processing.deliveries.root.join(&id).exists();
            if gone && removed && snapshot.used == 0 && snapshot.stuck.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("delivery workers were not released");
    f.stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_without_disk_headroom_fails_and_releases_capacity() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    // A floor no volume can satisfy: preparation refuses to start the encoder.
    let f = Fixture::with_storage(playscale::storage::Settings {
        min_free_bytes: i64::MAX as u64,
        ..Default::default()
    })
    .await;
    let source = f.dir.path().join("media/clip.mkv");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=12",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let (status, created) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(
                json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":null}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    // Failed, then forgotten once nothing remains: either observation is final.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (status, body) = f
                .json("GET", &format!("/api/v1/deliveries/{id}"), None)
                .await;
            if status == StatusCode::NOT_FOUND {
                break;
            }
            if body["status"] == "failed" {
                assert_eq!(body["active"], Value::Null, "{body}");
                break;
            }
            assert_ne!(body["status"], "ready", "{body}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("delivery did not fail");
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("capacity was not released");
    f.stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_reports_recorded_deliveries_as_interrupted() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_realtime(Default::default(), true).await;
    let source = f.dir.path().join("media/clip.mkv");
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=12",
            "-t",
            "60",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let (status, created) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(
                json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":null}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    // Creation is durable before acknowledgement, even if readiness is still pending.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM delivery_sessions WHERE id=?")
        .bind(&id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.until(&id, |d| d["active"]["status"] == "active").await;
    // The ready state is recorded asynchronously.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM delivery_sessions WHERE id=?")
                    .bind(&id)
                    .fetch_optional(&f.app.db)
                    .await
                    .unwrap();
            if status.as_deref() == Some("ready") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("delivery state was not recorded");

    // A new process over the same database and data directory.
    let mut processing =
        playscale::processing::Runtime::new(f.dir.path().join("cache"), Default::default());
    processing.supervisor = PathBuf::from(env!("CARGO_BIN_EXE_playscale"));
    let restarted = App {
        processing: Arc::new(processing),
        ..f.app.clone()
    };
    playscale::delivery::recover(&restarted).await.unwrap();
    let path = format!("/api/v1/deliveries/{id}");
    let (status, body) = json_on(&restarted, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "interrupted", "{body}");
    assert_eq!(body["active"], Value::Null);
    assert_eq!(body["pending"], Value::Null);
    assert_eq!(body["lease_expires_in_ms"], 0);
    let (status, refused) = json_on(
        &restarted,
        "POST",
        &format!("{path}/heartbeat"),
        Some(json!({"active_generation":"1"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "delivery_closed");
    assert_eq!(
        raw_on(&restarted, "DELETE", &path, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        raw_on(&restarted, "GET", "/api/v1/deliveries/unknown", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // The original process still owns its encoder; the new one counted it.
    assert_eq!(restarted.processing.execution.snapshot().stuck.len(), 1);
    assert_eq!(f.raw("DELETE", &path, None).await.0, StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0
            || restarted.processing.execution.snapshot().used != 0
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("encoder capacity was not released");
    // The old runtime's later writes must not undo the restart fence.
    let (status, body) = json_on(&restarted, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "interrupted");
    f.stop.cancel();
}

async fn admission_fixture() -> (Fixture, Value) {
    let f = Fixture::with_realtime(Default::default(), true).await;
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=12",
                "-t",
                "20",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
            ])
            .arg(f.dir.path().join("media/clip.mkv"))
            .status()
            .unwrap()
            .success()
    );
    f.scan().await;
    let (file, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let request = json!({"file_id":file,"file_revision":revision,"start_ms":0,"audio_track":null});
    (f, request)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_admission_and_retired_restart_retries_replay_exact_acknowledgement() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let (f, request) = admission_fixture().await;
    let path = "/api/v1/deliveries";
    let key = "delivery-retry-key-0001";
    let (first, second) = tokio::join!(
        raw_on_key(&f.app, "POST", path, Some(request.clone()), Some(key)),
        raw_on_key(&f.app, "POST", path, Some(request.clone()), Some(key)),
    );
    assert_eq!(first.0, StatusCode::CREATED);
    assert_eq!(
        second, first,
        "duplicate must replay exact initial acknowledgement"
    );
    let acknowledgement: Value = serde_json::from_slice(&first.1).unwrap();
    let id = acknowledgement["id"].as_str().unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_sessions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_admissions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut changed = request.clone();
    changed["start_ms"] = json!(1);
    let conflict = raw_on_key(&f.app, "POST", path, Some(changed), Some(key)).await;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.1).unwrap()["code"],
        "idempotency_conflict"
    );
    assert_eq!(
        raw_on_key(&f.app, "POST", path, Some(request.clone()), Some("short"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.raw("DELETE", &format!("{path}/{id}"), None).await.0,
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.deliveries.is_live(id)
            || f.app.processing.execution.snapshot().used != 0
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        raw_on_key(&f.app, "POST", path, Some(request.clone()), Some(key)).await,
        first
    );
    assert!(!f.app.processing.deliveries.is_live(id));
    let mut runtime =
        playscale::processing::Runtime::new(f.dir.path().join("cache"), Default::default());
    runtime.supervisor = PathBuf::from(env!("CARGO_BIN_EXE_playscale"));
    let restarted = App {
        processing: Arc::new(runtime),
        ..f.app.clone()
    };
    playscale::delivery::recover(&restarted).await.unwrap();
    assert_eq!(
        raw_on_key(&restarted, "POST", path, Some(request), Some(key)).await,
        first
    );
    assert_eq!(restarted.processing.execution.snapshot().used, 0);
    assert!(!restarted.processing.deliveries.is_live(id));
    f.stop.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receipt_failure_rolls_back_initial_state_before_any_encoder_starts() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let (f, request) = admission_fixture().await;
    sqlx::query("CREATE TRIGGER reject_admission BEFORE INSERT ON delivery_admissions BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END")
        .execute(&f.app.db).await.unwrap();
    let key = "delivery-rollback-key-0001";
    let failed = raw_on_key(
        &f.app,
        "POST",
        "/api/v1/deliveries",
        Some(request.clone()),
        Some(key),
    )
    .await;
    assert_eq!(failed.0, StatusCode::INTERNAL_SERVER_ERROR);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_sessions")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "initial state and receipt must roll back together"
    );
    assert_eq!(f.app.processing.execution.snapshot().used, 0);
    assert_eq!(
        std::fs::read_dir(&f.app.processing.deliveries.root)
            .unwrap()
            .count(),
        0
    );
    sqlx::query("DROP TRIGGER reject_admission")
        .execute(&f.app.db)
        .await
        .unwrap();
    let retried = raw_on_key(
        &f.app,
        "POST",
        "/api/v1/deliveries",
        Some(request),
        Some(key),
    )
    .await;
    assert_eq!(retried.0, StatusCode::CREATED);
    let body: Value = serde_json::from_slice(&retried.1).unwrap();
    assert_eq!(
        f.raw(
            "DELETE",
            &format!("/api/v1/deliveries/{}", body["id"].as_str().unwrap()),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    f.stop.cancel();
}

struct DatabaseAuthority {
    principal: String,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    gate: std::sync::atomic::AtomicBool,
}
impl playscale::delivery::AdmissionAuthority for DatabaseAuthority {
    fn reauthorize<'a>(
        &'a self,
        db: &'a mut sqlx::SqliteConnection,
        _: &'a playscale::delivery::CreateRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, playscale::api::ApiError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let allowed: bool = sqlx::query_scalar("SELECT allowed FROM test_admission_authority")
                .fetch_one(db)
                .await?;
            if !allowed {
                return Err(playscale::api::ApiError::new(
                    StatusCode::FORBIDDEN,
                    "revoked",
                    "Permission revoked",
                ));
            }
            if self.gate.swap(false, std::sync::atomic::Ordering::SeqCst) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(self.principal.clone())
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lost_waiter_does_not_cancel_admission_and_replay_rechecks_authority() {
    use axum::response::IntoResponse;
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let (f, request) = admission_fixture().await;
    sqlx::query("CREATE TABLE test_admission_authority(allowed INTEGER NOT NULL)")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO test_admission_authority VALUES (1)")
        .execute(&f.app.db)
        .await
        .unwrap();
    let authority = Arc::new(DatabaseAuthority {
        principal: "legacy-admin".into(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        gate: std::sync::atomic::AtomicBool::new(true),
    });
    let key = "lost-delivery-waiter-key";
    let task = tokio::spawn(playscale::delivery::admit_request(
        f.app.clone(),
        authority.clone(),
        Some(key.into()),
        serde_json::from_value(request.clone()).unwrap(),
    ));
    tokio::time::timeout(Duration::from_secs(5), authority.entered.notified())
        .await
        .unwrap();
    task.abort(); // The HTTP consumer vanished while admission was in progress.
    assert!(task.await.is_err());
    authority.release.notify_one();
    let id = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let id: Option<String> = sqlx::query_scalar(
                "SELECT delivery_id FROM delivery_admissions WHERE request_key=?",
            )
            .bind(key)
            .fetch_optional(&f.app.db)
            .await
            .unwrap();
            if let Some(id) = id {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let replay = raw_on_key(
        &f.app,
        "POST",
        "/api/v1/deliveries",
        Some(request.clone()),
        Some(key),
    )
    .await;
    assert_eq!(replay.0, StatusCode::CREATED);
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.1).unwrap()["id"],
        id
    );
    sqlx::query("UPDATE test_admission_authority SET allowed=0")
        .execute(&f.app.db)
        .await
        .unwrap();
    let denied = playscale::delivery::admit_request(
        f.app.clone(),
        authority,
        Some(key.into()),
        serde_json::from_value(request.clone()).unwrap(),
    )
    .await;
    let error = match denied {
        Ok(_) => panic!("revoked authority received a receipt"),
        Err(error) => error,
    };
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
    sqlx::query("UPDATE test_admission_authority SET allowed=1")
        .execute(&f.app.db)
        .await
        .unwrap();
    let other = Arc::new(DatabaseAuthority {
        principal: "other-principal".into(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        gate: std::sync::atomic::AtomicBool::new(false),
    });
    let (_, other) = playscale::delivery::admit_request(
        f.app.clone(),
        other,
        Some(key.into()),
        serde_json::from_value(request).unwrap(),
    )
    .await
    .unwrap_or_else(|_| panic!("independent principal admission failed"));
    assert_ne!(other.id, id, "idempotency keys are principal scoped");
    assert_eq!(
        f.raw("DELETE", &format!("/api/v1/deliveries/{}", other.id), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.raw("DELETE", &format!("/api/v1/deliveries/{id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    f.stop.cancel();
}

/// Copy routes keep the source video bitstream and timestamps: a seek starts at
/// the preceding keyframe, and only eligible stream combinations are admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copy_routes_start_at_keyframes_and_keep_source_video() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    // Copy routes are implemented but unqualified, so off unless enabled.
    let f = Fixture::with_options(Default::default(), false, true).await;
    // A 2 s GOP (48 frames at 24 fps); one file with AAC, one with AC-3 audio.
    // The AC-3 file is converted from its start, where B-frame decode times are
    // negative: a shifted output would fail the keyframe verification.
    for (name, audio) in [("aac.mkv", "aac"), ("ac3.mkv", "ac3")] {
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "60",
                // Main profile with B-frames: negative decode times at the start
                // exercise timestamp preservation, and Main differs from the
                // High profile a transcode would produce.
                "-c:v",
                "libx264",
                "-preset",
                "medium",
                "-profile:v",
                "main",
                "-g",
                "48",
                "-keyint_min",
                "48",
                "-sc_threshold",
                "0",
                "-c:a",
                audio,
            ])
            .arg(f.dir.path().join("media").join(name))
            .status()
            .unwrap();
        assert!(status.success());
    }
    // The copy routes require every stream (hence the container) to start at
    // zero: check the fixture really does, so a shifted encode fails loudly here.
    for name in ["aac.mkv", "ac3.mkv"] {
        let probed = probe(&f.dir.path().join("media").join(name));
        for stream in probed["streams"].as_array().unwrap() {
            let start: f64 = stream["start_time"].as_str().unwrap().parse().unwrap();
            assert!(start.abs() < 0.0005, "{name} stream starts at {start}");
        }
    }
    f.scan().await;
    let mut files = Vec::new();
    for name in ["aac.mkv", "ac3.mkv"] {
        let row: (String, String) =
            sqlx::query_as("SELECT id,revision FROM media_files WHERE relative_path=?")
                .bind(name)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        files.push(row);
    }
    let [(aac, aac_revision), (ac3, ac3_revision)]: [(String, String); 2] =
        files.try_into().unwrap();
    let create = |id: &str, revision: &str, operation: &str, start: u64| json!({"file_id":id,"file_revision":revision,"start_ms":start,"audio_track":0,"operation":operation});
    // Ineligible combinations are refused before anything starts.
    for body in [
        create(&ac3, &ac3_revision, "remux", 0),
        create(&aac, &aac_revision, "audio_convert", 0),
    ] {
        let (status, refused) = f.json("POST", "/api/v1/deliveries", Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
        assert_eq!(refused["code"], "route_unsupported");
    }
    let source_profile =
        probe(&f.dir.path().join("media/aac.mkv"))["streams"][0]["profile"].clone();
    assert_eq!(source_profile, json!("Main"));
    let b_frames = probe(&f.dir.path().join("media/aac.mkv"))["streams"][0]["has_b_frames"].clone();
    assert!(
        b_frames.as_i64().is_some_and(|b| b > 0),
        "fixture must contain B-frames: {b_frames}"
    );

    let mut deliveries = Vec::new();
    for (id, revision, operation, start, audio_codec) in [
        (&aac, &aac_revision, "remux", 31_000, "aac"),
        (&ac3, &ac3_revision, "audio_convert", 0, "aac"),
    ] {
        let (status, created) = f
            .json(
                "POST",
                "/api/v1/deliveries",
                Some(create(id, revision, operation, start)),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let delivery = created["id"].as_str().unwrap().to_owned();
        let ready = f
            .until(&delivery, |d| d["active"]["status"] == "active")
            .await;
        assert_eq!(ready["active"]["operation"], operation);
        // Segment 0 begins at the keyframe at or before the request.
        let begin = ready["active"]["available_start_ms"].as_u64().unwrap();
        assert_eq!(begin, start / 2_000 * 2_000, "{ready}");
        let base = format!("/api/v1/streams/{delivery}/1");
        let playlist =
            String::from_utf8(f.raw("GET", &format!("{base}/index.m3u8"), None).await.1).unwrap();
        assert!(
            playlist.contains("#EXT-X-TARGETDURATION:12\n"),
            "{playlist}"
        );
        let (_, init) = f.raw("GET", &format!("{base}/init.mp4"), None).await;
        let (_, first) = f.raw("GET", &format!("{base}/segments/0.m4s"), None).await;
        let fragment = f.dir.path().join(format!("{operation}.mp4"));
        std::fs::write(&fragment, [init, first].concat()).unwrap();
        let streams = probe(&fragment)["streams"].as_array().unwrap().clone();
        let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(video["profile"], source_profile, "video was re-encoded");
        assert_eq!(video["width"], 320);
        let audio = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
        assert_eq!(audio["codec_name"], audio_codec);
        deliveries.push(delivery);
    }
    for delivery in &deliveries {
        assert_eq!(
            f.raw("DELETE", &format!("/api/v1/deliveries/{delivery}"), None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("copy route capacity was not released");
    f.stop.cancel();
}

/// Hardware live transcoding: the VideoToolbox recipe runs Apple's encoder (no
/// libx264 signature in the bitstream), with forced segment keyframes.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn videotoolbox_live_transcode_uses_the_hardware_encoder() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // Hosted CI runners are VMs without a hardware encoder session; the server
    // then answers backend_unavailable for every VideoToolbox request.
    if !playscale::processing::videotoolbox_available(&f.app).await {
        eprintln!("SKIPPED: no VideoToolbox encoder session on this host");
        return;
    }
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "60",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
        ])
        .arg(f.dir.path().join("media/clip.mkv"))
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    // Hardware encoding is a transcode choice only.
    let (status, refused) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":0,"operation":"remux","backend":"videotoolbox"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["code"], "backend_unsupported");
    let mut signatures = Vec::new();
    for backend in ["videotoolbox", "software"] {
        let (status, created) = f
            .json(
                "POST",
                "/api/v1/deliveries",
                Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":0,"backend":backend})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["id"].as_str().unwrap().to_owned();
        let ready = f.until(&id, |d| d["active"]["status"] == "active").await;
        assert_eq!(ready["active"]["backend"], backend);
        let base = format!("/api/v1/streams/{id}/1");
        let playlist =
            String::from_utf8(f.raw("GET", &format!("{base}/index.m3u8"), None).await.1).unwrap();
        assert!(playlist.contains("#EXT-X-TARGETDURATION:6\n"), "{playlist}");
        // Forced keyframes: wait for four segments, then check every one is about
        // 4 s, bounded in size, and starts on a keyframe (independently decodable).
        f.until(&id, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 16_000
        })
        .await;
        let playlist =
            String::from_utf8(f.raw("GET", &format!("{base}/index.m3u8"), None).await.1).unwrap();
        let durations: Vec<f64> = playlist
            .lines()
            .filter_map(|l| l.strip_prefix("#EXTINF:"))
            .filter_map(|l| l.trim_end_matches(',').parse().ok())
            .collect();
        assert!(durations.len() >= 4, "{playlist}");
        assert!(
            durations[..4].iter().all(|d| (3.9..=4.1).contains(d)),
            "segments are not cut on the forced 4 s keyframes: {durations:?}"
        );
        let (_, init) = f.raw("GET", &format!("{base}/init.mp4"), None).await;
        let mut bytes = init.clone();
        for index in 0..4 {
            let (status, segment) = f
                .raw("GET", &format!("{base}/segments/{index}.m4s"), None)
                .await;
            assert_eq!(status, StatusCode::OK);
            assert!(segment.len() < 8 * 1024 * 1024, "segment {index} too large");
            let fragment = f.dir.path().join(format!("{backend}-{index}.mp4"));
            std::fs::write(&fragment, [init.clone(), segment.clone()].concat()).unwrap();
            let first = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "packet=flags",
                    "-of",
                    "csv=p=0",
                    "-read_intervals",
                    "%+#1",
                ])
                .arg(&fragment)
                .output()
                .unwrap();
            assert!(
                String::from_utf8_lossy(&first.stdout).starts_with('K'),
                "segment {index} does not start on a keyframe"
            );
            if index == 0 {
                bytes.extend(segment);
            }
        }
        signatures.push(bytes.windows(4).any(|w| w == b"x264"));
        let fragment = f.dir.path().join(format!("{backend}.mp4"));
        std::fs::write(&fragment, &bytes).unwrap();
        let streams = probe(&fragment)["streams"].as_array().unwrap().clone();
        assert!(streams.iter().any(|s| s["codec_name"] == "h264"));
        assert_eq!(
            f.raw("DELETE", &format!("/api/v1/deliveries/{id}"), None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    // libx264 writes its signature into the bitstream; Apple's encoder does not.
    assert_eq!(signatures, [false, true]);
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("encoder capacity was not released");
    f.stop.cancel();
}

/// Text subtitles are delivered as a WebVTT sidecar in timeline time, on the
/// generation that pins them; removing the choice removes the sidecar.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn text_subtitles_are_served_as_webvtt_sidecars() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    let srt = f.dir.path().join("cues.srt");
    std::fs::write(
        &srt,
        "1\n00:00:01,000 --> 00:00:03,000\nFirst cue\n\n2\n00:00:40,500 --> 00:00:42,000\nLater cue\n",
    )
    .unwrap();
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
        ])
        .arg("-i")
        .arg(&srt)
        .args([
            "-t",
            "60",
            "-map",
            "0",
            "-map",
            "1",
            "-map",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-c:s",
            "srt",
            // A container that starts at 5 s: cues and video are both relative
            // to the container start (timeline time).
            "-output_ts_offset",
            "5",
        ])
        .arg(f.dir.path().join("media/clip.mkv"))
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let (status, refused) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":0,"subtitle_track":1})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    let (status, created) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":30000,"audio_track":0,"subtitle_track":0})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    let ready = f.until(&id, |d| d["active"]["status"] == "active").await;
    assert_eq!(ready["active"]["subtitle_track"], 0);
    let url = ready["active"]["subtitles_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = f.raw("GET", &url, None).await;
    assert_eq!(status, StatusCode::OK);
    let vtt = String::from_utf8(body).unwrap();
    assert!(vtt.starts_with("WEBVTT"), "{vtt}");
    // Cues keep source time, which is timeline time, even for a seek to 30 s.
    assert!(
        vtt.contains("00:01.000 --> 00:03.000") && vtt.contains("First cue"),
        "{vtt}"
    );
    assert!(
        vtt.contains("00:40.500 --> 00:42.000") && vtt.contains("Later cue"),
        "{vtt}"
    );
    // A second request is served from the cached sidecar, not reconverted.
    let cached = f
        .app
        .processing
        .deliveries
        .root
        .join(&id)
        .join("subtitles-0.vtt");
    let modified = std::fs::metadata(&cached).unwrap().modified().unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(f.raw("GET", &url, None).await.1, vtt.as_bytes());
    assert_eq!(
        std::fs::metadata(&cached).unwrap().modified().unwrap(),
        modified
    );
    // An explicit null removes the subtitle in the replacement generation.
    let (status, changed) = f
        .json(
            "POST",
            &format!("/api/v1/deliveries/{id}/changes"),
            Some(json!({"expected_generation":"1","position_ms":0,"subtitle_track":null})),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{changed}");
    assert!(
        changed["pending"]
            .get("subtitle_track")
            .is_some_and(Value::is_null),
        "{changed}"
    );
    let staged = f.until(&id, |d| d["pending"]["status"] == "ready").await;
    assert!(
        staged["pending"]
            .get("subtitles_url")
            .is_some_and(Value::is_null),
        "{staged}"
    );
    assert_eq!(
        f.raw(
            "GET",
            &format!("/api/v1/streams/{id}/2/subtitles.vtt"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.raw("DELETE", &format!("/api/v1/deliveries/{id}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("encoder capacity was not released");
    f.stop.cancel();
}

/// A live encoder that never publishes a segment fails its generation once the
/// startup deadline passes, and its capacity is released after it is stopped.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silent_live_encoder_fails_at_the_startup_deadline() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::configured(Default::default(), false, false, |processing, dir| {
        use std::os::unix::fs::PermissionsExt;
        let silent = dir.join("silent-ffmpeg");
        std::fs::write(&silent, "#!/bin/sh\nexec sleep 600\n").unwrap();
        std::fs::set_permissions(&silent, std::fs::Permissions::from_mode(0o700)).unwrap();
        processing.settings.ffmpeg = silent;
        processing.settings.startup_timeout_seconds = Some(3);
    })
    .await;
    // The source is made with the real tools; only the live encoder is silent.
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=12",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(f.dir.path().join("media/clip.mkv"))
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let started = std::time::Instant::now();
    let (status, created) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(
                json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":null}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (status, body) = f
                .json("GET", &format!("/api/v1/deliveries/{id}"), None)
                .await;
            if status == StatusCode::NOT_FOUND || body["status"] == "failed" {
                break;
            }
            assert_ne!(body["status"], "ready", "{body}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("silent encoder was not failed");
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "failed before the deadline"
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("encoder capacity was not released");
    f.stop.cancel();
}

/// Completion ends the progress watch: an encoder that published its final
/// segment but is slow to exit keeps its completed generation.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_generation_survives_a_slow_encoder_exit() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let real = String::from_utf8(Command::new("which").arg("ffmpeg").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    let f = Fixture::configured(Default::default(), false, false, |processing, dir| {
        use std::os::unix::fs::PermissionsExt;
        let wrapper = dir.join("encode-then-wait");
        std::fs::write(
            &wrapper,
            format!("#!/bin/sh\n'{real}' \"$@\" || exit $?\nexec sleep 4\n"),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        processing.settings.ffmpeg = wrapper;
        processing.settings.no_progress_timeout_seconds = Some(1);
    })
    .await;
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=12",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(f.dir.path().join("media/clip.mkv"))
        .status()
        .unwrap();
    assert!(status.success());
    f.scan().await;
    let (file_id, revision): (String, String) =
        sqlx::query_as("SELECT id,revision FROM media_files LIMIT 1")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let (status, created) = f
        .json(
            "POST",
            "/api/v1/deliveries",
            Some(
                json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":null}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    f.until(&id, |v| v["active"]["complete"] == true).await;
    // Past the no-progress budget while the wrapper is still running, then past its exit.
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            let (_, body) = f
                .json("GET", &format!("/api/v1/deliveries/{id}"), None)
                .await;
            assert_eq!(
                body["status"], "ready",
                "completed output was discarded: {body}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("encoder capacity was not released");
    let (_, body) = f
        .json("GET", &format!("/api/v1/deliveries/{id}"), None)
        .await;
    assert_eq!(body["status"], "ready", "{body}");
    assert_eq!(body["active"]["complete"], true, "{body}");
    f.stop.cancel();
}

/// An open-GOP source's later keyframes are recovery points, not IDRs: a copied
/// segment cut there is not independent, so the generation fails rather than
/// publishing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copy_route_refuses_open_gop_segments() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_options(Default::default(), false, true).await;
    // The same encode with closed and open GOPs: the closed one is the control
    // that the fixture can publish every copied segment.
    for (name, gop) in [("closed.mkv", "open-gop=0"), ("open.mkv", "open-gop=1")] {
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "40",
                "-c:v",
                "libx264",
                "-preset",
                "medium",
                "-profile:v",
                "main",
                "-g",
                "48",
                "-keyint_min",
                "48",
                "-sc_threshold",
                "0",
                "-x264-params",
                gop,
                "-c:a",
                "aac",
            ])
            .arg(f.dir.path().join("media").join(name))
            .status()
            .unwrap();
        assert!(status.success());
    }
    f.scan().await;
    let mut ids = Vec::new();
    for name in ["closed.mkv", "open.mkv"] {
        let (file_id, revision): (String, String) =
            sqlx::query_as("SELECT id,revision FROM media_files WHERE relative_path=?")
                .bind(name)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        let (status, created) = f
            .json(
                "POST",
                "/api/v1/deliveries",
                Some(json!({"file_id":file_id,"file_revision":revision,"start_ms":0,"audio_track":0,"operation":"remux"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        ids.push(created["id"].as_str().unwrap().to_owned());
    }
    // Every closed-GOP segment is published, through completion.
    let closed = f
        .until(&ids[0], |v| {
            v["active"]["complete"] == true || v["status"] == "failed"
        })
        .await;
    assert_eq!(closed["status"], "ready", "{closed}");
    assert_eq!(closed["active"]["complete"], true, "{closed}");
    // The open-GOP source's segment 0 begins at its first frame, an IDR; its
    // later cuts are recovery points, so the generation fails.
    let failed = f.until(&ids[1], |v| v["status"] == "failed").await;
    assert!(failed["active"].is_null(), "{failed}");
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("encoder capacity was not released");
    f.stop.cancel();
}
