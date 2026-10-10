//! HTTP conformance for the v2 organization adapter (A09 service, A08
//! adapter): saved filters, collections, playlists and play queues. Real
//! router, real SQLite, real transactions. The decisions are unit-tested in
//! crates/core/src/organization.rs and model-checked in
//! crates/core/tests/organization_model.rs.
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
    library: String,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Fixture {
    async fn new(mode: AccessMode) -> Self {
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
                mode,
                playscale::v2::auth::random_key(),
            )),
        };
        Self {
            _dir: dir,
            app,
            library,
        }
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
        let response = self
            .raw(self.request(method, path, body.as_ref(), headers))
            .await;
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

    /// Pair a device with the given grant; returns (device_id, token).
    async fn pair(&self, profiles: &[&str], permissions: &[&str]) -> (String, String) {
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
    /// Two libraries, one work (and one timeline) in each, and a second
    /// profile. Returns the second library's ID.
    async fn seed(&self) -> String {
        let root = self._dir.path().join("shows");
        std::fs::create_dir(&root).unwrap();
        let shows = db::add_library(&self.app.db, "Shows", &root)
            .await
            .unwrap()
            .id;
        for sql in [
            "INSERT INTO items(id,title,kind) VALUES ('film','A Film','video')",
            "INSERT INTO items(id,title,kind) VALUES ('show','B Show','video')",
            "INSERT INTO items(id,title,kind) VALUES ('bare','C Bare','video')",
            "INSERT INTO editions(id,item_id,label) VALUES ('t-film','film','Original')",
            "INSERT INTO editions(id,item_id,label) VALUES ('t-show','show','Original')",
            "INSERT INTO timelines(id,edition_id) VALUES ('t-film','t-film'), ('t-show','t-show')",
            "INSERT INTO profiles(id,name) VALUES ('kids','Kids')",
        ] {
            sqlx::query(sql).execute(&self.app.db).await.unwrap();
        }
        for (id, edition, library) in [("f1", "t-film", &self.library), ("f2", "t-show", &shows)] {
            sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES (?,?,?,?,'r1','fp',1)")
                .bind(id)
                .bind(edition)
                .bind(library)
                .bind(format!("{id}.mp4"))
                .execute(&self.app.db)
                .await
                .unwrap();
        }
        shows
    }

    /// Pair a device and restrict it to `libraries`; returns its bearer header.
    async fn restricted(
        &self,
        profiles: &[&str],
        permissions: &[&str],
        libraries: &[&str],
    ) -> String {
        let (device, token) = self.pair(profiles, permissions).await;
        let replaced = self
            .call(
                "PUT",
                &format!("/api/v2/devices/{device}/policy"),
                Some(json!({"library_ids":libraries,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                &[("authorization", &bearer(OPERATOR)), ("if-match", "\"r-1\"")],
            )
            .await;
        assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
        bearer(&token)
    }

    async fn set_policy(
        &self,
        path: &str,
        libraries: &[&str],
        permissions: &[&str],
        tag: &str,
    ) -> Reply {
        self.call(
            "PUT",
            path,
            Some(json!({"library_ids":libraries,"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
            &[("authorization", &bearer(OPERATOR)), ("if-match", tag)],
        )
        .await
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

    async fn put(&self, path: &str, auth: &str, tag: &str, body: Value) -> Reply {
        self.call(
            "PUT",
            path,
            Some(body),
            &[("authorization", auth), ("if-match", tag)],
        )
        .await
    }

    async fn delete(&self, path: &str, auth: &str, tag: &str) -> Reply {
        self.call(
            "DELETE",
            path,
            None,
            &[("authorization", auth), ("if-match", tag)],
        )
        .await
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn problem(reply: &Reply, status: StatusCode, code: &str) {
    assert_eq!(reply.status, status, "{:?}", reply.body);
    assert_eq!(
        reply.headers["content-type"], "application/problem+json",
        "{:?}",
        reply.body
    );
    assert_eq!(reply.body["code"], code, "{:?}", reply.body);
    assert_eq!(reply.body["status"], status.as_u16());
    assert_eq!(
        reply.body["request_id"].as_str(),
        reply.headers["x-request-id"].to_str().ok()
    );
}

fn etag(reply: &Reply) -> String {
    reply.headers["etag"].to_str().unwrap().to_owned()
}

#[tokio::test]
async fn saved_filters_are_profile_scoped_conditional_and_idempotent() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    f.seed().await;
    let operator = bearer(OPERATOR);
    let device = f
        .restricted(
            &["default"],
            &["collections:write", "catalog:read"],
            &[&f.library],
        )
        .await;
    let reader = f
        .restricted(&["default"], &["catalog:read"], &[&f.library])
        .await;
    let body = json!({"name":"Recent","profile_id":"default","all":[{"field":"year","operator":"gte","value":2000},{"field":"watched","operator":"eq","value":false}]});

    // 401, 403, missing key, unknown field.
    let anonymous = f.call("GET", "/api/v2/catalog/filters", None, &[]).await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let denied = f
        .post(
            "/api/v2/catalog/filters",
            &reader,
            "filter-key-0000001",
            body.clone(),
        )
        .await;
    problem(&denied, StatusCode::FORBIDDEN, "permission_denied");
    problem(
        &f.get("/api/v2/catalog/filters", &reader).await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    let no_key = f
        .call(
            "POST",
            "/api/v2/catalog/filters",
            Some(body.clone()),
            &[("authorization", &device)],
        )
        .await;
    problem(&no_key, StatusCode::BAD_REQUEST, "idempotency_key_required");
    let mut extra = body.clone();
    extra["sql"] = json!("DROP TABLE items");
    let unknown = f
        .post(
            "/api/v2/catalog/filters",
            &device,
            "filter-key-0000002",
            extra,
        )
        .await;
    problem(&unknown, StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");
    let bad_term = f
        .post(
            "/api/v2/catalog/filters",
            &device,
            "filter-key-0000003",
            json!({"name":"Bad","profile_id":"default","all":[{"field":"year","operator":"contains","value":"x"}]}),
        )
        .await;
    problem(
        &bad_term,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_filter",
    );
    // A profile the device may not use is indistinguishable from a missing one.
    let mut kids = body.clone();
    kids["profile_id"] = json!("kids");
    problem(
        &f.post(
            "/api/v2/catalog/filters",
            &device,
            "filter-key-0000004",
            kids.clone(),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let mut ghost = body.clone();
    ghost["profile_id"] = json!("ghost");
    problem(
        &f.post(
            "/api/v2/catalog/filters",
            &operator,
            "filter-key-0000005",
            ghost,
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Create, replay, key reuse.
    let created = f
        .post(
            "/api/v2/catalog/filters",
            &device,
            "filter-key-0000006",
            body.clone(),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    assert_eq!(etag(&created), "\"r-1\"");
    assert_eq!(created.body["revision"], "1");
    assert_eq!(created.body["profile_id"], "default");
    assert_eq!(created.body["all"], body["all"]);
    let id = created.body["id"].as_str().unwrap().to_owned();
    let replay = f
        .post(
            "/api/v2/catalog/filters",
            &device,
            "filter-key-0000006",
            body.clone(),
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, created.body);
    let reused = f
        .post(
            "/api/v2/catalog/filters",
            &device,
            "filter-key-0000006",
            json!({"name":"Other","profile_id":"default","all":[]}),
        )
        .await;
    problem(&reused, StatusCode::CONFLICT, "idempotency_key_reused");
    let theirs = f
        .post(
            "/api/v2/catalog/filters",
            &operator,
            "filter-key-0000007",
            kids,
        )
        .await;
    assert_eq!(theirs.status, StatusCode::CREATED);
    let theirs = theirs.body["id"].as_str().unwrap().to_owned();

    // Read and list only the profiles the caller may use.
    let path = format!("/api/v2/catalog/filters/{id}");
    let read = f.get(&path, &device).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(etag(&read), "\"r-1\"");
    assert_eq!(read.body, created.body);
    let hidden = format!("/api/v2/catalog/filters/{theirs}");
    problem(
        &f.get(&hidden, &device).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.get("/api/v2/catalog/filters/missing", &device).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let list = f.get("/api/v2/catalog/filters", &device).await;
    assert_eq!(list.body["items"], json!([created.body]));
    assert!(list.body["event_cursor"].is_string());
    assert_eq!(
        f.get("/api/v2/catalog/filters", &operator).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let first = f.get("/api/v2/catalog/filters?limit=1", &operator).await;
    let cursor = first.body["next_cursor"].as_str().unwrap().to_owned();
    let second = f
        .get(
            &format!("/api/v2/catalog/filters?limit=1&cursor={cursor}"),
            &operator,
        )
        .await;
    assert_eq!(second.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(second.body["next_cursor"], Value::Null);
    assert_ne!(first.body["items"][0]["id"], second.body["items"][0]["id"]);

    // Conditional replacement.
    let renamed = json!({"name":"Renamed","profile_id":"default","all":[]});
    let missing = f
        .call(
            "PUT",
            &path,
            Some(renamed.clone()),
            &[("authorization", &device)],
        )
        .await;
    problem(
        &missing,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    problem(
        &f.put(&path, &device, "\"r-9\"", renamed.clone()).await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    problem(
        &f.put(&hidden, &device, "\"r-1\"", renamed.clone()).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let mut moved = renamed.clone();
    moved["profile_id"] = json!("kids");
    problem(
        &f.put(&path, &device, "\"r-1\"", moved).await,
        StatusCode::CONFLICT,
        "profile_immutable",
    );
    let replaced = f.put(&path, &device, "\"r-1\"", renamed).await;
    assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    assert_eq!(etag(&replaced), "\"r-2\"");
    assert_eq!(replaced.body["name"], "Renamed");
    assert_eq!(replaced.body["all"], json!([]));

    // A filter defining a smart collection cannot be removed.
    let smart = f
        .post(
            "/api/v2/collections",
            &device,
            "smart-key-00000001",
            json!({"name":"Smart","profile_id":"default","kind":"smart","item_ids":[],"filter_id":id}),
        )
        .await;
    assert_eq!(smart.status, StatusCode::CREATED, "{:?}", smart.body);
    problem(
        &f.delete(&path, &device, "\"r-2\"").await,
        StatusCode::CONFLICT,
        "filter_in_use",
    );
    let smart_path = format!("/api/v2/collections/{}", smart.body["id"].as_str().unwrap());
    assert_eq!(
        f.delete(&smart_path, &device, "\"r-1\"").await.status,
        StatusCode::NO_CONTENT
    );
    let no_tag = f
        .call("DELETE", &path, None, &[("authorization", &device)])
        .await;
    problem(
        &no_tag,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    problem(
        &f.delete(&path, &device, "\"r-1\"").await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    problem(
        &f.delete(&hidden, &device, "\"r-1\"").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.delete(&path, &reader, "\"r-2\"").await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    assert_eq!(
        f.delete(&path, &device, "\"r-2\"").await.status,
        StatusCode::NO_CONTENT
    );
    problem(
        &f.get(&path, &device).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.delete(&path, &device, "\"r-2\"").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn collections_validate_and_never_disclose_members_outside_the_scope() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let shows = f.seed().await;
    let operator = bearer(OPERATOR);
    let movies = f
        .restricted(
            &["default"],
            &["collections:write", "catalog:read"],
            &[&f.library],
        )
        .await;
    let both = f
        .restricted(
            &["default"],
            &["collections:write", "catalog:read"],
            &[&f.library, &shows],
        )
        .await;
    let reader = f
        .restricted(&["default"], &["catalog:read"], &[&f.library])
        .await;

    // Shape rules from the contract and the core.
    for (key, body, code) in [
        (
            "coll-bad-0000001",
            json!({"name":"x","profile_id":"default","kind":"manual","item_ids":[]}),
            "invalid_body",
        ),
        (
            "coll-bad-0000002",
            json!({"name":"x","profile_id":"default","kind":"manual","item_ids":[],"filter_id":"f"}),
            "invalid_collection",
        ),
        (
            "coll-bad-0000003",
            json!({"name":"x","profile_id":"default","kind":"smart","item_ids":[],"filter_id":null}),
            "invalid_collection",
        ),
        (
            "coll-bad-0000004",
            json!({"name":"x","profile_id":"default","kind":"manual","item_ids":["film","film"],"filter_id":null}),
            "duplicate_member",
        ),
        (
            "coll-bad-0000005",
            json!({"name":" ","profile_id":"default","kind":"manual","item_ids":[],"filter_id":null}),
            "invalid_name",
        ),
        (
            "coll-bad-0000006",
            json!({"name":"x","profile_id":"default","kind":"manual","item_ids":["no/slash"],"filter_id":null}),
            "invalid_field",
        ),
        (
            "coll-bad-0000007",
            json!({"name":"x","profile_id":"default","kind":"smart","item_ids":[],"filter_id":"missing"}),
            "invalid_reference",
        ),
    ] {
        problem(
            &f.post("/api/v2/collections", &operator, key, body).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
        );
    }
    let denied = f
        .post("/api/v2/collections", &reader, "coll-key-00000000", json!({"name":"x","profile_id":"default","kind":"manual","item_ids":[],"filter_id":null}))
        .await;
    problem(&denied, StatusCode::FORBIDDEN, "permission_denied");

    // An item outside the caller's libraries is rejected exactly like a missing one.
    for (key, item) in [
        ("coll-key-00000001", "show"),
        ("coll-key-00000002", "ghost"),
        ("coll-key-00000003", "bare"),
    ] {
        let rejected = f
            .post("/api/v2/collections", &movies, key, json!({"name":"Mine","profile_id":"default","kind":"manual","item_ids":[item],"filter_id":null}))
            .await;
        problem(
            &rejected,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_reference",
        );
        assert_eq!(
            rejected.body["detail"],
            "A referenced item, timeline or filter does not exist or is not accessible"
        );
    }

    let shared = f
        .post("/api/v2/collections", &both, "coll-key-00000004", json!({"name":"Shared","profile_id":"default","kind":"manual","item_ids":["show","film"],"filter_id":null}))
        .await;
    assert_eq!(shared.status, StatusCode::CREATED, "{:?}", shared.body);
    assert_eq!(shared.body["item_ids"], json!(["film", "show"]));
    assert_eq!(shared.body["filter_id"], Value::Null);
    assert_eq!(shared.body["kind"], "manual");
    let id = shared.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/collections/{id}");

    // The movies-only device sees (and counts) only the film, in GET and list.
    let read = f.get(&path, &movies).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["item_ids"], json!(["film"]));
    let list = f.get("/api/v2/collections", &movies).await;
    assert_eq!(list.body["items"][0]["item_ids"], json!(["film"]));
    assert_eq!(
        f.get("/api/v2/collections", &both).await.body["items"][0]["item_ids"],
        json!(["film", "show"])
    );

    // Its replacement keeps the member it cannot see.
    let emptied = f
        .put(&path, &movies, "\"r-1\"", json!({"name":"Shared","profile_id":"default","kind":"manual","item_ids":[],"filter_id":null}))
        .await;
    assert_eq!(emptied.status, StatusCode::OK, "{:?}", emptied.body);
    assert_eq!(emptied.body["item_ids"], json!([]));
    assert_eq!(etag(&emptied), "\"r-2\"");
    assert_eq!(f.get(&path, &both).await.body["item_ids"], json!(["show"]));
    let readded = f
        .put(&path, &movies, "\"r-2\"", json!({"name":"Shared","profile_id":"default","kind":"manual","item_ids":["film"],"filter_id":null}))
        .await;
    assert_eq!(readded.body["item_ids"], json!(["film"]));
    assert_eq!(
        f.get(&path, &operator).await.body["item_ids"],
        json!(["film", "show"])
    );
    // It cannot add the hidden member either.
    let smuggled = f
        .put(&path, &movies, "\"r-3\"", json!({"name":"Shared","profile_id":"default","kind":"manual","item_ids":["film","show"],"filter_id":null}))
        .await;
    problem(
        &smuggled,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_reference",
    );

    // A device restricted by ratings reads no catalog at all.
    let rated = {
        let (device, token) = f
            .pair(&["default"], &["collections:write", "catalog:read"])
            .await;
        let replaced = f
            .call(
                "PUT",
                &format!("/api/v2/devices/{device}/policy"),
                Some(json!({"library_ids":[f.library, shows],"allow_unrated":false,"allowed_ratings":[],"blocked_labels":[],"permissions":["collections:write","catalog:read"]})),
                &[("authorization", &operator), ("if-match", "\"r-1\"")],
            )
            .await;
        assert_eq!(replaced.status, StatusCode::OK);
        bearer(&token)
    };
    assert_eq!(f.get(&path, &rated).await.body["item_ids"], json!([]));

    // Inaccessible profiles are missing; profile_id cannot change.
    let kids = f
        .post("/api/v2/collections", &operator, "coll-key-00000005", json!({"name":"Kids","profile_id":"kids","kind":"manual","item_ids":["film"],"filter_id":null}))
        .await;
    assert_eq!(kids.status, StatusCode::CREATED);
    let kids_path = format!("/api/v2/collections/{}", kids.body["id"].as_str().unwrap());
    problem(
        &f.get(&kids_path, &movies).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.put(&kids_path, &movies, "\"r-1\"", json!({"name":"Kids","profile_id":"kids","kind":"manual","item_ids":[],"filter_id":null})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.delete(&kids_path, &movies, "\"r-1\"").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(
        f.get("/api/v2/collections", &movies).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.get("/api/v2/collections", &operator).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    problem(
        &f.put(&path, &operator, "\"r-3\"", json!({"name":"Shared","profile_id":"kids","kind":"manual","item_ids":[],"filter_id":null})).await,
        StatusCode::CONFLICT,
        "profile_immutable",
    );

    // Replay after the caller lost the profile is not disclosed.
    let body = json!({"name":"Later","profile_id":"default","kind":"manual","item_ids":[],"filter_id":null});
    let (device, token) = f.pair(&["default"], &["collections:write"]).await;
    let token = bearer(&token);
    let first = f
        .post(
            "/api/v2/collections",
            &token,
            "coll-key-00000006",
            body.clone(),
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED);
    sqlx::query("UPDATE devices SET profile_ids='[]',revision=revision+1 WHERE id=?")
        .bind(&device)
        .execute(&f.app.db)
        .await
        .unwrap();
    problem(
        &f.post("/api/v2/collections", &token, "coll-key-00000006", body)
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Delete is conditional.
    problem(
        &f.delete(&path, &movies, "\"r-1\"").await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    assert_eq!(
        f.delete(&path, &movies, "\"r-3\"").await.status,
        StatusCode::NO_CONTENT
    );
    problem(
        &f.get(&path, &operator).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn playlists_keep_entries_outside_the_scope_in_place() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let shows = f.seed().await;
    let operator = bearer(OPERATOR);
    let movies = f
        .restricted(
            &["default"],
            &["collections:write", "catalog:read"],
            &[&f.library],
        )
        .await;
    let reader = f
        .restricted(&["default"], &["viewing:write"], &[&f.library])
        .await;
    let _ = shows;
    let entries = json!([
        {"entry_id":"e1","timeline_id":"t-film"},
        {"entry_id":"e2","timeline_id":"t-show"},
        {"entry_id":"e3","timeline_id":"t-film"}
    ]);

    problem(
        &f.get("/api/v2/playlists", &reader).await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    problem(
        &f.post("/api/v2/playlists", &operator, "pl-key-000000001", json!({"name":"x","profile_id":"default","entries":[{"entry_id":"a","timeline_id":"t-film","extra":1}]})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    problem(
        &f.post("/api/v2/playlists", &operator, "pl-key-000000002", json!({"name":"x","profile_id":"default","entries":[{"entry_id":"a","timeline_id":"t-film"},{"entry_id":"a","timeline_id":"t-show"}]})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "duplicate_member",
    );
    problem(
        &f.post("/api/v2/playlists", &operator, "pl-key-000000003", json!({"name":"x","profile_id":"default","entries":[{"entry_id":"a","timeline_id":"ghost"}]})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_reference",
    );
    // A timeline of a work in another library is rejected like a missing one.
    problem(
        &f.post("/api/v2/playlists", &movies, "pl-key-000000004", json!({"name":"x","profile_id":"default","entries":[{"entry_id":"a","timeline_id":"t-show"}]})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_reference",
    );

    let created = f
        .post(
            "/api/v2/playlists",
            &operator,
            "pl-key-000000005",
            json!({"name":"Night","profile_id":"default","entries":entries}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    assert_eq!(created.body["entries"], entries);
    let replay = f
        .post(
            "/api/v2/playlists",
            &operator,
            "pl-key-000000005",
            json!({"name":"Night","profile_id":"default","entries":entries}),
        )
        .await;
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body["id"], created.body["id"]);
    let path = format!("/api/v2/playlists/{}", created.body["id"].as_str().unwrap());

    // The restricted device neither sees nor counts the show's entry.
    let read = f.get(&path, &movies).await;
    assert_eq!(read.body["entries"], json!([entries[0], entries[2]]));
    assert_eq!(etag(&read), "\"r-1\"");
    let list = f.get("/api/v2/playlists", &movies).await;
    assert_eq!(
        list.body["items"][0]["entries"].as_array().unwrap().len(),
        2
    );

    // Reordering what it sees keeps the hidden entry after its anchor.
    let reordered = f
        .put(
            &path,
            &movies,
            "\"r-1\"",
            json!({"name":"Night","profile_id":"default","entries":[entries[2], entries[0]]}),
        )
        .await;
    assert_eq!(reordered.status, StatusCode::OK, "{:?}", reordered.body);
    assert_eq!(reordered.body["entries"], json!([entries[2], entries[0]]));
    let full = f.get(&path, &operator).await;
    assert_eq!(
        full.body["entries"],
        json!([entries[2], entries[0], entries[1]])
    );
    assert_eq!(full.body["revision"], "2");
    // Reusing the hidden entry's ID is a duplicate.
    problem(
        &f.put(&path, &movies, "\"r-2\"", json!({"name":"Night","profile_id":"default","entries":[{"entry_id":"e2","timeline_id":"t-film"}]})).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "duplicate_member",
    );
    problem(
        &f.put(
            &path,
            &movies,
            "\"r-1\"",
            json!({"name":"Night","profile_id":"default","entries":[]}),
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let other = f
        .post(
            "/api/v2/playlists",
            &operator,
            "pl-key-000000006",
            json!({"name":"Kids","profile_id":"kids","entries":[]}),
        )
        .await;
    let other = format!("/api/v2/playlists/{}", other.body["id"].as_str().unwrap());
    problem(
        &f.get(&other, &movies).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.delete(&other, &movies, "\"r-1\"").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(
        f.get("/api/v2/playlists", &movies).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.delete(&path, &movies, "\"r-2\"").await.status,
        StatusCode::NO_CONTENT
    );
    problem(
        &f.get(&path, &operator).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn queues_round_trip_seeds_and_hide_the_current_entry_outside_the_scope() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    f.seed().await;
    let operator = bearer(OPERATOR);
    let movies = f
        .restricted(
            &["default"],
            &["viewing:write", "catalog:read"],
            &[&f.library],
        )
        .await;
    let organizer = f
        .restricted(
            &["default"],
            &["collections:write", "catalog:read"],
            &[&f.library],
        )
        .await;
    let entries = json!([
        {"entry_id":"e1","timeline_id":"t-film"},
        {"entry_id":"e2","timeline_id":"t-show"}
    ]);
    let body = json!({"profile_id":"default","entries":entries,"repeat":"all","shuffle_seed":"18446744073709551615"});

    problem(
        &f.get("/api/v2/playback/queues", &organizer).await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    problem(
        &f.call("GET", "/api/v2/playback/queues", None, &[]).await,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    for (key, seed) in [
        ("q-key-0000000001", json!("01")),
        ("q-key-0000000002", json!("18446744073709551616")),
        ("q-key-0000000003", json!(7)),
    ] {
        let mut bad = body.clone();
        bad["shuffle_seed"] = seed;
        let reply = f.post("/api/v2/playback/queues", &operator, key, bad).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{:?}",
            reply.body
        );
    }
    let mut no_seed = body.clone();
    no_seed.as_object_mut().unwrap().remove("shuffle_seed");
    problem(
        &f.post(
            "/api/v2/playback/queues",
            &operator,
            "q-key-0000000004",
            no_seed,
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    let mut bad_repeat = body.clone();
    bad_repeat["repeat"] = json!("forever");
    problem(
        &f.post(
            "/api/v2/playback/queues",
            &operator,
            "q-key-0000000005",
            bad_repeat,
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );

    let created = f
        .post(
            "/api/v2/playback/queues",
            &operator,
            "q-key-0000000006",
            body.clone(),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    assert_eq!(created.body["shuffle_seed"], "18446744073709551615");
    assert_eq!(created.body["repeat"], "all");
    assert_eq!(created.body["current_entry_id"], Value::Null);
    assert_eq!(created.body["entries"], entries);
    let id = created.body["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v2/playback/queues/{id}");
    let replay = f
        .post(
            "/api/v2/playback/queues",
            &operator,
            "q-key-0000000006",
            body.clone(),
        )
        .await;
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, created.body);
    problem(
        &f.post(
            "/api/v2/playback/queues",
            &operator,
            "q-key-0000000006",
            json!({"profile_id":"default","entries":[],"repeat":"off","shuffle_seed":null}),
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency_key_reused",
    );

    // Playback moved onto the show (hidden from the device).
    sqlx::query("UPDATE queues SET current_entry_id='e2' WHERE id=?")
        .bind(&id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let read = f.get(&path, &movies).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["entries"], json!([entries[0]]));
    assert_eq!(read.body["current_entry_id"], Value::Null);
    assert_eq!(
        f.get("/api/v2/playback/queues", &movies).await.body["items"][0]["entries"],
        json!([entries[0]])
    );
    assert_eq!(f.get(&path, &operator).await.body["current_entry_id"], "e2");

    // The device's edit keeps the hidden (current) entry; the core keeps it current.
    let edited = f
        .put(&path, &movies, "\"r-1\"", json!({"profile_id":"default","entries":[{"entry_id":"e3","timeline_id":"t-film"}],"repeat":"off","shuffle_seed":null}))
        .await;
    assert_eq!(edited.status, StatusCode::OK, "{:?}", edited.body);
    assert_eq!(etag(&edited), "\"r-2\"");
    assert_eq!(
        edited.body["entries"],
        json!([{"entry_id":"e3","timeline_id":"t-film"}])
    );
    assert_eq!(edited.body["shuffle_seed"], Value::Null);
    let full = f.get(&path, &operator).await;
    assert_eq!(
        full.body["entries"],
        json!([entries[1], {"entry_id":"e3","timeline_id":"t-film"}])
    );
    assert_eq!(full.body["current_entry_id"], "e2");
    assert_eq!(full.body["repeat"], "off");

    problem(
        &f.call(
            "PUT",
            &path,
            Some(json!({"profile_id":"default","entries":[],"repeat":"off","shuffle_seed":null})),
            &[("authorization", &movies)],
        )
        .await,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    problem(
        &f.put(
            &path,
            &movies,
            "\"r-1\"",
            json!({"profile_id":"default","entries":[],"repeat":"off","shuffle_seed":null}),
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    problem(
        &f.put(
            &path,
            &movies,
            "\"r-2\"",
            json!({"profile_id":"kids","entries":[],"repeat":"off","shuffle_seed":null}),
        )
        .await,
        StatusCode::CONFLICT,
        "profile_immutable",
    );
    let kids = f
        .post(
            "/api/v2/playback/queues",
            &operator,
            "q-key-0000000007",
            json!({"profile_id":"kids","entries":[],"repeat":"off","shuffle_seed":null}),
        )
        .await;
    let kids = format!(
        "/api/v2/playback/queues/{}",
        kids.body["id"].as_str().unwrap()
    );
    problem(
        &f.get(&kids, &movies).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.put(
            &kids,
            &movies,
            "\"r-1\"",
            json!({"profile_id":"kids","entries":[],"repeat":"off","shuffle_seed":null}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(
        f.get("/api/v2/playback/queues", &movies).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    problem(
        &f.delete(&path, &organizer, "\"r-2\"").await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    assert_eq!(
        f.delete(&path, &movies, "\"r-2\"").await.status,
        StatusCode::NO_CONTENT
    );
    problem(
        &f.get(&path, &movies).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn revocation_after_admission_is_enforced_inside_the_transaction() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let (device, token) = f.pair(&["default"], &["collections:write"]).await;
    let token = bearer(&token);
    // Hold the writer lock, so the request is admitted and then waits.
    let guard = f.app.jobs.lock().await;
    let request = f.request(
        "POST",
        "/api/v2/catalog/filters",
        Some(&json!({"name":"x","profile_id":"default","all":[]})),
        &[
            ("authorization", &token),
            ("idempotency-key", "revoke-key-000001"),
        ],
    );
    let pending = tokio::spawn(api::router(f.app.clone(), None).oneshot(request));
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    sqlx::query("UPDATE devices SET revoked=1,revision=revision+1 WHERE id=?")
        .bind(&device)
        .execute(&f.app.db)
        .await
        .unwrap();
    drop(guard);
    let response = pending.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_filters")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn replays_are_redacted_for_the_current_catalog_scope() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let shows = f.seed().await;
    let permissions = ["collections:write", "viewing:write", "catalog:read"];
    let (device, token) = f.pair(&["default"], &permissions).await;
    let token = bearer(&token);
    let policy = format!("/api/v2/devices/{device}/policy");
    assert_eq!(
        f.set_policy(&policy, &[&f.library, &shows], &permissions, "\"r-1\"")
            .await
            .status,
        StatusCode::OK
    );
    let entries =
        json!([{"entry_id":"e1","timeline_id":"t-film"},{"entry_id":"e2","timeline_id":"t-show"}]);
    let requests = [
        (
            "/api/v2/collections",
            "replay-coll-00001",
            json!({"name":"C","profile_id":"default","kind":"manual","item_ids":["film","show"],"filter_id":null}),
        ),
        (
            "/api/v2/playlists",
            "replay-play-00001",
            json!({"name":"P","profile_id":"default","entries":entries}),
        ),
        (
            "/api/v2/playback/queues",
            "replay-queue-0001",
            json!({"profile_id":"default","entries":entries,"repeat":"off","shuffle_seed":null}),
        ),
    ];
    let mut queue = String::new();
    for (path, key, body) in &requests {
        let created = f.post(path, &token, key, body.clone()).await;
        assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
        if path.contains("queues") {
            queue = created.body["id"].as_str().unwrap().to_owned();
        }
    }
    sqlx::query("UPDATE queues SET current_entry_id='e2' WHERE id=?")
        .bind(&queue)
        .execute(&f.app.db)
        .await
        .unwrap();
    // The stored acknowledgement still lists the show; the replay must not.
    assert_eq!(
        f.set_policy(&policy, &[&f.library], &permissions, "\"r-2\"")
            .await
            .status,
        StatusCode::OK
    );
    for (path, key, body) in &requests {
        let replay = f.post(path, &token, key, body.clone()).await;
        assert_eq!(replay.status, StatusCode::CREATED, "{:?}", replay.body);
        assert_eq!(replay.headers["idempotent-replayed"], "true");
        if path.contains("collections") {
            assert_eq!(replay.body["item_ids"], json!(["film"]));
        } else {
            assert_eq!(replay.body["entries"], json!([entries[0]]));
        }
    }
    // Acknowledgements are not current state: a replayed queue still names
    // the entry it was created with (none), never the hidden one.
    let replay = f
        .post(requests[2].0, &token, requests[2].1, requests[2].2.clone())
        .await;
    assert_eq!(replay.body["current_entry_id"], Value::Null);

    // Deleting a queue hints its profile in the same transaction.
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM change_events WHERE topic='queues' AND resource_id='default'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    let path = format!("/api/v2/playback/queues/{queue}");
    assert_eq!(
        f.delete(&path, &token, "\"r-1\"").await.status,
        StatusCode::NO_CONTENT
    );
    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM change_events WHERE topic='queues' AND resource_id='default'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(after, before + 1);
}
