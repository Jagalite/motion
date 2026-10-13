//! HTTP conformance for offline downloads and offline reconciliation (A13):
//! real router, real SQLite transactions, real scanned files and byte ranges.
//! The decisions are unit-tested in crates/core/src/downloads.rs and
//! model-checked in crates/core/tests/downloads_model.rs.
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tower::ServiceExt;

const OPERATOR: &str = "operator-secret-token-0123456789abcdef";
const ORIGIN: &str = "http://127.0.0.1:8787";
const PERMISSIONS: [&str; 3] = ["catalog:read", "downloads:manage", "viewing:write"];

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
    library: String,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn problem(reply: &Reply, status: StatusCode, code: &str) {
    assert_eq!(reply.status, status, "{:?}", reply.body);
    assert_eq!(reply.body["code"], code, "{:?}", reply.body);
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let library = db::add_library(&db, "Movies", &root).await.unwrap().id;
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            db,
            admin_token: Arc::new(OPERATOR.into()),
            origin: Arc::new(ORIGIN.into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(2)),
            event_streams: Arc::new(Semaphore::new(4)),
            storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
            processing: Arc::new(playscale::processing::Runtime::new(
                dir.path().join("cache"),
                Default::default(),
            )),
            access: Arc::new(playscale::v2::Runtime::new(
                AccessMode::Restricted,
                playscale::v2::auth::random_key(),
            )),
        };
        Self { dir, app, library }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Request<Body> {
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
        request
            .body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
            .unwrap()
    }

    async fn raw(&self, request: Request<Body>) -> axum::response::Response {
        api::router(self.app.clone(), None)
            .oneshot(request)
            .await
            .unwrap()
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let mut request = self.request(method, path, body.as_ref(), headers);
        if method == "PUT" && path.ends_with("/viewing") {
            let (server, restore) = playscale::v2::system::server_identity(&self.app)
                .await
                .unwrap();
            request.headers_mut().insert(
                "x-motion-viewing-epoch",
                format!("{server}:{restore}").parse().unwrap(),
            );
        }
        let response = self.raw(request).await;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn bytes(&self, path: &str, headers: &[(&str, &str)]) -> (StatusCode, Vec<u8>) {
        let response = self.raw(self.request("GET", path, None, headers)).await;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, body)
    }

    /// Pair a device with the given grant and the fixture library; returns
    /// (device_id, bearer header).
    async fn device(&self, profiles: &[&str], permissions: &[&str]) -> (String, String) {
        let pairing = self
            .call(
                "POST",
                "/api/v2/auth/pairings",
                Some(json!({"device_name":"Laptop","client_name":"motion-test"})),
                &[],
            )
            .await;
        assert_eq!(pairing.status, StatusCode::CREATED, "{:?}", pairing.body);
        let id = pairing.body["id"].as_str().unwrap().to_owned();
        let key = format!("approve-{id}");
        let approved = self
            .call("POST", &format!("/api/v2/auth/pairings/{id}/approve"),
                Some(json!({"user_code":pairing.body["user_code"],"profile_ids":profiles,"permissions":permissions})),
                &[("authorization", &bearer(OPERATOR)), ("idempotency-key", &key)])
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
        let etag = self
            .call(
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[("authorization", &bearer(OPERATOR))],
            )
            .await
            .headers["etag"]
            .to_str()
            .unwrap()
            .to_owned();
        let policy = self
            .call("PUT", &format!("/api/v2/devices/{device}/policy"),
                Some(json!({"library_ids":[self.library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                &[("authorization", &bearer(OPERATOR)), ("if-match", &etag)])
            .await;
        assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
        (
            device,
            bearer(claimed.body["access_token"].as_str().unwrap()),
        )
    }

    /// Scan `name` into the library; returns (timeline_id, file_id, revision).
    async fn scanned(&self, name: &str, bytes: &[u8]) -> (String, String, String) {
        std::fs::write(self.dir.path().join("media").join(name), bytes).unwrap();
        let job = db::enqueue_mode(&self.app, &self.library, false)
            .await
            .unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let task = tokio::spawn(playscale::scan::worker(self.app.clone(), stop.clone()));
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let row = db::get_job(&self.app.db, &job.id).await.unwrap();
                if !["queued", "running", "cancelling"].contains(&row.phase.as_str()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
        task.await.unwrap().unwrap();
        let (timeline, file, revision): (String, String, String) = sqlx::query_as(
            "SELECT t.id,f.id,f.revision FROM media_files f JOIN timelines t ON t.edition_id=f.edition_id WHERE f.relative_path=? AND f.available=1",
        )
        .bind(name)
        .fetch_one(&self.app.db)
        .await
        .unwrap();
        // Synthetic bytes cannot be probed; the timeline duration is curated.
        sqlx::query("UPDATE timelines SET duration_ms=100000 WHERE id=?")
            .bind(&timeline)
            .execute(&self.app.db)
            .await
            .unwrap();
        (timeline, file, revision)
    }

    async fn post(&self, path: &str, auth: &str, key: &str, body: Value) -> Reply {
        self.call(
            "POST",
            path,
            Some(body),
            &[("authorization", auth), ("idempotency-key", key)],
        )
        .await
    }

    async fn get(&self, path: &str, auth: &str) -> Reply {
        self.call("GET", path, None, &[("authorization", auth)])
            .await
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.app.db)
            .await
            .unwrap()
    }
}

fn input(device: &str, timeline: &str) -> Value {
    json!({"device_id":device,"profile_id":"default","timeline_id":timeline,"version_id":null,"recipe_id":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"auto","audio_track_id":null,"subtitle_track_id":null}})
}

fn download_ticket(id: &str) -> Value {
    json!({"purpose":"download","delivery_id":null,"generation":null,"file":null,"download_id":id,"ttl_seconds":600})
}

#[tokio::test]
async fn downloads_pin_exact_originals_and_grant_only_their_bytes() {
    let f = Fixture::new().await;
    let media: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let (timeline, file, revision) = f.scanned("film.mp4", &media).await;
    let (mkv, _, _) = f.scanned("other.mkv", b"matroska bytes").await;
    let (short, _, _) = f.scanned("short.webm", b"short webm bytes").await;
    let (device, auth) = f.device(&["default"], &PERMISSIONS).await;

    // Explicit refusals never substitute another selection.
    let mut recipe = input(&device, &timeline);
    recipe["recipe_id"] = json!("x");
    problem(
        &f.post("/api/v2/downloads", &auth, "download-key-0001", recipe)
            .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "download_preparation_unavailable",
    );
    let mut subtitles = input(&device, &timeline);
    subtitles["tracks"]["subtitle_policy"] = json!("require");
    problem(
        &f.post("/api/v2/downloads", &auth, "download-key-0002", subtitles)
            .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "download_tracks_unavailable",
    );
    problem(
        &f.post(
            "/api/v2/downloads",
            &auth,
            "download-key-0003",
            input(&device, &mkv),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "download_container_unavailable",
    );
    problem(
        &f.post(
            "/api/v2/downloads",
            &auth,
            "download-key-0004",
            input("another-device", &timeline),
        )
        .await,
        StatusCode::FORBIDDEN,
        "device_mismatch",
    );
    problem(
        &f.post(
            "/api/v2/downloads",
            &bearer(OPERATOR),
            "download-key-0005",
            input(&device, &timeline),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "device_required",
    );
    assert_eq!(f.count("SELECT count(*) FROM downloads").await, 0);

    // An explicitly requested version is honoured (filtered before any bound).
    let version: String =
        sqlx::query_scalar("SELECT v.id FROM media_versions v WHERE v.timeline_id=?")
            .bind(&timeline)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let mut pinned = input(&device, &timeline);
    pinned["version_id"] = json!(version);
    let explicit = f
        .post("/api/v2/downloads", &auth, "download-key-0010", pinned)
        .await;
    assert_eq!(explicit.status, StatusCode::ACCEPTED, "{:?}", explicit.body);
    let explicit_id = explicit.body["id"].as_str().unwrap().to_owned();
    let gone = f
        .call(
            "DELETE",
            &format!("/api/v2/downloads/{explicit_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    sqlx::query("DELETE FROM downloads WHERE id=?")
        .bind(&explicit_id)
        .execute(&f.app.db)
        .await
        .unwrap();

    let created = f
        .post(
            "/api/v2/downloads",
            &auth,
            "download-key-0006",
            input(&device, &timeline),
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    assert_eq!(created.body["state"], "ready");
    assert_eq!(created.body["size_bytes"], "4096");
    let id = created.body["id"].as_str().unwrap().to_owned();
    let replay = f
        .post(
            "/api/v2/downloads",
            &auth,
            "download-key-0006",
            input(&device, &timeline),
        )
        .await;
    assert_eq!(
        (replay.status, &replay.body),
        (StatusCode::ACCEPTED, &created.body)
    );
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(f.count("SELECT count(*) FROM downloads").await, 1);

    let manifest = f
        .get(&format!("/api/v2/downloads/{id}/manifest"), &auth)
        .await;
    assert_eq!(manifest.status, StatusCode::OK, "{:?}", manifest.body);
    let entry = &manifest.body["files"][0];
    assert_eq!(entry["digest"], format!("sha256:{revision}"));
    assert_eq!(entry["size_bytes"], "4096");
    assert_eq!(entry["relative_name"], "media.mp4");
    assert_eq!(entry["role"], "media");
    assert_eq!(manifest.body["duration_ms"], 100000);
    assert_eq!(manifest.body["viewing_revision"], "0");
    assert_eq!(manifest.body["manual_epoch"], "0");
    let (server, restore) = playscale::v2::system::server_identity(&f.app)
        .await
        .unwrap();
    assert_eq!(
        manifest.body["viewing_epoch"],
        format!("{server}:{restore}")
    );
    let timeline_revision = f
        .count(&format!(
            "SELECT revision FROM timelines WHERE id='{timeline}'"
        ))
        .await;
    assert_eq!(
        manifest.body["timeline_revision"],
        timeline_revision.to_string()
    );

    // A download ticket admits exactly the pinned revision, resumably.
    let ticket = f
        .post(
            "/api/v2/content-access",
            &auth,
            "ticket-key-00001",
            download_ticket(&id),
        )
        .await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{:?}", ticket.body);
    let url = ticket.body["url"].as_str().unwrap().to_owned();
    assert!(url.contains(&format!("/media/files/{file}/content?revision={revision}")));
    let (status, head) = f.bytes(&url, &[("range", "bytes=0-1023")]).await;
    assert_eq!((status, head.len()), (StatusCode::PARTIAL_CONTENT, 1024));
    let (status, tail) = f.bytes(&url, &[("range", "bytes=1024-")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    let whole = [head, tail].concat();
    assert_eq!(format!("{:x}", Sha256::digest(&whole)), revision);

    // Another device of the same household sees nothing of it.
    let (other, other_auth) = f.device(&["default"], &PERMISSIONS).await;
    problem(
        &f.get(&format!("/api/v2/downloads/{id}"), &other_auth).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.post(
            "/api/v2/content-access",
            &other_auth,
            "ticket-key-00002",
            download_ticket(&id),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let listed = f.get("/api/v2/downloads", &other_auth).await;
    assert_eq!(listed.body["items"], json!([]));
    let listed = f.get("/api/v2/downloads", &auth).await;
    assert_eq!(listed.body["items"][0]["id"], id.as_str());
    let _ = other;

    // Changing the source expires the download: no manifest, no new grant.
    let changed: Vec<u8> = media.iter().rev().copied().collect();
    let (_, _, new_revision) = f.scanned("film.mp4", &changed).await;
    assert_ne!(new_revision, revision);
    assert_eq!(
        f.get(&format!("/api/v2/downloads/{id}"), &auth).await.body["state"],
        "expired"
    );
    problem(
        &f.get(&format!("/api/v2/downloads/{id}/manifest"), &auth)
            .await,
        StatusCode::CONFLICT,
        "download_expired",
    );
    problem(
        &f.post(
            "/api/v2/content-access",
            &auth,
            "ticket-key-00003",
            download_ticket(&id),
        )
        .await,
        StatusCode::CONFLICT,
        "download_unavailable",
    );
    let (status, _) = f.bytes(&url, &[]).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a changed source is never served under the old pin"
    );

    // Until the changed content is reviewed into the version, no new
    // download can pin either revision.
    problem(
        &f.post(
            "/api/v2/downloads",
            &auth,
            "download-key-0007",
            input(&device, &timeline),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Cancelling is idempotent and revokes the download's byte grants.
    let fresh = f
        .post(
            "/api/v2/downloads",
            &auth,
            "download-key-0008",
            input(&device, &short),
        )
        .await;
    assert_eq!(fresh.status, StatusCode::ACCEPTED, "{:?}", fresh.body);
    let fresh_id = fresh.body["id"].as_str().unwrap().to_owned();
    let ticket = f
        .post(
            "/api/v2/content-access",
            &auth,
            "ticket-key-00004",
            download_ticket(&fresh_id),
        )
        .await;
    let url = ticket.body["url"].as_str().unwrap().to_owned();
    assert_eq!(f.bytes(&url, &[]).await.0, StatusCode::OK);
    let delete = f
        .call(
            "DELETE",
            &format!("/api/v2/downloads/{fresh_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(delete.status, StatusCode::NO_CONTENT);
    let again = f
        .call(
            "DELETE",
            &format!("/api/v2/downloads/{fresh_id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    assert_eq!(f.bytes(&url, &[]).await.0, StatusCode::UNAUTHORIZED);
    let read = f.get(&format!("/api/v2/downloads/{fresh_id}"), &auth).await;
    assert_eq!(
        (read.body["state"].as_str(), read.body["revision"].as_str()),
        (Some("cancelled"), Some("2"))
    );
    problem(
        &f.get(&format!("/api/v2/downloads/{fresh_id}/manifest"), &auth)
            .await,
        StatusCode::CONFLICT,
        "download_cancelled",
    );
}

fn event(
    sequence: u64,
    timeline: &str,
    base: (&str, &str),
    position_ms: u64,
    status: &str,
) -> Value {
    // Curating the duration in `scanned` makes the timeline revision 2.
    json!({"event_id":format!("offline-{sequence}"),"device_sequence":sequence.to_string(),"download_id":"offline-download","profile_id":"default",
        "timeline_id":timeline,"timeline_revision":"2","base_viewing_revision":base.0,"base_manual_epoch":base.1,
        "position_ms":position_ms,"status":status,"device_time":format!("2026-10-11T10:00:0{}Z", sequence % 10)})
}

fn outcomes(reply: &Reply) -> Vec<(String, Value)> {
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["outcome"].as_str().unwrap().to_owned(), r["code"].clone()))
        .collect()
}

#[tokio::test]
async fn offline_sync_consumes_sequences_once_and_never_overwrites_newer_authority() {
    let f = Fixture::new().await;
    let (timeline, _, _) = f.scanned("film.mp4", b"offline film").await;
    assert_eq!(
        f.count(&format!(
            "SELECT revision FROM timelines WHERE id='{timeline}'"
        ))
        .await,
        2
    );
    let (device, auth) = f.device(&["default"], &PERMISSIONS).await;
    let (server, restore) = playscale::v2::system::server_identity(&f.app)
        .await
        .unwrap();
    let history = format!("{server}:{restore}");
    let sync_with = |events: Vec<Value>, device: String, auth: String, epoch: Option<String>| {
        let f = &f;
        async move {
            let mut headers = vec![("authorization", auth.as_str())];
            if let Some(epoch) = &epoch {
                headers.push(("x-motion-viewing-epoch", epoch.as_str()));
            }
            f.call(
                "POST",
                "/api/v2/offline/sync",
                Some(json!({"device_id":device,"events":events})),
                &headers,
            )
            .await
        }
    };
    let sync = |events: Vec<Value>, device: String, auth: String| {
        sync_with(events, device, auth, Some(history.clone()))
    };
    let viewing = format!("/api/v2/profiles/default/timelines/{timeline}/viewing");
    let base = ("0", "0");

    // Two consecutive offline observations chain from the download's base.
    let batch = vec![
        event(1, &timeline, base, 10_000, "paused"),
        event(2, &timeline, base, 20_000, "paused"),
    ];
    let first = sync(batch.clone(), device.clone(), auth.clone()).await;
    assert_eq!(
        outcomes(&first),
        vec![
            ("applied".into(), Value::Null),
            ("applied".into(), Value::Null)
        ]
    );
    assert_eq!(first.body["next_device_sequence"], "3");
    assert_eq!(
        first.body["results"][1]["current_viewing"]["position_ms"],
        20000
    );
    let state = f.get(&viewing, &auth).await;
    assert_eq!(
        (
            state.body["position_ms"].as_u64(),
            state.body["revision"].as_str()
        ),
        (Some(20_000), Some("2"))
    );

    // An exact retry (lost response) changes nothing and replays the outcome.
    let retry = sync(batch.clone(), device.clone(), auth.clone()).await;
    assert_eq!(
        outcomes(&retry),
        vec![
            ("duplicate".into(), json!("applied")),
            ("duplicate".into(), json!("applied"))
        ]
    );
    assert_eq!(retry.body["next_device_sequence"], "3");
    assert_eq!(f.count("SELECT count(*) FROM offline_events").await, 2);
    // Altered reuse and gaps consume nothing.
    let mut altered = event(2, &timeline, base, 25_000, "paused");
    altered["device_time"] = batch[1]["device_time"].clone();
    let rejected = sync(
        vec![altered, event(5, &timeline, base, 1, "paused")],
        device.clone(),
        auth.clone(),
    )
    .await;
    assert_eq!(
        outcomes(&rejected),
        vec![
            ("rejected".into(), json!("event_conflict")),
            ("rejected".into(), json!("sequence_gap"))
        ]
    );
    assert_eq!(rejected.body["next_device_sequence"], "3");

    // A manual change on the server wins over later offline progress.
    let tag = f.get(&viewing, &auth).await.headers["etag"]
        .to_str()
        .unwrap()
        .to_owned();
    let manual = f
        .call(
            "PUT",
            &viewing,
            Some(json!({"manual_watched":false,"reset_position":true})),
            &[("authorization", &auth), ("if-match", &tag)],
        )
        .await;
    assert_eq!(manual.status, StatusCode::OK, "{:?}", manual.body);
    let stale = sync(
        vec![event(3, &timeline, base, 30_000, "paused")],
        device.clone(),
        auth.clone(),
    )
    .await;
    assert_eq!(
        outcomes(&stale),
        vec![("history_only".into(), json!("manual_override_newer"))]
    );
    let state = f.get(&viewing, &auth).await;
    assert_eq!(
        (
            state.body["position_ms"].as_u64(),
            state.body["manual_watched"].as_bool()
        ),
        (Some(0), Some(false))
    );

    // Rebasing on a fresh manifest read lets later offline progress apply;
    // an out-of-range position is rejected but still consumes its sequence.
    let rebased = (
        state.body["revision"].as_str().unwrap().to_owned(),
        state.body["manual_epoch"].as_str().unwrap().to_owned(),
    );
    let later = sync(
        vec![
            event(4, &timeline, (&rebased.0, &rebased.1), 99_500, "ended"),
            event(5, &timeline, (&rebased.0, &rebased.1), 500_000, "paused"),
        ],
        device.clone(),
        auth.clone(),
    )
    .await;
    assert_eq!(
        outcomes(&later),
        vec![
            ("applied".into(), Value::Null),
            ("rejected".into(), json!("invalid_position"))
        ]
    );
    assert_eq!(later.body["next_device_sequence"], "6");
    let state = f.get(&viewing, &auth).await;
    assert_eq!(state.body["position_ms"], 99_500);
    // Completion is recorded underneath, but the manual override still decides.
    assert_eq!(
        (
            state.body["watched"].as_bool(),
            state.body["manual_watched"].as_bool()
        ),
        (Some(false), Some(false))
    );

    // Another device with a stale base cannot overwrite the newer state.
    let (other, other_auth) = f.device(&["default"], &PERMISSIONS).await;
    let foreign = sync(
        vec![event(
            1,
            &timeline,
            (&rebased.0, &rebased.1),
            5_000,
            "paused",
        )],
        other.clone(),
        other_auth.clone(),
    )
    .await;
    assert_eq!(
        outcomes(&foreign),
        vec![("history_only".into(), json!("viewing_newer"))]
    );
    assert_eq!(
        f.get(&viewing, &auth).await.body["position_ms"],
        state.body["position_ms"]
    );
    // Devices speak only for themselves.
    problem(
        &sync(
            vec![event(6, &timeline, base, 1, "paused")],
            device.clone(),
            other_auth.clone(),
        )
        .await,
        StatusCode::FORBIDDEN,
        "device_mismatch",
    );

    // An unusable profile holds the whole batch without consuming it.
    let mut kids = event(6, &timeline, base, 1, "paused");
    kids["profile_id"] = json!("kids");
    problem(
        &sync(vec![kids.clone()], device.clone(), auth.clone()).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &sync(
            vec![event(6, &timeline, base, 1, "paused"), kids],
            device.clone(),
            auth.clone(),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "mixed_profiles",
    );
    assert_eq!(f.count("SELECT next_sequence FROM offline_sequences WHERE profile_id='default' AND device_id=(SELECT device_id FROM offline_events WHERE event_id='offline-5')").await, 6);

    // A missing timeline is rejected per event and consumes its sequence.
    let gone = sync(
        vec![event(6, "missing", base, 1, "paused")],
        device.clone(),
        auth.clone(),
    )
    .await;
    assert_eq!(
        outcomes(&gone),
        vec![("rejected".into(), json!("not_found"))]
    );
    assert_eq!(gone.body["next_device_sequence"], "7");

    // Events based on another server history (a restored database or a
    // replaced server) are refused before anything is consumed or applied.
    let before = f.get(&viewing, &auth).await.body;
    problem(
        &sync_with(
            vec![event(7, &timeline, base, 1, "paused")],
            device.clone(),
            auth.clone(),
            Some(format!("{server}:restored-elsewhere")),
        )
        .await,
        StatusCode::CONFLICT,
        "viewing_epoch_changed",
    );
    problem(
        &sync_with(
            vec![event(7, &timeline, base, 1, "paused")],
            device.clone(),
            auth.clone(),
            None,
        )
        .await,
        StatusCode::PRECONDITION_REQUIRED,
        "viewing_epoch_required",
    );
    assert_eq!(f.get(&viewing, &auth).await.body, before);
    assert_eq!(
        f.count("SELECT count(*) FROM offline_events WHERE sequence=7")
            .await,
        0
    );
}

#[tokio::test]
async fn offline_event_retention_follows_the_devices_acknowledgement() {
    let mut f = Fixture::new().await;
    let (timeline, _, _) = f.scanned("film.mp4", b"retained film").await;
    let (device, auth) = f.device(&["default"], &PERMISSIONS).await;
    let (server, restore) = playscale::v2::system::server_identity(&f.app)
        .await
        .unwrap();
    let history = format!("{server}:{restore}");
    let base = ("0", "0");
    // Macros, not closures: the fixture is reopened in between.
    macro_rules! sync {
        ($events:expr, $acknowledged:expr) => {
            retention_sync(&f, &device, &auth, &history, $events, $acknowledged).await
        };
    }
    macro_rules! retained {
        () => {
            (
                retention_value(&f, &device, "count(*)", "offline_events").await,
                retention_value(&f, &device, "pruned_through", "offline_sequences").await,
            )
        };
    }

    // An older agent reports nothing: every consumed event is retained.
    let first = vec![
        event(1, &timeline, base, 10_000, "paused"),
        event(2, &timeline, base, 20_000, "paused"),
    ];
    let reply = sync!(first.clone(), Value::Null);
    assert_eq!(outcomes(&reply).len(), 2);
    assert_eq!(retained!(), (2, 0));

    // The device compacted 1 and 2 and says so with its next batch.
    let third = vec![event(3, &timeline, base, 30_000, "paused")];
    let reply = sync!(third.clone(), json!("2"));
    assert_eq!(reply.body["next_device_sequence"], "4");
    assert_eq!(retained!(), (1, 2));
    // Its response was lost: the retry with the same report is a duplicate.
    let retry = sync!(third.clone(), json!("2"));
    assert_eq!(outcomes(&retry)[0].0, "duplicate");
    assert_eq!(retry.body["next_device_sequence"], "4");

    // The floor and records survive a restart (a reopened database).
    f.app.db.close().await;
    f.app.db = db::connect(&f.dir.path().join("db.sqlite")).await.unwrap();
    assert_eq!(retained!(), (1, 2));

    // A stale copy of the device resends a pruned event: refused, nothing
    // consumed, and an older report never moves the floor back.
    let stale = sync!(first[..1].to_vec(), json!("0"));
    assert_eq!(
        outcomes(&stale),
        vec![("rejected".into(), json!("sequence_acknowledged"))]
    );
    assert_eq!(stale.body["next_device_sequence"], "4");
    assert_eq!(retained!(), (1, 2));

    // A report beyond what was consumed, or a malformed one, is refused
    // before anything is pruned or consumed.
    let fourth = vec![event(4, &timeline, base, 40_000, "paused")];
    problem(
        &sync!(fourth.clone(), json!("4")),
        StatusCode::UNPROCESSABLE_ENTITY,
        "acknowledgement_ahead",
    );
    problem(
        &sync!(fourth.clone(), json!("x")),
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_acknowledgement",
    );
    // A batch refused for an unusable profile prunes nothing either.
    let mut kids = fourth[0].clone();
    kids["profile_id"] = json!("kids");
    problem(
        &sync!(vec![kids], json!("3")),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(retained!(), (1, 2));

    // The bound: with 10,000 consumed events unacknowledged, a new event
    // waits (not consumed) until the device reports its compaction.
    let limit = playscale_core::downloads::MAX_RETAINED_EVENTS as i64;
    sqlx::query("UPDATE offline_sequences SET next_sequence=? WHERE device_id=?")
        .bind(2 + limit + 1)
        .bind(&device)
        .execute(&f.app.db)
        .await
        .unwrap();
    let next = (2 + limit + 1) as u64;
    let waiting = vec![event(next, &timeline, base, 50_000, "paused")];
    let full = sync!(waiting.clone(), json!("2"));
    assert_eq!(
        outcomes(&full),
        vec![("rejected".into(), json!("acknowledgement_required"))]
    );
    assert_eq!(full.body["next_device_sequence"], next.to_string());
    let accepted = sync!(waiting, json!((next - 1).to_string()));
    assert_ne!(outcomes(&accepted)[0].0, "rejected");
    assert_eq!(
        accepted.body["next_device_sequence"],
        (next + 1).to_string()
    );
    assert_eq!(retained!(), (1, next as i64 - 1));
}

async fn retention_sync(
    f: &Fixture,
    device: &str,
    auth: &str,
    history: &str,
    events: Vec<Value>,
    acknowledged: Value,
) -> Reply {
    let body = json!({"device_id":device,"events":events,"acknowledged_through":acknowledged});
    f.call(
        "POST",
        "/api/v2/offline/sync",
        Some(body),
        &[("authorization", auth), ("x-motion-viewing-epoch", history)],
    )
    .await
}

async fn retention_value(f: &Fixture, device: &str, column: &str, table: &str) -> i64 {
    f.count(&format!(
        "SELECT {column} FROM {table} WHERE device_id='{device}'"
    ))
    .await
}

#[tokio::test]
async fn reconciliation_uses_the_downloads_effective_duration() {
    let f = Fixture::new().await;
    let (timeline, file, _) = f.scanned("film.mp4", b"probed only").await;
    // An ordinary scanned timeline: no curated duration, only the file's probe.
    sqlx::query("UPDATE timelines SET duration_ms=NULL WHERE id=?")
        .bind(&timeline)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_files SET duration_seconds=100 WHERE id=?")
        .bind(&file)
        .execute(&f.app.db)
        .await
        .unwrap();
    let revision = f
        .count(&format!(
            "SELECT revision FROM timelines WHERE id='{timeline}'"
        ))
        .await
        .to_string();
    let (device, auth) = f.device(&["default"], &PERMISSIONS).await;
    let created = f
        .post(
            "/api/v2/downloads",
            &auth,
            "download-key-0020",
            input(&device, &timeline),
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    let played = created.body["id"].as_str().unwrap().to_owned();
    // A newer demand pins a different probed duration; the event's own
    // download, not the newest one, decides.
    sqlx::query("UPDATE media_files SET duration_seconds=120 WHERE id=?")
        .bind(&file)
        .execute(&f.app.db)
        .await
        .unwrap();
    let newer = f
        .post(
            "/api/v2/downloads",
            &auth,
            "download-key-0021",
            input(&device, &timeline),
        )
        .await;
    assert_eq!(newer.status, StatusCode::ACCEPTED, "{:?}", newer.body);
    let (server, restore) = playscale::v2::system::server_identity(&f.app)
        .await
        .unwrap();
    let history = format!("{server}:{restore}");
    let mut beyond = event(1, &timeline, ("0", "0"), 500_000, "paused");
    // Ended at 99.5 s: complete for the 100 s copy, too early for 120 s.
    let mut completed = event(2, &timeline, ("0", "0"), 99_500, "ended");
    for e in [&mut beyond, &mut completed] {
        e["timeline_revision"] = json!(revision);
        e["download_id"] = json!(played);
    }
    // Naming no download of this device, profile and timeline leaves no
    // effective duration: the event cannot be validated and is rejected.
    let mut foreign = event(3, &timeline, ("0", "0"), 0, "ended");
    foreign["timeline_revision"] = json!(revision);
    foreign["download_id"] = json!("not-this-devices-download");
    let reply = f
        .call(
            "POST",
            "/api/v2/offline/sync",
            Some(json!({"device_id":device,"events":[beyond, completed, foreign]})),
            &[
                ("authorization", &auth),
                ("x-motion-viewing-epoch", &history),
            ],
        )
        .await;
    // Validated against the 100 s the played download was authorized with.
    assert_eq!(
        outcomes(&reply),
        vec![
            ("rejected".into(), json!("invalid_position")),
            ("applied".into(), Value::Null),
            ("rejected".into(), json!("not_found"))
        ]
    );
    let viewing = f
        .get(
            &format!("/api/v2/profiles/default/timelines/{timeline}/viewing"),
            &auth,
        )
        .await;
    assert_eq!(
        viewing.body["watched"], true,
        "ending the played copy completes the timeline"
    );
}

#[tokio::test]
async fn the_downloads_page_lists_every_actionable_record() {
    let f = Fixture::new().await;
    let (timeline, file, revision) = f.scanned("film.mp4", b"paged").await;
    let (device, auth) = f.device(&["default"], &PERMISSIONS).await;
    let created = f
        .post(
            "/api/v2/downloads",
            &auth,
            "download-key-0030",
            input(&device, &timeline),
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    // More records than one API page: 205 ready and one cancelled.
    for n in 0..206 {
        sqlx::query(
            "INSERT INTO downloads(id,revision,principal_id,device_id,profile_id,timeline_id,timeline_revision,version_id,file_id,file_revision,content_type,size_bytes,duration_ms,state,created_at,updated_at) \
             SELECT ?,1,principal_id,device_id,profile_id,timeline_id,timeline_revision,version_id,?,?,content_type,size_bytes,duration_ms,?,0,0 FROM downloads WHERE id=?",
        )
        .bind(format!("zz-{n:03}"))
        .bind(&file)
        .bind(&revision)
        .bind(if n == 205 { "cancelled" } else { "ready" })
        .bind(created.body["id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    }
    let (status, html) = f.bytes("/downloads", &[("authorization", &auth)]).await;
    let html = String::from_utf8(html).unwrap();
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains("DELETE /api/v2/downloads/zz-000"));
    assert!(
        html.contains("DELETE /api/v2/downloads/zz-204"),
        "the last page is listed"
    );
    assert!(!html.contains("zz-205"), "cancelled records need no action");
}

/// After the server is restored from a backup, a device whose compaction
/// point belongs to the earlier history continues its sequence with its
/// first batch under the new one (`downloads::reanchor`); without the
/// re-anchor it would stop at a gap forever. Only this server's own earlier
/// history is accepted, and a retry changes nothing.
#[tokio::test]
async fn offline_sync_reanchors_the_devices_sequence_after_a_restore() {
    let f = Fixture::new().await;
    let (timeline, _, _) = f.scanned("film.mp4", b"restored film").await;
    let (device, auth) = f.device(&["default"], &PERMISSIONS).await;
    let (server, restore) = playscale::v2::system::server_identity(&f.app)
        .await
        .unwrap();
    let before = format!("{server}:{restore}");
    let base = ("0", "0");
    let sync = |history: String, events: Vec<Value>, acknowledged: Value, reanchor: Value| {
        let body = json!({"device_id":device,"events":events,"acknowledged_through":acknowledged,"reanchor":reanchor});
        let auth = auth.clone();
        let f = &f;
        async move {
            f.call(
                "POST",
                "/api/v2/offline/sync",
                Some(body),
                &[
                    ("authorization", &auth),
                    ("x-motion-viewing-epoch", &history),
                ],
            )
            .await
        }
    };
    let first = vec![
        event(1, &timeline, base, 10_000, "paused"),
        event(2, &timeline, base, 20_000, "paused"),
    ];
    let reply = sync(before.clone(), first, Value::Null, Value::Null).await;
    assert_eq!(reply.body["next_device_sequence"], "3", "{:?}", reply.body);
    // The database is restored from a backup taken before the device synced:
    // a new restore epoch and no record of this device's sequence.
    sqlx::query("UPDATE server_identity SET restore_epoch='restored-backup'")
        .execute(&f.app.db)
        .await
        .unwrap();
    for table in ["offline_events", "offline_sequences"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE device_id=?"))
            .bind(&device)
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    let after = format!("{server}:restored-backup");
    let third = vec![event(3, &timeline, base, 30_000, "paused")];
    // The device compacted 1 and 2 in the earlier history: without a
    // re-anchor its report is ahead of this server and nothing proceeds.
    problem(
        &sync(after.clone(), third.clone(), json!("2"), Value::Null).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "acknowledgement_ahead",
    );
    let reanchor = json!({"from_viewing_epoch": before, "through": "2"});
    let continued = sync(after.clone(), third.clone(), json!("2"), reanchor.clone()).await;
    assert_eq!(continued.status, StatusCode::OK, "{:?}", continued.body);
    assert_ne!(
        outcomes(&continued)[0].0,
        "rejected",
        "{:?}",
        continued.body
    );
    assert_eq!(continued.body["next_device_sequence"], "4");
    assert_eq!(
        retention_value(&f, &device, "pruned_through", "offline_sequences").await,
        2
    );
    // A lost response: the retry is a duplicate and the anchor stays.
    let retry = sync(after.clone(), third, json!("2"), reanchor).await;
    assert_eq!(outcomes(&retry)[0].0, "duplicate");
    assert_eq!(retry.body["next_device_sequence"], "4");
    // Never the current history, another server, or a malformed sequence.
    let fourth = vec![event(4, &timeline, base, 40_000, "paused")];
    for reanchor in [
        json!({"from_viewing_epoch": after, "through": "3"}),
        json!({"from_viewing_epoch": "another-server:r1", "through": "3"}),
        json!({"from_viewing_epoch": before, "through": "x"}),
    ] {
        problem(
            &sync(after.clone(), fourth.clone(), json!("3"), reanchor).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_reanchor",
        );
    }
    assert_eq!(
        retention_value(&f, &device, "next_sequence", "offline_sequences").await,
        4
    );
}
