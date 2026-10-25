//! Actual packaged process qualification. No harness is registered as a closed
//! observer and no serving gate is bypassed by the fixture.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::process::{Child, Command};
const TOKEN: &str = "native-process-operator-token-0123456789abcdef";
struct Server {
    stderr: PathBuf,
    child: std::sync::Mutex<Child>,
    origin: String,
    client: reqwest::Client,
}
impl Server {
    async fn start(data: &Path, native: Option<&Path>) -> Self {
        std::fs::create_dir_all(data).unwrap();
        std::fs::write(data.join("admin-token"), TOKEN).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let origin = format!("http://{address}");
        let log = data.join(format!("process-test-{}.stderr", address.port()));
        let stdout = data.join(format!("process-test-{}.stdout", address.port()));
        let mut command = Command::new(env!("CARGO_BIN_EXE_playscale"));
        command
            .arg("--data-dir")
            .arg(data)
            .arg("--listen")
            .arg(address.to_string())
            .arg("--public-origin")
            .arg(&origin)
            .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap()))
            .kill_on_drop(true);
        if let Some(native) = native {
            command.arg("--catalog-path").arg(native);
        }
        let server = Self {
            stderr: log.clone(),
            child: std::sync::Mutex::new(command.spawn().unwrap()),
            origin,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        let started = std::time::Instant::now();
        let mut last_response = String::from("no readiness attempt completed");
        loop {
            if let Some(status) = server.child.lock().unwrap().try_wait().unwrap() {
                panic!(
                    "native process exited {status}; data={} stderr={} stdout={} last={last_response}: {}",
                    data.display(),
                    log.display(),
                    stdout.display(),
                    std::fs::read_to_string(&log).unwrap()
                );
            }
            match server
                .client
                .get(format!("{}/api/v2/catalog/items", server.origin))
                .bearer_auth(TOKEN)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        break;
                    }
                    last_response = format!(
                        "HTTP {status}: {}",
                        response
                            .text()
                            .await
                            .unwrap_or_else(|error| error.to_string())
                    );
                }
                Err(error) => last_response = format!("request error: {error}"),
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(90),
                "native process did not admit serving; data={} stderr={} stdout={} last={last_response}: {}",
                data.display(),
                log.display(),
                stdout.display(),
                std::fs::read_to_string(&log).unwrap()
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        server
    }
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> (u16, Value) {
        self.call_as(method, path, body, key, Some(TOKEN)).await
    }
    async fn call_as(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
        token: Option<&str>,
    ) -> (u16, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if path.ends_with("/cancel") {
            request = request.timeout(std::time::Duration::from_secs(150));
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = request.send().await.unwrap_or_else(|error| {
            // Capture real child status before kill-on-drop unwinds the fixture.
            let (pid, status) = {
                let mut child = self.child.lock().unwrap();
                (child.id(), child.try_wait())
            };
            let mut causes = Vec::new();
            let mut cause = std::error::Error::source(&error);
            while let Some(value) = cause {
                causes.push(value.to_string());
                cause = value.source();
            }
            panic!("process HTTP transport failed at {}{path}, child_pid={pid:?}, child_status={status:?}, timeout={}, connect={}, causes={causes:?}: {error:?}", self.origin, error.is_timeout(), error.is_connect());
        });
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .map(|v| v.to_str().unwrap().to_owned());
        let text = response.text().await.unwrap();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if status == 503 && body["code"] == "native_projection_busy" {
            assert_eq!(
                retry_after.as_deref(),
                Some("1"),
                "typed busy response must advertise bounded retry"
            );
        }
        (status, body)
    }
    /// Consume the real packaged service and wait for its successful terminal
    /// drain before taking the same operational lock for the ordered upgrade.
    async fn upgrade_preview_control(self, data: &Path) -> Self {
        use sqlx::Connection as _;
        use std::os::unix::fs::OpenOptionsExt;
        self.stop().await;
        let data = data.canonicalize().unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(data.join("server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        assert!(
            playscale::processing::held_witnesses(&data.join("generated"))
                .unwrap()
                .is_empty(),
            "clean service exit must leave no held worker witnesses"
        );
        let upgrade = motion_catalog_backend::OfflineControlUpgrade::prepare()
            .await
            .unwrap();
        let path = data.join("playscale.sqlite3").canonicalize().unwrap();
        let mut db = sqlx::SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .read_only(true)
                .create_if_missing(false),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT max(version) FROM _sqlx_migrations WHERE success=1"
            )
            .fetch_one(&mut db)
            .await
            .unwrap(),
            93
        );
        let binding: String =
            sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
                .fetch_one(&mut db)
                .await
                .unwrap();
        db.close().await.unwrap();
        let stopped_data = data.clone();
        tokio::task::spawn_blocking(move || {
            // The actual process lock remains owned until the synchronous
            // upgrade writer closes, even if this test's async waiter is lost.
            let _lock = lock;
            let path = stopped_data
                .join("playscale.sqlite3")
                .canonicalize()
                .unwrap();
            let native = stopped_data.join("catabolic.sqlite3");
            let native_before = catabolic_core::sha256(std::fs::read(&native).unwrap());
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .unwrap();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
            let image_sha = catabolic_core::sha256(&bytes);
            drop(bytes);
            std::io::Seek::rewind(&mut file).unwrap();
            let ack =
                motion_catalog_backend::OfflineControlShutdownAcknowledgment::for_stopped_target(
                    &path,
                    motion_catalog_backend::OfflineControlShutdownFacts {
                        service_admission_stopped: true,
                        native_workers_joined: true,
                        application_owners_drained: true,
                        exclusive_operational_ownership_retained: true,
                    },
                )
                .unwrap();
            upgrade
                .upgrade(
                    file,
                    &path,
                    &image_sha,
                    &catabolic_core::sha256(binding.as_bytes()),
                    ack,
                    0,
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap()
                .close()
                .unwrap();
            assert_eq!(
                catabolic_core::sha256(std::fs::read(&native).unwrap()),
                native_before,
                "ordered Motion upgrade must not change native source bytes"
            );
        })
        .await
        .unwrap();
        // Ordinary startup must validate the exact ordered97 ledger/schema.
        Self::start(&data, None).await
    }
    async fn stop(self) {
        let mut child = self.child.into_inner().unwrap();
        #[cfg(unix)]
        unsafe {
            libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        child.start_kill().unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(150), child.wait())
            .await
            .expect("native shutdown timed out")
            .unwrap();
        assert!(
            status.success(),
            "native process did not acknowledge clean shutdown: {status}; stderr={}: {}",
            self.stderr.display(),
            std::fs::read_to_string(&self.stderr).unwrap_or_default()
        );
    }
}
async fn scenario(root: &Path, custom: bool) -> (PathBuf, PathBuf) {
    let data = root.join("state");
    let native = if custom {
        root.join("custom.sqlite3")
    } else {
        data.join("catabolic.sqlite3")
    };
    let server = Server::start(&data, custom.then_some(native.as_path())).await;
    let work = json!({"kind":"movie","title":"Native process movie","library_ids":[]});
    let (status, created) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/catalog/items",
            Some(work.clone()),
            Some("process-work-create"),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    let (status, replayed) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/catalog/items",
            Some(work),
            Some("process-work-create"),
        )
        .await;
    assert_eq!(status, 201, "{replayed}");
    assert_eq!(created, replayed);
    let media = root.join("media");
    std::fs::create_dir(&media).unwrap();
    let source = json!({"name":"Movies","root_path":media,"exclusions":["Extras/"]});
    let (status, registered) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/sources",
            Some(source.clone()),
            Some("process-source-create"),
        )
        .await;
    assert_eq!(status, 201, "{registered}");
    let source_id = registered["id"].as_str().unwrap().to_owned();
    let first = motion_catalog_backend::inspect(native.clone())
        .await
        .unwrap()
        .identity;
    server.stop().await;
    let server = Server::start(&data, custom.then_some(native.as_path())).await;
    let (status, read) = server
        .call(
            reqwest::Method::GET,
            &format!("/api/v2/catalog/items/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["id"], created["id"]);
    assert_eq!(read["title"], "Native process movie");
    let (status, read) = server
        .call(
            reqwest::Method::GET,
            &format!("/api/v2/sources/{source_id}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["id"], registered["id"]);
    let (status, replayed) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/sources",
            Some(source),
            Some("process-source-create"),
        )
        .await;
    assert_eq!(status, 201, "{replayed}");
    assert_eq!(registered, replayed);
    server.stop().await;
    assert_eq!(
        first,
        motion_catalog_backend::inspect(native.clone())
            .await
            .unwrap()
            .identity
    );
    (data, native)
}
#[tokio::test]
async fn default_native_process_creates_reads_registers_and_restarts() {
    let dir = tempfile::tempdir().unwrap().keep();
    eprintln!("native process qualification evidence: {}", dir.display());
    scenario(&dir, false).await;
}
#[tokio::test]
async fn explicit_native_process_cannot_fall_back_after_configuration_loss() {
    let dir = tempfile::tempdir().unwrap().keep();
    eprintln!("native process qualification evidence: {}", dir.display());
    let (data, _) = scenario(&dir, true).await;
    let mut command = Command::new(env!("CARGO_BIN_EXE_playscale"));
    command
        .arg("--data-dir")
        .arg(&data)
        .arg("--listen")
        .arg("127.0.0.1:0")
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !output.status.success(),
        "custom native configuration silently changed"
    );
    assert!(
        !data.join("catabolic.sqlite3").exists(),
        "configuration loss created another catalog"
    );
}

fn write_pcm(path: &Path) {
    let bytes = 16000u32;
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((bytes + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(8000u32.to_le_bytes());
    wav.extend(16000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(bytes.to_le_bytes());
    wav.resize(44 + bytes as usize, 0);
    std::fs::write(path, wav).unwrap();
}
fn write_offline_audio(path: &Path) {
    // WAV discovery/readiness remains covered by native_scan_queue. This
    // process path additionally requests offline delivery, whose real player
    // contract supports AAC in MP4 but not WAV. Do not broaden that contract.
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "1",
            "-c:a",
            "aac",
            "-n",
        ])
        .arg(path)
        .output()
        .expect("ffmpeg AAC encoder required for offline fixture");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
async fn poll_scan(server: &Server, id: &str, wanted: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        let (status, body) = server
            .call(
                reqwest::Method::GET,
                &format!("/api/v2/scans/{id}"),
                None,
                None,
            )
            .await;
        assert_eq!(status, 200, "{body}");
        if body["status"] == wanted {
            return body;
        }
        assert!(
            !matches!(
                body["status"].as_str(),
                Some("failed" | "cancelled" | "complete")
            ),
            "scan terminated before {wanted}: {body}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "scan never reached {wanted}: {body}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
async fn create_source_library(server: &Server, root: &Path, label: &str) -> (String, String) {
    let (status, source) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/sources",
            Some(json!({"name":label,"root_path":root,"exclusions":["Extras/"]})),
            Some(&format!("process-source-{}", label.replace(' ', "-"))),
        )
        .await;
    assert_eq!(status, 201, "{source}");
    let source = source["id"].as_str().unwrap().to_owned();
    let (status, library) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/libraries",
            Some(json!({"name":label,"kind":"mixed","language":"en","source_ids":[source]})),
            Some(&format!("process-library-{}", label.replace(' ', "-"))),
        )
        .await;
    assert_eq!(status, 201, "{library}");
    (source, library["id"].as_str().unwrap().into())
}
async fn restricted_device(server: &Server, library: &str) -> String {
    let (status, pair) = server
        .call_as(
            reqwest::Method::POST,
            "/api/v2/auth/pairings",
            Some(
                json!({"device_name":"Restricted scan client","client_name":"native-process-test"}),
            ),
            None,
            None,
        )
        .await;
    assert_eq!(status, 201, "{pair}");
    let id = pair["id"].as_str().unwrap();
    let (status,approved)=server.call(reqwest::Method::POST,&format!("/api/v2/auth/pairings/{id}/approve"),Some(json!({"user_code":pair["user_code"],"profile_ids":["default"],"permissions":["catalog:read","sources:manage"]})),Some(&format!("process-approve-{id}"))).await;
    assert_eq!(status, 200, "{approved}");
    let (status, claimed) = server
        .call_as(
            reqwest::Method::POST,
            &format!("/api/v2/auth/pairings/{id}/claim"),
            Some(json!({"device_code":pair["device_code"]})),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{claimed}");
    let device = claimed["device_id"].as_str().unwrap();
    let url = format!("{}/api/v2/devices/{device}", server.origin);
    let response = server
        .client
        .get(&url)
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let etag = response.headers().get("etag").unwrap().clone();
    let response=server.client.put(format!("{url}/policy")).bearer_auth(TOKEN).header("if-match",etag)
        .header("content-type","application/json").body(json!({"library_ids":[library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read","sources:manage"]}).to_string()).send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status.as_u16(), 200, "{text}");
    claimed["access_token"].as_str().unwrap().into()
}
async fn download_device(server: &Server) -> (String, String) {
    let (status, pair) = server.call_as(reqwest::Method::POST, "/api/v2/auth/pairings",
        Some(json!({"device_name":"All-scope measured download operator","client_name":"native-process-test"})), None, None).await;
    assert_eq!(status, 201, "{pair}");
    let id = pair["id"].as_str().unwrap();
    let (status, approved) = server.call(reqwest::Method::POST, &format!("/api/v2/auth/pairings/{id}/approve"),
        Some(json!({"user_code":pair["user_code"],"profile_ids":["default"],"permissions":["catalog:read","downloads:manage","system:admin"]})),
        Some(&format!("process-download-approve-{id}"))).await;
    assert_eq!(status, 200, "{approved}");
    let (status, claimed) = server
        .call_as(
            reqwest::Method::POST,
            &format!("/api/v2/auth/pairings/{id}/claim"),
            Some(json!({"device_code":pair["device_code"]})),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{claimed}");
    (
        claimed["device_id"].as_str().unwrap().into(),
        claimed["access_token"].as_str().unwrap().into(),
    )
}
#[tokio::test]
async fn native_process_scan_publication_scope_cancellation_and_restart() {
    let dir = tempfile::tempdir().unwrap().keep();
    eprintln!("native public scan evidence: {}", dir.display());
    let data = dir.join("state");
    let media = dir.join("media");
    let other = dir.join("other");
    std::fs::create_dir(&media).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::fs::create_dir(media.join("Extras")).unwrap();
    write_offline_audio(&media.join("original.m4a"));
    write_pcm(&media.join("Extras/ignored.wav"));
    let server = Server::start(&data, None).await;
    let (_, library) = create_source_library(&server, &media, "primary").await;
    let (_, other_library) = create_source_library(&server, &other, "unrelated").await;
    let restricted = restricted_device(&server, &other_library).await;
    let input = json!({"mode":"verify","require_complete":true});
    let route = format!("/api/v2/libraries/{library}/scans");
    let (status, accepted) = server
        .call(
            reqwest::Method::POST,
            &route,
            Some(input.clone()),
            Some("process-scan-original"),
        )
        .await;
    assert_eq!(status, 202, "{accepted}");
    let scan = accepted["id"].as_str().unwrap().to_owned();
    poll_scan(&server, &scan, "complete").await;
    let (status, browse) = server
        .call(reqwest::Method::GET, "/api/v2/catalog/items", None, None)
        .await;
    assert_eq!(status, 200, "{browse}");
    let items = browse["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "excluded source subtree must not create a work: {browse}"
    );
    let work = items[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(items[0]["kind"], "other");
    assert_eq!(items[0]["availability"], "available");
    assert_eq!(items[0]["library_ids"], json!([library]));
    let timeline = items[0]["default_timeline_id"].clone();
    assert!(timeline.is_string());
    let (download_device, download_token) = download_device(&server).await;
    let download_input = json!({"device_id":download_device,"profile_id":"default","timeline_id":timeline,"version_id":null,"recipe_id":null,
        "tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}});
    let (status, download) = server
        .call_as(
            reqwest::Method::POST,
            "/api/v2/downloads",
            Some(download_input.clone()),
            Some("process-measured-download"),
            Some(&download_token),
        )
        .await;
    assert_eq!(status, 202, "{download}");
    assert_eq!(
        download["state"], "ready",
        "the actual measured original must be downloadable"
    );
    let download_path = format!("/api/v2/downloads/{}", download["id"].as_str().unwrap());
    let (status, read_download) = server
        .call_as(
            reqwest::Method::GET,
            &download_path,
            None,
            None,
            Some(&download_token),
        )
        .await;
    assert_eq!(status, 200, "{read_download}");
    assert_eq!(read_download["state"], "ready");
    assert_eq!(read_download["timeline_id"], timeline);
    let (status, denied_download) = server
        .call_as(
            reqwest::Method::GET,
            &download_path,
            None,
            None,
            Some(&restricted),
        )
        .await;
    assert!(
        matches!(status, 403 | 404),
        "another device must not read the download: {status} {denied_download}"
    );

    for (method, path, body) in [
        (reqwest::Method::GET, format!("/api/v2/scans/{scan}"), None),
        (
            reqwest::Method::POST,
            format!("/api/v2/scans/{scan}/cancel"),
            None,
        ),
        (reqwest::Method::POST, route.clone(), Some(input.clone())),
    ] {
        let (status, body) = server
            .call_as(
                method,
                &path,
                body,
                Some("process-denied-operation"),
                Some(&restricted),
            )
            .await;
        assert!(
            matches!(status, 403 | 404),
            "cross-library principal must not act: {status} {body}"
        );
    }
    let (status, list) = server
        .call_as(
            reqwest::Method::GET,
            "/api/v2/scans",
            None,
            None,
            Some(&restricted),
        )
        .await;
    assert_eq!(status, 200, "{list}");
    assert!(list["items"].as_array().unwrap().is_empty(), "{list}");
    let (status, list) = server
        .call(reqwest::Method::GET, "/api/v2/scans", None, None)
        .await;
    assert_eq!(status, 200, "{list}");
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == scan)
    );
    // Force a real active worker so cancellation exercises HTTP custody/drain.
    for n in 0..5000 {
        std::fs::write(media.join(format!("noise-{n}.txt")), b"observation").unwrap();
    }
    let (status, next) = server
        .call(
            reqwest::Method::POST,
            &route,
            Some(input.clone()),
            Some("process-scan-cancel"),
        )
        .await;
    assert_eq!(status, 202, "{next}");
    let cancelled = next["id"].as_str().unwrap().to_owned();
    poll_scan(&server, &cancelled, "running").await;
    let (status, cancel) = server
        .call(
            reqwest::Method::POST,
            &format!("/api/v2/scans/{cancelled}/cancel"),
            None,
            Some("process-scan-cancel-ack"),
        )
        .await;
    assert_eq!(status, 200, "{cancel}");
    assert_eq!(cancel["status"], "cancelled");
    server.stop().await;
    let server = Server::start(&data, None).await;
    let (status, replay) = server
        .call(
            reqwest::Method::POST,
            &route,
            Some(input),
            Some("process-scan-original"),
        )
        .await;
    assert_eq!(status, 202, "{replay}");
    assert_eq!(replay, accepted);
    poll_scan(&server, &scan, "complete").await;
    let (status, cancel) = server
        .call(
            reqwest::Method::GET,
            &format!("/api/v2/scans/{cancelled}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{cancel}");
    assert_eq!(cancel["status"], "cancelled");
    let (status, item) = server
        .call(
            reqwest::Method::GET,
            &format!("/api/v2/catalog/items/{work}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{item}");
    assert_eq!(item["default_timeline_id"], timeline);
    assert_eq!(item["availability"], "available");
    let (status, replay_download) = server
        .call_as(
            reqwest::Method::POST,
            "/api/v2/downloads",
            Some(download_input),
            Some("process-measured-download"),
            Some(&download_token),
        )
        .await;
    assert_eq!(status, 202, "{replay_download}");
    assert_eq!(
        replay_download, download,
        "download acceptance must replay without a new identity"
    );
    let (status, read_download) = server
        .call_as(
            reqwest::Method::GET,
            &download_path,
            None,
            None,
            Some(&download_token),
        )
        .await;
    assert_eq!(status, 200, "{read_download}");
    assert_eq!(read_download["state"], "ready");
    assert_eq!(read_download["timeline_id"], timeline);
    server.stop().await;
}

/// The public process owns both the encoder and native publication. No manual
/// worker invocation, direct database identities, or test-only route bypass.
#[tokio::test]
async fn native_process_preparation_publication_and_restart() {
    let dir = tempfile::tempdir().unwrap().keep();
    eprintln!("native public preparation evidence: {}", dir.display());
    let data = dir.join("state");
    let media = dir.join("media");
    std::fs::create_dir(&media).unwrap();
    let encoded = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            "-n",
        ])
        .arg(media.join("original.mp4"))
        .output()
        .expect("actual H264/AAC encoder required for process preparation fixture");
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let server = Server::start(&data, None).await;
    let (_, library) = create_source_library(&server, &media, "render source").await;
    // This is an actual operator-approved SystemAdmin device, not an empty
    // restricted policy mislabeled as unscoped. Admin implies processing and
    // playback permissions, and the captured credential is rechecked by worker.
    let (_, token) = download_device(&server).await;
    let (status, scan) = server
        .call(
            reqwest::Method::POST,
            &format!("/api/v2/libraries/{library}/scans"),
            Some(json!({"mode":"verify","require_complete":true})),
            Some("process-render-source-scan"),
        )
        .await;
    assert_eq!(status, 202, "{scan}");
    poll_scan(&server, scan["id"].as_str().unwrap(), "complete").await;
    let (status, browse) = server
        .call(reqwest::Method::GET, "/api/v2/catalog/items", None, None)
        .await;
    assert_eq!(status, 200, "{browse}");
    let items = browse["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{browse}");
    let work = items[0]["id"].as_str().unwrap().to_owned();
    let timeline = items[0]["default_timeline_id"].clone();
    assert!(timeline.is_string());
    let tracks = json!({"audio_component_id":null,"subtitle_component_id":null,
        "subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null});
    let plan_input = json!({"profile_id":"default","timeline_id":timeline,"version_id":null,"source":null,
        "tracks":tracks,"quality":{"mode":"original","max_bitrate_bps":null,"max_height":null,
        "allow_client_software":false,"hdr_policy":"preserve_if_supported"},
        "client":{"client_id":"native-process-render","client_build":"1","demuxe_asset_digest":null,
        "transports":["http_range"],"video_codecs":["avc1"],"audio_codecs":["mp4a"],"subtitle_modes":["text"],
        "hdr":"unknown","max_height":null,"software_decode":"unknown","cross_origin_isolated":false},
        "failed_candidate_ids":[]});
    let (status, plan) = server
        .call_as(
            reqwest::Method::POST,
            "/api/v2/playback/plans",
            Some(plan_input),
            None,
            Some(&token),
        )
        .await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(plan["status"], "ready", "{plan}");
    assert_eq!(plan["operation"], "original");
    assert!(plan["source"]["file_id"].is_string());
    let input = json!({"source":plan["source"],"timeline_id":timeline,
        "recipe_id":"remux_mp4","backend":"software","video_profile":null,"tracks":tracks});
    let (status, accepted) = server
        .call_as(
            reqwest::Method::POST,
            "/api/v2/processing/preparations",
            Some(input.clone()),
            Some("process-render-request"),
            Some(&token),
        )
        .await;
    assert_eq!(status, 202, "{accepted}");
    let job = accepted["id"].as_str().unwrap().to_owned();
    let route = format!(
        "/api/v2/processing/renditions?timeline_id={}",
        timeline.as_str().unwrap()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut busy_polls = 0usize;
    let rendition = loop {
        let requested = std::time::Instant::now();
        let (status, page) = server
            .call_as(reqwest::Method::GET, &route, None, None, Some(&token))
            .await;
        if status == 503 && page["code"] == "native_projection_busy" {
            busy_polls += 1;
            assert!(
                requested.elapsed() < std::time::Duration::from_secs(5),
                "busy response consumed the unchanged request deadline"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "native owner never settled within existing render deadline: {page}"
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        assert_eq!(status, 200, "{page}");
        let rows = page["items"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "one accepted preparation must have one rendition identity: {page}"
        );
        let row = &rows[0];
        assert_eq!(row["id"], job);
        match row["status"].as_str().unwrap() {
            "ready" => break row.clone(),
            "pending" => {}
            _ => panic!(
                "native renderer did not publish a current rendition: {row}; logs in {}",
                data.display()
            ),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native renderer timed out: {page}; logs in {}",
            data.display()
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    // Fast helpers may finish before a poll. Deterministic held-owner tests
    // prove the busy branch; this process flow proves eventual real readiness.
    eprintln!("actual render completed after {busy_polls} typed busy polls");
    assert_eq!(rendition["source"], plan["source"]);
    assert_eq!(rendition["timeline_id"], timeline);
    assert_eq!(rendition["recipe_id"], "remux_mp4");
    assert!(rendition["version_id"].is_string());
    assert!(rendition["output"]["file_revision"].is_string());
    assert_ne!(rendition["output"]["file_id"], plan["source"]["file_id"]);
    // Fresh native serving deliberately remains genuine93. Preview artifacts
    // require the actual stopped ordered97 upgrade, not an export allowlist.
    let server = server.upgrade_preview_control(&data).await;
    // This runs only the packaged server's normal native worker composition:
    // no test-spawned preview worker can satisfy this accepted demand.
    let preview_route = format!(
        "/api/v2/catalog/timelines/{}/previews",
        timeline.as_str().unwrap()
    );
    let (status, preview_accepted) = server
        .call_as(
            reqwest::Method::POST,
            &preview_route,
            None,
            Some("process-preview-request"),
            Some(&token),
        )
        .await;
    assert_eq!(status, 202, "{preview_accepted}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let preview = loop {
        let (status, body) = server
            .call_as(
                reqwest::Method::GET,
                &preview_route,
                None,
                None,
                Some(&token),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        if body["status"] == "ready" {
            break body;
        }
        assert_eq!(
            body["status"], "pending",
            "actual native preview did not remain admitted: {body}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "normal native preview worker did not publish: {body}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert_eq!(preview["job_id"], preview_accepted["job_id"]);
    assert_eq!(preview["file_id"], plan["source"]["file_id"]);
    let sheet = preview["sheets"][0].as_str().unwrap().to_owned();
    let response = server
        .client
        .get(format!("{}{}", server.origin, sheet))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let preview_bytes = response.bytes().await.unwrap();
    assert!(image::load_from_memory(&preview_bytes).is_ok());
    server.stop().await;

    let server = Server::start(&data, None).await;
    let (status, restarted_preview) = server
        .call_as(
            reqwest::Method::GET,
            &preview_route,
            None,
            None,
            Some(&token),
        )
        .await;
    assert_eq!(status, 200, "{restarted_preview}");
    assert_eq!(restarted_preview, preview);
    let response = server
        .client
        .get(format!("{}{}", server.origin, sheet))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().await.unwrap(), preview_bytes);
    // Restart recovery may own the same current projection admission. Retry
    // only its explicit transient refusal, never an arbitrary service error.
    let restart_deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let page = loop {
        let requested = std::time::Instant::now();
        let (status, page) = server
            .call_as(reqwest::Method::GET, &route, None, None, Some(&token))
            .await;
        if status == 503 && page["code"] == "native_projection_busy" {
            assert!(requested.elapsed() < std::time::Duration::from_secs(5));
            assert!(
                std::time::Instant::now() < restart_deadline,
                "restart projection admission never settled: {page}"
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        assert_eq!(status, 200, "{page}");
        break page;
    };
    assert_eq!(
        page["items"],
        json!([rendition]),
        "restart must preserve exact published identities"
    );
    let (status, replay) = server
        .call_as(
            reqwest::Method::POST,
            "/api/v2/processing/preparations",
            Some(input),
            Some("process-render-request"),
            Some(&token),
        )
        .await;
    assert_eq!(status, 202, "{replay}");
    assert_eq!(
        replay, accepted,
        "restart must replay original acceptance, not enqueue new work"
    );
    let (status, current) = server
        .call(
            reqwest::Method::GET,
            &format!("/api/v2/catalog/items/{work}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{current}");
    assert_eq!(current["default_timeline_id"], timeline);
    server.stop().await;
}

/// A genuine database created and activated at93 is not silently upgraded
/// across reserved94-96 just to expose preview97. Existing native serving
/// remains usable; preview capability refuses explicitly before table access.
#[tokio::test(flavor = "multi_thread")]
async fn native_sealed93_process_serves_catalog_and_refuses_preview97() {
    let temp = tempfile::tempdir().unwrap().keep();
    eprintln!("native sealed93 process evidence: {}", temp.display());
    let data = temp.join("sealed93-state");
    std::fs::create_dir(&data).unwrap();
    let motion = data.join("playscale.sqlite3");
    let native = data.join("catabolic.sqlite3");
    let mut old = sqlx::migrate!("./migrations");
    old.migrations = std::borrow::Cow::Owned(
        old.iter()
            .filter(|migration| migration.version <= 93)
            .cloned()
            .collect(),
    );
    let pool = playscale::db::connect_with(&motion, &old).await.unwrap();
    motion_catalog_backend::initialize(native.clone())
        .await
        .unwrap();
    let identity = motion_catalog_backend::inspect(native.clone())
        .await
        .unwrap()
        .identity;
    let backend =
        motion_catalog_backend::NativeCatalog::bind(pool.clone(), native.clone(), identity)
            .await
            .unwrap();
    backend
        .freeze_motion_source("genuine-sealed93-process", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("genuine-sealed93-process")
        .await
        .unwrap();
    backend
        .stage_import("genuine-sealed93-process")
        .await
        .unwrap();
    backend
        .prepare_motion_cutover("genuine-sealed93-process")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    drop(backend);
    pool.close().await;
    let server = Server::start(&data, None).await;
    let (status, created) = server
        .call(
            reqwest::Method::POST,
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"Retained93 remains usable","library_ids":[]})),
            Some("sealed93-native-work-create"),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let path = "/api/v2/catalog/timelines/00000000-0000-0000-0000-000000000001/previews";
    for method in [reqwest::Method::GET, reqwest::Method::POST] {
        let (status, body) = server
            .call(method, path, None, Some("sealed93-preview-capability"))
            .await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(
            body["code"], "preview_artifact_schema_unavailable",
            "{body}"
        );
    }
    server.stop().await;
    let server = Server::start(&data, None).await;
    let (status, read) = server
        .call(
            reqwest::Method::GET,
            &format!("/api/v2/catalog/items/{}", created["id"].as_str().unwrap()),
            None,
            None,
        )
        .await;
    assert_eq!(status, 200, "{read}");
    assert_eq!(read["title"], created["title"]);
    server.stop().await;
    let pool = playscale::db::connect_catalog_control(&motion)
        .await
        .unwrap();
    let version: i64 =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(version, 93);
    let artifacts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='preview_sheet_artifacts'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(artifacts, 0);
    pool.close().await;
}
