//! Application-owned state survives real native activation. No fake native
//! runtime, media tool skip, or compatibility catalog authority is used.
use super::playscale_app as playscale;
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tower::ServiceExt;

pub(crate) fn observer_binary() -> std::path::PathBuf {
    let path = option_env!("CARGO_BIN_EXE_playscale").map(str::to_owned)
        .or_else(|| std::env::var("MOTION_TEST_OBSERVER").ok())
        .expect("MOTION_TEST_OBSERVER must name the verified packaged playscale binary for unit fixtures");
    let path = std::path::PathBuf::from(path);
    assert!(
        path.is_absolute() && path.is_file(),
        "packaged playscale artifact missing"
    );
    path
}

pub(crate) const OPERATOR: &str = "operator-secret-token-0123456789abcdef";
const ORIGIN: &str = "http://127.0.0.1:8787";

pub(crate) struct Fixture {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) app: App,
}
pub(crate) struct Reply {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Value,
}
impl Fixture {
    pub(crate) async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // Export a genuine supported historical93 database, not latest97.
        // Preview artifact97 requires the separate ordered offline owner.
        let mut historical = sqlx::migrate!("./migrations");
        historical.migrations = std::borrow::Cow::Owned(
            historical
                .iter()
                .filter(|m| m.version <= 93)
                .cloned()
                .collect(),
        );
        let db = db::connect_with(&dir.path().join("db.sqlite"), &historical)
            .await
            .unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let mut processing =
            playscale::processing::Runtime::new(dir.path().join("cache"), Default::default());
        processing.supervisor = observer_binary();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            native_catalog: None,
            db,
            admin_token: Arc::new(OPERATOR.into()),
            origin: Arc::new(ORIGIN.into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(2)),
            event_streams: Arc::new(Semaphore::new(4)),
            storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
            processing: Arc::new(processing),
            access: Arc::new(playscale::v2::Runtime::new(
                AccessMode::Restricted,
                playscale::v2::auth::random_key(),
            )),
        };
        Self { _dir: dir, app }
    }
    /// Real file/source plus preserved independent Motion reference revisions.
    /// The caller supplies media bytes; this builder does not claim decoding or
    /// native admission. `cutover_source` obtains native identity by real scan.
    pub(crate) async fn seed_preserved_timeline(&self, relative: &str, bytes: &[u8]) -> String {
        let root = self._dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let file = root.join(relative);
        std::fs::write(&file, bytes).unwrap();
        let library = db::add_library(&self.app.db, "Source", &root)
            .await
            .unwrap()
            .id;
        sqlx::raw_sql("INSERT INTO items(id,title,kind,catalog_revision) VALUES('work','Work','video',11);
            INSERT INTO item_origins(item_id,title) VALUES('work','Work');
            INSERT INTO editions(id,item_id,label,revision) VALUES('edition','work','Original',5);
            INSERT INTO timelines(id,edition_id,revision,duration_ms) VALUES('timeline','edition',7,1000);
            INSERT INTO media_versions(id,timeline_id,revision,label,origin,equivalence) VALUES('version','timeline',9,'Source','original','declared');
            INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,automatic_watched,updated_at) VALUES('default','timeline',4,2,250,0,1);")
            .execute(&self.app.db).await.unwrap();
        let revision = catabolic_core::sha256(bytes);
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes,duration_seconds) VALUES('file','edition',?,?,?,?,?,1.0)")
            .bind(&library).bind(relative).bind(&revision)
            .bind(playscale::scan::fingerprint(&std::fs::metadata(&file).unwrap()))
            .bind(bytes.len() as i64).execute(&self.app.db).await.unwrap();
        sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES('version',1,'file',?)")
            .bind(&revision).execute(&self.app.db).await.unwrap();
        library
    }
    pub(crate) async fn cutover(&mut self) {
        self.cutover_source(None).await;
    }
    pub(crate) async fn cutover_source(&mut self, source: Option<&str>) {
        let path = self._dir.path().join("native.sqlite3");
        motion_catalog_backend::initialize(path.clone())
            .await
            .unwrap();
        let backend = motion_catalog_backend::NativeCatalog::bind(
            self.app.db.clone(),
            path.clone(),
            motion_catalog_backend::inspect(path.clone())
                .await
                .unwrap()
                .identity,
        )
        .await
        .unwrap();
        if let Some(source) = source {
            backend.register_motion_source(source).await.unwrap();
            // Use the production observer dispatcher, never the test harness.
            let binary = &observer_binary();
            let artifact = catabolic_runtime::helper::TrustedClosedObservationArtifact::after_packaging_verification(
                binary,
                &catabolic_core::sha256(std::fs::read(binary).unwrap()),
                catabolic_runtime::helper::ClosedObservationProtocol::NoChildWithExactJoinV2,
            ).unwrap();
            backend
                .scan_source(
                    source,
                    artifact,
                    motion_catalog_backend::sources::ScanCancellation::default(),
                    std::time::Duration::from_secs(30),
                )
                .await
                .unwrap();
        }
        backend.freeze_motion_source("app-empty", 0).await.unwrap();
        backend.capture_motion_import("app-empty").await.unwrap();
        backend.stage_import("app-empty").await.unwrap();
        if let Err(error) = backend.prepare_motion_cutover("app-empty").await {
            let inventory = backend.motion_reference_inventory("app-empty").await;
            let predicates = backend
                .motion_reference_candidate_diagnostics("app-empty")
                .await;
            // Diagnostic only: counts/classification, never raw auth or receipt bodies.
            let audit: Option<String> = sqlx::query_scalar("SELECT json_object('table_rows',json_extract(audit_json,'$.table_rows'),'logical_only_supported',json_extract(audit_json,'$.logical_only_supported'),'logical_root_structure_supported',json_extract(audit_json,'$.logical_root_structure_supported'),'retained_schedules_supported',json_extract(audit_json,'$.retained_schedules_supported'),'retained_schedule_receipts',json_extract(audit_json,'$.retained_schedule_receipts'),'retained_pairing_receipts',json_extract(audit_json,'$.retained_pairing_receipts'),'retained_download_receipts',json_extract(audit_json,'$.retained_download_receipts'),'physical_history_supported',json_extract(audit_json,'$.physical_history_supported')) FROM catabolic_reference_audits WHERE import_id='app-empty' AND length(audit_json)<=16777216")
                .fetch_optional(&self.app.db).await.unwrap();
            panic!(
                "cutover refusal: {error:#}; inventory={inventory:?}; audit_counts={audit:?}; physical_predicates={predicates:?}"
            );
        }
        backend
            .activate_motion_cutover(std::time::Duration::from_secs(5))
            .await
            .unwrap();
        drop(backend);
        self.open().await;
    }
    /// This fixture has never started a listening service. Retain actual
    /// private operational custody through the ordered offline upgrade; this
    /// is not evidence of production NativeService shutdown by itself.
    pub(crate) async fn upgrade_preview_control(&mut self) {
        use std::os::unix::fs::OpenOptionsExt;
        let upgrade = motion_catalog_backend::OfflineControlUpgrade::prepare()
            .await
            .unwrap();
        let raw: String =
            sqlx::query_scalar("SELECT identity_json FROM catabolic_binding WHERE id=1")
                .fetch_one(&self.app.db)
                .await
                .unwrap();
        let binding_sha = catabolic_core::sha256(raw.as_bytes());
        self.close().await;
        let runtime = Arc::get_mut(&mut self.app.processing)
            .expect("preview upgrade requires sole application runtime owner");
        runtime.execution.close_admission_if_idle().unwrap();
        runtime.hardware.close_admission_if_idle().unwrap();
        assert!(
            playscale::processing::held_witnesses(&runtime.root)
                .unwrap()
                .is_empty()
        );
        Arc::get_mut(&mut runtime.artifacts).expect("preview upgrade requires no artifact owners");
        let mut replacement =
            playscale::processing::Runtime::new(runtime.root.clone(), runtime.settings.clone());
        replacement.supervisor = runtime.supervisor.clone();
        let old = std::mem::replace(&mut self.app.processing, Arc::new(replacement));
        let drained = match Arc::try_unwrap(old) {
            Ok(owner) => owner,
            Err(_) => panic!("preview upgrade retained application owner"),
        };
        self.app.db.close().await;
        let path = self._dir.path().join("db.sqlite").canonicalize().unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self._dir.path().join("state/server.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&lock).unwrap();
        let upgraded_path = path.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            let _drained = drained;
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .unwrap();
            let mut image = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut image).unwrap();
            let image_sha = catabolic_core::sha256(&image);
            drop(image);
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
                    &binding_sha,
                    ack,
                    0,
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .unwrap()
                .close()
                .unwrap();
        })
        .await
        .unwrap();
        self.app.db = db::connect_catalog_control(&upgraded_path).await.unwrap();
        self.open().await;
        let schema: i64 =
            sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success=1")
                .fetch_one(&self.app.db)
                .await
                .unwrap();
        assert_eq!(schema, 97);
    }
    pub(crate) async fn open(&mut self) {
        self.app.native_catalog = Some(Arc::new(
            playscale::native_catalog::Runtime::open(
                self.app.db.clone(),
                self._dir.path().join("native.sqlite3"),
                "default".into(),
            )
            .await
            .unwrap(),
        ));
        // Match real startup: delivery recovery establishes its managed root
        // and fences prior sessions before any byte-only admission can publish.
        playscale::delivery::recover(&self.app).await.unwrap();
    }
    pub(crate) async fn close(&mut self) {
        self.app
            .native_catalog
            .take()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    }
    /// Full public router, including native route qualification and real auth.
    pub(crate) async fn viewing_adapter(
        &self,
        method: &str,
        token: &str,
        body: Option<Value>,
        tag: Option<&str>,
    ) -> Reply {
        let (server, epoch) = playscale::v2::system::server_identity(&self.app)
            .await
            .unwrap();
        let epoch = format!("{server}:{epoch}");
        let mut headers = vec![
            ("authorization", token),
            ("x-motion-viewing-epoch", epoch.as_str()),
        ];
        if let Some(tag) = tag {
            headers.push(("if-match", tag));
        }
        self.call(
            method,
            "/api/v2/profiles/default/timelines/timeline/viewing",
            body,
            &headers,
        )
        .await
    }
    pub(crate) async fn call(
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
        let request = request
            .body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
            .unwrap();
        let response = api::router(self.app.clone(), None)
            .oneshot(request)
            .await
            .unwrap();
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

    /// Full public downloads route, including native qualification and auth.
    pub(crate) async fn download_adapter(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Reply {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/api/v2{path}"))
            .header("authorization", token)
            .header("host", "127.0.0.1:8787");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
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
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        Reply {
            status,
            headers,
            body,
        }
    }

    pub(crate) async fn content_adapter(
        &self,
        token: &str,
        revision: &str,
    ) -> (StatusCode, Vec<u8>) {
        let request = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/v2/media/files/file/content?revision={revision}"
            ))
            .header("host", "127.0.0.1:8787")
            .header("authorization", token)
            .header("range", "bytes=0-15")
            .body(Body::empty())
            .unwrap();
        let response = api::router(self.app.clone(), None)
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    /// Full public router: playback qualification, ingress auth, and handlers
    /// are exercised together, including range tickets without a bearer.
    pub(crate) async fn playback_raw(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        key: Option<&str>,
        range: bool,
    ) -> axum::response::Response {
        let (server, epoch) = playscale::v2::system::server_identity(&self.app)
            .await
            .unwrap();
        let mut request = Request::builder()
            .method(method)
            .uri(if path.starts_with("/api/v2/") {
                path.to_owned()
            } else {
                format!("/api/v2{path}")
            })
            .header("host", "127.0.0.1:8787")
            .header("authorization", token)
            .header("x-motion-viewing-epoch", format!("{server}:{epoch}"));
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        if range {
            request = request.header("range", "bytes=0-15");
        }
        api::router(self.app.clone(), None)
            .oneshot(
                request
                    .body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    pub(crate) async fn playback_adapter(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> Reply {
        let response = self
            .playback_raw(method, path, token, body, key, false)
            .await;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        Reply {
            status,
            headers,
            body,
        }
    }

    pub(crate) async fn manifest_adapter(&self, id: &str, token: &str) -> Reply {
        self.call(
            "GET",
            &format!("/api/v2/downloads/{id}/manifest"),
            None,
            &[("authorization", token)],
        )
        .await
    }

    pub(crate) async fn pair(&self, profiles: &[&str], permissions: &[&str]) -> (String, String) {
        let pairing = self
            .call(
                "POST",
                "/api/v2/auth/pairings",
                Some(json!({"device_name":"Den TV","client_name":"motion-test"})),
                &[],
            )
            .await;
        assert_eq!(pairing.status, StatusCode::CREATED, "{:?}", pairing.body);
        let id = pairing.body["id"].as_str().unwrap().to_owned();
        let key = format!("approve-{id}");
        let approved = self
            .call(
                "POST",
                &format!("/api/v2/auth/pairings/{id}/approve"),
                Some(json!({"user_code":pairing.body["user_code"],"profile_ids":profiles,"permissions":permissions})),
                &[(
                    "authorization",
                    &format!("Bearer {OPERATOR}"),
                ), ("idempotency-key", &key)],
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
        (
            claimed.body["device_id"].as_str().unwrap().into(),
            claimed.body["access_token"].as_str().unwrap().into(),
        )
    }
}
