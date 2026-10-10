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
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:8787")
        .header("authorization", "Bearer test-secret-token");
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
