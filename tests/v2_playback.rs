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
        Self::start_with(dir, key, Default::default()).await
    }

    async fn start_with(
        dir: &Path,
        key: [u8; 32],
        settings: playscale::processing::Settings,
    ) -> Self {
        let db = db::connect(&dir.join("db.sqlite")).await.unwrap();
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let mut processing = playscale::processing::Runtime::new(dir.join("cache"), settings);
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
        if ["POST", "PUT"].contains(&method)
            && (path.contains("/viewing-sessions") || path.ends_with("/viewing"))
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("x-motion-viewing-epoch"))
        {
            let (server, restore) = playscale::v2::system::server_identity(&self.app)
                .await
                .unwrap();
            request = request.header("x-motion-viewing-epoch", format!("{server}:{restore}"));
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
        Self::with_settings(Default::default()).await
    }

    async fn with_copy_routes(copy_routes: bool) -> Self {
        Self::with_settings(playscale::processing::Settings {
            experimental_copy_routes: copy_routes,
            ..Default::default()
        })
        .await
    }

    async fn with_settings(settings: playscale::processing::Settings) -> Self {
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
        let server = Server::start_with(dir.path(), key, settings).await;
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
        let settings = self.server.app.processing.settings.clone();
        self.restart_with(settings).await;
    }

    async fn restart_with(&mut self, settings: playscale::processing::Settings) {
        self.server.stop.cancel();
        self.server = Server::start_with(self.dir.path(), self.key, settings).await;
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
        self.scan_library(&self.library).await;
    }

    async fn scan_library(&self, library: &str) {
        let app = &self.server.app;
        let row = db::enqueue_mode(app, library, false).await.unwrap();
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

    async fn assign_user(&self, device: &str) {
        let operator = bearer(OPERATOR);
        let row = self
            .get(&format!("/api/v2/devices/{device}"), &operator)
            .await;
        let policy = self
            .get(&format!("/api/v2/devices/{device}/policy"), &operator)
            .await;
        let mut ceiling = policy.body;
        ceiling.as_object_mut().unwrap().remove("revision");
        ceiling.as_object_mut().unwrap().remove("permissions");
        let user = self.post("/api/v2/users", &operator, Some(&format!("playback-owner-{}", uuid::Uuid::new_v4())),
            json!({"name":"Playback owner","disabled":false,"profile_ids":row.body["profile_ids"],"permissions":row.body["permissions"],"policy":ceiling})).await;
        assert_eq!(user.status, StatusCode::CREATED, "{}", user.body);
        let assigned = self
            .call(
                "PUT",
                &format!("/api/v2/devices/{device}/user"),
                Some(json!({"user_id":user.body["id"],"user_revision":user.body["revision"]})),
                &[
                    ("authorization", &operator),
                    ("if-match", row.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(assigned.status, StatusCode::OK, "{}", assigned.body);
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
    /// Retries a job at the revision a fresh read reports.
    async fn retry_job(&self, id: &str, auth: &str, key: &str) -> Reply {
        let reviewed = self.get(&format!("/api/v2/jobs/{id}"), auth).await;
        let etag = reviewed.headers["etag"].to_str().unwrap().to_owned();
        let headers = [
            ("authorization", auth),
            ("idempotency-key", key),
            ("if-match", etag.as_str()),
        ];
        self.call("POST", &format!("/api/v2/jobs/{id}/retry"), None, &headers)
            .await
    }
    async fn post(&self, path: &str, auth: &str, key: Option<&str>, body: Value) -> Reply {
        let mut headers = vec![("authorization", auth)];
        if let Some(key) = key {
            headers.push(("idempotency-key", key));
        }
        self.call("POST", path, Some(body), &headers).await
    }

    /// The exact source pin (file and revision) of the first original.
    async fn source(&self) -> Value {
        let (file_id, file_revision): (String, String) = sqlx::query_as(
            "SELECT id,revision FROM media_files WHERE generated=0 ORDER BY relative_path LIMIT 1",
        )
        .fetch_one(&self.server.app.db)
        .await
        .unwrap();
        json!({"file_id": file_id, "file_revision": file_revision})
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

/// `plan_input` with revision-local track pins and their exact source pin.
fn pinned_plan_input(
    profile: &str,
    timeline: &str,
    mode: &str,
    audio: &str,
    transports: &[&str],
    source: &Value,
) -> Value {
    let mut input = plan_input(profile, timeline, mode, Some(audio), transports);
    input["source"] = source.clone();
    input
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
    // The same sequence with other content, or a skipped sequence, is refused.
    problem(
        &f.post(&events, &auth, None, event("1", 7000, "playing", "1"))
            .await,
        StatusCode::CONFLICT,
        "event_conflict",
    );
    problem(
        &f.post(&events, &auth, None, event("3", 7000, "playing", "1"))
            .await,
        StatusCode::CONFLICT,
        "sequence_gap",
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
    // An activation key names one request: another body under it conflicts;
    // the exact retry replays its acknowledgement.
    problem(
        &f.post(
            &format!("{path}/generations/2/activate"),
            &auth,
            Some("activate-two-0001"),
            json!({"expected_active_generation":"2"}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    );
    let replayed = f
        .post(
            &format!("{path}/generations/2/activate"),
            &auth,
            Some("activate-two-0001"),
            json!({"expected_active_generation":"1"}),
        )
        .await;
    assert_eq!(replayed.status, StatusCode::OK, "{:?}", replayed.body);
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
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
            pinned_plan_input(
                "default",
                &timeline,
                "original",
                "a1",
                &["http_range", "hls"],
                &f.source().await,
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
            pinned_plan_input(
                "default",
                &timeline,
                "auto",
                "a9",
                &["http_range", "hls"],
                &f.source().await,
            ),
        )
        .await;
    problem(&unknown, StatusCode::UNPROCESSABLE_ENTITY, "unknown_track");
    // Revision-local track pins need the exact source they index.
    problem(
        &f.post(
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
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_plan_input",
    );

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
            pinned_plan_input(
                "default",
                &timeline,
                "auto",
                "a1",
                &["http_range", "hls"],
                &f.source().await,
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
    assert!(text.contains("CODECS=\"avc1."), "{text}");
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
    let avcc = init
        .bytes
        .windows(4)
        .position(|w| w == b"avcC")
        .expect("H.264 initialization");
    let actual = format!(
        "avc1.{:02x}{:02x}{:02x}",
        init.bytes[avcc + 5],
        init.bytes[avcc + 6],
        init.bytes[avcc + 7]
    );
    assert!(
        text.contains(&format!("CODECS=\"{actual},mp4a.40.2\"")),
        "{text}"
    );
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
    let live_mode = monitored(&f, &id).await["mode"].clone();
    assert_ne!(live_mode, "direct");
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
    // History keeps how the bytes were produced after the generations that
    // produced them are cleaned up.
    assert_eq!(finished(&f, &id).await["mode"], live_mode);
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

    let ticket = f
        .post(
            "/api/v2/content-access",
            &auth,
            Some("restart-ticket-access"),
            json!({"purpose":"playback","delivery_id":id,"generation":"1","ttl_seconds":600}),
        )
        .await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{}", ticket.body);
    let ticket_url = ticket.body["url"].as_str().unwrap().to_owned();
    assert_eq!(
        f.call("GET", &ticket_url, None, &[]).await.status,
        StatusCode::OK
    );
    let identity_before = f.get("/api/v2/system/capabilities", &auth).await.body;
    f.restart().await;
    assert_eq!(
        f.call("GET", &ticket_url, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let identity_after = f.get("/api/v2/system/capabilities", &auth).await.body;
    assert_eq!(identity_before["server_id"], identity_after["server_id"]);
    assert_eq!(
        identity_before["restore_epoch"],
        identity_after["restore_epoch"]
    );
    assert_ne!(
        identity_before["server_epoch"],
        identity_after["server_epoch"]
    );

    // The transport does not survive; its owner reads the fenced state and
    // must plan again. Closing it again is an acknowledged no-op.
    let path = format!("/api/v2/playback/delivery-sessions/{id}");
    let fenced = f.get(&path, &auth).await;
    assert_eq!(fenced.status, StatusCode::OK, "{:?}", fenced.body);
    assert!(
        ["closed", "interrupted"].contains(&fenced.body["status"].as_str().unwrap()),
        "{:?}",
        fenced.body
    );
    assert_eq!(fenced.body["timeline_id"], timeline);
    problem(
        &f.post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            json!({"active_generation":"1"}),
        )
        .await,
        StatusCode::CONFLICT,
        "delivery_closed",
    );
    let closed = f
        .call("DELETE", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT, "{:?}", closed.body);
    // A queued event from before the restart still drains with its binding.
    let history_epoch = format!(
        "{}:{}",
        identity_before["server_id"].as_str().unwrap(),
        identity_before["restore_epoch"].as_str().unwrap()
    );
    let drained = f.call("POST", &events,
        Some(json!({"event_id":"e2","sequence":"2","delivery_generation":"1","position_ms":9000,"status":"paused"})),
        &[("authorization", &auth), ("x-motion-viewing-epoch", &history_epoch)]).await;
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

    // Restore a real SQLite snapshot, including the SAME session and receipts.
    // An already-open page must not replay across the changed durable identity.
    let backup = f.dir.path().join("history-backup.sqlite");
    sqlx::query("VACUUM INTO ?")
        .bind(backup.to_str().unwrap())
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let restored_dir = tempfile::tempdir().unwrap();
    db::restore_into(&backup, &restored_dir.path().join("db.sqlite"))
        .await
        .unwrap();
    let restored = Server::start(restored_dir.path(), f.key).await;
    let (_, restored_epoch) = playscale::v2::system::server_identity(&restored.app)
        .await
        .unwrap();
    assert_ne!(
        restored_epoch,
        identity_before["restore_epoch"].as_str().unwrap()
    );
    let denied = restored
        .call("GET", "/api/v2/profiles", None, &[("authorization", &auth)])
        .await;
    assert_eq!(
        denied.status,
        StatusCode::UNAUTHORIZED,
        "restored credentials must be paired again"
    );
    // The operator can still exercise the independent history-epoch fence.
    let auth = bearer(OPERATOR);
    for (method, target, body, extra) in [
        (
            "POST",
            events.clone(),
            json!({"event_id":"e2","sequence":"2","delivery_generation":"1","position_ms":9000,"status":"paused"}),
            ("idempotency-key", "unused-retry"),
        ),
        (
            "POST",
            events.clone(),
            json!({"event_id":"e3","sequence":"3","delivery_generation":"1","position_ms":10000,"status":"paused"}),
            ("idempotency-key", "unused-new"),
        ),
        (
            "POST",
            "/api/v2/playback/viewing-sessions".into(),
            json!({"delivery_id":id,"expected_viewing_revision":"0"}),
            ("idempotency-key", "view-before-restart1"),
        ),
        (
            "PUT",
            format!("/api/v2/playback/viewing-sessions/{sid}/delivery"),
            json!({"delivery_id":id}),
            ("if-match", "\"r-3\""),
        ),
        (
            "PUT",
            format!("/api/v2/profiles/default/timelines/{timeline}/viewing"),
            json!({"manual_watched":true,"reset_position":false}),
            ("if-match", "\"r-3\""),
        ),
    ] {
        let rejected = restored
            .call(
                method,
                &target,
                Some(body),
                &[
                    ("authorization", &auth),
                    ("x-motion-viewing-epoch", &history_epoch),
                    extra,
                ],
            )
            .await;
        problem(&rejected, StatusCode::CONFLICT, "viewing_epoch_changed");
    }
    let malformed = restored.call("POST", &events, Some(json!({"event_id":"e3","sequence":"3","delivery_generation":"1","position_ms":10000,"status":"paused"})),
        &[("authorization", &auth), ("x-motion-viewing-epoch", "")]).await;
    problem(
        &malformed,
        StatusCode::PRECONDITION_REQUIRED,
        "viewing_epoch_required",
    );
    let position: i64 =
        sqlx::query_scalar("SELECT position_ms FROM timeline_viewing WHERE timeline_id=?")
            .bind(&timeline)
            .fetch_one(&restored.app.db)
            .await
            .unwrap();
    assert_eq!(position, 9000);
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
    let create = json!({"delivery_id": d.body["id"], "expected_viewing_revision": "0"});
    let session = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("revocation-viewing-01"),
            create.clone(),
        )
        .await;
    assert_eq!(session.status, StatusCode::CREATED, "{:?}", session.body);

    // Removing the library from the device's policy ends control and admission.
    f.policy(&device, &[], PLAYER, "\"r-2\"").await;
    // An acknowledged creation no longer replays the session to this caller.
    problem(
        &f.post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("revocation-viewing-01"),
            create,
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
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
    // A fresh plan never uses content the version binding did not review.
    let unreviewed = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range"]),
        )
        .await;
    assert_eq!(unreviewed.status, StatusCode::OK, "{:?}", unreviewed.body);
    assert_eq!(unreviewed.body["status"], "blocked");
    assert_eq!(
        unreviewed.body["reason_codes"],
        json!(["source_unavailable"])
    );

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

/// Admit a fresh original delivery of `timeline` for `auth`.
async fn admit_original(
    f: &Fixture,
    auth: &str,
    timeline: &str,
    key: &str,
    start_ms: u64,
) -> Value {
    let plan = f
        .post(
            "/api/v2/playback/plans",
            auth,
            None,
            plan_input("default", timeline, "auto", None, &["http_range"]),
        )
        .await;
    assert_eq!(plan.body["status"], "ready", "{:?}", plan.body);
    let d = f
        .post(
            "/api/v2/playback/delivery-sessions",
            auth,
            Some(key),
            json!({"plan_token": plan.body["plan_token"], "start_ms": start_ms}),
        )
        .await;
    assert_eq!(d.status, StatusCode::CREATED, "{:?}", d.body);
    d.body
}

#[tokio::test(flavor = "multi_thread")]
async fn viewing_authority_rebinds_fences_overrides_and_resolves_release_order() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "30");
    f.media(
        "sequel.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-movflags",
            "+faststart",
        ],
    );
    f.scan().await;
    let timelines: Vec<(String, String)> = sqlx::query_as(
        "SELECT t.id,f.relative_path FROM timelines t JOIN media_files f ON f.edition_id=t.edition_id ORDER BY f.relative_path",
    )
    .fetch_all(&f.server.app.db)
    .await
    .unwrap();
    let film = timelines
        .iter()
        .find(|t| t.1 == "film.mp4")
        .unwrap()
        .0
        .clone();
    let sequel = timelines
        .iter()
        .find(|t| t.1 == "sequel.mp4")
        .unwrap()
        .0
        .clone();
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let view_path = format!("/api/v2/profiles/default/timelines/{film}/viewing");

    let first = admit_original(&f, &auth, &film, "rebind-admit-0001", 0).await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    let session = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("rebind-viewing-0001"),
            json!({"delivery_id": first_id, "expected_viewing_revision": "0"}),
        )
        .await;
    assert_eq!(session.status, StatusCode::CREATED, "{:?}", session.body);
    assert_eq!(session.body["manual_epoch"], "0");
    assert_eq!(session.headers["etag"], "\"r-1\"");
    let sid = session.body["id"].as_str().unwrap().to_owned();
    let events = format!("/api/v2/playback/viewing-sessions/{sid}/events");
    let event = |id: &str, seq: &str, position: u64, status: &str| {
        json!({"event_id": id, "sequence": seq, "delivery_generation": "1",
            "position_ms": position, "status": status})
    };
    let one = f
        .post(&events, &auth, None, event("e1", "1", 5000, "playing"))
        .await;
    assert_eq!(one.status, StatusCode::OK, "{:?}", one.body);
    // An event identity is used once per session.
    problem(
        &f.post(&events, &auth, None, event("e1", "2", 6000, "playing"))
            .await,
        StatusCode::CONFLICT,
        "event_conflict",
    );

    // Rebind to a second delivery of the same timeline: same session identity,
    // sequence and progress; the session revision is the precondition.
    let second = admit_original(&f, &auth, &film, "rebind-admit-0002", 5000).await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    let rebind = format!("/api/v2/playback/viewing-sessions/{sid}/delivery");
    let body = json!({"delivery_id": second_id});
    let missing = f
        .call(
            "PUT",
            &rebind,
            Some(body.clone()),
            &[("authorization", &auth)],
        )
        .await;
    problem(
        &missing,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    let stale = f
        .call(
            "PUT",
            &rebind,
            Some(body.clone()),
            &[("authorization", &auth), ("if-match", "\"r-1\"")],
        )
        .await;
    problem(
        &stale,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let tag = one.headers["etag"].to_str().unwrap().to_owned();
    let moved = f
        .call(
            "PUT",
            &rebind,
            Some(body.clone()),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{:?}", moved.body);
    assert_eq!(moved.body["id"], sid);
    assert_eq!(moved.body["delivery_id"], second_id);
    assert_eq!(moved.body["sequence"], "1");
    assert_eq!(moved.body["position_ms"], 5000);
    let two = f
        .post(&events, &auth, None, event("e2", "2", 6000, "playing"))
        .await;
    assert_eq!(two.status, StatusCode::OK, "{:?}", two.body);
    assert_eq!(two.body["session"]["delivery_id"], second_id);

    // A change key names one request: a different body under it is refused.
    let changes = format!("/api/v2/playback/delivery-sessions/{second_id}/changes");
    let seek = f
        .post(
            &changes,
            &auth,
            Some("rebind-change-0001"),
            json!({"kind":"seek","expected_generation":"1","position_ms":8000}),
        )
        .await;
    assert_eq!(seek.status, StatusCode::ACCEPTED, "{:?}", seek.body);
    problem(
        &f.post(
            &changes,
            &auth,
            Some("rebind-change-0001"),
            json!({"kind":"seek","expected_generation":"1","position_ms":9000}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    );

    // A manual override advances the epoch and fences the session; an exact
    // retry of an acknowledged event still replays.
    let current = f.get(&view_path, &auth).await;
    assert_eq!(current.body["revision"], two.body["viewing"]["revision"]);
    let tag = current.headers["etag"].to_str().unwrap().to_owned();
    let watched = f
        .call(
            "PUT",
            &view_path,
            Some(json!({"manual_watched": true, "reset_position": false})),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(watched.status, StatusCode::OK, "{:?}", watched.body);
    assert_eq!(watched.body["manual_epoch"], "1");
    assert_eq!(watched.body["watched"], true);
    assert_eq!(watched.body["session_id"], Value::Null);
    assert_eq!(watched.body["position_ms"], 6000);
    problem(
        &f.post(&events, &auth, None, event("e3", "3", 7000, "playing"))
            .await,
        StatusCode::CONFLICT,
        "superseded_session",
    );
    let retry = f
        .post(&events, &auth, None, event("e2", "2", 6000, "playing"))
        .await;
    assert_eq!(retry.status, StatusCode::OK, "{:?}", retry.body);
    assert_eq!(retry.body["duplicate"], true);
    let fenced = f
        .get(&format!("/api/v2/playback/viewing-sessions/{sid}"), &auth)
        .await;
    assert_eq!(fenced.body["status"], "superseded");
    problem(
        &f.call(
            "PUT",
            &rebind,
            Some(body),
            &[
                ("authorization", &auth),
                ("if-match", fenced.headers["etag"].to_str().unwrap()),
            ],
        )
        .await,
        StatusCode::CONFLICT,
        "superseded_session",
    );
    let cont = f
        .get("/api/v2/profiles/default/continue-watching", &auth)
        .await;
    assert_eq!(cont.body["items"], json!([]), "watched is not resumable");

    // Clearing the override and the position is another epoch; a stale
    // expected revision cannot start a session.
    let tag = watched.headers["etag"].to_str().unwrap().to_owned();
    let cleared = f
        .call(
            "PUT",
            &view_path,
            Some(json!({"manual_watched": null, "reset_position": true})),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{:?}", cleared.body);
    assert_eq!(cleared.body["manual_epoch"], "2");
    assert_eq!(cleared.body["position_ms"], 0);
    assert_eq!(cleared.body["watched"], false);
    problem(
        &f.post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("rebind-viewing-0002"),
            json!({"delivery_id": second_id, "expected_viewing_revision": "1"}),
        )
        .await,
        StatusCode::CONFLICT,
        "viewing_revision_conflict",
    );

    // The profile's completion threshold drives automatic, sticky completion.
    let prefs = f.get("/api/v2/profiles/default/preferences", &auth).await;
    assert_eq!(
        prefs.body["max_bitrate_bps"],
        Value::Null,
        "no limit by default"
    );
    let mut document = prefs.body.clone();
    // A streaming limit is a positive decimal string.
    for bad in [
        json!("0"),
        json!("08"),
        json!(8000000),
        json!("8e6"),
        json!(u64::MAX.to_string()),
    ] {
        let mut invalid = document.clone();
        invalid["max_bitrate_bps"] = bad;
        let refused = f
            .call(
                "PUT",
                "/api/v2/profiles/default/preferences",
                Some(invalid),
                &[
                    ("authorization", &auth),
                    ("if-match", prefs.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert!(refused.status.is_client_error(), "{:?}", refused.body);
    }
    let mut original_capped = document.clone();
    original_capped["quality_mode"] = json!("original");
    original_capped["max_bitrate_bps"] = json!("8000000");
    let refused = f
        .call(
            "PUT",
            "/api/v2/profiles/default/preferences",
            Some(original_capped),
            &[
                ("authorization", &auth),
                ("if-match", prefs.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert!(refused.status.is_client_error());
    document["max_bitrate_bps"] = json!("8000000");
    document["completion_percent"] = json!(50);
    let replaced = f
        .call(
            "PUT",
            "/api/v2/profiles/default/preferences",
            Some(document.clone()),
            &[
                ("authorization", &auth),
                ("if-match", prefs.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    assert_eq!(replaced.body["max_bitrate_bps"], "8000000");
    assert_eq!(
        f.get("/api/v2/profiles/default/preferences", &auth)
            .await
            .body["max_bitrate_bps"],
        "8000000"
    );
    problem(
        &f.call(
            "PUT",
            "/api/v2/profiles/default/preferences",
            Some(document),
            &[
                ("authorization", &auth),
                ("if-match", prefs.headers["etag"].to_str().unwrap()),
            ],
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let fresh = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("rebind-viewing-0003"),
            json!({"delivery_id": second_id, "expected_viewing_revision": cleared.body["revision"]}),
        )
        .await;
    assert_eq!(fresh.status, StatusCode::CREATED, "{:?}", fresh.body);
    assert_eq!(fresh.body["manual_epoch"], "2");
    let fresh_events = format!(
        "/api/v2/playback/viewing-sessions/{}/events",
        fresh.body["id"].as_str().unwrap()
    );
    let half = f
        .post(
            &fresh_events,
            &auth,
            None,
            event("f1", "1", 16_000, "playing"),
        )
        .await;
    assert_eq!(half.status, StatusCode::OK, "{:?}", half.body);
    assert_eq!(half.body["viewing"]["watched"], true);
    let back = f
        .post(
            &fresh_events,
            &auth,
            None,
            event("f2", "2", 1_000, "paused"),
        )
        .await;
    assert_eq!(
        back.body["viewing"]["watched"], true,
        "completion is sticky"
    );

    // Release order (fixture: timelines placed by direct SQL until a catalog
    // placement writer is merged). Unplaced timelines have no next.
    let next = |t: &str| format!("/api/v2/profiles/default/timelines/{t}/next");
    let none = f.get(&next(&film), &auth).await;
    assert_eq!(none.status, StatusCode::OK, "{:?}", none.body);
    assert_eq!(none.body["items"], json!([]));
    for (timeline, position) in [(&film, 3), (&sequel, 7)] {
        sqlx::query(
            "UPDATE timelines SET order_group_id='fixture-aired',order_position=? WHERE id=?",
        )
        .bind(position)
        .bind(timeline)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    }
    let ordered = f.get(&next(&film), &auth).await;
    assert_eq!(ordered.body["items"][0]["id"], sequel, "{:?}", ordered.body);
    assert_eq!(ordered.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(f.get(&next(&sequel), &auth).await.body["items"], json!([]));

    // Conversions never exceed client limits; blocked plans pin nothing.
    let mut limited = plan_input("default", &film, "convert", None, &["hls"]);
    limited["quality"]["max_height"] = json!(120);
    let blocked = f.post("/api/v2/playback/plans", &auth, None, limited).await;
    assert_eq!(blocked.status, StatusCode::OK, "{:?}", blocked.body);
    assert_eq!(blocked.body["status"], "blocked");
    assert_eq!(
        blocked.body["reason_codes"],
        json!(["conversion_exceeds_quality_limit"])
    );
    for field in [
        "plan_token",
        "candidate_id",
        "version_id",
        "source",
        "transport",
        "operation",
    ] {
        assert_eq!(blocked.body[field], Value::Null, "{field}");
    }
    let mut capped = plan_input("default", &film, "convert", None, &["hls"]);
    capped["quality"]["max_bitrate_bps"] = json!("50000000");
    let capped = f.post("/api/v2/playback/plans", &auth, None, capped).await;
    // A capped conversion keeps (and measures) the limit.
    assert_eq!(
        capped.body["operation"], "video_transcode",
        "{}",
        capped.body
    );
}

/// Migration 0032 attributes legacy work-keyed progress only where the
/// timeline is certain: an exact legacy attribution, or a work with exactly
/// one timeline. Ambiguous progress stays unattributed; legacy rows remain.
#[tokio::test]
async fn timeline_viewing_backfill_attributes_only_certain_progress() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let dir = tempfile::tempdir().unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(dir.path().join("db.sqlite"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let all = sqlx::migrate!("./migrations");
    let mut before = sqlx::migrate!("./migrations");
    before.migrations = all
        .migrations
        .iter()
        .filter(|m| m.version < 32)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    before.run(&pool).await.unwrap();
    for statement in [
        // single: one timeline; split: two timelines (ambiguous); exact: two
        // timelines with an exact legacy attribution to the second.
        "INSERT INTO items(id,title,kind) VALUES ('single','S','video'),('split','P','video'),('exact','E','video')",
        "INSERT INTO editions(id,item_id,label) VALUES ('single-e','single',''),('split-a','split',''),('split-b','split',''),('exact-a','exact',''),('exact-b','exact','')",
        "INSERT INTO timelines(id,edition_id) VALUES ('single-t','single-e'),('split-ta','split-a'),('split-tb','split-b'),('exact-ta','exact-a'),('exact-tb','exact-b')",
        "INSERT INTO progress(profile_id,item_id,position_seconds,updated_at) VALUES ('default','single',12.3456,100),('default','split',40,200),('default','exact',7.5,300)",
        "INSERT INTO viewing_state(profile_id,item_id,automatic_watched,manual_watched) VALUES ('default','single',1,0)",
        "INSERT INTO catalog_receipts(id,kind,created_at,document_json) VALUES ('r','legacy',0,'{}')",
        "INSERT INTO legacy_progress_attribution(profile_id,item_id,outcome,timeline_id,candidates_json,receipt_id) VALUES ('default','exact','exact','exact-tb','[]','r'),('default','split','ambiguous',NULL,'[]','r')",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
    all.run(&pool).await.unwrap();
    let rows: Vec<(String, i64, bool, Option<bool>, i64, i64)> = sqlx::query_as(
        "SELECT timeline_id,position_ms,automatic_watched,manual_watched,revision,updated_at FROM timeline_viewing ORDER BY timeline_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("exact-tb".into(), 7500, false, None, 0, 300),
            ("single-t".into(), 12346, true, Some(false), 0, 100),
        ]
    );
    let legacy: i64 = sqlx::query_scalar("SELECT count(*) FROM progress")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(legacy, 3, "legacy rows are retained");
}

/// A timeline whose original versions sit in two libraries plays for a
/// principal granted either library: an unreadable first copy is skipped.
#[tokio::test(flavor = "multi_thread")]
async fn planning_skips_originals_outside_the_callers_scope() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "10");
    f.scan().await;
    let other_root = f.dir.path().join("other");
    std::fs::create_dir(&other_root).unwrap();
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-t",
            "8",
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-movflags",
            "+faststart",
        ])
        .arg(other_root.join("cut.mp4"))
        .status()
        .unwrap();
    assert!(status.success());
    let other = db::add_library(&f.server.app.db, "Other", &other_root)
        .await
        .unwrap()
        .id;
    f.scan_library(&other).await;
    // Fixture: make the second library's file another original version of
    // the first library's timeline (an operator edition reassignment).
    let db = &f.server.app.db;
    let (timeline, edition): (String, String) = sqlx::query_as(
        "SELECT t.id,t.edition_id FROM timelines t JOIN media_files f ON f.edition_id=t.edition_id WHERE f.relative_path='film.mp4'",
    )
    .fetch_one(db)
    .await
    .unwrap();
    let (file, version): (String, String) = sqlx::query_as(
        "SELECT f.id,b.version_id FROM media_files f JOIN version_files b ON b.file_id=f.id WHERE f.relative_path='cut.mp4'",
    )
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query("UPDATE media_files SET edition_id=? WHERE id=?")
        .bind(&edition)
        .bind(&file)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_versions SET timeline_id=? WHERE id=?")
        .bind(&timeline)
        .bind(&version)
        .execute(db)
        .await
        .unwrap();
    for (library, expected) in [(f.library.clone(), "film.mp4"), (other.clone(), "cut.mp4")] {
        let (device, auth) = f.device(&["default"], PLAYER).await;
        f.policy(&device, &[library.as_str()], PLAYER, "\"r-2\"")
            .await;
        let plan = f
            .post(
                "/api/v2/playback/plans",
                &auth,
                None,
                plan_input("default", &timeline, "auto", None, &["http_range"]),
            )
            .await;
        assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
        assert_eq!(plan.body["status"], "ready", "{:?}", plan.body);
        let path: String = sqlx::query_scalar("SELECT relative_path FROM media_files WHERE id=?")
            .bind(plan.body["source"]["file_id"].as_str().unwrap())
            .fetch_one(db)
            .await
            .unwrap();
        assert_eq!(path, expected);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn playback_only_observations_advance_live_conversion_and_revocation_stops_worker() {
    live_conversion_access_withdrawal("device").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn administrator_user_narrowing_stops_live_conversion() {
    live_conversion_access_withdrawal("narrow").await;
}
#[tokio::test(flavor = "multi_thread")]
async fn administrator_user_disable_stops_live_conversion() {
    live_conversion_access_withdrawal("disable").await;
}
async fn live_conversion_access_withdrawal(action: &str) {
    assert!(
        tools_available(),
        "this qualification requires ffmpeg and ffprobe"
    );
    let f = Fixture::new().await;
    two_audio_mp4(&f, "180");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (device, auth) = f
        .device(&["default"], &["catalog:read", "playback:request"])
        .await;
    if action != "device" {
        f.assign_user(&device).await;
    }
    let caps = f.get("/api/v2/system/capabilities", &auth).await;
    assert_eq!(caps.status, StatusCode::OK);
    for id in ["catalog.v2", "playback.v2"] {
        let feature = caps.body["features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .unwrap();
        assert_eq!(feature["implemented"], true);
        assert_eq!(feature["enabled"], true);
        assert_eq!(feature["qualification"], "unqualified");
    }
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "convert", None, &["hls"]),
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{}", plan.body);
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("playback-only-live"),
            json!({"plan_token":plan.body["plan_token"],"start_ms":0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap();
    let path = format!("/api/v2/playback/delivery-sessions/{id}");
    let ready = f
        .until(id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 45000
        })
        .await;
    assert!(ready["active"]["available_end_ms"].as_u64().unwrap() < 180000);
    let denied = f
        .post(
            "/api/v2/playback/viewing-sessions",
            &auth,
            Some("no-history-write"),
            json!({"delivery_id":id,"expected_viewing_revision":"0"}),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let beat = |sequence: &str, position, generation: &str| json!({"active_generation":generation,"playhead":{"sequence":sequence,"position_ms":position}});
    let first = f
        .post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            beat("2", 90000, "1"),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let duplicate = f
        .post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            beat("2", 90000, "1"),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::OK);
    let conflict = f
        .post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            beat("2", 80000, "1"),
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    let stale = f
        .post(&format!("{path}/heartbeat"), &auth, None, beat("1", 0, "1"))
        .await;
    assert_eq!(stale.status, StatusCode::OK);
    let wrong = f
        .post(&format!("{path}/heartbeat"), &auth, None, beat("3", 0, "2"))
        .await;
    assert_eq!(wrong.status, StatusCode::CONFLICT);
    let advanced = f
        .until(id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 100000
        })
        .await;
    let manifest = advanced["active"]["manifest_url"].as_str().unwrap();
    assert_eq!(f.get(manifest, &auth).await.status, StatusCode::OK);
    let backward = f
        .post(
            &format!("{path}/heartbeat"),
            &auth,
            None,
            beat("3", 1000, "1"),
        )
        .await;
    assert_eq!(backward.status, StatusCode::OK);
    for table in [
        "timeline_viewing",
        "viewing_sessions",
        "viewing_events",
        "progress",
        "viewing_state",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "playhead wrote {table}");
    }
    let operator = bearer(OPERATOR);
    let row = f.get(&format!("/api/v2/devices/{device}"), &operator).await;
    let start = std::time::Instant::now();
    if action == "device" {
        let revoked = f
            .call(
                "DELETE",
                &format!("/api/v2/devices/{device}"),
                None,
                &[
                    ("authorization", &operator),
                    ("if-match", row.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    } else {
        let path = format!("/api/v2/users/{}", row.body["user_id"].as_str().unwrap());
        let user = f.get(&path, &operator).await;
        let mut edit = user.body.clone();
        edit.as_object_mut().unwrap().remove("id");
        edit.as_object_mut().unwrap().remove("revision");
        if action == "disable" {
            edit["disabled"] = json!(true);
        } else {
            edit["permissions"] = json!(["catalog:read"]);
        }
        let result = f
            .call(
                "PUT",
                &path,
                Some(edit),
                &[
                    ("authorization", &operator),
                    ("if-match", user.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(result.status, StatusCode::OK, "{}", result.body);
    }
    assert_eq!(
        f.get(manifest, &auth).await.status,
        if action == "narrow" {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        }
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.server.app.processing.deliveries.is_live(id) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("revoked delivery retained its worker or output");
    println!(
        "revoked live delivery worker and output released in {:?}",
        start.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn generation_tickets_cover_hls_children_but_never_widen_authority() {
    assert!(tools_available());
    let f = Fixture::new().await;
    two_audio_mp4(&f, "120");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (device, auth) = f
        .device(&["default"], &["catalog:read", "playback:request"])
        .await;
    let (_, other) = f.device(&["default"], PLAYER).await;
    for mode in ["original", "convert"] {
        let plan = f
            .post(
                "/api/v2/playback/plans",
                &auth,
                None,
                plan_input("default", &timeline, mode, None, &["http_range", "hls"]),
            )
            .await;
        assert_eq!(plan.body["status"], "ready", "{}", plan.body);
        let admitted = f
            .post(
                "/api/v2/playback/delivery-sessions",
                &auth,
                Some(&format!("ticket-delivery-{mode}")),
                json!({"plan_token":plan.body["plan_token"],"start_ms":0}),
            )
            .await;
        assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
        let id = admitted.body["id"].as_str().unwrap();
        f.until(id, &auth, |d| d["active"]["status"] == "active")
            .await;
        let input = json!({"purpose":"playback","delivery_id":id,"generation":"1","file":null,"download_id":null,"ttl_seconds":60});
        assert_eq!(
            f.post(
                "/api/v2/content-access",
                &other,
                Some(&format!("stolen-ticket-{mode}")),
                input.clone()
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
        let key = format!("generation-ticket-{mode}");
        let ticket = f
            .post("/api/v2/content-access", &auth, Some(&key), input.clone())
            .await;
        assert_eq!(ticket.status, StatusCode::CREATED, "{}", ticket.body);
        assert_eq!(
            f.post("/api/v2/content-access", &auth, Some(&key), input.clone())
                .await
                .body,
            ticket.body
        );
        let url = ticket.body["url"].as_str().unwrap();
        let token = url.split("ticket=").nth(1).unwrap();
        let response = f.call("GET", url, None, &[]).await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(f.call("HEAD", url, None, &[]).await.status, StatusCode::OK);
        if mode == "convert" {
            let master = String::from_utf8(response.bytes).unwrap();
            let variant = master
                .lines()
                .find(|l| !l.starts_with('#') && !l.is_empty())
                .unwrap();
            assert!(variant.contains("?ticket="));
            let base = url::Url::parse(&format!("http://motion.test{url}")).unwrap();
            let variant_url = base.join(variant).unwrap();
            let reply = f
                .call(
                    "GET",
                    &format!("{}?{}", variant_url.path(), variant_url.query().unwrap()),
                    None,
                    &[],
                )
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
            let playlist = String::from_utf8(reply.bytes).unwrap();
            let init = playlist
                .lines()
                .find_map(|l| {
                    l.strip_prefix("#EXT-X-MAP:URI=\"")
                        .and_then(|s| s.split('"').next())
                })
                .unwrap();
            for child in std::iter::once(init).chain(
                playlist
                    .lines()
                    .filter(|l| !l.starts_with('#') && !l.is_empty()),
            ) {
                assert!(child.contains("?ticket="));
                let child = variant_url.join(child).unwrap();
                assert_eq!(
                    f.call(
                        "GET",
                        &format!("{}?{}", child.path(), child.query().unwrap()),
                        None,
                        &[]
                    )
                    .await
                    .status,
                    StatusCode::OK
                );
            }
        }
        let source = f.source().await;
        let widened = format!(
            "/api/v2/media/files/{}/content?ticket={token}",
            source["file_id"].as_str().unwrap()
        );
        assert_eq!(
            f.call("GET", &widened, None, &[]).await.status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.call("GET", &url.replace("/1/", "/2/"), None, &[])
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            f.call(
                "POST",
                &format!("/api/v2/playback/delivery-sessions/{id}/heartbeat?ticket={token}"),
                Some(json!({"active_generation":"1"})),
                &[]
            )
            .await
            .status,
            StatusCode::UNAUTHORIZED
        );
        sqlx::query("UPDATE content_tickets SET expires_at=0 WHERE id=?")
            .bind(ticket.body["id"].as_str().unwrap())
            .execute(&f.server.app.db)
            .await
            .unwrap();
        assert_eq!(
            f.call("GET", url, None, &[]).await.status,
            StatusCode::UNAUTHORIZED
        );
        assert_ne!(
            f.post("/api/v2/content-access", &auth, Some(&key), input.clone())
                .await
                .status,
            StatusCode::CREATED
        );
        let fresh = f
            .post(
                "/api/v2/content-access",
                &auth,
                Some(&format!("fresh-ticket-{mode}")),
                input,
            )
            .await;
        let fresh_url = fresh.body["url"].as_str().unwrap();
        assert_eq!(
            f.call(
                "DELETE",
                &format!(
                    "/api/v2/content-access/{}",
                    fresh.body["id"].as_str().unwrap()
                ),
                None,
                &[("authorization", &auth)]
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.call("GET", fresh_url, None, &[]).await.status,
            StatusCode::UNAUTHORIZED
        );
        // Closing a generation invalidates even an unexpired ticket.
        let live = f
            .post(
                "/api/v2/content-access",
                &auth,
                Some(&format!("close-ticket-{mode}")),
                json!({"purpose":"playback","delivery_id":id,"generation":"1","ttl_seconds":60}),
            )
            .await;
        f.call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
        assert_eq!(
            f.call("GET", live.body["url"].as_str().unwrap(), None, &[])
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Current grant removal is also enforced by already issued tickets.
    let admitted = admit_original(&f, &auth, &timeline, "scope-ticket-delivery", 0).await;
    let id = admitted["id"].as_str().unwrap();
    let input = json!({"purpose":"playback","delivery_id":id,"generation":"1","ttl_seconds":60});
    let ticket = f
        .post(
            "/api/v2/content-access",
            &auth,
            Some("scope-ticket-issue"),
            input.clone(),
        )
        .await;
    sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
        .bind(&device)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    assert_eq!(
        f.call("GET", ticket.body["url"].as_str().unwrap(), None, &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_ne!(
        f.post(
            "/api/v2/content-access",
            &auth,
            Some("scope-ticket-issue"),
            input
        )
        .await
        .status,
        StatusCode::CREATED
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_media_policy_requires_observed_facts_and_components_never_substitute() {
    let f = Fixture::new().await;
    two_audio_mp4(&f, "10");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let components = playscale::components::timeline_components(&f.server.app.db, &timeline)
        .await
        .unwrap();
    let second = components
        .iter()
        .find(|c| {
            c.kind == playscale_core::components::Kind::Audio
                && c.occurrences.iter().any(|o| o.track_index == Some(2))
        })
        .unwrap();
    let mut input = plan_input("default", &timeline, "auto", None, &["http_range", "hls"]);
    input["tracks"]["audio_component_id"] = json!(second.id);
    let selected = f
        .post("/api/v2/playback/plans", &auth, None, input.clone())
        .await;
    assert_eq!(
        selected.body["tracks"]["audio_track_id"], "a1",
        "{}",
        selected.body
    );
    input["tracks"]["audio_component_id"] = json!("missing-component");
    assert_eq!(
        f.post("/api/v2/playback/plans", &auth, None, input)
            .await
            .body["reason_codes"][0],
        "component_unavailable"
    );
    for limit in [239, 240, 241] {
        let mut input = plan_input("default", &timeline, "original", None, &["http_range"]);
        input["quality"]["max_height"] = json!(limit);
        assert_eq!(
            f.post("/api/v2/playback/plans", &auth, None, input)
                .await
                .body["status"],
            if limit < 240 { "blocked" } else { "ready" }
        );
    }
    // Even a very generous cap is not proven by this variable-bitrate file's
    // small average size. File routes have no trustworthy peak/VBV observation.
    let mut capped = plan_input("default", &timeline, "original", None, &["http_range"]);
    capped["quality"]["max_bitrate_bps"] = json!("999999999999");
    assert_eq!(
        f.post("/api/v2/playback/plans", &auth, None, capped)
            .await
            .body["status"],
        "blocked"
    );
    let file = source["file_id"].as_str().unwrap();
    let tracks: String = sqlx::query_scalar("SELECT tracks_json FROM media_files WHERE id=?")
        .bind(file)
        .fetch_one(&f.server.app.db)
        .await
        .unwrap();
    // PQ and HLG cannot be tone-mapped by this service. Byte preservation is
    // permitted only when the client reports HDR support and does not require SDR.
    for transfer in ["smpte2084", "arib-std-b67"] {
        let mut hdr: Value = serde_json::from_str(&tracks).unwrap();
        for track in hdr.as_array_mut().unwrap() {
            if track["kind"] == "video" {
                track["color_transfer"] = json!(transfer);
            }
        }
        sqlx::query("UPDATE media_files SET tracks_json=? WHERE id=?")
            .bind(hdr.to_string())
            .bind(file)
            .execute(&f.server.app.db)
            .await
            .unwrap();
        for (mode, policy, capability, expected) in [
            ("original", "preserve_if_supported", "supported", "ready"),
            ("original", "require_sdr", "supported", "blocked"),
            (
                "original",
                "preserve_if_supported",
                "unsupported",
                "blocked",
            ),
            ("original", "preserve_if_supported", "unknown", "blocked"),
            ("convert", "preserve_if_supported", "supported", "blocked"),
            ("convert", "reject_conversion", "supported", "blocked"),
        ] {
            let mut input = plan_input("default", &timeline, mode, None, &["http_range", "hls"]);
            input["quality"]["hdr_policy"] = json!(policy);
            input["client"]["hdr"] = json!(capability);
            let reply = f.post("/api/v2/playback/plans", &auth, None, input).await;
            assert_eq!(
                reply.body["status"], expected,
                "{transfer} {mode} {policy} {capability}: {}",
                reply.body
            );
        }
    }
    let mut unknown: Value = serde_json::from_str(&tracks).unwrap();
    for t in unknown.as_array_mut().unwrap() {
        if t["kind"] == "video" {
            t["height"] = Value::Null;
        }
    }
    sqlx::query("UPDATE media_files SET tracks_json=?,duration_seconds=NULL WHERE id=?")
        .bind(unknown.to_string())
        .bind(file)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    for (key, value) in [
        ("max_height", json!(1080)),
        ("max_bitrate_bps", json!("999999999")),
    ] {
        let mut input = plan_input("default", &timeline, "original", None, &["http_range"]);
        input["quality"][key] = value;
        assert_eq!(
            f.post("/api/v2/playback/plans", &auth, None, input)
                .await
                .body["status"],
            "blocked"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_plain_text_subtitles_are_revision_bound_and_other_formats_refused() {
    assert!(tools_available());
    let f = Fixture::new().await;
    two_audio_mp4(&f, "10");
    let original = f.dir.path().join("base.mp4");
    std::fs::rename(f.dir.path().join("media/film.mp4"), &original).unwrap();
    let srt = f.dir.path().join("captions.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:03,000\nSelected caption\n").unwrap();
    f.media(
        "film.mp4",
        &[
            "-i",
            original.to_str().unwrap(),
            "-i",
            srt.to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-c",
            "copy",
            "-c:s",
            "mov_text",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let mut input = plan_input("default", &timeline, "original", None, &["http_range"]);
    input["source"] = source.clone();
    input["tracks"]["subtitle_track_id"] = json!("s0");
    input["tracks"]["subtitle_policy"] = json!("require");
    let mut automatic = input.clone();
    automatic["tracks"]["subtitle_track_id"] = Value::Null;
    automatic["tracks"]["subtitle_policy"] = json!("auto");
    // Automatic selection never takes a caption of unknown language: the
    // plan proceeds without subtitles.
    let automatic = f
        .post("/api/v2/playback/plans", &auth, None, automatic)
        .await;
    assert_eq!(automatic.body["status"], "ready", "{}", automatic.body);
    assert_eq!(automatic.body["tracks"]["subtitle_track_id"], Value::Null);
    let plan = f
        .post("/api/v2/playback/plans", &auth, None, input.clone())
        .await;
    assert_eq!(plan.body["status"], "ready", "{}", plan.body);
    let delivery = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("text-subtitle-delivery"),
            json!({"plan_token":plan.body["plan_token"],"start_ms":0}),
        )
        .await;
    assert_eq!(delivery.status, StatusCode::CREATED, "{}", delivery.body);
    let url = delivery.body["active"]["subtitles_url"].as_str().unwrap();
    assert_eq!(
        f.call("GET", url, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    let captions = f.get(url, &auth).await;
    assert_eq!(captions.status, StatusCode::OK, "{}", captions.body);
    let text = String::from_utf8(captions.bytes).unwrap();
    assert!(
        text.starts_with("WEBVTT")
            && text.contains("Selected caption")
            && text.contains("00:01.000"),
        "{text}"
    );
    let id = delivery.body["id"].as_str().unwrap();
    let ticket = f
        .post(
            "/api/v2/content-access",
            &auth,
            Some("subtitle-content-access"),
            json!({"purpose":"playback","delivery_id":id,"generation":"1","ttl_seconds":60}),
        )
        .await;
    let token = ticket.body["url"]
        .as_str()
        .unwrap()
        .split("ticket=")
        .nth(1)
        .unwrap();
    assert_eq!(
        f.call("GET", &format!("{url}?ticket={token}"), None, &[])
            .await
            .status,
        StatusCode::OK
    );
    // Removing and restoring the selection creates separately pinned generations.
    for (generation, selected) in [(2, false), (3, true)] {
        let mut changed = input.clone();
        if !selected {
            changed["tracks"]["subtitle_track_id"] = Value::Null;
            changed["tracks"]["subtitle_policy"] = json!("off");
        }
        let plan = f.post("/api/v2/playback/plans", &auth, None, changed).await;
        assert_eq!(plan.body["status"], "ready", "{}", plan.body);
        let staged = f
            .post(
                &format!("/api/v2/playback/delivery-sessions/{id}/changes"),
                &auth,
                Some(&format!("subtitle-replan-{generation}")),
                json!({"kind":"replan", "expected_generation":(generation-1).to_string(),
                "plan_token":plan.body["plan_token"], "position_ms":1500}),
            )
            .await;
        assert_eq!(staged.status, StatusCode::ACCEPTED, "{}", staged.body);
        assert_eq!(
            staged.body["pending"]["subtitles_url"].is_string(),
            selected
        );
        let activated = f
            .post(
                &format!(
                    "/api/v2/playback/delivery-sessions/{id}/generations/{generation}/activate"
                ),
                &auth,
                Some(&format!("subtitle-activate-{generation}")),
                json!({"expected_active_generation":(generation-1).to_string()}),
            )
            .await;
        assert_eq!(activated.status, StatusCode::OK, "{}", activated.body);
        assert_eq!(
            activated.body["active"]["subtitles_url"].is_string(),
            selected
        );
        if selected {
            assert_eq!(
                f.get(
                    activated.body["active"]["subtitles_url"].as_str().unwrap(),
                    &auth
                )
                .await
                .status,
                StatusCode::OK
            );
        }
    }
    // The old generation ticket cannot be changed to authorize its successor.
    assert_eq!(
        f.call(
            "GET",
            &format!("/api/v2/streams/{id}/3/subtitles.vtt?ticket={token}"),
            None,
            &[]
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    // A live route carries a text subtitle only for a client that selects an
    // in-manifest subtitle rendition (an external track on a live
    // presentation is refused by the player).
    input["quality"]["mode"] = json!("convert");
    input["client"]["transports"] = json!(["hls"]);
    assert_eq!(
        f.post("/api/v2/playback/plans", &auth, None, input.clone())
            .await
            .body["reason_codes"][0],
        "subtitle_conversion_unavailable"
    );
    let mut live_text = input.clone();
    live_text["client"]["subtitle_modes"] = json!(["text", "hls_text"]);
    // The rendition's segment 0 of a conversion admitted at `start_ms`, with
    // the generation's media time origin.
    let rendition = |input: Value, start_ms: u64, key: &'static str| {
        let f = &f;
        let auth = auth.clone();
        async move {
            let plan = f.post("/api/v2/playback/plans", &auth, None, input).await;
            assert_eq!(plan.body["operation"], "video_transcode", "{}", plan.body);
            let admitted = f
                .post(
                    "/api/v2/playback/delivery-sessions",
                    &auth,
                    Some(key),
                    json!({"plan_token": plan.body["plan_token"], "start_ms": start_ms}),
                )
                .await;
            assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
            let id = admitted.body["id"].as_str().unwrap().to_owned();
            let ready = f
                .until(&id, &auth, |d| {
                    d["active"]["available_end_ms"].as_u64().unwrap_or(0) > start_ms
                })
                .await;
            let url = ready["active"]["subtitles_url"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                url,
                format!("/api/v2/streams/{id}/1/variants/subtitles/index.m3u8")
            );
            let auth = &auth;
            let get = |path: String| async move {
                let reply = f.call("GET", &path, None, &[("authorization", auth)]).await;
                assert_eq!(reply.status, StatusCode::OK, "{path}");
                String::from_utf8(reply.bytes).unwrap()
            };
            let master = get(format!("/api/v2/streams/{id}/1/master.m3u8")).await;
            assert!(
                master.contains("#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\"")
                    && master.contains("URI=\"variants/subtitles/index.m3u8\"")
                    && master.contains(",SUBTITLES=\"subs\"\n"),
                "{master}"
            );
            // The window may advance between two reads of a live generation:
            // compare the rendition with a media playlist unchanged around it.
            let (media, subtitles) = loop {
                let media = get(format!("/api/v2/streams/{id}/1/variants/main/index.m3u8")).await;
                let subtitles = get(url.clone()).await;
                if get(format!("/api/v2/streams/{id}/1/variants/main/index.m3u8")).await == media {
                    break (media, subtitles);
                }
            };
            // The rendition mirrors the media playlist's window and durations.
            let entries = |text: &str| -> Vec<String> {
                text.lines()
                    .filter(|l| l.starts_with("#EXTINF") || l.starts_with("#EXT-X-MEDIA-SEQUENCE"))
                    .map(str::to_owned)
                    .collect()
            };
            assert_eq!(entries(&subtitles), entries(&media), "{subtitles}");
            assert!(
                subtitles.contains("../../segments/subtitles-0.vtt"),
                "{subtitles}"
            );
            let segment = get(format!("/api/v2/streams/{id}/1/segments/subtitles-0.vtt")).await;
            let origin = ready["active"]["media_time_origin_ms"].as_u64().unwrap();
            // Opt-in: write the server's exact HLS output (same layout) for a
            // player check outside this test (scripts/check_hls_subtitles.mjs).
            if let Some(dir) = std::env::var_os("MOTION_DUMP_HLS") {
                let dir = std::path::Path::new(&dir).join(key);
                let base = format!("/api/v2/streams/{id}/1");
                let mut files = vec![
                    "master.m3u8".to_owned(),
                    "variants/main/index.m3u8".into(),
                    "variants/main/init.mp4".into(),
                    "variants/subtitles/index.m3u8".into(),
                ];
                for line in media.lines().chain(subtitles.lines()) {
                    if let Some(name) = line.strip_prefix("../../") {
                        files.push(name.to_owned());
                    }
                }
                for name in files {
                    let reply = f
                        .call(
                            "GET",
                            &format!("{base}/{name}"),
                            None,
                            &[("authorization", auth)],
                        )
                        .await;
                    assert_eq!(reply.status, StatusCode::OK, "{name}");
                    let path = dir.join(&name);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, reply.bytes).unwrap();
                }
            }
            (segment, origin)
        }
    };
    // From 2 s: the 1-3 s caption is at 0 to (3 s - origin) in media time.
    let (segment, origin) = rendition(live_text.clone(), 2000, "live-text-rendition-1").await;
    let expected = format!(
        "00:00:00.000 --> 00:00:0{}.{:03}\nSelected caption",
        (3000 - origin) / 1000,
        (3000 - origin) % 1000
    );
    assert!(
        segment.starts_with("WEBVTT") && segment.contains(&expected),
        "{expected}\n{segment}"
    );
    // An interval binding starting at 0.5 s: media time is file time minus
    // the interval start, so the caption is at 0.5 to 2.5 s.
    sqlx::query("UPDATE version_files SET start_ms=500,end_ms=9000")
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let mut interval = live_text.clone();
    interval["quality"]["mode"] = json!("auto");
    let (segment, origin) = rendition(interval, 0, "live-text-rendition-2").await;
    assert_eq!(origin, 0);
    assert!(
        segment.contains("00:00:00.500 --> 00:00:02.500\nSelected caption"),
        "{segment}"
    );
    sqlx::query("UPDATE version_files SET start_ms=NULL,end_ms=NULL")
        .execute(&f.server.app.db)
        .await
        .unwrap();
    input["quality"]["mode"] = json!("original");
    input["client"]["transports"] = json!(["http_range"]);
    input["tracks"]["subtitle_policy"] = json!("off");
    problem(
        &f.post("/api/v2/playback/plans", &auth, None, input.clone())
            .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "subtitle_policy_conflict",
    );
    input["tracks"]["subtitle_policy"] = json!("require");
    for codec in ["ass", "hdmv_pgs_subtitle", "dvd_subtitle"] {
        let tracks: String = sqlx::query_scalar("SELECT tracks_json FROM media_files WHERE id=?")
            .bind(source["file_id"].as_str().unwrap())
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
        let mut tracks: Value = serde_json::from_str(&tracks).unwrap();
        for track in tracks.as_array_mut().unwrap() {
            if track["kind"] == "subtitle" {
                track["codec"] = json!(codec);
            }
        }
        sqlx::query("UPDATE media_files SET tracks_json=? WHERE id=?")
            .bind(tracks.to_string())
            .bind(source["file_id"].as_str().unwrap())
            .execute(&f.server.app.db)
            .await
            .unwrap();
        assert_eq!(
            f.post("/api/v2/playback/plans", &auth, None, input.clone())
                .await
                .body["reason_codes"][0],
            "subtitle_format_unsupported"
        );
    }
}

/// A source eligible for stream copy: Main-profile H.264 with B-frames and a
/// 2 s GOP, an AAC track (44.1 kHz) and an AC-3 track (48 kHz), every stream
/// starting at zero. `level` overrides the H.264 level_idc.
fn copyable_mkv(f: &Fixture, name: &str, level: &str) {
    f.media(
        name,
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
            "sine=frequency=880:sample_rate=48000",
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
            "medium",
            "-profile:v",
            "main",
            "-level:v",
            level,
            "-g",
            "48",
            "-keyint_min",
            "48",
            "-sc_threshold",
            "0",
            "-c:a:0",
            "aac",
            "-c:a:1",
            "ac3",
        ],
    );
}

/// Streams of an init segment followed by one media segment.
fn fragment_streams(f: &Fixture, name: &str, init: Vec<u8>, segment: Vec<u8>) -> Vec<Value> {
    let fragment = f.dir.path().join(name);
    std::fs::write(&fragment, [init, segment].concat()).unwrap();
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&fragment)
        .output()
        .unwrap();
    let streams: Value = serde_json::from_slice(&probe.stdout).unwrap();
    streams["streams"].as_array().unwrap().clone()
}

/// Copy routes the server admits are planned over the production router: the
/// core asks for bitstream details before choosing one, a copy keeps the
/// source video and timestamps from the preceding keyframe, a planned replan
/// can move between copy routes, failures fall back to conversion, Convert
/// never copies, and unqualified details or a disabled server only convert.
#[tokio::test(flavor = "multi_thread")]
async fn stream_copy_routes_are_planned_observed_and_fall_back_to_conversion() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_copy_routes(true).await;
    copyable_mkv(&f, "film.mkv", "3.0");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (device, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let file = source["file_id"].as_str().unwrap().to_owned();
    let caps = f.get("/api/v2/system/capabilities", &auth).await;
    let playback = caps.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "playback.v2")
        .unwrap()
        .clone();
    assert_eq!(
        playback["limits"]["routes"], "original,prepared,stream_copy_hls,software_hls",
        "{playback}"
    );
    let plan = |mode: &str, audio: &str, failed: &[String]| {
        let mut input = pinned_plan_input(
            "default",
            &timeline,
            mode,
            audio,
            &["http_range", "hls"],
            &source,
        );
        input["failed_candidate_ids"] = json!(failed);
        input
    };
    let original = format!("o-{file}");

    // The original byte route still wins when it can honor the request.
    let direct = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("auto", "a0", &[]),
        )
        .await;
    assert_eq!(direct.body["operation"], "original", "{}", direct.body);
    // After the client reports the original failed, the AAC track is copied.
    let remux = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("auto", "a0", std::slice::from_ref(&original)),
        )
        .await;
    assert_eq!(remux.status, StatusCode::OK, "{}", remux.body);
    assert_eq!(remux.body["operation"], "remux", "{}", remux.body);
    assert_eq!(remux.body["transport"], "hls");
    assert_eq!(remux.body["candidate_id"], format!("r-{file}"));
    // Convert asks for converted video: never a copy.
    let convert = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("convert", "a0", &[]),
        )
        .await;
    assert_eq!(
        convert.body["operation"], "video_transcode",
        "{}",
        convert.body
    );

    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-remux-000001"),
            json!({"plan_token": remux.body["plan_token"], "start_ms": 31000}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/playback/delivery-sessions/{id}");
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 31000
        })
        .await;
    assert_eq!(ready["active"]["operation"], "remux", "{ready}");
    // Segment 0 starts at the source keyframe before the request (2 s GOP).
    assert_eq!(ready["active"]["available_start_ms"], 30000, "{ready}");
    let base = format!("/api/v2/streams/{id}/1");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    // The master playlist describes the copied output itself: Main profile
    // (0x4d), level 3.0 (0x1e) and AAC-LC, not the transcode recipe.
    let master = String::from_utf8(get(format!("{base}/master.m3u8")).await).unwrap();
    let codecs = master
        .split("CODECS=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap();
    assert!(
        codecs.starts_with("avc1.4d") && codecs.ends_with("1e,mp4a.40.2"),
        "{master}"
    );
    let variant = String::from_utf8(get(format!("{base}/variants/main/index.m3u8")).await).unwrap();
    assert!(variant.contains("#EXT-X-TARGETDURATION:12\n"), "{variant}");
    let streams = fragment_streams(
        &f,
        "remux.mp4",
        get(format!("{base}/variants/main/init.mp4")).await,
        get(format!("{base}/segments/0.m4s")).await,
    );
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!(video["profile"], "Main", "video was re-encoded");
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    assert_eq!(audio.len(), 1);
    assert_eq!(audio[0]["codec_name"], "aac");
    assert_eq!(audio[0]["sample_rate"], "44100", "the selected first track");

    // Switching to the AC-3 track replans to an audio conversion that still
    // copies the video.
    let converted = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("auto", "a1", &[]),
        )
        .await;
    assert_eq!(
        converted.body["operation"], "audio_convert",
        "{}",
        converted.body
    );
    assert_eq!(converted.body["candidate_id"], format!("c-{file}"));
    let staged = f
        .post(
            &format!("{path}/changes"),
            &auth,
            Some("replan-audio-convert-1"),
            json!({"kind":"replan","expected_generation":"1","plan_token":converted.body["plan_token"],"position_ms":12000}),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{}", staged.body);
    assert_eq!(staged.body["pending"]["operation"], "audio_convert");
    let ready = f
        .until(&id, &auth, |d| {
            d["pending"]["status"] == "ready" || d["pending"]["status"] == "failed"
        })
        .await;
    assert_eq!(ready["pending"]["status"], "ready", "{ready}");
    assert_eq!(ready["pending"]["available_start_ms"], 12000, "{ready}");
    let base = format!("/api/v2/streams/{id}/2");
    let streams = fragment_streams(
        &f,
        "audio-convert.mp4",
        get(format!("{base}/variants/main/init.mp4")).await,
        get(format!("{base}/segments/0.m4s")).await,
    );
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!(video["profile"], "Main", "video was re-encoded");
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    assert_eq!(audio.len(), 1);
    assert_eq!(audio[0]["codec_name"], "aac");
    assert_eq!(
        audio[0]["sample_rate"], "48000",
        "the converted second track"
    );
    let closed = f
        .call("DELETE", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);

    // A failed copy falls back to conversion; a failed conversion ends it.
    let copy_failed = vec![original.clone(), format!("c-{file}")];
    let fallback = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("auto", "a1", &copy_failed),
        )
        .await;
    assert_eq!(
        fallback.body["operation"], "video_transcode",
        "{}",
        fallback.body
    );
    let mut all_failed = copy_failed.clone();
    all_failed.push(format!("t-{file}"));
    let exhausted = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("auto", "a1", &all_failed),
        )
        .await;
    assert_eq!(exhausted.body["reason_codes"][0], "all_candidates_failed");

    // A retained copy plan never reaches the source once its holder's scope
    // no longer includes it: refused before any probe or admission.
    let retained = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan("auto", "a1", &[]),
        )
        .await;
    assert_eq!(retained.body["operation"], "audio_convert");
    f.policy(&device, &[], PLAYER, "\"r-2\"").await;
    let refused = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-revoked-copy-1"),
            json!({"plan_token": retained.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.body);

    // Copy-route capacity is released with the delivery.
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.server.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("copy route capacity was not released");

    // Observed details that do not qualify (level 5.2) leave only conversion.
    let g = Fixture::with_copy_routes(true).await;
    copyable_mkv(&g, "film.mkv", "5.2");
    g.scan().await;
    let (timeline, _) = g.timeline().await;
    let (_, auth) = g.device(&["default"], PLAYER).await;
    let source = g.source().await;
    let unqualified = g
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned_plan_input(
                "default",
                &timeline,
                "auto",
                "a1",
                &["http_range", "hls"],
                &source,
            ),
        )
        .await;
    assert_eq!(
        unqualified.body["operation"], "video_transcode",
        "{}",
        unqualified.body
    );

    // A server without copy routes neither advertises nor plans them.
    let h = Fixture::new().await;
    copyable_mkv(&h, "film.mkv", "3.0");
    h.scan().await;
    let (timeline, _) = h.timeline().await;
    let (_, auth) = h.device(&["default"], PLAYER).await;
    let source = h.source().await;
    let disabled = h
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned_plan_input(
                "default",
                &timeline,
                "auto",
                "a1",
                &["http_range", "hls"],
                &source,
            ),
        )
        .await;
    assert_eq!(
        disabled.body["operation"], "video_transcode",
        "{}",
        disabled.body
    );
    let caps = h.get("/api/v2/system/capabilities", &auth).await;
    let playback = caps.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "playback.v2")
        .unwrap()
        .clone();
    assert_eq!(
        playback["limits"]["routes"],
        "original,prepared,software_hls"
    );
}

/// The routes a server advertises in its playback capability.
async fn advertised_routes(f: &Fixture, auth: &str) -> Value {
    let caps = f.get("/api/v2/system/capabilities", auth).await;
    caps.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "playback.v2")
        .unwrap()["limits"]["routes"]
        .clone()
}

/// Hardware conversion is planned as its own candidate when VideoToolbox is
/// enabled, available and admissible; it runs Apple's encoder, a planned
/// replan can move the delivery to software, a failed hardware candidate falls
/// back to software, and both sessions are released on close.
#[tokio::test(flavor = "multi_thread")]
async fn hardware_conversion_is_planned_when_available_and_falls_back_to_software() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let mut f = Fixture::with_settings(playscale::processing::Settings {
        experimental_live_hardware: true,
        ..Default::default()
    })
    .await;
    if !playscale::processing::videotoolbox_available(&f.server.app).await {
        assert!(
            std::env::var_os("MOTION_REQUIRE_VIDEOTOOLBOX").is_none(),
            "MOTION_REQUIRE_VIDEOTOOLBOX is set but the VideoToolbox probe failed"
        );
        eprintln!("SKIPPED: no VideoToolbox encoder session on this host");
        return;
    }
    two_audio_mp4(&f, "120");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let file = f.source().await["file_id"].as_str().unwrap().to_owned();
    assert_eq!(
        advertised_routes(&f, &auth).await,
        "original,prepared,software_hls,hardware_hls"
    );
    let plan = |failed: &[String]| {
        let mut input = plan_input("default", &timeline, "convert", None, &["hls"]);
        input["failed_candidate_ids"] = json!(failed);
        input
    };
    let hardware = f
        .post("/api/v2/playback/plans", &auth, None, plan(&[]))
        .await;
    assert_eq!(hardware.status, StatusCode::OK, "{}", hardware.body);
    assert_eq!(hardware.body["operation"], "video_transcode");
    assert_eq!(hardware.body["candidate_id"], format!("h-{file}"));
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-hardware-0001"),
            json!({"plan_token": hardware.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/playback/delivery-sessions/{id}");
    f.until(&id, &auth, |d| {
        d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 0
    })
    .await;
    let segment = |generation: &str| {
        let auth = auth.clone();
        let f = &f;
        let path = format!("/api/v2/streams/{id}/{generation}/segments/0.m4s");
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    // libx264 writes its settings into an SEI message; VideoToolbox does not.
    let x264 = |bytes: &[u8]| bytes.windows(4).any(|w| w == b"x264");
    assert!(
        !x264(&segment("1").await),
        "hardware output came from libx264"
    );
    assert_eq!(f.server.app.processing.hardware.snapshot().used, 1);

    // The client moves the delivery to software with a new plan.
    let software = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan(&[format!("h-{file}")]),
        )
        .await;
    assert_eq!(
        software.body["candidate_id"],
        format!("t-{file}"),
        "{}",
        software.body
    );
    let staged = f
        .post(
            &format!("{path}/changes"),
            &auth,
            Some("replan-to-software-1"),
            json!({"kind":"replan","expected_generation":"1","plan_token":software.body["plan_token"],"position_ms":30000}),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{}", staged.body);
    let ready = f
        .until(&id, &auth, |d| {
            d["pending"]["status"] == "ready" || d["pending"]["status"] == "failed"
        })
        .await;
    assert_eq!(ready["pending"]["status"], "ready", "{ready}");
    assert!(
        x264(&segment("2").await),
        "software output must come from libx264"
    );
    let exhausted = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan(&[format!("h-{file}"), format!("t-{file}")]),
        )
        .await;
    assert_eq!(exhausted.body["reason_codes"][0], "all_candidates_failed");
    let closed = f
        .call("DELETE", &path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.server.app.processing.hardware.snapshot().used != 0
            || f.server.app.processing.execution.snapshot().used != 0
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("hardware and CPU capacity were not released");

    // A hardware plan signed before a restart that disabled live hardware
    // cannot start an encoder.
    let retained = f
        .post("/api/v2/playback/plans", &auth, None, plan(&[]))
        .await;
    assert_eq!(retained.body["candidate_id"], format!("h-{file}"));
    f.restart_with(Default::default()).await;
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("admit-disabled-hw-1"),
            json!({"plan_token": retained.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "playback_unavailable",
    );
    assert_eq!(
        advertised_routes(&f, &auth).await,
        "original,prepared,software_hls"
    );
}

/// Sidecar subtitles are selected by component on exactly the planned file,
/// pinned with their scanned fingerprint, converted to WebVTT from the
/// validated sidecar for byte delivery, refused when styled or live, and never
/// served after the sidecar changes until it is scanned again.
#[tokio::test(flavor = "multi_thread")]
async fn sidecar_subtitles_are_component_pinned_and_fingerprinted() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "10");
    let media = f.dir.path().join("media");
    std::fs::write(
        media.join("film.en.srt"),
        "1\n00:00:01,000 --> 00:00:03,000\nSidecar caption\n",
    )
    .unwrap();
    std::fs::write(
        media.join("film.fr.ass"),
        "[Script Info]\nScriptType: v4.00+\n\n[Events]\nFormat: Layer, Start, End, Style, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,Styled\n",
    )
    .unwrap();
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let components = playscale::components::timeline_components(&f.server.app.db, &timeline)
        .await
        .unwrap();
    let sidecar = |name: &str| {
        components
            .iter()
            .find(|c| {
                c.occurrences
                    .iter()
                    .any(|o| o.sidecar.as_deref() == Some(name))
            })
            .unwrap()
            .id
            .clone()
    };
    let pinned = |component: &str, mode: &str, transports: &[&str]| {
        let mut input = plan_input("default", &timeline, mode, None, transports);
        input["tracks"]["subtitle_component_id"] = json!(component);
        input["tracks"]["subtitle_policy"] = json!("require");
        input
    };
    let english = sidecar("film.en.srt");
    // Styled sidecars are refused rather than flattened.
    let styled = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned(&sidecar("film.fr.ass"), "auto", &["http_range"]),
        )
        .await;
    assert_eq!(
        styled.body["reason_codes"][0],
        "subtitle_format_unsupported"
    );
    // "off" conflicts with an explicit sidecar.
    let mut off = pinned(&english, "auto", &["http_range"]);
    off["tracks"]["subtitle_policy"] = json!("off");
    problem(
        &f.post("/api/v2/playback/plans", &auth, None, off).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "subtitle_policy_conflict",
    );

    // Byte delivery of the original with the sidecar as WebVTT.
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned(&english, "auto", &["http_range"]),
        )
        .await;
    assert_eq!(plan.body["operation"], "original", "{}", plan.body);
    assert_eq!(plan.body["tracks"]["subtitle_track_id"], Value::Null);
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("sidecar-original-01"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    // The same key with another subtitle choice is a different request.
    let plain = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["http_range"]),
        )
        .await;
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("sidecar-original-01"),
            json!({"plan_token": plain.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    );
    let url = admitted.body["active"]["subtitles_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let captions = f.get(&url, &auth).await;
    assert_eq!(captions.status, StatusCode::OK, "{}", captions.body);
    let text = String::from_utf8(captions.bytes).unwrap();
    assert!(
        text.starts_with("WEBVTT")
            && text.contains("00:01.000")
            && text.contains("Sidecar caption"),
        "{text}"
    );

    // A live conversion cannot carry a text sidecar.
    let live = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned(&english, "convert", &["hls"]),
        )
        .await;
    assert_eq!(
        live.body["reason_codes"][0], "subtitle_conversion_unavailable",
        "{}",
        live.body
    );

    // The sidecar changes on disk: a new delivery pinned to the scanned
    // fingerprint is never served the new text.
    std::fs::write(
        media.join("film.en.srt"),
        "1\n00:00:01,000 --> 00:00:03,000\nEdited caption\n",
    )
    .unwrap();
    let stale = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned(&english, "auto", &["http_range"]),
        )
        .await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("sidecar-stale-00001"),
            json!({"plan_token": stale.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    problem(
        &f.get(
            admitted.body["active"]["subtitles_url"].as_str().unwrap(),
            &auth,
        )
        .await,
        StatusCode::CONFLICT,
        "source_revision_changed",
    );
    // After a rescan the new fingerprint is planned and served.
    f.scan().await;
    let fresh = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            pinned(&english, "auto", &["http_range"]),
        )
        .await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("sidecar-fresh-00001"),
            json!({"plan_token": fresh.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let text = String::from_utf8(
        f.get(
            admitted.body["active"]["subtitles_url"].as_str().unwrap(),
            &auth,
        )
        .await
        .bytes,
    )
    .unwrap();
    assert!(text.contains("Edited caption"), "{text}");
    // A plan signed against the old fingerprint is refused at admission.
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("sidecar-stale-00002"),
            json!({"plan_token": stale.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::CONFLICT,
        "source_revision_changed",
    );
}

/// A minimal PGS (`.sup`) stream: one opaque white `w`x`h` box at (`x`, `y`)
/// on a `width`x`height` canvas, shown from `start_ms` until `end_ms`.
fn pgs_box(
    width: u16,
    height: u16,
    (x, y, w, h): (u16, u16, u16, u16),
    start_ms: u32,
    end_ms: u32,
) -> Vec<u8> {
    let segment = |pts_ms: u32, kind: u8, payload: &[u8]| {
        let mut out = b"PG".to_vec();
        out.extend((pts_ms * 90).to_be_bytes());
        out.extend(0u32.to_be_bytes());
        out.push(kind);
        out.extend((payload.len() as u16).to_be_bytes());
        out.extend(payload);
        out
    };
    let be = |v: u16| v.to_be_bytes();
    let window = [&[1u8, 0][..], &be(x), &be(y), &be(w), &be(h)].concat();
    let mut rle = Vec::new();
    for _ in 0..h {
        rle.extend([
            0x00,
            0xC0 | (w >> 8) as u8,
            (w & 0xff) as u8,
            0x01,
            0x00,
            0x00,
        ]);
    }
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
    ods.extend(be(w));
    ods.extend(be(h));
    ods.extend(&rle);
    let pcs = |number: u16, state: u8, objects: bool| {
        let mut p = [
            &be(width)[..],
            &be(height),
            &[0x10],
            &be(number),
            &[state, 0, 0],
        ]
        .concat();
        if objects {
            p.extend([1, 0, 0, 0, 0]);
            p.extend(be(x));
            p.extend(be(y));
        } else {
            p.push(0);
        }
        p
    };
    [
        segment(start_ms, 0x16, &pcs(0, 0x80, true)),
        segment(start_ms, 0x17, &window),
        segment(start_ms, 0x14, &[0, 0, 1, 235, 128, 128, 255]),
        segment(start_ms, 0x15, &ods),
        segment(start_ms, 0x80, &[]),
        segment(end_ms, 0x16, &pcs(1, 0x00, false)),
        segment(end_ms, 0x17, &window),
        segment(end_ms, 0x80, &[]),
    ]
    .concat()
}

/// A bitmap subtitle is burned into a conversion for a client that accepts
/// burned subtitles: never offered on byte delivery, a copy or to a client
/// that does not accept it, and the decoded output carries the subpicture.
#[tokio::test(flavor = "multi_thread")]
async fn bitmap_subtitles_are_burned_into_conversions_only() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_copy_routes(true).await;
    two_audio_mp4(&f, "20");
    let base = f.dir.path().join("base.mp4");
    std::fs::rename(f.dir.path().join("media/film.mp4"), &base).unwrap();
    let sup = f.dir.path().join("box.sup");
    // A 640x480 canvas on 320x240 video: the box maps to (40, 40, 240, 60).
    std::fs::write(&sup, pgs_box(640, 480, (80, 80, 480, 120), 0, 10000)).unwrap();
    f.media(
        "film.mkv",
        &[
            "-i",
            base.to_str().unwrap(),
            "-i",
            sup.to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-c",
            "copy",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let input = |mode: &str, modes: &[&str]| {
        let mut input = plan_input("default", &timeline, mode, None, &["http_range", "hls"]);
        input["source"] = source.clone();
        input["tracks"]["subtitle_track_id"] = json!("s0");
        input["tracks"]["subtitle_policy"] = json!("require");
        input["client"]["subtitle_modes"] = json!(modes);
        input
    };
    let plan = |body| f.post("/api/v2/playback/plans", &auth, None, body);
    assert_eq!(
        plan(input("auto", &["text"])).await.body["reason_codes"][0],
        "subtitle_format_unsupported"
    );
    assert_eq!(
        plan(input("original", &["text", "burned"])).await.body["reason_codes"][0],
        "subtitle_requires_conversion"
    );
    // Auto skips the original and the (eligible) copy for a conversion.
    let burned = plan(input("auto", &["text", "burned"])).await;
    assert_eq!(
        burned.body["operation"], "video_transcode",
        "{}",
        burned.body
    );
    assert_eq!(burned.body["tracks"]["subtitle_track_id"], "s0");
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("burned-subtitle-001"),
            json!({"plan_token": burned.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 0
        })
        .await;
    // Burned, not a sidecar.
    assert_eq!(ready["active"]["subtitles_url"], Value::Null, "{ready}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    // Mean luma inside the box (inset from its edges) and below it, `at`
    // seconds into a generation's first segment.
    let means = |generation_id: String, at: &'static str| {
        let get = &get;
        let f = &f;
        async move {
            let base_url = format!("/api/v2/streams/{generation_id}");
            let fragment = f
                .dir
                .path()
                .join(format!("burned-{}.mp4", generation_id.replace('/', "-")));
            std::fs::write(
                &fragment,
                [
                    get(format!("{base_url}/variants/main/init.mp4")).await,
                    get(format!("{base_url}/segments/0.m4s")).await,
                ]
                .concat(),
            )
            .unwrap();
            let frame = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&fragment)
                .args([
                    "-ss",
                    at,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gray",
                    "-",
                ])
                .output()
                .unwrap()
                .stdout;
            assert_eq!(frame.len(), 320 * 240);
            let mean = |x0: usize, y0: usize| {
                let mut sum = 0usize;
                for y in y0..y0 + 40 {
                    sum += frame[y * 320 + x0..y * 320 + x0 + 220]
                        .iter()
                        .map(|&p| p as usize)
                        .sum::<usize>();
                }
                sum / (220 * 40)
            };
            (mean(50, 50), mean(50, 150))
        }
    };
    // From the start (FFmpeg presents a subpicture from just after the first
    // frame on): the box, scaled from its larger canvas, is white.
    let (inside, below) = means(format!("{id}/1"), "1").await;
    assert!(inside > 225 && below < 200, "box {inside}, picture {below}");
    let closed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    // Starting at 8 s, inside the 0-10 s cue: the cue is still drawn although
    // its display set precedes the start (2 s GOP: input seeking skips it).
    let later = plan(input("auto", &["text", "burned"])).await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("burned-subtitle-002"),
            json!({"plan_token": later.body["plan_token"], "start_ms": 8000}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 8000
        })
        .await;
    assert_eq!(ready["active"]["media_time_origin_ms"], 8000, "{ready}");
    let (inside, below) = means(format!("{id}/1"), "0.5").await;
    assert!(inside > 225 && below < 200, "box {inside}, picture {below}");
    let closed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
}

/// Without an explicit pin, subtitles follow the profile: foreign audio gets
/// full subtitles in a preferred language (plain over forced), forced mode
/// takes only a forced track in the audio's language, and "require" refuses
/// rather than substitute another language or the first track.
#[tokio::test(flavor = "multi_thread")]
async fn automatic_subtitles_follow_profile_languages_and_roles() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "10");
    let base = f.dir.path().join("base.mp4");
    std::fs::rename(f.dir.path().join("media/film.mp4"), &base).unwrap();
    let srt = f.dir.path().join("captions.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:03,000\nCaption\n").unwrap();
    // French audio; s0 forced English, s1 full English.
    f.media(
        "film.mp4",
        &[
            "-i",
            base.to_str().unwrap(),
            "-i",
            srt.to_str().unwrap(),
            "-i",
            srt.to_str().unwrap(),
            "-map",
            "0:v",
            "-map",
            "0:a:0",
            "-map",
            "1",
            "-map",
            "2",
            "-c",
            "copy",
            "-c:s",
            "mov_text",
            "-metadata:s:a:0",
            "language=fre",
            "-metadata:s:s:0",
            "language=eng",
            "-disposition:s:0",
            "forced",
            "-metadata:s:s:1",
            "language=eng",
            "-disposition:s:1",
            "0",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let prefer = |mode: &str, audio: &[&str], subtitles: &[&str]| {
        let (mode, audio, subtitles) = (mode.to_owned(), json!(audio), json!(subtitles));
        let f = &f;
        let auth = auth.clone();
        async move {
            let prefs = f.get("/api/v2/profiles/default/preferences", &auth).await;
            let mut document = prefs.body.clone();
            document["subtitle_mode"] = json!(mode);
            document["audio_languages"] = audio;
            document["subtitle_languages"] = subtitles;
            let replaced = f
                .call(
                    "PUT",
                    "/api/v2/profiles/default/preferences",
                    Some(document),
                    &[
                        ("authorization", &auth),
                        ("if-match", prefs.headers["etag"].to_str().unwrap()),
                    ],
                )
                .await;
            assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
        }
    };
    let plan = |policy: &str| {
        let mut input = plan_input("default", &timeline, "auto", None, &["http_range"]);
        input["tracks"]["subtitle_policy"] = json!(policy);
        f.post("/api/v2/playback/plans", &auth, None, input)
    };
    // French audio is foreign to an English profile: full English subtitles.
    prefer("foreign_audio", &["en"], &["en"]).await;
    let chosen = plan("auto").await;
    assert_eq!(
        chosen.body["tracks"]["subtitle_track_id"], "s1",
        "{}",
        chosen.body
    );
    assert_eq!(chosen.body["operation"], "original");
    // A client that can only take a conversion cannot be given text
    // subtitles: auto plays without them, require refuses.
    let mut live = plan_input("default", &timeline, "auto", None, &["hls"]);
    live["tracks"]["subtitle_policy"] = json!("auto");
    let converted = f
        .post("/api/v2/playback/plans", &auth, None, live.clone())
        .await;
    assert_eq!(
        converted.body["operation"], "video_transcode",
        "{}",
        converted.body
    );
    assert_eq!(converted.body["tracks"]["subtitle_track_id"], Value::Null);
    live["tracks"]["subtitle_policy"] = json!("require");
    assert_eq!(
        f.post("/api/v2/playback/plans", &auth, None, live)
            .await
            .body["reason_codes"][0],
        "subtitle_conversion_unavailable"
    );
    // Forced mode: only a forced track in the audio's language (none here).
    prefer("forced", &["en"], &["en"]).await;
    let none = plan("auto").await;
    assert_eq!(none.body["status"], "ready", "{}", none.body);
    assert_eq!(none.body["tracks"]["subtitle_track_id"], Value::Null);
    // A French-speaking profile hears French audio: forced subtitles only,
    // and there is no forced French track.
    prefer("foreign_audio", &["fr"], &["fr"]).await;
    assert_eq!(
        plan("auto").await.body["tracks"]["subtitle_track_id"],
        Value::Null
    );
    // Require: preferred languages only.
    assert_eq!(
        plan("require").await.body["reason_codes"][0],
        "subtitle_unavailable"
    );
    prefer("off", &[], &["de", "en"]).await;
    assert_eq!(
        plan("require").await.body["tracks"]["subtitle_track_id"],
        "s1"
    );
    assert_eq!(
        plan("auto").await.body["tracks"]["subtitle_track_id"],
        Value::Null
    );
}

/// A prepared rendition presents the source audio it was prepared from. It
/// serves a plan whose selected audio is exactly that stream of the same
/// source revision, by byte delivery, and never one that selects another.
#[tokio::test(flavor = "multi_thread")]
async fn prepared_renditions_serve_exactly_the_audio_they_were_prepared_from() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "6");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    // Prepare a remux of the second audio stream (22.05 kHz).
    let operator = bearer(OPERATOR);
    let prepared = f
        .post(
            "/api/v2/processing/preparations",
            &operator,
            Some("prepare-second-audio-0001"),
            json!({"source": source, "timeline_id": timeline, "recipe_id": "remux_mp4",
                "tracks": {"audio_component_id": null, "subtitle_component_id": null,
                    "subtitle_policy": "off", "audio_track_id": "a1", "subtitle_track_id": null}}),
        )
        .await;
    assert_eq!(prepared.status, StatusCode::ACCEPTED, "{}", prepared.body);
    let job = prepared.body["id"].as_str().unwrap().to_owned();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::processing::worker(
        f.server.app.clone(),
        stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT phase FROM processing_jobs WHERE id=?")
                .bind(&job)
                .fetch_one(&f.server.app.db)
                .await
                .unwrap();
            if phase == "completed" {
                break;
            }
            assert!(
                !["failed", "cancelled"].contains(&phase.as_str()),
                "{phase}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("preparation did not complete");
    stop.cancel();
    let _ = worker.await;
    let rendition: String =
        sqlx::query_scalar("SELECT file_id FROM renditions WHERE external_id=?")
            .bind(&job)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    let plan = |mode: &str, audio: &str| {
        let input = pinned_plan_input(
            "default",
            &timeline,
            mode,
            audio,
            &["http_range", "hls"],
            &source,
        );
        f.post("/api/v2/playback/plans", &auth, None, input)
    };
    // The second audio stream, explicitly chosen: the rendition carries it.
    let second = plan("auto", "a1").await;
    assert_eq!(second.body["operation"], "prepared", "{}", second.body);
    assert_eq!(second.body["source"]["file_id"], rendition.as_str());
    let second = plan("convert", "a1").await;
    assert_eq!(second.body["operation"], "prepared", "{}", second.body);
    // The default first stream: the rendition does not carry it.
    let first = plan("convert", "a0").await;
    assert_eq!(first.body["operation"], "video_transcode", "{}", first.body);
    assert_eq!(first.body["source"]["file_id"], source["file_id"]);
    assert_eq!(plan("auto", "a0").await.body["operation"], "original");
    // Pinning the rendition's version plays its own default streams.
    let version: String =
        sqlx::query_scalar("SELECT version_id FROM version_files WHERE file_id=?")
            .bind(&rendition)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    let mut pinned = plan_input("default", &timeline, "auto", None, &["http_range", "hls"]);
    pinned["version_id"] = json!(version);
    let pinned = f.post("/api/v2/playback/plans", &auth, None, pinned).await;
    assert_eq!(pinned.body["operation"], "prepared", "{}", pinned.body);
    // The rendition really holds the second stream (22.05 kHz).
    let path: (String, String) = sqlx::query_as(
        "SELECT l.root,f.relative_path FROM media_files f JOIN libraries l ON l.id=f.library_id WHERE f.id=?",
    )
    .bind(&rendition)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=sample_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(Path::new(&path.0).join(&path.1))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(probe.stdout).unwrap().trim(), "22050");

    // A live v2 byte delivery pins this generated artifact without creating a
    // legacy playback_sessions row. Retention must honor that real ownership.
    sqlx::query("UPDATE processing_jobs SET updated_at=0 WHERE id=?")
        .bind(&job)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let delivery = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("prepared-gc-pin-0001"),
            json!({"plan_token":pinned.body["plan_token"],"start_ms":0}),
        )
        .await;
    assert_eq!(delivery.status, StatusCode::CREATED, "{}", delivery.body);
    assert_eq!(
        playscale::maintenance::clean(&f.server.app).await.unwrap(),
        0
    );
    assert!(Path::new(&path.0).join(&path.1).exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT available FROM media_files WHERE id=?")
            .bind(&rendition)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap(),
        1
    );
    let id = delivery.body["id"].as_str().unwrap();
    let generation = delivery.body["active"]["generation"].as_str().unwrap();
    // Retain an admitted response without polling its body. It must own the
    // generated file independently of the delivery's runtime lifetime.
    let expected = std::fs::read(Path::new(&path.0).join(&path.1)).unwrap();
    let held = api::router(f.server.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(format!("/api/v2/streams/{id}/{generation}/content"))
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(held.status(), StatusCode::OK);
    let removed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.server.app.processing.deliveries.is_live(id) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("closed prepared delivery did not retire");
    assert_eq!(
        playscale::maintenance::clean(&f.server.app).await.unwrap(),
        0,
        "an admitted response must retain its artifact after delivery retirement"
    );
    assert!(Path::new(&path.0).join(&path.1).exists());
    let bytes = held.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.as_ref(), expected.as_slice());
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let removed = playscale::maintenance::clean(&f.server.app).await.unwrap();
            if removed == 1 {
                break;
            }
            assert_eq!(removed, 0);
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("generated artifact stayed pinned after its last reader completed");
    assert!(!Path::new(&path.0).join(&path.1).exists());
    let cleaned: (i64, i64, i64) = sqlx::query_as(
        "SELECT f.available,p.expired,p.cache_cleaned FROM processing_jobs p JOIN media_files f ON f.id=p.output_file_id WHERE p.id=?",
    )
    .bind(&job)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    assert_eq!(cleaned, (0, 1, 1));
}

/// A preparation keeps the selected audio component and an explicitly chosen
/// text subtitle of its exact source revision. Playback serves that rendition
/// only for exactly that selection, delivering the subtitle from the
/// rendition's own bytes; other selections are never substituted.
#[tokio::test(flavor = "multi_thread")]
async fn preparations_keep_the_selected_audio_component_and_text_subtitle() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "6");
    let original = f.dir.path().join("base.mp4");
    std::fs::rename(f.dir.path().join("media/film.mp4"), &original).unwrap();
    let srt = f.dir.path().join("captions.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:03,000\nKept caption\n").unwrap();
    f.media(
        "film.mp4",
        &[
            "-i",
            original.to_str().unwrap(),
            "-i",
            srt.to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-c",
            "copy",
            "-c:s",
            "mov_text",
            "-metadata:s:a:0",
            "language=eng",
            "-metadata:s:a:1",
            "language=jpn",
            "-metadata:s:s:0",
            "language=eng",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let components = playscale::components::timeline_components(&f.server.app.db, &timeline)
        .await
        .unwrap();
    let component = |kind, language: &str| {
        components
            .iter()
            .find(|c| c.kind == kind && c.language.as_deref() == Some(language))
            .unwrap()
            .id
            .clone()
    };
    use playscale_core::components::Kind;
    let japanese = component(Kind::Audio, "jpn");
    let captions = component(Kind::Subtitle, "eng");
    let operator = bearer(OPERATOR);
    let prepare = |tracks: Value, key: &'static str| {
        let f = &f;
        let operator = operator.clone();
        let source = source.clone();
        let timeline = timeline.clone();
        async move {
            f.post(
                "/api/v2/processing/preparations",
                &operator,
                Some(key),
                json!({"source": source, "timeline_id": timeline, "recipe_id": "remux_mp4",
                    "tracks": tracks}),
            )
            .await
        }
    };
    let tracks = |audio_component: Value,
                  subtitle_component: Value,
                  policy: &str,
                  audio: Value,
                  subtitle: Value| {
        json!({"audio_component_id": audio_component, "subtitle_component_id": subtitle_component,
            "subtitle_policy": policy, "audio_track_id": audio, "subtitle_track_id": subtitle})
    };
    // Refused: automatic subtitles, an explicit one with off, disagreeing
    // pins, a component absent from this revision.
    for (selection, status, code, key) in [
        (
            tracks(Value::Null, Value::Null, "auto", Value::Null, Value::Null),
            StatusCode::UNPROCESSABLE_ENTITY,
            "preparation_tracks_unsupported",
            "prepare-refused-000001",
        ),
        (
            tracks(
                Value::Null,
                json!(captions),
                "off",
                Value::Null,
                Value::Null,
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
            "subtitle_policy_conflict",
            "prepare-refused-000002",
        ),
        (
            tracks(
                json!(japanese),
                Value::Null,
                "off",
                json!("a0"),
                Value::Null,
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
            "track_component_conflict",
            "prepare-refused-000003",
        ),
        (
            tracks(
                json!("not-a-component"),
                Value::Null,
                "off",
                Value::Null,
                Value::Null,
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
            "component_unavailable",
            "prepare-refused-000004",
        ),
    ] {
        problem(&prepare(selection, key).await, status, code);
    }
    let prepared = prepare(
        tracks(
            json!(japanese),
            json!(captions),
            "require",
            Value::Null,
            Value::Null,
        ),
        "prepare-component-subtitle-1",
    )
    .await;
    assert_eq!(prepared.status, StatusCode::ACCEPTED, "{}", prepared.body);
    let job = prepared.body["id"].as_str().unwrap().to_owned();
    // The record names the streams of this revision the components resolved to.
    let recorded: String =
        sqlx::query_scalar("SELECT tracks_json FROM preparations WHERE job_id=?")
            .bind(&job)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    let recorded: Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(
        (&recorded["audio_track_id"], &recorded["subtitle_track_id"]),
        (&json!("a1"), &json!("s0")),
        "{recorded}"
    );
    let stop = CancellationToken::new();
    let worker = tokio::spawn(playscale::processing::worker(
        f.server.app.clone(),
        stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let (phase, error): (String, Option<String>) =
                sqlx::query_as("SELECT phase,error FROM processing_jobs WHERE id=?")
                    .bind(&job)
                    .fetch_one(&f.server.app.db)
                    .await
                    .unwrap();
            if phase == "completed" {
                break;
            }
            assert!(
                !["failed", "cancelled"].contains(&phase.as_str()),
                "{phase} {error:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("preparation did not complete");
    stop.cancel();
    let _ = worker.await;
    let rendition: String =
        sqlx::query_scalar("SELECT file_id FROM renditions WHERE external_id=?")
            .bind(&job)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    // The rendition holds the Japanese (22.05 kHz) audio and one mov_text subtitle.
    let path: (String, String) = sqlx::query_as(
        "SELECT l.root,f.relative_path FROM media_files f JOIN libraries l ON l.id=f.library_id WHERE f.id=?",
    )
    .bind(&rendition)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,sample_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(Path::new(&path.0).join(&path.1))
        .output()
        .unwrap();
    let streams = String::from_utf8(probe.stdout).unwrap();
    assert!(streams.contains("aac,audio,22050"), "{streams}");
    assert_eq!(streams.matches(",audio,").count(), 1, "{streams}");
    assert_eq!(streams.matches("mov_text,subtitle").count(), 1, "{streams}");
    let plan = |audio: &str, subtitle: Option<&str>| {
        let mut input = pinned_plan_input(
            "default",
            &timeline,
            "convert",
            audio,
            &["http_range", "hls"],
            &source,
        );
        input["tracks"]["subtitle_track_id"] = json!(subtitle);
        input["tracks"]["subtitle_policy"] =
            json!(if subtitle.is_some() { "require" } else { "off" });
        f.post("/api/v2/playback/plans", &auth, None, input)
    };
    // Exactly the prepared selection: the rendition, with its own subtitle.
    let kept = plan("a1", Some("s0")).await;
    assert_eq!(kept.body["operation"], "prepared", "{}", kept.body);
    assert_eq!(kept.body["source"]["file_id"], rendition.as_str());
    // Without the subtitle, or with other audio: never the rendition.
    for (audio, subtitle) in [("a1", None), ("a0", Some("s0"))] {
        let other = plan(audio, subtitle).await;
        assert_ne!(other.body["operation"], "prepared", "{}", other.body);
    }
    // Automatic selection must happen before testing prepared candidates.
    let prefs = f.get("/api/v2/profiles/default/preferences", &auth).await;
    let mut document = prefs.body.clone();
    document["subtitle_mode"] = json!("always");
    document["subtitle_languages"] = json!(["eng"]);
    let updated = f
        .call(
            "PUT",
            "/api/v2/profiles/default/preferences",
            Some(document),
            &[
                ("authorization", &auth),
                ("if-match", prefs.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK);
    let mut automatic = pinned_plan_input(
        "default",
        &timeline,
        "convert",
        "a1",
        &["http_range", "hls"],
        &source,
    );
    automatic["tracks"]["subtitle_policy"] = json!("auto");
    let automatic = f
        .post("/api/v2/playback/plans", &auth, None, automatic)
        .await;
    assert_eq!(
        automatic.body["operation"], "prepared",
        "{}",
        automatic.body
    );
    assert_eq!(automatic.body["tracks"]["subtitle_track_id"], "s0");
    // The subtitle is delivered from the rendition's bytes.
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("prepared-subtitles-01"),
            json!({"plan_token": kept.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let url = admitted.body["active"]["subtitles_url"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", admitted.body))
        .to_owned();
    let vtt = f.call("GET", &url, None, &[("authorization", &auth)]).await;
    assert_eq!(vtt.status, StatusCode::OK);
    let vtt = String::from_utf8(vtt.bytes).unwrap();
    assert!(
        vtt.contains("00:01.000 --> 00:03.000") && vtt.contains("Kept caption"),
        "{vtt}"
    );
    // A device downloads exactly this prepared version (5E): the same recipe
    // and selection; any other selection is refused, never substituted.
    let (saver, saver_auth) = f
        .device(
            &["default"],
            &["catalog:read", "downloads:manage", "viewing:write"],
        )
        .await;
    let download = |key: &'static str, audio: &str, subtitle: Option<&str>| {
        let f = &f;
        let saver_auth = saver_auth.clone();
        let body = json!({"device_id": saver, "profile_id": "default", "timeline_id": timeline,
            "version_id": null, "recipe_id": "remux_mp4",
            "tracks": {"audio_component_id": null, "subtitle_component_id": null,
                "subtitle_policy": if subtitle.is_some() { "require" } else { "off" },
                "audio_track_id": audio, "subtitle_track_id": subtitle}});
        async move {
            f.post("/api/v2/downloads", &saver_auth, Some(key), body)
                .await
        }
    };
    let saved = download("prepared-download-0001", "a1", Some("s0")).await;
    assert_eq!(saved.status, StatusCode::ACCEPTED, "{}", saved.body);
    let pinned = f
        .get(
            &format!(
                "/api/v2/downloads/{}/manifest",
                saved.body["id"].as_str().unwrap()
            ),
            &saver_auth,
        )
        .await;
    let rendition_revision: String =
        sqlx::query_scalar("SELECT revision FROM media_files WHERE id=?")
            .bind(&rendition)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    assert_eq!(
        pinned.body["files"][0]["digest"],
        format!("sha256:{rendition_revision}"),
        "{}",
        pinned.body
    );
    // The subtitle policy follows the same rule as preparing (kept only
    // when explicit and required).
    for (key, policy, subtitle, code) in [
        (
            "prepared-download-0004",
            "off",
            json!("s0"),
            "subtitle_policy_conflict",
        ),
        (
            "prepared-download-0005",
            "require",
            Value::Null,
            "preparation_tracks_unsupported",
        ),
        (
            "prepared-download-0006",
            "auto",
            json!("s0"),
            "preparation_tracks_unsupported",
        ),
    ] {
        let body = json!({"device_id": saver, "profile_id": "default", "timeline_id": timeline,
            "version_id": null, "recipe_id": "remux_mp4",
            "tracks": {"audio_component_id": null, "subtitle_component_id": null,
                "subtitle_policy": policy, "audio_track_id": "a1", "subtitle_track_id": subtitle}});
        let refused = f
            .post("/api/v2/downloads", &saver_auth, Some(key), body)
            .await;
        assert_eq!(refused.body["code"], code, "{}", refused.body);
    }
    // Another generated version binding only an interval of the same output
    // (sorting first) is never the listed pin: it cannot be downloaded.
    let output_revision: String = sqlx::query_scalar("SELECT revision FROM media_files WHERE id=?")
        .bind(&rendition)
        .fetch_one(&f.server.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO media_versions(id,timeline_id,origin,equivalence) VALUES ('0-interval',?,'generated','declared')")
        .bind(&timeline)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision,start_ms,end_ms) VALUES ('0-interval',1,?,?,0,1000)")
        .bind(&rendition)
        .bind(&output_revision)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    // The rendition's generated version (as listed) pins exactly it.
    let listed = f
        .get(
            &format!("/api/v2/processing/renditions?timeline_id={timeline}"),
            &operator,
        )
        .await;
    let version = listed.body["items"][0]["version_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(version, "0-interval");
    let pinned_body = json!({"device_id": saver, "profile_id": "default", "timeline_id": timeline,
        "version_id": version, "recipe_id": "remux_mp4",
        "tracks": listed.body["items"][0]["tracks"].clone()});
    let exact = f
        .post(
            "/api/v2/downloads",
            &saver_auth,
            Some("prepared-download-0007"),
            pinned_body,
        )
        .await;
    assert_eq!(exact.status, StatusCode::ACCEPTED, "{}", exact.body);
    for (key, audio, subtitle) in [
        ("prepared-download-0002", "a0", Some("s0")),
        ("prepared-download-0003", "a1", None),
    ] {
        let refused = download(key, audio, subtitle).await;
        assert_eq!(
            refused.body["code"], "download_preparation_unavailable",
            "{}",
            refused.body
        );
    }
    // A replan of the prepared delivery for the same selection keeps it.
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let again = plan("a1", Some("s0")).await;
    assert_eq!(again.body["operation"], "prepared", "{}", again.body);
    let staged = f
        .post(
            &format!("/api/v2/playback/delivery-sessions/{id}/changes"),
            &auth,
            Some("prepared-subtitles-replan-1"),
            json!({"kind":"replan", "expected_generation":"1",
                "plan_token":again.body["plan_token"], "position_ms":1500}),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{}", staged.body);
    assert!(
        staged.body["pending"]["subtitles_url"].is_string(),
        "{}",
        staged.body
    );
    // The rendition's source changes: the prepared download is no longer
    // current for it and expires (never served from a stale preparation).
    sqlx::query("UPDATE media_files SET revision='changed-source' WHERE id=?")
        .bind(source["file_id"].as_str().unwrap())
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let expired = f
        .get(
            &format!("/api/v2/downloads/{}", saved.body["id"].as_str().unwrap()),
            &saver_auth,
        )
        .await;
    assert_eq!(expired.body["state"], "expired", "{}", expired.body);
}

/// One gray frame of `path` at `at` seconds (320x240 sources).
fn gray_frame(path: &Path, at: &str) -> Vec<u8> {
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", at, "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(frame.len(), 320 * 240, "{at}");
    frame
}

fn mean_difference(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y) as u64)
        .sum::<u64>()
        / a.len() as u64
}

/// A version bound to an interval of its file is a timeline of that
/// interval: only a conversion presents it, logical time zero is the
/// interval's start, the delivery ends at its end, and a changed interval
/// fences plans signed before it.
#[tokio::test(flavor = "multi_thread")]
async fn interval_bindings_play_their_clip_of_the_file() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_copy_routes(true).await;
    two_audio_mp4(&f, "30");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    sqlx::query(
        "UPDATE version_files SET start_ms=10000,end_ms=20000 WHERE version_id IN \
         (SELECT id FROM media_versions WHERE timeline_id=?)",
    )
    .bind(&timeline)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    let plan = |mode: &str| {
        let input = plan_input("default", &timeline, mode, None, &["http_range", "hls"]);
        f.post("/api/v2/playback/plans", &auth, None, input)
    };
    assert_eq!(
        plan("original").await.body["reason_codes"][0],
        "interval_requires_conversion"
    );
    // Not the original bytes, nor an (eligible) copy: a conversion.
    let converted = plan("auto").await;
    assert_eq!(
        converted.body["operation"], "video_transcode",
        "{}",
        converted.body
    );
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("interval-delivery-01"),
            json!({"plan_token": converted.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    assert_eq!(admitted.body["logical_duration_ms"], 10000);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/playback/delivery-sessions/{id}");
    f.until(&id, &auth, |d| {
        d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 0
    })
    .await;
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let fragment = f.dir.path().join("interval.mp4");
    std::fs::write(
        &fragment,
        [
            get(format!("/api/v2/streams/{id}/1/variants/main/init.mp4")).await,
            get(format!("/api/v2/streams/{id}/1/segments/0.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    // Logical 0.5 s is file 10.5 s, not file 0.5 s.
    let source = f.dir.path().join("media/film.mp4");
    let delivered = gray_frame(&fragment, "0.5");
    let inside = mean_difference(&delivered, &gray_frame(&source, "10.5"));
    let outside = mean_difference(&delivered, &gray_frame(&source, "0.5"));
    assert!(inside * 4 < outside, "inside {inside}, outside {outside}");
    // A seek within the interval: the generation ends at its end.
    let seek = f
        .post(
            &format!("{path}/changes"),
            &auth,
            Some("interval-seek-000001"),
            json!({"kind":"seek","expected_generation":"1","position_ms":8000}),
        )
        .await;
    assert_eq!(seek.status, StatusCode::ACCEPTED, "{}", seek.body);
    let done = f
        .until(&id, &auth, |d| {
            d["pending"]["available_end_ms"].as_u64().unwrap_or(0) >= 9900
                || d["pending"]["status"] == "failed"
        })
        .await;
    let end = done["pending"]["available_end_ms"].as_u64().unwrap();
    assert!((9900..=10100).contains(&end), "{done}");
    let variant =
        String::from_utf8(get(format!("/api/v2/streams/{id}/2/variants/main/index.m3u8")).await)
            .unwrap();
    assert!(variant.contains("#EXT-X-ENDLIST"), "{variant}");
    assert_eq!(
        f.call("DELETE", &path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    // The interval changes: a plan signed for the old one is refused.
    let retained = plan("auto").await;
    sqlx::query(
        "UPDATE version_files SET end_ms=25000 WHERE version_id IN \
         (SELECT id FROM media_versions WHERE timeline_id=?)",
    )
    .bind(&timeline)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("interval-delivery-02"),
            json!({"plan_token": retained.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // A fresh plan for the changed interval is a different request: the first
    // delivery's key cannot replay it.
    let changed = plan("auto").await;
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("interval-delivery-01"),
            json!({"plan_token": changed.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency_conflict",
    );
    // Bounds outside the probed file are never played.
    sqlx::query(
        "UPDATE version_files SET end_ms=90000 WHERE version_id IN \
         (SELECT id FROM media_versions WHERE timeline_id=?)",
    )
    .bind(&timeline)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    assert_eq!(
        plan("auto").await.body["reason_codes"][0],
        "source_unavailable"
    );
}

/// A multipart version plays its parts in order as one timeline: only by a
/// conversion, with the parts' summed duration, a position past the first
/// part reaching the second, and a changed or incompatible part fencing it.
#[tokio::test(flavor = "multi_thread")]
async fn multipart_versions_play_their_parts_in_order() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_copy_routes(true).await;
    // The second part is Matroska (another container and time base); the
    // others differ in picture size or audio language.
    for (name, video, tone, language) in [
        ("a-part1.mp4", "testsrc2=size=320x240:rate=24", "440", "eng"),
        (
            "b-part2.mkv",
            "testsrc2=size=320x240:rate=24,hflip",
            "880",
            "eng",
        ),
        ("c-other.mp4", "testsrc2=size=640x480:rate=24", "660", "eng"),
        (
            "d-language.mp4",
            "testsrc2=size=320x240:rate=24",
            "660",
            "jpn",
        ),
    ] {
        let tone = format!("sine=frequency={tone}:sample_rate=44100");
        let language = format!("language={language}");
        f.media(
            name,
            &[
                "-f",
                "lavfi",
                "-i",
                video,
                "-f",
                "lavfi",
                "-i",
                &tone,
                "-t",
                "6",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "48",
                "-c:a",
                "aac",
                "-metadata:s:a:0",
                &language,
            ],
        );
    }
    f.scan().await;
    let file = |name: &str| {
        let db = f.server.app.db.clone();
        let name = name.to_owned();
        async move {
            sqlx::query_as::<_, (String, String)>(
                "SELECT id,revision FROM media_files WHERE relative_path=?",
            )
            .bind(name)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let (first, _) = file("a-part1.mp4").await;
    let (second, second_revision) = file("b-part2.mkv").await;
    let (version, timeline): (String, String) = sqlx::query_as(
        "SELECT v.id,v.timeline_id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=?",
    )
    .bind(&first)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    // Parts of one version belong to its edition (as a scan of a split film
    // would record them).
    sqlx::query(
        "UPDATE media_files SET edition_id=(SELECT edition_id FROM media_files WHERE id=?) \
         WHERE relative_path IN ('b-part2.mkv','c-other.mp4','d-language.mp4')",
    )
    .bind(&first)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    let bind_second = |file: String, revision: String| {
        let db = f.server.app.db.clone();
        let version = version.clone();
        async move {
            sqlx::query("DELETE FROM version_files WHERE version_id=? AND part=2")
                .bind(&version)
                .execute(&db)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,2,?,?)",
            )
            .bind(&version)
            .bind(file)
            .bind(revision)
            .execute(&db)
            .await
            .unwrap();
        }
    };
    bind_second(second.clone(), second_revision.clone()).await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let plan = |mode: &str, policy: &str| {
        let mut input = plan_input("default", &timeline, mode, None, &["http_range", "hls"]);
        input["tracks"]["subtitle_policy"] = json!(policy);
        f.post("/api/v2/playback/plans", &auth, None, input)
    };
    assert_eq!(
        plan("original", "auto").await.body["reason_codes"][0],
        "multipart_requires_conversion"
    );
    assert_eq!(
        plan("auto", "require").await.body["reason_codes"][0],
        "subtitle_unavailable"
    );
    let converted = plan("auto", "auto").await;
    assert_eq!(
        converted.body["operation"], "video_transcode",
        "{}",
        converted.body
    );
    assert_eq!(converted.body["source"]["file_id"], first.as_str());
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-delivery-1"),
            json!({"plan_token": converted.body["plan_token"], "start_ms": 7000}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let duration = admitted.body["logical_duration_ms"].as_u64().unwrap();
    assert!((11_900..=12_100).contains(&duration), "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    f.until(&id, &auth, |d| {
        d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 7000
    })
    .await;
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let fragment = f.dir.path().join("multipart.mp4");
    std::fs::write(
        &fragment,
        [
            get(format!("/api/v2/streams/{id}/1/variants/main/init.mp4")).await,
            get(format!("/api/v2/streams/{id}/1/segments/0.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    // Logical 7.5 s is the second part at 1.5 s (mirrored), not the first.
    let media = f.dir.path().join("media");
    let delivered = gray_frame(&fragment, "0.5");
    let second_part = mean_difference(&delivered, &gray_frame(&media.join("b-part2.mkv"), "1.5"));
    let first_part = mean_difference(&delivered, &gray_frame(&media.join("a-part1.mp4"), "1.5"));
    assert!(
        second_part * 4 < first_part,
        "second {second_part}, first {first_part}"
    );
    assert_eq!(
        f.call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    // The second part's binding changes: a plan signed before is refused.
    let retained = plan("auto", "auto").await;
    bind_second(second.clone(), "another-revision".into()).await;
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-delivery-2"),
            json!({"plan_token": retained.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // A part of another picture size, or whose audio is another language at
    // the same position, cannot join the timeline.
    // A part of another picture size cannot join the timeline; a part without
    // the selected audio component (only Japanese audio) cannot play it.
    for (name, reason) in [
        ("c-other.mp4", "source_unavailable"),
        ("d-language.mp4", "multipart_audio_unavailable"),
    ] {
        let (other, other_revision) = file(name).await;
        bind_second(other, other_revision).await;
        assert_eq!(
            plan("auto", "auto").await.body["reason_codes"][0],
            reason,
            "{name}"
        );
    }
    // A later part with HDR video is never converted without tone mapping.
    bind_second(second.clone(), second_revision.clone()).await;
    let tracks: String = sqlx::query_scalar("SELECT tracks_json FROM media_files WHERE id=?")
        .bind(&second)
        .fetch_one(&f.server.app.db)
        .await
        .unwrap();
    let mut hdr: Value = serde_json::from_str(&tracks).unwrap();
    for track in hdr.as_array_mut().unwrap() {
        if track["kind"] == "video" {
            track["color_transfer"] = json!("smpte2084");
        }
    }
    let set_tracks = |value: String| {
        let db = f.server.app.db.clone();
        let second = second.clone();
        async move {
            sqlx::query("UPDATE media_files SET tracks_json=? WHERE id=?")
                .bind(value)
                .bind(second)
                .execute(&db)
                .await
                .unwrap();
        }
    };
    set_tracks(hdr.to_string()).await;
    assert_eq!(
        plan("auto", "auto").await.body["reason_codes"][0],
        "source_unavailable"
    );
    set_tracks(tracks).await;
    // A clipped binding added after a plan was signed fences that plan.
    let signed = plan("auto", "auto").await;
    assert_eq!(
        signed.body["operation"], "video_transcode",
        "{}",
        signed.body
    );
    let (other, other_revision) = file("c-other.mp4").await;
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision,start_ms,end_ms) VALUES (?,3,?,?,0,1000)")
        .bind(&version)
        .bind(&other)
        .bind(&other_revision)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    problem(
        &f.post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-delivery-4"),
            json!({"plan_token": signed.body["plan_token"], "start_ms": 0}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    sqlx::query("DELETE FROM version_files WHERE version_id=? AND part=3")
        .bind(&version)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    // From the start, every part plays: the Matroska part's own time base does
    // not truncate the timeline.
    let whole = plan("auto", "auto").await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-delivery-3"),
            json!({"plan_token": whole.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let done = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 11_800
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(done["active"]["status"], "failed", "{done}");
    // A later part changes while the delivery is live: its generation fails.
    let seek = f
        .post(
            &format!("/api/v2/playback/delivery-sessions/{id}/changes"),
            &auth,
            Some("multipart-seek-00001"),
            json!({"kind":"seek","expected_generation":"1","position_ms":1000}),
        )
        .await;
    assert_eq!(seek.status, StatusCode::ACCEPTED, "{}", seek.body);
    sqlx::query("UPDATE media_files SET revision='rescanned' WHERE id=?")
        .bind(&second)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    // Like a changed single source: every current generation fails.
    let fenced = f.until(&id, &auth, |d| d["status"] == "failed").await;
    assert_eq!(fenced["status"], "failed", "{fenced}");
}

/// An explicitly chosen text subtitle of a multipart version is delivered
/// across its parts: each part's own stream of the component, its cues moved
/// by the parts before it into timeline time, as the live generation's
/// subtitle rendition. A part without the component refuses the choice.
#[tokio::test(flavor = "multi_thread")]
async fn multipart_text_subtitles_are_joined_in_timeline_time() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    let captions = |name: &str, cues: &str| {
        let path = f.dir.path().join(name);
        std::fs::write(&path, cues).unwrap();
        path
    };
    let first_cues = captions(
        "first.srt",
        "1\n00:00:01,000 --> 00:00:02,000\nfirst part\n\n2\n00:00:04,000 --> 00:00:05,000\nfirst late\n",
    );
    let second_cues = captions(
        "second.srt",
        "1\n00:00:01,000 --> 00:00:02,000\nsecond part\n",
    );
    for (name, cues) in [
        ("a-part1.mp4", Some(&first_cues)),
        ("b-part2.mp4", Some(&second_cues)),
        ("c-nosubs.mp4", None),
    ] {
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
        ];
        if let Some(cues) = cues {
            args.extend([
                "-i",
                cues.to_str().unwrap(),
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
            ]);
            args.extend(["-c:s", "mov_text", "-metadata:s:s:0", "language=eng"]);
        }
        args.extend([
            "-t",
            "6",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
            "-metadata:s:a:0",
            "language=eng",
        ]);
        f.media(name, &args);
    }
    f.scan().await;
    let file = |name: &str| {
        let db = f.server.app.db.clone();
        let name = name.to_owned();
        async move {
            sqlx::query_as::<_, (String, String, f64)>(
                "SELECT id,revision,duration_seconds FROM media_files WHERE relative_path=?",
            )
            .bind(name)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let (first, _, first_seconds) = file("a-part1.mp4").await;
    let (version, timeline): (String, String) = sqlx::query_as(
        "SELECT v.id,v.timeline_id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=?",
    )
    .bind(&first)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE media_files SET edition_id=(SELECT edition_id FROM media_files WHERE id=?) \
         WHERE relative_path IN ('b-part2.mp4','c-nosubs.mp4')",
    )
    .bind(&first)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    let bind_second = |name: &'static str| {
        let db = f.server.app.db.clone();
        let version = version.clone();
        let file = file(name);
        async move {
            let (id, revision, _) = file.await;
            sqlx::query("DELETE FROM version_files WHERE version_id=? AND part=2")
                .bind(&version)
                .execute(&db)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,2,?,?)",
            )
            .bind(&version)
            .bind(id)
            .bind(revision)
            .execute(&db)
            .await
            .unwrap();
        }
    };
    let (_, auth) = f.device(&["default"], PLAYER).await;
    // The English subtitle component, as the timeline's components name it.
    let subtitle_component = || {
        let db = f.server.app.db.clone();
        let timeline = timeline.clone();
        async move {
            playscale::components::timeline_components(&db, &timeline)
                .await
                .unwrap()
                .into_iter()
                .find(|c| c.kind == playscale_core::components::Kind::Subtitle)
                .unwrap()
                .id
        }
    };
    let plan = |policy: &'static str, modes: Value| {
        let f = &f;
        let auth = auth.clone();
        let timeline = timeline.clone();
        async move {
            let mut input = plan_input("default", &timeline, "auto", None, &["hls"]);
            input["tracks"]["subtitle_component_id"] = json!(subtitle_component().await);
            input["tracks"]["subtitle_policy"] = json!(policy);
            input["client"]["subtitle_modes"] = modes;
            f.post("/api/v2/playback/plans", &auth, None, input).await
        }
    };
    let text = || json!(["text", "hls_text"]);
    // A part without the chosen subtitle component cannot carry it.
    bind_second("c-nosubs.mp4").await;
    let refused = plan("require", text()).await;
    assert_eq!(
        refused.body["reason_codes"][0], "subtitle_multipart_unavailable",
        "{}",
        refused.body
    );
    bind_second("b-part2.mp4").await;
    let planned = plan("require", text()).await;
    assert_eq!(
        planned.body["operation"], "video_transcode",
        "{}",
        planned.body
    );
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-subtitles-1"),
            json!({"plan_token": planned.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let done = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 11_800
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(done["active"]["status"], "failed", "{done}");
    let url = done["active"]["subtitles_url"].as_str().unwrap().to_owned();
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            String::from_utf8(reply.bytes).unwrap()
        }
    };
    // Every segment of the rendition, its cues in media time.
    let playlist = get(url).await;
    let mut cues = std::collections::BTreeMap::new();
    for name in playlist.lines().filter_map(|l| l.strip_prefix("../../")) {
        let segment = get(format!("/api/v2/streams/{id}/1/{name}")).await;
        for block in segment.split("\n\n").skip(1) {
            let mut lines = block.lines();
            let timing = lines.next().unwrap().to_owned();
            cues.insert(lines.collect::<Vec<_>>().join("\n"), timing);
        }
    }
    let origin = done["active"]["media_time_origin_ms"].as_u64().unwrap();
    let at = |ms: u64| {
        let ms = ms - origin;
        let (s, ms) = (ms / 1000, ms % 1000);
        format!("{:02}:{:02}:{:02}.{ms:03}", s / 3600, s / 60 % 60, s % 60)
    };
    // The second part's cue follows the first part's pinned duration.
    let offset = (first_seconds * 1000.0) as u64;
    assert_eq!(
        cues.get("first part"),
        Some(&format!("{} --> {}", at(1_000), at(2_000))),
        "{cues:?}"
    );
    assert_eq!(
        cues.get("first late"),
        Some(&format!("{} --> {}", at(4_000), at(5_000))),
        "{cues:?}"
    );
    assert_eq!(
        cues.get("second part"),
        Some(&format!(
            "{} --> {}",
            at(offset + 1_000),
            at(offset + 2_000)
        )),
        "{cues:?}"
    );
    assert_eq!(cues.len(), 3, "{cues:?}");
    // Automatic selection across parts: the profile's English subtitles,
    // only as a text component every part carries.
    let prefs = f.get("/api/v2/profiles/default/preferences", &auth).await;
    let mut document = prefs.body.clone();
    document["subtitle_mode"] = json!("always");
    document["audio_languages"] = json!(["eng"]);
    document["subtitle_languages"] = json!(["eng"]);
    let replaced = f
        .call(
            "PUT",
            "/api/v2/profiles/default/preferences",
            Some(document),
            &[
                ("authorization", &auth),
                ("if-match", prefs.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    let automatic = |policy: &'static str| {
        let f = &f;
        let auth = auth.clone();
        let timeline = timeline.clone();
        async move {
            let mut input = plan_input("default", &timeline, "auto", None, &["hls"]);
            input["tracks"]["subtitle_policy"] = json!(policy);
            input["client"]["subtitle_modes"] = json!(["text", "hls_text"]);
            f.post("/api/v2/playback/plans", &auth, None, input).await
        }
    };
    // A part without the component: "auto" plays without subtitles and
    // "require" is refused, never another part's track alone.
    bind_second("c-nosubs.mp4").await;
    let without = automatic("auto").await;
    assert_eq!(without.body["status"], "ready", "{}", without.body);
    assert_eq!(without.body["tracks"]["subtitle_track_id"], Value::Null);
    assert_eq!(
        automatic("require").await.body["reason_codes"][0],
        "subtitle_unavailable"
    );
    bind_second("b-part2.mp4").await;
    let selected = automatic("auto").await;
    assert_eq!(selected.body["status"], "ready", "{}", selected.body);
    assert_eq!(selected.body["tracks"]["subtitle_track_id"], "s0");
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-subtitles-auto"),
            json!({"plan_token": selected.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let auto_id = admitted.body["id"].as_str().unwrap().to_owned();
    let done = f
        .until(&auto_id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 11_800
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(done["active"]["status"], "failed", "{done}");
    let playlist = get(done["active"]["subtitles_url"].as_str().unwrap().to_owned()).await;
    let mut joined = String::new();
    for name in playlist.lines().filter_map(|l| l.strip_prefix("../../")) {
        joined += &get(format!("/api/v2/streams/{auto_id}/1/{name}")).await;
    }
    assert!(
        joined.contains("first part") && joined.contains("second part"),
        "{joined}"
    );
    // Burned subtitles are not delivered across parts.
    let burned = plan("require", json!(["burned"])).await;
    assert_eq!(burned.body["status"], "blocked", "{}", burned.body);
}

/// A bitrate cap is kept by a capped conversion: byte delivery cannot promise
/// a peak, the encoder is VBV-limited below the cap, every published segment
/// is measured within it, and the manifest advertises it. A cap too small to
/// leave room for video is refused.
#[tokio::test(flavor = "multi_thread")]
async fn bitrate_caps_are_kept_by_capped_conversions() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // Random noise: several Mb/s uncapped at this size.
    f.media(
        "noise.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=640x360:rate=24,geq=random(1)*255:128:128",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let plan = |cap: &str| {
        let mut input = plan_input("default", &timeline, "auto", None, &["http_range", "hls"]);
        input["quality"]["max_bitrate_bps"] = json!(cap);
        f.post("/api/v2/playback/plans", &auth, None, input)
    };
    assert_eq!(
        plan("400000").await.body["reason_codes"][0],
        "conversion_exceeds_quality_limit"
    );
    let capped = plan("1000000").await;
    assert_eq!(
        capped.body["operation"], "video_transcode",
        "{}",
        capped.body
    );
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("capped-delivery-0001"),
            json!({"plan_token": capped.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let done = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 9_900
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(done["active"]["status"], "failed", "{done}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let master =
        String::from_utf8(get(format!("/api/v2/streams/{id}/1/master.m3u8")).await).unwrap();
    assert!(master.contains("BANDWIDTH=1000000,"), "{master}");
    let variant =
        String::from_utf8(get(format!("/api/v2/streams/{id}/1/variants/main/index.m3u8")).await)
            .unwrap();
    let mut measured = 0;
    let mut duration_ms = None;
    for line in variant.lines() {
        if let Some(seconds) = line.strip_prefix("#EXTINF:") {
            let seconds: f64 = seconds.trim_end_matches(',').parse().unwrap();
            duration_ms = Some((seconds * 1000.0) as u64);
        } else if let (Some(ms), false) = (duration_ms.take(), line.starts_with('#')) {
            let index = line.rsplit('/').next().unwrap();
            let bytes = get(format!("/api/v2/streams/{id}/1/segments/{index}"))
                .await
                .len() as u64;
            if ms >= 3_000 {
                assert!(
                    bytes * 8_000 <= 1_000_000 * ms,
                    "segment {index}: {bytes} bytes in {ms} ms"
                );
                measured += 1;
            }
        }
    }
    assert!(measured >= 2, "{variant}");
}

/// A part plays for exactly its pinned duration even when the streams the
/// delivery joins are shorter than its container: continuous playback crosses
/// the boundary where seeking places it.
#[tokio::test(flavor = "multi_thread")]
async fn multipart_boundaries_follow_pinned_part_durations() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // Two seconds of picture in a three-second first part whose sound starts
    // half a second in; the third part has anamorphic (8:9) pixels.
    f.media(
        "a-short.mp4",
        &[
            "-f",
            "lavfi",
            "-t",
            "2",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-itsoffset",
            "0.5",
            "-f",
            "lavfi",
            "-t",
            "2.5",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ],
    );
    f.media(
        "b-long.mp4",
        &[
            "-f",
            "lavfi",
            "-t",
            "3",
            "-i",
            "testsrc2=size=320x240:rate=24,hflip",
            "-f",
            "lavfi",
            "-t",
            "3",
            "-i",
            "sine=frequency=880:sample_rate=44100",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ],
    );
    f.media(
        "c-anamorphic.mp4",
        &[
            "-f",
            "lavfi",
            "-t",
            "3",
            "-i",
            "testsrc2=size=320x240:rate=24,setsar=8/9",
            "-f",
            "lavfi",
            "-t",
            "3",
            "-i",
            "sine=frequency=660:sample_rate=44100",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ],
    );
    f.scan().await;
    let ids: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id,revision,relative_path FROM media_files ORDER BY relative_path")
            .fetch_all(&f.server.app.db)
            .await
            .unwrap();
    let (first, second, second_revision) = (&ids[0].0, &ids[1].0, &ids[1].1);
    let (third, third_revision) = (&ids[2].0, &ids[2].1);
    sqlx::query("UPDATE media_files SET edition_id=(SELECT edition_id FROM media_files WHERE id=?) WHERE id IN (?,?)")
        .bind(first)
        .bind(second)
        .bind(third)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let (version, timeline): (String, String) = sqlx::query_as(
        "SELECT v.id,v.timeline_id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=?",
    )
    .bind(first)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,2,?,?)",
    )
    .bind(&version)
    .bind(second)
    .bind(second_revision)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,3,?,?)",
    )
    .bind(&version)
    .bind(third)
    .bind(third_revision)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["hls"]),
        )
        .await;
    assert_eq!(plan.body["operation"], "video_transcode", "{}", plan.body);
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("multipart-boundary-1"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let done = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 8_900
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(done["active"]["status"], "failed", "{done}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let joined = f.dir.path().join("joined.mp4");
    std::fs::write(
        &joined,
        [
            get(format!("/api/v2/streams/{id}/1/variants/main/init.mp4")).await,
            get(format!("/api/v2/streams/{id}/1/segments/0.m4s")).await,
            get(format!("/api/v2/streams/{id}/1/segments/1.m4s")).await,
            get(format!("/api/v2/streams/{id}/1/segments/2.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    // The first part's sound still starts half a second in.
    let pcm = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&joined)
        .args([
            "-map", "0:a", "-f", "s16le", "-ac", "1", "-ar", "48000", "-",
        ])
        .output()
        .unwrap()
        .stdout;
    let onset = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .position(|s| i16::from_le_bytes(*s).unsigned_abs() > 1000)
        .unwrap() as f64
        / 48_000.0;
    assert!((0.45..0.56).contains(&onset), "audio onset {onset}");
    let second_part = gray_frame(&f.dir.path().join("media/b-long.mp4"), "0.5");
    // Logical 3.5 s is the second part at 0.5 s; at 2.5 s the first part's
    // last picture still holds.
    let after = mean_difference(&gray_frame(&joined, "3.5"), &second_part);
    let before = mean_difference(&gray_frame(&joined, "2.5"), &second_part);
    assert!(after * 4 < before, "after {after}, before {before}");
}

/// A copy keeps the source's rate, so its manifest declares that rate (the
/// higher of the probed average and the measured peak), not the transcode's.
#[tokio::test(flavor = "multi_thread")]
async fn copied_manifests_declare_the_source_rate() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::with_copy_routes(true).await;
    // Noise: about 18 Mb/s of H.264 and AAC in Matroska (every stream starts at
    // zero), so the original is copyable.
    f.media(
        "noise.mkv",
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=640x360:rate=24,geq=random(1)*255:128:128",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let file = source["file_id"].as_str().unwrap().to_owned();
    let mut input = plan_input("default", &timeline, "auto", None, &["http_range", "hls"]);
    input["failed_candidate_ids"] = json!([format!("o-{file}")]);
    let copy = f.post("/api/v2/playback/plans", &auth, None, input).await;
    assert_eq!(copy.body["operation"], "remux", "{}", copy.body);
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("copied-rate-000001"),
            json!({"plan_token": copy.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    f.until(&id, &auth, |d| {
        d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 8_000
    })
    .await;
    let master = f
        .get(&format!("/api/v2/streams/{id}/1/master.m3u8"), &auth)
        .await;
    let master = String::from_utf8(master.bytes).unwrap();
    let bandwidth: u64 = master
        .split("BANDWIDTH=")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .unwrap()
        .parse()
        .unwrap();
    assert!(bandwidth > 10_000_000, "{master}");
}

/// The dominant tone (Hz) of a mono 48 kHz PCM window, by zero crossings.
fn tone(pcm: &[i16]) -> u64 {
    let crossings = pcm.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count() as u64;
    crossings * 48_000 / 2 / pcm.len() as u64
}

/// A multipart version whose parts order two same-language, same-role audio
/// alternatives differently still plays the selected one in every part: each
/// part's own occurrence of the component, never the first part's position.
#[tokio::test(flavor = "multi_thread")]
async fn multipart_audio_follows_the_component_in_every_part() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // "Stereo" is 440 Hz and "Director" 880 Hz in both parts, in opposite
    // stream orders; both English with no roles. Matroska keeps stream
    // titles (MP4 drops them, which would make the two indistinguishable).
    for (name, first, second) in [
        ("a-part.mkv", ("Stereo", "440"), ("Director", "880")),
        ("b-part.mkv", ("Director", "880"), ("Stereo", "440")),
    ] {
        let (a, b) = (
            format!("sine=frequency={}:sample_rate=48000", first.1),
            format!("sine=frequency={}:sample_rate=48000", second.1),
        );
        let (ta, tb) = (format!("title={}", first.0), format!("title={}", second.0));
        f.media(
            name,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                &a,
                "-f",
                "lavfi",
                "-i",
                &b,
                "-t",
                "4",
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
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:a:0",
                &ta,
                "-metadata:s:a:1",
                "language=eng",
                "-metadata:s:a:1",
                &tb,
            ],
        );
    }
    f.scan().await;
    let ids: Vec<(String, String)> =
        sqlx::query_as("SELECT id,revision FROM media_files ORDER BY relative_path")
            .fetch_all(&f.server.app.db)
            .await
            .unwrap();
    let ((first, first_revision), (second, second_revision)) = (ids[0].clone(), ids[1].clone());
    sqlx::query("UPDATE media_files SET edition_id=(SELECT edition_id FROM media_files WHERE id=?) WHERE id=?")
        .bind(&first)
        .bind(&second)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    let (version, timeline): (String, String) = sqlx::query_as(
        "SELECT v.id,v.timeline_id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=?",
    )
    .bind(&first)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,2,?,?)",
    )
    .bind(&version)
    .bind(&second)
    .bind(&second_revision)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let components = playscale::components::timeline_components(&f.server.app.db, &timeline)
        .await
        .unwrap();
    let director = components
        .iter()
        .find(|c| c.title.as_deref() == Some("Director"))
        .unwrap()
        .id
        .clone();
    // Delivered tones in the first part (logical 1-2 s) and the second (5-6 s).
    let deliver = |input: Value, key: &'static str| {
        let f = &f;
        let auth = auth.clone();
        async move {
            let plan = f.post("/api/v2/playback/plans", &auth, None, input).await;
            assert_eq!(plan.body["operation"], "video_transcode", "{}", plan.body);
            let admitted = f
                .post(
                    "/api/v2/playback/delivery-sessions",
                    &auth,
                    Some(key),
                    json!({"plan_token": plan.body["plan_token"], "start_ms": 0}),
                )
                .await;
            assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
            let id = admitted.body["id"].as_str().unwrap().to_owned();
            let done = f
                .until(&id, &auth, |d| {
                    d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 7_900
                        || d["active"]["status"] == "failed"
                })
                .await;
            assert_ne!(done["active"]["status"], "failed", "{done}");
            let mut bytes = Vec::new();
            for path in ["variants/main/init.mp4", "segments/0.m4s", "segments/1.m4s"] {
                let reply = f
                    .call(
                        "GET",
                        &format!("/api/v2/streams/{id}/1/{path}"),
                        None,
                        &[("authorization", &auth)],
                    )
                    .await;
                assert_eq!(reply.status, StatusCode::OK, "{path}");
                bytes.extend(reply.bytes);
            }
            let joined = f.dir.path().join(format!("{key}.mp4"));
            std::fs::write(&joined, bytes).unwrap();
            let pcm = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&joined)
                .args([
                    "-map", "0:a", "-f", "s16le", "-ac", "1", "-ar", "48000", "-",
                ])
                .output()
                .unwrap()
                .stdout;
            let pcm: Vec<i16> = pcm
                .as_chunks::<2>()
                .0
                .iter()
                .map(|s| i16::from_le_bytes(*s))
                .collect();
            (tone(&pcm[48_000..96_000]), tone(&pcm[240_000..288_000]))
        }
    };
    // The default (first) audio of part one is "Stereo": 440 Hz throughout.
    let (one, two) = deliver(
        plan_input("default", &timeline, "auto", None, &["hls"]),
        "mp-audio-default-1",
    )
    .await;
    assert!(
        (420..=460).contains(&one) && (420..=460).contains(&two),
        "{one} {two}"
    );
    // The "Director" component: 880 Hz throughout.
    let mut pinned = plan_input("default", &timeline, "auto", None, &["hls"]);
    pinned["tracks"]["audio_component_id"] = json!(director);
    let (one, two) = deliver(pinned, "mp-audio-director-1").await;
    assert!(
        (860..=900).contains(&one) && (860..=900).contains(&two),
        "{one} {two}"
    );
    // A live multipart delivery can switch to the other component by replan.
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "auto", None, &["hls"]),
        )
        .await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("mp-audio-switch-0001"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    f.until(&id, &auth, |d| d["active"]["status"] == "active")
        .await;
    let mut switch = plan_input("default", &timeline, "auto", None, &["hls"]);
    switch["tracks"]["audio_component_id"] = json!(director);
    let switch = f.post("/api/v2/playback/plans", &auth, None, switch).await;
    let staged = f
        .post(
            &format!("/api/v2/playback/delivery-sessions/{id}/changes"),
            &auth,
            Some("mp-audio-switch-0002"),
            json!({"kind":"replan","expected_generation":"1","plan_token":switch.body["plan_token"],"position_ms":5000}),
        )
        .await;
    assert_eq!(staged.status, StatusCode::ACCEPTED, "{}", staged.body);
    let _ = first_revision;
}

/// An uncapped software conversion on a server with adaptive bitrate enabled
/// is a two-rung ladder: a capped main (at most 720p) and a capped low (at
/// most 360p) variant from one encode, segments aligned and published only
/// when both have them, each rung described and capped from its own output,
/// holding both execution units. A client cap keeps a single variant.
#[tokio::test(flavor = "multi_thread")]
async fn adaptive_conversions_publish_aligned_capped_variants() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let mut f = Fixture::with_settings(playscale::processing::Settings {
        experimental_adaptive_bitrate: true,
        ..Default::default()
    })
    .await;
    f.media(
        "film.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1280x720:rate=24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "20",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "48",
            "-c:a",
            "aac",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let caps = advertised_routes(&f, &auth).await;
    assert_eq!(caps, "original,prepared,software_hls");
    let plan = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "convert", None, &["hls"]),
        )
        .await;
    assert_eq!(plan.body["operation"], "video_transcode", "{}", plan.body);
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("adaptive-ladder-0001"),
            json!({"plan_token": plan.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let done = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) >= 19_900
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(done["active"]["status"], "failed", "{done}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let base = format!("/api/v2/streams/{id}/1");
    let master = String::from_utf8(get(format!("{base}/master.m3u8")).await).unwrap();
    let rungs: Vec<&str> = master
        .lines()
        .filter(|l| l.starts_with("#EXT-X-STREAM-INF"))
        .collect();
    assert_eq!(rungs.len(), 2, "{master}");
    assert!(
        rungs[0].starts_with("#EXT-X-STREAM-INF:BANDWIDTH=3000000,CODECS=\"avc1."),
        "{master}"
    );
    assert!(
        rungs[1].starts_with("#EXT-X-STREAM-INF:BANDWIDTH=800000,CODECS=\"avc1."),
        "{master}"
    );
    // Each rung names its size, read from its own output.
    assert!(rungs[0].contains(",RESOLUTION=1280x720"), "{master}");
    assert!(rungs[1].contains(",RESOLUTION=640x360"), "{master}");
    assert!(
        master.contains("variants/main/index.m3u8") && master.contains("variants/low/index.m3u8")
    );
    let main = String::from_utf8(get(format!("{base}/variants/main/index.m3u8")).await).unwrap();
    let low = String::from_utf8(get(format!("{base}/variants/low/index.m3u8")).await).unwrap();
    let entries = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| {
                l.starts_with("#EXTINF")
                    || l.starts_with("#EXT-X-MEDIA-SEQUENCE")
                    || l.starts_with("#EXT-X-ENDLIST")
            })
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(entries(&low), entries(&main), "{low}");
    // Each rung's picture, and the low rung's measured rate.
    let probe = |name: &str, init: Vec<u8>, segment: Vec<u8>| {
        let path = f.dir.path().join(name);
        std::fs::write(&path, [init, segment].concat()).unwrap();
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v",
                "-show_entries",
                "stream=height",
                "-of",
                "csv=p=0",
            ])
            .arg(&path)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let main_init = get(format!("{base}/variants/main/init.mp4")).await;
    let low_init = get(format!("{base}/variants/low/init.mp4")).await;
    assert_eq!(
        probe(
            "main.mp4",
            main_init,
            get(format!("{base}/segments/0.m4s")).await
        ),
        "720"
    );
    assert_eq!(
        probe(
            "low.mp4",
            low_init,
            get(format!("{base}/segments/low-0.m4s")).await
        ),
        "360"
    );
    let mut duration_ms = None;
    for line in low.lines() {
        if let Some(seconds) = line.strip_prefix("#EXTINF:") {
            duration_ms =
                Some((seconds.trim_end_matches(',').parse::<f64>().unwrap() * 1000.0) as u64);
        } else if let (Some(ms), Some(name)) =
            (duration_ms.take(), line.strip_prefix("../../segments/"))
        {
            let bytes = get(format!("{base}/segments/{name}")).await.len() as u64;
            if ms >= 3_000 {
                assert!(
                    bytes * 8_000 <= 800_000 * ms,
                    "{name}: {bytes} bytes in {ms} ms"
                );
            }
        }
    }
    // Opt-in: the exact output, for scripts/check_hls_subtitles.mjs --abr.
    if let Some(dir) = std::env::var_os("MOTION_DUMP_HLS") {
        let dir = std::path::Path::new(&dir).join("adaptive-ladder");
        let mut files = vec![
            "master.m3u8".to_owned(),
            "variants/main/index.m3u8".into(),
            "variants/main/init.mp4".into(),
            "variants/low/index.m3u8".into(),
            "variants/low/init.mp4".into(),
        ];
        for line in main.lines().chain(low.lines()) {
            if let Some(name) = line.strip_prefix("../../") {
                files.push(name.to_owned());
            }
        }
        for name in files {
            let path = dir.join(&name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, get(format!("{base}/{name}")).await).unwrap();
        }
    }
    // The low rung is served under the same ownership as the main one: another
    // device of the same profile cannot read it, and it retires with its
    // delivery.
    let low_segment = low
        .lines()
        .find_map(|l| l.strip_prefix("../../"))
        .unwrap()
        .to_owned();
    let low_paths = [
        format!("{base}/variants/low/index.m3u8"),
        format!("{base}/variants/low/init.mp4"),
        format!("{base}/{low_segment}"),
    ];
    let (_, stranger) = f.device(&["default"], PLAYER).await;
    for path in &low_paths {
        let reply = f
            .call("GET", path, None, &[("authorization", &stranger)])
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    let closed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.server.app.processing.execution.snapshot().used != 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("adaptive capacity was not released");
    for path in &low_paths {
        let reply = f.call("GET", path, None, &[("authorization", &auth)]).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    // A client cap keeps one capped variant.
    let mut capped = plan_input("default", &timeline, "convert", None, &["hls"]);
    capped["quality"]["max_bitrate_bps"] = json!("2000000");
    let capped = f.post("/api/v2/playback/plans", &auth, None, capped).await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("adaptive-ladder-0002"),
            json!({"plan_token": capped.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    f.until(&id, &auth, |d| {
        d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 0
    })
    .await;
    let master =
        String::from_utf8(get(format!("/api/v2/streams/{id}/1/master.m3u8")).await).unwrap();
    assert_eq!(master.matches("#EXT-X-STREAM-INF").count(), 1, "{master}");
    assert!(master.contains("BANDWIDTH=2000000,"), "{master}");
    // A ladder plan signed before a restart that disabled ladders cannot
    // start one.
    let ladder = f
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            plan_input("default", &timeline, "convert", None, &["hls"]),
        )
        .await;
    assert_eq!(ladder.status, StatusCode::OK, "{}", ladder.body);
    f.restart_with(Default::default()).await;
    let refused = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("adaptive-ladder-0003"),
            json!({"plan_token": ladder.body["plan_token"], "start_ms": 0}),
        )
        .await;
    problem(
        &refused,
        StatusCode::UNPROCESSABLE_ENTITY,
        "route_unsupported",
    );
}

#[tokio::test]
async fn recovery_keeps_an_adaptive_encoders_two_execution_units() {
    let mut f = Fixture::new().await;
    let root = f.server.app.processing.deliveries.root.clone();
    // Encoders that outlive the server: a single-variant generation and an
    // adaptive one (its low variant directory precedes its witness).
    let single = root.join("single").join("1");
    let ladder = root.join("ladder").join("1");
    std::fs::create_dir_all(&single).unwrap();
    std::fs::create_dir_all(ladder.join("low")).unwrap();
    let (_, single_held) = playscale::execution::Witness::create(&single.join(".owner")).unwrap();
    let (_, ladder_held) = playscale::execution::Witness::create(&ladder.join(".owner")).unwrap();
    f.restart().await;
    assert_eq!(f.server.app.processing.execution.snapshot().used, 3);
    drop(ladder_held);
    tokio::time::timeout(Duration::from_secs(30), async {
        while f.server.app.processing.execution.snapshot().used != 1 || ladder.exists() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the adaptive encoder's units were not released");
    assert!(single.exists());
    drop(single_held);
}

/// The administrator's view of a delivery, once it lists it.
async fn monitored(f: &Fixture, id: &str) -> Value {
    let reply = f
        .get("/api/v2/admin/playback/sessions", &bearer(OPERATOR))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["delivery_id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

/// Wait for a finished history entry for `id`.
async fn finished(f: &Fixture, id: &str) -> Value {
    for _ in 0..300 {
        let reply = f
            .get(
                "/api/v2/admin/playback/history?limit=200",
                &bearer(OPERATOR),
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
        if let Some(entry) = reply.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["delivery_id"] == id)
        {
            return entry.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("delivery {id} never reached playback history");
}

#[tokio::test(flavor = "multi_thread")]
async fn administrators_see_who_is_watching_exact_bytes_and_finished_history() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let mut f = Fixture::new().await;
    two_audio_mp4(&f, "20");
    f.scan().await;
    let (timeline, item) = f.timeline().await;
    let (device, auth) = f.device(&["default"], PLAYER).await;

    // Administrators only, on both endpoints.
    for path in [
        "/api/v2/admin/playback/sessions",
        "/api/v2/admin/playback/history",
    ] {
        problem(
            &f.get(path, &auth).await,
            StatusCode::FORBIDDEN,
            "permission_denied",
        );
        let anonymous = f.call("GET", path, None, &[]).await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    }
    let capabilities = f.get("/api/v2/system/capabilities", &auth).await;
    let feature = capabilities.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["id"] == "admin.playback_sessions")
        .cloned()
        .unwrap();
    assert_eq!(feature["enabled"], true);
    assert_eq!(feature["limits"]["history_retention_days"], "90");

    let d = admit_original(&f, &auth, &timeline, "monitor-delivery-0001", 0).await;
    let id = d["id"].as_str().unwrap().to_owned();
    let listed = monitored(&f, &id).await;
    assert_eq!(listed["device"]["id"], device);
    assert_eq!(listed["device"]["name"], "Living room");
    assert_eq!(listed["profile"]["id"], "default");
    assert_eq!(listed["item"]["id"], item);
    assert_eq!(listed["timeline_id"], timeline);
    assert_eq!(listed["mode"], "direct");
    // In-process requests carry no peer address.
    assert_eq!(listed["client_network"], "unknown");
    assert_eq!(listed["bytes_sent"], "0");

    // Bytes through the delivery's own content route (a range) and through the
    // file URL direct play uses, attributed to this principal's delivery.
    let generation = d["active"]["generation"].as_str().unwrap().to_owned();
    let ranged = f
        .call(
            "GET",
            &format!("/api/v2/streams/{id}/{generation}/content"),
            None,
            &[("authorization", &auth), ("range", "bytes=0-4095")],
        )
        .await;
    assert_eq!(ranged.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(ranged.bytes.len(), 4096);
    let media = d["active"]["media_url"].as_str().unwrap().to_owned();
    let whole = f
        .call("GET", &media, None, &[("authorization", &auth)])
        .await;
    assert_eq!(whole.status, StatusCode::OK);
    let expected = 4096 + whole.bytes.len() as u64;
    // Another principal's reads of the same file are not this delivery's.
    let (_, other) = f.device(&["default"], PLAYER).await;
    let foreign = f
        .call("GET", &media, None, &[("authorization", &other)])
        .await;
    assert_eq!(foreign.status, StatusCode::OK);
    // Direct-play URLs name their delivery, so a second delivery of the same
    // file on the same device is charged only for its own reads, and a read
    // that names none is not guessed between them.
    assert!(media.contains(&format!("delivery={id}")), "{media}");
    let twin = admit_original(&f, &auth, &timeline, "monitor-delivery-0003", 0).await;
    let twin_id = twin["id"].as_str().unwrap().to_owned();
    let twin_media = twin["active"]["media_url"].as_str().unwrap().to_owned();
    let read = f
        .call(
            "GET",
            &twin_media,
            None,
            &[("authorization", &auth), ("range", "bytes=0-999")],
        )
        .await;
    assert_eq!(read.status, StatusCode::PARTIAL_CONTENT);
    let unnamed = media.split("&delivery=").next().unwrap().to_owned();
    let ambiguous = f
        .call("GET", &unnamed, None, &[("authorization", &auth)])
        .await;
    assert_eq!(ambiguous.status, StatusCode::OK);
    assert_eq!(monitored(&f, &twin_id).await["bytes_sent"], "1000");
    // A response still being sent when its delivery closes: the session shows
    // as closing (and administrators are told) until the response is done,
    // and its bytes reach history.
    let twin_generation = twin["active"]["generation"].as_str().unwrap().to_owned();
    let held = api::router(f.server.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v2/streams/{twin_id}/{twin_generation}/content"
                ))
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(held.status(), StatusCode::OK);
    let hints = |f: &Fixture, id: String| {
        let db = f.server.app.db.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM change_events WHERE topic='playback_sessions' AND resource_id=? AND kind='changed'",
            )
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let before = hints(&f, twin_id.clone()).await;
    let closed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{twin_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    let mut announced = false;
    for _ in 0..100 {
        if monitored(&f, &twin_id).await["status"] == "closing"
            && hints(&f, twin_id.clone()).await > before
        {
            announced = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        announced,
        "a draining session is shown and announced as closing"
    );
    let twin_rest = held.into_body().collect().await.unwrap().to_bytes().len();
    let entry = finished(&f, &twin_id).await;
    assert_eq!(entry["bytes_sent"], (1000 + twin_rest).to_string());

    // A playhead from the player.
    let beat = f
        .post(
            &format!("/api/v2/playback/delivery-sessions/{id}/heartbeat"),
            &auth,
            None,
            json!({"active_generation": generation, "playhead": {"sequence": "1", "position_ms": 1500}}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{:?}", beat.body);
    // A conflicting report (same sequence, another position) is refused by
    // the delivery and changes nothing the administrator sees.
    let conflict = f
        .post(
            &format!("/api/v2/playback/delivery-sessions/{id}/heartbeat"),
            &auth,
            None,
            json!({"active_generation": generation, "playhead": {"sequence": "1", "position_ms": 9000}}),
        )
        .await;
    assert_ne!(conflict.status, StatusCode::OK);
    let listed = monitored(&f, &id).await;
    assert_eq!(listed["bytes_sent"], expected.to_string());
    assert_eq!(listed["position_ms"], 1500);
    assert!(["playing", "paused", "starting"].contains(&listed["status"].as_str().unwrap()));

    // Change hints exist for the session (administrative topic).
    let hints: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM change_events WHERE topic='playback_sessions' AND resource_id=?",
    )
    .bind(&id)
    .fetch_one(&f.server.app.db)
    .await
    .unwrap();
    assert!(hints >= 1);

    // Closing ends the session; its history keeps the final counters.
    let closed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT, "{:?}", closed.body);
    let entry = finished(&f, &id).await;
    assert_eq!(entry["end_reason"], "closed");
    assert_eq!(entry["bytes_sent"], expected.to_string());
    assert_eq!(entry["device"]["name"], "Living room");
    assert_eq!(entry["item"]["id"], item);
    assert_eq!(entry["start_position_ms"], 0);
    assert_eq!(entry["end_position_ms"], 1500);
    assert!(monitored(&f, &id).await.is_null());

    // Filters select by device, profile and item; bad input is refused.
    let mine = f
        .get(
            &format!("/api/v2/admin/playback/history?device_id={device}&item_id={item}"),
            &bearer(OPERATOR),
        )
        .await;
    // This delivery and its twin, both closed.
    assert_eq!(mine.body["items"].as_array().unwrap().len(), 2);
    let none = f
        .get(
            "/api/v2/admin/playback/history?profile_id=kids",
            &bearer(OPERATOR),
        )
        .await;
    assert!(none.body["items"].as_array().unwrap().is_empty());
    for bad in ["?since=yesterday", "?limit=0", "?cursor=x", "?unknown=1"] {
        let reply = f
            .get(
                &format!("/api/v2/admin/playback/history{bad}"),
                &bearer(OPERATOR),
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{bad}: {:?}",
            reply.body
        );
    }

    // A delivery live when the process stops ends as interrupted, with the
    // counters last written (every few seconds while live).
    let d = admit_original(&f, &auth, &timeline, "monitor-delivery-0002", 0).await;
    let second = d["id"].as_str().unwrap().to_owned();
    let generation = d["active"]["generation"].as_str().unwrap().to_owned();
    let read = f
        .call(
            "GET",
            &format!("/api/v2/streams/{second}/{generation}/content"),
            None,
            &[("authorization", &auth), ("range", "bytes=0-9999")],
        )
        .await;
    assert_eq!(read.bytes.len(), 10_000);
    for _ in 0..80 {
        let written: i64 = sqlx::query_scalar(
            "SELECT bytes_sent FROM playback_monitor_sessions WHERE delivery_id=?",
        )
        .bind(&second)
        .fetch_one(&f.server.app.db)
        .await
        .unwrap();
        if written == 10_000 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // A response still being sent when the server stops (graceful HTTP
    // shutdown keeps sending it) is waited for: its bytes reach history.
    let held = api::router(f.server.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(format!("/api/v2/streams/{second}/{generation}/content"))
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(held.status(), StatusCode::OK);
    // A clean stop finishes live sessions as server_stopped.
    f.server.stop.cancel();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let draining: Option<i64> =
        sqlx::query_scalar("SELECT ended_at FROM playback_monitor_sessions WHERE delivery_id=?")
            .bind(&second)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    assert_eq!(draining, None, "history waits for the open response");
    let rest = held.into_body().collect().await.unwrap().to_bytes().len() as i64;
    assert!(rest > 0);
    let mut reason: Option<String> = None;
    for _ in 0..100 {
        reason = sqlx::query_scalar(
            "SELECT end_reason FROM playback_monitor_sessions WHERE delivery_id=? AND ended_at IS NOT NULL",
        )
        .bind(&second)
        .fetch_optional(&f.server.app.db)
        .await
        .unwrap()
        .flatten();
        if reason.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(reason.as_deref(), Some("server_stopped"));
    let sent: i64 =
        sqlx::query_scalar("SELECT bytes_sent FROM playback_monitor_sessions WHERE delivery_id=?")
            .bind(&second)
            .fetch_one(&f.server.app.db)
            .await
            .unwrap();
    assert_eq!(sent, 10_000 + rest, "the drained response is counted");
    // An abrupt stop never finishes the row: startup recovery does, keeping a
    // reason recorded with an ending decision (the twin was closed before the
    // crash) and otherwise as interrupted.
    sqlx::query(
        "UPDATE playback_monitor_sessions SET ended_at=NULL,end_reason=NULL WHERE delivery_id=?",
    )
    .bind(&second)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE playback_monitor_sessions SET ended_at=NULL,end_reason='closed' WHERE delivery_id=?",
    )
    .bind(&twin_id)
    .execute(&f.server.app.db)
    .await
    .unwrap();
    f.restart().await;
    let entry = finished(&f, &second).await;
    assert_eq!(entry["end_reason"], "interrupted");
    assert_eq!(entry["bytes_sent"], (10_000 + rest).to_string());
    assert_eq!(finished(&f, &twin_id).await["end_reason"], "closed");
}

/// A uniform 10-bit limited-range picture (luma `code`, neutral chroma) with
/// stereo silence, tagged with `params` (`setparams` options), as lossless
/// FFV1 in Matroska. Written by the host ffmpeg; the tags are what the scanner
/// probes.
fn uniform_hdr_mkv(f: &Fixture, name: &str, code: u16, params: &str) {
    let (width, height, frames) = (320usize, 180usize, 100usize);
    let mut frame = Vec::with_capacity(width * height * 3);
    for _ in 0..width * height {
        frame.extend_from_slice(&code.to_le_bytes());
    }
    for _ in 0..width * height / 2 {
        frame.extend_from_slice(&512u16.to_le_bytes());
    }
    let mut child = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p10le",
        ])
        .args(["-s", "320x180", "-r", "25", "-i", "-"])
        .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", "4"])
        .args(["-vf", &format!("setparams={params}")])
        .args(["-c:v", "ffv1", "-c:a", "aac", "-shortest"])
        .arg(f.dir.path().join("media").join(name))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut input = child.stdin.take().unwrap();
        for _ in 0..frames {
            input.write_all(&frame).unwrap();
        }
    }
    assert!(child.wait().unwrap().success());
}

/// Mean limited-range luma code of the 320x180 frame at `at` seconds: the
/// Y plane of 8-bit yuv420p as coded (converting to `gray` would expand it
/// to full range).
fn mean_luma(path: &Path, at: &str) -> f64 {
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", at, "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .output()
        .unwrap()
        .stdout;
    let luma = &frame[..320 * 180];
    assert_eq!(frame.len(), 320 * 180 * 3 / 2, "{at}");
    luma.iter().map(|v| f64::from(*v)).sum::<f64>() / luma.len() as f64
}

async fn hdr_plan(f: &Fixture, auth: &str, timeline: &str) -> Reply {
    let input = plan_input("default", timeline, "auto", None, &["http_range", "hls"]);
    f.post("/api/v2/playback/plans", auth, None, input).await
}

/// 10-bit limited-range PQ code value of a 100-nit neutral patch (ST 2084).
const PQ_100_NITS: u16 = 509;

/// HDR10 sources play to SDR clients through a tone-mapped conversion whose
/// pixels (not only tags) are SDR, decided from probed colour facts and the
/// observed filters. Unknown facts and missing filters are refused. Needs an
/// FFmpeg with zimg and tonemap: `MOTION_FILTER_FFMPEG` (for example the
/// N2 media-tools build); the refusals run without it.
#[tokio::test(flavor = "multi_thread")]
async fn hdr_sources_are_tone_mapped_to_sdr_only_with_known_facts_and_filters() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let tagged = "color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc:range=tv";

    // Without the filters (an FFmpeg that cannot even list them): refused,
    // and capabilities do not advertise tone mapping.
    let none = Fixture::with_settings(playscale::processing::Settings {
        ffmpeg: "/usr/bin/false".into(),
        ..Default::default()
    })
    .await;
    uniform_hdr_mkv(&none, "hdr10.mkv", PQ_100_NITS, tagged);
    none.scan().await;
    let (timeline, _) = none.timeline().await;
    let (_, auth) = none.device(&["default"], PLAYER).await;
    let refused = hdr_plan(&none, &auth, &timeline).await;
    assert_eq!(refused.body["status"], "blocked", "{}", refused.body);
    let capabilities = none
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert!(
        !capabilities.body.to_string().contains("tone_map_to_sdr"),
        "{}",
        capabilities.body
    );

    let Ok(ffmpeg) = std::env::var("MOTION_FILTER_FFMPEG") else {
        eprintln!("SKIPPED (tone mapping): MOTION_FILTER_FFMPEG is not set");
        return;
    };
    let f = Fixture::with_settings(playscale::processing::Settings {
        ffmpeg: ffmpeg.into(),
        ..Default::default()
    })
    .await;
    uniform_hdr_mkv(&f, "hdr10.mkv", PQ_100_NITS, tagged);
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    // A row probed before colour facts were recorded is probed again by the
    // next ordinary scan instead of being refused forever.
    sqlx::query("UPDATE media_files SET tracks_json=json_remove(tracks_json,'$[0].probe_version','$[0].color_primaries','$[0].color_space','$[0].color_range')")
        .execute(&f.server.app.db)
        .await
        .unwrap();
    assert_eq!(
        hdr_plan(&f, &auth, &timeline).await.body["status"],
        "blocked"
    );
    f.scan().await;
    let tracks: String = sqlx::query_scalar("SELECT tracks_json FROM catalog_files")
        .fetch_one(&f.server.app.db)
        .await
        .unwrap();
    assert!(
        tracks.contains("\"color_primaries\":\"bt2020\""),
        "{tracks}"
    );

    let planned = hdr_plan(&f, &auth, &timeline).await;
    assert_eq!(
        planned.body["operation"], "video_transcode",
        "{}",
        planned.body
    );
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("hdr-tone-map-0000001"),
            json!({"plan_token": planned.body["plan_token"], "start_ms": 0}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 0
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(ready["active"]["status"], "failed", "{ready}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let fragment = f.dir.path().join("tone-mapped.mp4");
    std::fs::write(
        &fragment,
        [
            get(format!("/api/v2/streams/{id}/1/variants/main/init.mp4")).await,
            get(format!("/api/v2/streams/{id}/1/segments/0.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    // Hable for a 1,000-nit peak maps 100-nit diffuse white to about 151 in
    // 8-bit limited BT.709 (pixel-verified in scripts/test_media_transforms.py);
    // retagging the PQ code as BT.709 would give about 127.
    let luma = mean_luma(&fragment, "0.5");
    assert!((147.0..=155.0).contains(&luma), "tone-mapped luma {luma}");
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .args([
            "stream=color_primaries,color_transfer,color_space,color_range",
            "-of",
            "json",
        ])
        .arg(&fragment)
        .output()
        .unwrap()
        .stdout;
    let tags: Value = serde_json::from_slice(&probe).unwrap();
    assert_eq!(
        tags["streams"][0],
        json!({"color_primaries":"bt709","color_transfer":"bt709","color_space":"bt709","color_range":"tv"})
    );
    let capabilities = f
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert!(capabilities.body.to_string().contains("tone_map_to_sdr"));

    // Unknown primaries and matrix: refused, never guessed.
    let g = Fixture::with_settings(f.server.app.processing.settings.clone()).await;
    uniform_hdr_mkv(
        &g,
        "untagged.mkv",
        PQ_100_NITS,
        "color_trc=smpte2084:range=tv",
    );
    g.scan().await;
    let (timeline, _) = g.timeline().await;
    let (_, auth) = g.device(&["default"], PLAYER).await;
    let unknown = hdr_plan(&g, &auth, &timeline).await;
    assert_eq!(unknown.body["status"], "blocked", "{}", unknown.body);
}

/// An ASS script whose one event draws a filled white rectangle (a drawing,
/// so no font is needed) from `start` to `end` on a 320x240 canvas.
fn ass_box(rectangle: (u32, u32, u32, u32), start: &str, end: &str) -> String {
    let (x0, y0, x1, y1) = rectangle;
    format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 320\nPlayResY: 240\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,Arial,20,&H00FFFFFF,&H00FFFFFF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,0,0,7,0,0,0,1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: 0,{start},{end},Default,,0,0,0,,{{\\an7\\pos(0,0)\\p1}}m {x0} {y0} l {x1} {y0} {x1} {y1} {x0} {y1}{{\\p0}}\n"
    )
}

/// Styled (ASS) subtitles are burned into conversions by libass from the
/// exact selected stream, at the right file time after a seek, with the
/// file's attachments extracted; never flattened to WebVTT, and refused
/// where the FFmpeg lacks libass. Needs `MOTION_FILTER_FFMPEG` (an FFmpeg with
/// libass); the refusals run without it.
#[tokio::test(flavor = "multi_thread")]
async fn styled_subtitles_are_burned_by_libass_from_the_exact_stream() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let build = |f: &Fixture| {
        two_audio_mp4(f, "20");
        let base = f.dir.path().join("base.mp4");
        std::fs::rename(f.dir.path().join("media/film.mp4"), &base).unwrap();
        // Two English tracks: the selected one draws high, the other low.
        let high = f.dir.path().join("high.ass");
        let low = f.dir.path().join("low.ass");
        std::fs::write(
            &high,
            ass_box((40, 40, 260, 100), "0:00:02.00", "0:00:15.00"),
        )
        .unwrap();
        std::fs::write(
            &low,
            ass_box((40, 140, 260, 200), "0:00:00.00", "0:00:20.00"),
        )
        .unwrap();
        let font = f.dir.path().join("font.ttf");
        std::fs::write(&font, b"not really a font").unwrap();
        f.media(
            "film.mkv",
            &[
                "-i",
                base.to_str().unwrap(),
                "-i",
                high.to_str().unwrap(),
                "-i",
                low.to_str().unwrap(),
                "-attach",
                font.to_str().unwrap(),
                "-metadata:s:t",
                "mimetype=font/ttf",
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
                "-metadata:s:s:0",
                "language=eng",
                "-metadata:s:s:1",
                "language=eng",
                "-c",
                "copy",
            ],
        );
    };
    let input = |timeline: &str, source: &Value, modes: &[&str]| {
        let mut input = plan_input("default", timeline, "auto", None, &["http_range", "hls"]);
        input["source"] = source.clone();
        input["tracks"]["subtitle_track_id"] = json!("s0");
        input["tracks"]["subtitle_policy"] = json!("require");
        input["client"]["subtitle_modes"] = json!(modes);
        input
    };

    // Without libass: styled subtitles are refused, never sent as WebVTT.
    let plain = Fixture::with_settings(playscale::processing::Settings {
        ffmpeg: "/usr/bin/false".into(),
        ..Default::default()
    })
    .await;
    build(&plain);
    plain.scan().await;
    // An attachment recorded before its MIME type and size were probed is
    // probed again by the next ordinary scan (fonts are bounded by them).
    let attachment = |f: &Fixture| {
        let db = f.server.app.db.clone();
        async move {
            let tracks: String = sqlx::query_scalar("SELECT tracks_json FROM media_files")
                .fetch_one(&db)
                .await
                .unwrap();
            let tracks: Value = serde_json::from_str(&tracks).unwrap();
            tracks
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["kind"] == "attachment")
                .cloned()
                .unwrap()
        }
    };
    let probed = attachment(&plain).await;
    assert_eq!(
        (
            probed["mimetype"].as_str(),
            probed["attachment_bytes"].as_u64()
        ),
        (Some("font/ttf"), Some(17))
    );
    // As written by an intermediate probe: colour facts recorded (its old
    // `color_probed` flag set) but no attachment facts and no probe version.
    sqlx::query("UPDATE media_files SET tracks_json=(SELECT json_group_array(json_set(json_remove(value,'$.probe_version','$.mimetype','$.attachment_bytes'),'$.color_probed',json('true'))) FROM json_each(tracks_json))")
        .execute(&plain.server.app.db)
        .await
        .unwrap();
    assert!(attachment(&plain).await["mimetype"].is_null());
    plain.scan().await;
    assert_eq!(
        attachment(&plain).await["attachment_bytes"].as_u64(),
        Some(17)
    );
    let (timeline, _) = plain.timeline().await;
    let (_, auth) = plain.device(&["default"], PLAYER).await;
    let source = plain.source().await;
    let refused = plain
        .post(
            "/api/v2/playback/plans",
            &auth,
            None,
            input(&timeline, &source, &["text", "burned"]),
        )
        .await;
    assert_eq!(
        refused.body["reason_codes"][0], "subtitle_format_unsupported",
        "{}",
        refused.body
    );

    let Ok(ffmpeg) = std::env::var("MOTION_FILTER_FFMPEG") else {
        eprintln!("SKIPPED (libass): MOTION_FILTER_FFMPEG is not set");
        return;
    };
    let f = Fixture::with_settings(playscale::processing::Settings {
        ffmpeg: ffmpeg.into(),
        ..Default::default()
    })
    .await;
    build(&f);
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let source = f.source().await;
    let plan = |modes: &'static [&'static str]| {
        let body = input(&timeline, &source, modes);
        let f = &f;
        let auth = auth.clone();
        async move { f.post("/api/v2/playback/plans", &auth, None, body).await }
    };
    // A text-only client cannot be given styled subtitles.
    assert_eq!(
        plan(&["text"]).await.body["reason_codes"][0],
        "subtitle_format_unsupported"
    );
    // The fragment's mean luma inside the selected track's box and the other
    // track's box, `at` seconds into the first segment.
    let boxes = |id: String, at: &'static str| {
        let f = &f;
        let auth = auth.clone();
        async move {
            let mut bytes = Vec::new();
            for path in ["variants/main/init.mp4", "segments/0.m4s"] {
                let reply = f
                    .call(
                        "GET",
                        &format!("/api/v2/streams/{id}/1/{path}"),
                        None,
                        &[("authorization", &auth)],
                    )
                    .await;
                assert_eq!(reply.status, StatusCode::OK, "{path}");
                bytes.extend(reply.bytes);
            }
            let fragment = f.dir.path().join(format!("styled-{id}-{at}.mp4"));
            std::fs::write(&fragment, bytes).unwrap();
            let frame = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&fragment)
                .args([
                    "-ss",
                    at,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gray",
                    "-",
                ])
                .output()
                .unwrap()
                .stdout;
            assert_eq!(frame.len(), 320 * 240);
            let mean = |y0: usize| {
                let mut sum = 0usize;
                for y in y0 + 10..y0 + 50 {
                    sum += frame[y * 320 + 60..y * 320 + 240]
                        .iter()
                        .map(|&p| p as usize)
                        .sum::<usize>();
                }
                sum / (180 * 40)
            };
            (mean(40), mean(140))
        }
    };
    for (key, start_ms, before, during) in [
        // From the start: nothing before 2 s, the box from then on.
        ("styled-burn-0000001", 0, Some("1"), "3"),
        // A seek into the event: drawn although its start precedes the seek.
        ("styled-burn-0000002", 8000, None, "0.5"),
    ] {
        let planned = plan(&["text", "burned"]).await;
        assert_eq!(
            planned.body["operation"], "video_transcode",
            "{}",
            planned.body
        );
        let admitted = f
            .post(
                "/api/v2/playback/delivery-sessions",
                &auth,
                Some(key),
                json!({"plan_token": planned.body["plan_token"], "start_ms": start_ms}),
            )
            .await;
        assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
        let id = admitted.body["id"].as_str().unwrap().to_owned();
        let ready = f
            .until(&id, &auth, |d| {
                d["active"]["available_end_ms"].as_u64().unwrap_or(0) > start_ms
                    || d["active"]["status"] == "failed"
            })
            .await;
        assert_ne!(ready["active"]["status"], "failed", "{ready}");
        assert_eq!(
            ready["active"]["subtitles_url"],
            Value::Null,
            "burned, not a track"
        );
        if let Some(at) = before {
            let (selected, other) = boxes(id.clone(), at).await;
            assert!(
                selected < 200 && other < 200,
                "before the cue: {selected}, {other}"
            );
        }
        let (selected, other) = boxes(id.clone(), during).await;
        // Only the selected stream is drawn, not its same-language sibling.
        assert!(
            selected > 225 && other < 200,
            "cue {selected}, other track {other}"
        );
        // The file's attachment was extracted under a fixed name.
        let fonts = f
            .server
            .app
            .processing
            .deliveries
            .root
            .join(&id)
            .join("burn-fonts-0/font0");
        assert_eq!(std::fs::read(&fonts).unwrap(), b"not really a font");
        assert_eq!(
            f.call(
                "DELETE",
                &format!("/api/v2/playback/delivery-sessions/{id}"),
                None,
                &[("authorization", &auth)]
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
    }
}

/// Chapters a file's container records become embedded chapter markers of
/// its timeline (6B): derived at read time at the timeline's current
/// revision, outranked by a manual chapter, and probed alone (without
/// hashing the file again) for files scanned before chapters were recorded.
#[tokio::test(flavor = "multi_thread")]
async fn container_chapters_are_embedded_markers_of_the_timeline() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    let chapters = f.dir.path().join("chapters.txt");
    std::fs::write(
        &chapters,
        ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=2000\ntitle=Opening\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=2000\nEND=6000\ntitle=Middle\n",
    )
    .unwrap();
    f.media(
        "film.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
            "-i",
            chapters.to_str().unwrap(),
            "-map",
            "0",
            "-map_metadata",
            "1",
            "-map_chapters",
            "1",
            "-t",
            "6",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let path = format!("/api/v2/catalog/timelines/{timeline}/markers");
    let read = |query: &'static str| {
        let f = &f;
        let auth = auth.clone();
        let path = path.clone();
        async move { f.get(&format!("{path}{query}"), &auth).await.body }
    };
    let labels = |page: &Value| -> Vec<(String, String)> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["label"].as_str().unwrap_or("").to_owned(),
                    m["provenance"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let embedded = vec![
        ("Opening".to_owned(), "embedded".to_owned()),
        ("Middle".to_owned(), "embedded".to_owned()),
    ];
    assert_eq!(labels(&read("").await), embedded);
    assert_eq!(labels(&read("?effective=true").await), embedded);
    // A timeline edit keeps them in effect (file facts follow the timeline).
    sqlx::query("UPDATE timelines SET revision=revision+1 WHERE id=?")
        .bind(&timeline)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    assert_eq!(labels(&read("?effective=true").await), embedded);
    // A manual chapter outranks the file's chapters; they stay listed.
    let revision = f
        .get(&format!("/api/v2/catalog/timelines/{timeline}"), &auth)
        .await
        .body["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let created = f
        .post(
            &path,
            &bearer(OPERATOR),
            Some("manual-chapter-0001"),
            json!({"timeline_revision": revision, "kind": "chapter", "start_ms": 1000, "end_ms": null, "label": "Mine"}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        labels(&read("?effective=true").await),
        [("Mine".to_owned(), "manual".to_owned())]
    );
    assert_eq!(read("").await["items"].as_array().unwrap().len(), 3);
    // A file scanned before chapters were recorded is probed for them alone.
    sqlx::query("UPDATE media_files SET chapters_json=NULL")
        .execute(&f.server.app.db)
        .await
        .unwrap();
    f.scan().await;
    let stored: Option<String> = sqlx::query_scalar("SELECT chapters_json FROM media_files")
        .fetch_one(&f.server.app.db)
        .await
        .unwrap();
    assert!(
        stored.as_deref().is_some_and(|c| c.contains("Opening")),
        "{stored:?}"
    );
    // A scan-created timeline records no duration; the file's own bounds
    // the chapters, so one past the media's end is dropped.
    let db = &f.server.app.db;
    sqlx::query("UPDATE timelines SET duration_ms=NULL WHERE id=?")
        .bind(&timeline)
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE media_files SET chapters_json='[{"start_ms":0,"end_ms":2000,"title":"Opening"},{"start_ms":9000,"end_ms":null,"title":"Past the end"}]'"#,
    )
    .execute(db)
    .await
    .unwrap();
    assert_eq!(
        labels(&read("").await)
            .into_iter()
            .map(|(l, _)| l)
            .collect::<Vec<_>>(),
        ["Opening", "Mine"]
    );
    // Replaced content whose chapters cannot be probed keeps none of the
    // old ones: they belonged to the previous revision.
    let before: String = sqlx::query_scalar("SELECT revision FROM media_files")
        .fetch_one(db)
        .await
        .unwrap();
    std::fs::write(
        f.dir.path().join("media").join("film.mp4"),
        b"not a media file any more",
    )
    .unwrap();
    f.scan().await;
    let (after, stored): (String, Option<String>) =
        sqlx::query_as("SELECT revision,chapters_json FROM media_files")
            .fetch_one(db)
            .await
            .unwrap();
    assert_ne!(after, before);
    assert_eq!(stored, None, "stale chapters on a new revision");
}

/// Embedded chapters come only from an original the caller may read: a
/// chaptered copy of the same timeline in another library discloses
/// nothing to a caller scoped to the first.
#[tokio::test(flavor = "multi_thread")]
async fn embedded_chapters_come_only_from_readable_originals() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "6");
    f.scan().await;
    let other_root = f.dir.path().join("other");
    std::fs::create_dir(&other_root).unwrap();
    let chapters = f.dir.path().join("chapters.txt");
    std::fs::write(
        &chapters,
        ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=3000\ntitle=Hidden\n",
    )
    .unwrap();
    let status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=24",
        ])
        .arg("-i")
        .arg(&chapters)
        .args([
            "-map",
            "0",
            "-map_chapters",
            "1",
            "-t",
            "6",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ])
        .arg(other_root.join("cut.mp4"))
        .status()
        .unwrap();
    assert!(status.success());
    let other = db::add_library(&f.server.app.db, "Other", &other_root)
        .await
        .unwrap()
        .id;
    f.scan_library(&other).await;
    let db = &f.server.app.db;
    let (timeline, edition): (String, String) = sqlx::query_as(
        "SELECT t.id,t.edition_id FROM timelines t JOIN media_files f ON f.edition_id=t.edition_id WHERE f.relative_path='film.mp4'",
    )
    .fetch_one(db)
    .await
    .unwrap();
    let (file, version): (String, String) = sqlx::query_as(
        "SELECT f.id,b.version_id FROM media_files f JOIN version_files b ON b.file_id=f.id WHERE f.relative_path='cut.mp4'",
    )
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query("UPDATE media_files SET edition_id=? WHERE id=?")
        .bind(&edition)
        .bind(&file)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_versions SET timeline_id=? WHERE id=?")
        .bind(&timeline)
        .bind(&version)
        .execute(db)
        .await
        .unwrap();
    for (library, expected) in [(f.library.clone(), 0), (other.clone(), 1)] {
        let (device, auth) = f.device(&["default"], PLAYER).await;
        f.policy(&device, &[library.as_str()], PLAYER, "\"r-2\"")
            .await;
        let page = f
            .get(
                &format!("/api/v2/catalog/timelines/{timeline}/markers"),
                &auth,
            )
            .await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        let embedded: Vec<_> = page.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["provenance"] == "embedded")
            .collect();
        assert_eq!(embedded.len(), expected, "{library}: {}", page.body);
    }
    // A hundred chaptered copies the caller cannot read, ordered before the
    // readable one, never hide its chapters.
    let film: String =
        sqlx::query_scalar("SELECT id FROM media_files WHERE relative_path='film.mp4'")
            .fetch_one(db)
            .await
            .unwrap();
    let film_version: String =
        sqlx::query_scalar("SELECT version_id FROM version_files WHERE file_id=?")
            .bind(&film)
            .fetch_one(db)
            .await
            .unwrap();
    let mut tx = db.begin().await.unwrap();
    for n in 0..100 {
        for sql in [
            "CREATE TEMP TABLE copy_file AS SELECT * FROM media_files WHERE id=?1",
            "UPDATE copy_file SET id='copy-file-'||?2,relative_path='copy-'||?2||'.mp4',chapters_json='[{\"start_ms\":0,\"end_ms\":null,\"title\":\"Copy\"}]'",
            "INSERT INTO media_files SELECT * FROM copy_file",
            "CREATE TEMP TABLE copy_version AS SELECT * FROM media_versions WHERE id=?3",
            "UPDATE copy_version SET id='0000-copy-'||?2",
            "INSERT INTO media_versions SELECT * FROM copy_version",
            "CREATE TEMP TABLE copy_binding AS SELECT * FROM version_files WHERE version_id=?3",
            "UPDATE copy_binding SET version_id='0000-copy-'||?2,file_id='copy-file-'||?2",
            "INSERT INTO version_files SELECT * FROM copy_binding",
            "DROP TABLE copy_file",
            "DROP TABLE copy_version",
            "DROP TABLE copy_binding",
        ] {
            sqlx::query(sql)
                .bind(&film)
                .bind(format!("{n:03}"))
                .bind(&film_version)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
    }
    tx.commit().await.unwrap();
    let (device, auth) = f.device(&["default"], PLAYER).await;
    f.policy(&device, &[other.as_str()], PLAYER, "\"r-2\"")
        .await;
    let page = f
        .get(
            &format!("/api/v2/catalog/timelines/{timeline}/markers"),
            &auth,
        )
        .await;
    let labels: Vec<_> = page.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["provenance"] == "embedded")
        .map(|m| m["label"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(labels, ["Hidden"], "{}", page.body);
    // The first readable original has no chapters: later chaptered copies
    // must not contribute marker positions from a different cut.
    sqlx::query("UPDATE media_files SET chapters_json=NULL WHERE id='copy-file-000'")
        .execute(db)
        .await
        .unwrap();
    let (device, auth) = f.device(&["default"], PLAYER).await;
    f.policy(&device, &[f.library.as_str()], PLAYER, "\"r-2\"")
        .await;
    let page = f
        .get(
            &format!("/api/v2/catalog/timelines/{timeline}/markers"),
            &auth,
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body["items"].as_array().unwrap().is_empty(),
        "{}",
        page.body
    );
}

/// A burned bitmap cue whose display set began long before a start (beyond
/// what reading the video earlier would cover) is still drawn: the subtitle
/// is read from its own descriptor of the source, BURN_LOOKBACK_MS earlier.
#[tokio::test(flavor = "multi_thread")]
async fn bitmap_cues_established_long_before_a_start_are_drawn() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "50");
    let base = f.dir.path().join("base.mp4");
    std::fs::rename(f.dir.path().join("media/film.mp4"), &base).unwrap();
    let sup = f.dir.path().join("box.sup");
    // One cue on a 640x480 canvas over 320x240 video; muxing places its
    // display set at the file's start, and its clearing set 44 s later.
    std::fs::write(&sup, pgs_box(640, 480, (80, 80, 480, 120), 1000, 45000)).unwrap();
    f.media(
        "film.mkv",
        &[
            "-i",
            base.to_str().unwrap(),
            "-i",
            sup.to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-c",
            "copy",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let mut input = plan_input("default", &timeline, "auto", None, &["hls"]);
    input["source"] = f.source().await;
    input["tracks"]["subtitle_track_id"] = json!("s0");
    input["tracks"]["subtitle_policy"] = json!("require");
    input["client"]["subtitle_modes"] = json!(["burned"]);
    let planned = f.post("/api/v2/playback/plans", &auth, None, input).await;
    assert_eq!(
        planned.body["operation"], "video_transcode",
        "{}",
        planned.body
    );
    // Starting at 40 s: the cue's display set is 40 s earlier, beyond any
    // fixed short lookback.
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("burned-lookback-001"),
            json!({"plan_token": planned.body["plan_token"], "start_ms": 40000}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 40000
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(ready["active"]["status"], "failed", "{ready}");
    assert_eq!(ready["active"]["media_time_origin_ms"], 40000, "{ready}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let base_url = format!("/api/v2/streams/{id}/1");
    let fragment = f.dir.path().join("lookback.mp4");
    std::fs::write(
        &fragment,
        [
            get(format!("{base_url}/variants/main/init.mp4")).await,
            get(format!("{base_url}/segments/0.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&fragment)
        .args([
            "-ss",
            "0.5",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(frame.len(), 320 * 240);
    let mean = |x0: usize, y0: usize| {
        let mut sum = 0usize;
        for y in y0..y0 + 40 {
            sum += frame[y * 320 + x0..y * 320 + x0 + 220]
                .iter()
                .map(|&p| p as usize)
                .sum::<usize>();
        }
        sum / (220 * 40)
    };
    let (inside, below) = (mean(50, 50), mean(50, 150));
    assert!(inside > 225 && below < 200, "box {inside}, picture {below}");
    let closed = f
        .call(
            "DELETE",
            &format!("/api/v2/playback/delivery-sessions/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(closed.status, StatusCode::NO_CONTENT);
    // Starting after the cue was cleared: nothing is drawn, and the
    // conversion plays on (no earlier reading, no waiting for cues).
    let planned = {
        let mut input = plan_input("default", &timeline, "auto", None, &["hls"]);
        input["source"] = f.source().await;
        input["tracks"]["subtitle_track_id"] = json!("s0");
        input["tracks"]["subtitle_policy"] = json!("require");
        input["client"]["subtitle_modes"] = json!(["burned"]);
        f.post("/api/v2/playback/plans", &auth, None, input).await
    };
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("burned-lookback-002"),
            json!({"plan_token": planned.body["plan_token"], "start_ms": 46000}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 46000
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(ready["active"]["status"], "failed", "{ready}");
    let base_url = format!("/api/v2/streams/{id}/1");
    std::fs::write(
        &fragment,
        [
            get(format!("{base_url}/variants/main/init.mp4")).await,
            get(format!("{base_url}/segments/0.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&fragment)
        .args([
            "-ss",
            "0.5",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(frame.len(), 320 * 240);
    let mut sum = 0usize;
    for y in 50..90 {
        sum += frame[y * 320 + 50..y * 320 + 270]
            .iter()
            .map(|&p| p as usize)
            .sum::<usize>();
    }
    let cleared = sum / (220 * 40);
    assert!(cleared < 200, "box still drawn: {cleared}");
}

/// A PGS stream whose composition at 30 s reuses the object and palette its
/// epoch defined at 0 s (shown 0-10 s, cleared, shown again from 30 s).
fn pgs_reuse() -> Vec<u8> {
    let (width, height, x, y, w, h) = (640u16, 480u16, 80u16, 80u16, 480u16, 120u16);
    let be = |v: u16| v.to_be_bytes();
    let segment = |pts_ms: u32, kind: u8, payload: &[u8]| {
        let mut out = b"PG".to_vec();
        out.extend((pts_ms * 90).to_be_bytes());
        out.extend(0u32.to_be_bytes());
        out.push(kind);
        out.extend((payload.len() as u16).to_be_bytes());
        out.extend(payload);
        out
    };
    let window = [&[1u8, 0][..], &be(x), &be(y), &be(w), &be(h)].concat();
    let mut rle = Vec::new();
    for _ in 0..h {
        rle.extend([
            0x00,
            0xC0 | (w >> 8) as u8,
            (w & 0xff) as u8,
            0x01,
            0x00,
            0x00,
        ]);
    }
    let mut ods = vec![0, 0, 0, 0xC0];
    ods.extend(&((rle.len() + 4) as u32).to_be_bytes()[1..]);
    ods.extend(be(w));
    ods.extend(be(h));
    ods.extend(&rle);
    let pcs = |number: u16, state: u8, objects: bool| {
        let mut p = [
            &be(width)[..],
            &be(height),
            &[0x10],
            &be(number),
            &[state, 0, 0],
        ]
        .concat();
        if objects {
            p.extend([1, 0, 0, 0, 0]);
            p.extend(be(x));
            p.extend(be(y));
        } else {
            p.push(0);
        }
        p
    };
    [
        segment(0, 0x16, &pcs(0, 0x80, true)),
        segment(0, 0x17, &window),
        segment(0, 0x14, &[0, 0, 1, 235, 128, 128, 255]),
        segment(0, 0x15, &ods),
        segment(0, 0x80, &[]),
        segment(10000, 0x16, &pcs(1, 0x00, false)),
        segment(10000, 0x17, &window),
        segment(10000, 0x80, &[]),
        // A normal composition: no palette or object of its own.
        segment(30000, 0x16, &pcs(2, 0x00, true)),
        segment(30000, 0x17, &window),
        segment(30000, 0x80, &[]),
        segment(45000, 0x16, &pcs(3, 0x00, false)),
        segment(45000, 0x17, &window),
        segment(45000, 0x80, &[]),
    ]
    .concat()
}

/// A PGS cue whose composition reuses objects its epoch defined earlier is
/// read from that epoch, so a start inside it draws it (a decoder started
/// at the composition itself has no palette or object to show).
#[tokio::test(flavor = "multi_thread")]
async fn pgs_compositions_reusing_objects_are_read_from_their_epoch() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "50");
    let base = f.dir.path().join("base.mp4");
    std::fs::rename(f.dir.path().join("media/film.mp4"), &base).unwrap();
    let sup = f.dir.path().join("reuse.sup");
    std::fs::write(&sup, pgs_reuse()).unwrap();
    // A container that starts at 50,000 s (as some recordings do): the
    // lookback works in file time whatever the container's timestamps.
    f.media(
        "film.mkv",
        &[
            "-i",
            base.to_str().unwrap(),
            "-i",
            sup.to_str().unwrap(),
            "-map",
            "0",
            "-map",
            "1",
            "-c",
            "copy",
            "-output_ts_offset",
            "50000",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let mut input = plan_input("default", &timeline, "auto", None, &["hls"]);
    input["source"] = f.source().await;
    input["tracks"]["subtitle_track_id"] = json!("s0");
    input["tracks"]["subtitle_policy"] = json!("require");
    input["client"]["subtitle_modes"] = json!(["burned"]);
    let planned = f.post("/api/v2/playback/plans", &auth, None, input).await;
    let admitted = f
        .post(
            "/api/v2/playback/delivery-sessions",
            &auth,
            Some("burned-epoch-001"),
            json!({"plan_token": planned.body["plan_token"], "start_ms": 38000}),
        )
        .await;
    assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
    let id = admitted.body["id"].as_str().unwrap().to_owned();
    let ready = f
        .until(&id, &auth, |d| {
            d["active"]["available_end_ms"].as_u64().unwrap_or(0) > 38000
                || d["active"]["status"] == "failed"
        })
        .await;
    assert_ne!(ready["active"]["status"], "failed", "{ready}");
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let base_url = format!("/api/v2/streams/{id}/1");
    let fragment = f.dir.path().join("epoch.mp4");
    std::fs::write(
        &fragment,
        [
            get(format!("{base_url}/variants/main/init.mp4")).await,
            get(format!("{base_url}/segments/0.m4s")).await,
        ]
        .concat(),
    )
    .unwrap();
    let frame = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&fragment)
        .args([
            "-ss",
            "0.5",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(frame.len(), 320 * 240);
    let mut sum = 0usize;
    for y in 50..90 {
        sum += frame[y * 320 + 50..y * 320 + 270]
            .iter()
            .map(|&p| p as usize)
            .sum::<usize>();
    }
    let inside = sum / (220 * 40);
    assert!(inside > 225, "reused cue not drawn: {inside}");
}

/// A bitmap subtitle component present in every part is burned into each
/// part's own pictures for a client that accepts burned subtitles, and a
/// start inside a later part still draws that part's cue showing there.
#[tokio::test(flavor = "multi_thread")]
async fn bitmap_subtitles_are_burned_across_parts() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // Two 8 s parts, each with its own English PGS cue over most of it.
    for name in ["a-part1", "b-part2"] {
        let base = f.dir.path().join(format!("{name}.mp4"));
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100",
                "-t",
                "8",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "48",
                "-c:a",
                "aac",
                "-metadata:s:a:0",
                "language=eng",
            ])
            .arg(&base)
            .status()
            .unwrap();
        assert!(status.success());
        let sup = f.dir.path().join(format!("{name}.sup"));
        std::fs::write(&sup, pgs_box(640, 480, (80, 80, 480, 120), 0, 7000)).unwrap();
        // Part 2's audio starts 0.5 s after its pictures.
        let delay = if name == "b-part2" { "0.5" } else { "0" };
        f.media(
            &format!("{name}.mkv"),
            &[
                "-i",
                base.to_str().unwrap(),
                "-itsoffset",
                delay,
                "-i",
                base.to_str().unwrap(),
                "-i",
                sup.to_str().unwrap(),
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2",
                "-c",
                "copy",
                "-metadata:s:a:0",
                "language=eng",
                "-metadata:s:s:0",
                "language=eng",
            ],
        );
    }
    f.scan().await;
    let file = |name: &'static str| {
        let db = f.server.app.db.clone();
        async move {
            sqlx::query_as::<_, (String, String, f64)>(
                "SELECT id,revision,duration_seconds FROM media_files WHERE relative_path=?",
            )
            .bind(name)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let (first, _, first_seconds) = file("a-part1.mkv").await;
    let (second, second_revision, _) = file("b-part2.mkv").await;
    let db = &f.server.app.db;
    let (version, timeline): (String, String) = sqlx::query_as(
        "SELECT v.id,v.timeline_id FROM media_versions v JOIN version_files b ON b.version_id=v.id WHERE b.file_id=?",
    )
    .bind(&first)
    .fetch_one(db)
    .await
    .unwrap();
    sqlx::query("UPDATE media_files SET edition_id=(SELECT edition_id FROM media_files WHERE id=?) WHERE id=?")
        .bind(&first)
        .bind(&second)
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,2,?,?)",
    )
    .bind(&version)
    .bind(&second)
    .bind(&second_revision)
    .execute(db)
    .await
    .unwrap();
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let component = playscale::components::timeline_components(db, &timeline)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.kind == playscale_core::components::Kind::Subtitle)
        .unwrap()
        .id;
    let plan = |modes: Value| {
        let f = &f;
        let auth = auth.clone();
        let timeline = timeline.clone();
        let component = component.clone();
        async move {
            let mut input = plan_input("default", &timeline, "auto", None, &["hls"]);
            input["tracks"]["subtitle_component_id"] = json!(component);
            input["tracks"]["subtitle_policy"] = json!("require");
            input["client"]["subtitle_modes"] = modes;
            f.post("/api/v2/playback/plans", &auth, None, input).await
        }
    };
    // A client that presents only text cannot be given a bitmap.
    assert_eq!(plan(json!(["text"])).await.body["status"], "blocked");
    let burned = plan(json!(["burned"])).await;
    assert_eq!(
        burned.body["operation"], "video_transcode",
        "{}",
        burned.body
    );
    let get = |path: String| {
        let auth = auth.clone();
        let f = &f;
        async move {
            let reply = f
                .call("GET", &path, None, &[("authorization", &auth)])
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{path}");
            reply.bytes
        }
    };
    let offset = (first_seconds * 1000.0) as u64;
    for (n, start) in [(1, 0), (2, offset + 2000), (3, offset + 250)] {
        let planned = plan(json!(["burned"])).await;
        let admitted = f
            .post(
                "/api/v2/playback/delivery-sessions",
                &auth,
                Some(&format!("burned-parts-session-{n}")),
                json!({"plan_token": planned.body["plan_token"], "start_ms": start}),
            )
            .await;
        assert_eq!(admitted.status, StatusCode::CREATED, "{}", admitted.body);
        let id = admitted.body["id"].as_str().unwrap().to_owned();
        let ready = f
            .until(&id, &auth, |d| {
                d["active"]["available_end_ms"].as_u64().unwrap_or(0) > start
                    || d["active"]["status"] == "failed"
            })
            .await;
        assert_ne!(ready["active"]["status"], "failed", "{ready}");
        let base_url = format!("/api/v2/streams/{id}/1");
        let fragment = f.dir.path().join(format!("parts-{n}.mp4"));
        std::fs::write(
            &fragment,
            [
                get(format!("{base_url}/variants/main/init.mp4")).await,
                get(format!("{base_url}/segments/0.m4s")).await,
            ]
            .concat(),
        )
        .unwrap();
        let frame = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&fragment)
            .args([
                "-ss",
                "0.5",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-",
            ])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(frame.len(), 320 * 240);
        let mean = |x0: usize, y0: usize| {
            let mut sum = 0usize;
            for y in y0..y0 + 40 {
                sum += frame[y * 320 + x0..y * 320 + x0 + 220]
                    .iter()
                    .map(|&p| p as usize)
                    .sum::<usize>();
            }
            sum / (220 * 40)
        };
        let (inside, below) = (mean(50, 50), mean(50, 150));
        assert!(
            inside > 225 && below < 200,
            "start {start}: box {inside}, picture {below}"
        );
        // Read early for the cue, part 2's delayed audio keeps its offset:
        // from 0.25 s into the part, sound begins 0.25 s into the output.
        if n == 3 {
            let detect = |range: &str| {
                let out = Command::new("ffmpeg")
                    .args(["-v", "info", "-i"])
                    .arg(&fragment)
                    .args(["-af", &format!("{range},volumedetect"), "-f", "null", "-"])
                    .output()
                    .unwrap();
                let text = String::from_utf8_lossy(&out.stderr).into_owned();
                text.lines()
                    .find_map(|l| l.split("max_volume: ").nth(1))
                    .and_then(|v| v.trim_end_matches(" dB").parse::<f64>().ok())
                    .unwrap()
            };
            assert!(detect("atrim=end=0.2") < -60.0, "audio not delayed");
            assert!(detect("atrim=start=0.4:end=1") > -40.0, "audio missing");
        }
        let closed = f
            .call(
                "DELETE",
                &format!("/api/v2/playback/delivery-sessions/{id}"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(closed.status, StatusCode::NO_CONTENT);
    }
}

/// Trick-play previews (6C): requested once per file revision, rendered by
/// the bounded worker into thumbnail sheets of the promised layout, served
/// only for the current revision to a caller who may read the file, never
/// rendered once cancelled, and retired when the revision changes.
#[tokio::test(flavor = "multi_thread")]
async fn previews_are_rendered_once_served_by_revision_and_retired() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    // 25 s whose brightness is the second it shows (9 levels per second),
    // so each thumbnail's content says which picture it is.
    f.media(
        "film.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=160x120:r=24:d=25,format=gray,geq=lum='trunc(T)*9'",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "24",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let (_, auth) = f.device(&["default"], PLAYER).await;
    let operator = bearer(OPERATOR);
    let path = format!("/api/v2/catalog/timelines/{timeline}/previews");
    let manifest = f.get(&path, &auth).await;
    assert_eq!(manifest.status, StatusCode::OK, "{}", manifest.body);
    assert_eq!(manifest.body["status"], "none");
    // A reader cannot request rendering.
    let refused = f
        .post(&path, &auth, Some("preview-request-0001"), json!(null))
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    // A cancelled queued job is never rendered; a new request queues again.
    let first = f
        .post(&path, &operator, Some("preview-request-0002"), json!(null))
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    let cancelled_id = first.body["job_id"].as_str().unwrap().to_owned();
    let again = f
        .post(&path, &operator, Some("preview-request-0003"), json!(null))
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(
        again.body["job_id"],
        cancelled_id.as_str(),
        "never duplicated"
    );
    let cancel = f
        .post(
            &format!("/api/v2/jobs/{cancelled_id}/cancel"),
            &operator,
            Some("preview-cancel-0001"),
            json!(null),
        )
        .await;
    assert_eq!(cancel.status, StatusCode::OK, "{}", cancel.body);
    assert_eq!(cancel.body["phase"], "cancelled");
    let queued = f
        .post(&path, &operator, Some("preview-request-0004"), json!(null))
        .await;
    assert_eq!(queued.status, StatusCode::ACCEPTED, "{}", queued.body);
    assert_ne!(queued.body["job_id"], cancelled_id.as_str());
    assert_eq!(queued.body["status"], "pending");
    // Retrying the cancelled job alongside the active one is refused.
    let retried = f
        .retry_job(&cancelled_id, &operator, "preview-retry-00001")
        .await;
    assert_eq!(retried.status, StatusCode::CONFLICT, "{}", retried.body);
    assert_eq!(retried.body["code"], "preview_duplicate");
    assert_eq!(f.get(&path, &auth).await.body["status"], "pending");
    let stop = tokio_util::sync::CancellationToken::new();
    let worker = tokio::spawn(playscale::v2::previews::worker(
        f.server.app.clone(),
        stop.clone(),
    ));
    let mut ready = Value::Null;
    for _ in 0..600 {
        ready = f.get(&path, &auth).await.body;
        if ready["status"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(ready["status"], "ready", "{ready}");
    assert_eq!(ready["job_id"], queued.body["job_id"]);
    // 25 s at one thumbnail per 10 s: 3 tiles of 160x120 on one sheet.
    assert_eq!(
        (
            &ready["interval_ms"],
            &ready["tiles"],
            &ready["tile_width"],
            &ready["tile_height"]
        ),
        (&json!(10000), &json!(3), &json!(160), &json!(120))
    );
    let sheets = ready["sheets"].as_array().unwrap();
    assert_eq!(sheets.len(), 1);
    let url = sheets[0].as_str().unwrap().to_owned();
    let sheet = f.call("GET", &url, None, &[("authorization", &auth)]).await;
    assert_eq!(sheet.status, StatusCode::OK);
    assert_eq!(sheet.headers["content-type"], "image/jpeg");
    let image = f.dir.path().join("sheet.jpg");
    std::fs::write(&image, &sheet.bytes).unwrap();
    let size = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&image)
        .output()
        .unwrap()
        .stdout;
    assert_eq!(String::from_utf8_lossy(&size).trim(), "1600,1200");
    // Thumbnail n shows the picture at n * 10 s (the frame at or before it).
    for (n, second) in [(0u32, 0u32), (1, 10), (2, 20)] {
        let tile = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&image)
            .args([
                "-vf",
                &format!("crop=40:40:{}:40", n * 160 + 60),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-",
            ])
            .output()
            .unwrap()
            .stdout;
        let mean = tile.iter().map(|&p| u32::from(p)).sum::<u32>() / tile.len() as u32;
        assert!(
            mean.abs_diff(second * 9) <= 6,
            "thumbnail {n} shows brightness {mean}, not second {second}"
        );
    }
    // The cancelled job rendered nothing, and its record says so.
    let cancelled = f
        .get(&format!("/api/v2/jobs/{cancelled_id}"), &operator)
        .await;
    assert_eq!(cancelled.body["phase"], "cancelled");
    // Only the current revision, an existing sheet, and a reader.
    let other = url.replace("/previews/1?", "/previews/2?");
    assert_eq!(
        f.call("GET", &other, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call("GET", &url, None, &[]).await.status,
        StatusCode::UNAUTHORIZED
    );
    // A device that may not read this library sees neither.
    let (scoped, scoped_auth) = f.device(&["default"], PLAYER).await;
    f.policy(&scoped, &[], PLAYER, "\"r-2\"").await;
    assert_eq!(
        f.call("GET", &url, None, &[("authorization", &scoped_auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.get(&path, &scoped_auth).await.status,
        StatusCode::NOT_FOUND
    );
    // A new request for the same revision answers with the rendering job.
    let done = f
        .post(&path, &operator, Some("preview-request-0005"), json!(null))
        .await;
    assert_eq!(
        (done.status, &done.body["job_id"]),
        (StatusCode::OK, &queued.body["job_id"])
    );
    assert_eq!(done.body["status"], "ready");
    // Technical metadata changes invalidate previews and retries even when bytes do not.
    let ready_file = ready["file_id"].as_str().unwrap();
    sqlx::query("UPDATE media_files SET duration_seconds=30 WHERE id=?")
        .bind(ready_file)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    assert_eq!(f.get(&path, &auth).await.body["status"], "none");
    assert_eq!(
        f.call("GET", &url, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let stale_retry = f
        .retry_job(&cancelled_id, &operator, "preview-stale-retry-01")
        .await;
    assert_eq!(stale_retry.status, StatusCode::NOT_FOUND);
    sqlx::query("UPDATE media_files SET duration_seconds=25 WHERE id=?")
        .bind(ready_file)
        .execute(&f.server.app.db)
        .await
        .unwrap();
    assert_eq!(f.get(&path, &auth).await.body["status"], "ready");
    // Sheets missing on disk (a database restored without them): retention
    // withdraws the previews, and a request renders them again.
    let file_id = ready["file_id"].as_str().unwrap().to_owned();
    let revision = ready["file_revision"].as_str().unwrap().to_owned();
    let root = &f.server.app.processing.root;
    let published = root
        .parent()
        .unwrap_or(root)
        .join("previews")
        .join(&file_id)
        .join(&revision);
    // A corrupted sparse sheet is refused without loading its full length.
    std::fs::OpenOptions::new()
        .write(true)
        .open(published.join("sheet-1.jpg"))
        .unwrap()
        .set_len(playscale_core::previews::MAX_SHEET_BYTES + 1)
        .unwrap();
    assert_eq!(
        f.call("GET", &url, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    std::fs::remove_file(published.join("sheet-1.jpg")).unwrap();
    playscale::v2::previews::retain(&f.server.app)
        .await
        .unwrap();
    assert_eq!(f.get(&path, &auth).await.body["status"], "none");
    let rebuilt = f
        .post(&path, &operator, Some("preview-request-0006"), json!(null))
        .await;
    assert_eq!(rebuilt.status, StatusCode::ACCEPTED, "{}", rebuilt.body);
    for _ in 0..600 {
        if f.get(&path, &auth).await.body["status"] == "ready" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(f.get(&path, &auth).await.body["status"], "ready");
    // The file changes: its previews retire with the old revision.
    sqlx::query("UPDATE media_files SET revision='0000000000000000000000000000000000000000000000000000000000000001'")
        .execute(&f.server.app.db)
        .await
        .unwrap();
    playscale::v2::previews::retain(&f.server.app)
        .await
        .unwrap();
    assert_eq!(
        f.call("GET", &url, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    stop.cancel();
    worker.await.unwrap().unwrap();
}

#[tokio::test]
async fn planning_uses_one_pool_slot_for_components_and_preferences() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    let f = Fixture::new().await;
    two_audio_mp4(&f, "2");
    f.scan().await;
    let (timeline, _) = f.timeline().await;
    let auth = bearer(OPERATOR);
    let mut occupied = Vec::new();
    for _ in 0..3 {
        occupied.push(f.server.app.db.acquire().await.unwrap());
    }
    for policy in ["off", "auto"] {
        let mut input = plan_input("default", &timeline, "original", None, &["http_range"]);
        input["tracks"]["subtitle_policy"] = json!(policy);
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            f.post("/api/v2/playback/plans", &auth, None, input),
        )
        .await
        .expect("planning must not acquire a second pool connection");
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
        assert_eq!(response.body["status"], "ready");
    }
}
