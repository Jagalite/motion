//! HTTP conformance for the v2 playback, delivery and viewing adapters over
//! the production router, real SQLite, real scans and real FFmpeg output.
//! Decisions are unit-tested and model-checked in
//! crates/core/src/playback_session.rs, crates/core/tests/playback_session_model.rs,
//! crates/core/src/delivery.rs and crates/core/src/viewing.rs. These tests
//! check that the adapter supplies the right observations, enforces the
//! decisions and executes the effects (bytes, segments, durable progress).
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db, scan};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use std::{path::Path, process::Command, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const OPERATOR: &str = "operator-secret-token-0123456789abcdef";

fn tools_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
    bytes: Vec<u8>,
}

struct Server {
    app: App,
    stop: CancellationToken,
}

impl Server {
    /// A production App over `dir` (database, media, cache). A second Server
    /// over the same directory is a restart: deliveries are recovered as
    /// interrupted; viewing state is durable.
    async fn start(dir: &Path, key: [u8; 32]) -> Self {
        let db = db::connect(&dir.join("db.sqlite")).await.unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let mut processing =
            playscale::processing::Runtime::new(dir.join("cache"), Default::default());
        processing.supervisor = env!("CARGO_BIN_EXE_playscale").into();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            db,
            admin_token: Arc::new(OPERATOR.into()),
            origin: Arc::new("http://127.0.0.1:8787".into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(8)),
            event_streams: Arc::new(Semaphore::new(2)),
            storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
            processing: Arc::new(processing),
            access: Arc::new(playscale::v2::Runtime::new(AccessMode::Restricted, key)),
        };
        playscale::delivery::recover(&app).await.unwrap();
        let stop = CancellationToken::new();
        tokio::spawn(playscale::delivery::worker(app.clone(), stop.clone()));
        Self { app, stop }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8787");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let response = api::router(self.app.clone(), None)
            .oneshot(
                request
                    .body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            body,
            bytes,
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    key: [u8; 32],
    server: Server,
    library: String,
}

impl Fixture {
    async fn new() -> Self {
        // The first launch of a freshly linked supervisor can be slow (macOS
        // assessment); warm it untimed so leases are not consumed by it.
        let warm = Command::new(env!("CARGO_BIN_EXE_playscale"))
            .args(["--internal-ffmpeg-supervisor", "/usr/bin/true"])
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(warm.code().is_some());
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("media")).unwrap();
        let key = playscale::v2::auth::random_key();
        let server = Server::start(dir.path(), key).await;
        let library = db::add_library(&server.app.db, "Movies", &dir.path().join("media"))
            .await
            .unwrap()
            .id;
        sqlx::query("INSERT INTO profiles(id,name) VALUES ('kids','Kids')")
            .execute(&server.app.db)
            .await
            .unwrap();
        Self {
            dir,
            key,
            server,
            library,
        }
    }

    /// Stop this server and start another over the same state.
    async fn restart(&mut self) {
        self.server.stop.cancel();
        self.server = Server::start(self.dir.path(), self.key).await;
    }

    fn media(&self, name: &str, args: &[&str]) {
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(args)
            .arg(self.dir.path().join("media").join(name))
            .status()
            .unwrap();
        assert!(status.success());
    }

    async fn scan(&self) {
        let app = &self.server.app;
        let row = db::enqueue_mode(app, &self.library, false).await.unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(scan::worker(app.clone(), stop.clone()));
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let row = db::get_job(&app.db, &row.id).await.unwrap();
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

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        h: &[(&str, &str)],
    ) -> Reply {
        self.server.call(method, path, body, h).await
    }

    /// A paired device restricted to this library; returns (device, bearer).
    async fn device(&self, profiles: &[&str], permissions: &[&str]) -> (String, String) {
        let pairing = self
            .call(
                "POST",
                "/api/v2/auth/pairings",
                Some(json!({"device_name":"Living room","client_name":"motion-test"})),
                &[],
            )
            .await;
        assert_eq!(pairing.status, StatusCode::CREATED, "{:?}", pairing.body);
        let id = pairing.body["id"].as_str().unwrap().to_owned();
        let approved = self
            .call(
                "POST",
                &format!("/api/v2/auth/pairings/{id}/approve"),
                Some(json!({"user_code":pairing.body["user_code"],"profile_ids":profiles,"permissions":permissions})),
                &[("authorization", &bearer(OPERATOR)), ("idempotency-key", &format!("approve-{id}"))],
            )
            .await;
        assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
        let claimed = self
            .call(
                "POST",
                &format!("/api/v2/auth/pairings/{id}/claim"),
                Some(json!({"device_code":pairing.body["device_code"]})),
                &[],
            )
            .await;
        assert_eq!(claimed.status, StatusCode::OK, "{:?}", claimed.body);
        let device = claimed.body["device_id"].as_str().unwrap().to_owned();
        let token = bearer(claimed.body["access_token"].as_str().unwrap());
        self.policy(&device, &[self.library.as_str()], permissions, "\"r-1\"")
            .await;
        (device, token)
    }

    async fn policy(&self, device: &str, libraries: &[&str], permissions: &[&str], tag: &str) {
        let replaced = self
            .call(
                "PUT",
                &format!("/api/v2/devices/{device}/policy"),
                Some(json!({"library_ids":libraries,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                &[("authorization", &bearer(OPERATOR)), ("if-match", tag)],
            )
            .await;
        assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    }

    async fn get(&self, path: &str, auth: &str) -> Reply {
        self.call("GET", path, None, &[("authorization", auth)])
            .await
    }
    async fn post(&self, path: &str, auth: &str, key: Option<&str>, body: Value) -> Reply {
        let mut headers = vec![("authorization", auth)];
        if let Some(key) = key {
            headers.push(("idempotency-key", key));
        }
        self.call("POST", path, Some(body), &headers).await
    }

    async fn timeline(&self) -> (String, String) {
        sqlx::query_as("SELECT t.id,e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id ORDER BY t.id LIMIT 1")
            .fetch_one(&self.server.app.db)
            .await
            .unwrap()
    }

    /// Poll a delivery (renewing its lease) until `done` holds.
    async fn until(&self, id: &str, auth: &str, done: impl Fn(&Value) -> bool) -> Value {
        let mut last = Value::Null;
        for _ in 0..600 {
            let reply = self
                .get(&format!("/api/v2/playback/delivery-sessions/{id}"), auth)
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
            if done(&reply.body) {
                return reply.body;
            }
            if let Some(g) = reply.body["active"]["generation"]
                .as_str()
                .or(reply.body["pending"]["generation"].as_str())
            {
                self.post(
                    &format!("/api/v2/playback/delivery-sessions/{id}/heartbeat"),
                    auth,
                    None,
                    json!({"active_generation": g}),
                )
                .await;
            }
            last = reply.body;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("delivery never reached the expected state: {last}");
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn client(transports: &[&str]) -> Value {
    json!({"client_id":"motion-test","client_build":"1","demuxe_asset_digest":null,
        "transports":transports,"video_codecs":["avc1"],"audio_codecs":["mp4a"],
        "subtitle_modes":["text"],"hdr":"unknown","max_height":null,"software_decode":"unknown",
        "cross_origin_isolated":false})
}

fn plan_input(
    profile: &str,
    timeline: &str,
    mode: &str,
    audio: Option<&str>,
    transports: &[&str],
) -> Value {
    json!({"profile_id":profile,"timeline_id":timeline,"version_id":null,"source":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"auto",
            "audio_track_id":audio,"subtitle_track_id":null},
        "quality":{"mode":mode,"max_bitrate_bps":null,"max_height":null,"allow_client_software":true,
            "hdr_policy":"preserve_if_supported"},
        "client":client(transports),"failed_candidate_ids":[]})
}

fn problem(reply: &Reply, status: StatusCode, code: &str) {
    assert_eq!(reply.status, status, "{:?}", reply.body);
    assert_eq!(reply.body["code"], code, "{:?}", reply.body);
}

const PLAYER: &[&str] = &["catalog:read", "playback:request", "viewing:write"];

/// H.264/AAC MP4 with two audio tracks (44.1 kHz, then 22.05 kHz) so the
/// delivered output proves which track was selected.
fn two_audio_mp4(f: &Fixture, seconds: &str) {
    f.media(
        "film.mp4",
        &[
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
            seconds,
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
            "-movflags",
            "+faststart",
        ],
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn original_playback_seek_resume_and_ordered_viewing() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "60");
    f.scan().await;
    let (timeline, item) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let (_, other) = f.device(&["default"], PLAYER).await;

    // Fresh viewing state and preferences.
    let viewing = f
        .get(
            &format!("/api/v2/profiles/default/timelines/{timeline}/viewing"),
            &auth,
        )
        .await;
    assert_eq!(viewing.status, StatusCode::OK, "{:?}", viewing.body);
    assert_eq!(viewing.body["revision"], "0");
    assert_eq!(viewing.body["position_ms"], 0);
    assert_eq!(viewing.headers["etag"], "\"r-0\"");
    let prefs = f.get("/api/v2/profiles/default/preferences", &auth).await;
    assert_eq!(prefs.status, StatusCode::OK, "{:?}", prefs.body);
    assert_eq!(prefs.body["quality_mode"], "auto");

    // Plan: a supported MP4 plays as the original over byte ranges.
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range", "hls"]),
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    assert_eq!(plan.body["status"], "ready");
    assert_eq!(plan.body["transport"], "http_range");
    assert_eq!(plan.body["operation"], "original");
    assert_eq!(plan.body["tracks"]["audio_track_id"], "a0");
    let token = plan.body["plan_token"].as_str().unwrap().to_owned();
    let file = plan.body["source"]["file_id"].as_str().unwrap().to_owned();

    // Admission needs an idempotency key and the planning principal.
    let create = json!({"plan_token": token, "start_ms": 0});
    let missing = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            None,
            create.clone(),
        )
        .await;
    problem(
        &missing,
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
    let stolen = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &other,
            Some("stolen-plan-key-0001"),
            create.clone(),
        )
        .await;
    problem(
        &stolen,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_plan_token",
    );
    let mut forged = token.clone();
    forged.pop();
    forged.push(if token.ends_with('0') { '1' } else { '0' });
    let forged = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("forged-plan-key-0001"),
            json!({"plan_token": forged, "start_ms": 0}),
        )
        .await;
    problem(
        &forged,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_plan_token",
    );

    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-original-0001"),
            create.clone(),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{:?}", admitted.body);
    let d = &admitted.body;
    let id = d["id"].as_str().unwrap().to_owned();
    assert_eq!(d["status"], "ready");
    assert_eq!(d["profile_id"], "default");
    assert_eq!(d["timeline_id"], timeline);
    assert_eq!(d["active"]["generation"], "1");
    assert_eq!(d["active"]["transport"], "http_range");
    assert_eq!(d["active"]["manifest_url"], Value::Null);
    assert_eq!(d["heartbeat_interval_seconds"], 10);
    let media = d["active"]["media_url"].as_str().unwrap().to_owned();
    assert!(media.starts_with(&format!("/api/v2/media/files/{file}/content?revision=")));

    // An exact retry replays the same delivery; another body is a conflict.
    let retry = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-original-0001"),
            create.clone(),
        )
        .await;
    assert_eq!(retry.status, StatusCode::CREATED, "{:?}", retry.body);
    assert_eq!(retry.body["id"], id);
    let changed = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-original-0001"),
            json!({"plan_token": token, "start_ms": 5000}),
        )
        .await;
    problem(&changed, StatusCode::CONFLICT, "idempotency_conflict");

    // Original bytes, by range, with the session's credential.
    let range = f
        .call(
            "GET",
            &media,
            None,
            &[("authorization", &auth), ("range", "bytes=0-1023")],
        )
        .await;
    assert_eq!(range.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.bytes.len(), 1024);
    assert_eq!(&range.bytes[4..8], b"ftyp");

    // Another principal cannot see or control the delivery.
    let path = format!("/api/v2/playback/delivery-sessions/{id}");
    problem(
        &f.get(&path, &other).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.post(
            &format!("{path}/heartbeat"),
            &other,
            None,
            json!({"active_generation":"1"}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Viewing session bound to this delivery.
    let session = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("viewing-create-0001"),
            json!({"delivery_id": id, "expected_viewing_revision": "0"}),
        )
        .await;
    assert_eq!(session.status, StatusCode::CREATED, "{:?}", session.body);
    let sid = session.body["id"].as_str().unwrap().to_owned();
    assert!(sid.ends_with(&format!("_{id}")), "{sid}");
    assert_eq!(session.body["delivery_id"], id);
    assert_eq!(session.body["timeline_id"], timeline);
    assert_eq!(session.body["sequence"], "0");
    let replay = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("viewing-create-0001"),
            json!({"delivery_id": id, "expected_viewing_revision": "0"}),
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body["id"], sid);

    let events = format!("/api/v2/playback/viewing-sessions/{sid}/events");
    let event = |seq: &str, position: u64, status: &str, generation: &str| {
        json!({"event_id": format!("event-{seq}"), "sequence": seq,
            "delivery_generation": generation, "position_ms": position, "status": status})
    };
    let ack = f
        .post(&events, &auth, None, event("1", 5000, "playing", "1"))
        .await;
    assert_eq!(ack.status, StatusCode::OK, "{:?}", ack.body);
    assert_eq!(ack.body["duplicate"], false);
    assert_eq!(ack.body["session"]["sequence"], "1");
    assert_eq!(ack.body["session"]["position_ms"], 5000);
    assert_eq!(ack.body["session"]["delivery_id"], id);
    assert_eq!(ack.body["viewing"]["position_ms"], 5000);
    let dup = f
        .post(&events, &auth, None, event("1", 5000, "playing", "1"))
        .await;
    assert_eq!(dup.status, StatusCode::OK);
    assert_eq!(dup.body["duplicate"], true);
    problem(
        &f.post(&events, &auth, None, event("1", 7000, "playing", "1"))
            .await,
        StatusCode::CONFLICT,
        "stale_sequence",
    );
    // A principal without the profile cannot write progress.
    let (_, kids_only) = f.device(&["kids"], PLAYER).await;
    problem(
        &f.post(&events, &kids_only, None, event("2", 9000, "playing", "1"))
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Seek: stage generation 2 at 30 s, retry the same change, then activate.
    let seek = json!({"kind":"seek","expected_generation":"1","position_ms":30000});
    let staged = f
        .post(
            &format!("{path}/changes"),
            &auth,
            Some("seek-change-0001"),
            seek.clone(),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{:?}", staged.body);
    assert_eq!(staged.body["replacement_mode"], "overlap");
    assert_eq!(staged.body["pending"]["generation"], "2");
    assert_eq!(staged.body["pending"]["status"], "ready");
    assert_eq!(staged.body["pending"]["requested_start_ms"], 30000);
    let again = f
        .post(
            &format!("{path}/changes"),
            &auth,
            Some("seek-change-0001"),
            seek,
        )
        .await;
    assert_eq!(again.status, StatusCode::ACCEPTED);
    assert_eq!(
        again.body["pending"]["generation"], "2",
        "replayed, not restaged"
    );
    assert_eq!(again.headers["idempotent-replayed"], "true");
    let activate = f
        .post(
            &format!("{path}/generations/2/activate"),
            &auth,
            Some("activate-two-0001"),
            json!({"expected_active_generation":"1"}),
        )
        .await;
    assert_eq!(activate.status, StatusCode::OK, "{:?}", activate.body);
    assert_eq!(activate.body["active"]["generation"], "2");
    assert_eq!(activate.body["active"]["requested_start_ms"], 30000);
    assert_eq!(activate.body["pending"], Value::Null);
    let duplicate = f
        .post(
            &format!("{path}/generations/2/activate"),
            &auth,
            Some("activate-two-0002"),
            json!({"expected_active_generation":"1"}),
        )
        .await;
    assert_eq!(
        duplicate.status,
        StatusCode::OK,
        "duplicate activation is acknowledged"
    );
    problem(
        &f.post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            json!({"active_generation":"1"}),
        )
        .await,
        StatusCode::CONFLICT,
        "generation_conflict",
    );
    let beat = f
        .post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            json!({"active_generation":"2"}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{:?}", beat.body);

    let paused = f
        .post(&events, &auth, None, event("2", 30500, "paused", "2"))
        .await;
    assert_eq!(paused.status, StatusCode::OK, "{:?}", paused.body);
    let resumed = f
        .get(
            &format!("/api/v2/profiles/default/timelines/{timeline}/viewing"),
            &auth,
        )
        .await;
    assert_eq!(resumed.body["position_ms"], 30500);
    assert_eq!(resumed.body["session_id"], sid);

    // Continue watching lists the title at its position, under its timeline.
    let cont = f
        .get("/api/v2/profiles/default/continue-watching", &auth)
        .await;
    assert_eq!(cont.status, StatusCode::OK, "{:?}", cont.body);
    assert_eq!(cont.body["items"][0]["item"]["id"], item);
    assert_eq!(cont.body["items"][0]["timeline"]["id"], timeline);
    assert_eq!(cont.body["items"][0]["viewing"]["position_ms"], 30500);

    // Closing ends control; the session's outbox may still drain.
    let closed = f
        .call("DELETE", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    let after = f.get(&path, &auth).await;
    assert!(
        after.status == StatusCode::NOT_FOUND || after.body["status"] == "closed",
        "{:?}",
        after.body
    );
    problem(
        &f.post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            json!({"active_generation":"2"}),
        )
        .await,
        StatusCode::CONFLICT,
        "delivery_closed",
    );
    let stopped = f
        .post(&events, &auth, None, event("3", 31000, "stopped", "2"))
        .await;
    assert_eq!(stopped.status, StatusCode::OK, "{:?}", stopped.body);
    assert_eq!(stopped.body["session"]["status"], "stopped");
    assert_eq!(stopped.body["session"]["delivery_id"], id);

    // Resume: a new plan and delivery start at the saved position.
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range"]),
        )
        .await;
    let resumed = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-resume-00001"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 31000}),
        )
        .await;
    assert_eq!(resumed.status, StatusCode::CREATED, "{:?}", resumed.body);
    assert_eq!(resumed.body["active"]["requested_start_ms"], 31000);
    let next = f
        .post("/api/v2/playback/viewing-sessions", &auth, Some("viewing-create-0002"),
            json!({"delivery_id": resumed.body["id"], "expected_viewing_revision": stopped.body["viewing"]["revision"]}))
        .await;
    assert_eq!(next.status, StatusCode::CREATED, "{:?}", next.body);
    assert_eq!(next.body["position_ms"], 31000);
    // The previous session was superseded and can no longer write.
    problem(
        &f.post(&events, &auth, None, event("4", 32000, "playing", "2"))
            .await,
        StatusCode::CONFLICT,
        "superseded_session",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn conversion_streams_hls_and_switches_audio_from_the_original() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "120");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;

    // A client without HLS cannot be offered a conversion.
    let none = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "convert", None, &["http_range"]),
        )
        .await;
    assert_eq!(none.body["status"], "blocked");
    assert_eq!(
        none.body["reason_codes"][0],
        "client_cannot_play_conversion"
    );
    // Strict original cannot honor a non-default audio track.
    let strict = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input(
                "default",
                &timeline,
                "original",
                Some("a1"),
                &["http_range", "hls"],
            ),
        )
        .await;
    assert_eq!(
        strict.body["reason_codes"][0],
        "stream_selection_requires_conversion"
    );
    let unknown = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input(
                "default",
                &timeline,
                "auto",
                Some("a9"),
                &["http_range", "hls"],
            ),
        )
        .await;
    problem(&unknown, StatusCode::UNPROCESSABLE_ENTITY, "unknown_track");

    // Start on the original, then switch to the second audio track: the
    // planner moves to a live conversion inside the same delivery.
    let original = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range", "hls"]),
        )
        .await;
    assert_eq!(original.body["transport"], "http_range");
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-switch-00001"),
            json!({"plan_token": original.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{:?}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/playback/delivery-sessions/{id}");

    let second = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input(
                "default",
                &timeline,
                "auto",
                Some("a1"),
                &["http_range", "hls"],
            ),
        )
        .await;
    assert_eq!(second.status, StatusCode::OK, "{:?}", second.body);
    assert_eq!(second.body["transport"], "hls");
    assert_eq!(second.body["operation"], "video_transcode");
    assert_eq!(second.body["tracks"]["audio_track_id"], "a1");
    assert_ne!(second.body["candidate_id"], original.body["candidate_id"]);
    let staged = f
        .post(&format!("{path}/changes"), &auth, Some("replan-audio-00001"),
            json!({"kind":"replan","expected_generation":"1","plan_token":second.body["plan_token"],"position_ms":12000}))
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{:?}", staged.body);
    assert_eq!(staged.body["pending"]["transport"], "hls");
    assert_eq!(staged.body["pending"]["generation"], "2");
    let ready = f
        .until(&id, &auth, |d| {
            d["pending"]["status"] == "ready" || d["pending"]["status"] == "failed"
        })
        .await;
    assert_eq!(ready["pending"]["status"], "ready", "{ready}");
    let master = ready["pending"]["manifest_url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(master, format!("/api/v2/streams/{id}/2/master.m3u8"));
    let activated = f
        .post(
            &format!("{path}/generations/2/activate"),
            &auth,
            Some("activate-hls-00001"),
            json!({"expected_active_generation":"1"}),
        )
        .await;
    assert_eq!(activated.status, StatusCode::OK, "{:?}", activated.body);
    assert_eq!(activated.body["active"]["transport"], "hls");
    let origin = activated.body["active"]["media_time_origin_ms"]
        .as_u64()
        .unwrap();
    assert!(origin <= 12000, "origin {origin} must not pass the request");

    // Streams follow the contract layout and need the owner's credential.
    let m = f
        .call("GET", &master, None, &[("authorization", &auth)])
        .await;
    assert_eq!(m.status, StatusCode::OK);
    let text = String::from_utf8(m.bytes).unwrap();
    assert!(text.contains("CODECS=\"avc1.64001f,mp4a.40.2\""), "{text}");
    assert!(text.contains("variants/main/index.m3u8"), "{text}");
    let (_, other) = f.device(&["default"], PLAYER).await;
    problem(
        &f.call("GET", &master, None, &[("authorization", &other)])
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let base = format!("/api/v2/streams/{id}/2");
    let variant = f
        .call(
            "GET",
            &format!("{base}/variants/main/index.m3u8"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(variant.status, StatusCode::OK);
    let variant = String::from_utf8(variant.bytes).unwrap();
    assert!(variant.contains("#EXT-X-MAP:URI=\"init.mp4\""), "{variant}");
    assert!(variant.contains("../../segments/0.m4s"), "{variant}");
    let init = f
        .call(
            "GET",
            &format!("{base}/variants/main/init.mp4"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(init.status, StatusCode::OK);
    let first = f
        .call(
            "GET",
            &format!("{base}/segments/0.m4s"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK);
    let fragment = f.dir.path().join("fragment.mp4");
    std::fs::write(&fragment, [init.bytes, first.bytes].concat()).unwrap();
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&fragment)
        .output()
        .unwrap();
    let streams: Value = serde_json::from_slice(&probe.stdout).unwrap();
    let streams = streams["streams"].as_array().unwrap();
    assert!(streams.iter().any(|s| s["codec_name"] == "h264"));
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    assert_eq!(audio.len(), 1);
    assert_eq!(
        audio[0]["sample_rate"], "22050",
        "the selected second audio track"
    );

    // Seek within the conversion: the new generation starts encoding at 60 s.
    let seek = f
        .post(
            &format!("{path}/changes"),
            &auth,
            Some("seek-hls-0000001"),
            json!({"kind":"seek","expected_generation":"2","position_ms":60000}),
        )
        .await;
    assert_eq!(seek.status, StatusCode::ACCEPTED, "{:?}", seek.body);
    assert_eq!(seek.body["pending"]["transport"], "hls");
    let ready = f
        .until(&id, &auth, |d| {
            d["pending"]["status"] == "ready" || d["pending"]["status"] == "failed"
        })
        .await;
    assert_eq!(ready["pending"]["status"], "ready", "{ready}");
    let start = ready["pending"]["available_start_ms"].as_u64().unwrap();
    assert!(start <= 60000 && ready["pending"]["available_end_ms"].as_u64().unwrap() > 60000);
    let closed = f
        .call("DELETE", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    problem(
        &f.call("GET", &master, None, &[("authorization", &auth)])
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_fences_deliveries_but_keeps_progress_and_drains_the_outbox() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let mut f = Fixture::new().await;
    two_audio_mp4(&f, "30");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range"]),
        )
        .await;
    let d = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-before-restart"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 0}),
        )
        .await;
    let id = d.body["id"].as_str().unwrap().to_owned();
    let s = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("view-before-restart1"),
            json!({"delivery_id": id, "expected_viewing_revision": "0"}),
        )
        .await;
    let sid = s.body["id"].as_str().unwrap().to_owned();
    let events = format!("/api/v2/playback/viewing-sessions/{sid}/events");
    let ack = f
        .post(&events, &auth, None,
            json!({"event_id":"e1","sequence":"1","delivery_generation":"1","position_ms":8000,"status":"playing"}))
        .await;
    assert_eq!(ack.status, StatusCode::OK, "{:?}", ack.body);

    f.restart().await;

    // The transport does not survive; the client must plan again.
    problem(
        &f.get(&format!("/api/v2/playback/delivery-sessions/{id}"), &auth)
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // A queued event from before the restart still drains with its binding.
    let drained = f
        .post(&events, &auth, None,
            json!({"event_id":"e2","sequence":"2","delivery_generation":"1","position_ms":9000,"status":"paused"}))
        .await;
    assert_eq!(drained.status, StatusCode::OK, "{:?}", drained.body);
    assert_eq!(drained.body["session"]["delivery_id"], id);
    assert_eq!(drained.body["session"]["timeline_id"], timeline);
    let viewing = f
        .get(
            &format!("/api/v2/profiles/default/timelines/{timeline}/viewing"),
            &auth,
        )
        .await;
    assert_eq!(viewing.body["position_ms"], 9000);
    // A plan issued before the restart is still signed with the same key and
    // admits a new delivery at the saved position.
    let again = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-after-restart1"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 9000}),
        )
        .await;
    assert_eq!(again.status, StatusCode::CREATED, "{:?}", again.body);
    assert_ne!(again.body["id"], id);
    assert_eq!(again.body["active"]["requested_start_ms"], 9000);
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_and_source_changes_end_control_immediately() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "30");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (device, auth) = f.device(&["default"], PLAYER).await;

    // Profile scope: a profile outside the grant is not found.
    let foreign = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("kids", &timeline, "auto", None, &["http_range"]),
        )
        .await;
    problem(&foreign, StatusCode::NOT_FOUND, "not_found");
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range"]),
        )
        .await;
    let token = plan.body["plan_token"].clone();
    let d = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-revocation-01"),
            json!({"plan_token": token, "start_ms": 0}),
        )
        .await;
    assert_eq!(d.status, StatusCode::CREATED, "{:?}", d.body);
    let path = format!(
        "/api/v2/playback/delivery-sessions/{}",
        d.body["id"].as_str().unwrap()
    );
    assert_eq!(f.get(&path, &auth).await.status, StatusCode::OK);

    // Removing the library from the device's policy ends control and admission.
    f.policy(&device, &[], PLAYER, "\"r-2\"").await;
    problem(
        &f.get(&path, &auth).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let readmit = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-revocation-02"),
            json!({"plan_token": token, "start_ms": 0}),
        )
        .await;
    problem(&readmit, StatusCode::NOT_FOUND, "not_found");

    // Restored scope: a changed source revision refuses the old plan.
    f.policy(&device, &[f.library.as_str()], PLAYER, "\"r-3\"")
        .await;
    assert_eq!(f.get(&path, &auth).await.status, StatusCode::OK);
    sqlx::query("UPDATE media_files SET revision='changed'")
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let stale = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-revocation-03"),
            json!({"plan_token": token, "start_ms": 0}),
        )
        .await;
    problem(&stale, StatusCode::CONFLICT, "source_revision_changed");

    // Losing playback:request ends control even with the library granted.
    f.policy(
        &device,
        &[f.library.as_str()],
        &["catalog:read", "viewing:write"],
        "\"r-4\"",
    )
    .await;
    problem(
        &f.get(&path, &auth).await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
}
