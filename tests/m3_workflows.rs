//! Lane N1: the ordinary M3 library path through the real Topcoat facade and
//! the public v2 API, on real SQLite, a real scan and real media. Each change
//! is submitted with the validator, idempotency rules and body the rendered
//! command form carries, standing in for the bridge (whose transport rules
//! are tested in packages/ui-bridge). The server decides every rule; this
//! proves the pages reach those decisions and show their results.
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db, processing, scan};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use std::{path::Path, process::Command, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const OPERATOR: &str = "operator-secret-token-0123456789abcdef";

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

fn tools_available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn ffmpeg(args: &[&str], out: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(out)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Decode the HTML attribute escaping Topcoat applies.
fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The rendered command form for `command` whose markup contains `marker`.
struct Form {
    markup: String,
    if_match: Option<String>,
    json_body: Option<Value>,
}
impl Form {
    /// The rendered value of the field named `name` in this form.
    fn value(&self, name: &str) -> String {
        let at = self
            .markup
            .find(&format!("name=\"{name}\""))
            .unwrap_or_else(|| panic!("no field {name}: {}", self.markup));
        let rest = &self.markup[at..];
        let start = rest.find("value=\"").unwrap() + 7;
        unescape(&rest[start..start + rest[start..].find('"').unwrap()])
    }
}
fn form(html: &str, command: &str, marker: &str) -> Form {
    let needle = format!("data-command=\"{command}\"");
    let found = html
        .match_indices(&needle)
        .map(|(at, _)| {
            let start = html[..at].rfind("<form").unwrap();
            let end = at + html[at..].find("</form>").unwrap();
            &html[start..end]
        })
        .find(|f| f.contains(marker))
        .unwrap_or_else(|| panic!("no form {command} with {marker}: {html}"));
    let attr = |name: &str| {
        let key = format!("{name}=\"");
        found.find(&key).map(|at| {
            let rest = &found[at + key.len()..];
            unescape(&rest[..rest.find('"').unwrap()])
        })
    };
    Form {
        markup: found.to_owned(),
        if_match: attr("data-if-match"),
        json_body: found.contains("data-json-body").then(|| {
            let at = found.find("data-json-body").unwrap();
            let rest = &found[at..];
            let start = rest.find("value=\"").unwrap() + 7;
            serde_json::from_str(&unescape(
                &rest[start..start + rest[start..].find('"').unwrap()],
            ))
            .unwrap()
        }),
    }
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for d in ["legacy", "films", "state", "cache"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        // Encoders run under the real server binary's supervisor mode.
        let mut runtime = processing::Runtime::new(dir.path().join("cache"), Default::default());
        runtime.supervisor = env!("CARGO_BIN_EXE_playscale").into();
        db::add_library(&db, "Legacy", &dir.path().join("legacy"))
            .await
            .unwrap();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            db,
            admin_token: Arc::new(OPERATOR.into()),
            origin: Arc::new("http://127.0.0.1:8787".into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(2)),
            event_streams: Arc::new(Semaphore::new(4)),
            storage: Arc::new(playscale::storage::Runtime::new(
                dir.path().join("state"),
                Default::default(),
            )),
            processing: Arc::new(runtime),
            access: Arc::new(playscale::v2::Runtime::new(
                AccessMode::Restricted,
                playscale::v2::auth::random_key(),
            )),
        };
        Self { dir, app }
    }

    fn router(&self) -> axum::Router {
        api::router_with(
            self.app.clone(),
            None,
            Some(playscale::presentation::router(self.app.clone())),
        )
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Body>,
        content_type: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8787");
        if body.is_some() {
            request = request.header("content-type", content_type);
        }
        for (k, v) in headers {
            request = request.header(*k, *v);
        }
        let response = self
            .router()
            .oneshot(request.body(body.unwrap_or_default()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, bytes.to_vec())
    }

    async fn call(
        &self,
        auth: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        // An empty credential means an unauthenticated call (pairing).
        let mut all = if auth.is_empty() {
            vec![]
        } else {
            vec![("authorization", auth)]
        };
        all.extend_from_slice(headers);
        let (status, headers, bytes) = self
            .send(
                method,
                path,
                body.map(|b| Body::from(b.to_string())),
                "application/json",
                &all,
            )
            .await;
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn page(&self, auth: &str, path: &str) -> (StatusCode, String) {
        let (status, _, bytes) = self
            .send("GET", path, None, "", &[("authorization", auth)])
            .await;
        (status, String::from_utf8(bytes).unwrap())
    }

    async fn ok_page(&self, auth: &str, path: &str) -> String {
        let (status, html) = self.page(auth, path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {html}");
        html
    }

    /// Run the scan worker until the scan reaches a terminal state.
    async fn finish_scan(&self, auth: &str, scan_id: &str) -> Value {
        let stop = CancellationToken::new();
        let task = tokio::spawn(scan::worker(self.app.clone(), stop.clone()));
        let done = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let reply = self
                    .call(auth, "GET", &format!("/api/v2/scans/{scan_id}"), None, &[])
                    .await;
                if !["queued", "running"].contains(&reply.body["status"].as_str().unwrap()) {
                    break reply.body;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("scan finished");
        stop.cancel();
        task.await.unwrap().unwrap();
        done
    }

    async fn finish_job(&self, auth: &str, job_id: &str) -> Value {
        let stop = CancellationToken::new();
        let task = tokio::spawn(processing::worker(self.app.clone(), stop.clone()));
        let done = tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let reply = self
                    .call(auth, "GET", &format!("/api/v2/jobs/{job_id}"), None, &[])
                    .await;
                let phase = reply.body["phase"].as_str().unwrap().to_owned();
                if ["completed", "failed", "cancelled", "interrupted"].contains(&phase.as_str()) {
                    break reply.body;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("job finished");
        stop.cancel();
        task.await.unwrap().unwrap();
        done
    }

    async fn pair(&self, permissions: &[&str], library: &str) -> String {
        let operator = format!("Bearer {OPERATOR}");
        let pairing = self
            .call(
                "",
                "POST",
                "/api/v2/auth/pairings",
                Some(json!({"device_name":"Den TV","client_name":"motion-test"})),
                &[],
            )
            .await;
        assert_eq!(pairing.status, StatusCode::CREATED, "{:?}", pairing.body);
        let id = pairing.body["id"].as_str().unwrap().to_owned();
        let profiles: Vec<String> = self
            .call(&operator, "GET", "/api/v2/profiles", None, &[])
            .await
            .body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_owned())
            .collect();
        let approved = self
            .call(
                &operator,
                "POST",
                &format!("/api/v2/auth/pairings/{id}/approve"),
                Some(json!({"user_code":pairing.body["user_code"],"profile_ids":profiles,"permissions":permissions})),
                &[("idempotency-key", &format!("approve-{id}"))],
            )
            .await;
        assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
        let claimed = self
            .call(
                "",
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
                &operator,
                "GET",
                &format!("/api/v2/devices/{device}"),
                None,
                &[],
            )
            .await
            .headers["etag"]
            .to_str()
            .unwrap()
            .to_owned();
        let policy = self
            .call(
                &operator,
                "PUT",
                &format!("/api/v2/devices/{device}/policy"),
                Some(json!({"library_ids":[library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                &[("if-match", &etag)],
            )
            .await;
        assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
        format!("Bearer {}", claimed.body["access_token"].as_str().unwrap())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn library_workflow_runs_end_to_end_through_the_pages() {
    if !tools_available() {
        eprintln!("SKIPPED: ffmpeg/ffprobe not on PATH");
        return;
    }
    // Server diagnostics (e.g. encoder stderr) appear in failing test output.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("playscale=warn")
        .with_test_writer()
        .try_init();
    let f = Fixture::new().await;
    let op = format!("Bearer {OPERATOR}");
    let films = f.dir.path().join("films");
    // A real film with two audio tracks, so the preparation proves which one it kept.
    ffmpeg(
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
            "3",
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
            "-movflags",
            "+faststart",
        ],
        &films.join("Night Train (1999).mp4"),
    );
    // A second title, so playlists have an order to change.
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=24",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
        ],
        &films.join("Harbour Lights (2001).mp4"),
    );

    // 1. Register a source and a library from the sources page's forms.
    let sources = f.ok_page(&op, "/sources").await;
    assert!(sources.contains(r#"data-command="POST /api/v2/sources""#));
    let source = f
        .call(
            &op,
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Films disk","root_path":films.to_str().unwrap(),"exclusions":[]})),
            &[("idempotency-key", "n1-source-0001-key-padding")],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{:?}", source.body);
    let source_id = source.body["id"].as_str().unwrap().to_owned();
    let library = f
        .call(
            &op,
            "POST",
            "/api/v2/libraries",
            Some(json!({"name":"Films","kind":"movies","language":"en","source_ids":[source_id]})),
            &[("idempotency-key", "n1-library-0001-key-padding")],
        )
        .await;
    assert_eq!(library.status, StatusCode::CREATED, "{:?}", library.body);
    let library_id = library.body["id"].as_str().unwrap().to_owned();

    // 2. Scan from the page; its progress and result are shown there.
    let sources = f.ok_page(&op, "/sources").await;
    assert!(!sources.contains("Scanning is unavailable"), "{sources}");
    let scan_command = format!("POST /api/v2/libraries/{library_id}/scans");
    form(&sources, &scan_command, "Scan now");
    let scan = f
        .call(
            &op,
            "POST",
            &scan_command[5..],
            Some(json!({"mode":"incremental","require_complete":false})),
            &[("idempotency-key", "n1-scan-00000001-key-padding")],
        )
        .await;
    assert_eq!(scan.status, StatusCode::ACCEPTED, "{:?}", scan.body);
    let scan_id = scan.body["id"].as_str().unwrap().to_owned();
    let sources = f.ok_page(&op, "/sources").await;
    assert!(
        sources.contains(&format!("POST /api/v2/scans/{scan_id}/cancel"))
            || sources.contains("Complete")
    );
    let done = f.finish_scan(&op, &scan_id).await;
    assert_eq!(done["status"], "complete", "{done}");
    let sources = f.ok_page(&op, "/sources").await;
    assert!(sources.contains("Films</strong> — Complete"), "{sources}");
    assert!(
        sources.contains("Films disk: Complete, 2 files seen"),
        "{sources}"
    );

    // 3. Browse and search find the scanned title.
    let page = f.ok_page(&op, &format!("/library/{library_id}")).await;
    assert!(page.contains("Night Train"), "{page}");
    let found = f.ok_page(&op, "/search?q=night").await;
    assert!(found.contains("Night Train"), "{found}");
    let items = f
        .call(
            &op,
            "GET",
            &format!("/api/v2/catalog/items?library_id={library_id}"),
            None,
            &[],
        )
        .await;
    let by_title = |needle: &str| {
        items.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"].as_str().unwrap().contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {}", items.body))
            .clone()
    };
    let item_id = by_title("Night Train")["id"].as_str().unwrap().to_owned();
    let other_id = by_title("Harbour")["id"].as_str().unwrap().to_owned();
    let item_page = f.ok_page(&op, &format!("/item/{item_id}")).await;
    let manage = format!("/item/{item_id}/manage");
    assert!(item_page.contains(&format!("href=\"{manage}\"")));

    // 4. Correct metadata with the validator the page rendered.
    let html = f.ok_page(&op, &manage).await;
    let details_command =
        format!("PUT /api/v2/catalog/items/{item_id}/metadata/contributions/local");
    let details = form(&html, &details_command, "Save details");
    let stale_details = details.if_match.clone().unwrap();
    let saved = f
        .call(
            &op,
            "PUT",
            &details_command[4..],
            Some(json!({"values":{"title":"Night Train","release_year":1999,"release_date":"1999-05-01"},"tags":["noir"],"excluded_tags":[],"locked_fields":[]})),
            &[("if-match", &stale_details)],
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{:?}", saved.body);
    // A page edit replaces the whole local contribution: the form carries the
    // local fields it does not show, so release_date survives a title fix.
    let html = f.ok_page(&op, &manage).await;
    let details = form(&html, &details_command, "Save details");
    let mut values: Value = serde_json::from_str(&details.value("values")).unwrap();
    assert_eq!(values, json!({"release_date":"1999-05-01"}));
    assert_eq!(details.value("values.title"), "Night Train");
    values["title"] = json!("The Night Train");
    values["release_year"] = json!(1999);
    let saved = f
        .call(
            &op,
            "PUT",
            &details_command[4..],
            Some(json!({"values":values,"tags":["noir"],"excluded_tags":[],"locked_fields":[]})),
            &[("if-match", &details.if_match.unwrap())],
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{:?}", saved.body);
    let local = saved.body["contributions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["source"] == "local")
        .unwrap()
        .clone();
    assert_eq!(local["values"]["release_date"], "1999-05-01", "{local}");
    // The same rendered validator is now stale: a second edit is refused, not merged.
    let stale = f
        .call(
            &op,
            "PUT",
            &details_command[4..],
            Some(json!({"values":{"title":"Overwritten"},"tags":[],"excluded_tags":[],"locked_fields":[]})),
            &[("if-match", &stale_details)],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    let html = f.ok_page(&op, &manage).await;
    assert!(html.contains("The Night Train"), "{html}");
    assert!(html.contains("from your edit"), "{html}");

    // 5. Upload artwork as raw bytes and select it with the item validator shown.
    let png = f.dir.path().join("poster.png");
    ffmpeg(
        &["-f", "lavfi", "-i", "color=c=red:s=20x30", "-frames:v", "1"],
        &png,
    );
    let html = f.ok_page(&op, &manage).await;
    let upload_command = format!("POST /api/v2/catalog/items/{item_id}/artwork");
    form(&html, &upload_command, "data-body=\"file\"");
    let (status, _, bytes) = f
        .send(
            "POST",
            &upload_command[5..],
            Some(Body::from(std::fs::read(&png).unwrap())),
            "application/octet-stream",
            &[
                ("authorization", &op),
                ("idempotency-key", "n1-artwork-0001-key-padding"),
            ],
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let asset: Value = serde_json::from_slice(&bytes).unwrap();
    let asset_id = asset["id"].as_str().unwrap();
    let html = f.ok_page(&op, &manage).await;
    let select_command = format!("PUT /api/v2/catalog/items/{item_id}/artwork/selection");
    let select = form(&html, &select_command, asset_id);
    let selected = f
        .call(
            &op,
            "PUT",
            &select_command[4..],
            Some(json!({"role":"poster","asset_id":asset_id})),
            &[("if-match", &select.if_match.unwrap())],
        )
        .await;
    assert_eq!(selected.status, StatusCode::OK, "{:?}", selected.body);
    let poster = format!("/api/v2/media/assets/{asset_id}/content?revision=1");
    let item_page = f.ok_page(&op, &format!("/item/{item_id}")).await;
    assert!(
        item_page.contains(&format!("src=\"{poster}\"")),
        "{item_page}"
    );
    let (status, _, _) = f
        .send("GET", &poster, None, "", &[("authorization", &op)])
        .await;
    assert_eq!(status, StatusCode::OK);

    // 6. Rescan (verifying contents): corrections and artwork survive.
    let rescan = f
        .call(
            &op,
            "POST",
            &scan_command[5..],
            Some(json!({"mode":"verify","require_complete":false})),
            &[("idempotency-key", "n1-scan-00000002-key-padding")],
        )
        .await;
    assert_eq!(rescan.status, StatusCode::ACCEPTED, "{:?}", rescan.body);
    let done = f
        .finish_scan(&op, rescan.body["id"].as_str().unwrap())
        .await;
    assert_eq!(done["status"], "complete", "{done}");
    let item_page = f.ok_page(&op, &format!("/item/{item_id}")).await;
    assert!(item_page.contains("The Night Train"), "{item_page}");
    assert!(
        item_page.contains(&format!("src=\"{poster}\"")),
        "{item_page}"
    );

    // 7. Identification: a manual proposal for the exact file reaches Matches.
    let html = f.ok_page(&op, &manage).await;
    form(&html, "POST /api/v2/catalog/matches", "Find matches");
    let file = f
        .call(
            &op,
            "GET",
            &format!("/api/v2/catalog/items/{item_id}/files"),
            None,
            &[],
        )
        .await
        .body["items"][0]
        .clone();
    let file_ref = json!({"file_id":file["id"],"file_revision":file["revision"]});
    assert!(
        html.contains(
            &serde_json::to_string(&file_ref)
                .unwrap()
                .replace('"', "&quot;")
        ),
        "{html}"
    );
    let proposal = f
        .call(
            &op,
            "POST",
            "/api/v2/catalog/matches",
            Some(json!({"file":file_ref,"library_id":library_id,"provider":null})),
            &[("idempotency-key", "n1-match-000001-key-padding")],
        )
        .await;
    assert_eq!(proposal.status, StatusCode::ACCEPTED, "{:?}", proposal.body);
    let matches = f.ok_page(&op, "/matches").await;
    assert!(
        matches.contains(proposal.body["id"].as_str().unwrap()),
        "{matches}"
    );

    // 8. Organize: create a collection, then add the title with the exact body shown.
    let organize = f.ok_page(&op, "/organize").await;
    let profile = {
        let at = organize
            .find(r#"name="profile_id" data-type="text" value=""#)
            .unwrap();
        let rest = &organize[at + 42..];
        rest[..rest.find('"').unwrap()].to_owned()
    };
    let created = f
        .call(
            &op,
            "POST",
            "/api/v2/collections",
            Some(json!({"name":"Weekend","profile_id":profile,"kind":"manual","item_ids":[],"filter_id":null})),
            &[("idempotency-key", "n1-collection-01-key-padding")],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let collection = created.body["id"].as_str().unwrap().to_owned();
    let html = f.ok_page(&op, &manage).await;
    let add_command = format!("PUT /api/v2/collections/{collection}");
    let add = form(&html, &add_command, "Add to collection");
    assert_eq!(
        add.json_body.as_ref().unwrap()["item_ids"],
        json!([item_id])
    );
    let added = f
        .call(
            &op,
            "PUT",
            &add_command[4..],
            add.json_body.clone(),
            &[("if-match", &add.if_match.clone().unwrap())],
        )
        .await;
    assert_eq!(added.status, StatusCode::OK, "{:?}", added.body);
    // Replaying the old form after the change is refused.
    let replay = f
        .call(
            &op,
            "PUT",
            &add_command[4..],
            add.json_body,
            &[("if-match", &add.if_match.unwrap())],
        )
        .await;
    assert_eq!(
        replay.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        replay.body
    );
    let organize = f.ok_page(&op, "/organize").await;
    assert!(
        organize.contains(&format!("href=\"/item/{item_id}\">The Night Train</a>")),
        "{organize}"
    );

    // Removing it from the Organize page uses that page's exact body.
    let remove = form(&organize, &add_command, "Remove");
    assert_eq!(remove.json_body.as_ref().unwrap()["item_ids"], json!([]));
    let removed = f
        .call(
            &op,
            "PUT",
            &add_command[4..],
            remove.json_body,
            &[("if-match", &remove.if_match.unwrap())],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.body);
    assert_eq!(removed.body["item_ids"], json!([]));

    // Playlists: add both titles from their manage pages, reorder, remove.
    let created = f
        .call(
            &op,
            "POST",
            "/api/v2/playlists",
            Some(json!({"name":"Tonight","profile_id":profile,"entries":[]})),
            &[("idempotency-key", "n1-playlist-01-key-padding")],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let playlist = created.body["id"].as_str().unwrap().to_owned();
    let playlist_command = format!("PUT /api/v2/playlists/{playlist}");
    for title in [&item_id, &other_id] {
        let html = f.ok_page(&op, &format!("/item/{title}/manage")).await;
        let add = form(&html, &playlist_command, "Add to playlist");
        let reply = f
            .call(
                &op,
                "PUT",
                &playlist_command[4..],
                add.json_body,
                &[("if-match", &add.if_match.unwrap())],
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    }
    let timeline_of = |id: &str| {
        items.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap()["default_timeline_id"]
            .clone()
    };
    let order = |body: &Value| -> Vec<Value> {
        body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["timeline_id"].clone())
            .collect()
    };
    let current = f.call(&op, "GET", &playlist_command[4..], None, &[]).await;
    assert_eq!(
        order(&current.body),
        [timeline_of(&item_id), timeline_of(&other_id)]
    );
    let entry_ids: Vec<&str> = current.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["entry_id"].as_str().unwrap())
        .collect();
    assert_ne!(
        entry_ids[0], entry_ids[1],
        "generated entry IDs are distinct"
    );
    let organize = f.ok_page(&op, "/organize").await;
    let up = form(&organize, &playlist_command, "Move up");
    let moved = f
        .call(
            &op,
            "PUT",
            &playlist_command[4..],
            up.json_body,
            &[("if-match", &up.if_match.clone().unwrap())],
        )
        .await;
    assert_eq!(moved.status, StatusCode::OK, "{:?}", moved.body);
    assert_eq!(
        order(&moved.body),
        [timeline_of(&other_id), timeline_of(&item_id)]
    );
    // The moved-up form is now stale.
    let stale = f
        .call(
            &op,
            "PUT",
            &playlist_command[4..],
            form(&organize, &playlist_command, "Remove").json_body,
            &[("if-match", &up.if_match.unwrap())],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{:?}",
        stale.body
    );
    let organize = f.ok_page(&op, "/organize").await;
    assert!(organize.find("Harbour Lights").unwrap() < organize.find("The Night Train").unwrap());
    let remove = form(&organize, &playlist_command, "Remove");
    let removed = f
        .call(
            &op,
            "PUT",
            &playlist_command[4..],
            remove.json_body,
            &[("if-match", &remove.if_match.unwrap())],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{:?}", removed.body);
    assert_eq!(order(&removed.body), [timeline_of(&item_id)]);

    // 9. Prepare a version keeping the second audio track; inspect it on the page.
    let html = f.ok_page(&op, &manage).await;
    form(&html, "POST /api/v2/processing/preparations", "remux_mp4");
    assert!(html.contains(r#"<option value="a1">"#), "{html}");
    let timeline = timeline_of(&item_id).as_str().unwrap().to_owned();
    let unsupported = f
        .call(
            &op,
            "POST",
            "/api/v2/processing/preparations",
            Some(json!({"source":file_ref,"timeline_id":timeline,"recipe_id":"remux_mp4","tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"require","audio_track_id":"a1","subtitle_track_id":null}})),
            &[("idempotency-key", "n1-prepare-00000-key-padding")],
        )
        .await;
    assert_eq!(
        unsupported.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{:?}",
        unsupported.body
    );
    let job = f
        .call(
            &op,
            "POST",
            "/api/v2/processing/preparations",
            Some(json!({"source":file_ref,"timeline_id":timeline,"recipe_id":"remux_mp4","tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":"a1","subtitle_track_id":null}})),
            &[("idempotency-key", "n1-prepare-00001-key-padding")],
        )
        .await;
    assert_eq!(job.status, StatusCode::ACCEPTED, "{:?}", job.body);
    let finished = f.finish_job(&op, job.body["id"].as_str().unwrap()).await;
    let failure: Option<String> = sqlx::query_scalar("SELECT error FROM api_jobs WHERE id=?")
        .bind(job.body["id"].as_str().unwrap())
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(finished["phase"], "completed", "{finished} {failure:?}");
    let html = f.ok_page(&op, &manage).await;
    assert!(html.contains(r#"data-rendition-status="ready""#), "{html}");
    let processing_page = f.ok_page(&op, "/processing").await;
    assert!(processing_page.contains(job.body["id"].as_str().unwrap()));

    // 10. Schedules: add one from the page, then pause it with its validator.
    let created = f
        .call(
            &op,
            "POST",
            "/api/v2/schedules",
            Some(json!({"library_id":library_id,"task":"scan","interval_seconds":86400,"enabled":true})),
            &[("idempotency-key", "n1-schedule-0001-key-padding")],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let schedule = created.body["id"].as_str().unwrap().to_owned();
    let sources = f.ok_page(&op, "/sources").await;
    assert!(
        sources.contains("Films</strong> — Scan for changes, every 1 day(s)"),
        "{sources}"
    );
    let edit_command = format!("PUT /api/v2/schedules/{schedule}");
    let edit = form(&sources, &edit_command, "Save");
    let paused = f
        .call(
            &op,
            "PUT",
            &edit_command[4..],
            Some(json!({"library_id":library_id,"task":"scan","interval_seconds":3600,"enabled":false})),
            &[("if-match", &edit.if_match.unwrap())],
        )
        .await;
    assert_eq!(paused.status, StatusCode::OK, "{:?}", paused.body);
    let sources = f.ok_page(&op, "/sources").await;
    assert!(
        sources.contains("every 1 hour(s)") && sources.contains(" · paused"),
        "{sources}"
    );

    // 11. Library edit from the page's validator; the library list reflects it.
    let edit_command = format!("PUT /api/v2/libraries/{library_id}");
    let edit = form(&sources, &edit_command, "Save library");
    let renamed = f
        .call(
            &op,
            "PUT",
            &edit_command[4..],
            Some(json!({"name":"Feature films","kind":"movies","language":"en","source_ids":[source_id]})),
            &[("if-match", &edit.if_match.clone().unwrap())],
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{:?}", renamed.body);
    let home = f.ok_page(&op, "/").await;
    assert!(home.contains("Feature films"));

    // 12. Restricted principals reach only what their grant allows.
    let viewer = f
        .pair(&["catalog:read", "playback:request"], &library_id)
        .await;
    let (status, _) = f.page(&viewer, &manage).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let item_page = f.ok_page(&viewer, &format!("/item/{item_id}")).await;
    assert!(!item_page.contains(&manage));
    for page in ["/sources", "/organize", "/matches"] {
        let (status, _) = f.page(&viewer, page).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{page}");
    }
    let preparer = f
        .pair(&["catalog:read", "processing:request"], &library_id)
        .await;
    let html = f.ok_page(&preparer, &manage).await;
    assert!(html.contains("POST /api/v2/processing/preparations"));
    assert!(
        !html.contains("/metadata/contributions/local") && !html.contains("/api/v2/collections/")
    );
    // A grant without this library cannot learn that the title exists.
    let other = f
        .call(&op, "GET", "/api/v2/libraries", None, &[])
        .await
        .body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] != library_id.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let outsider = f
        .pair(
            &["catalog:read", "catalog:write", "processing:request"],
            &other,
        )
        .await;
    let (hidden, hidden_html) = f.page(&outsider, &manage).await;
    let (missing, missing_html) = f.page(&outsider, "/item/no-such-item/manage").await;
    assert_eq!(hidden, StatusCode::NOT_FOUND);
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert_eq!(hidden_html, missing_html);

    // 13. Relocation: preview against the shown source validator, then commit the plan.
    let moved = f.dir.path().join("films-moved");
    std::fs::rename(&films, &moved).unwrap();
    let sources = f.ok_page(&op, "/sources").await;
    let preview_command = format!("POST /api/v2/sources/{source_id}/relocation-plans");
    let preview = form(&sources, &preview_command, "Check new location");
    let plan = f
        .call(
            &op,
            "POST",
            &preview_command[5..],
            Some(json!({"new_root_path":moved.to_str().unwrap()})),
            &[
                ("if-match", &preview.if_match.unwrap()),
                ("idempotency-key", "n1-relocate-0001-key-padding"),
            ],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    assert!(sources.contains(&format!(
        "data-plan-commit=\"POST /api/v2/sources/{source_id}/relocation\""
    )));
    let committed = f
        .call(
            &op,
            "POST",
            &format!("/api/v2/sources/{source_id}/relocation"),
            Some(json!({"plan_token":plan.body["plan_token"]})),
            &[("idempotency-key", "n1-relocate-0002-key-padding")],
        )
        .await;
    assert_eq!(committed.status, StatusCode::OK, "{:?}", committed.body);
    let sources = f.ok_page(&op, "/sources").await;
    assert!(sources.contains(moved.to_str().unwrap()), "{sources}");
    let item_page = f.ok_page(&op, &format!("/item/{item_id}")).await;
    assert!(
        item_page.contains("The Night Train"),
        "identity survives relocation"
    );
}
