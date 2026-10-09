use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db, scan};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
    app: App,
    library: String,
}
impl Fixture {
    async fn new() -> Self {
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
        let app = App {
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
        Self {
            dir,
            root,
            app,
            library,
        }
    }
    async fn scan(&self) -> db::JobRow {
        self.scan_mode(false).await
    }
    async fn scan_mode(&self, full: bool) -> db::JobRow {
        let row = db::enqueue_mode(&self.app, &self.library, full)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(scan::worker(self.app.clone(), stop.clone()));
        let result = tokio::time::timeout(Duration::from_secs(15), async {
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
        result
    }
    async fn response(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> axum::response::Response {
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
        api::router(self.app.clone(), None)
            .oneshot(
                request
                    .body(
                        body.map(|v| Body::from(v.to_string()))
                            .unwrap_or_else(Body::empty),
                    )
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    async fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        admin: bool,
    ) -> (StatusCode, Value) {
        let headers = if admin {
            vec![("authorization", "Bearer test-secret-token")]
        } else {
            vec![]
        };
        let response = self.response(method, path, body, &headers).await;
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn items(&self) -> Vec<Value> {
        self.json("GET", "/api/v1/items", None, false).await.1["items"]
            .as_array()
            .unwrap()
            .clone()
    }
}

#[tokio::test]
async fn catalog_moves_replacements_progress_and_missing_sources() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("one.mp4"), b"0123456789").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let first = f.items().await.remove(0);
    let id = first["id"].as_str().unwrap();
    let path = format!("/api/v1/profiles/default/progress/{id}");
    assert_eq!(
        f.json("PUT", &path, Some(json!({"position_seconds":12.5})), false)
            .await
            .0,
        StatusCode::OK
    );
    std::fs::rename(f.root.join("one.mp4"), f.root.join("moved.mp4")).unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let moved = f.items().await.remove(0);
    assert_eq!(moved["id"], first["id"]);
    assert_eq!(moved["file_id"], first["file_id"]);
    std::fs::write(f.root.join("moved.mp4"), b"changed bytes").unwrap();
    assert_eq!(
        f.response("GET", first["media_url"].as_str().unwrap(), None, &[])
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(f.scan().await.phase, "completed");
    let replaced = f.items().await.remove(0);
    assert_eq!(replaced["id"], first["id"]);
    assert_ne!(replaced["revision"], first["revision"]);
    let offline = f.dir.path().join("offline");
    std::fs::rename(&f.root, &offline).unwrap();
    assert_eq!(f.scan().await.phase, "failed");
    assert_eq!(f.items().await[0]["available"], true);
    std::fs::create_dir(&f.root).unwrap();
    assert_eq!(f.scan().await.phase, "failed");
    assert_eq!(f.items().await[0]["available"], true);
    std::fs::remove_dir(&f.root).unwrap();
    std::fs::rename(&offline, &f.root).unwrap();
    std::fs::remove_file(f.root.join("moved.mp4")).unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    assert_eq!(f.items().await[0]["available"], false);
    assert_eq!(
        f.json("GET", &path, None, false).await.1["position_seconds"],
        12.5
    );
    f.app.db.close().await;
    let reopened = db::connect(&f.dir.path().join("db.sqlite")).await.unwrap();
    let position: f64 = sqlx::query_scalar("SELECT position_seconds FROM progress")
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(position, 12.5);
}

#[tokio::test]
async fn byte_ranges_validators_empty_files_and_permit_cleanup() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("one.mp4"), b"0123456789").unwrap();
    std::fs::write(f.root.join("zero.mp4"), b"").unwrap();
    f.scan().await;
    let items = f.items().await;
    let url = items.iter().find(|i| i["title"] == "one").unwrap()["media_url"]
        .as_str()
        .unwrap();
    let response = f.response("GET", url, None, &[]).await;
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    drop(response);
    for (method, range, status, expected) in [
        ("GET", "bytes=0-3", 206, "0123"),
        ("GET", "bytes=-3", 206, "789"),
        ("GET", "bytes=4-99", 206, "456789"),
        ("GET", "bytes=4-", 206, "456789"),
        ("GET", "bytes=99-", 416, ""),
        ("GET", "bytes=0-1,4-5", 200, "0123456789"),
        ("HEAD", "bytes=0-3", 200, ""),
    ] {
        let r = f.response(method, url, None, &[("range", range)]).await;
        assert_eq!(r.status().as_u16(), status);
        assert_eq!(
            &r.into_body().collect().await.unwrap().to_bytes()[..],
            expected.as_bytes()
        );
    }
    for (header, value, status) in [
        ("if-range", "\"old\"", 200),
        ("if-range", etag.as_str(), 206),
        ("if-none-match", etag.as_str(), 304),
        ("if-match", "\"old\"", 412),
    ] {
        let r = f
            .response("GET", url, None, &[("range", "bytes=0-3"), (header, value)])
            .await;
        assert_eq!(r.status().as_u16(), status);
        drop(r);
    }
    let zero = items.iter().find(|i| i["title"] == "zero").unwrap()["media_url"]
        .as_str()
        .unwrap();
    let r = f
        .response("GET", zero, None, &[("range", "bytes=0-3")])
        .await;
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["content-range"], "bytes */0");
    drop(r);
    assert_eq!(f.app.streams.available_permits(), 2);
    let a = f.response("GET", url, None, &[]).await;
    let b = f.response("GET", url, None, &[]).await;
    assert_eq!(f.app.streams.available_permits(), 0);
    assert_eq!(f.response("GET", url, None, &[]).await.status(), 503);
    drop(a);
    drop(b);
    assert_eq!(f.app.streams.available_permits(), 2);
}

#[tokio::test]
async fn metadata_imports_local_precedence_tag_provenance_and_cas() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("fixture.mp4"), b"some bytes").unwrap();
    f.scan().await;
    let item = f.items().await.remove(0);
    let id = item["id"].as_str().unwrap();
    let base = format!("/api/v1/items/{id}/metadata");
    let cat = json!({"expected_revision":0,"external_id":"cat-item-1","values":{"title":"Imported title","year":2026,"custom:rating":4},"tags":["Favorite","Drama"]});
    assert_eq!(
        f.json(
            "PUT",
            &format!("{base}/catabolic"),
            Some(cat.clone()),
            false
        )
        .await
        .0,
        401
    );
    let (status, metadata) = f
        .json("PUT", &format!("{base}/catabolic"), Some(cat.clone()), true)
        .await;
    assert_eq!(status, 200);
    assert_eq!(metadata["values"]["title"], "Imported title");
    assert_eq!(metadata["tag_sources"]["favorite"], json!(["catabolic"]));
    assert_eq!(
        f.json("PUT", &format!("{base}/catabolic"), Some(cat), true)
            .await
            .0,
        409
    );
    let local = json!({"expected_revision":0,"external_id":null,"values":{"title":"Local title"},"tags":["favorite"],"excluded_tags":["drama"]});
    assert_eq!(
        f.json("PUT", &format!("{base}/local"), Some(local), true)
            .await
            .0,
        200
    );
    f.scan().await;
    assert_eq!(f.items().await[0]["title"], "Local title");
    let cat = json!({"expected_revision":1,"external_id":"cat-item-1","values":{"title":"Changed import"},"tags":["drama"]});
    let (_, metadata) = f
        .json("PUT", &format!("{base}/catabolic"), Some(cat), true)
        .await;
    assert_eq!(metadata["values"]["title"], "Local title");
    assert_eq!(metadata["tags"], json!(["favorite"]));
    assert_eq!(metadata["sources"].as_array().unwrap().len(), 2);
    let other = json!({"expected_revision":0,"external_id":null,"values":{"year":2025},"tags":[]});
    assert_eq!(
        f.json("PUT", &format!("{base}/other"), Some(other), true)
            .await
            .0,
        200
    );
}

#[tokio::test]
async fn existing_renditions_register_idempotently_without_jobs_and_pin_revisions() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("original.mp4"), b"original").unwrap();
    std::fs::write(f.root.join("smaller.mp4"), b"smaller").unwrap();
    f.scan().await;
    let files = f.items().await;
    let original = files.iter().find(|i| i["title"] == "original").unwrap();
    let output = files.iter().find(|i| i["title"] == "smaller").unwrap();
    let id = original["id"].as_str().unwrap();
    let path = format!("/api/v1/items/{id}/renditions/catabolic/output-1");
    let body = json!({"file_id":output["file_id"],"file_revision":output["revision"],"source_file_id":original["file_id"],"source_revision":original["revision"],"label":"Small","recipe":{"codec":"h264","external_job":"example"}});
    let (_, a) = f.json("PUT", &path, Some(body.clone()), true).await;
    let (status, b) = f.json("PUT", &path, Some(body), true).await;
    assert_eq!(status, 200);
    assert_eq!(a["renditions"][0]["id"], b["renditions"][0]["id"]);
    assert_eq!(b["renditions"][0]["available"], true);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let response = f
        .response(
            "GET",
            b["renditions"][0]["media_url"].as_str().unwrap(),
            None,
            &[],
        )
        .await;
    assert_eq!(
        &response.into_body().collect().await.unwrap().to_bytes()[..],
        b"smaller"
    );
    std::fs::remove_file(f.root.join("original.mp4")).unwrap();
    f.scan().await;
    assert_eq!(
        f.json(
            "GET",
            &format!("/api/v1/items/{id}/playback-options"),
            None,
            false
        )
        .await
        .1["renditions"][0]["available"],
        true
    );
    std::fs::write(f.root.join("original.mp4"), b"changed original").unwrap();
    f.scan().await;
    assert_eq!(
        f.json(
            "GET",
            &format!("/api/v1/items/{id}/playback-options"),
            None,
            false
        )
        .await
        .1["renditions"][0]["available"],
        false
    );
}

#[tokio::test]
async fn job_admission_cancellation_and_restart_recovery() {
    let f = Fixture::new().await;
    let job = db::enqueue(&f.app, &f.library).await.unwrap();
    assert_eq!(db::enqueue(&f.app, &f.library).await.unwrap().id, job.id);
    assert_eq!(
        db::cancel(&f.app, &job.id).await.unwrap().phase,
        "cancelled"
    );
    let job = db::enqueue(&f.app, &f.library).await.unwrap();
    sqlx::query("UPDATE jobs SET phase='running',attempt=1 WHERE id=?")
        .bind(&job.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    db::recover(&f.app.db).await.unwrap();
    assert_eq!(
        db::get_job(&f.app.db, &job.id).await.unwrap().phase,
        "queued"
    );
    sqlx::query("UPDATE jobs SET phase='cancelling' WHERE id=?")
        .bind(&job.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    db::recover(&f.app.db).await.unwrap();
    assert_eq!(
        db::get_job(&f.app.db, &job.id).await.unwrap().phase,
        "cancelled"
    );
}

#[tokio::test]
async fn boundary_contract_and_paths_are_private() {
    let f = Fixture::new().await;
    let response = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri("/api/v1/items")
                .header("host", "evil.invalid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(
        f.response(
            "POST",
            "/api/v1/libraries",
            Some(json!({"name":"x","root":"/tmp"})),
            &[
                ("origin", "https://evil.invalid"),
                ("authorization", "Bearer test-secret-token")
            ]
        )
        .await
        .status(),
        403
    );
    let (_, libs) = f.json("GET", "/api/v1/libraries", None, false).await;
    assert!(!libs.to_string().contains(f.root.to_str().unwrap()));
    assert_eq!(
        f.json("GET", "/api/v1/items?limit=0", None, false).await.0,
        400
    );
    let (_, spec) = f.json("GET", "/api/v1/openapi.json", None, false).await;
    for path in [
        "/api/v1/items",
        "/api/v1/items/{id}/metadata/{source}",
        "/api/v1/items/{id}/renditions/{source}/{external_id}",
        "/media/{id}",
    ] {
        assert!(spec["paths"][path].is_object(), "{path}");
    }
    assert!(spec["components"]["securitySchemes"]["admin_token"].is_object());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_escape_library() {
    let f = Fixture::new().await;
    let path = f.root.join("inside.mp4");
    std::fs::write(&path, b"inside").unwrap();
    f.scan().await;
    let url = f.items().await[0]["media_url"]
        .as_str()
        .unwrap()
        .to_string();
    let outside = f.dir.path().join("secret");
    std::fs::write(&outside, b"secret").unwrap();
    std::fs::remove_file(path).unwrap();
    std::os::unix::fs::symlink(outside, f.root.join("inside.mp4")).unwrap();
    assert_eq!(f.response("GET", &url, None, &[]).await.status(), 404);
}

#[cfg(unix)]
#[tokio::test]
async fn active_cancellation_stops_probe_and_does_not_publish_partial_catalog() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new().await;
    std::fs::write(f.root.join("original.mp4"), b"old").unwrap();
    f.scan().await;
    let before = f.items().await;
    std::fs::write(f.root.join("new.mp4"), b"new").unwrap();
    let marker = f.dir.path().join("probe.pid");
    let executable = f.dir.path().join("slow-probe");
    // Use an OS shell and exec so the recorded PID is the process being cancelled.
    // Avoid an env/Python startup dependency before the readiness marker.
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"${0%/*}/probe.pid\"\nexec /bin/sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.app.ffprobe = Arc::new(executable);
    let job = db::enqueue(&f.app, &f.library).await.unwrap();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    // Helper startup has the probe budget; cancellation and reaping below remain bounded to five seconds.
    tokio::time::timeout(Duration::from_secs(30), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(
        db::cancel(&f.app, &job.id).await.unwrap().phase,
        "cancelling"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if db::get_job(&f.app.db, &job.id).await.unwrap().phase == "cancelled" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    worker.await.unwrap().unwrap();
    assert_eq!(f.items().await, before);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let output = tokio::process::Command::new("ps")
                .args(["-p", pid.trim(), "-o", "pid="])
                .output()
                .await
                .unwrap();
            if !output.status.success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn changed_file_aborts_an_admitted_body_and_profiles_are_separate() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("one.mp4"), vec![1; 256 * 1024]).unwrap();
    f.scan().await;
    let item = f.items().await.remove(0);
    let response = f
        .response("GET", item["media_url"].as_str().unwrap(), None, &[])
        .await;
    std::fs::write(f.root.join("one.mp4"), vec![2; 256 * 1024]).unwrap();
    assert!(response.into_body().collect().await.is_err());
    assert_eq!(f.app.streams.available_permits(), 2);
    let (status, profile) = f
        .json(
            "POST",
            "/api/v1/profiles",
            Some(json!({"name":"Second viewer"})),
            true,
        )
        .await;
    assert_eq!(status, 201);
    let id = item["id"].as_str().unwrap();
    let base = format!("/api/v1/profiles/default/progress/{id}");
    let second = format!(
        "/api/v1/profiles/{}/progress/{id}",
        profile["id"].as_str().unwrap()
    );
    assert_eq!(
        f.json("PUT", &base, Some(json!({"position_seconds":3.0})), false)
            .await
            .0,
        200
    );
    assert_eq!(f.json("GET", &second, None, false).await.0, 404);
    assert_eq!(
        f.json("PUT", &second, Some(json!({"position_seconds":8.0})), false)
            .await
            .0,
        200
    );
    assert_eq!(
        f.json("GET", &base, None, false).await.1["position_seconds"],
        3.0
    );
}

#[tokio::test]
async fn logical_catalog_hierarchy_editions_and_rescan_preserve_identity() {
    let f = Fixture::new().await;
    let create = |kind: &str, title: &str, parent: Value, number: Value| json!({"media_type":kind,"title":title,"parent_id":parent,"number":number});
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/catalog/items",
            Some(create("series", "Show", Value::Null, Value::Null)),
            false
        )
        .await
        .0,
        401
    );
    let (status, series) = f
        .json(
            "POST",
            "/api/v1/catalog/items",
            Some(create("series", "Show", Value::Null, Value::Null)),
            true,
        )
        .await;
    assert_eq!(status, 201);
    let series_id = series["id"].as_str().unwrap();
    let (status, season) = f
        .json(
            "POST",
            "/api/v1/catalog/items",
            Some(create("season", "Specials", json!(series_id), json!(0))),
            true,
        )
        .await;
    assert_eq!(status, 201);
    let season_id = season["id"].as_str().unwrap();
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/catalog/items",
            Some(create("season", "Duplicate", json!(series_id), json!(0))),
            true
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/catalog/items",
            Some(create(
                "episode",
                "Wrong parent",
                json!(series_id),
                json!(1)
            )),
            true
        )
        .await
        .0,
        400
    );
    std::fs::write(f.root.join("episode.mp4"), b"test episode").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let original = f.items().await.remove(0);
    let id = original["id"].as_str().unwrap();
    let structure =
        json!({"expected_revision":0,"media_type":"episode","parent_id":season_id,"number":1});
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/items/{id}/structure"),
            Some(structure.clone()),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/items/{id}/structure"),
            Some(structure),
            true
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/items/{series_id}/structure"),
            Some(json!({"expected_revision":1,"media_type":"movie"})),
            true
        )
        .await
        .0,
        400
    );
    assert_eq!(f.json("PUT",&format!("/api/v1/items/{season_id}/structure"),Some(json!({"expected_revision":1,"media_type":"season","parent_id":season_id,"number":0})),true).await.0,400);
    let page = f
        .json(
            "GET",
            &format!("/api/v1/catalog/items?parent_id={season_id}"),
            None,
            false,
        )
        .await
        .1;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["id"], id);
    assert_eq!(
        f.json("GET", "/api/v1/catalog/items?limit=0", None, false)
            .await
            .0,
        400
    );
    let (status, edition) = f
        .json(
            "POST",
            &format!("/api/v1/items/{id}/editions"),
            Some(json!({"label":"Broadcast"})),
            true,
        )
        .await;
    assert_eq!(status, 201);
    let eid = edition["id"].as_str().unwrap();
    assert_eq!(
        f.json(
            "PUT",
            &format!(
                "/api/v1/files/{}/edition",
                original["file_id"].as_str().unwrap()
            ),
            Some(json!({"expected_edition_id":original["edition_id"],"edition_id":eid})),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/editions/{eid}"),
            Some(json!({"expected_revision":1,"label":"Broadcast cut"})),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/editions/{eid}"),
            Some(json!({"expected_revision":1,"label":"Stale"})),
            true
        )
        .await
        .0,
        409
    );
    let metadata = json!({"expected_revision":0,"values":{"title":"Curated episode","description":"A special episode","release_year":2026,"cast":[{"name":"Test Actor","role":"Host"}]},"tags":["special"]});
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/items/{id}/metadata/catabolic"),
            Some(metadata),
            true
        )
        .await
        .0,
        200
    );
    let progress = format!("/api/v1/profiles/default/progress/{id}");
    f.json("PUT", &progress, Some(json!({"position_seconds":4})), false)
        .await;
    assert_eq!(f.scan().await.phase, "completed");
    let after = f.items().await.remove(0);
    assert_eq!(after["id"], original["id"]);
    assert_eq!(after["file_id"], original["file_id"]);
    assert_eq!(after["edition_id"], eid);
    assert_eq!(after["edition_label"], "Broadcast cut");
    assert_eq!(after["title"], "Curated episode");
    assert_eq!(
        f.json("GET", &format!("/api/v1/catalog/items/{id}"), None, false)
            .await
            .1["media_type"],
        "episode"
    );
    assert_eq!(
        f.json("GET", &progress, None, false).await.1["position_seconds"],
        4.0
    );
    assert_eq!(
        f.json(
            "POST",
            &format!("/api/v1/items/{series_id}/editions"),
            Some(json!({"label":"Invalid"})),
            true
        )
        .await
        .0,
        400
    );
}

async fn upload_art(
    f: &Fixture,
    id: &str,
    source: &str,
    revision: i64,
    bytes: Vec<u8>,
    auth: bool,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/v1/items/{id}/artwork/poster/{source}?expected_revision={revision}"
        ))
        .header("host", "127.0.0.1:8787")
        .header("content-type", "application/octet-stream");
    if auth {
        request = request.header("authorization", "Bearer test-secret-token");
    }
    api::router(f.app.clone(), None)
        .oneshot(request.body(Body::from(bytes)).unwrap())
        .await
        .unwrap()
}
fn test_png(color: [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(32, 48, image::Rgb(color));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}
#[tokio::test]
async fn artwork_validates_bytes_conflicts_selection_and_immutable_delivery() {
    let f = Fixture::new().await;
    let (_, item) = f
        .json(
            "POST",
            "/api/v1/catalog/items",
            Some(json!({"title":"Movie","media_type":"movie"})),
            true,
        )
        .await;
    let id = item["id"].as_str().unwrap();
    let red = test_png([255, 0, 0]);
    let blue = test_png([0, 0, 255]);
    assert_eq!(
        upload_art(&f, id, "catabolic", 0, red.clone(), false)
            .await
            .status(),
        401
    );
    assert_eq!(
        upload_art(
            &f,
            id,
            "catabolic",
            0,
            b"<svg>not allowed</svg>".to_vec(),
            true
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        upload_art(&f, id, "catabolic", 0, red[..20].to_vec(), true)
            .await
            .status(),
        400
    );
    assert_eq!(
        upload_art(&f, id, "catabolic", 0, vec![0; 8 * 1024 * 1024 + 1], true)
            .await
            .status(),
        413
    );
    // Valid image above the default JSON body limit proves the route override works.
    let mut larger = red.clone();
    larger.extend(vec![0; 20_000]);
    assert_eq!(
        upload_art(&f, id, "catabolic", 0, larger.clone(), true)
            .await
            .status(),
        200
    );
    assert_eq!(
        upload_art(&f, id, "catabolic", 0, red.clone(), true)
            .await
            .status(),
        409
    );
    let a = f
        .json("GET", &format!("/api/v1/items/{id}/artwork"), None, false)
        .await
        .1;
    let asset = a["contributions"][0]["asset_id"].as_str().unwrap();
    assert_eq!(a["contributions"][0]["width"], 32);
    assert_eq!(a["selections"][0]["asset_id"], asset);
    assert_eq!(
        upload_art(&f, id, "other", 0, blue, true).await.status(),
        200
    );
    let conflict = f
        .json("GET", &format!("/api/v1/items/{id}/artwork"), None, false)
        .await
        .1;
    assert_eq!(conflict["selections"][0]["conflict"], true);
    let selection = format!("/api/v1/items/{id}/artwork-selection/poster");
    assert_eq!(
        f.json(
            "PUT",
            &selection,
            Some(json!({"expected_revision":0,"asset_id":"unknown"})),
            true
        )
        .await
        .0,
        400
    );
    let (status, pinned) = f
        .json(
            "PUT",
            &selection,
            Some(json!({"expected_revision":0,"asset_id":asset})),
            true,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(pinned["selections"][0]["asset_id"], asset);
    assert_eq!(
        upload_art(&f, id, "catabolic", 1, red, true).await.status(),
        200
    );
    let pinned = f
        .json("GET", &format!("/api/v1/items/{id}/artwork"), None, false)
        .await
        .1;
    assert_eq!(pinned["selections"][0]["asset_id"], asset);
    let response = f
        .response(
            "GET",
            &format!("/api/v1/artwork/{asset}/content"),
            None,
            &[],
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    let etag = response.headers()["etag"].to_str().unwrap().to_owned();
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        larger
    );
    assert_eq!(
        f.response(
            "GET",
            &format!("/api/v1/artwork/{asset}/content"),
            None,
            &[("if-none-match", &etag)]
        )
        .await
        .status(),
        304
    );
    assert_eq!(
        f.json(
            "PUT",
            &selection,
            Some(json!({"expected_revision":1,"asset_id":null})),
            true
        )
        .await
        .1["selections"][0]["conflict"],
        true
    );
    assert_eq!(
        f.json(
            "PUT",
            &selection,
            Some(json!({"expected_revision":1,"asset_id":null})),
            true
        )
        .await
        .0,
        409
    );
}

#[tokio::test]
async fn catalog_migration_preserves_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("migrations");
    std::fs::create_dir(&migrations).unwrap();
    for name in [
        "0001_catalog.sql",
        "0002_metadata.sql",
        "0003_renditions.sql",
    ] {
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("migrations")
                .join(name),
            migrations.join(name),
        )
        .unwrap();
    }
    let path = dir.path().join("old.sqlite");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO items VALUES ('old','Existing','video')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_origins VALUES ('old','Existing')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO editions VALUES ('edition','old','Original')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let upgraded = db::connect(&path).await.unwrap();
    let row: (String, i64) =
        sqlx::query_as("SELECT label,revision FROM editions WHERE id='edition'")
            .fetch_one(&upgraded)
            .await
            .unwrap();
    assert_eq!(row, ("Original".into(), 1));
    let title: String = sqlx::query_scalar("SELECT title FROM items WHERE id='old'")
        .fetch_one(&upgraded)
        .await
        .unwrap();
    assert_eq!(title, "Existing");
}

#[tokio::test]
async fn catalog_concurrent_edits_external_lookup_and_metadata_validation() {
    let f = Fixture::new().await;
    let (_, item) = f
        .json(
            "POST",
            "/api/v1/catalog/items",
            Some(json!({"title":"Movie","media_type":"movie"})),
            true,
        )
        .await;
    let id = item["id"].as_str().unwrap();
    let path = format!("/api/v1/items/{id}/structure");
    let update = json!({"expected_revision":1,"media_type":"unclassified"});
    let (a, b) = tokio::join!(
        f.json("PUT", &path, Some(update.clone()), true),
        f.json("PUT", &path, Some(update), true)
    );
    assert!((a.0 == 200 && b.0 == 409) || (a.0 == 409 && b.0 == 200));
    let path = format!("/api/v1/items/{id}/metadata/catabolic");
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"expected_revision":0,"values":{"release_date":"2025-02-29"},"tags":[]})),
            true
        )
        .await
        .0,
        400
    );
    assert_eq!(f.json("PUT",&path,Some(json!({"expected_revision":0,"external_id":"cat-1","values":{"release_date":"2024-02-29"},"tags":[]})),true).await.0,200);
    let found = f
        .json(
            "GET",
            "/api/v1/catalog/items?source=catabolic&external_id=cat-1",
            None,
            false,
        )
        .await
        .1;
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["id"], id);
    assert_eq!(
        f.json("GET", "/api/v1/catalog/items?source=catabolic", None, false)
            .await
            .0,
        400
    );
}

#[tokio::test]
async fn artwork_supported_formats_local_precedence_and_dimension_limits() {
    let f = Fixture::new().await;
    let (_, item) = f
        .json(
            "POST",
            "/api/v1/catalog/items",
            Some(json!({"title":"Movie","media_type":"movie"})),
            true,
        )
        .await;
    let id = item["id"].as_str().unwrap();
    for (source, format, mime) in [
        ("jpeg", image::ImageFormat::Jpeg, "image/jpeg"),
        ("webp", image::ImageFormat::WebP, "image/webp"),
    ] {
        let picture = image::RgbImage::from_pixel(32, 48, image::Rgb([0, 100, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        picture.write_to(&mut out, format).unwrap();
        let response = upload_art(&f, id, source, 0, out.into_inner(), true).await;
        assert_eq!(response.status(), 200);
        let data: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            data["contributions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["mime"] == mime)
        );
    }
    let response = upload_art(&f, id, "local", 0, test_png([255, 100, 0]), true).await;
    assert_eq!(response.status(), 200);
    let data: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let local = data["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["source"] == "local")
        .unwrap();
    assert_eq!(data["selections"][0]["asset_id"], local["asset_id"]);
    assert_eq!(data["selections"][0]["conflict"], false);
    let picture = image::RgbImage::from_pixel(8193, 1, image::Rgb([0, 0, 0]));
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).unwrap();
    assert_eq!(
        upload_art(&f, id, "large", 0, out.into_inner(), true)
            .await
            .status(),
        400
    );
}

async fn viewing_item(f: &Fixture) -> Value {
    std::fs::write(f.root.join("viewing.mp4"), b"synthetic viewing fixture").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let item = f.items().await.remove(0);
    sqlx::query("UPDATE media_files SET duration_seconds=100 WHERE id=?")
        .bind(item["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    item
}
fn session_request(item: &Value, revision: i64) -> Value {
    json!({"item_id":item["id"],"file_id":item["file_id"],"file_revision":item["revision"],"expected_revision":revision})
}
#[tokio::test]
async fn viewing_sessions_order_events_supersede_and_preserve_manual_overrides() {
    let f = Fixture::new().await;
    let item = viewing_item(&f).await;
    let id = item["id"].as_str().unwrap();
    let view_path = format!("/api/v1/profiles/default/viewing/{id}");
    let sessions = "/api/v1/profiles/default/playback-sessions";
    let initial = f.json("GET", &view_path, None, false).await.1;
    assert_eq!(initial["revision"], 0);
    assert_eq!(initial["watched"], false);
    let (status, a) = f
        .json("POST", sessions, Some(session_request(&item, 0)), false)
        .await;
    assert_eq!(status, 201);
    let a_path = format!("{sessions}/{}", a["id"].as_str().unwrap());
    let first = json!({"sequence":1,"position_seconds":30,"status":"playing"});
    assert_eq!(
        f.json("PUT", &a_path, Some(first.clone()), false).await.0,
        200
    );
    let revision = f.json("GET", &view_path, None, false).await.1["revision"].clone();
    assert_eq!(f.json("PUT", &a_path, Some(first), false).await.0, 200);
    assert_eq!(
        f.json("GET", &view_path, None, false).await.1["revision"],
        revision
    );
    assert_eq!(
        f.json(
            "PUT",
            &a_path,
            Some(json!({"sequence":1,"position_seconds":31,"status":"playing"})),
            false
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json(
            "PUT",
            &a_path,
            Some(json!({"sequence":2,"position_seconds":30,"status":"ended"})),
            false
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.json(
            "GET",
            "/api/v1/profiles/default/continue-watching",
            None,
            false
        )
        .await
        .1["total"],
        1
    );
    let (status, b) = f
        .json(
            "POST",
            sessions,
            Some(session_request(&item, revision.as_i64().unwrap())),
            false,
        )
        .await;
    assert_eq!(status, 201);
    assert_eq!(
        f.json(
            "POST",
            sessions,
            Some(session_request(&item, revision.as_i64().unwrap())),
            false
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json(
            "PUT",
            &a_path,
            Some(json!({"sequence":2,"position_seconds":99,"status":"playing"})),
            false
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json("GET", &a_path, None, false).await.1["status"],
        "superseded"
    );
    assert_eq!(
        f.json(
            "PUT",
            &format!("/api/v1/profiles/default/progress/{id}"),
            Some(json!({"position_seconds":90})),
            false
        )
        .await
        .0,
        409
    );
    let b_path = format!("{sessions}/{}", b["id"].as_str().unwrap());
    let end = json!({"sequence":1,"position_seconds":100,"status":"ended"});
    assert_eq!(
        f.json("PUT", &b_path, Some(end.clone()), false).await.0,
        200
    );
    assert_eq!(f.json("PUT", &b_path, Some(end), false).await.0, 200);
    assert_eq!(
        f.json(
            "GET",
            "/api/v1/profiles/default/continue-watching",
            None,
            false
        )
        .await
        .1["total"],
        0
    );
    let completed = f.json("GET", &view_path, None, false).await.1;
    assert_eq!(completed["automatic_watched"], true);
    let (status, manual) = f
        .json(
            "PUT",
            &view_path,
            Some(json!({"expected_revision":completed["revision"],"watched":false})),
            false,
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(manual["watched"], false);
    assert_eq!(
        f.json(
            "GET",
            "/api/v1/profiles/default/continue-watching",
            None,
            false
        )
        .await
        .1["total"],
        1
    );
    assert_eq!(
        f.json(
            "PUT",
            &b_path,
            Some(json!({"sequence":2,"position_seconds":0,"status":"playing"})),
            false
        )
        .await
        .0,
        409
    );
    let (_, c) = f
        .json(
            "POST",
            sessions,
            Some(session_request(&item, manual["revision"].as_i64().unwrap())),
            false,
        )
        .await;
    let c_path = format!("{sessions}/{}", c["id"].as_str().unwrap());
    f.json(
        "PUT",
        &c_path,
        Some(json!({"sequence":1,"position_seconds":100,"status":"ended"})),
        false,
    )
    .await;
    let still_manual = f.json("GET", &view_path, None, false).await.1;
    assert_eq!(still_manual["watched"], false);
    assert_eq!(
        f.json(
            "PUT",
            &view_path,
            Some(json!({"expected_revision":still_manual["revision"],"watched":null})),
            false
        )
        .await
        .1["watched"],
        true
    );
}

#[tokio::test]
async fn viewing_concurrent_updates_profile_isolation_and_source_revision_guards() {
    let f = Fixture::new().await;
    let item = viewing_item(&f).await;
    let sessions = "/api/v1/profiles/default/playback-sessions";
    let (_, session) = f
        .json("POST", sessions, Some(session_request(&item, 0)), false)
        .await;
    let path = format!("{sessions}/{}", session["id"].as_str().unwrap());
    let (a, b) = tokio::join!(
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":1,"position_seconds":20,"status":"playing"})),
            false
        ),
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":1,"position_seconds":40,"status":"playing"})),
            false
        )
    );
    assert!((a.0 == 200 && b.0 == 409) || (a.0 == 409 && b.0 == 200));
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":2,"position_seconds":5,"status":"paused"})),
            false
        )
        .await
        .0,
        200
    );
    let (_, other) = f
        .json(
            "POST",
            "/api/v1/profiles",
            Some(json!({"name":"Other"})),
            true,
        )
        .await;
    let profile = other["id"].as_str().unwrap();
    assert_eq!(
        f.json(
            "GET",
            &format!(
                "/api/v1/profiles/{profile}/playback-sessions/{}",
                session["id"].as_str().unwrap()
            ),
            None,
            false
        )
        .await
        .0,
        404
    );
    assert_eq!(
        f.json(
            "GET",
            &format!("/api/v1/profiles/{profile}/continue-watching"),
            None,
            false
        )
        .await
        .1["total"],
        0
    );
    assert_eq!(
        f.json(
            "POST",
            &format!("/api/v1/profiles/{profile}/playback-sessions"),
            Some(session_request(&item, 0)),
            false
        )
        .await
        .0,
        201
    );
    std::fs::write(f.root.join("viewing.mp4"), b"changed revision").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":3,"position_seconds":10,"status":"paused"})),
            false
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json(
            "GET",
            &format!(
                "/api/v1/profiles/default/viewing/{}",
                item["id"].as_str().unwrap()
            ),
            None,
            false
        )
        .await
        .1["position_seconds"],
        5.0
    );
    assert_eq!(
        f.json(
            "GET",
            "/api/v1/profiles/default/continue-watching?limit=0",
            None,
            false
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn playback_preferences_validate_revision_and_remain_profile_local() {
    let f = Fixture::new().await;
    let path = "/api/v1/profiles/default/playback-preferences";
    assert_eq!(f.json("GET", path, None, false).await.1["revision"], 0);
    let prefs = json!({"audio_languages":["en-US","ja"],"subtitle_languages":["en"],"subtitle_mode":"foreign_audio","quality":"original"});
    let body = json!({"expected_revision":0,"preferences":prefs});
    let (status, saved) = f.json("PUT", path, Some(body.clone()), false).await;
    assert_eq!(status, 200);
    assert_eq!(saved["preferences"]["audio_languages"][0], "en-us");
    assert_eq!(f.json("PUT", path, Some(body), false).await.0, 409);
    for bad in [
        json!({"audio_languages":["en","EN"],"subtitle_languages":[],"subtitle_mode":"off","quality":"auto"}),
        json!({"audio_languages":["en_uk"],"subtitle_languages":[],"subtitle_mode":"off","quality":"auto"}),
        json!({"audio_languages":[],"subtitle_languages":[],"subtitle_mode":"bad","quality":"auto"}),
    ] {
        assert_eq!(
            f.json(
                "PUT",
                path,
                Some(json!({"expected_revision":1,"preferences":bad})),
                false
            )
            .await
            .0,
            400
        );
    }
    let (_, other) = f
        .json(
            "POST",
            "/api/v1/profiles",
            Some(json!({"name":"Second"})),
            true,
        )
        .await;
    assert_eq!(
        f.json(
            "GET",
            &format!(
                "/api/v1/profiles/{}/playback-preferences",
                other["id"].as_str().unwrap()
            ),
            None,
            false
        )
        .await
        .1["preferences"]["quality"],
        "auto"
    );
}

#[tokio::test]
async fn next_episode_uses_explicit_order_watched_overrides_and_specials() {
    let f = Fixture::new().await;
    let (_, series) = f
        .json(
            "POST",
            "/api/v1/catalog/items",
            Some(json!({"title":"Series","media_type":"series"})),
            true,
        )
        .await;
    let mut episodes = Vec::new();
    for number in [0, 1, 2] {
        let (_,season)=f.json("POST","/api/v1/catalog/items",Some(json!({"title":"Season","media_type":"season","parent_id":series["id"],"number":number})),true).await;
        for episode in [1, 2] {
            let (_,e)=f.json("POST","/api/v1/catalog/items",Some(json!({"title":"Episode","media_type":"episode","parent_id":season["id"],"number":episode})),true).await;
            episodes.push(e);
        }
    }
    let lookup = |index: usize| {
        format!(
            "/api/v1/profiles/default/next-episode/{}",
            episodes[index]["id"].as_str().unwrap()
        )
    };
    let next = f.json("GET", &lookup(0), None, false).await.1;
    assert_eq!(next["next"]["item_id"], episodes[2]["id"]);
    assert_eq!(next["next"]["available"], false);
    assert_eq!(
        f.json(
            "GET",
            &format!("{}?include_specials=true", lookup(0)),
            None,
            false
        )
        .await
        .1["next"]["item_id"],
        episodes[1]["id"]
    );
    let watched = format!(
        "/api/v1/profiles/default/viewing/{}",
        episodes[3]["id"].as_str().unwrap()
    );
    f.json(
        "PUT",
        &watched,
        Some(json!({"expected_revision":0,"watched":true})),
        false,
    )
    .await;
    assert_eq!(
        f.json("GET", &lookup(2), None, false).await.1["next"]["item_id"],
        episodes[4]["id"]
    );
    assert_eq!(
        f.json(
            "GET",
            &format!("{}?skip_watched=false", lookup(2)),
            None,
            false
        )
        .await
        .1["next"]["item_id"],
        episodes[3]["id"]
    );
    assert!(f.json("GET", &lookup(5), None, false).await.1["next"].is_null());
    assert_eq!(
        f.json(
            "GET",
            &format!(
                "/api/v1/profiles/default/next-episode/{}",
                series["id"].as_str().unwrap()
            ),
            None,
            false
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.json(
            "GET",
            "/api/v1/profiles/missing/continue-watching",
            None,
            false
        )
        .await
        .0,
        404
    );
}

#[tokio::test]
async fn viewing_migration_preserves_legacy_positions_without_inventing_completion() {
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("migrations");
    std::fs::create_dir(&migrations).unwrap();
    for name in [
        "0001_catalog.sql",
        "0002_metadata.sql",
        "0003_renditions.sql",
        "0004_catalog_structure.sql",
    ] {
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("migrations")
                .join(name),
            migrations.join(name),
        )
        .unwrap();
    }
    let path = dir.path().join("old.sqlite");
    let pool = sqlx::SqlitePool::connect_with(
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
    sqlx::query("INSERT INTO items VALUES ('old','Existing','video')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO progress VALUES ('default','old',42,1)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let upgraded = db::connect(&path).await.unwrap();
    let row:(f64,bool,Option<bool>,i64,Option<String>)=sqlx::query_as("SELECT p.position_seconds,v.automatic_watched,v.manual_watched,v.revision,v.session_id FROM progress p JOIN viewing_state v ON v.profile_id=p.profile_id AND v.item_id=p.item_id WHERE p.item_id='old'").fetch_one(&upgraded).await.unwrap();
    assert_eq!(row, (42.0, false, None, 0, None));
}

#[tokio::test]
async fn rendition_sessions_survive_offline_original_but_reject_replaced_source() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("original.mp4"), b"original video").unwrap();
    std::fs::write(f.root.join("small.mp4"), b"small video").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let items = f.items().await;
    let original = items.iter().find(|i| i["title"] == "original").unwrap();
    let small = items.iter().find(|i| i["title"] == "small").unwrap();
    let sessions = "/api/v1/profiles/default/playback-sessions";
    let request = json!({"item_id":original["id"],"file_id":small["file_id"],"file_revision":small["revision"],"expected_revision":0});
    assert_eq!(
        f.json("POST", sessions, Some(request.clone()), false)
            .await
            .0,
        404
    );
    assert_eq!(f.json("PUT",&format!("/api/v1/items/{}/renditions/test/small",original["id"].as_str().unwrap()),Some(json!({"file_id":small["file_id"],"file_revision":small["revision"],"source_file_id":original["file_id"],"source_revision":original["revision"],"label":"Small","recipe":{}})),true).await.0,200);
    std::fs::remove_file(f.root.join("original.mp4")).unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    let (status, s) = f.json("POST", sessions, Some(request), false).await;
    assert_eq!(status, 201);
    let path = format!("{sessions}/{}", s["id"].as_str().unwrap());
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":1,"position_seconds":1,"status":"playing"})),
            false
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.json(
            "GET",
            "/api/v1/profiles/default/continue-watching",
            None,
            false
        )
        .await
        .1["items"][0]["available"],
        true
    );
    std::fs::write(f.root.join("original.mp4"), b"replaced original").unwrap();
    assert_eq!(f.scan().await.phase, "completed");
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":2,"position_seconds":2,"status":"playing"})),
            false
        )
        .await
        .0,
        409
    );
}

#[tokio::test]
async fn readiness_tracks_worker_database_and_shutdown_and_diagnostics_are_private() {
    use std::sync::atomic::Ordering;
    let f = Fixture::new().await;
    let (status, state) = f.json("GET", "/ready", None, false).await;
    assert_eq!(status, 503);
    assert_eq!(state["database"], true);
    assert_eq!(state["worker"], false);
    f.app.health.worker_running.store(true, Ordering::Relaxed);
    assert_eq!(f.json("GET", "/ready", None, false).await.0, 200);
    assert_eq!(
        f.json("GET", "/api/v1/admin/diagnostics", None, false)
            .await
            .0,
        401
    );
    let (status, diagnostics) = f.json("GET", "/api/v1/admin/diagnostics", None, true).await;
    assert_eq!(status, 200);
    assert_eq!(diagnostics["libraries"], 1);
    assert_eq!(diagnostics["files"], 0);
    assert_eq!(diagnostics["ffprobe_available"], true);
    assert_eq!(diagnostics["demuxe_present_at_startup"], false);
    assert!(!diagnostics.to_string().contains(f.root.to_str().unwrap()));
    assert!(!diagnostics.to_string().contains("test-secret-token"));
    f.app.health.shutting_down.store(true, Ordering::Relaxed);
    assert_eq!(f.json("GET", "/ready", None, false).await.0, 503);
    f.app.health.shutting_down.store(false, Ordering::Relaxed);
    f.app.db.close().await;
    let (status, state) = f.json("GET", "/ready", None, false).await;
    assert_eq!(status, 503);
    assert_eq!(state["database"], false);
}

#[tokio::test]
async fn legacy_pages_remain_consistent_during_title_changes() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("match.mp4"), b"fixture").unwrap();
    f.scan().await;
    let pool = f.app.db.clone();
    let writer = tokio::spawn(async move {
        for i in 0..600 {
            sqlx::query("UPDATE items SET title=?")
                .bind(if i % 2 == 0 { "match" } else { "other" })
                .execute(&pool)
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    });
    for _ in 0..600 {
        let (status, page) = f.json("GET", "/api/v1/items?q=match", None, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            page["total"].as_u64().unwrap() as usize,
            page["items"].as_array().unwrap().len()
        );
    }
    writer.await.unwrap();
}

#[tokio::test]
async fn canonical_browser_origin_passes_boundary_but_foreign_origin_does_not() {
    let mut f = Fixture::new().await;
    f.app.origin =
        Arc::new(playscale::config::canonical_origin("https://EXAMPLE.com:443/").unwrap());
    f.app.authority = Arc::new("example.com".into());
    for (host, origin, expected) in [
        ("example.com", "https://example.com", StatusCode::NOT_FOUND),
        ("evil.example", "https://example.com", StatusCode::FORBIDDEN),
        ("example.com", "https://evil.example", StatusCode::FORBIDDEN),
        (
            "example.com",
            "https://example.com:8443",
            StatusCode::FORBIDDEN,
        ),
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/missing")
            .header("host", host)
            .header("origin", origin)
            .body(Body::empty())
            .unwrap();
        let response = api::router(f.app.clone(), None)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn media_admission_precedes_filesystem_access() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("clip.mp4"), b"fixture").unwrap();
    f.scan().await;
    let item = f.items().await.remove(0);
    let _slots = f.app.streams.acquire_many(2).await.unwrap();
    std::fs::rename(&f.root, f.dir.path().join("offline")).unwrap();
    for method in ["GET", "HEAD"] {
        let response = f
            .response(method, item["media_url"].as_str().unwrap(), None, &[])
            .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

#[tokio::test]
async fn processing_admission_idempotency_cancellation_and_recovery() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("clip.mp4"), b"fixture").unwrap();
    f.scan().await;
    sqlx::query("UPDATE media_files SET duration_seconds=10")
        .execute(&f.app.db)
        .await
        .unwrap();
    let item = f.items().await.remove(0);
    let request = json!({"source_file_id":item["file_id"],"source_revision":item["revision"],"recipe":"h264720p","backend":"software","idempotency_key":"replay"});
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/processing-jobs",
            Some(request.clone()),
            false
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (a, b) = tokio::join!(
        f.json(
            "POST",
            "/api/v1/processing-jobs",
            Some(request.clone()),
            true
        ),
        f.json(
            "POST",
            "/api/v1/processing-jobs",
            Some(request.clone()),
            true
        )
    );
    assert_eq!(a.1["id"], b.1["id"]);
    assert!([a.0, b.0].contains(&StatusCode::CREATED));
    assert!([a.0, b.0].contains(&StatusCode::OK));
    let mut changed = request.clone();
    changed["recipe"] = json!("audio_aac");
    assert_eq!(
        f.json("POST", "/api/v1/processing-jobs", Some(changed), true)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let id = a.1["id"].as_str().unwrap();
    let control = format!("/api/v1/processing-jobs/{id}/control");
    assert_eq!(
        f.json("POST", &control, Some(json!({"action":"cancel"})), true)
            .await
            .1["phase"],
        "cancelled"
    );
    assert_eq!(
        f.json("POST", &control, Some(json!({"action":"retry"})), true)
            .await
            .1["phase"],
        "queued"
    );
    sqlx::query("UPDATE processing_jobs SET phase='running',attempt=1")
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::processing::recover(&f.app).await.unwrap();
    assert_eq!(
        playscale::processing::load(&f.app, id).await.unwrap().phase,
        "queued"
    );
    sqlx::query("UPDATE processing_jobs SET phase='cancelling',attempt=2")
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::processing::recover(&f.app).await.unwrap();
    let row = playscale::processing::load(&f.app, id).await.unwrap();
    assert_eq!(row.phase, "cancelled");
    assert_eq!(row.attempt, 2);
}

#[tokio::test]
async fn events_are_transactional_replayable_and_bounded_by_subscriber_capacity() {
    let f = Fixture::new().await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    let mut tx = f.app.db.begin().await.unwrap();
    sqlx::query("INSERT INTO items (id,title,kind) VALUES ('rolled-back','No event','video')")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(before, after);
    sqlx::query("INSERT INTO items (id,title,kind) VALUES ('committed','Published','video')")
        .execute(&f.app.db)
        .await
        .unwrap();
    let response = f
        .response("GET", &format!("/api/v1/events?after={before}"), None, &[])
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    let text = String::from_utf8(frame.to_vec()).unwrap();
    assert!(text.contains("catalog") && text.contains("committed"));
    let second = f.response("GET", "/api/v1/events", None, &[]).await;
    assert_eq!(
        f.response("GET", "/api/v1/events", None, &[])
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(body);
    drop(second);
    assert_eq!(f.app.event_streams.available_permits(), 2);
    assert_eq!(
        f.response("GET", "/api/v1/events?after=-1", None, &[])
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut response = f
        .response("GET", "/api/v1/events?after=99999", None, &[])
        .await
        .into_body();
    let frame = response
        .frame()
        .await
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("cursor_expired"));
}

#[tokio::test]
async fn schedules_enqueue_once_and_cache_maintenance_keeps_originals() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("clip.mp4"), b"original").unwrap();
    f.scan().await;
    let path = format!("/api/v1/admin/scan-schedules/{}", f.library);
    assert_eq!(
        f.json("PUT", &path, Some(json!({"interval_seconds":30})), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.json("PUT", &path, Some(json!({"interval_seconds":60})), true)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE scan_schedules SET next_run=0")
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE phase='queued'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.json("PUT", &path, Some(json!({"interval_seconds":0})), true)
        .await;
    assert!(
        f.json("GET", "/api/v1/admin/scan-schedules", None, true)
            .await
            .1
            .as_array()
            .unwrap()
            .is_empty()
    );
    let file = f.items().await.remove(0);
    let id = playscale::new_id();
    sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at) VALUES (?,?,?,'h264720p','software',?,'failed',0,0)").bind(&id).bind(file["file_id"].as_str().unwrap()).bind(file["revision"].as_str().unwrap()).bind(&id).execute(&f.app.db).await.unwrap();
    let attempt = f.app.processing.root.join(&id).join("1");
    std::fs::create_dir_all(&attempt).unwrap();
    std::fs::write(attempt.join("partial.mp4"), b"partial").unwrap();
    let untouched = f.app.processing.root.join("unowned");
    std::fs::write(&untouched, b"keep").unwrap();
    assert_eq!(playscale::maintenance::clean(&f.app).await.unwrap(), 1);
    assert!(!attempt.exists());
    assert!(untouched.exists());
    assert_eq!(std::fs::read(f.root.join("clip.mp4")).unwrap(), b"original");
}

#[tokio::test]
async fn generated_same_item_sessions_still_depend_on_source_revision() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("original.mp4"), b"original").unwrap();
    std::fs::write(f.root.join("output.mp4"), b"output").unwrap();
    f.scan().await;
    let rows = f.items().await;
    let source = rows.iter().find(|v| v["title"] == "original").unwrap();
    let output = rows.iter().find(|v| v["title"] == "output").unwrap();
    sqlx::query("UPDATE media_files SET generated=1,edition_id=? WHERE id=?")
        .bind(source["edition_id"].as_str().unwrap())
        .bind(output["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/items/{}/renditions/playscale/generated",
        source["id"].as_str().unwrap()
    );
    let(status,_)=f.json("PUT",&path,Some(json!({"file_id":output["file_id"],"file_revision":output["revision"],"source_file_id":source["file_id"],"source_revision":source["revision"],"label":"Generated","recipe":{}})),true).await;
    assert_eq!(status, StatusCode::OK);
    let start = json!({"item_id":source["id"],"file_id":output["file_id"],"file_revision":output["revision"],"expected_revision":0});
    let (status, session) = f
        .json(
            "POST",
            "/api/v1/profiles/default/playback-sessions",
            Some(start.clone()),
            false,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    sqlx::query("UPDATE media_files SET revision='changed' WHERE id=?")
        .bind(source["file_id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/profiles/default/playback-sessions/{}",
        session["id"].as_str().unwrap()
    );
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"sequence":1,"position_seconds":0,"status":"playing"})),
            false
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut start = start;
    start["expected_revision"] = json!(1);
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/profiles/default/playback-sessions",
            Some(start),
            false
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn event_retention_prunes_old_cursors() {
    let f = Fixture::new().await;
    sqlx::query("WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<10005) INSERT INTO change_events(topic,resource_id) SELECT 'catalog',cast(v AS TEXT) FROM n").execute(&f.app.db).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 10000);
    let mut body = f
        .response("GET", "/api/v1/events?after=1", None, &[])
        .await
        .into_body();
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(String::from_utf8_lossy(&frame).contains("cursor_expired"));
}

#[path = "support/playback_planner.rs"]
mod playback_planner_tests;

#[tokio::test]
async fn incremental_scan_reuses_unchanged_files_but_full_scan_reprobes() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("one.mp4"), b"fixture").unwrap();
    let first = f.scan().await;
    assert_eq!((first.inspected_files, first.reused_files), (1, 0));
    let item = f.items().await.remove(0);
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM change_events WHERE topic='catalog'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    let second = f.scan().await;
    if cfg!(unix) {
        assert_eq!((second.inspected_files, second.reused_files), (0, 1));
    }
    assert_eq!(f.items().await[0]["revision"], item["revision"]);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events WHERE topic='catalog'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(events, after);
    let full = f.scan_mode(true).await;
    assert_eq!((full.inspected_files, full.reused_files), (1, 0));
    std::fs::write(f.root.join("one.mp4"), b"changed").unwrap();
    assert_eq!(f.scan().await.inspected_files, 1);
    assert_ne!(f.items().await[0]["revision"], item["revision"]);
}

#[tokio::test]
async fn library_administration_preserves_identity_bytes_and_progress() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("one.mp4"), b"original bytes").unwrap();
    f.scan().await;
    let item = f.items().await.remove(0);
    let progress = format!(
        "/api/v1/profiles/default/progress/{}",
        item["id"].as_str().unwrap()
    );
    assert_eq!(
        f.json(
            "PUT",
            &progress,
            Some(json!({"position_seconds":42})),
            false
        )
        .await
        .0,
        200
    );
    let path = format!("/api/v1/admin/libraries/{}", f.library);
    let rename = json!({"expected_revision":0,"name":"Renamed"});
    assert_eq!(
        f.json("PUT", &path, Some(rename.clone()), false).await.0,
        401
    );
    assert_eq!(
        f.json("PUT", &path, Some(rename.clone()), true).await.0,
        200
    );
    assert_eq!(f.json("PUT", &path, Some(rename), true).await.0, 409);
    db::add_library(&f.app.db, "Old startup name", &f.root)
        .await
        .unwrap();
    assert_eq!(
        f.json("GET", "/api/v1/admin/libraries", None, true).await.1[0]["name"],
        "Renamed"
    );
    let candidate = f.dir.path().join("new-root");
    std::fs::create_dir(&candidate).unwrap();
    std::fs::write(candidate.join("one.mp4"), b"different bytes").unwrap();
    let relocate = json!({"expected_revision":1,"root":candidate});
    assert_eq!(
        f.json(
            "POST",
            &format!("{path}/relocate"),
            Some(relocate.clone()),
            true
        )
        .await
        .0,
        400
    );
    assert_eq!(f.items().await[0]["available"], true);
    std::fs::copy(f.root.join("one.mp4"), candidate.join("one.mp4")).unwrap();
    let (status, moved) = f
        .json("POST", &format!("{path}/relocate"), Some(relocate), true)
        .await;
    assert_eq!(status, 200, "{moved}");
    assert_eq!(moved["revision"], 2);
    assert_eq!(f.items().await[0]["file_id"], item["file_id"]);
    assert_eq!(f.items().await[0]["revision"], item["revision"]);
    let queued = db::enqueue(&f.app, &f.library).await.unwrap();
    assert_eq!(
        f.json(
            "POST",
            &format!("{path}/detach"),
            Some(json!({"expected_revision":2})),
            true
        )
        .await
        .0,
        409
    );
    sqlx::query("UPDATE jobs SET phase='cancelled' WHERE id=?")
        .bind(queued.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        f.json(
            "POST",
            &format!("{path}/detach"),
            Some(json!({"expected_revision":2})),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(f.items().await[0]["available"], false);
    assert!(db::enqueue(&f.app, &f.library).await.is_err());
    assert_eq!(
        std::fs::read(f.root.join("one.mp4")).unwrap(),
        b"original bytes"
    );
    assert_eq!(
        std::fs::read(candidate.join("one.mp4")).unwrap(),
        b"original bytes"
    );
    assert_eq!(
        f.json(
            "POST",
            &format!("{path}/relocate"),
            Some(json!({"expected_revision":3,"root":candidate})),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(f.items().await[0]["available"], true);
    assert_eq!(
        f.json("GET", &progress, None, false).await.1["position_seconds"],
        42.0
    );
    assert_eq!(
        f.json(
            "POST",
            &format!("{path}/relocate"),
            Some(json!({"expected_revision":4,"root":f.app.storage.root})),
            true
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn snapshots_are_validated_retained_and_disk_pressure_is_reported() {
    let mut f = Fixture::new().await;
    let settings = playscale::storage::Settings {
        backups_keep: 1,
        min_free_bytes: 0,
        ..Default::default()
    };
    f.app.storage = Arc::new(playscale::storage::Runtime::new(
        f.app.storage.root.clone(),
        settings,
    ));
    let item = viewing_item(&f).await;
    let root = f.app.storage.root.join("backups");
    std::fs::create_dir_all(root.join("manual-keep")).unwrap();
    std::fs::write(root.join("manual-keep/sentinel"), b"keep").unwrap();
    for _ in 0..2 {
        let (status, manifest) = f.json("POST", "/api/v1/admin/storage", None, true).await;
        assert_eq!(status, 201, "{manifest}");
        assert_eq!(manifest["format"], 1);
        assert_eq!(
            manifest["schema_versions"].as_array().unwrap().last(),
            Some(&json!(latest_migration()))
        );
    }
    let snapshots: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| e.file_name().to_string_lossy().starts_with("auto-"))
        .collect();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        std::fs::read(root.join("manual-keep/sentinel")).unwrap(),
        b"keep"
    );
    let snapshot = &snapshots[0].path();
    let bytes = std::fs::read(snapshot.join("playscale.sqlite3")).unwrap();
    use sha2::{Digest, Sha256};
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(snapshot.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["sha256"], format!("{:x}", Sha256::digest(bytes)));
    let restored = db::connect(&snapshot.join("playscale.sqlite3"))
        .await
        .unwrap();
    let id: String = sqlx::query_scalar("SELECT id FROM items")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(id, item["id"].as_str().unwrap());
    restored.close().await;
    f.app.storage = Arc::new(playscale::storage::Runtime::new(
        f.app.storage.root.clone(),
        playscale::storage::Settings {
            min_free_bytes: i64::MAX as u64,
            ..Default::default()
        },
    ));
    assert_eq!(
        f.json("POST", "/api/v1/admin/storage", None, true).await.0,
        503
    );
    let error: Option<String> =
        sqlx::query_scalar("SELECT error FROM maintenance_state WHERE name='backup'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert!(error.unwrap().contains("backup_failed"));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
}

#[tokio::test]
async fn profile_administration_requires_revision_and_preserves_other_profiles() {
    let f = Fixture::new().await;
    let item = viewing_item(&f).await;
    let (_, profile) = f
        .json(
            "POST",
            "/api/v1/profiles",
            Some(json!({"name":"Temporary"})),
            true,
        )
        .await;
    let id = profile["id"].as_str().unwrap();
    let path = format!("/api/v1/admin/profiles/{id}");
    let progress = format!(
        "/api/v1/profiles/{id}/progress/{}",
        item["id"].as_str().unwrap()
    );
    assert_eq!(
        f.json(
            "PUT",
            &progress,
            Some(json!({"position_seconds":17})),
            false
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.json(
            "PUT",
            &path,
            Some(json!({"expected_revision":0,"name":"Renamed"})),
            true
        )
        .await
        .0,
        200
    );
    assert_eq!(
        f.json("GET", &progress, None, false).await.1["position_seconds"],
        17.0
    );
    assert_eq!(
        f.json("DELETE", &format!("{path}?expected_revision=0"), None, true)
            .await
            .0,
        409
    );
    assert_eq!(
        f.response(
            "DELETE",
            &format!("{path}?expected_revision=1"),
            None,
            &[("authorization", "Bearer test-secret-token")]
        )
        .await
        .status(),
        204
    );
    assert_eq!(
        f.json(
            "DELETE",
            "/api/v1/admin/profiles/default?expected_revision=0",
            None,
            true
        )
        .await
        .0,
        409
    );
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    assert_eq!(f.items().await.len(), 1);
}

#[tokio::test]
async fn retention_keeps_authoritative_session_and_legacy_write_barrier() {
    let f = Fixture::new().await;
    let item = viewing_item(&f).await;
    let sessions = "/api/v1/profiles/default/playback-sessions";
    let (_, old) = f
        .json("POST", sessions, Some(session_request(&item, 0)), false)
        .await;
    let view_path = format!(
        "/api/v1/profiles/default/viewing/{}",
        item["id"].as_str().unwrap()
    );
    let revision = f.json("GET", &view_path, None, false).await.1["revision"]
        .as_i64()
        .unwrap();
    let (status, latest) = f
        .json(
            "POST",
            sessions,
            Some(session_request(&item, revision)),
            false,
        )
        .await;
    assert_eq!(status, 201);
    let latest_path = format!("{sessions}/{}", latest["id"].as_str().unwrap());
    assert_eq!(
        f.json(
            "PUT",
            &latest_path,
            Some(json!({"sequence":1,"position_seconds":25,"status":"stopped"})),
            false
        )
        .await
        .0,
        200
    );
    sqlx::query("UPDATE playback_sessions SET updated_at=1")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET created_at=1 WHERE phase='completed'")
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::storage::prune_history(&f.app).await.unwrap();
    assert_eq!(
        f.json(
            "GET",
            &format!("{sessions}/{}", old["id"].as_str().unwrap()),
            None,
            false
        )
        .await
        .0,
        404
    );
    assert_eq!(f.json("GET", &latest_path, None, false).await.0, 200);
    assert_eq!(
        f.json(
            "PUT",
            &format!(
                "/api/v1/profiles/default/progress/{}",
                item["id"].as_str().unwrap()
            ),
            Some(json!({"position_seconds":1})),
            false
        )
        .await
        .0,
        409
    );
    assert_eq!(
        f.json("GET", &view_path, None, false).await.1["position_seconds"],
        25.0
    );
}

#[tokio::test]
async fn verified_registration_holds_admission_and_does_not_resolve_root_twice() {
    let f = Fixture::new().await;
    let root = f.dir.path().join("candidate");
    std::fs::create_dir(&root).unwrap();
    let inspected = playscale::administration::candidate_root(&f.app, root.clone())
        .await
        .unwrap();
    assert!(
        f.app
            .storage
            .library_io
            .clone()
            .try_acquire_owned()
            .is_err()
    );
    // Replace the pathname after inspection. Registration must retain the inspected
    // identity; a subsequent scan must fail instead of adopting this replacement.
    std::fs::rename(&root, f.dir.path().join("original-candidate")).unwrap();
    std::fs::create_dir(&root).unwrap();
    let lock = f.app.jobs.lock().await;
    let app = f.app.clone();
    let task = tokio::spawn(async move {
        playscale::administration::register_root(&app, inspected, Some("Candidate")).await
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert!(
        f.app
            .storage
            .library_io
            .clone()
            .try_acquire_owned()
            .is_err()
    );
    drop(lock);
    let library = task.await.unwrap().unwrap();
    assert!(f.app.storage.library_io.clone().try_acquire_owned().is_ok());
    let stored: String = sqlx::query_scalar("SELECT root_identity FROM libraries WHERE id=?")
        .bind(&library.id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_ne!(
        stored,
        db::root_identity(&std::fs::metadata(&root).unwrap())
    );
    let mut job = db::enqueue(&f.app, &library.id).await.unwrap();
    job.phase = "running".into();
    assert!(
        scan::run_scan(&f.app, &job, &CancellationToken::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disk_pressure_still_reclaims_old_owned_partial_snapshots() {
    let mut f = Fixture::new().await;
    f.app.storage = Arc::new(playscale::storage::Runtime::new(
        f.app.storage.root.clone(),
        playscale::storage::Settings {
            min_free_bytes: i64::MAX as u64,
            backups_keep: 1,
            ..Default::default()
        },
    ));
    let backups = f.app.storage.root.join("backups");
    for name in [
        "auto-old.partial",
        "auto-young.partial",
        "unowned.partial",
        "auto-complete-1",
        "auto-complete-2",
        "auto-complete-3",
    ] {
        let path = backups.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("bytes"), b"keep unless stale owned partial").unwrap();
        if name != "unowned.partial" {
            std::fs::write(path.join(".playscale-backup"), b"playscale-scheduled-v1").unwrap();
        }
        if name.starts_with("auto-complete") {
            std::fs::write(
                path.join("manifest.json"),
                serde_json::to_vec(&json!({
                    "format":1,"created_at":1,"sha256":"fixture","bytes":1,"schema_versions":[8]
                }))
                .unwrap(),
            )
            .unwrap();
        }
        if name != "auto-young.partial" {
            let old = std::time::SystemTime::now() - Duration::from_secs(86460);
            std::fs::File::open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
    }
    assert_eq!(
        f.json("POST", "/api/v1/admin/storage", None, true).await.0,
        503
    );
    assert!(!backups.join("auto-old.partial").exists());
    for name in [
        "auto-young.partial",
        "unowned.partial",
        "auto-complete-1",
        "auto-complete-2",
        "auto-complete-3",
    ] {
        assert!(backups.join(name).join("bytes").exists());
    }
}

#[path = "support/video_profiles.rs"]
mod video_profiles_tests;

#[tokio::test]
async fn scheduled_admission_rolls_back_with_schedule_and_serializes_ticks() {
    let f = Fixture::new().await;
    sqlx::query("INSERT INTO scan_schedules VALUES (?,60,0)")
        .bind(&f.library)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER reject_schedule BEFORE UPDATE ON scan_schedules BEGIN SELECT RAISE(ABORT,'injected schedule failure'); END").execute(&f.app.db).await.unwrap();
    assert!(playscale::maintenance::tick(&f.app).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "admitted job must roll back when schedule advancement fails"
    );
    let next: i64 = sqlx::query_scalar("SELECT next_run FROM scan_schedules")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(next, 0);
    sqlx::query("DROP TRIGGER reject_schedule")
        .execute(&f.app.db)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        playscale::maintenance::tick(&f.app),
        playscale::maintenance::tick(&f.app)
    );
    a.unwrap();
    b.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn scan_publication_rolls_back_catalog_when_terminal_write_fails() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("clip.mp4"), b"fixture").unwrap();
    let job = db::enqueue(&f.app, &f.library).await.unwrap();
    sqlx::query("UPDATE jobs SET phase='running',attempt=1 WHERE id=?")
        .bind(&job.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let job = db::get_job(&f.app.db, &job.id).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_completion BEFORE UPDATE OF phase ON jobs WHEN NEW.phase='completed' BEGIN SELECT RAISE(ABORT,'injected completion failure'); END").execute(&f.app.db).await.unwrap();
    assert!(
        scan::run_scan(&f.app, &job, &CancellationToken::new())
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_files")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        db::get_job(&f.app.db, &job.id).await.unwrap().phase,
        "running"
    );
    sqlx::query("DROP TRIGGER reject_completion")
        .execute(&f.app.db)
        .await
        .unwrap();
    scan::run_scan(&f.app, &job, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        db::get_job(&f.app.db, &job.id).await.unwrap().phase,
        "completed"
    );
}

#[tokio::test]
async fn exhausted_jobs_do_not_stop_workers_or_block_following_work() {
    let f = Fixture::new().await;
    std::fs::write(f.root.join("clip.mp4"), b"fixture").unwrap();
    let exhausted = db::enqueue(&f.app, &f.library).await.unwrap();
    sqlx::query("UPDATE jobs SET attempt=? WHERE id=?")
        .bind(i64::from(u32::MAX))
        .bind(&exhausted.id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(scan::worker(f.app.clone(), stop.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while db::get_job(&f.app.db, &exhausted.id).await.unwrap().phase != "failed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let next = db::enqueue(&f.app, &f.library).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while db::get_job(&f.app.db, &next.id).await.unwrap().phase != "completed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    task.await.unwrap().unwrap();
    let file = f.items().await.remove(0);
    for (id, attempt, created) in [("exhausted", i64::from(u32::MAX), 0), ("following", 0, 1)] {
        sqlx::query("INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,attempt,created_at,updated_at) VALUES (?,?,?,'h264720p','software',?,'queued',?,?,0)")
            .bind(id).bind(file["file_id"].as_str().unwrap()).bind(file["revision"].as_str().unwrap()).bind(id).bind(attempt).bind(created).execute(&f.app.db).await.unwrap();
    }
    // A sentinel directory proves the unadmitted attempt never reaches cleanup.
    let sentinel = f
        .app
        .processing
        .root
        .join("exhausted")
        .join(u32::MAX.to_string());
    std::fs::create_dir_all(&sentinel).unwrap();
    std::fs::write(sentinel.join("keep"), b"untouched").unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(playscale::processing::worker(f.app.clone(), stop.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = playscale::processing::load(&f.app, "following")
                .await
                .unwrap();
            if row.attempt == 1 && row.phase == "failed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.cancel();
    task.await.unwrap().unwrap();
    let row = playscale::processing::load(&f.app, "exhausted")
        .await
        .unwrap();
    assert_eq!(row.phase, "failed");
    assert_eq!(row.attempt, i64::from(u32::MAX));
    assert!(sentinel.join("keep").exists());
    assert_eq!(
        f.json(
            "POST",
            "/api/v1/processing-jobs/exhausted/control",
            Some(json!({"action":"retry"})),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

/// Highest migration version shipped in `migrations/`.
fn latest_migration() -> i64 {
    std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().to_str()?.get(..4)?.parse().ok())
        .max()
        .unwrap()
}
