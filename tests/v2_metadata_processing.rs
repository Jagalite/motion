//! HTTP conformance for the A08 metadata adapter (A04 services): resolved
//! metadata and contributions, identification proposals and decisions, item
//! artwork and asset bytes. Real router, real SQLite, real transactions.
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tower::ServiceExt;

const OPERATOR: &str = "operator-secret-token-0123456789abcdef";
const ORIGIN: &str = "http://127.0.0.1:8787";

struct Fixture {
    _dir: tempfile::TempDir,
    app: App,
    /// Library A: visible to the restricted device.
    library: String,
    /// Library B: hidden from the restricted device.
    hidden: String,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
    raw: Vec<u8>,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let db = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        let library = db::add_library(&db, "Movies", &root).await.unwrap().id;
        let hidden = db::add_library(&db, "Hidden", &other).await.unwrap().id;
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
        let f = Self {
            _dir: dir,
            app,
            library,
            hidden,
        };
        // Three works titled "Film": two in A, one in B.
        f.work(
            "film-a",
            "Film",
            &f.library.clone(),
            "Film (2001).mkv",
            "rev-a",
        )
        .await;
        f.work(
            "film-a2",
            "Film",
            &f.library.clone(),
            "copy/film.mkv",
            "rev-a2",
        )
        .await;
        f.work("film-b", "Film", &f.hidden.clone(), "Film.mkv", "rev-b")
            .await;
        f
    }

    async fn work(&self, id: &str, title: &str, library: &str, path: &str, revision: &str) {
        for (sql, binds) in [
            (
                "INSERT INTO items(id,title,kind) VALUES (?,?,'video')",
                vec![id, title],
            ),
            ("INSERT INTO item_origins VALUES (?,?)", vec![id, title]),
            (
                "INSERT INTO item_structure(item_id,media_type) VALUES (?,'movie')",
                vec![id],
            ),
        ] {
            let mut q = sqlx::query(sql);
            for b in binds {
                q = q.bind(b);
            }
            q.execute(&self.app.db).await.unwrap();
        }
        let edition = format!("ed-{id}");
        sqlx::query("INSERT INTO editions(id,item_id,label) VALUES (?,?,'Original')")
            .bind(&edition)
            .bind(id)
            .execute(&self.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES (?,?,?,?,?,'fp',1)")
            .bind(format!("file-{}", id.trim_start_matches("film-")))
            .bind(&edition)
            .bind(library)
            .bind(path)
            .bind(revision)
            .execute(&self.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO timelines(id,edition_id) VALUES (?,?)")
            .bind(&edition)
            .bind(&edition)
            .execute(&self.app.db)
            .await
            .unwrap();
        let file = format!("file-{}", id.trim_start_matches("film-"));
        sqlx::query("INSERT INTO media_versions(id,timeline_id,origin,equivalence) VALUES (?,?,'original','declared')").bind(&file).bind(&edition).execute(&self.app.db).await.unwrap();
        sqlx::query(
            "INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,1,?,?)",
        )
        .bind(&file)
        .bind(&file)
        .bind(revision)
        .execute(&self.app.db)
        .await
        .unwrap();
    }

    /// Store an artwork asset contributed to `item`; returns its ID.
    async fn artwork(&self, item: &str, bytes: &[u8]) -> String {
        let id = format!("{:x}", Sha256::digest(bytes));
        sqlx::query("INSERT OR IGNORE INTO artwork_assets VALUES (?,'image/png',1,1,?)")
            .bind(&id)
            .bind(bytes)
            .execute(&self.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO artwork_contributions VALUES (?,'poster','local',?,1)")
            .bind(item)
            .bind(&id)
            .execute(&self.app.db)
            .await
            .unwrap();
        id
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
        let raw = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        let body = serde_json::from_slice(&raw).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            body,
            raw,
        }
    }

    /// Pair a device with `permissions`; with `restrict`, its policy admits
    /// only library A. Returns the bearer header value.
    async fn device(&self, permissions: &[&str], restrict: bool) -> String {
        let operator = bearer(OPERATOR);
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
                Some(json!({"user_code":pairing.body["user_code"],"profile_ids":["default"],"permissions":permissions})),
                &[("authorization", &operator), ("idempotency-key", &key)],
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
        let device = claimed.body["device_id"].as_str().unwrap();
        if restrict {
            let policy = self
                .call(
                    "PUT",
                    &format!("/api/v2/devices/{device}/policy"),
                    Some(json!({"library_ids":[self.library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                    &[("authorization", &operator), ("if-match", "\"r-1\"")],
                )
                .await;
            assert_eq!(policy.status, StatusCode::OK, "{:?}", policy.body);
        }
        bearer(claimed.body["access_token"].as_str().unwrap())
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
}

fn auth(token: &str) -> [(&str, &str); 1] {
    [("authorization", token)]
}

#[tokio::test]
async fn metadata_read_replace_preconditions_and_scope() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let path = "/api/v2/catalog/items/film-a/metadata";

    problem(
        &f.call("GET", path, None, &[]).await,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let read = f.call("GET", path, None, &auth(&op)).await;
    assert_eq!(read.status, StatusCode::OK, "{:?}", read.body);
    assert_eq!(read.headers["etag"], "\"r-1\"");
    assert_eq!(
        read.body,
        json!({"item_id":"film-a","revision":"1","values":{"title":"Film"},"field_sources":{"title":["scan"]},"contributions":[]})
    );
    problem(
        &f.call(
            "GET",
            "/api/v2/catalog/items/missing/metadata",
            None,
            &auth(&op),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Permission and scope.
    let events_only = f.device(&["events:read"], false).await;
    problem(
        &f.call("GET", path, None, &auth(&events_only)).await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    let reader = f.device(&["catalog:read"], true).await;
    assert_eq!(
        f.call("GET", path, None, &auth(&reader)).await.status,
        StatusCode::OK
    );
    problem(
        &f.call(
            "GET",
            "/api/v2/catalog/items/film-b/metadata",
            None,
            &auth(&reader),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // A device without a library policy reads nothing.
    let unscoped = f.device(&["catalog:read"], false).await;
    problem(
        &f.call("GET", path, None, &auth(&unscoped)).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Replace a contribution.
    let put = "/api/v2/catalog/items/film-a/metadata/contributions/local";
    let input = json!({"values":{"title":"My Film","release_year":2001},"tags":["Drama"],"excluded_tags":[],"locked_fields":[]});
    problem(
        &f.call("PUT", put, Some(input.clone()), &auth(&op)).await,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    problem(
        &f.call(
            "PUT",
            put,
            Some(input.clone()),
            &[("authorization", &op), ("if-match", "\"r-7\"")],
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    problem(
        &f.call(
            "PUT",
            put,
            Some(input.clone()),
            &[("authorization", &reader), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    let mut unknown = input.clone();
    unknown["extra"] = json!(1);
    problem(
        &f.call(
            "PUT",
            put,
            Some(unknown),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    let mut locked = input.clone();
    locked["locked_fields"] = json!(["title"]);
    problem(
        &f.call(
            "PUT",
            put,
            Some(locked),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "locked_fields_unsupported",
    );
    problem(
        &f.call(
            "PUT",
            "/api/v2/catalog/items/film-a/metadata/contributions/scan",
            Some(input.clone()),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_source",
    );
    let mut bad_year = input.clone();
    bad_year["values"]["release_year"] = json!("soon");
    problem(
        &f.call(
            "PUT",
            put,
            Some(bad_year),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_metadata",
    );
    problem(
        &f.call(
            "PUT",
            "/api/v2/catalog/items/missing/metadata/contributions/local",
            Some(input.clone()),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let replaced = f
        .call(
            "PUT",
            put,
            Some(input.clone()),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    assert_eq!(replaced.headers["etag"], "\"r-2\"");
    assert_eq!(replaced.body["revision"], "2");
    assert_eq!(replaced.body["values"]["title"], "My Film");
    assert_eq!(replaced.body["field_sources"]["title"], json!(["local"]));
    assert_eq!(
        replaced.body["contributions"],
        json!([{"source":"local","revision":"1","values":{"title":"My Film","release_year":2001},"tags":["drama"],"excluded_tags":[],"locked_fields":[]}])
    );
    // The title projection followed.
    let title: String = sqlx::query_scalar("SELECT title FROM items WHERE id='film-a'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(title, "My Film");
    // An identical replacement is not a new revision; the old tag is stale.
    let same = f
        .call(
            "PUT",
            put,
            Some(input.clone()),
            &[("authorization", &op), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(same.status, StatusCode::OK);
    assert_eq!(same.body["revision"], "2");
    problem(
        &f.call(
            "PUT",
            put,
            Some(input.clone()),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    // A second source advances the item revision again.
    let provider = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film-a/metadata/contributions/catabolic",
            Some(json!({"values":{"title":"Imported"},"tags":[],"excluded_tags":[],"locked_fields":[]})),
            &[("authorization", &op), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(provider.status, StatusCode::OK, "{:?}", provider.body);
    assert_eq!(provider.body["revision"], "3");
    assert_eq!(provider.body["values"]["title"], "My Film", "local wins");

    // A restricted writer cannot write an item outside its scope.
    let writer = f.device(&["catalog:read", "catalog:write"], true).await;
    problem(
        &f.call(
            "PUT",
            "/api/v2/catalog/items/film-b/metadata/contributions/local",
            Some(input.clone()),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let inside = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film-a2/metadata/contributions/local",
            Some(input),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(inside.status, StatusCode::OK, "{:?}", inside.body);
}

#[tokio::test]
async fn matches_create_replay_decide_and_scope() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let create = |file: &str, revision: &str, library: &str| json!({"file":{"file_id":file,"file_revision":revision},"library_id":library,"provider":null});
    let body = create("file-a", "rev-a", &f.library);

    problem(
        &f.call("GET", "/api/v2/catalog/matches", None, &[]).await,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let reader = f.device(&["catalog:read"], true).await;
    problem(
        &f.call("GET", "/api/v2/catalog/matches", None, &auth(&reader))
            .await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(body.clone()),
            &auth(&op),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
    let key = [
        ("authorization", op.as_str()),
        ("idempotency-key", "match-key-0000000001"),
    ];
    let mut provider = body.clone();
    provider["provider"] = json!("tmdb");
    problem(
        &f.call("POST", "/api/v2/catalog/matches", Some(provider), &key)
            .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "provider_unavailable",
    );
    let mut missing_provider = body.clone();
    missing_provider.as_object_mut().unwrap().remove("provider");
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(missing_provider),
            &key,
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    let mut unknown = body.clone();
    unknown["extra"] = json!(true);
    problem(
        &f.call("POST", "/api/v2/catalog/matches", Some(unknown), &key)
            .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(create("file-a", "rev-old", &f.library)),
            &key,
        )
        .await,
        StatusCode::CONFLICT,
        "file_changed",
    );
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(create("file-a", "rev-a", &f.hidden)),
            &key,
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(create("nope", "rev-a", &f.library)),
            &key,
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let created = f
        .call("POST", "/api/v2/catalog/matches", Some(body.clone()), &key)
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    assert_eq!(created.headers["etag"], "\"r-1\"");
    assert_eq!(created.body["status"], "review");
    assert_eq!(
        created.body["file"],
        json!({"file_id":"file-a","file_revision":"rev-a"})
    );
    let candidates: Vec<&str> = created.body["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["item_id"].as_str().unwrap())
        .collect();
    assert_eq!(candidates, ["film-a2", "film-b"]);
    assert_eq!(
        created.body["candidates"][0],
        json!({"id":"local:film-a2","item_id":"film-a2","title":"Film","external_id":null,"reason_codes":["title"],"confidence":null})
    );
    let id = created.body["id"].as_str().unwrap().to_owned();
    // Replay and key reuse.
    let replay = f
        .call("POST", "/api/v2/catalog/matches", Some(body.clone()), &key)
        .await;
    assert_eq!(replay.status, StatusCode::ACCEPTED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, created.body);
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(create("file-a2", "rev-a2", &f.library)),
            &key,
        )
        .await,
        StatusCode::CONFLICT,
        "idempotency_key_reused",
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM match_proposals")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1, "replay and conflicts had no effect");

    // A proposal for the hidden library's file.
    let hidden = f
        .call(
            "POST",
            "/api/v2/catalog/matches",
            Some(create("file-b", "rev-b", &f.hidden)),
            &[
                ("authorization", op.as_str()),
                ("idempotency-key", "match-key-0000000002"),
            ],
        )
        .await;
    assert_eq!(hidden.status, StatusCode::ACCEPTED, "{:?}", hidden.body);
    let hidden_id = hidden.body["id"].as_str().unwrap().to_owned();

    // Get.
    let path = format!("/api/v2/catalog/matches/{id}");
    let got = f.call("GET", &path, None, &auth(&op)).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.headers["etag"], "\"r-1\"");
    assert_eq!(got.body, created.body);
    problem(
        &f.call("GET", "/api/v2/catalog/matches/missing", None, &auth(&op))
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Restricted writer: lists, counts and candidates are scoped.
    let writer = f.device(&["catalog:read", "catalog:write"], true).await;
    let all = f
        .call("GET", "/api/v2/catalog/matches", None, &auth(&op))
        .await;
    assert_eq!(all.body["items"].as_array().unwrap().len(), 2);
    let listed = f
        .call(
            "GET",
            "/api/v2/catalog/matches?limit=1",
            None,
            &auth(&writer),
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{:?}", listed.body);
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed.body["items"][0]["id"], id.as_str());
    assert!(
        listed.body["next_cursor"].is_null(),
        "no hidden row is counted"
    );
    assert_eq!(
        listed.body["items"][0]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "the hidden candidate is withheld"
    );
    problem(
        &f.call(
            "GET",
            &format!("/api/v2/catalog/matches/{hidden_id}"),
            None,
            &auth(&writer),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.call(
            "POST",
            "/api/v2/catalog/matches",
            Some(create("file-b", "rev-b", &f.hidden)),
            &[
                ("authorization", writer.as_str()),
                ("idempotency-key", "match-key-0000000003"),
            ],
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Decisions.
    let decision = format!("{path}/decision");
    let reject = json!({"decision":"reject","candidate_id":null});
    problem(
        &f.call("PUT", &decision, Some(reject.clone()), &auth(&op))
            .await,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    problem(
        &f.call(
            "PUT",
            &decision,
            Some(reject.clone()),
            &[("authorization", &op), ("if-match", "\"r-9\"")],
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    problem(
        &f.call(
            "PUT",
            &decision,
            Some(json!({"decision":"accept","candidate_id":null})),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_decision",
    );
    problem(
        &f.call(
            "PUT",
            &decision,
            Some(json!({"decision":"reject"})),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );
    problem(
        &f.call(
            "PUT",
            &decision,
            Some(reject.clone()),
            &[("authorization", &reader), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    problem(
        &f.call(
            "PUT",
            &format!("/api/v2/catalog/matches/{hidden_id}/decision"),
            Some(reject.clone()),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // The hidden candidate cannot be accepted by the restricted writer.
    problem(
        &f.call(
            "PUT",
            &decision,
            Some(json!({"decision":"accept","candidate_id":"local:film-b"})),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown_candidate",
    );
    let deferred = f
        .call(
            "PUT",
            &decision,
            Some(json!({"decision":"defer","candidate_id":null})),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(deferred.status, StatusCode::OK, "{:?}", deferred.body);
    assert_eq!(deferred.body["status"], "deferred");
    assert_eq!(deferred.headers["etag"], "\"r-2\"");
    let accepted = f
        .call(
            "PUT",
            &decision,
            Some(json!({"decision":"accept","candidate_id":"local:film-a2"})),
            &[("authorization", &writer), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    assert_eq!(accepted.body["status"], "accepted");
    assert_eq!(accepted.body["revision"], "3");
    let alias: String =
        sqlx::query_scalar("SELECT item_id FROM item_aliases WHERE alias_id='film-a'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(alias, "film-a2", "the work was merged into the candidate");
    problem(
        &f.call(
            "PUT",
            &decision,
            Some(reject),
            &[("authorization", &op), ("if-match", "\"r-3\"")],
        )
        .await,
        StatusCode::CONFLICT,
        "match_already_decided",
    );
}

#[tokio::test]
async fn artwork_listing_and_asset_bytes_are_scoped() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let png = b"\x89PNG-fixture-bytes-0123456789";
    let visible = f.artwork("film-a", png).await;
    let secret = f.artwork("film-b", b"\x89PNG-hidden").await;

    problem(
        &f.call("GET", "/api/v2/catalog/items/film-a/artwork", None, &[])
            .await,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let listed = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a/artwork",
            None,
            &auth(&op),
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{:?}", listed.body);
    assert_eq!(
        listed.body["items"],
        json!([{
            "id": visible,
            "revision": "1",
            "digest": format!("sha256:{visible}"),
            "kind": "artwork",
            "mime_type": "image/png",
            "size_bytes": png.len().to_string(),
            "content_url": format!("/api/v2/media/assets/{visible}/content?revision=1"),
            "source_revision": null,
        }])
    );
    assert!(listed.body["next_cursor"].is_null());
    assert!(listed.body["event_cursor"].is_string());
    problem(
        &f.call(
            "GET",
            "/api/v2/catalog/items/missing/artwork",
            None,
            &auth(&op),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let events_only = f.device(&["events:read"], false).await;
    problem(
        &f.call(
            "GET",
            "/api/v2/catalog/items/film-a/artwork",
            None,
            &auth(&events_only),
        )
        .await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    let reader = f.device(&["catalog:read"], true).await;
    let mine = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a/artwork",
            None,
            &auth(&reader),
        )
        .await;
    assert_eq!(mine.body["items"].as_array().unwrap().len(), 1);
    problem(
        &f.call(
            "GET",
            "/api/v2/catalog/items/film-b/artwork",
            None,
            &auth(&reader),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Asset bytes.
    let content = format!("/api/v2/media/assets/{visible}/content?revision=1");
    problem(
        &f.call("GET", &content, None, &[]).await,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let full = f.call("GET", &content, None, &auth(&reader)).await;
    assert_eq!(full.status, StatusCode::OK);
    assert_eq!(full.raw, png);
    assert_eq!(full.headers["etag"], "\"r-1\"");
    assert_eq!(full.headers["content-type"], "image/png");
    assert_eq!(full.headers["accept-ranges"], "bytes");
    let head = f.call("HEAD", &content, None, &auth(&reader)).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.raw.is_empty());
    assert_eq!(head.headers["content-length"], png.len().to_string());
    let part = f
        .call(
            "GET",
            &content,
            None,
            &[("authorization", &reader), ("range", "bytes=0-3")],
        )
        .await;
    assert_eq!(part.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.raw, &png[..4]);
    assert_eq!(
        part.headers["content-range"],
        format!("bytes 0-3/{}", png.len())
    );
    let stale_range = f
        .call(
            "GET",
            &content,
            None,
            &[
                ("authorization", &reader),
                ("range", "bytes=0-3"),
                ("if-range", "\"r-0\""),
            ],
        )
        .await;
    assert_eq!(stale_range.status, StatusCode::OK);
    let beyond = f
        .call(
            "GET",
            &content,
            None,
            &[("authorization", &reader), ("range", "bytes=999-")],
        )
        .await;
    assert_eq!(beyond.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        beyond.headers["content-range"],
        format!("bytes */{}", png.len())
    );
    let cached = f
        .call(
            "GET",
            &content,
            None,
            &[("authorization", &reader), ("if-none-match", "W/\"r-1\"")],
        )
        .await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    problem(
        &f.call(
            "GET",
            &content,
            None,
            &[("authorization", &reader), ("if-match", "\"r-2\"")],
        )
        .await,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    problem(
        &f.call(
            "GET",
            &format!("/api/v2/media/assets/{visible}/content?revision=2"),
            None,
            &auth(&reader),
        )
        .await,
        StatusCode::CONFLICT,
        "revision_mismatch",
    );
    problem(
        &f.call(
            "GET",
            &format!("/api/v2/media/assets/{visible}/content"),
            None,
            &auth(&reader),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "revision_required",
    );
    // A hidden item's asset is indistinguishable from a missing one, whatever
    // the revision or method.
    for (method, revision) in [("GET", "1"), ("HEAD", "1"), ("GET", "2")] {
        let reply = f
            .call(
                method,
                &format!("/api/v2/media/assets/{secret}/content?revision={revision}"),
                None,
                &auth(&reader),
            )
            .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {revision}");
    }
    problem(
        &f.call(
            "GET",
            "/api/v2/media/assets/missing/content?revision=1",
            None,
            &auth(&op),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(
        f.call(
            "GET",
            &format!("/api/v2/media/assets/{secret}/content?revision=1"),
            None,
            &auth(&op),
        )
        .await
        .status,
        StatusCode::OK
    );
    // Without catalog:read nothing is disclosed.
    problem(
        &f.call("GET", &content, None, &auth(&events_only)).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn blocked_operations_are_not_routed() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    for (method, path) in [
        ("POST", "/api/v2/catalog/items/film-a/metadata/refresh"),
        ("POST", "/api/v2/catalog/items/film-a/artwork"),
        ("PUT", "/api/v2/catalog/items/film-a/artwork/selection"),
        ("GET", "/api/v2/processing/capabilities"),
        ("GET", "/api/v2/schedules"),
        ("POST", "/api/v2/admin/imports"),
    ] {
        let reply = f.call(method, path, None, &auth(&op)).await;
        assert!(
            matches!(
                reply.status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{method} {path}: {}",
            reply.status
        );
    }
}

#[tokio::test]
async fn match_presentation_uses_authorized_candidates_and_api_revision() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/matches",
            Some(json!({
                "file":{"file_id":"file-a","file_revision":"rev-a"},
                "library_id":f.library,"provider":null
            })),
            &[
                ("authorization", &op),
                ("idempotency-key", "presentation-match-0001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    let token = f.device(&["catalog:read", "catalog:write"], true).await;
    let router = api::router_with(
        f.app.clone(),
        None,
        Some(playscale::presentation::router(f.app.clone())),
    );
    let response = router
        .oneshot(
            Request::builder()
                .uri("/matches")
                .header("host", "127.0.0.1:8787")
                .header("authorization", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("File file-a"), "{html}");
    assert!(html.contains("Film"), "{html}");
    assert!(!html.contains("file-b"), "{html}");
    // The form must send the same strong validator as the API representation.
    let tag = created.headers["etag"]
        .to_str()
        .unwrap()
        .replace('"', "&quot;");
    assert!(html.contains(&tag), "missing {tag}: {html}");
}

#[tokio::test]
async fn matching_uses_logical_library_membership_and_protects_shared_files() {
    let f = Fixture::new().await;
    sqlx::query(
        "INSERT INTO catalog_libraries(id,name,kind) VALUES ('logical-alias','Alias','mixed')",
    )
    .execute(&f.app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO library_sources(library_id,source_id) VALUES ('logical-alias',?)")
        .bind(&f.library)
        .execute(&f.app.db)
        .await
        .unwrap();
    let op = bearer(OPERATOR);
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/matches",
            Some(json!({
                "file":{"file_id":"file-a","file_revision":"rev-a"},
                "library_id":"logical-alias","provider":null
            })),
            &[
                ("authorization", &op),
                ("idempotency-key", "logical-match-000001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{:?}", created.body);
    let restricted = f.device(&["catalog:read", "catalog:write"], true).await;
    let rejected = f
        .call(
            "POST",
            "/api/v2/catalog/matches",
            Some(json!({
                "file":{"file_id":"file-a2","file_revision":"rev-a2"},
                "library_id":f.library,"provider":null
            })),
            &[
                ("authorization", &restricted),
                ("idempotency-key", "shared-match-0000001"),
            ],
        )
        .await;
    problem(&rejected, StatusCode::FORBIDDEN, "outside_scope");
    let decision = f
        .call(
            "PUT",
            &format!(
                "/api/v2/catalog/matches/{}/decision",
                created.body["id"].as_str().unwrap()
            ),
            Some(json!({"decision":"defer","candidate_id":null})),
            &[
                ("authorization", &restricted),
                ("if-match", created.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    problem(&decision, StatusCode::FORBIDDEN, "outside_scope");
    let metadata = f.call("PUT", "/api/v2/catalog/items/film-a/metadata/contributions/local", Some(json!({"values":{"title":"Changed"},"tags":[],"excluded_tags":[],"locked_fields":[]})), &[("authorization", &restricted), ("if-match", "\"r-1\"")]).await;
    problem(&metadata, StatusCode::FORBIDDEN, "outside_scope");
    let title: String = sqlx::query_scalar("SELECT title FROM items WHERE id='film-a'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(title, "Film");
}

#[tokio::test]
async fn match_replay_rechecks_current_file_visibility() {
    let f = Fixture::new().await;
    let device = f.device(&["catalog:read", "catalog:write"], true).await;
    let body = json!({"file":{"file_id":"file-a","file_revision":"rev-a"},"library_id":f.library,"provider":null});
    let headers = [
        ("authorization", device.as_str()),
        ("idempotency-key", "replay-current-scope-001"),
    ];
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/matches",
            Some(body.clone()),
            &headers,
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED);
    sqlx::query("UPDATE media_files SET library_id=? WHERE id='file-a'")
        .bind(&f.hidden)
        .execute(&f.app.db)
        .await
        .unwrap();
    let replay = f
        .call("POST", "/api/v2/catalog/matches", Some(body), &headers)
        .await;
    problem(&replay, StatusCode::NOT_FOUND, "not_found");
}
