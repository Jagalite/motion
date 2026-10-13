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
                AccessMode::TrustedHousehold,
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

    /// An item with one edition `<id>-ed` and one file per `(library, revision)`.
    async fn item(&self, id: &str, title: &str, files: &[(&str, &str)]) {
        let db = &self.app.db;
        sqlx::query("INSERT INTO items(id,title,kind) VALUES (?,?,'video')")
            .bind(id)
            .bind(title)
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item_origins(item_id,title) VALUES (?,?)")
            .bind(id)
            .bind(title)
            .execute(db)
            .await
            .unwrap();
        if files.is_empty() {
            return;
        }
        sqlx::query("INSERT INTO editions(id,item_id,label) VALUES (?,?,'Original')")
            .bind(format!("{id}-ed"))
            .bind(id)
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO timelines(id,edition_id) VALUES (?,?)")
            .bind(format!("{id}-ed"))
            .bind(format!("{id}-ed"))
            .execute(db)
            .await
            .unwrap();
        for (n, (library, revision)) in files.iter().enumerate() {
            sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES (?,?,?,?,?,'fp',1)")
                .bind(format!("{id}-f{n}"))
                .bind(format!("{id}-ed"))
                .bind(library)
                .bind(format!("{id}/{n}.mkv"))
                .bind(revision)
                .execute(db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO media_versions(id,timeline_id,origin,equivalence) SELECT min(id),edition_id,'original','declared' FROM media_files WHERE edition_id=? GROUP BY edition_id,revision")
            .bind(format!("{id}-ed")).execute(db).await.unwrap();
        sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) SELECT v.id,1,f.id,f.revision FROM media_versions v JOIN media_files f ON f.id=v.id WHERE v.timeline_id=?")
            .bind(format!("{id}-ed")).execute(db).await.unwrap();
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
    assert_eq!(
        reply.body["request_id"].as_str(),
        reply.headers["x-request-id"].to_str().ok()
    );
}

fn ids(reply: &Reply) -> Vec<String> {
    reply.body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{:?}", reply.body))
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Items in A, in B, in both, and a manual item with no files.
async fn catalog(f: &Fixture) {
    f.item("film-a", "Alpha Film", &[(&f.a, "r1")]).await;
    f.item("film-b", "Beta Film", &[(&f.b, "r1")]).await;
    f.item("both", "Shared Film", &[(&f.a, "r1"), (&f.b, "r2")])
        .await;
    f.item("manual", "Manual", &[]).await;
}

#[tokio::test]
async fn browse_search_and_read_apply_the_catalog_scope_in_the_query() {
    let f = Fixture::new().await;
    catalog(&f).await;
    let operator = bearer(OPERATOR);
    let all = f
        .call(
            "GET",
            "/api/v2/catalog/items",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(all.status, StatusCode::OK, "{:?}", all.body);
    assert_eq!(ids(&all), ["film-a", "film-b", "manual", "both"]);
    assert!(all.body["next_cursor"].is_null());
    assert!(all.body["read_revision"].is_string());
    assert!(all.body["event_cursor"].is_string());
    let shared = &all.body["items"][3];
    let mut libraries = vec![f.a.clone(), f.b.clone()];
    libraries.sort();
    assert_eq!(
        shared,
        &json!({"id":"both","revision":shared["revision"],"kind":"video","title":"Shared Film",
            "library_ids":libraries,"availability":"available","external_ids":[],"artwork_id":null,
            "default_timeline_id":"both-ed","match_state":"unmatched","parent_ids":[]})
    );
    assert_eq!(all.body["items"][2]["availability"], "unknown");
    // Keyset pages: title then ID.
    let first = f
        .call(
            "GET",
            "/api/v2/catalog/items?limit=2",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&first), ["film-a", "film-b"]);
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let second = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items?limit=2&cursor={cursor}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&second), ["manual", "both"]);
    let bad_cursor = f
        .call(
            "GET",
            "/api/v2/catalog/items?cursor=%21%21",
            None,
            &[("authorization", &operator)],
        )
        .await;
    problem(&bad_cursor, StatusCode::BAD_REQUEST, "invalid_cursor");

    // A device restricted to library A neither sees nor counts B-only rows,
    // and B membership of a shared item is not disclosed.
    let restricted = f.device(&["catalog:read"], &[&f.a]).await;
    let page = f
        .call(
            "GET",
            "/api/v2/catalog/items?limit=1",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(ids(&page), ["film-a"]);
    let cursor = page.body["next_cursor"].as_str().unwrap();
    let rest = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items?cursor={cursor}"),
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(ids(&rest), ["both"]);
    assert_eq!(rest.body["items"][0]["library_ids"], json!([f.a]));
    let by_b = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items?library_id={}", f.b),
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(ids(&by_b), Vec::<String>::new());
    let search = f
        .call(
            "GET",
            "/api/v2/catalog/search?q=FILM",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(ids(&search), ["film-a", "both"]);
    let search_all = f
        .call(
            "GET",
            "/api/v2/catalog/search?q=beta",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&search_all), ["film-b"]);
    let editions = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-b/editions",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    problem(&editions, StatusCode::NOT_FOUND, "not_found");

    // Single reads: inaccessible and missing are indistinguishable.
    for path in ["film-b", "manual", "missing"] {
        let reply = f
            .call(
                "GET",
                &format!("/api/v2/catalog/items/{path}"),
                None,
                &[("authorization", &restricted)],
            )
            .await;
        problem(&reply, StatusCode::NOT_FOUND, "not_found");
    }
    let read = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(
        read.headers["etag"],
        format!("\"r-{}\"", read.body["revision"].as_str().unwrap())
    );

    // Filters and validation.
    let videos = f
        .call(
            "GET",
            "/api/v2/catalog/items?kind=video",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&videos).len(), 4);
    let albums = f
        .call(
            "GET",
            "/api/v2/catalog/items?kind=album",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&albums), Vec::<String>::new());
    for (path, code) in [
        (
            "/api/v2/catalog/items?sort=release_date",
            "unsupported_sort",
        ),
        ("/api/v2/catalog/items?unknown=1", "invalid_request"),
        ("/api/v2/catalog/items?limit=0", "invalid_limit"),
        ("/api/v2/catalog/items?library_id=a%20b", "invalid_query"),
        ("/api/v2/catalog/search", "invalid_request"),
        ("/api/v2/catalog/search?q=", "invalid_query"),
    ] {
        let reply = f
            .call("GET", path, None, &[("authorization", &operator)])
            .await;
        problem(&reply, StatusCode::BAD_REQUEST, code);
    }

    // 401 and 403.
    let unauthenticated = f.call("GET", "/api/v2/catalog/items", None, &[]).await;
    problem(
        &unauthenticated,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let events_only = f.device(&["events:read"], &[&f.a]).await;
    for path in [
        "/api/v2/catalog/items",
        "/api/v2/catalog/search?q=a",
        "/api/v2/catalog/items/film-a",
        "/api/v2/catalog/items/film-a/editions",
    ] {
        let reply = f
            .call("GET", path, None, &[("authorization", &events_only)])
            .await;
        problem(&reply, StatusCode::FORBIDDEN, "permission_denied");
    }
}

#[tokio::test]
async fn retired_ids_resolve_to_their_live_work() {
    let f = Fixture::new().await;
    catalog(&f).await;
    sqlx::query("INSERT INTO catalog_receipts VALUES ('rc','catalog:merge',0,'{}')")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_aliases VALUES ('manual','film-a','rc')")
        .execute(&f.app.db)
        .await
        .unwrap();
    let operator = bearer(OPERATOR);
    let read = f
        .call(
            "GET",
            "/api/v2/catalog/items/manual",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(read.body["id"], "film-a");
    let list = f
        .call(
            "GET",
            "/api/v2/catalog/items",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert!(!ids(&list).contains(&"manual".to_string()));
}

#[tokio::test]
async fn items_are_created_and_replaced_conditionally_and_idempotently() {
    let f = Fixture::new().await;
    catalog(&f).await;
    let operator = bearer(OPERATOR);
    let body = json!({"kind":"movie","title":"  New Film ","library_ids":[]});
    let key = ("idempotency-key", "create-item-0001");
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(body.clone()),
            &[("authorization", &operator), key],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    let id = created.body["id"].as_str().unwrap().to_owned();
    assert_eq!(created.body["title"], "New Film");
    assert_eq!(created.body["kind"], "movie");
    assert_eq!(created.body["library_ids"], json!([]));
    assert_eq!(created.body["default_timeline_id"], Value::Null);
    let revision = created.body["revision"].as_str().unwrap().to_owned();
    assert_eq!(created.headers["etag"], format!("\"r-{revision}\""));
    let replay = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(body),
            &[("authorization", &operator), key],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, created.body);
    let reused = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"Other","library_ids":[]})),
            &[("authorization", &operator), key],
        )
        .await;
    problem(&reused, StatusCode::CONFLICT, "idempotency_key_reused");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM items WHERE title='New Film'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 1);

    for (input, status, code) in [
        (
            json!({"kind":"movie","title":"X","library_ids":[],"extra":1}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
        (
            json!({"kind":"movie","title":"X","library_ids":[f.a]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "derived_library_membership",
        ),
        (
            json!({"kind":"album","title":"X","library_ids":[]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_kind",
        ),
        (
            json!({"kind":"episode","title":"X","library_ids":[]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_structure",
        ),
        (
            json!({"kind":"movie","title":" ","library_ids":[]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_field",
        ),
    ] {
        let reply = f
            .call(
                "POST",
                "/api/v2/catalog/items",
                Some(input),
                &[
                    ("authorization", &operator),
                    ("idempotency-key", "create-item-0002"),
                ],
            )
            .await;
        problem(&reply, status, code);
    }
    let no_key = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"X","library_ids":[]})),
            &[("authorization", &operator)],
        )
        .await;
    problem(&no_key, StatusCode::BAD_REQUEST, "idempotency_key_required");
    let anonymous = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"X","library_ids":[]})),
            &[("idempotency-key", "create-item-0003")],
        )
        .await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let reader = f.device(&["catalog:read"], &[&f.a]).await;
    let forbidden = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"X","library_ids":[]})),
            &[
                ("authorization", &reader),
                ("idempotency-key", "create-item-0003"),
            ],
        )
        .await;
    problem(&forbidden, StatusCode::FORBIDDEN, "permission_denied");
    // A library-restricted writer cannot create an item outside every library.
    let writer = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    let scoped = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"movie","title":"X","library_ids":[]})),
            &[
                ("authorization", &writer),
                ("idempotency-key", "create-item-0004"),
            ],
        )
        .await;
    problem(&scoped, StatusCode::FORBIDDEN, "outside_library_scope");

    // Replacement.
    let path = format!("/api/v2/catalog/items/{id}");
    let input = json!({"kind":"series","title":"Renamed","library_ids":[]});
    let missing = f
        .call(
            "PUT",
            &path,
            Some(input.clone()),
            &[("authorization", &operator)],
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
            &path,
            Some(input.clone()),
            &[("authorization", &operator), ("if-match", "\"r-999\"")],
        )
        .await;
    problem(
        &stale,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let tag = format!("\"r-{revision}\"");
    let replaced = f
        .call(
            "PUT",
            &path,
            Some(input.clone()),
            &[("authorization", &operator), ("if-match", &tag)],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    assert_eq!(replaced.body["title"], "Renamed");
    assert_eq!(replaced.body["kind"], "series");
    let next: u64 = replaced.body["revision"].as_str().unwrap().parse().unwrap();
    assert_eq!(next, revision.parse::<u64>().unwrap() + 1);
    // An identical replacement is not a new revision; the old ETag is stale.
    let tag = format!("\"r-{next}\"");
    let same = f
        .call(
            "PUT",
            &path,
            Some(input.clone()),
            &[("authorization", &operator), ("if-match", &tag)],
        )
        .await;
    assert_eq!(same.headers["etag"], tag.as_str());
    let old = f
        .call(
            "PUT",
            &path,
            Some(input),
            &[
                ("authorization", &operator),
                ("if-match", &format!("\"r-{revision}\"")),
            ],
        )
        .await;
    problem(&old, StatusCode::PRECONDITION_FAILED, "precondition_failed");
    for (input, code) in [
        (
            json!({"kind":"series","title":"R","library_ids":[f.a]}),
            "derived_library_membership",
        ),
        (
            json!({"kind":"episode","title":"R","library_ids":[]}),
            "invalid_structure",
        ),
        (
            json!({"kind":"other","title":"R","library_ids":[]}),
            "unsupported_kind",
        ),
        (
            json!({"kind":"series","title":"R","library_ids":[],"parent_ids":[]}),
            "invalid_body",
        ),
    ] {
        let reply = f
            .call(
                "PUT",
                &path,
                Some(input),
                &[("authorization", &operator), ("if-match", &tag)],
            )
            .await;
        problem(&reply, StatusCode::UNPROCESSABLE_ENTITY, code);
    }

    // Scope on replacement: B-only is missing, A+B is not wholly writable.
    let film_b = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film-b",
            Some(json!({"kind":"video","title":"X","library_ids":[f.b]})),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await;
    problem(&film_b, StatusCode::NOT_FOUND, "not_found");
    let both = f
        .call(
            "PUT",
            "/api/v2/catalog/items/both",
            Some(json!({"kind":"video","title":"X","library_ids":[f.a]})),
            &[("authorization", &writer), ("if-match", "\"r-1\"")],
        )
        .await;
    problem(&both, StatusCode::FORBIDDEN, "outside_library_scope");
    let film_a = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a",
            None,
            &[("authorization", &writer)],
        )
        .await;
    let etag = film_a.headers["etag"].to_str().unwrap().to_owned();
    let mine = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film-a",
            Some(json!({"kind":"movie","title":"Alpha","library_ids":[f.a]})),
            &[("authorization", &writer), ("if-match", &etag)],
        )
        .await;
    assert_eq!(mine.status, StatusCode::OK, "{:?}", mine.body);
    assert_eq!(mine.body["kind"], "movie");
    let reader_put = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film-a",
            Some(json!({"kind":"movie","title":"Alpha","library_ids":[f.a]})),
            &[("authorization", &reader), ("if-match", &etag)],
        )
        .await;
    problem(&reader_put, StatusCode::FORBIDDEN, "permission_denied");
}

#[tokio::test]
async fn editions_are_scoped_and_created_under_the_item_etag() {
    let f = Fixture::new().await;
    catalog(&f).await;
    sqlx::query("INSERT INTO editions(id,item_id,label) VALUES ('both-ed2','both','Director')")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO timelines(id,edition_id) VALUES ('both-ed2','both-ed2')")
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut conn = f.app.db.acquire().await.unwrap();
    playscale::curation::reassign_file(&mut conn, "both-f1", "both-ed2")
        .await
        .unwrap();
    drop(conn);
    let operator = bearer(OPERATOR);
    let restricted = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    let all = f
        .call(
            "GET",
            "/api/v2/catalog/items/both/editions",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(all.status, StatusCode::OK, "{:?}", all.body);
    assert_eq!(ids(&all), ["both-ed", "both-ed2"]);
    assert_eq!(
        all.body["items"][0],
        json!({"id":"both-ed","revision":"1","item_id":"both","label":"Original","kind":"default","timeline_ids":["both-ed"]})
    );
    let card = f
        .call(
            "GET",
            "/api/v2/catalog/items/both",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(card.body["default_timeline_id"], "both-ed");
    let hidden_timeline = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/both-ed2",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(hidden_timeline.status, StatusCode::NOT_FOUND);
    let timelines = f
        .call(
            "GET",
            "/api/v2/catalog/items/both/timelines?limit=1",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(ids(&timelines), ["both-ed"]);
    assert!(timelines.body["next_cursor"].is_null());
    // The edition whose only file is in library B is not disclosed.
    let scoped = f
        .call(
            "GET",
            "/api/v2/catalog/items/both/editions",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_eq!(ids(&scoped), ["both-ed"]);
    let paged = f
        .call(
            "GET",
            "/api/v2/catalog/items/both/editions?limit=1",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(paged.body["next_cursor"], "both-ed");

    let item = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    let etag = item.headers["etag"].to_str().unwrap().to_owned();
    let path = "/api/v2/catalog/items/film-a/editions";
    let body = json!({"label":"Extended","kind":"default"});
    let key = ("idempotency-key", "create-edition-01");
    let no_match = f
        .call(
            "POST",
            path,
            Some(body.clone()),
            &[("authorization", &restricted), key],
        )
        .await;
    problem(
        &no_match,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    let stale = f
        .call(
            "POST",
            path,
            Some(body.clone()),
            &[
                ("authorization", &restricted),
                key,
                ("if-match", "\"r-999\""),
            ],
        )
        .await;
    problem(
        &stale,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let created = f
        .call(
            "POST",
            path,
            Some(body.clone()),
            &[("authorization", &restricted), key, ("if-match", &etag)],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    assert_eq!(created.headers["etag"], "\"r-1\"");
    assert_eq!(created.body["kind"], "default");
    assert_eq!(created.body["item_id"], "film-a");
    let edition = created.body["id"].as_str().unwrap();
    assert_eq!(created.body["timeline_ids"], json!([edition]));
    // The retry replays even though the item ETag has moved on.
    let replay = f
        .call(
            "POST",
            path,
            Some(body),
            &[("authorization", &restricted), key, ("if-match", &etag)],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, created.body);
    let after = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a",
            None,
            &[("authorization", &restricted)],
        )
        .await;
    assert_ne!(after.headers["etag"], etag.as_str());
    let reused = f
        .call(
            "POST",
            path,
            Some(json!({"label":"Other","kind":"default"})),
            &[("authorization", &restricted), key, ("if-match", &etag)],
        )
        .await;
    problem(&reused, StatusCode::CONFLICT, "idempotency_key_reused");
    let stale_again = f
        .call(
            "POST",
            path,
            Some(json!({"label":"Other","kind":"default"})),
            &[
                ("authorization", &restricted),
                ("idempotency-key", "create-edition-02"),
                ("if-match", &etag),
            ],
        )
        .await;
    problem(
        &stale_again,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM editions WHERE item_id='film-a'")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 2);

    let current = after.headers["etag"].to_str().unwrap().to_owned();
    for (input, code) in [
        (
            json!({"label":"X","kind":"movie_cut"}),
            "unsupported_edition_kind",
        ),
        (
            json!({"label":"X","kind":"default","timeline_ids":[]}),
            "invalid_body",
        ),
        (json!({"label":"","kind":"default"}), "invalid_field"),
    ] {
        let reply = f
            .call(
                "POST",
                path,
                Some(input),
                &[
                    ("authorization", &restricted),
                    ("idempotency-key", "create-edition-03"),
                    ("if-match", &current),
                ],
            )
            .await;
        problem(&reply, StatusCode::UNPROCESSABLE_ENTITY, code);
    }
    for (item, status, code) in [
        ("film-b", StatusCode::NOT_FOUND, "not_found"),
        ("missing", StatusCode::NOT_FOUND, "not_found"),
        ("both", StatusCode::FORBIDDEN, "outside_library_scope"),
    ] {
        let reply = f
            .call(
                "POST",
                &format!("/api/v2/catalog/items/{item}/editions"),
                Some(json!({"label":"X","kind":"default"})),
                &[
                    ("authorization", &restricted),
                    ("idempotency-key", "create-edition-04"),
                    ("if-match", "\"r-1\""),
                ],
            )
            .await;
        problem(&reply, status, code);
    }
    // Containers cannot own editions.
    let series = f
        .call(
            "POST",
            "/api/v2/catalog/items",
            Some(json!({"kind":"series","title":"Show","library_ids":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "create-series-01"),
            ],
        )
        .await;
    let container = f
        .call(
            "POST",
            &format!(
                "/api/v2/catalog/items/{}/editions",
                series.body["id"].as_str().unwrap()
            ),
            Some(json!({"label":"X","kind":"default"})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "create-edition-05"),
                ("if-match", series.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    problem(&container, StatusCode::CONFLICT, "container_item");
    let reader = f.device(&["catalog:read"], &[&f.a]).await;
    let forbidden = f
        .call(
            "POST",
            path,
            Some(json!({"label":"X","kind":"default"})),
            &[
                ("authorization", &reader),
                ("idempotency-key", "create-edition-06"),
                ("if-match", &current),
            ],
        )
        .await;
    problem(&forbidden, StatusCode::FORBIDDEN, "permission_denied");
    let anonymous = f
        .call(
            "POST",
            path,
            Some(json!({"label":"X","kind":"default"})),
            &[
                ("idempotency-key", "create-edition-06"),
                ("if-match", &current),
            ],
        )
        .await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
}

async fn revision(f: &Fixture, id: &str) -> String {
    let reply = f
        .call(
            "GET",
            &format!("/api/v2/catalog/items/{id}"),
            None,
            &[("authorization", &bearer(OPERATOR))],
        )
        .await;
    reply.body["revision"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn merge_plans_are_reviewed_scoped_and_committed_once() {
    let f = Fixture::new().await;
    catalog(&f).await;
    f.item("film-a2", "Alpha Copy", &[(&f.a, "r9")]).await;
    let operator = bearer(OPERATOR);
    let ra = revision(&f, "film-a").await;
    let ra2 = revision(&f, "film-a2").await;
    let input = json!({"source_item_ids":["film-a2"],"target_item_id":"film-a","expected_revisions":{"film-a":ra,"film-a2":ra2}});
    let path = "/api/v2/catalog/items/film-a/merge-plans";
    let key = ("idempotency-key", "merge-preview-001");
    let plan = f
        .call(
            "POST",
            path,
            Some(input.clone()),
            &[("authorization", &operator), key],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    assert_eq!(
        plan.body["affected_ids"],
        json!(["film-a", "film-a2", "film-a2-ed"])
    );
    assert!(plan.body["expires_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(plan.body["warnings"].as_array().unwrap().len(), 1);
    let token = plan.body["plan_token"].as_str().unwrap().to_owned();
    let replay = f
        .call(
            "POST",
            path,
            Some(input.clone()),
            &[("authorization", &operator), key],
        )
        .await;
    assert_eq!(replay.body, plan.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let reused = f
        .call(
            "POST",
            path,
            Some(json!({"source_item_ids":["film-a2"],"target_item_id":"film-a","expected_revisions":{}})),
            &[("authorization", &operator), key],
        )
        .await;
    problem(&reused, StatusCode::CONFLICT, "idempotency_key_reused");

    for (input, status, code) in [
        (
            json!({"source_item_ids":["film-a2"],"target_item_id":"film-a","expected_revisions":{"film-a":"999","film-a2":ra2}}),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
        (
            json!({"source_item_ids":["film-a2"],"target_item_id":"film-a","expected_revisions":{"film-a":ra}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "missing_expected_revision",
        ),
        (
            json!({"source_item_ids":["film-a2"],"target_item_id":"film-a","expected_revisions":{"film-a":"01"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_field",
        ),
        (
            json!({"source_item_ids":[],"target_item_id":"film-a","expected_revisions":{}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "empty_selection",
        ),
        (
            json!({"source_item_ids":["missing"],"target_item_id":"film-a","expected_revisions":{}}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            json!({"source_item_ids":["film-b"],"target_item_id":"both","expected_revisions":{}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_field",
        ),
        (
            json!({"source_item_ids":["film-a2"],"target_item_id":"film-a","expected_revisions":{},"plan":1}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
    ] {
        let reply = f
            .call(
                "POST",
                path,
                Some(input),
                &[
                    ("authorization", &operator),
                    ("idempotency-key", "merge-preview-002"),
                ],
            )
            .await;
        problem(&reply, status, code);
    }

    // A restricted writer: B-only participants are missing; A+B are not writable.
    let writer = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    for (target, source, status, code) in [
        ("film-a", "film-b", StatusCode::NOT_FOUND, "not_found"),
        (
            "film-a",
            "both",
            StatusCode::FORBIDDEN,
            "outside_library_scope",
        ),
    ] {
        let reply = f
            .call(
                "POST",
                &format!("/api/v2/catalog/items/{target}/merge-plans"),
                Some(json!({"source_item_ids":[source],"target_item_id":target,"expected_revisions":{}})),
                &[("authorization", &writer), ("idempotency-key", "merge-preview-003")],
            )
            .await;
        problem(&reply, status, code);
    }
    let reader = f.device(&["catalog:read"], &[&f.a]).await;
    let forbidden = f
        .call(
            "POST",
            path,
            Some(input.clone()),
            &[
                ("authorization", &reader),
                ("idempotency-key", "merge-preview-004"),
            ],
        )
        .await;
    problem(&forbidden, StatusCode::FORBIDDEN, "permission_denied");
    let anonymous = f
        .call(
            "POST",
            path,
            Some(input),
            &[("idempotency-key", "merge-preview-004")],
        )
        .await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );

    // Tokens are bound to the reviewing principal and cannot be altered.
    let commit = "/api/v2/catalog/reconciliations";
    let other = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token})),
            &[
                ("authorization", &writer),
                ("idempotency-key", "merge-commit-0001"),
            ],
        )
        .await;
    problem(
        &other,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_plan_token",
    );
    let mut tampered = token.clone();
    tampered.insert(10, 'A');
    for bad in [tampered.as_str(), "mcp_short-token-x"] {
        let reply = f
            .call(
                "POST",
                commit,
                Some(json!({"plan_token":bad})),
                &[
                    ("authorization", &operator),
                    ("idempotency-key", "merge-commit-0002"),
                ],
            )
            .await;
        problem(
            &reply,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_plan_token",
        );
    }
    let key = ("idempotency-key", "merge-commit-0003");
    let committed = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token})),
            &[("authorization", &operator), key],
        )
        .await;
    assert_eq!(committed.status, StatusCode::OK, "{:?}", committed.body);
    assert_eq!(committed.body["id"], "film-a");
    assert_eq!(
        committed.headers["etag"],
        format!("\"r-{}\"", committed.body["revision"].as_str().unwrap())
    );
    let replay = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token})),
            &[("authorization", &operator), key],
        )
        .await;
    assert_eq!(replay.body, committed.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    // The same plan cannot be applied twice.
    let again = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "merge-commit-0004"),
            ],
        )
        .await;
    problem(&again, StatusCode::NOT_FOUND, "not_found");
    let alias = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a2",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(alias.body["id"], "film-a");
    let editions = f
        .call(
            "GET",
            "/api/v2/catalog/items/film-a/editions",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&editions), ["film-a-ed", "film-a2-ed"]);
    let unknown = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token,"force":true})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "merge-commit-0005"),
            ],
        )
        .await;
    problem(&unknown, StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");
    let anonymous = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token})),
            &[("idempotency-key", "merge-commit-0005")],
        )
        .await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let forbidden = f
        .call(
            "POST",
            commit,
            Some(json!({"plan_token":token})),
            &[
                ("authorization", &reader),
                ("idempotency-key", "merge-commit-0005"),
            ],
        )
        .await;
    problem(&forbidden, StatusCode::FORBIDDEN, "permission_denied");
}

#[tokio::test]
async fn split_plans_move_versions_and_go_stale_on_change() {
    let f = Fixture::new().await;
    // Two versions (distinct content revisions) of one edition in library A.
    f.item("film", "Film", &[(&f.a, "r1"), (&f.a, "r2")]).await;
    f.item("other", "Other", &[(&f.b, "r1")]).await;
    let operator = bearer(OPERATOR);
    let writer = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    let current = revision(&f, "film").await;
    let path = "/api/v2/catalog/items/film/split-plans";
    let input = json!({"version_ids":["film-f1"],"new_title":"Film (Director)","expected_revision":current});
    let plan = f
        .call(
            "POST",
            path,
            Some(input.clone()),
            &[
                ("authorization", &writer),
                ("idempotency-key", "split-preview-01"),
            ],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    let affected = plan.body["affected_ids"].as_array().unwrap();
    assert_eq!(affected[0], "film");
    assert!(affected.contains(&json!("film-f1")));
    let token = plan.body["plan_token"].as_str().unwrap().to_owned();
    for (input, status, code) in [
        (
            json!({"version_ids":["film-f0","film-f1"],"new_title":"X","expected_revision":current}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "split_would_empty_source",
        ),
        (
            json!({"version_ids":["nope"],"new_title":"X","expected_revision":current}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_version",
        ),
        (
            json!({"version_ids":["film-f1"],"new_title":"X","expected_revision":"999"}),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
        (
            json!({"version_ids":["film-f1"],"new_title":"","expected_revision":current}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_field",
        ),
        (
            json!({"version_ids":["film-f1"],"new_title":"X"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_body",
        ),
    ] {
        let reply = f
            .call(
                "POST",
                path,
                Some(input),
                &[
                    ("authorization", &writer),
                    ("idempotency-key", "split-preview-02"),
                ],
            )
            .await;
        problem(&reply, status, code);
    }
    let hidden = f
        .call(
            "POST",
            "/api/v2/catalog/items/other/split-plans",
            Some(json!({"version_ids":["other-f0"],"new_title":"X","expected_revision":"1"})),
            &[
                ("authorization", &writer),
                ("idempotency-key", "split-preview-03"),
            ],
        )
        .await;
    problem(&hidden, StatusCode::NOT_FOUND, "not_found");

    // A change after review makes the plan stale.
    let stale_token = token.clone();
    let tag = format!("\"r-{current}\"");
    let renamed = f
        .call(
            "PUT",
            "/api/v2/catalog/items/film",
            Some(json!({"kind":"video","title":"Film!","library_ids":[f.a]})),
            &[("authorization", &writer), ("if-match", &tag)],
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK, "{:?}", renamed.body);
    let stale = f
        .call(
            "POST",
            "/api/v2/catalog/reconciliations",
            Some(json!({"plan_token":stale_token})),
            &[
                ("authorization", &writer),
                ("idempotency-key", "split-commit-001"),
            ],
        )
        .await;
    problem(&stale, StatusCode::CONFLICT, "revision_conflict");

    let current = renamed.body["revision"].as_str().unwrap();
    let plan = f
        .call(
            "POST",
            path,
            Some(json!({"version_ids":["film-f1"],"new_title":"Film (Director)","expected_revision":current})),
            &[("authorization", &writer), ("idempotency-key", "split-preview-04")],
        )
        .await;
    let committed = f
        .call(
            "POST",
            "/api/v2/catalog/reconciliations",
            Some(json!({"plan_token":plan.body["plan_token"]})),
            &[
                ("authorization", &writer),
                ("idempotency-key", "split-commit-002"),
            ],
        )
        .await;
    assert_eq!(committed.status, StatusCode::OK, "{:?}", committed.body);
    assert_eq!(committed.body["title"], "Film (Director)");
    assert_eq!(committed.body["library_ids"], json!([f.a]));
    let new_id = committed.body["id"].as_str().unwrap();
    assert_ne!(new_id, "film");
    let files: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM media_files f JOIN editions e ON e.id=f.edition_id WHERE e.item_id=?",
    )
    .bind(new_id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(files, 1);
    let list = f
        .call(
            "GET",
            "/api/v2/catalog/items",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(ids(&list).len(), 3);
}

#[tokio::test]
async fn timeline_versions_filter_hidden_bindings_before_paging() {
    let f = Fixture::new().await;
    f.item("work", "Film", &[(&f.a, "r1"), (&f.b, "r2")]).await;
    let restricted = f.device(&["catalog:read"], &[&f.a]).await;
    let headers = [("authorization", restricted.as_str())];
    let versions = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/work-ed/versions?limit=1",
            None,
            &headers,
        )
        .await;
    assert_eq!(versions.status, StatusCode::OK, "{:?}", versions.body);
    assert_eq!(ids(&versions), ["work-f0"]);
    assert!(versions.body["next_cursor"].is_null());
    assert_eq!(
        versions.body["items"][0]["files"][0]["file"]["file_id"],
        "work-f0"
    );
    let timeline = f
        .call("GET", "/api/v2/catalog/timelines/work-ed", None, &headers)
        .await;
    assert_eq!(timeline.status, StatusCode::OK, "{:?}", timeline.body);
    assert_eq!(timeline.body["version_ids"], json!(["work-f0"]));
    sqlx::query("UPDATE media_files SET revision='r-new' WHERE id='work-f0'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let stale = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/work-ed/versions",
            None,
            &headers,
        )
        .await;
    assert_eq!(stale.body["items"][0]["availability"], "unavailable");
    // Making a multipart version cross the policy boundary hides the entire
    // version rather than presenting a truncated playable sequence.
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES ('work-f0',2,'work-f1','r2')").execute(&f.app.db).await.unwrap();
    let hidden = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/work-ed/versions?limit=1",
            None,
            &headers,
        )
        .await;
    assert!(ids(&hidden).is_empty());
    assert!(hidden.body["next_cursor"].is_null());
}

#[tokio::test]
async fn reconciliation_replays_recheck_current_catalog_scope() {
    let f = Fixture::new().await;
    f.item("one", "One", &[(&f.a, "r1")]).await;
    f.item("two", "Two", &[(&f.a, "r2")]).await;
    let writer = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    let input = json!({"source_item_ids":["two"],"target_item_id":"one","expected_revisions":{"one":revision(&f,"one").await,"two":revision(&f,"two").await}});
    let headers = [
        ("authorization", writer.as_str()),
        ("idempotency-key", "scope-preview-00001"),
    ];
    let path = "/api/v2/catalog/items/one/merge-plans";
    let plan = f.call("POST", path, Some(input.clone()), &headers).await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    sqlx::query("UPDATE media_files SET library_id=? WHERE edition_id='two-ed'")
        .bind(&f.b)
        .execute(&f.app.db)
        .await
        .unwrap();
    let denied = f.call("POST", path, Some(input), &headers).await;
    problem(&denied, StatusCode::NOT_FOUND, "not_found");
    sqlx::query("UPDATE media_files SET library_id=? WHERE edition_id='two-ed'")
        .bind(&f.a)
        .execute(&f.app.db)
        .await
        .unwrap();
    // Obtain a fresh plan after the source observations changed revisions.
    let fresh = json!({"source_item_ids":["two"],"target_item_id":"one","expected_revisions":{"one":revision(&f,"one").await,"two":revision(&f,"two").await}});
    let plan = f
        .call(
            "POST",
            path,
            Some(fresh),
            &[
                ("authorization", &writer),
                ("idempotency-key", "scope-preview-00002"),
            ],
        )
        .await;
    assert_eq!(plan.status, StatusCode::OK, "{:?}", plan.body);
    let body = json!({"plan_token":plan.body["plan_token"]});
    let headers = [
        ("authorization", writer.as_str()),
        ("idempotency-key", "scope-commit-000001"),
    ];
    let committed = f
        .call(
            "POST",
            "/api/v2/catalog/reconciliations",
            Some(body.clone()),
            &headers,
        )
        .await;
    assert_eq!(committed.status, StatusCode::OK, "{:?}", committed.body);
    sqlx::query("UPDATE media_files SET library_id=?")
        .bind(&f.b)
        .execute(&f.app.db)
        .await
        .unwrap();
    let denied = f
        .call(
            "POST",
            "/api/v2/catalog/reconciliations",
            Some(body),
            &headers,
        )
        .await;
    problem(&denied, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn scans_are_atomic_idempotent_scoped_demands() {
    let f = Fixture::new().await;
    let auth = bearer(OPERATOR);
    let path = format!("/api/v2/libraries/{}/scans", f.a);
    let input = json!({"mode":"verify","require_complete":true});
    let h = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "scan-admission-00001"),
    ];
    let first = f.call("POST", &path, Some(input.clone()), &h).await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    assert_eq!(first.body["status"], "queued");
    assert_eq!(first.body["sources"].as_array().unwrap().len(), 1);
    let replay = f.call("POST", &path, Some(input), &h).await;
    assert_eq!(replay.body, first.body);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    let conflict = f
        .call(
            "POST",
            &path,
            Some(json!({"mode":"incremental","require_complete":true})),
            &h,
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    let second = f
        .call(
            "POST",
            &path,
            Some(json!({"mode":"verify","require_complete":true})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-admission-00002"),
            ],
        )
        .await;
    assert_eq!(second.status, StatusCode::ACCEPTED);
    let cancel = format!(
        "/api/v2/scans/{}/cancel",
        first.body["id"].as_str().unwrap()
    );
    let h = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "scan-cancel-00000001"),
    ];
    let cancelled = f.call("POST", &cancel, None, &h).await;
    assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
    assert_eq!(cancelled.body["status"], "cancelled");
    assert_eq!(f.call("POST", &cancel, None, &h).await.body, cancelled.body);
    let phase: String = sqlx::query_scalar("SELECT phase FROM jobs LIMIT 1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(phase, "queued", "other demand still owns physical work");
    let limited = f.device(&["catalog:read"], &[&f.a]).await;
    assert_eq!(
        f.call("GET", "/api/v2/scans", None, &[("authorization", &limited)])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let malformed = f
        .call(
            "POST",
            &path,
            Some(json!({"mode":"verify","require_complete":false,"source_ids":[f.a,f.a]})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-invalid-0000001"),
            ],
        )
        .await;
    assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
    let listed = f
        .call(
            "GET",
            "/api/v2/scans?limit=1",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
    assert!(listed.body["next_cursor"].is_string());
}

#[tokio::test]
async fn scan_followup_reports_eligible_attempt_and_decimal_counters() {
    let f = Fixture::new().await;
    let auth = bearer(OPERATOR);
    let path = format!("/api/v2/libraries/{}/scans", f.a);
    let first = f
        .call(
            "POST",
            &path,
            Some(json!({"mode":"incremental","require_complete":false})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-followup-00001"),
            ],
        )
        .await;
    let old = first.body["job_ids"][0].as_str().unwrap();
    sqlx::query("UPDATE jobs SET phase='running',attempt=1,started_barrier=1 WHERE id=?")
        .bind(old)
        .execute(&f.app.db)
        .await
        .unwrap();
    let next = f
        .call(
            "POST",
            &path,
            Some(json!({"mode":"verify","require_complete":false})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-followup-00002"),
            ],
        )
        .await;
    assert_eq!(next.status, StatusCode::ACCEPTED, "{}", next.body);
    assert_eq!(next.body["status"], "queued");
    assert_ne!(next.body["job_ids"][0], old);
    for field in [
        "complete_directories",
        "incomplete_directories",
        "observed_files",
        "verified_files",
    ] {
        assert_eq!(next.body["sources"][0][field], "0");
    }
}

#[tokio::test]
async fn public_structure_writes_preserve_revisions_and_retry_identity() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let f = Fixture::new().await;
    let auth = bearer(OPERATOR);
    f.item("film", "Film", &[(&f.a, "r1")]).await;
    f.item("extra", "Extra", &[(&f.a, "r2")]).await;
    let read = f
        .call(
            "GET",
            "/api/v2/catalog/items/film",
            None,
            &[("authorization", &auth)],
        )
        .await;
    let etag = read.headers["etag"].to_str().unwrap();
    let input = json!({"edition_id":"film-ed","duration_ms":100000,"order_group_id":null,"order_position":null});
    let h = [
        ("authorization", auth.as_str()),
        ("if-match", etag),
        ("idempotency-key", "timeline-create-00001"),
    ];
    let timeline = f
        .call(
            "POST",
            "/api/v2/catalog/items/film/timelines",
            Some(input.clone()),
            &h,
        )
        .await;
    assert_eq!(timeline.status, StatusCode::CREATED, "{}", timeline.body);
    assert_eq!(
        f.call(
            "POST",
            "/api/v2/catalog/items/film/timelines",
            Some(input),
            &h
        )
        .await
        .body,
        timeline.body
    );
    let stale=f.call("POST","/api/v2/catalog/items/film/timelines",Some(json!({"edition_id":"film-ed","duration_ms":null,"order_group_id":null,"order_position":null})),&[("authorization",&auth),("if-match",etag),("idempotency-key","timeline-create-00002")]).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let files = f
        .call(
            "GET",
            "/api/v2/catalog/items/film/files",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(files.status, StatusCode::OK, "{}", files.body);
    let file = &files.body["items"][0];
    assert!(file["size_bytes"].is_string());
    assert!(file.get("relative_path").is_none());
    let timeline = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/film-ed",
            None,
            &[("authorization", &auth)],
        )
        .await;
    let id = timeline.body["id"].as_str().unwrap();
    let version = json!({"timeline_id":id,"label":"Original","files":[{"file":{"file_id":file["id"],"file_revision":file["revision"]},"part":1,"start_ms":null,"end_ms":null}],"timeline_equivalence":"declared"});
    let path = format!("/api/v2/catalog/timelines/{id}/versions");
    let h = [
        ("authorization", auth.as_str()),
        ("if-match", timeline.headers["etag"].to_str().unwrap()),
        ("idempotency-key", "version-create-000001"),
    ];
    let created = f.call("POST", &path, Some(version.clone()), &h).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(
        f.call("POST", &path, Some(version), &h).await.body,
        created.body
    );
    let stale_version=f.call("POST",&path,Some(json!({"timeline_id":id,"label":"Second","files":[{"file":{"file_id":file["id"],"file_revision":file["revision"]},"part":1,"start_ms":null,"end_ms":null}],"timeline_equivalence":"declared"})),&[("authorization",&auth),("if-match",timeline.headers["etag"].to_str().unwrap()),("idempotency-key","version-stale-0000001")]).await;
    assert_eq!(stale_version.status, StatusCode::PRECONDITION_FAILED);
    let admin = f
        .call(
            "GET",
            "/api/v2/admin/files",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(admin.status, StatusCode::OK, "{}", admin.body);
    assert!(admin.body["items"][0]["file"]["id"].is_string());
    assert!(admin.body["items"][0]["relative_path"].is_string());
    assert!(admin.body["items"][0]["binding_revision"].is_string());
    let read = f
        .call(
            "GET",
            "/api/v2/catalog/items/extra",
            None,
            &[("authorization", &auth)],
        )
        .await;
    let relation=f.call("POST","/api/v2/catalog/items/extra/relationships",Some(json!({"source_item_id":"extra","target_item_id":"film","kind":"extra_of","position":null})),&[("authorization",&auth),("if-match",read.headers["etag"].to_str().unwrap()),("idempotency-key","relation-create-0001")]).await;
    assert_eq!(relation.status, StatusCode::CREATED, "{}", relation.body);
    let path = format!(
        "/api/v2/catalog/relationships/{}",
        relation.body["id"].as_str().unwrap()
    );
    assert_eq!(
        f.call("GET", &path, None, &[("authorization", &auth)])
            .await
            .body,
        relation.body
    );
    assert_eq!(
        f.call(
            "DELETE",
            &path,
            None,
            &[("authorization", &auth), ("if-match", "\"r-0\"")]
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
            &[("authorization", &auth), ("if-match", "\"r-1\"")]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn source_relocation_preview_commit_and_retry_are_fenced() {
    let f = Fixture::new().await;
    let auth = bearer(OPERATOR);
    let source = f
        .call(
            "GET",
            &format!("/api/v2/sources/{}", f.a),
            None,
            &[("authorization", &auth)],
        )
        .await;
    let new_root = f._dir.path().join("relocated");
    std::fs::create_dir(&new_root).unwrap();
    let path = format!("/api/v2/sources/{}/relocation-plans", f.a);
    let input = json!({"new_root_path":new_root});
    let h = [
        ("authorization", auth.as_str()),
        ("if-match", source.headers["etag"].to_str().unwrap()),
        ("idempotency-key", "relocation-preview-01"),
    ];
    let preview = f.call("POST", &path, Some(input.clone()), &h).await;
    assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
    assert_eq!(
        f.call("POST", &path, Some(input), &h).await.body,
        preview.body
    );
    let path = format!("/api/v2/sources/{}/relocation", f.a);
    let input = json!({"plan_token":preview.body["plan_token"]});
    let h = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "relocation-commit-001"),
    ];
    let committed = f.call("POST", &path, Some(input.clone()), &h).await;
    assert_eq!(committed.status, StatusCode::OK, "{}", committed.body);
    assert_eq!(
        f.call("POST", &path, Some(input.clone()), &h).await.body,
        committed.body
    );
    let stale = f
        .call(
            "POST",
            &path,
            Some(input),
            &[
                ("authorization", &auth),
                ("idempotency-key", "relocation-commit-002"),
            ],
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
}

#[tokio::test]
async fn restricted_source_manager_controls_demands_without_owning_shared_jobs() {
    let f = Fixture::new().await;
    let token = f.device(&["catalog:read", "sources:manage"], &[&f.a]).await;
    let created = f
        .call(
            "POST",
            &format!("/api/v2/libraries/{}/scans", f.a),
            Some(json!({"mode":"incremental","require_complete":false})),
            &[
                ("authorization", &token),
                ("idempotency-key", "restricted-scan-001"),
            ],
        )
        .await;
    assert_eq!(created.status, StatusCode::ACCEPTED, "{}", created.body);
    assert_eq!(created.body["job_ids"], json!([]));
    let cancelled = f
        .call(
            "POST",
            &format!(
                "/api/v2/scans/{}/cancel",
                created.body["id"].as_str().unwrap()
            ),
            None,
            &[
                ("authorization", &token),
                ("idempotency-key", "restricted-cancel-01"),
            ],
        )
        .await;
    assert_eq!(cancelled.status, StatusCode::OK);
    assert_eq!(cancelled.body["status"], "cancelled");
    assert_eq!(
        f.call(
            "POST",
            &format!("/api/v2/libraries/{}/scans", f.b),
            Some(json!({"mode":"verify","require_complete":false})),
            &[
                ("authorization", &token),
                ("idempotency-key", "restricted-hidden-01")
            ]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn order_group_writer_rejects_foreign_editions_and_outside_scope() {
    let f = Fixture::new().await;
    f.item("visible", "Visible", &[(&f.a, "r-visible")]).await;
    f.item("hidden", "Hidden", &[(&f.b, "r-hidden")]).await;
    f.item(
        "mixed",
        "Mixed",
        &[(&f.a, "r-mixed-a"), (&f.b, "r-mixed-b")],
    )
    .await;
    let op = bearer(OPERATOR);
    let device = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    let reader = f.device(&["catalog:read"], &[&f.a]).await;
    for (item, edition, auth, expected) in [
        ("visible", "hidden-ed", op.as_str(), StatusCode::NOT_FOUND),
        (
            "hidden",
            "hidden-ed",
            device.as_str(),
            StatusCode::NOT_FOUND,
        ),
        ("mixed", "mixed-ed", device.as_str(), StatusCode::FORBIDDEN),
        (
            "visible",
            "visible-ed",
            reader.as_str(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let current = f
            .call(
                "GET",
                &format!("/api/v2/catalog/items/{item}"),
                None,
                &[("authorization", &op)],
            )
            .await;
        let result = f
            .call(
                "POST",
                &format!("/api/v2/catalog/items/{item}/order-groups"),
                Some(json!({"edition_id":edition,"name":"Release","kind":"custom"})),
                &[
                    ("authorization", auth),
                    ("if-match", current.headers["etag"].to_str().unwrap()),
                    ("idempotency-key", "scope-order-group-01"),
                ],
            )
            .await;
        assert_eq!(result.status, expected, "{}", result.body);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM order_groups")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn public_timeline_order_writer_drives_next_resolver() {
    let f = Fixture::new().await;
    let op = bearer(OPERATOR);
    f.item("ordered", "Ordered", &[(&f.a, "r1")]).await;
    f.item("retired-order", "Retired", &[]).await;
    sqlx::query("INSERT INTO catalog_receipts VALUES ('order-alias','catalog:merge',0,'{}')")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_aliases VALUES ('retired-order','ordered','order-alias')")
        .execute(&f.app.db)
        .await
        .unwrap();

    let item = f
        .call(
            "GET",
            "/api/v2/catalog/items/ordered",
            None,
            &[("authorization", &op)],
        )
        .await;
    let input = json!({"edition_id":"ordered-ed","name":"Release","kind":"custom"});
    let headers = [
        ("authorization", op.as_str()),
        ("if-match", item.headers["etag"].to_str().unwrap()),
        ("idempotency-key", "ordered-group-0001"),
    ];
    let created = f
        .call(
            "POST",
            "/api/v2/catalog/items/retired-order/order-groups",
            Some(input.clone()),
            &headers,
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let replay = f
        .call(
            "POST",
            "/api/v2/catalog/items/ordered/order-groups",
            Some(input.clone()),
            &headers,
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.body);
    assert_eq!(replay.body, created.body);
    assert_eq!(created.body["item_id"], "ordered");
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let stale = f
        .call(
            "POST",
            "/api/v2/catalog/items/ordered/order-groups",
            Some(input),
            &[
                ("authorization", &op),
                ("if-match", item.headers["etag"].to_str().unwrap()),
                ("idempotency-key", "ordered-group-0002"),
            ],
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.body
    );
    let groups = f
        .call(
            "GET",
            "/api/v2/catalog/items/ordered/order-groups",
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(groups.status, StatusCode::OK, "{}", groups.body);
    assert_eq!(groups.body["items"], json!([created.body]));
    let group = created.body["id"].as_str().unwrap().to_owned();
    let mut ids = Vec::new();
    for position in [1, 2] {
        let item = f
            .call(
                "GET",
                "/api/v2/catalog/items/ordered",
                None,
                &[("authorization", &op)],
            )
            .await;
        let key = format!("ordered-timeline-{position:04}");
        let created = f.call("POST", "/api/v2/catalog/items/ordered/timelines",
            Some(json!({"edition_id":"ordered-ed","duration_ms":1000,"order_group_id":group,"order_position":position})),
            &[("authorization", &op),("if-match",item.headers["etag"].to_str().unwrap()),("idempotency-key",&key)]).await;
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        assert_eq!(created.body["order_position"], position);
        ids.push(created.body["id"].as_str().unwrap().to_owned());
    }
    let next = f
        .call(
            "GET",
            &format!("/api/v2/profiles/default/timelines/{}/next", ids[0]),
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.body);
    assert_eq!(next.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(next.body["items"][0]["id"], ids[1]);
    let end = f
        .call(
            "GET",
            &format!("/api/v2/profiles/default/timelines/{}/next", ids[1]),
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(end.status, StatusCode::OK, "{}", end.body);
    assert_eq!(end.body["items"], json!([]));
}

#[tokio::test]
async fn existing_playable_timeline_order_is_fenced_scoped_and_preserves_identity() {
    let f = Fixture::new().await;
    f.item("first", "First", &[(&f.a, "r1")]).await;
    f.item("second", "Second", &[(&f.a, "r2")]).await;
    f.item("hidden", "Hidden", &[(&f.b, "r3")]).await;
    // As after a reviewed work merge, two editions retain their playable
    // timeline identities under one work. Generic relationships are not the
    // canonical series/season ancestry used by ordering.
    sqlx::query("UPDATE editions SET item_id='first' WHERE id='second-ed'")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO order_groups(id,edition_id,name,kind) VALUES ('visible-order','first-ed','Release','custom'),('hidden-order','hidden-ed','Private','custom')")
        .execute(&f.app.db).await.unwrap();
    sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,position_ms,revision,updated_at) VALUES ('default','first-ed',1234,7,1)")
        .execute(&f.app.db).await.unwrap();
    let operator = bearer(OPERATOR);
    let scoped = f.device(&["catalog:read", "catalog:write"], &[&f.a]).await;
    let input = |group: &str, position, revision: &str| json!({"order_group_id":group,"order_position":position,"expected_group_revision":revision});
    let first = "/api/v2/catalog/timelines/first-ed/order";
    let second = "/api/v2/catalog/timelines/second-ed/order";
    let placed = f
        .call(
            "PUT",
            first,
            Some(input("visible-order", 1, "1")),
            &[("authorization", &scoped), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(placed.status, StatusCode::OK, "{}", placed.body);
    assert_eq!(placed.body["revision"], "3");
    assert_eq!(placed.body["version_ids"], json!(["first-f0"]));
    let placed = f
        .call(
            "PUT",
            second,
            Some(input("visible-order", 2, "2")),
            &[("authorization", &scoped), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(placed.status, StatusCode::OK, "{}", placed.body);
    let next = f
        .call(
            "GET",
            "/api/v2/profiles/default/timelines/first-ed/next",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.body);
    assert_eq!(next.body["items"][0]["id"], "second-ed");
    assert_eq!(next.body["items"][0]["version_ids"], json!(["second-f0"]));
    for (tag, group_revision, position, expected) in [
        ("\"r-2\"", "3", 3, StatusCode::PRECONDITION_FAILED),
        ("\"r-3\"", "2", 3, StatusCode::CONFLICT),
        ("\"r-3\"", "3", 2, StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let rejected = f
            .call(
                "PUT",
                first,
                Some(input("visible-order", position, group_revision)),
                &[("authorization", &scoped), ("if-match", tag)],
            )
            .await;
        assert_eq!(rejected.status, expected, "{}", rejected.body);
    }
    let revisions: (i64,i64) = sqlx::query_as("SELECT t.revision,g.revision FROM timelines t,order_groups g WHERE t.id='first-ed' AND g.id='visible-order'").fetch_one(&f.app.db).await.unwrap();
    assert_eq!(revisions, (3, 3));
    let denied = f
        .call(
            "PUT",
            first,
            Some(input("hidden-order", 1, "1")),
            &[("authorization", &scoped), ("if-match", "\"r-3\"")],
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    let unrelated = f
        .call(
            "PUT",
            first,
            Some(input("hidden-order", 1, "1")),
            &[("authorization", &operator), ("if-match", "\"r-3\"")],
        )
        .await;
    assert_eq!(unrelated.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Seed a retained previous membership that is no longer in caller scope.
    // This is a conservative authorization fixture, not a successful hierarchy
    // mutation: even a legacy membership must not authorize hidden-group writes.
    sqlx::query("UPDATE timelines SET order_group_id='hidden-order',order_position=1,revision=revision+1 WHERE id='first-ed'")
        .execute(&f.app.db).await.unwrap();
    // Destination and timeline are visible, but leaving the previous hidden
    // group would mutate authority outside this device's scope.
    let denied = f
        .call(
            "PUT",
            first,
            Some(input("visible-order", 1, "3")),
            &[("authorization", &scoped), ("if-match", "\"r-4\"")],
        )
        .await;
    assert_eq!(denied.status, StatusCode::NOT_FOUND);
    let groups: Vec<(String, i64)> =
        sqlx::query_as("SELECT id,revision FROM order_groups ORDER BY id")
            .fetch_all(&f.app.db)
            .await
            .unwrap();
    assert_eq!(
        groups,
        vec![("hidden-order".into(), 1), ("visible-order".into(), 3)]
    );
    let history: (i64, i64) = sqlx::query_as(
        "SELECT position_ms,revision FROM timeline_viewing WHERE timeline_id='first-ed'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(history, (1234, 7));
    let binding: (String, String) = sqlx::query_as(
        "SELECT file_id,file_revision FROM version_files WHERE version_id='first-f0'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(binding, ("first-f0".into(), "r1".into()));
}

#[tokio::test]
async fn file_inventory_filters_scope_before_pagination() {
    let f = Fixture::new().await;
    f.item("a-hidden", "Hidden", &[(&f.b, "hidden")]).await;
    f.item("b-visible", "Visible", &[(&f.a, "visible")]).await;
    let reader = f.device(&["catalog:read", "sources:manage"], &[&f.a]).await;
    let files = f
        .call(
            "GET",
            "/api/v2/admin/files?limit=1",
            None,
            &[("authorization", &reader)],
        )
        .await;
    assert_eq!(files.status, StatusCode::OK, "{}", files.body);
    assert_eq!(files.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(files.body["items"][0]["file"]["id"], "b-visible-f0");
    let files = f
        .call(
            "GET",
            "/api/v2/catalog/items/b-visible/files",
            None,
            &[("authorization", &reader)],
        )
        .await;
    assert_eq!(files.status, StatusCode::OK, "{}", files.body);
    assert_eq!(files.body["items"][0]["id"], "b-visible-f0");
    assert!(files.body["items"][0].get("relative_path").is_none());
    assert_eq!(
        f.call(
            "GET",
            "/api/v2/catalog/items/a-hidden/files",
            None,
            &[("authorization", &reader)]
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

/// Settled scan history beyond the old 1,000-row page bound, plus one still
/// pending request, inserted directly; `minute` orders them.
async fn scan_history(f: &Fixture, library: &str, prefix: &str, settled: usize) {
    for n in 0..settled {
        let id = format!("{prefix}-{n:04}");
        sqlx::query(
            "INSERT INTO scan_requests(id,library_id,require_complete,created_at) VALUES (?,?,0,?)",
        )
        .bind(&id)
        .bind(library)
        .bind(1_000 + n as i64)
        .execute(&f.app.db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO scan_demands(id,request_id,source_id,barrier,verify,status) VALUES (?,?,?,0,0,'complete')")
            .bind(format!("{id}-d"))
            .bind(&id)
            .bind(library)
            .execute(&f.app.db)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn scan_history_pages_newest_first_stably_and_stays_bounded() {
    let f = Fixture::new().await;
    let auth = bearer(OPERATOR);
    // The oldest request is still running: it is never pruned.
    sqlx::query("INSERT INTO scan_requests(id,library_id,require_complete,created_at) VALUES ('a-running',?,0,1)")
        .bind(&f.a)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO scan_demands(id,request_id,source_id,barrier,verify,status) VALUES ('a-running-d','a-running',?,0,0,'pending')")
        .bind(&f.a)
        .execute(&f.app.db)
        .await
        .unwrap();
    scan_history(&f, &f.a, "a", 1_100).await;
    scan_history(&f, &f.b, "b", 3).await;
    let get = |path: String, auth: String| {
        let f = &f;
        async move {
            f.call("GET", &path, None, &[("authorization", &auth)])
                .await
        }
    };

    // Newest first across pages; a scan admitted mid-read appears only on a
    // fresh first page and never shifts or repeats a later one.
    let first = get(
        format!("/api/v2/scans?order=recent&limit=200&library_id={}", f.a),
        auth.clone(),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(ids(&first)[..2], ["a-1099".to_owned(), "a-1098".to_owned()]);
    // Admission order, not the clock: this one claims an older capture time.
    sqlx::query("INSERT INTO scan_requests(id,library_id,require_complete,created_at) VALUES ('a-late',?,0,0)")
        .bind(&f.a)
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut seen = ids(&first);
    let mut cursor = first.body["next_cursor"].as_str().map(str::to_owned);
    while let Some(c) = cursor {
        let page = get(
            format!(
                "/api/v2/scans?order=recent&limit=200&library_id={}&cursor={c}",
                f.a
            ),
            auth.clone(),
        )
        .await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        seen.extend(ids(&page));
        cursor = page.body["next_cursor"].as_str().map(str::to_owned);
    }
    let mut expected: Vec<String> = (0..1_100).rev().map(|n| format!("a-{n:04}")).collect();
    expected.push("a-running".into());
    assert_eq!(seen, expected, "every existing scan once, newest first");
    let fresh = get(
        format!("/api/v2/scans?order=recent&limit=1&library_id={}", f.a),
        auth.clone(),
    )
    .await;
    assert_eq!(ids(&fresh), ["a-late"]);
    sqlx::query("DELETE FROM scan_requests WHERE id='a-late'")
        .execute(&f.app.db)
        .await
        .unwrap();

    // The library filter and the caller's scope both narrow the rows; a
    // filter naming a hidden library is an empty page, not an error.
    let limited = f.device(&["catalog:read", "sources:manage"], &[&f.b]).await;
    let mine = get("/api/v2/scans?order=recent".into(), limited.clone()).await;
    assert_eq!(ids(&mine), ["b-0002", "b-0001", "b-0000"]);
    // A real admission between two page reads, within the same second as
    // the history it follows, appears only on a fresh first page.
    sqlx::query("UPDATE scan_requests SET created_at=? WHERE id LIKE 'b-%'")
        .bind(playscale::now())
        .execute(&f.app.db)
        .await
        .unwrap();
    let first = get("/api/v2/scans?order=recent&limit=2".into(), limited.clone()).await;
    assert_eq!(ids(&first), ["b-0002", "b-0001"]);
    let admitted = f
        .call(
            "POST",
            &format!("/api/v2/libraries/{}/scans", f.b),
            Some(json!({"mode":"incremental","require_complete":false})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-mid-read-00001"),
            ],
        )
        .await;
    assert_eq!(admitted.status, StatusCode::ACCEPTED, "{}", admitted.body);
    let next = get(
        format!(
            "/api/v2/scans?order=recent&limit=2&cursor={}",
            first.body["next_cursor"].as_str().unwrap()
        ),
        limited.clone(),
    )
    .await;
    assert_eq!(ids(&next), ["b-0000"]);
    let fresh = get("/api/v2/scans?order=recent&limit=1".into(), limited.clone()).await;
    assert_eq!(ids(&fresh), [admitted.body["id"].as_str().unwrap()]);
    let hidden = get(
        format!("/api/v2/scans?order=recent&library_id={}", f.a),
        limited,
    )
    .await;
    assert_eq!(hidden.status, StatusCode::OK);
    assert!(ids(&hidden).is_empty());
    problem(
        &get(
            "/api/v2/scans?order=recent&cursor=not-a-cursor".into(),
            auth.clone(),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_cursor",
    );

    // The newest history is deleted between page reads (as deleting its
    // library does), then a scan is admitted: SQLite would reuse the deleted
    // rowids, the admission sequence never is, so the new scan stays off the
    // later page.
    for (id, library) in [("seq-old", &f.b), ("seq-mid", &f.b), ("seq-new", &f.b)] {
        sqlx::query(
            "INSERT INTO scan_requests(id,library_id,require_complete,created_at) VALUES (?,?,0,1)",
        )
        .bind(id)
        .bind(library)
        .execute(&f.app.db)
        .await
        .unwrap();
    }
    let top = get("/api/v2/scans?order=recent&limit=1".into(), auth.clone()).await;
    assert_eq!(ids(&top), ["seq-new"]);
    sqlx::query("DELETE FROM scan_requests WHERE id IN ('seq-mid','seq-new')")
        .execute(&f.app.db)
        .await
        .unwrap();
    let readmitted = f
        .call(
            "POST",
            &format!("/api/v2/libraries/{}/scans", f.b),
            Some(json!({"mode":"incremental","require_complete":false})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-after-delete-01"),
            ],
        )
        .await;
    assert_eq!(
        readmitted.status,
        StatusCode::ACCEPTED,
        "{}",
        readmitted.body
    );
    let after = get(
        format!(
            "/api/v2/scans?order=recent&limit=1&cursor={}",
            top.body["next_cursor"].as_str().unwrap()
        ),
        auth.clone(),
    )
    .await;
    assert_eq!(ids(&after), ["seq-old"]);

    // Admitting a scan bounds the library's settled history; the running
    // request and the other library's history are kept.
    let admitted = f
        .call(
            "POST",
            &format!("/api/v2/libraries/{}/scans", f.a),
            Some(json!({"mode":"incremental","require_complete":false})),
            &[
                ("authorization", &auth),
                ("idempotency-key", "scan-retention-00001"),
            ],
        )
        .await;
    assert_eq!(admitted.status, StatusCode::ACCEPTED, "{}", admitted.body);
    let count = |sql: &'static str| {
        let f = &f;
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .bind(&f.a)
                .fetch_one(&f.app.db)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        count("SELECT count(*) FROM scan_requests WHERE library_id=? AND (id LIKE 'a-0%' OR id LIKE 'a-1%')").await,
        playscale_core::demands::RETAINED_SCANS_PER_LIBRARY as i64
    );
    assert_eq!(
        count("SELECT count(*) FROM scan_requests WHERE library_id=? AND id='a-running'").await,
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM scan_demands WHERE request_id='a-0000' AND ?<>''").await,
        0,
        "demands cascade"
    );
    assert_eq!(
        count("SELECT count(*) FROM scan_requests WHERE id LIKE 'b-%' AND ?<>''").await,
        3
    );
    // The newest settled requests are the ones kept.
    assert_eq!(
        count("SELECT count(*) FROM scan_requests WHERE id='a-1099' AND ?<>''").await,
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM scan_requests WHERE id='a-0899' AND ?<>''").await,
        0
    );
}

#[tokio::test]
async fn batched_item_reads_apply_the_catalog_scope_and_skip_aliases() {
    let f = Fixture::new().await;
    catalog(&f).await;
    sqlx::query("INSERT INTO catalog_receipts VALUES ('rc','catalog:merge',0,'{}')")
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO item_aliases VALUES ('manual','film-a','rc')")
        .execute(&f.app.db)
        .await
        .unwrap();
    let get = |path: &'static str, auth: String| {
        let f = &f;
        async move { f.call("GET", path, None, &[("authorization", &auth)]).await }
    };
    let operator = bearer(OPERATOR);
    let all = get(
        "/api/v2/catalog/items?ids=both,film-b,film-a,manual,missing",
        operator.clone(),
    )
    .await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    // Title order, live items only: the alias and the unknown ID match nothing.
    assert_eq!(ids(&all), ["film-a", "film-b", "both"]);
    // A restricted reader sees only what it could browse; a hidden title is
    // indistinguishable from an unknown one.
    let limited = f.device(&["catalog:read"], &[&f.a]).await;
    let mine = get(
        "/api/v2/catalog/items?ids=both,film-b,film-a",
        limited.clone(),
    )
    .await;
    assert_eq!(ids(&mine), ["film-a", "both"]);
    let hidden = get("/api/v2/catalog/items?ids=film-b", limited).await;
    assert_eq!(hidden.status, StatusCode::OK);
    assert!(ids(&hidden).is_empty());
    for bad in [
        "/api/v2/catalog/items?ids=film-a,film-a",
        "/api/v2/catalog/items?ids=",
        "/api/v2/catalog/items?ids=no%20spaces",
    ] {
        problem(
            &get(bad, operator.clone()).await,
            StatusCode::BAD_REQUEST,
            "invalid_query",
        );
    }
    let many: Vec<String> = (0..101).map(|n| format!("i{n}")).collect();
    let path = format!("/api/v2/catalog/items?ids={}", many.join(","));
    let reply = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    problem(&reply, StatusCode::BAD_REQUEST, "invalid_query");
}

#[tokio::test]
async fn markers_are_revision_bound_and_manual_corrections_take_effect() {
    let f = Fixture::new().await;
    catalog(&f).await;
    let op = bearer(OPERATOR);
    let path = "/api/v2/catalog/timelines/film-a-ed/markers";
    let timeline = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/film-a-ed",
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(timeline.status, StatusCode::OK, "{}", timeline.body);
    let revision = timeline.body["revision"].as_str().unwrap().to_owned();
    // A detected intro and embedded chapters, recorded against this revision.
    for (id, kind, start, end, provenance) in [
        ("det-intro", "intro", 1_000, Some(5_000), "detected"),
        ("chap-1", "chapter", 0, None, "embedded"),
        ("chap-2", "chapter", 60_000, None, "embedded"),
    ] {
        sqlx::query("INSERT INTO markers(id,timeline_id,timeline_revision,kind,start_ms,end_ms,label,provenance) VALUES (?,'film-a-ed',?,?,?,?,NULL,?)")
            .bind(id)
            .bind(revision.parse::<i64>().unwrap())
            .bind(kind)
            .bind(start)
            .bind(end)
            .bind(provenance)
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    let all = f.call("GET", path, None, &[("authorization", &op)]).await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    assert_eq!(ids(&all), ["chap-1", "det-intro", "chap-2"]);
    assert_eq!(all.body["items"][1]["provenance"], "detected");

    // A manual correction outranks the detected intro in the effective view.
    let input = json!({"timeline_revision":revision,"kind":"intro","start_ms":2000,"end_ms":6000,"label":"Opening"});
    let headers = [
        ("authorization", op.as_str()),
        ("idempotency-key", "marker-create-0000001"),
    ];
    let created = f.call("POST", path, Some(input.clone()), &headers).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["provenance"], "manual");
    let replay = f.call("POST", path, Some(input.clone()), &headers).await;
    assert_eq!(replay.body, created.body);
    let effective = f
        .call(
            "GET",
            &format!("{path}?effective=true"),
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(
        ids(&effective),
        ["chap-1", created.body["id"].as_str().unwrap(), "chap-2"]
    );

    // Refusals: a stale revision, an open-ended intro, no write permission,
    // a title outside the caller's scope.
    let mut stale = input.clone();
    stale["timeline_revision"] = json!("999");
    problem(
        &f.call(
            "POST",
            path,
            Some(stale),
            &[
                ("authorization", &op),
                ("idempotency-key", "marker-create-0000002"),
            ],
        )
        .await,
        StatusCode::CONFLICT,
        "timeline_changed",
    );
    let mut open = input.clone();
    open["end_ms"] = Value::Null;
    problem(
        &f.call(
            "POST",
            path,
            Some(open),
            &[
                ("authorization", &op),
                ("idempotency-key", "marker-create-0000003"),
            ],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_interval",
    );
    let reader = f.device(&["catalog:read"], &[&f.a]).await;
    assert_eq!(
        f.call(
            "POST",
            path,
            Some(input.clone()),
            &[
                ("authorization", &reader),
                ("idempotency-key", "marker-create-0000004")
            ]
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.call("GET", path, None, &[("authorization", &reader)])
            .await
            .status,
        StatusCode::OK
    );
    let outsider = f.device(&["catalog:read", "catalog:write"], &[&f.b]).await;
    assert_eq!(
        f.call("GET", path, None, &[("authorization", &outsider)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Positions beyond playback's bound are refused even without a known
    // duration (no unsafe JSON integers, no SQLite overflow).
    let mut huge = input.clone();
    huge["start_ms"] = json!(9_007_199_254_740_991_u64);
    huge["end_ms"] = Value::Null;
    huge["kind"] = json!("chapter");
    problem(
        &f.call(
            "POST",
            path,
            Some(huge),
            &[
                ("authorization", &op),
                ("idempotency-key", "marker-create-0000005"),
            ],
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "outside_timeline",
    );
    // A timeline holds at most MAX_MARKERS; every one stays readable.
    let max = playscale_core::markers::MAX_MARKERS;
    sqlx::query("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?) INSERT INTO markers(id,timeline_id,timeline_revision,kind,start_ms,end_ms,label,provenance) SELECT printf('bulk-%04d',i),'film-a-ed',?,'chapter',i*1000,NULL,NULL,'embedded' FROM n")
        .bind((max - 4) as i64)
        .bind(revision.parse::<i64>().unwrap())
        .execute(&f.app.db)
        .await
        .unwrap();
    problem(
        &f.call(
            "POST",
            path,
            Some(input.clone()),
            &[
                ("authorization", &op),
                ("idempotency-key", "marker-create-0000006"),
            ],
        )
        .await,
        StatusCode::CONFLICT,
        "marker_limit",
    );
    // The adapter's other creation path enforces the same bound.
    assert!(matches!(
        playscale::markers::create(
            &f.app,
            "film-a-ed",
            revision.parse().unwrap(),
            playscale::markers::NewMarker {
                kind: playscale_core::markers::Kind::Chapter,
                start_ms: 500,
                end_ms: None,
                label: None,
                provenance: playscale_core::markers::Provenance::Detected,
            },
        )
        .await,
        Err(playscale::markers::MarkersError::Rejected(
            playscale_core::markers::MarkerError::TooManyMarkers
        ))
    ));
    let mut seen = 0;
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("{path}?limit=200&cursor={c}"),
            None => format!("{path}?limit=200"),
        };
        let page = f.call("GET", &url, None, &[("authorization", &op)]).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        seen += page.body["items"].as_array().unwrap().len();
        match page.body["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, max, "every marker is readable across pages");
    // Effective pages are bound to one snapshot (here the embedded chapters
    // span several pages): any marker changing between pages refuses the
    // next page instead of mixing two effective sets.
    let effective = format!("{path}?effective=true&limit=200");
    let first = f
        .call("GET", &effective, None, &[("authorization", &op)])
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let next = first.body["next_cursor"].as_str().unwrap().to_owned();
    let resumed = |cursor: &str| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("cursor", cursor)
            .finish();
        format!("{effective}&{q}")
    };
    let unchanged = f
        .call("GET", &resumed(&next), None, &[("authorization", &op)])
        .await;
    assert_eq!(unchanged.status, StatusCode::OK, "{}", unchanged.body);
    sqlx::query("UPDATE markers SET revision=revision+1 WHERE id='chap-2'")
        .execute(&f.app.db)
        .await
        .unwrap();
    problem(
        &f.call("GET", &resumed(&next), None, &[("authorization", &op)])
            .await,
        StatusCode::CONFLICT,
        "markers_changed",
    );
    // A plain cursor is not accepted by the effective view.
    let plain = f
        .call(
            "GET",
            &format!("{path}?limit=200"),
            None,
            &[("authorization", &op)],
        )
        .await;
    problem(
        &f.call(
            "GET",
            &resumed(plain.body["next_cursor"].as_str().unwrap()),
            None,
            &[("authorization", &op)],
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_cursor",
    );
    sqlx::query("DELETE FROM markers WHERE id LIKE 'bulk-%'")
        .execute(&f.app.db)
        .await
        .unwrap();
    // After the timeline changes, the old markers stay listed but none is in effect.
    sqlx::query("UPDATE timelines SET revision=revision+1 WHERE id='film-a-ed'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let effective = f
        .call(
            "GET",
            &format!("{path}?effective=true"),
            None,
            &[("authorization", &op)],
        )
        .await;
    assert!(ids(&effective).is_empty(), "{}", effective.body);
    assert_eq!(
        ids(&f.call("GET", path, None, &[("authorization", &op)]).await).len(),
        4
    );
}

#[tokio::test]
async fn later_manual_marker_correction_supersedes_earlier_value() {
    let f = Fixture::new().await;
    catalog(&f).await;
    let op = bearer(OPERATOR);
    let path = "/api/v2/catalog/timelines/film-a-ed/markers";
    let timeline = f
        .call(
            "GET",
            "/api/v2/catalog/timelines/film-a-ed",
            None,
            &[("authorization", &op)],
        )
        .await;
    let input = json!({"timeline_revision":timeline.body["revision"],"kind":"intro","start_ms":1000,"end_ms":5000,"label":"章".repeat(100)});
    let first = f
        .call(
            "POST",
            path,
            Some(input.clone()),
            &[
                ("authorization", &op),
                ("idempotency-key", "manual-correction-first"),
            ],
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let mut correction = input;
    correction["start_ms"] = json!(2000);
    correction["end_ms"] = json!(6000);
    let second = f
        .call(
            "POST",
            path,
            Some(correction),
            &[
                ("authorization", &op),
                ("idempotency-key", "manual-correction-second"),
            ],
        )
        .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    let page = f
        .call(
            "GET",
            &format!("{path}?effective=true"),
            None,
            &[("authorization", &op)],
        )
        .await;
    assert_eq!(ids(&page), [second.body["id"].as_str().unwrap()]);
    let all = f.call("GET", path, None, &[("authorization", &op)]).await;
    assert_eq!(all.body["items"].as_array().unwrap().len(), 2);
}
