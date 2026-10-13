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
        created.body["subject"],
        json!({"file_name":"Film (2001).mkv","item_id":"film-a","title":"Film"})
    );
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
    for (method, path) in [("POST", "/api/v2/admin/imports")] {
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
    // The proposal names the file's current title and file name, not an ID.
    assert!(html.contains("Film (Film (2001).mkv)"), "{html}");
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

#[tokio::test]
async fn artwork_upload_is_unselected_idempotent_and_selection_uses_item_etag() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let bytes = bytes.into_inner();
    let mut uploaded = Value::Null;
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v2/catalog/items/film-a/artwork")
            .header("host", "127.0.0.1:8787")
            .header("authorization", &op)
            .header("idempotency-key", "artwork-upload-00001")
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.clone()))
            .unwrap();
        let response = api::router(f.app.clone(), None)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        if uploaded.is_null() {
            uploaded = body;
        } else {
            assert_eq!(body, uploaded);
        }
    }
    let item = f
        .call("GET", "/api/v2/catalog/items/film-a", None, &auth(&op))
        .await;
    assert_eq!(item.body["artwork_id"], Value::Null);
    let body = json!({"role":"poster","asset_id":uploaded["id"]});
    let headers = [
        ("authorization", op.as_str()),
        ("if-match", item.headers["etag"].to_str().unwrap()),
    ];
    let selected = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film-a/artwork/selection",
            Some(body.clone()),
            &headers,
        )
        .await;
    assert_eq!(selected.status, StatusCode::OK, "{}", selected.body);
    assert_eq!(selected.body["artwork_id"], uploaded["id"]);
    assert_eq!(
        f.call(
            "PUT",
            "/api/v2/catalog/items/film-a/artwork/selection",
            Some(body),
            &headers
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let device = f.device(&["catalog:read"], true).await;
    let content = f
        .call(
            "GET",
            uploaded["content_url"].as_str().unwrap(),
            None,
            &auth(&device),
        )
        .await;
    assert_eq!(content.status, StatusCode::OK);
    assert_eq!(content.raw, bytes);
}

#[tokio::test]
async fn metadata_refresh_is_durable_replayable_and_cancelled_before_publication() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    std::fs::write(
        f._dir.path().join("media/Film (2001).nfo"),
        "<movie><title>Local title</title></movie>",
    )
    .unwrap();
    let path = "/api/v2/catalog/items/film-a/metadata/refresh";
    let h = [
        ("authorization", op.as_str()),
        ("idempotency-key", "metadata-refresh-001"),
    ];
    let job = f.call("POST", path, None, &h).await;
    assert_eq!(job.status, StatusCode::ACCEPTED, "{}", job.body);
    assert_eq!(f.call("POST", path, None, &h).await.body, job.body);
    let id = job.body["id"].as_str().unwrap();
    sqlx::query("UPDATE metadata_jobs SET phase='running',attempt=1 WHERE id=?")
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::metadata_jobs::recover(&f.app.db).await.unwrap();
    assert!(playscale::metadata_jobs::run_one(&f.app).await.unwrap());
    let completed = f
        .call("GET", &format!("/api/v2/jobs/{id}"), None, &auth(&op))
        .await;
    assert_eq!(completed.body["phase"], "completed");
    assert_eq!(completed.body["attempt_generation"], "2");
    let metadata = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a/metadata",
            None,
            &auth(&op),
        )
        .await;
    assert_eq!(metadata.body["values"]["title"], "Local title");
    std::fs::write(
        f._dir.path().join("media/Film (2001).nfo"),
        "<movie><title>Cancelled title</title></movie>",
    )
    .unwrap();
    let job = f
        .call(
            "POST",
            path,
            None,
            &[
                ("authorization", &op),
                ("idempotency-key", "metadata-refresh-002"),
            ],
        )
        .await;
    let id = job.body["id"].as_str().unwrap();
    let cancel = f
        .call(
            "POST",
            &format!("/api/v2/jobs/{id}/cancel"),
            None,
            &[
                ("authorization", &op),
                ("idempotency-key", "metadata-cancel-0001"),
            ],
        )
        .await;
    assert_eq!(cancel.status, StatusCode::OK);
    assert_eq!(cancel.body["phase"], "cancelled");
    assert!(!playscale::metadata_jobs::run_one(&f.app).await.unwrap());
    let metadata = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a/metadata",
            None,
            &auth(&op),
        )
        .await;
    assert_eq!(metadata.body["values"]["title"], "Local title");
}

#[tokio::test]
async fn metadata_refresh_failures_are_retryable_and_do_not_escape_worker() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let file = f._dir.path().join("media/Film (2001).nfo");
    std::fs::write(&file, "<broken>").unwrap();
    let job = f
        .call(
            "POST",
            "/api/v2/catalog/items/film-a/metadata/refresh",
            None,
            &[
                ("authorization", &op),
                ("idempotency-key", "metadata-failure-001"),
            ],
        )
        .await;
    assert_eq!(job.status, StatusCode::ACCEPTED);
    let id = job.body["id"].as_str().unwrap();
    assert!(playscale::metadata_jobs::run_one(&f.app).await.unwrap());
    let state = f
        .call("GET", &format!("/api/v2/jobs/{id}"), None, &auth(&op))
        .await;
    assert_eq!(state.body["phase"], "failed");
    std::fs::write(&file, "<movie><title>Recovered title</title></movie>").unwrap();
    sqlx::query("CREATE TRIGGER fail_metadata BEFORE INSERT ON metadata_documents WHEN NEW.source='nfo' BEGIN SELECT RAISE(ABORT,'injected publication failure'); END").execute(&f.app.db).await.unwrap();
    let path = format!("/api/v2/jobs/{id}/retry");
    assert_eq!(
        f.call(
            "POST",
            &path,
            None,
            &[
                ("authorization", &op),
                ("idempotency-key", "metadata-retry-00001")
            ]
        )
        .await
        .status,
        StatusCode::OK
    );
    assert!(
        playscale::metadata_jobs::run_one(&f.app).await.unwrap(),
        "publication failure is a job failure, not a worker failure"
    );
    assert_eq!(
        f.call("GET", &format!("/api/v2/jobs/{id}"), None, &auth(&op))
            .await
            .body["phase"],
        "failed"
    );
    sqlx::query("DROP TRIGGER fail_metadata")
        .execute(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        f.call(
            "POST",
            &path,
            None,
            &[
                ("authorization", &op),
                ("idempotency-key", "metadata-retry-00002")
            ]
        )
        .await
        .status,
        StatusCode::OK
    );
    assert!(playscale::metadata_jobs::run_one(&f.app).await.unwrap());
    assert_eq!(
        f.call("GET", &format!("/api/v2/jobs/{id}"), None, &auth(&op))
            .await
            .body["phase"],
        "completed"
    );
}

#[tokio::test]
async fn preparation_admission_pins_identity_and_replays_once() {
    let f = Fixture::new().await;
    sqlx::query("UPDATE media_files SET duration_seconds=10 WHERE id='file-a'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let op = format!("Bearer {OPERATOR}");
    let headers = [
        ("authorization", op.as_str()),
        ("idempotency-key", "prepare-one-0000000000000000"),
    ];
    let input = json!({"source":{"file_id":"file-a","file_revision":"rev-a"},"timeline_id":"ed-film-a","recipe_id":"remux_mp4","tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}});
    let first = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(input.clone()),
            &headers,
        )
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    assert_eq!(first.body["kind"], "rendition");
    let replay = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(input.clone()),
            &headers,
        )
        .await;
    assert_eq!(replay.body, first.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let mut changed = input.clone();
    changed["recipe_id"] = json!("audio_aac");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/processing/preparations",
            Some(changed),
            &headers
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM preparations")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let list = f
        .call(
            "GET",
            "/api/v2/processing/renditions",
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(list.body["items"][0]["status"], "pending");
    // Many renditions of another timeline: the timeline filter applies
    // before the page limit, so one timeline's rows stay one short read.
    sqlx::query("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<250) INSERT INTO processing_jobs(id,source_file_id,source_revision,recipe,backend,idempotency_key,phase,created_at,updated_at,requester_id) SELECT printf('0bulk-%04d',i),source_file_id,source_revision,recipe,backend,printf('v2-bulk-%04d',i),'completed',created_at,updated_at,requester_id FROM processing_jobs, n WHERE processing_jobs.id=?")
        .bind(first.body["id"].as_str().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO preparations(job_id,timeline_id,recipe_digest,tracks_json) SELECT j.id,'ed-film-b',p.recipe_digest,p.tracks_json FROM processing_jobs j, preparations p WHERE j.id LIKE '0bulk-%'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let get = |path: &'static str| {
        let f = &f;
        let op = op.clone();
        async move { f.call("GET", path, None, &[("authorization", &op)]).await }
    };
    let mine = get("/api/v2/processing/renditions?timeline_id=ed-film-a&limit=1").await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    assert_eq!(mine.body["items"][0]["id"], first.body["id"]);
    assert!(mine.body["next_cursor"].is_null());
    let other = get("/api/v2/processing/renditions?timeline_id=ed-film-b&limit=200").await;
    assert_eq!(other.body["items"].as_array().unwrap().len(), 200);
    assert!(
        other.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["timeline_id"] == "ed-film-b")
    );
    assert!(other.body["next_cursor"].is_string());
    assert!(
        get("/api/v2/processing/renditions?timeline_id=unknown")
            .await
            .body["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        get("/api/v2/processing/renditions?timeline_id=no%20spaces")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("DELETE FROM preparations WHERE job_id LIKE '0bulk-%'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut wrong = input.clone();
    wrong["timeline_id"] = json!("ed-film-b");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/processing/preparations",
            Some(wrong),
            &[
                ("authorization", &op),
                ("idempotency-key", "wrong-timeline-0000000000000000")
            ]
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    let mut track = input;
    track["tracks"]["audio_track_id"] = json!("a9");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/processing/preparations",
            Some(track),
            &[
                ("authorization", &op),
                ("idempotency-key", "wrong-track-0000000000000000")
            ]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    sqlx::query("UPDATE media_files SET revision='replaced' WHERE id='file-a'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let list = f
        .call(
            "GET",
            "/api/v2/processing/renditions",
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(list.body["items"][0]["status"], "stale");
}

#[tokio::test]
async fn schedules_preserve_disabled_rows_and_atomically_claim_logical_scans() {
    let f = Fixture::new().await;
    let op = format!("Bearer {OPERATOR}");
    let input = json!({"library_id":f.library,"task":"scan","interval_seconds":60,"enabled":true});
    let first = f
        .call(
            "POST",
            "/api/v2/schedules",
            Some(input.clone()),
            &[
                ("authorization", &op),
                ("idempotency-key", "schedule-one-0000000000000000"),
            ],
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let id = first.body["id"].as_str().unwrap();
    let path = format!("/api/v2/schedules/{id}");
    let replay = f
        .call(
            "POST",
            "/api/v2/schedules",
            Some(input.clone()),
            &[
                ("authorization", &op),
                ("idempotency-key", "schedule-one-0000000000000000"),
            ],
        )
        .await;
    assert_eq!(replay.body, first.body);
    sqlx::query("UPDATE api_schedules SET next_run=0 WHERE id=?")
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM scan_requests")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut disabled = input.clone();
    disabled["enabled"] = json!(false);
    let updated = f
        .call(
            "PUT",
            &path,
            Some(disabled),
            &[("authorization", &op), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.body);
    assert_eq!(updated.body["enabled"], false);
    assert_eq!(
        f.call(
            "DELETE",
            &path,
            None,
            &[("authorization", &op), ("if-match", "\"r-1\"")]
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        f.call(
            "DELETE",
            &path,
            None,
            &[("authorization", &op), ("if-match", "\"r-2\"")]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn backup_request_replay_publishes_one_verified_snapshot_with_asset_inventory() {
    let f = Fixture::new().await;
    let op = format!("Bearer {OPERATOR}");
    let headers = [
        ("authorization", op.as_str()),
        ("idempotency-key", "backup-one-0000000000000000"),
    ];
    let body = json!({"include_generated_media":true});
    let first = f
        .call(
            "POST",
            "/api/v2/admin/backups",
            Some(body.clone()),
            &headers,
        )
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    let replay = f
        .call("POST", "/api/v2/admin/backups", Some(body), &headers)
        .await;
    assert_eq!(first.body, replay.body);
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/admin/backups",
            Some(json!({"include_generated_media":false})),
            &headers
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert!(playscale::v2::admin::run_backup(&f.app).await.unwrap());
    let list = f
        .call(
            "GET",
            "/api/v2/admin/backups",
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(list.body["items"][0]["status"], "ready", "{}", list.body);
    let id = first.body["id"].as_str().unwrap();
    let manifest = std::fs::read(
        f.app
            .storage
            .root
            .join("backups")
            .join(format!("request-{id}"))
            .join("manifest.json"),
    )
    .unwrap();
    assert_eq!(
        list.body["items"][0]["manifest_digest"],
        format!("sha256:{:x}", Sha256::digest(&manifest))
    );
    let value: Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(value["include_generated_media"], true);
    assert!(value["assets"].is_array());
    // Lost acknowledgement: repeat the running attempt against the immutable path.
    sqlx::query("UPDATE backup_jobs SET phase='running',manifest_digest=NULL WHERE id=?")
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    assert!(playscale::v2::admin::run_backup(&f.app).await.unwrap());
    assert_eq!(
        std::fs::read_dir(f.app.storage.root.join("backups"))
            .unwrap()
            .count(),
        1
    );
    let storage = f
        .call(
            "GET",
            "/api/v2/admin/storage",
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(storage.status, StatusCode::OK, "{}", storage.body);
    assert!(
        storage.body["database_bytes"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > 0
    );
}

#[tokio::test]
async fn empty_schedule_does_not_starve_healthy_library_and_fanout_is_resumable() {
    let f = Fixture::new().await;
    sqlx::query("INSERT INTO catalog_libraries(id,name,kind) VALUES ('empty','Empty','mixed')")
        .execute(&f.app.db)
        .await
        .unwrap();
    for (id, library, task) in [
        ("empty-schedule", "empty", "scan"),
        ("healthy-schedule", f.library.as_str(), "scan"),
    ] {
        sqlx::query("INSERT INTO api_schedules(id,library_id,task,interval_seconds,enabled,next_run,requester_id) VALUES (?,?,?,60,1,0,'operator')").bind(id).bind(library).bind(task).execute(&f.app.db).await.unwrap();
    }
    playscale::maintenance::tick(&f.app).await.unwrap();
    let enabled: bool =
        sqlx::query_scalar("SELECT enabled FROM api_schedules WHERE id='empty-schedule'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert!(!enabled);
    let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM scan_requests WHERE library_id=?")
        .bind(&f.library)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(requests, 1);
    // A full library pass is split into durable 200-item claims.
    for index in 0..201 {
        f.work(
            &format!("batch-{index:03}"),
            "Batch",
            &f.library,
            &format!("batch-{index:03}.mp4"),
            "rev",
        )
        .await;
    }
    sqlx::query("INSERT INTO api_schedules(id,library_id,task,interval_seconds,enabled,next_run,requester_id) VALUES ('metadata',?,'metadata_refresh',60,1,0,'operator')").bind(&f.library).execute(&f.app.db).await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    let first: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(first, 200);
    let progress: i64 = sqlx::query_scalar("SELECT count(*) FROM schedule_dispatch_progress")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(progress, 1);
    playscale::maintenance::tick(&f.app).await.unwrap();
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM metadata_jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(total, 203);
    let progress: i64 = sqlx::query_scalar("SELECT count(*) FROM schedule_dispatch_progress")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(progress, 0);
}

#[tokio::test]
async fn backup_retention_preserves_history_and_corrupt_recovery_fails_closed() {
    let mut f = Fixture::new().await;
    Arc::get_mut(&mut f.app.storage)
        .unwrap()
        .settings
        .backups_keep = 1;
    let op = format!("Bearer {OPERATOR}");
    let mut ids = vec![];
    for index in 0..2 {
        let result = f
            .call(
                "POST",
                "/api/v2/admin/backups",
                Some(json!({"include_generated_media":false})),
                &[
                    ("authorization", &op),
                    ("idempotency-key", &format!("retention-request-{index}")),
                ],
            )
            .await;
        assert_eq!(result.status, StatusCode::ACCEPTED);
        let id = result.body["id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE backup_jobs SET created_at=? WHERE id=?")
            .bind(index)
            .bind(&id)
            .execute(&f.app.db)
            .await
            .unwrap();
        assert!(playscale::v2::admin::run_backup(&f.app).await.unwrap());
        ids.push(id);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM backup_jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 2);
    assert!(
        !f.app
            .storage
            .root
            .join("backups")
            .join(format!("request-{}", ids[0]))
            .exists()
    );
    let path = f
        .app
        .storage
        .root
        .join("backups")
        .join(format!("request-{}", ids[1]))
        .join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["assets"] = json!([{"asset_id":"invented","bytes":"1","storage":"database"}]);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    sqlx::query("UPDATE backup_jobs SET phase='running',manifest_digest=NULL WHERE id=?")
        .bind(&ids[1])
        .execute(&f.app.db)
        .await
        .unwrap();
    playscale::v2::admin::run_backup(&f.app).await.unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM backup_jobs WHERE id=?")
        .bind(&ids[1])
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(phase, "failed");
}

#[tokio::test]
async fn interrupted_retention_retries_without_new_backup_request() {
    let f = Fixture::new().await;
    let op = format!("Bearer {OPERATOR}");
    let response = f
        .call(
            "POST",
            "/api/v2/admin/backups",
            Some(json!({"include_generated_media":false})),
            &[
                ("authorization", &op),
                ("idempotency-key", "retention-restart"),
            ],
        )
        .await;
    let id = response.body["id"].as_str().unwrap();
    playscale::v2::admin::run_backup(&f.app).await.unwrap();
    let path = f
        .app
        .storage
        .root
        .join("backups")
        .join(format!("request-{id}"));
    assert!(path.exists());
    sqlx::query("UPDATE backup_jobs SET error='backup_expired' WHERE id=?")
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    assert!(!playscale::v2::admin::run_backup(&f.app).await.unwrap());
    assert!(!path.exists());
    let error: String = sqlx::query_scalar("SELECT error FROM backup_jobs WHERE id=?")
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(error, "backup_reclaimed");
}

#[tokio::test]
async fn preparation_scope_keys_and_first_video_hdr_are_enforced() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let device = f
        .device(&["catalog:read", "processing:request"], true)
        .await;
    sqlx::query("UPDATE media_files SET duration_seconds=10")
        .execute(&f.app.db)
        .await
        .unwrap();
    let input = json!({"source":{"file_id":"file-a","file_revision":"rev-a"},"timeline_id":"ed-film-a","recipe_id":"remux_mp4","tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}});
    let mut ids = vec![];
    for who in [&op, &device] {
        let r = f
            .call(
                "POST",
                "/api/v2/processing/preparations",
                Some(input.clone()),
                &[
                    ("authorization", who),
                    ("idempotency-key", "shared-key-0000000000000000"),
                ],
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        ids.push(r.body["id"].clone());
    }
    assert_ne!(ids[0], ids[1]);
    let mut hidden = input.clone();
    hidden["source"] = json!({"file_id":"file-b","file_revision":"rev-b"});
    hidden["timeline_id"] = json!("ed-film-b");
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/processing/preparations",
            Some(hidden),
            &[
                ("authorization", &device),
                ("idempotency-key", "hidden-0000000000000000")
            ]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/admin/backups",
            Some(json!({"include_generated_media":false})),
            &[
                ("authorization", &device),
                ("idempotency-key", "forbidden-backup")
            ]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let tracks = json!([{"index":0,"kind":"video","codec":"hevc","width":1920,"height":1080,"color_transfer":"smpte2084"},{"index":1,"kind":"video","codec":"h264","width":1920,"height":1080,"color_transfer":"bt709"}]);
    sqlx::query("UPDATE media_files SET tracks_json=? WHERE id='file-a'")
        .bind(tracks.to_string())
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut hdr = input;
    hdr["recipe_id"] = json!("h264720p");
    let rejected = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(hdr),
            &[
                ("authorization", &op),
                ("idempotency-key", "hdr-first-0000000000000000"),
            ],
        )
        .await;
    assert_eq!(
        rejected.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        rejected.body
    );
    assert_eq!(rejected.body["code"], "preparation_video_unsupported");
}

#[tokio::test]
async fn parameterized_preparation_persists_profile_and_fences_changed_inputs() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    let tracks = json!([{"index":0,"kind":"video","codec":"h264","width":640,"height":360,"color_transfer":"bt709"}]);
    sqlx::query("UPDATE media_files SET duration_seconds=10,tracks_json=? WHERE id='file-a'")
        .bind(tracks.to_string())
        .execute(&f.app.db)
        .await
        .unwrap();
    let profile = json!({"version":1,"codec":"h264","max_width":320,"max_height":180,"video_bitrate":500000,"audio_bitrate":96000,"frame_rate":{"numerator":30,"denominator":1}});
    let input = json!({"source":{"file_id":"file-a","file_revision":"rev-a"},"timeline_id":"ed-film-a","recipe_id":"video_profile","backend":"software","video_profile":profile,"tracks":{"audio_component_id":null,"subtitle_component_id":null,"subtitle_policy":"off","audio_track_id":null,"subtitle_track_id":null}});
    let headers = [
        ("authorization", op.as_str()),
        ("idempotency-key", "profile-preparation-0001"),
    ];
    let first = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(input.clone()),
            &headers,
        )
        .await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    let replay = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(input.clone()),
            &headers,
        )
        .await;
    assert_eq!(replay.body, first.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let row: (String, String, String) =
        sqlx::query_as("SELECT recipe,backend,video_profile FROM processing_jobs WHERE id=?")
            .bind(first.body["id"].as_str().unwrap())
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(row.0, "video_profile");
    assert_eq!(row.1, "software");
    assert_eq!(serde_json::from_str::<Value>(&row.2).unwrap(), profile);
    let mut changed = input.clone();
    changed["video_profile"]["max_width"] = json!(640);
    let conflict = f
        .call(
            "POST",
            "/api/v2/processing/preparations",
            Some(changed),
            &headers,
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT, "{}", conflict.body);
    for (field, value) in [
        ("video_profile", Value::Null),
        ("backend", json!("automatic")),
    ] {
        let mut invalid = input.clone();
        invalid[field] = value;
        let result = f
            .call(
                "POST",
                "/api/v2/processing/preparations",
                Some(invalid),
                &[
                    ("authorization", &op),
                    ("idempotency-key", "invalid-profile-0001"),
                ],
            )
            .await;
        assert_eq!(
            result.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            result.body
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM preparations")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

/// Preview schedules (6C part 2) queue one job per eligible revision of the
/// library's originals: never alongside an active job or published
/// previews, never for a file without pictures, and never beyond the
/// preview queue, resuming where a full queue stopped them.
#[tokio::test]
async fn preview_schedules_queue_each_revision_once_within_the_queue() {
    let f = Fixture::new().await;
    for name in ["a", "b", "c", "d"] {
        f.work(
            &format!("pv-{name}"),
            name,
            &f.library,
            &format!("pv-{name}.mp4"),
            &format!("rev-{name}"),
        )
        .await;
    }
    let video =
        r#"[{"index":0,"kind":"video","codec":"h264","language":null,"width":1920,"height":1080}]"#;
    sqlx::query("UPDATE media_files SET duration_seconds=60,tracks_json=? WHERE id IN ('file-pv-a','file-pv-b','file-pv-c')")
        .bind(video)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_files SET duration_seconds=60 WHERE id='file-pv-d'")
        .execute(&f.app.db)
        .await
        .unwrap();
    // c's revision already has published previews.
    sqlx::query("INSERT INTO previews(file_id,file_revision,job_id,interval_ms,tile_width,tile_height,columns,rows,tiles,sheets,created_at) VALUES ('file-pv-c','rev-c','old',10000,160,90,10,10,6,1,1)")
        .execute(&f.app.db)
        .await
        .unwrap();
    let op = format!("Bearer {OPERATOR}");
    let created = f
        .call(
            "POST",
            "/api/v2/schedules",
            Some(json!({"library_id": f.library, "task": "preview", "interval_seconds": 3600, "enabled": true})),
            &[("authorization", &op), ("idempotency-key", "preview-schedule-000000000001")],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    let due = || {
        sqlx::query("UPDATE api_schedules SET next_run=0 WHERE id=?")
            .bind(&id)
            .execute(&f.app.db)
    };
    let queued = || async {
        sqlx::query_scalar::<_, String>(
            "SELECT file_id FROM preview_jobs WHERE phase='queued' AND file_id LIKE 'file-pv-%' ORDER BY file_id",
        )
        .fetch_all(&f.app.db)
        .await
        .unwrap()
    };
    due().await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    assert_eq!(queued().await, ["file-pv-a", "file-pv-b"]);
    // The next run adds nothing: those revisions have active jobs.
    due().await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    assert_eq!(queued().await, ["file-pv-a", "file-pv-b"]);
    // A full queue stops the run where it is, and the next one resumes.
    sqlx::query("UPDATE preview_jobs SET phase='cancelled'")
        .execute(&f.app.db)
        .await
        .unwrap();
    for n in 0..99 {
        sqlx::query("INSERT INTO preview_jobs(id,file_id,file_revision,requester_id,created_at) VALUES (?,?,'r','operator',1)")
            .bind(format!("busy-{n}"))
            .bind(format!("other-{n}"))
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    due().await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    assert_eq!(queued().await, ["file-pv-a"]);
    let progress: i64 = sqlx::query_scalar("SELECT count(*) FROM schedule_dispatch_progress")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(progress, 1);
    // Blocked, it backed off rather than staying due.
    let next: i64 = sqlx::query_scalar("SELECT next_run FROM api_schedules WHERE id=?")
        .bind(&id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert!(next > playscale::now() + 200, "{next}");
    sqlx::query("UPDATE preview_jobs SET phase='cancelled' WHERE id LIKE 'busy-%'")
        .execute(&f.app.db)
        .await
        .unwrap();
    due().await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    assert_eq!(queued().await, ["file-pv-a", "file-pv-b"]);
    let progress: i64 = sqlx::query_scalar("SELECT count(*) FROM schedule_dispatch_progress")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(progress, 0);
    // Many blocked preview schedules never starve the others: blocked, they
    // back off, and a later-due scan schedule is dispatched.
    for n in 0..100 {
        sqlx::query("INSERT INTO preview_jobs(id,file_id,file_revision,requester_id,created_at) VALUES (?,?,'r2','operator',1)")
            .bind(format!("full-{n}"))
            .bind(format!("full-{n}"))
            .execute(&f.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO api_schedules(id,library_id,task,interval_seconds,enabled,next_run,requester_id) VALUES (?,?,'preview',3600,1,0,'operator')")
            .bind(format!("blocked-{n}"))
            .bind(&f.library)
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE api_schedules SET next_run=1 WHERE id=?")
        .bind(&id)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO api_schedules(id,library_id,task,interval_seconds,enabled,next_run,requester_id) VALUES ('later-scan',?,'scan',3600,1,1,'operator')")
        .bind(&f.library)
        .execute(&f.app.db)
        .await
        .unwrap();
    let scans =
        || sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scan_requests").fetch_one(&f.app.db);
    let before = scans().await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    playscale::maintenance::tick(&f.app).await.unwrap();
    assert_eq!(
        scans().await.unwrap(),
        before + 1,
        "the scan schedule was starved"
    );
}
