//! HTTP conformance for the catalog/storage v2 adapter: real router, real
//! SQLite, real transactions. Hierarchy, identity and access decisions are
//! unit- and model-checked in crates/core.
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

const OPERATOR: &str = "operator-secret-token-0123456789abcdef";
const ORIGIN: &str = "http://127.0.0.1:8787";

struct Fixture {
    _dir: tempfile::TempDir,
    app: App,
    /// Library A (the fixture's original) and library B.
    a: String,
    b: String,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Fixture {
    async fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_test_writer()
            .try_init();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let a = db::add_library(&db, "Movies", &root).await.unwrap().id;
        let b = db::add_library(&db, "Private", &other).await.unwrap().id;
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
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
            processing: Arc::new(playscale::processing::Runtime::new(
                dir.path().join("cache"),
                Default::default(),
            )),
            access: Arc::new(playscale::v2::Runtime::new(
                AccessMode::Restricted,
                playscale::v2::auth::random_key(),
            )),
        };
        Self {
            _dir: dir,
            app,
            a,
            b,
        }
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

    /// Pair a device with `permissions` and a policy over `libraries`;
    /// returns its bearer header value.
    async fn device(&self, permissions: &[&str], libraries: &[&str]) -> String {
        let operator = bearer(OPERATOR);
        let pairing = self
            .call(
                "POST",
                "/api/v2/auth/pairings",
                Some(json!({"device_name":"Den TV","client_name":"motion-test"})),
                &[],
            )
            .await;
        let id = pairing.body["id"].as_str().unwrap().to_owned();
        let key = format!("approve-{id}");
        let approved = self
            .call(
                "POST",
                &format!("/api/v2/auth/pairings/{id}/approve"),
                Some(json!({"user_code":pairing.body["user_code"],"profile_ids":[],"permissions":permissions})),
                &[("authorization", &operator), ("idempotency-key", &key)],
            )
            .await;
        assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
        let device = approved.body["id"].as_str().unwrap().to_owned();
        let policy = self
            .call(
                "PUT",
                &format!("/api/v2/devices/{device}/policy"),
                Some(json!({"library_ids":libraries,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                &[("authorization", &operator), ("if-match", "\"r-1\"")],
            )
            .await;
        assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
        let claimed = self
            .call(
                "POST",
                &format!("/api/v2/auth/pairings/{id}/claim"),
                Some(json!({"device_code":pairing.body["device_code"]})),
                &[],
            )
            .await;
        assert_eq!(claimed.status, StatusCode::OK, "{:?}", claimed.body);
        bearer(claimed.body["access_token"].as_str().unwrap())
    }
}
fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn native() -> (Fixture, std::path::PathBuf, String) {
    let mut f = Fixture::new().await;
    sqlx::raw_sql(
        "DELETE FROM library_sources; DELETE FROM catalog_libraries; DELETE FROM libraries;",
    )
    .execute(&f.app.db)
    .await
    .unwrap();
    let path = f._dir.path().join("native.sqlite3");
    motion_catalog_backend::initialize(path.clone())
        .await
        .unwrap();
    let backend = motion_catalog_backend::NativeCatalog::bind(
        f.app.db.clone(),
        path.clone(),
        motion_catalog_backend::inspect(path.clone())
            .await
            .unwrap()
            .identity,
    )
    .await
    .unwrap();
    backend
        .freeze_motion_source("metadata-empty", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("metadata-empty")
        .await
        .unwrap();
    backend.stage_import("metadata-empty").await.unwrap();
    backend
        .prepare_motion_cutover("metadata-empty")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    drop(backend);
    f.app.native_catalog = Some(Arc::new(
        playscale::native_catalog::Runtime::open(f.app.db.clone(), path.clone(), "default".into())
            .await
            .unwrap(),
    ));
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"Native origin","library_ids":[]})),
            &[
                ("authorization", &bearer(OPERATOR)),
                ("idempotency-key", "metadata-create-0001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    (f, path, id)
}

#[tokio::test]
async fn native_metadata_sibling_routes_remain_closed() {
    let (f, _, id) = native().await;
    let auth = bearer(OPERATOR);
    for (method, suffix, body) in [("POST", "metadata/refresh", None)] {
        let reply = f
            .call(
                method,
                &format!("/api/v2/catalog/items/{id}/{suffix}"),
                body,
                &[("authorization", &auth), ("if-match", "\"r-1\"")],
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            reply.body
        );
        assert_eq!(reply.body["code"], "native_route_unavailable");
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn native_metadata_outer_http_source_cas_replay_scope_and_restart() {
    let (mut f, path, id) = native().await;
    let scoped = f.device(&["catalog:read", "catalog:write"], &[]).await;
    let auth = bearer(OPERATOR);
    let endpoint = format!("/api/v2/catalog/items/{id}/metadata");
    let uri = format!("{endpoint}/contributions/local");
    let initial = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(initial.status, StatusCode::OK, "{}", initial.body);
    assert_eq!(initial.body["values"]["title"], "Native origin");
    let first_etag = initial.headers["etag"].to_str().unwrap().to_owned();
    let input =
        json!({"values":{"title":"Curated"},"tags":[],"excluded_tags":[],"locked_fields":[]});
    let first = f
        .call(
            "PUT",
            &uri,
            Some(input.clone()),
            &[("authorization", &auth), ("if-match", &first_etag)],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["values"]["title"], "Curated");
    assert_eq!(first.body["contributions"][0]["source"], "local");
    assert_eq!(first.body["contributions"][0]["revision"], "1");
    let replay = f
        .call(
            "PUT",
            &uri,
            Some(input),
            &[("authorization", &auth), ("if-match", &first_etag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.body, first.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let stale = f
        .call(
            "PUT",
            &uri,
            Some(
                json!({"values":{"title":"stale"},"tags":[],"excluded_tags":[],"locked_fields":[]}),
            ),
            &[("authorization", &auth), ("if-match", &first_etag)],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    let second_etag = first.headers["etag"].to_str().unwrap().to_owned();
    let changed = f
        .call(
            "PUT",
            &uri,
            Some(json!({"values":{},"tags":[],"excluded_tags":[],"locked_fields":[]})),
            &[("authorization", &auth), ("if-match", &second_etag)],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(changed.body["contributions"][0]["revision"], "2");
    assert_eq!(changed.body["contributions"][0]["values"], json!({}));
    assert_eq!(changed.body["values"]["title"], "Native origin");
    let denied = f
        .call("GET", &endpoint, None, &[("authorization", &scoped)])
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    let denied = f
        .call(
            "PUT",
            &uri,
            Some(json!({"values":{},"tags":[],"excluded_tags":[],"locked_fields":[]})),
            &[
                ("authorization", &scoped),
                ("if-match", changed.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "native source writes must never enter legacy documents"
    );
    f.app.native_catalog = None;
    f.app.native_catalog = Some(Arc::new(
        playscale::native_catalog::Runtime::open(f.app.db.clone(), path, "default".into())
            .await
            .unwrap(),
    ));
    let restarted = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(restarted.status, StatusCode::OK, "{}", restarted.body);
    assert_eq!(restarted.body, changed.body);
    assert_eq!(restarted.headers["etag"], changed.headers["etag"]);
}

#[tokio::test]
async fn native40_metadata_new_fields_tags_removal_and_ordinary_item_reads() {
    let (f, _, id) = native().await;
    let auth = bearer(OPERATOR);
    let endpoint = format!("/api/v2/catalog/items/{id}/metadata");
    let uri = format!("{endpoint}/contributions/local");
    let initial = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(initial.status, StatusCode::OK, "{}", initial.body);
    let etag = initial.headers["etag"].to_str().unwrap();
    let values: serde_json::Map<String, Value> = (0..100)
        .map(|i| {
            (
                if i == 0 {
                    "x".repeat(100)
                } else {
                    format!("field{i}")
                },
                json!(i),
            )
        })
        .collect();
    let input =
        json!({"values":values,"tags":["one","two"],"excluded_tags":["hidden"],"locked_fields":[]});
    let first = f
        .call(
            "PUT",
            &uri,
            Some(input.clone()),
            &[("authorization", &auth), ("if-match", etag)],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(
        first.body["contributions"][0]["values"]
            .as_object()
            .unwrap()
            .len(),
        100
    );
    assert_eq!(
        first.body["contributions"][0]["tags"],
        json!(["one", "two"])
    );
    let item = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(item.status, StatusCode::OK, "{}", item.body);
    let replay = f
        .call(
            "PUT",
            &uri,
            Some(input),
            &[("authorization", &auth), ("if-match", etag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let next = first.headers["etag"].to_str().unwrap();
    let removed = f
        .call(
            "PUT",
            &uri,
            Some(json!({"values":{},"tags":[],"excluded_tags":[],"locked_fields":[]})),
            &[("authorization", &auth), ("if-match", next)],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert_eq!(removed.body["contributions"][0]["revision"], "2");
    assert_eq!(removed.body["contributions"][0]["tags"], json!([]));
    assert_eq!(removed.body["contributions"][0]["excluded_tags"], json!([]));
    let retained: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(retained, 0);
}

#[tokio::test]
async fn native_metadata_refuses_lossy_tag_identity_before_intent_or_source_write() {
    let (f, _, id) = native().await;
    let auth = bearer(OPERATOR);
    let endpoint = format!("/api/v2/catalog/items/{id}/metadata");
    let before = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let etag = before.headers["etag"].to_str().unwrap();
    let intents: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_commands")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    for tag in ["straße", "a :b", "e\u{301}"] {
        let response=f.call("PUT",&format!("{endpoint}/contributions/local"),Some(json!({"values":{"title":"not committed"},"tags":[tag],"excluded_tags":[],"locked_fields":[]})),&[("authorization",&auth),("if-match",etag)]).await;
        assert_eq!(
            response.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            response.body
        );
        assert_eq!(
            response.body["code"],
            "native_tag_normalization_unavailable"
        );
    }
    let after = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(after.body, before.body);
    let after_intents: i64 = sqlx::query_scalar("SELECT count(*) FROM catabolic_commands")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(after_intents, intents);
}

#[tokio::test]
async fn native40_receipt_proven_origin_allows_provider_precedence_and_restart() {
    let (mut f, path, id) = native().await;
    let auth = bearer(OPERATOR);
    let endpoint = format!("/api/v2/catalog/items/{id}/metadata");
    let uri = format!("{endpoint}/contributions/provider");
    let initial = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(initial.status, StatusCode::OK, "{}", initial.body);
    let etag = initial.headers["etag"].to_str().unwrap().to_owned();
    let input = json!({"values":{"title":"Provider title"},"tags":[],"excluded_tags":[],"locked_fields":[]});
    let changed = f
        .call(
            "PUT",
            &uri,
            Some(input.clone()),
            &[("authorization", &auth), ("if-match", &etag)],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(changed.body["values"]["title"], "Provider title");
    assert_eq!(changed.body["field_sources"]["title"], json!(["provider"]));
    let replay = f
        .call(
            "PUT",
            &uri,
            Some(input),
            &[("authorization", &auth), ("if-match", &etag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.body, changed.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let ordinary = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(ordinary.status, StatusCode::OK, "{}", ordinary.body);
    assert_eq!(ordinary.body["title"], "Provider title");
    f.app.native_catalog = None;
    f.app.native_catalog = Some(Arc::new(
        playscale::native_catalog::Runtime::open(f.app.db.clone(), path, "default".into())
            .await
            .unwrap(),
    ));
    let restarted = f
        .call("GET", &endpoint, None, &[("authorization", &auth)])
        .await;
    assert_eq!(restarted.status, StatusCode::OK, "{}", restarted.body);
    assert_eq!(restarted.body, changed.body);
    let removed = f
        .call(
            "PUT",
            &uri,
            Some(json!({"values":{},"tags":[],"excluded_tags":[],"locked_fields":[]})),
            &[
                ("authorization", &auth),
                ("if-match", restarted.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert_eq!(removed.body["values"]["title"], "Native origin");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn native_refresh_background_refuses_without_advancing_or_legacy_writes() {
    let (mut f, _, id) = native().await;
    // Native activation freezes the legacy queue itself. Exercise ordinary
    // admission honestly rather than fabricating an unreachable queued row.
    let enqueue = sqlx::query("INSERT INTO metadata_jobs(id,item_id,requester_id,phase,created_at) VALUES('pending-native-refresh',?,'operator','queued',?)")
        .bind(&id).bind(playscale::now()).execute(&f.app.db).await;
    assert!(
        enqueue.is_err(),
        "legacy refresh enqueue must remain frozen"
    );
    for missing_runtime in [false, true] {
        if missing_runtime {
            f.app.native_catalog = None;
        }
        let error = playscale::metadata_jobs::run_one(&f.app).await.unwrap_err();
        assert!(
            error
                .downcast_ref::<playscale::metadata_jobs::NativeRefreshUnavailable>()
                .is_some(),
            "{error:#}"
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_jobs")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "refusal must not fabricate or complete a job");
        let error = playscale::nfo::refresh_item(&f.app, &id).await.unwrap_err();
        assert!(
            error
                .downcast_ref::<playscale::metadata_jobs::NativeRefreshUnavailable>()
                .is_some(),
            "{error:#}"
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_documents")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}

async fn imported_artwork(pin_only: bool) -> (Fixture, std::path::PathBuf, String, String) {
    let mut f = Fixture::new().await;
    sqlx::raw_sql(
        "DELETE FROM library_sources; DELETE FROM catalog_libraries; DELETE FROM libraries;",
    )
    .execute(&f.app.db)
    .await
    .unwrap();
    let auth = bearer(OPERATOR);
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"Artwork import","library_ids":[]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "artwork-create-00001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let response = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v2/catalog/items/{id}/artwork"))
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .header("idempotency-key", "artwork-upload-00001")
                .header("content-type", "application/octet-stream")
                .body(Body::from(bytes.into_inner()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let uploaded: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let asset = uploaded["id"].as_str().unwrap().to_owned();
    if pin_only {
        let current = f
            .call(
                "GET",
                &format!("/api/v2/catalog/items/{id}"),
                None,
                &[("authorization", &auth)],
            )
            .await;
        assert_eq!(current.status, StatusCode::OK, "{}", current.body);
        let selected = f
            .call(
                "PUT",
                &format!("/api/v2/catalog/items/{id}/artwork/selection"),
                Some(json!({"role":"poster", "asset_id":asset})),
                &[
                    ("authorization", &auth),
                    ("if-match", current.headers["etag"].to_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(selected.status, StatusCode::OK, "{}", selected.body);
        // Historical pin survives withdrawal of its former candidate association.
        sqlx::query("DELETE FROM item_artwork_assets WHERE item_id=?")
            .bind(&id)
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    let path = f._dir.path().join("native-artwork.sqlite3");
    motion_catalog_backend::initialize(path.clone())
        .await
        .unwrap();
    let backend = motion_catalog_backend::NativeCatalog::bind(
        f.app.db.clone(),
        path.clone(),
        motion_catalog_backend::inspect(path.clone())
            .await
            .unwrap()
            .identity,
    )
    .await
    .unwrap();
    backend
        .freeze_motion_source("artwork-import", 0)
        .await
        .unwrap();
    backend
        .capture_motion_import("artwork-import")
        .await
        .unwrap();
    backend.stage_import("artwork-import").await.unwrap();
    backend
        .prepare_motion_cutover("artwork-import")
        .await
        .unwrap();
    backend
        .activate_motion_cutover(std::time::Duration::from_secs(5))
        .await
        .unwrap();
    drop(backend);
    f.app.native_catalog = Some(Arc::new(
        playscale::native_catalog::Runtime::open(f.app.db.clone(), path.clone(), "default".into())
            .await
            .unwrap(),
    ));
    (f, path, id, asset)
}

#[tokio::test]
async fn native_artwork_current_references_selection_cas_replay_and_restart() {
    let (mut f, path, id, asset) = imported_artwork(false).await;
    let auth = bearer(OPERATOR);
    let item_path = format!("/api/v2/catalog/items/{id}");
    let selection_path = format!("{item_path}/artwork/selection");
    let listed = f
        .call(
            "GET",
            &format!("{item_path}/artwork"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"][0]["id"], asset);
    let before = f
        .call("GET", &selection_path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let etag = before.headers["etag"].to_str().unwrap().to_owned();
    let request = json!({"role":"poster", "asset_id":asset});
    let selected = f
        .call(
            "PUT",
            &selection_path,
            Some(request.clone()),
            &[("authorization", &auth), ("if-match", &etag)],
        )
        .await;
    assert_eq!(selected.status, StatusCode::OK, "{}", selected.body);
    assert_eq!(selected.body["artwork_id"], asset);
    let replay = f
        .call(
            "PUT",
            &selection_path,
            Some(request),
            &[("authorization", &auth), ("if-match", &etag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let reader = f.device(&["catalog:read"], &[]).await;
    let forbidden_replay = f
        .call(
            "PUT",
            &selection_path,
            Some(json!({"role":"poster", "asset_id":asset})),
            &[("authorization", &reader), ("if-match", &etag)],
        )
        .await;
    assert_eq!(
        forbidden_replay.status,
        StatusCode::FORBIDDEN,
        "{}",
        forbidden_replay.body
    );
    let stale = f
        .call(
            "PUT",
            &selection_path,
            Some(json!({"role":"banner", "asset_id":asset})),
            &[("authorization", &auth), ("if-match", &etag)],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    let current = f
        .call("GET", &item_path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let invalid = f
        .call(
            "PUT",
            &selection_path,
            Some(json!({"role":"poster", "asset_id":"not-associated"})),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(
        invalid.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        invalid.body
    );
    assert_eq!(invalid.body["code"], "invalid_artwork_selection");
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM artwork_selections WHERE item_id=?")
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        retained, 0,
        "native selection never writes retained catalog selections"
    );
    drop(f.app.native_catalog.take());
    f.app.native_catalog = Some(Arc::new(
        playscale::native_catalog::Runtime::open(f.app.db.clone(), path, "default".into())
            .await
            .unwrap(),
    ));
    let restarted = f
        .call("GET", &selection_path, None, &[("authorization", &auth)])
        .await;
    assert_eq!(restarted.status, StatusCode::OK, "{}", restarted.body);
    assert_eq!(restarted.body["roles"][0]["asset_id"], asset);
    assert_eq!(restarted.body["roles"][0]["pinned"], true);
    let denied = f
        .call(
            "PUT",
            &selection_path,
            Some(json!({"role":"poster", "asset_id":asset})),
            &[("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
}

async fn upload_native_png(f: &Fixture, id: &str, key: &str, width: u32, auth: &str) -> Reply {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let response = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v2/catalog/items/{id}/artwork"))
                .header("host", "127.0.0.1:8787")
                .header("authorization", auth)
                .header("idempotency-key", key)
                .header("content-type", "application/octet-stream")
                .body(Body::from(bytes.into_inner()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Reply {
        status,
        headers,
        body: serde_json::from_slice(&body).unwrap(),
    }
}

#[tokio::test]
async fn native_artwork_upload_atomic_attach_replay_and_current_verified_content() {
    let (f, path, id) = native().await;
    let auth = bearer(OPERATOR);
    let first = upload_native_png(&f, &id, "native-art-upload-0001", 2, &auth).await;
    if first.status != StatusCode::CREATED {
        use sqlx::Connection;
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .read_only(true);
        let mut native = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        let metadata: String = sqlx::query_scalar("SELECT metadata FROM items WHERE id=?")
            .bind(&id)
            .fetch_one(&mut native)
            .await
            .unwrap();
        let receipts: Vec<String> = sqlx::query_scalar(
            "SELECT operation_id FROM catalog_command_receipts ORDER BY operation_id LIMIT 100",
        )
        .fetch_all(&mut native)
        .await
        .unwrap();
        let intents: Vec<String> = sqlx::query_scalar(
            "SELECT operation_id FROM catabolic_commands ORDER BY operation_id LIMIT 100",
        )
        .fetch_all(&f.app.db)
        .await
        .unwrap();
        eprintln!(
            "artwork failure state: metadata={metadata}; native receipts={receipts:?}; Motion intents={intents:?}"
        );
    }
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let asset = first.body["id"].as_str().unwrap();
    sqlx::query("UPDATE idempotency_records SET expires_at=expires_at+123 WHERE operation='uploadArtwork' AND target=? AND key='native-art-upload-0001'").bind(&id).execute(&f.app.db).await.unwrap();
    let expiry_before: i64 = sqlx::query_scalar("SELECT expires_at FROM idempotency_records WHERE operation='uploadArtwork' AND target=? AND key='native-art-upload-0001'").bind(&id).fetch_one(&f.app.db).await.unwrap();
    let replay = upload_native_png(&f, &id, "native-art-upload-0001", 2, &auth).await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let expiry_after: i64 = sqlx::query_scalar("SELECT expires_at FROM idempotency_records WHERE operation='uploadArtwork' AND target=? AND key='native-art-upload-0001'").bind(&id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        expiry_after, expiry_before,
        "ordinary replay must not renew retention"
    );
    sqlx::query("DELETE FROM idempotency_records WHERE operation='uploadArtwork' AND target=? AND key='native-art-upload-0001'").bind(&id).execute(&f.app.db).await.unwrap();
    let recovered = upload_native_png(&f, &id, "native-art-upload-0001", 2, &auth).await;
    assert_eq!(recovered.status, StatusCode::CREATED, "{}", recovered.body);
    assert_eq!(recovered.headers["idempotent-replayed"], "true");
    let conflict = upload_native_png(&f, &id, "native-art-upload-0001", 3, &auth).await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
    let second = upload_native_png(&f, &id, "native-art-upload-0002", 3, &auth).await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    sqlx::query("UPDATE idempotency_records SET expires_at=0 WHERE operation='uploadArtwork' AND target=? AND key='native-art-upload-0001'").bind(&id).execute(&f.app.db).await.unwrap();
    let renewed = upload_native_png(&f, &id, "native-art-upload-0001", 3, &auth).await;
    assert_eq!(renewed.status, StatusCode::CREATED, "{}", renewed.body);
    assert_eq!(renewed.body["id"], second.body["id"]);
    // Model a committed second native incarnation whose HTTP ACK rolled back:
    // the prior expired ACK remains the durable identity anchor.
    sqlx::query("UPDATE idempotency_records SET expires_at=0,digest=?,body=? WHERE operation='uploadArtwork' AND target=? AND key='native-art-upload-0001'")
        .bind(asset).bind(first.body.to_string()).bind(&id).execute(&f.app.db).await.unwrap();
    let recovered_expired = upload_native_png(&f, &id, "native-art-upload-0001", 3, &auth).await;
    assert_eq!(
        recovered_expired.status,
        StatusCode::CREATED,
        "{}",
        recovered_expired.body
    );
    assert_eq!(recovered_expired.headers["idempotent-replayed"], "true");
    assert_eq!(recovered_expired.body["id"], second.body["id"]);
    let listed = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items/{id}/artwork"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 2);
    let legacy: i64 =
        sqlx::query_scalar("SELECT count(*) FROM item_artwork_assets WHERE item_id=?")
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(legacy, 0);
    let url = format!("/api/v2/media/assets/{asset}/content?revision=1");
    let response = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(&url)
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .header("range", "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        &[137, 80, 78, 71]
    );
    let head = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(&url)
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let none = f.device(&["catalog:read"], &[]).await;
    let hidden = f.call("GET", &url, None, &[("authorization", &none)]).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND, "{}", hidden.body);
    sqlx::query("UPDATE artwork_assets SET bytes=x'000000' WHERE id=?")
        .bind(asset)
        .execute(&f.app.db)
        .await
        .unwrap();
    let tampered = f.call("GET", &url, None, &[("authorization", &auth)]).await;
    assert_eq!(
        tampered.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        tampered.body
    );
}

#[tokio::test]
async fn native_artwork_current_historical_pin_is_not_an_orphan_asset() {
    let (f, _, id, asset) = imported_artwork(true).await;
    let auth = bearer(OPERATOR);
    let selected = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items/{id}/artwork/selection"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(selected.status, StatusCode::OK, "{}", selected.body);
    assert_eq!(selected.body["roles"][0]["asset_id"], asset);
    let url = format!("/api/v2/media/assets/{asset}/content?revision=1");
    let content = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri(&url)
                .header("host", "127.0.0.1:8787")
                .header("authorization", &auth)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(content.status(), StatusCode::OK);
    assert!(
        !content
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let replacement = upload_native_png(&f, &id, "replace-pin-upload-01", 3, &auth).await;
    assert_eq!(
        replacement.status,
        StatusCode::CREATED,
        "{}",
        replacement.body
    );
    let current = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items/{id}/artwork/selection"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    let replaced = f
        .call(
            "PUT",
            &format!("/api/v2/catalog/items/{id}/artwork/selection"),
            Some(json!({"role":"poster", "asset_id":replacement.body["id"]})),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.body);
    let cleared = f.call("GET", &url, None, &[("authorization", &auth)]).await;
    assert_eq!(cleared.status, StatusCode::NOT_FOUND, "{}", cleared.body);
    let stale = f
        .call(
            "PUT",
            &format!("/api/v2/catalog/items/{id}/artwork/selection"),
            Some(json!({"role":"poster", "asset_id":asset})),
            &[
                ("authorization", &auth),
                ("if-match", current.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    sqlx::query("INSERT INTO artwork_assets(id,mime,width,height,bytes) SELECT 'orphan-asset',mime,width,height,bytes FROM artwork_assets WHERE id=?").bind(&asset).execute(&f.app.db).await.unwrap();
    let orphan = f
        .call(
            "GET",
            "/api/v2/media/assets/orphan-asset/content?revision=1",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(orphan.status, StatusCode::NOT_FOUND, "{}", orphan.body);
}

#[tokio::test]
async fn native_matching_service_refuses_legacy_effects_with_or_without_runtime() {
    let (mut f, _, id) = native().await;
    let mut baseline = std::collections::BTreeMap::new();
    for table in ["metadata_documents", "match_proposals", "catalog_receipts"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.app.db)
            .await
            .unwrap();
        baseline.insert(table, count);
    }
    let original: (String, String) =
        sqlx::query_as("SELECT title,match_state FROM items WHERE id=?")
            .bind(&id)
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    for missing_runtime in [false, true] {
        if missing_runtime {
            drop(f.app.native_catalog.take());
        }
        let errors = [
            playscale::matching::get(&f.app.db, "absent-proposal")
                .await
                .unwrap_err(),
            playscale::matching::inbox(&f.app.db, 20).await.unwrap_err(),
            playscale::matching::propose(&f.app, "absent-file")
                .await
                .unwrap_err(),
            playscale::matching::decide(
                &f.app,
                "absent-proposal",
                1,
                playscale_core::matching::Decision::Reject,
            )
            .await
            .unwrap_err(),
            playscale::matching::reopen(&f.app, "absent-proposal", 1)
                .await
                .unwrap_err(),
            playscale::matching::unmatch(&f.app, &id).await.unwrap_err(),
            playscale::matching::identify_manually(&f.app, &id, "tmdb", "42")
                .await
                .unwrap_err(),
        ];
        for error in errors {
            match error {
                playscale::matching::MatchingError::Storage(error) => assert!(
                    error
                        .downcast_ref::<playscale::matching::NativeMatchingUnavailable>()
                        .is_some(),
                    "{error:?}"
                ),
                other => panic!("expected typed refusal before legacy lookup, got {other:?}"),
            }
        }
        let current: (String, String) =
            sqlx::query_as("SELECT title,match_state FROM items WHERE id=?")
                .bind(&id)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        assert_eq!(current, original);
        for table in ["metadata_documents", "match_proposals", "catalog_receipts"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&f.app.db)
                .await
                .unwrap();
            assert_eq!(
                count, baseline[table],
                "legacy matching must not modify {table}"
            );
        }
    }
}
