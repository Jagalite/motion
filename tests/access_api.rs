//! HTTP conformance for the A08 identity/system/events adapter: real router,
//! real SQLite, real transactions. Decisions themselves are model-checked in
//! crates/core/tests/access_model.rs.
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
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

    /// Lets a claim poll run without waiting out the poll interval.
    async fn skip_poll_interval(&self) {
        sqlx::query("UPDATE pairings SET last_claim_at=last_claim_at-60")
            .execute(&self.app.db)
            .await
            .unwrap();
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

#[tokio::test]
async fn pairing_lifecycle_idempotency_and_claim_replay() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    let pairing = f
        .call(
            "POST",
            "/api/v2/auth/pairings",
            Some(json!({"device_name":"Laptop","client_name":"motion"})),
            &[],
        )
        .await;
    assert_eq!(pairing.status, StatusCode::CREATED);
    assert_eq!(pairing.headers["cache-control"], "no-store");
    let id = pairing.body["id"].as_str().unwrap();
    let claim_path = format!("/api/v2/auth/pairings/{id}/claim");
    let approve_path = format!("/api/v2/auth/pairings/{id}/approve");
    let claim = json!({"device_code":pairing.body["device_code"]});

    // Unapproved: pending, retryable, then rate-limited until the poll interval.
    let pending = f.call("POST", &claim_path, Some(claim.clone()), &[]).await;
    problem(&pending, StatusCode::CONFLICT, "pairing_pending");
    assert_eq!(pending.body["retryable"], true);
    let fast = f.call("POST", &claim_path, Some(claim.clone()), &[]).await;
    problem(&fast, StatusCode::TOO_MANY_REQUESTS, "slow_down");
    assert!(fast.headers.contains_key("retry-after"));
    // A wrong device code is indistinguishable from an unknown pairing.
    let wrong = f
        .call(
            "POST",
            &claim_path,
            Some(json!({"device_code":"mdc_00000000000000000000000000000000"})),
            &[],
        )
        .await;
    problem(&wrong, StatusCode::NOT_FOUND, "not_found");

    let approval = json!({"user_code":pairing.body["user_code"],"profile_ids":["default"],"permissions":["catalog:read","events:read"]});
    let anonymous = f
        .call(
            "POST",
            &approve_path,
            Some(approval.clone()),
            &[("idempotency-key", "approve-key-0000001")],
        )
        .await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let missing_key = f
        .call(
            "POST",
            &approve_path,
            Some(approval.clone()),
            &[("authorization", &operator)],
        )
        .await;
    problem(
        &missing_key,
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
    let wrong_code = f
        .call(
            "POST",
            &approve_path,
            Some(json!({"user_code":"ZZZZ-ZZZZ","profile_ids":[],"permissions":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "approve-key-0000002"),
            ],
        )
        .await;
    problem(
        &wrong_code,
        StatusCode::UNPROCESSABLE_ENTITY,
        "user_code_mismatch",
    );
    let unknown_profile = f
        .call(
            "POST",
            &approve_path,
            Some(json!({"user_code":pairing.body["user_code"],"profile_ids":["nope"],"permissions":[]})),
            &[("authorization", &operator), ("idempotency-key", "approve-key-0000003")],
        )
        .await;
    problem(
        &unknown_profile,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown_profile",
    );

    let headers = [
        ("authorization", operator.as_str()),
        ("idempotency-key", "approve-key-0000004"),
    ];
    let approved = f
        .call("POST", &approve_path, Some(approval.clone()), &headers)
        .await;
    assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
    assert_eq!(approved.headers["etag"], "\"r-1\"");
    assert_eq!(approved.body["revoked"], false);
    // Exact retry replays the acknowledgement; a different body conflicts;
    // a fresh key observes the decided state.
    let replay = f
        .call("POST", &approve_path, Some(approval.clone()), &headers)
        .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body, approved.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let mut other = approval.clone();
    other["permissions"] = json!(["catalog:read"]);
    let conflict = f.call("POST", &approve_path, Some(other), &headers).await;
    problem(&conflict, StatusCode::CONFLICT, "idempotency_key_reused");
    let again = f
        .call(
            "POST",
            &approve_path,
            Some(approval),
            &[
                ("authorization", &operator),
                ("idempotency-key", "approve-key-0000005"),
            ],
        )
        .await;
    problem(&again, StatusCode::CONFLICT, "pairing_already_decided");
    let devices: i64 = sqlx::query_scalar("SELECT count(*) FROM devices")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(devices, 1);

    f.skip_poll_interval().await;
    let claimed = f.call("POST", &claim_path, Some(claim.clone()), &[]).await;
    assert_eq!(claimed.status, StatusCode::OK, "{:?}", claimed.body);
    let token = claimed.body["access_token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("mdv_"));
    // A lost acknowledgement replays the same credential; nothing is rotated.
    f.skip_poll_interval().await;
    let replayed = f.call("POST", &claim_path, Some(claim), &[]).await;
    assert_eq!(replayed.status, StatusCode::OK);
    assert_eq!(replayed.body, claimed.body);
    let stored: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM credentials")
        .fetch_all(&f.app.db)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert!(!stored[0].contains(&token[4..]), "plaintext secret stored");

    let me = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(me.status, StatusCode::OK, "{:?}", me.body);
    assert_eq!(me.body["mode"], "paired");
    assert_eq!(me.body["profile_ids"], json!(["default"]));
    assert_eq!(
        me.body["permissions"],
        json!(["catalog:read", "events:read"])
    );
    assert_eq!(me.body["policy_revision"], "1");
}

#[tokio::test]
async fn access_tokens_are_bounded_children_and_replay_exactly() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let (_, device) = f.pair(&[], &["catalog:read"]).await;
    let device_auth = bearer(&device);
    let headers = [
        ("authorization", device_auth.as_str()),
        ("idempotency-key", "access-token-key-01"),
    ];
    let issued = f
        .call(
            "POST",
            "/api/v2/auth/access-tokens",
            Some(json!({"ttl_seconds":600})),
            &headers,
        )
        .await;
    assert_eq!(issued.status, StatusCode::CREATED, "{:?}", issued.body);
    let access = issued.body["access_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("mat_"));
    let replay = f
        .call(
            "POST",
            "/api/v2/auth/access-tokens",
            Some(json!({"ttl_seconds":600})),
            &headers,
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body, issued.body);
    let conflict = f
        .call(
            "POST",
            "/api/v2/auth/access-tokens",
            Some(json!({"ttl_seconds":900})),
            &headers,
        )
        .await;
    problem(&conflict, StatusCode::CONFLICT, "idempotency_key_reused");
    let me = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(&access))],
        )
        .await;
    assert_eq!(me.status, StatusCode::OK);
    // Children cannot derive; the operator token is not a device credential.
    for token in [access.as_str(), OPERATOR] {
        let denied = f
            .call(
                "POST",
                "/api/v2/auth/access-tokens",
                Some(json!({"ttl_seconds":600})),
                &[
                    ("authorization", &bearer(token)),
                    ("idempotency-key", "access-token-key-02"),
                ],
            )
            .await;
        problem(&denied, StatusCode::FORBIDDEN, "parent_credential_required");
    }
    let invalid = f
        .call(
            "POST",
            "/api/v2/auth/access-tokens",
            Some(json!({"ttl_seconds":30})),
            &[
                ("authorization", &device_auth),
                ("idempotency-key", "access-token-key-03"),
            ],
        )
        .await;
    problem(&invalid, StatusCode::UNPROCESSABLE_ENTITY, "invalid_ttl");
    // Deleting the parent row revokes the child even without device revocation.
    sqlx::query("DELETE FROM credentials WHERE kind='device'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let orphan = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(&access))],
        )
        .await;
    assert_eq!(orphan.status, StatusCode::UNAUTHORIZED, "{:?}", orphan.body);
}

#[tokio::test]
async fn revocation_is_conditional_and_denies_every_credential() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    let (device_id, token) = f.pair(&[], &["catalog:read"]).await;
    let access = f
        .call(
            "POST",
            "/api/v2/auth/access-tokens",
            Some(json!({"ttl_seconds":600})),
            &[
                ("authorization", &bearer(&token)),
                ("idempotency-key", "access-token-key-11"),
            ],
        )
        .await
        .body["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/v2/devices/{device_id}");
    // Device management is administrative.
    let denied = f
        .call("GET", &path, None, &[("authorization", &bearer(&token))])
        .await;
    problem(&denied, StatusCode::FORBIDDEN, "permission_denied");
    let device = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    let etag = device.headers["etag"].to_str().unwrap().to_owned();
    let missing = f
        .call("DELETE", &path, None, &[("authorization", &operator)])
        .await;
    problem(
        &missing,
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    let stale = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", "\"r-0\"")],
        )
        .await;
    problem(
        &stale,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let wildcard = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", "*")],
        )
        .await;
    problem(&wildcard, StatusCode::BAD_REQUEST, "wildcard_precondition");
    let revoked = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", &etag)],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    for t in [&token, &access] {
        let me = f
            .call("GET", "/api/v2/me", None, &[("authorization", &bearer(t))])
            .await;
        assert_eq!(me.status, StatusCode::UNAUTHORIZED);
    }
    let after = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(after.body["revoked"], true);
    assert_eq!(after.headers["etag"], "\"r-2\"");
    let again = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    let page = f
        .call(
            "GET",
            "/api/v2/devices",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(page.body["items"].as_array().unwrap().len(), 1);
    assert!(
        page.body["event_cursor"]
            .as_str()
            .unwrap()
            .contains(".operator.0.")
    );
}

#[tokio::test]
async fn browser_sessions_require_origin_cookie_flags_and_csrf() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let (_, token) = f.pair(&["default"], &["catalog:read"]).await;
    let exchange = json!({"kind":"credential","credential":token});
    let no_origin = f
        .call("POST", "/api/v2/auth/session", Some(exchange.clone()), &[])
        .await;
    problem(&no_origin, StatusCode::FORBIDDEN, "invalid_origin");
    let private = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"trusted_private"})),
            &[("origin", ORIGIN)],
        )
        .await;
    problem(
        &private,
        StatusCode::FORBIDDEN,
        "trusted_private_unavailable",
    );
    let operator = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"credential","credential":OPERATOR})),
            &[("origin", ORIGIN)],
        )
        .await;
    problem(
        &operator,
        StatusCode::FORBIDDEN,
        "parent_credential_required",
    );
    let unknown_field = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"trusted_private","admin":true})),
            &[("origin", ORIGIN)],
        )
        .await;
    problem(
        &unknown_field,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_body",
    );

    let session = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(exchange),
            &[("origin", ORIGIN)],
        )
        .await;
    assert_eq!(session.status, StatusCode::OK, "{:?}", session.body);
    let cookie = session.headers["set-cookie"].to_str().unwrap().to_owned();
    for flag in ["HttpOnly", "SameSite=Strict", "Path=/;", "Max-Age=43200"] {
        assert!(cookie.contains(flag), "{cookie}");
    }
    assert!(!cookie.contains("Secure"), "plain-http origin");
    assert!(session.headers.get_all("set-cookie").iter().any(|v| {
        let value = v.to_str().unwrap();
        value.contains("Path=/api/v2;") && value.contains("Max-Age=0")
    }));
    let pair = cookie.split(';').next().unwrap().to_owned();
    let csrf = session.body["csrf_token"].as_str().unwrap().to_owned();
    let read = f
        .call("GET", "/api/v2/auth/session", None, &[("cookie", &pair)])
        .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["csrf_token"], csrf.as_str());
    // A bearer caller has no browser session.
    let bearer_session = f
        .call(
            "GET",
            "/api/v2/auth/session",
            None,
            &[("authorization", &bearer(&token))],
        )
        .await;
    problem(&bearer_session, StatusCode::NOT_FOUND, "not_found");
    let forged = f
        .call("DELETE", "/api/v2/auth/session", None, &[("cookie", &pair)])
        .await;
    problem(&forged, StatusCode::FORBIDDEN, "csrf_required");
    let wrong = f
        .call(
            "DELETE",
            "/api/v2/auth/session",
            None,
            &[("cookie", &pair), ("x-csrf-token", "csrf_wrong")],
        )
        .await;
    problem(&wrong, StatusCode::FORBIDDEN, "csrf_required");
    let ended = f
        .call(
            "DELETE",
            "/api/v2/auth/session",
            None,
            &[("cookie", &pair), ("x-csrf-token", &csrf)],
        )
        .await;
    assert_eq!(ended.status, StatusCode::NO_CONTENT);
    assert!(
        ended.headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let gone = f
        .call("GET", "/api/v2/me", None, &[("cookie", &pair)])
        .await;
    assert_eq!(gone.status, StatusCode::UNAUTHORIZED);
    // Cross-site browser mutations are rejected before authentication, as a
    // v2 problem with its own request identity.
    let cross = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"trusted_private"})),
            &[("origin", "http://evil.example")],
        )
        .await;
    problem(&cross, StatusCode::FORBIDDEN, "invalid_origin");
    assert_eq!(cross.headers["cache-control"], "no-store");
}

#[tokio::test]
async fn profiles_are_scoped_conditional_and_idempotent() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    let create = |key: &'static str, name: &'static str| {
        let operator = operator.clone();
        let f = &f;
        async move {
            f.call(
                "POST",
                "/api/v2/profiles",
                Some(json!({ "name": name })),
                &[("authorization", &operator), ("idempotency-key", key)],
            )
            .await
        }
    };
    let kids = create("profile-create-key-1", "Kids").await;
    assert_eq!(kids.status, StatusCode::CREATED, "{:?}", kids.body);
    assert_eq!(kids.headers["etag"], "\"r-1\"");
    let replay = create("profile-create-key-1", "Kids").await;
    assert_eq!(replay.body, kids.body);
    let kids_id = kids.body["id"].as_str().unwrap().to_owned();
    create("profile-create-key-2", "Adults").await;

    let (_, viewer) = f.pair(&[&kids_id], &["viewing:write"]).await;
    let viewer = bearer(&viewer);
    let page = f
        .call(
            "GET",
            "/api/v2/profiles",
            None,
            &[("authorization", &viewer)],
        )
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let ids: Vec<_> = page.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, vec![kids_id.clone()]);
    let hidden = f
        .call(
            "GET",
            "/api/v2/profiles/default",
            None,
            &[("authorization", &viewer)],
        )
        .await;
    problem(&hidden, StatusCode::NOT_FOUND, "not_found");
    let denied = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Mine"})),
            &[
                ("authorization", &viewer),
                ("idempotency-key", "profile-create-key-3"),
            ],
        )
        .await;
    problem(&denied, StatusCode::FORBIDDEN, "permission_denied");

    let admin_page = f
        .call(
            "GET",
            "/api/v2/profiles?limit=1",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(admin_page.body["items"].as_array().unwrap().len(), 1);
    let next = admin_page.body["next_cursor"].as_str().unwrap();
    let second = f
        .call(
            "GET",
            &format!("/api/v2/profiles?limit=200&cursor={next}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(second.body["items"].as_array().unwrap().len(), 2);
    assert!(second.body["next_cursor"].is_null());
    let bad_limit = f
        .call(
            "GET",
            "/api/v2/profiles?limit=201",
            None,
            &[("authorization", &operator)],
        )
        .await;
    problem(&bad_limit, StatusCode::BAD_REQUEST, "invalid_limit");

    let path = format!("/api/v2/profiles/{kids_id}");
    let renamed = f
        .call(
            "PUT",
            &path,
            Some(json!({"name":"Children"})),
            &[("authorization", &operator), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(renamed.status, StatusCode::OK);
    assert_eq!(renamed.headers["etag"], "\"r-2\"");
    let stale = f
        .call(
            "PUT",
            &path,
            Some(json!({"name":"Kids"})),
            &[("authorization", &operator), ("if-match", "\"r-1\"")],
        )
        .await;
    problem(
        &stale,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    let blank = f
        .call(
            "PUT",
            &path,
            Some(json!({"name":" "})),
            &[("authorization", &operator), ("if-match", "\"r-2\"")],
        )
        .await;
    problem(&blank, StatusCode::UNPROCESSABLE_ENTITY, "invalid_field");
    let default = f
        .call(
            "DELETE",
            "/api/v2/profiles/default",
            None,
            &[("authorization", &operator), ("if-match", "\"r-0\"")],
        )
        .await;
    problem(&default, StatusCode::CONFLICT, "default_profile");
    let removed = f
        .call(
            "DELETE",
            &path,
            None,
            &[("authorization", &operator), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    let me = f
        .call("GET", "/api/v2/me", None, &[("authorization", &viewer)])
        .await;
    assert_eq!(me.body["profile_ids"], json!([]));
}

#[tokio::test]
async fn device_policy_is_validated_and_advances_the_principal_revision() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    let (device_id, token) = f.pair(&[], &["catalog:read"]).await;
    let path = format!("/api/v2/devices/{device_id}/policy");
    let policy = f
        .call("GET", &path, None, &[("authorization", &operator)])
        .await;
    assert_eq!(policy.body["library_ids"], json!([]));
    assert_eq!(policy.body["allow_unrated"], true);
    let unknown = f
        .call(
            "PUT",
            &path,
            Some(json!({"library_ids":["missing"],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read"]})),
            &[("authorization", &operator), ("if-match", "\"r-1\"")],
        )
        .await;
    problem(
        &unknown,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown_library",
    );
    let body = json!({"library_ids":[f.library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read","events:read"]});
    let replaced = f
        .call(
            "PUT",
            &path,
            Some(body.clone()),
            &[("authorization", &operator), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{:?}", replaced.body);
    assert_eq!(replaced.headers["etag"], "\"r-2\"");
    // An identical replacement is not a new revision.
    let same = f
        .call(
            "PUT",
            &path,
            Some(body),
            &[("authorization", &operator), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(same.headers["etag"], "\"r-2\"");
    let me = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(me.body["policy_revision"], "2");
    assert_eq!(
        me.body["permissions"],
        json!(["catalog:read", "events:read"])
    );
}

/// Reads SSE records until `count` arrive or the stream ends.
async fn sse(body: &mut Body, count: usize) -> (Vec<(String, Value)>, bool) {
    let mut out = vec![];
    let mut buffer = String::new();
    while out.len() < count {
        let frame = match tokio::time::timeout(Duration::from_secs(8), body.frame()).await {
            Ok(Some(Ok(frame))) => frame,
            Ok(None) => return (out, true),
            Ok(Some(Err(e))) => panic!("{e}"),
            Err(_) => panic!("timed out waiting for events; got {out:?}"),
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        buffer.push_str(&String::from_utf8_lossy(&data));
        while let Some(end) = buffer.find("\n\n") {
            let record: String = buffer.drain(..end + 2).collect();
            let mut event = String::new();
            let mut payload = String::new();
            for line in record.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().into();
                } else if let Some(v) = line.strip_prefix("data:") {
                    payload.push_str(v.trim_start());
                }
            }
            if !payload.is_empty() {
                out.push((event, serde_json::from_str(&payload).unwrap()));
            }
        }
    }
    (out, false)
}

#[tokio::test]
async fn event_stream_is_scoped_resets_on_policy_and_ends_on_revocation() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    let (_, silent) = f.pair(&["default"], &["catalog:read"]).await;
    let denied = f
        .call(
            "GET",
            "/api/v2/events",
            None,
            &[("authorization", &bearer(&silent))],
        )
        .await;
    problem(&denied, StatusCode::FORBIDDEN, "permission_denied");
    let bad = f
        .call(
            "GET",
            "/api/v2/events",
            None,
            &[("authorization", &operator), ("last-event-id", "garbage")],
        )
        .await;
    problem(&bad, StatusCode::BAD_REQUEST, "invalid_cursor");

    let (device_id, token) = f.pair(&["default"], &["events:read"]).await;
    let auth = bearer(&token);
    let response = f
        .raw(f.request("GET", "/api/v2/events", None, &[("authorization", &auth)]))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let (events, _) = sse(&mut body, 1).await;
    assert_eq!(events[0].0, "reset");
    assert_eq!(events[0].1["reason"], "initial_snapshot");
    let cursor = events[0].1["cursor"].as_str().unwrap().to_owned();
    assert!(cursor.contains(&format!(".{device_id}.1.")), "{cursor}");

    // A hidden profile change is skipped; a granted one is delivered.
    let other = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Hidden"})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "profile-events-key-1"),
            ],
        )
        .await;
    assert_eq!(other.status, StatusCode::CREATED);
    sqlx::query("UPDATE profiles SET name='Everyone!',revision=revision+1 WHERE id='default'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let (events, _) = sse(&mut body, 1).await;
    assert_eq!(events[0].0, "changed");
    assert_eq!(events[0].1["resource_type"], "profile");
    assert_eq!(events[0].1["resource_id"], "default");

    // A policy change resets the stream under the new revision.
    let policy_path = format!("/api/v2/devices/{device_id}/policy");
    let replaced = f
        .call(
            "PUT",
            &policy_path,
            Some(json!({"library_ids":[f.library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["events:read","catalog:read"]})),
            &[("authorization", &operator), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(replaced.status, StatusCode::OK);
    let (events, _) = sse(&mut body, 1).await;
    assert_eq!(events[0].0, "reset");
    assert_eq!(events[0].1["reason"], "policy_changed");
    assert_eq!(events[0].1["policy_revision"], "2");

    // Library hints are now visible; an old cursor resumes only as a reset.
    sqlx::query("UPDATE libraries SET name='Films' WHERE id=?")
        .bind(&f.library)
        .execute(&f.app.db)
        .await
        .unwrap();
    let (events, _) = sse(&mut body, 1).await;
    assert_eq!(events[0].1["resource_type"], "library");
    // File mutations are hinted as their item. Once the last file is gone the
    // item's scope is unknown, so the subscriber is reset rather than told
    // nothing (or told about something it may not see).
    for sql in [
        "INSERT INTO items(id,title,kind) VALUES ('item1','Film','video')",
        "INSERT INTO editions(id,item_id,label) VALUES ('ed1','item1','Original')",
    ] {
        sqlx::query(sql).execute(&f.app.db).await.unwrap();
    }
    sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES ('file1','ed1',?,'film.mp4','r1','fp',1)")
        .bind(&f.library)
        .execute(&f.app.db)
        .await
        .unwrap();
    let (events, _) = sse(&mut body, 1).await;
    assert_eq!(events[0].1["resource_type"], "catalog_item");
    assert_eq!(events[0].1["resource_id"], "item1");
    // The item also gains a file in a library this device cannot see.
    let hidden_root = f._dir.path().join("hidden");
    std::fs::create_dir(&hidden_root).unwrap();
    let hidden = db::add_library(&f.app.db, "Hidden", &hidden_root)
        .await
        .unwrap()
        .id;
    sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES ('file2','ed1',?,'film.mkv','r1','fp',1)")
        .bind(&hidden)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM media_files WHERE id='file1'")
        .execute(&f.app.db)
        .await
        .unwrap();
    // Item, edition and file inserts each hinted the item; drain them.
    let reset = loop {
        let (events, _) = sse(&mut body, 1).await;
        let (kind, hint) = events.into_iter().next().unwrap();
        if kind == "reset" {
            break hint;
        }
        assert_eq!(hint["resource_id"], "item1", "{hint}");
    };
    assert_eq!(reset["reason"], "scope_unknown");
    assert!(reset["resource_id"].is_null());
    let resumed = f
        .raw(f.request(
            "GET",
            "/api/v2/events",
            None,
            &[("authorization", &auth), ("last-event-id", &cursor)],
        ))
        .await;
    let (events, _) = sse(&mut resumed.into_body(), 1).await;
    assert_eq!(events[0].1["reason"], "policy_changed");

    // Revocation ends the stream.
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{device_id}"),
            None,
            &[("authorization", &operator), ("if-match", "\"r-2\"")],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let (_, ended) = sse(&mut body, 10).await;
    assert!(ended);
}

#[tokio::test]
async fn page_event_cursor_resumes_without_reset() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    let page = f
        .call(
            "GET",
            "/api/v2/profiles",
            None,
            &[("authorization", &operator)],
        )
        .await;
    let cursor = page.body["event_cursor"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE profiles SET name='Renamed' WHERE id='default'")
        .execute(&f.app.db)
        .await
        .unwrap();
    let response = f
        .raw(f.request(
            "GET",
            &format!("/api/v2/events?after={cursor}"),
            None,
            &[("authorization", &operator)],
        ))
        .await;
    let (events, _) = sse(&mut response.into_body(), 1).await;
    assert_eq!(events[0].0, "changed");
    assert_eq!(events[0].1["resource_id"], "default");
}

#[tokio::test]
async fn restricted_mode_closes_the_legacy_surface() {
    let open = Fixture::new(AccessMode::TrustedHousehold).await;
    let reply = open.call("GET", "/api/v1/libraries", None, &[]).await;
    assert_eq!(reply.status, StatusCode::OK);

    let f = Fixture::new(AccessMode::Restricted).await;
    for path in [
        "/api/v1/libraries",
        "/api/v1/items",
        "/api/v1/events",
        "/media/x",
    ] {
        let reply = f.call("GET", path, None, &[]).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(reply.body["code"], "restricted_mode");
    }
    let (_, device) = f.pair(&["default"], &["catalog:read"]).await;
    let paired = f
        .call(
            "GET",
            "/api/v1/libraries",
            None,
            &[("authorization", &bearer(&device))],
        )
        .await;
    assert_eq!(paired.status, StatusCode::UNAUTHORIZED);
    // A paired administrator's v2 scope is the whole catalog, so v1 reads
    // (which are not library-scoped) are admitted for it.
    let (_, admin) = f.pair(&[], &["system:admin"]).await;
    let admin_read = f
        .call(
            "GET",
            "/api/v1/libraries",
            None,
            &[("authorization", &bearer(&admin))],
        )
        .await;
    assert_eq!(admin_read.status, StatusCode::OK);
    let operator = f
        .call(
            "GET",
            "/api/v1/libraries",
            None,
            &[("authorization", &bearer(OPERATOR))],
        )
        .await;
    assert_eq!(operator.status, StatusCode::OK);
    let caps = f
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &bearer(&device))],
        )
        .await;
    let restricted = caps.body["features"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == "access.restricted_mode")
        .unwrap();
    assert_eq!(restricted["enabled"], true);
}

#[tokio::test]
async fn problems_contract_and_capabilities() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let operator = bearer(OPERATOR);
    problem(
        &f.call("GET", "/api/v2/nope", None, &[]).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &f.call("PATCH", "/api/v2/me", None, &[("authorization", &operator)])
            .await,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
    let text = f
        .raw(
            Request::builder()
                .method("POST")
                .uri("/api/v2/auth/pairings")
                .header("host", "127.0.0.1:8787")
                .header("content-type", "text/plain")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(text.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let malformed = f
        .raw(
            Request::builder()
                .method("POST")
                .uri("/api/v2/auth/pairings")
                .header("host", "127.0.0.1:8787")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let extra = f
        .call(
            "POST",
            "/api/v2/auth/pairings",
            Some(json!({"device_name":"a","client_name":"b","role":"admin"})),
            &[],
        )
        .await;
    problem(&extra, StatusCode::UNPROCESSABLE_ENTITY, "invalid_body");
    let large = f
        .call(
            "POST",
            "/api/v2/auth/pairings",
            Some(json!({"device_name":"x".repeat(300 * 1024),"client_name":"b"})),
            &[],
        )
        .await;
    problem(&large, StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
    let anonymous = f.call("GET", "/api/v2/me", None, &[]).await;
    problem(
        &anonymous,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );
    let malformed_auth = f
        .call("GET", "/api/v2/me", None, &[("authorization", "Basic abc")])
        .await;
    problem(
        &malformed_auth,
        StatusCode::UNAUTHORIZED,
        "authentication_required",
    );

    let contract = f.call("GET", "/api/v2/openapi.json", None, &[]).await;
    assert_eq!(contract.status, StatusCode::OK);
    assert_eq!(contract.body["openapi"], "3.1.0");
    assert_eq!(contract.body["info"]["version"], "2.0.0");
    let health = f.call("GET", "/api/v2/system/health", None, &[]).await;
    assert_eq!(health.body["status"], "degraded");
    let caps = f
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(caps.status, StatusCode::OK, "{:?}", caps.body);
    assert_eq!(caps.body["server_id"], health.body["server_id"]);
    assert_eq!(caps.body["server_epoch"], health.body["server_epoch"]);
    // The newest migration on disk, so a new or renumbered migration needs no edit here.
    let latest: i64 = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().to_str()?.get(..4)?.parse().ok())
        .max()
        .unwrap();
    assert_eq!(caps.body["schema_version"], latest.to_string());
    assert_eq!(
        caps.body["contract_digest"],
        playscale::v2::system::contract_digest()
    );
    assert!(
        caps.body["runtime_hashes"]["server"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[tokio::test]
async fn revocation_after_admission_is_enforced_inside_the_transaction() {
    let f = Fixture::new(AccessMode::TrustedHousehold).await;
    let (device_id, token) = f.pair(&["default"], &["profiles:manage"]).await;
    let mut headers = HeaderMap::new();
    headers.insert("authorization", bearer(&token).parse().unwrap());
    // Admitted while valid, as when a request body is still uploading.
    let caller = playscale::v2::auth::resolve(&f.app, &axum::http::Method::POST, &headers)
        .await
        .unwrap();
    sqlx::query("UPDATE devices SET revoked=1,revision=revision+1 WHERE id=?")
        .bind(&device_id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let mut conn = f.app.db.acquire().await.unwrap();
    let denied = caller
        .reauthorize(
            &mut conn,
            Some(playscale_core::access::Permission::ProfilesManage),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, "credential_revoked");
}

#[test]
fn served_contract_matches_reviewed_yaml() {
    let json: Value = serde_json::from_str(playscale::v2::system::CONTRACT_JSON).unwrap();
    let yaml = std::str::from_utf8(playscale::v2::system::CONTRACT_YAML).unwrap();
    let yaml_paths = yaml.lines().filter(|l| l.starts_with("  /api/v2/")).count();
    assert_eq!(json["paths"].as_object().unwrap().len(), yaml_paths);
    // Full parity needs a YAML parser; the development tool provides one.
    let status = std::process::Command::new("ruby")
        .args(["scripts/contract_json.rb", "--check"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("ruby is required to check the served contract against the YAML");
    assert!(status.success(), "contract JSON is stale or unreadable");
}

#[test]
fn deployments_are_restricted_unless_household_is_chosen() {
    assert_eq!(
        playscale::config::Settings::default().access_mode,
        AccessMode::Restricted
    );
}

impl Fixture {
    /// Scan one file into the library and return (file_id, revision).
    async fn scanned_file(&self, name: &str, bytes: &[u8]) -> (String, String) {
        std::fs::write(self._dir.path().join("media").join(name), bytes).unwrap();
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
        sqlx::query_as("SELECT id,revision FROM media_files WHERE relative_path=?")
            .bind(name)
            .fetch_one(&self.app.db)
            .await
            .unwrap()
    }

    /// Give a paired device access to the fixture library.
    async fn grant_library(&self, device_id: &str, permissions: &[&str]) {
        let etag = self.device_etag(device_id).await;
        let reply = self
            .call(
                "PUT",
                &format!("/api/v2/devices/{device_id}/policy"),
                Some(json!({"library_ids":[self.library],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":permissions})),
                &[("authorization", &bearer(OPERATOR)), ("if-match", &etag)],
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    }

    async fn device_etag(&self, device_id: &str) -> String {
        self.call(
            "GET",
            &format!("/api/v2/devices/{device_id}"),
            None,
            &[("authorization", &bearer(OPERATOR))],
        )
        .await
        .headers["etag"]
            .to_str()
            .unwrap()
            .to_owned()
    }

    async fn bytes(
        &self,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = self.raw(self.request("GET", path, None, headers)).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }
}

#[tokio::test]
async fn media_bytes_require_scope_and_byte_permission() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let (file, revision) = f.scanned_file("film.mp4", b"0123456789").await;
    let path = format!("/api/v2/media/files/{file}/content?revision={revision}");
    let (viewer_id, viewer) = f.pair(&["default"], &["catalog:read"]).await;
    f.grant_library(&viewer_id, &["catalog:read", "playback:request"])
        .await;
    let (_, outsider) = f
        .pair(&["default"], &["catalog:read", "playback:request"])
        .await;
    let (reader_id, reader) = f.pair(&["default"], &["catalog:read"]).await;
    f.grant_library(&reader_id, &["catalog:read"]).await;

    let (status, headers, body) = f.bytes(&path, &[("authorization", &bearer(&viewer))]).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, &b"0123456789"[..])
    );
    assert_eq!(headers["etag"], format!("\"{revision}\"").as_str());
    let (status, headers, body) = f
        .bytes(
            &path,
            &[("authorization", &bearer(&viewer)), ("range", "bytes=2-4")],
        )
        .await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &b"234"[..])
    );
    assert_eq!(headers["content-range"], "bytes 2-4/10");
    // Unsatisfiable ranges keep their protocol headers.
    let (status, headers, _) = f
        .bytes(
            &path,
            &[
                ("authorization", &bearer(&viewer)),
                ("range", "bytes=50-60"),
            ],
        )
        .await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers["content-range"], "bytes */10");
    let head = f
        .raw(f.request("HEAD", &path, None, &[("authorization", &bearer(&viewer))]))
        .await;
    assert_eq!(head.status(), StatusCode::OK);
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    // Outside the library: indistinguishable from missing. Metadata-only: denied.
    let (status, _, _) = f
        .bytes(&path, &[("authorization", &bearer(&outsider))])
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = f
        .bytes(
            "/api/v2/media/files/nope/content",
            &[("authorization", &bearer(&outsider))],
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = f.bytes(&path, &[("authorization", &bearer(&reader))]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = f.bytes(&path, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A stale revision is a conflict, not stale bytes.
    let (status, _, _) = f
        .bytes(
            &format!("/api/v2/media/files/{file}/content?revision=old"),
            &[("authorization", &bearer(&viewer))],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn content_tickets_are_narrow_revocable_and_replayable() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let (file, revision) = f.scanned_file("film.mp4", b"0123456789").await;
    let (other_file, _) = f.scanned_file("other.mp4", b"abc").await;
    let (device_id, token) = f.pair(&["default"], &["catalog:read"]).await;
    f.grant_library(&device_id, &["catalog:read", "playback:request"])
        .await;
    let auth = bearer(&token);
    let request = |purpose: &str| {
        json!({"purpose":purpose,"delivery_id":null,"generation":null,
               "file":{"file_id":file,"file_revision":revision},"download_id":null,"ttl_seconds":600})
    };
    let create = |body: Value, key: &'static str| {
        let auth = auth.clone();
        let f = &f;
        async move {
            f.call(
                "POST",
                "/api/v2/content-access",
                Some(body),
                &[("authorization", &auth), ("idempotency-key", key)],
            )
            .await
        }
    };
    let ticket = create(request("playback"), "ticket-key-000001").await;
    assert_eq!(ticket.status, StatusCode::CREATED, "{:?}", ticket.body);
    let url = ticket.body["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&format!("/api/v2/media/files/{file}/content?revision=")));
    let replay = create(request("playback"), "ticket-key-000001").await;
    assert_eq!(replay.body, ticket.body);
    let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM content_tickets")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(tickets, 1);

    // The ticket alone admits exactly that file and revision.
    let (status, _, body) = f.bytes(&url, &[]).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, &b"0123456789"[..])
    );
    let token_param = url.split("ticket=").nth(1).unwrap();
    let (status, _, _) = f
        .bytes(
            &format!("/api/v2/media/files/{other_file}/content?ticket={token_param}"),
            &[],
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Tickets do not authenticate JSON operations.
    let me = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(token_param))],
        )
        .await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED);

    // Purpose, family and revision rules.
    problem(
        &create(request("download"), "ticket-key-000002").await,
        StatusCode::FORBIDDEN,
        "permission_denied",
    );
    problem(
        &create(request("cast"), "ticket-key-000003").await,
        StatusCode::FORBIDDEN,
        "cast_unavailable",
    );
    let mut stale = request("playback");
    stale["file"]["file_revision"] = json!("old");
    problem(
        &create(stale, "ticket-key-000004").await,
        StatusCode::CONFLICT,
        "source_revision_changed",
    );
    let mut both = request("playback");
    both["download_id"] = json!("d1");
    problem(
        &create(both, "ticket-key-000005").await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_resource_family",
    );
    let delivery = json!({"purpose":"playback","delivery_id":"d1","generation":"1","file":null,"download_id":null,"ttl_seconds":60});
    problem(
        &create(delivery, "ticket-key-000006").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Revocation by another principal is invisible; by the owner it applies.
    let id = ticket.body["id"].as_str().unwrap();
    let (_, stranger) = f.pair(&["default"], &["catalog:read"]).await;
    let denied = f
        .call(
            "DELETE",
            &format!("/api/v2/content-access/{id}"),
            None,
            &[("authorization", &bearer(&stranger))],
        )
        .await;
    problem(&denied, StatusCode::NOT_FOUND, "not_found");
    let second = create(request("playback"), "ticket-key-000007").await;
    let second_url = second.body["url"].as_str().unwrap().to_owned();
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/content-access/{id}"),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let (status, _, _) = f.bytes(&url, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Narrowing the device's policy applies to tickets already issued.
    f.grant_library(&device_id, &["catalog:read"]).await;
    let (status, _, _) = f.bytes(&second_url, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Revoking the device kills every ticket it issued.
    let etag = f.device_etag(&device_id).await;
    let revoke = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{device_id}"),
            None,
            &[("authorization", &bearer(OPERATOR)), ("if-match", &etag)],
        )
        .await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
    let (status, _, _) = f.bytes(&second_url, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn approved_origins_get_bearer_only_cors_and_principals_are_rate_limited() {
    let mut f = Fixture::new(AccessMode::Restricted).await;
    let settings = playscale::v2::ApiSettings {
        approved_origins: vec!["https://app.example".into()],
        requests_per_minute: 3,
        trusted_ingress: None,
    };
    f.app.access = Arc::new(
        playscale::v2::Runtime::new(AccessMode::Restricted, playscale::v2::auth::random_key())
            .with_settings(settings),
    );
    let preflight = f
        .call(
            "OPTIONS",
            "/api/v2/profiles",
            None,
            &[
                ("origin", "https://app.example"),
                ("access-control-request-method", "POST"),
            ],
        )
        .await;
    assert_eq!(preflight.status, StatusCode::NO_CONTENT);
    assert_eq!(
        preflight.headers["access-control-allow-origin"],
        "https://app.example"
    );
    assert!(
        !preflight
            .headers
            .contains_key("access-control-allow-credentials")
    );
    let other = f
        .call(
            "OPTIONS",
            "/api/v2/profiles",
            None,
            &[
                ("origin", "https://evil.example"),
                ("access-control-request-method", "POST"),
            ],
        )
        .await;
    assert!(!other.headers.contains_key("access-control-allow-origin"));

    let operator = bearer(OPERATOR);
    let cross = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Remote"})),
            &[
                ("authorization", &operator),
                ("origin", "https://app.example"),
                ("idempotency-key", "cors-create-key-01"),
            ],
        )
        .await;
    assert_eq!(cross.status, StatusCode::CREATED, "{:?}", cross.body);
    assert_eq!(
        cross.headers["access-control-allow-origin"],
        "https://app.example"
    );
    assert!(
        cross.headers["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .contains("etag")
    );
    // An ambient cookie never rides a cross-origin request.
    let cookie = f
        .call(
            "POST",
            "/api/v2/profiles",
            Some(json!({"name":"Remote"})),
            &[
                ("authorization", &operator),
                ("cookie", "motion_session=x"),
                ("origin", "https://app.example"),
                ("idempotency-key", "cors-create-key-02"),
            ],
        )
        .await;
    problem(&cookie, StatusCode::FORBIDDEN, "invalid_origin");

    // The operator has used 1 of 3; the bucket refills at 3/minute.
    for _ in 0..2 {
        let ok = f
            .call("GET", "/api/v2/me", None, &[("authorization", &operator)])
            .await;
        assert_eq!(ok.status, StatusCode::OK);
    }
    let limited = f
        .call("GET", "/api/v2/me", None, &[("authorization", &operator)])
        .await;
    problem(&limited, StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    assert!(limited.headers.contains_key("retry-after"));
}

#[tokio::test]
async fn trusted_ingress_identity_is_listener_bound_mapped_and_never_admin() {
    let mut f = Fixture::new(AccessMode::Restricted).await;
    let (device_id, _) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let settings = playscale::v2::ApiSettings {
        trusted_ingress: Some(playscale::v2::IngressSettings {
            socket: "/tmp/unused.sock".into(),
            login_header: "tailscale-user-login".into(),
            logins: [("alice@example.com".to_string(), device_id.clone())].into(),
        }),
        ..Default::default()
    };
    let key = playscale::v2::auth::random_key();
    f.app.access =
        Arc::new(playscale::v2::Runtime::new(AccessMode::Restricted, key).with_settings(settings));
    let ingress = |method: &str, path: &str, body: Option<Value>, headers: Vec<(&str, String)>| {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:8787");
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        for (k, v) in headers {
            request = request.header(k, v);
        }
        let router = api::router(f.app.clone(), None)
            .layer(axum::Extension(playscale::v2::auth::TrustedIngress));
        async move {
            let response = router
                .oneshot(
                    request
                        .body(body.map(|v| Body::from(v.to_string())).unwrap_or_default())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let alice = ("tailscale-user-login", "alice@example.com".to_string());
    let (status, me) = ingress("GET", "/api/v2/me", None, vec![alice.clone()]).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["mode"], "trusted_private");
    assert!(
        !me["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("system:admin"))
    );
    let (status, _) = ingress(
        "GET",
        "/api/v2/me",
        None,
        vec![("tailscale-user-login", "mallory@example.com".into())],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The same header on the ordinary listener is ignored.
    let plain = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("tailscale-user-login", "alice@example.com")],
        )
        .await;
    assert_eq!(plain.status, StatusCode::UNAUTHORIZED);
    // The exchange returns the CSRF token unsafe requests need.
    let (status, session) = ingress(
        "POST",
        "/api/v2/auth/session",
        Some(json!({"kind":"trusted_private"})),
        vec![alice.clone(), ("origin", ORIGIN.into())],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let csrf = session["csrf_token"].as_str().unwrap().to_owned();
    let refused = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"trusted_private"})),
            &[
                ("origin", ORIGIN),
                ("tailscale-user-login", "alice@example.com"),
            ],
        )
        .await;
    problem(
        &refused,
        StatusCode::FORBIDDEN,
        "trusted_private_unavailable",
    );
    let (status, _) = ingress(
        "DELETE",
        "/api/v2/auth/session",
        None,
        vec![alice.clone(), ("origin", ORIGIN.into())],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = ingress("GET", "/api/v2/devices", None, vec![alice.clone()]).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "ingress callers are never administrators"
    );
    let _ = csrf;
    // Revoking the mapped device fails the identity closed.
    let etag = f.device_etag(&device_id).await;
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{device_id}"),
            None,
            &[("authorization", &bearer(OPERATOR)), ("if-match", &etag)],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    let (status, _) = ingress("GET", "/api/v2/me", None, vec![alice]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn desktop_bootstrap_is_one_use_and_never_exposed() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let secret = "desktop-bootstrap-0123456789abcdef0123456789";
    f.app.access.set_bootstrap(secret).unwrap();
    let exchange = json!({"kind":"credential","credential":secret});
    let wrong = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"credential","credential":"desktop-bootstrap-wrong-0123456789abcdef"})),
            &[("origin", ORIGIN)],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let session = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(exchange.clone()),
            &[("origin", ORIGIN)],
        )
        .await;
    assert_eq!(session.status, StatusCode::OK, "{:?}", session.body);
    assert_eq!(session.body["principal"]["device_id"], "desktop-local");
    assert!(
        session.headers["set-cookie"]
            .to_str()
            .unwrap()
            .contains("HttpOnly")
    );
    let cookie = session.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let me = f
        .call("GET", "/api/v2/me", None, &[("cookie", &cookie)])
        .await;
    assert_eq!(me.status, StatusCode::OK);
    let again = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(exchange),
            &[("origin", ORIGIN)],
        )
        .await;
    assert_eq!(
        again.status,
        StatusCode::UNAUTHORIZED,
        "bootstrap is one-use"
    );
    assert!(
        !session.body.to_string().contains("mdv_"),
        "device credential exposed"
    );
}

/// A stand-in presentation router (A11's Topcoat mount plugs in here): it
/// echoes the identity A08 resolved for it.
fn presentation() -> axum::Router {
    axum::Router::new().fallback(|request: Request<Body>| async move {
        let who = request
            .extensions()
            .get::<api::PresentationIdentity>()
            .and_then(|i| i.0.as_ref())
            .map(|c| c.principal.id.clone())
            .unwrap_or_else(|| "anonymous".into());
        (
            [("content-type", "text/html; charset=utf-8")],
            format!("<p>page {} for {who}</p>", request.uri()),
        )
    })
}

#[tokio::test]
async fn presentation_mount_keeps_api_json_and_sees_only_verified_identity() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let (device_id, token) = f.pair(&["default"], &["catalog:read"]).await;
    let session = f
        .call(
            "POST",
            "/api/v2/auth/session",
            Some(json!({"kind":"credential","credential":token})),
            &[("origin", ORIGIN)],
        )
        .await;
    let cookie = session.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let call = |path: &'static str, headers: Vec<(&'static str, String)>| {
        let mut request = Request::builder()
            .uri(path)
            .header("host", "127.0.0.1:8787");
        for (k, v) in headers {
            request = request.header(k, v);
        }
        let router = api::router_with(f.app.clone(), None, Some(presentation()));
        async move {
            let response = router
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            (status, headers, String::from_utf8_lossy(&body).into_owned())
        }
    };
    let (status, headers, body) = call("/library/movies", vec![("cookie", cookie.clone())]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("page /library/movies for "), "{body}");
    assert!(!body.contains("anonymous"));
    assert_eq!(headers["cache-control"], "private, no-store");
    assert!(body.contains(&device_id), "{body}");
    let (_, _, body) = call("/library?sort=title", vec![("cookie", cookie.clone())]).await;
    assert!(
        body.contains("/library?sort=title"),
        "full URI lost: {body}"
    );
    let (_, headers, body) = call("/", vec![("cookie", "motion_session=forged".into())]).await;
    assert_eq!(headers["cache-control"], "private, no-store");
    assert!(body.contains("for anonymous"), "{body}");
    // API, legacy API and media paths never fall through to HTML.
    for path in [
        "/api/v2/unknown",
        "/api/v1/unknown",
        "/media/unknown",
        "/media",
        "/api",
        "/api/v2",
    ] {
        let (status, headers, _) = call(path, vec![]).await;
        assert_ne!(status, StatusCode::OK, "{path}");
        assert!(
            headers["content-type"].to_str().unwrap().contains("json"),
            "{path}: {headers:?}"
        );
    }
    let (status, _, body) = call("/api/v2/system/health", vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("server_id"));
    // Unsafe requests with a valid ambient session must not degrade to anonymous.
    let csrf_denied = api::router_with(f.app.clone(), None, Some(presentation()))
        .oneshot(f.request(
            "POST",
            "/library",
            None,
            &[("cookie", &cookie), ("origin", ORIGIN)],
        ))
        .await
        .unwrap();
    assert_eq!(csrf_denied.status(), StatusCode::FORBIDDEN);
    let wrong_origin = api::router_with(f.app.clone(), None, Some(presentation()))
        .oneshot(f.request(
            "POST",
            "/library",
            None,
            &[
                ("cookie", &cookie),
                ("x-csrf-token", session.body["csrf_token"].as_str().unwrap()),
                ("origin", "https://evil.example"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(wrong_origin.status(), StatusCode::FORBIDDEN);
    sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
        .bind(&device_id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let (_, _, body) = call("/library", vec![("cookie", cookie)]).await;
    assert!(
        body.contains("for anonymous"),
        "revoked identity rendered: {body}"
    );
    // The host boundary applies before presentation.
    let wrong_host = api::router_with(f.app.clone(), None, Some(presentation()))
        .oneshot(
            Request::builder()
                .uri("/library")
                .header("host", "evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_host.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn presentation_identity_honors_only_verified_ingress() {
    let mut f = Fixture::new(AccessMode::Restricted).await;
    let (device_id, _) = f.pair(&["default"], &["catalog:read"]).await;
    let settings = playscale::v2::ApiSettings {
        trusted_ingress: Some(playscale::v2::IngressSettings {
            socket: "/tmp/unused.sock".into(),
            login_header: "tailscale-user-login".into(),
            logins: [("alice@example.com".to_string(), device_id.clone())].into(),
        }),
        ..Default::default()
    };
    f.app.access = Arc::new(
        playscale::v2::Runtime::new(AccessMode::Restricted, playscale::v2::auth::random_key())
            .with_settings(settings),
    );
    for trusted in [false, true] {
        let mut request = f.request(
            "GET",
            "/library",
            None,
            &[("tailscale-user-login", "alice@example.com")],
        );
        if trusted {
            request
                .extensions_mut()
                .insert(playscale::v2::auth::TrustedIngress);
        }
        let response = api::router_with(f.app.clone(), None, Some(presentation()))
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains(if trusted { &device_id } else { "anonymous" }),
            "{body}"
        );
    }
}

#[tokio::test]
async fn presentation_shares_admission_limits_and_propagates_storage_failure() {
    let mut f = Fixture::new(AccessMode::Restricted).await;
    f.app.access = Arc::new(
        playscale::v2::Runtime::new(AccessMode::Restricted, playscale::v2::auth::random_key())
            .with_settings(playscale::v2::ApiSettings {
                requests_per_minute: 1,
                ..Default::default()
            }),
    );
    for expected in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
        let response = api::router_with(f.app.clone(), None, Some(presentation()))
            .oneshot(f.request(
                "GET",
                "/library",
                None,
                &[("authorization", &bearer(OPERATOR))],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    f.app.db.close().await;
    let response = api::router_with(f.app.clone(), None, Some(presentation()))
        .oneshot(f.request("GET", "/library", None, &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn presentation_retains_default_extractor_body_limit() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let pages = axum::Router::new().route(
        "/render",
        axum::routing::post(|body: String| async move { body }),
    );
    let response = api::router_with(f.app.clone(), None, Some(pages))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/render")
                .header("host", "127.0.0.1:8787")
                .body(Body::from("x".repeat(16 * 1024 + 1)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn ingress_capability_separates_implementation_configuration_and_qualification() {
    let mut f = Fixture::new(AccessMode::Restricted).await;
    for configured in [false, true] {
        let settings = playscale::v2::ApiSettings {
            trusted_ingress: configured.then(|| playscale::v2::IngressSettings {
                socket: "/tmp/unused.sock".into(),
                login_header: "tailscale-user-login".into(),
                logins: Default::default(),
            }),
            ..Default::default()
        };
        f.app.access = Arc::new(
            playscale::v2::Runtime::new(AccessMode::Restricted, playscale::v2::auth::random_key())
                .with_settings(settings),
        );
        let reply = f
            .call(
                "GET",
                "/api/v2/system/capabilities",
                None,
                &[("authorization", &bearer(OPERATOR))],
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK);
        let feature = reply.body["features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|feature| feature["id"] == "identity.trusted_private_ingress")
            .unwrap();
        assert_eq!(feature["implemented"], true);
        assert_eq!(feature["enabled"], configured);
        assert_eq!(feature["qualification"], "unqualified");
        assert_eq!(feature["receipt_ids"], json!([]));
    }
}

#[tokio::test]
async fn logical_libraries_are_scoped_before_paging_and_authorize_source_files() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let (file, revision) = f.scanned_file("scope.mp4", b"0123456789").await;
    let operator = bearer(OPERATOR);
    let input =
        json!({"name":"Logical movies","kind":"movies","language":"en","source_ids":[f.library]});
    let headers = [
        ("authorization", operator.as_str()),
        ("idempotency-key", "logical-library-create-1"),
    ];
    let created = f
        .call("POST", "/api/v2/libraries", Some(input.clone()), &headers)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let logical = created.body["id"].as_str().unwrap();
    assert_ne!(logical, f.library);
    assert_eq!(created.body["revision"], "1");
    let replay = f
        .call("POST", "/api/v2/libraries", Some(input), &headers)
        .await;
    assert_eq!(replay.body, created.body);
    let conflict = f
        .call(
            "POST",
            "/api/v2/libraries",
            Some(json!({"name":"Different","kind":"movies","language":"en","source_ids":[]})),
            &headers,
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    let (device, token) = f
        .pair(&["default"], &["catalog:read", "playback:request"])
        .await;
    let etag = f.device_etag(&device).await;
    let policy = f.call("PUT", &format!("/api/v2/devices/{device}/policy"), Some(json!({"library_ids":[logical],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[],"permissions":["catalog:read","playback:request"]})), &[("authorization", &operator),("if-match", &etag)]).await;
    assert_eq!(policy.status, StatusCode::OK, "{}", policy.body);
    let auth = bearer(&token);
    let listed = f
        .call(
            "GET",
            "/api/v2/libraries?limit=1",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed.body["items"][0]["id"], logical);
    assert!(listed.body["next_cursor"].is_null());
    let hidden = f
        .call(
            "GET",
            &format!("/api/v2/libraries/{}", f.library),
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let (status, _, bytes) = f
        .bytes(
            &format!("/api/v2/media/files/{file}/content?revision={revision}"),
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"0123456789");
    let catalog = f
        .call(
            "GET",
            "/api/v2/catalog/items",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(catalog.status, StatusCode::OK, "{}", catalog.body);
    assert_eq!(catalog.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(catalog.body["items"][0]["library_ids"], json!([logical]));
}

#[tokio::test]
async fn source_library_crud_requires_authority_preconditions_and_preserves_media() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let root = f._dir.path().join("new-source");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("keep.txt"), b"keep").unwrap();
    let input = json!({"name":"New source","root_path":root,"exclusions":[]});
    let auth = bearer(OPERATOR);
    let headers = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "source-create-crud-1"),
    ];
    assert_eq!(
        f.call("POST", "/api/v2/sources", Some(input.clone()), &[])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let created = f
        .call("POST", "/api/v2/sources", Some(input.clone()), &headers)
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert!(created.body["binding_revision"].is_string());
    let replay = f
        .call("POST", "/api/v2/sources", Some(input), &headers)
        .await;
    assert_eq!(replay.body, created.body);
    let source = created.body["id"].as_str().unwrap();
    let library_input =
        json!({"name":"Library","kind":"mixed","language":"","source_ids":[source]});
    let library = f
        .call(
            "POST",
            "/api/v2/libraries",
            Some(library_input.clone()),
            &[
                ("authorization", &auth),
                ("idempotency-key", "library-create-crud-1"),
            ],
        )
        .await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let library_path = format!("/api/v2/libraries/{}", library.body["id"].as_str().unwrap());
    assert_eq!(
        f.call(
            "PUT",
            &library_path,
            Some(library_input.clone()),
            &[("authorization", &auth)]
        )
        .await
        .status,
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        f.call(
            "PUT",
            &library_path,
            Some(library_input),
            &[("authorization", &auth), ("if-match", "\"r-0\"")]
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    let source_path = format!("/api/v2/sources/{source}");
    let source_etag = created.headers["etag"].to_str().unwrap();
    assert_eq!(
        f.call(
            "DELETE",
            &source_path,
            None,
            &[("authorization", &auth), ("if-match", source_etag)]
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.call(
            "DELETE",
            &library_path,
            None,
            &[
                ("authorization", &auth),
                ("if-match", library.headers["etag"].to_str().unwrap())
            ]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.call(
            "DELETE",
            &source_path,
            None,
            &[("authorization", &auth), ("if-match", source_etag)]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(std::fs::read(root.join("keep.txt")).unwrap(), b"keep");
    assert_eq!(
        f.call("GET", &source_path, None, &[("authorization", &auth)])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn topcoat_uses_real_authorized_library_reads_and_keeps_api_json() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let app = api::router_with(
        f.app.clone(),
        None,
        Some(playscale::presentation::router(f.app.clone())),
    );
    let response = app
        .clone()
        .oneshot(f.request("GET", "/", None, &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
    let operator = bearer(OPERATOR);
    let response = app
        .clone()
        .oneshot(f.request("GET", "/", None, &[("authorization", &operator)]))
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
    assert!(html.contains("Movies"), "{html}");
    assert!(!html.contains("Development mock"));
    assert!(!html.contains(OPERATOR));
    let response = app
        .clone()
        .oneshot(f.request(
            "GET",
            "/api/v2/not-real",
            None,
            &[("authorization", &operator)],
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
    let response = app
        .oneshot(f.request("HEAD", "/", None, &[("authorization", &operator)]))
        .await
        .unwrap();
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn diagnostics_are_admin_only_and_do_not_expose_worker_output() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let operator = bearer(OPERATOR);
    let denied = f.call("GET", "/api/v2/admin/diagnostics", None, &[]).await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);
    sqlx::query("INSERT INTO jobs(id,library_id,phase,created_at,error) VALUES ('failed-job',?,'failed',0,'secret /private/media/path')").bind(&f.library).execute(&f.app.db).await.unwrap();
    let d = f
        .call(
            "GET",
            "/api/v2/admin/diagnostics",
            None,
            &[("authorization", &operator)],
        )
        .await;
    assert_eq!(d.status, StatusCode::OK, "{:?}", d.body);
    assert_eq!(d.body["worker_errors"], json!(["scan:failed-job:failed"]));
    assert!(!d.body.to_string().contains("private"));
    assert!(
        d.body["uptime_seconds"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
}

#[tokio::test]
async fn jobs_filter_ownership_and_cancel_replays_without_repeating_transition() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let operator = bearer(OPERATOR);
    let (device, token) = f.pair(&["default"], &["processing:request"]).await;
    let auth = bearer(&token);
    sqlx::query("INSERT INTO jobs(id,library_id,phase,created_at,requester_id) VALUES ('owned',?,'queued',0,?)").bind(&f.library).bind(&device).execute(&f.app.db).await.unwrap();
    let page = f
        .call(
            "GET",
            "/api/v2/jobs?limit=1",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(page.status, StatusCode::OK, "{:?}", page.body);
    assert_eq!(page.body["items"][0]["id"], "owned");
    let cancel = f
        .call(
            "POST",
            "/api/v2/jobs/owned/cancel",
            None,
            &[
                ("authorization", &auth),
                ("idempotency-key", "cancel-owned-job-key"),
            ],
        )
        .await;
    assert_eq!(cancel.status, StatusCode::OK, "{:?}", cancel.body);
    assert_eq!(cancel.body["phase"], "cancelled");
    let replay = f
        .call(
            "POST",
            "/api/v2/jobs/owned/cancel",
            None,
            &[
                ("authorization", &auth),
                ("idempotency-key", "cancel-owned-job-key"),
            ],
        )
        .await;
    assert_eq!(cancel.body, replay.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    sqlx::query("INSERT INTO jobs(id,library_id,phase,created_at) VALUES ('legacy',?,'queued',0)")
        .bind(&f.library)
        .execute(&f.app.db)
        .await
        .unwrap();
    let hidden = f
        .call(
            "GET",
            "/api/v2/jobs/legacy",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let page = f
        .call(
            "GET",
            "/api/v2/jobs?limit=1",
            None,
            &[("authorization", &auth)],
        )
        .await;
    assert_eq!(page.body["items"].as_array().unwrap().len(), 1);
    assert!(page.body["next_cursor"].is_null());
    let busy = f
        .call(
            "POST",
            "/api/v2/jobs/owned/retry",
            None,
            &[
                ("authorization", &operator),
                ("idempotency-key", "retry-busy-job-key"),
            ],
        )
        .await;
    assert_eq!(busy.status, StatusCode::CONFLICT, "{:?}", busy.body);
}

#[tokio::test]
async fn job_retry_replays_original_ack_after_attempt_advances_and_rechecks_authorization() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let (device, token) = f.pair(&["default"], &["processing:request"]).await;
    let auth = bearer(&token);
    sqlx::query("INSERT INTO jobs(id,library_id,phase,attempt,created_at,requester_id,error) VALUES ('retry-owned',?,'failed',1,0,?,'old failure')")
        .bind(&f.library).bind(&device).execute(&f.app.db).await.unwrap();
    let path = "/api/v2/jobs/retry-owned/retry";
    let headers = [
        ("authorization", auth.as_str()),
        ("idempotency-key", "retry-owned-job-key"),
    ];
    let admitted = f.call("POST", path, None, &headers).await;
    assert_eq!(admitted.status, StatusCode::OK, "{:?}", admitted.body);
    assert_eq!(admitted.body["phase"], "queued");
    assert_eq!(admitted.body["attempt_generation"], "1");
    assert_eq!(admitted.body["revision"], "2");
    assert!(admitted.body["error_code"].is_null());
    // Simulate the worker's next attempt finishing before the client retries
    // its lost acknowledgement. A replay must not enqueue another attempt.
    sqlx::query(
        "UPDATE jobs SET phase='failed',attempt=2,error='next failure' WHERE id='retry-owned'",
    )
    .execute(&f.app.db)
    .await
    .unwrap();
    let replay = f.call("POST", path, None, &headers).await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body, admitted.body);
    assert_eq!(replay.headers["etag"], admitted.headers["etag"]);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let state: (String, i64, i64) =
        sqlx::query_as("SELECT phase,attempt,revision FROM jobs WHERE id='retry-owned'")
            .fetch_one(&f.app.db)
            .await
            .unwrap();
    assert_eq!(state, ("failed".into(), 2, 3));
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_records WHERE operation='retryJob' AND target='retry-owned'")
        .fetch_one(&f.app.db).await.unwrap();
    assert_eq!(receipts, 1);
    let (_, other) = f.pair(&["default"], &["processing:request"]).await;
    let hidden = f
        .call(
            "POST",
            path,
            None,
            &[
                ("authorization", &bearer(&other)),
                ("idempotency-key", "other-retry-job-key"),
            ],
        )
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    // Revocation defeats even an otherwise valid replay.
    sqlx::query("UPDATE devices SET revoked=1 WHERE id=?")
        .bind(&device)
        .execute(&f.app.db)
        .await
        .unwrap();
    let revoked = f.call("POST", path, None, &headers).await;
    assert_eq!(revoked.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn topcoat_item_details_use_scoped_catalog_and_production_viewing_state() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let other_root = f._dir.path().join("private");
    std::fs::create_dir(&other_root).unwrap();
    let other = db::add_library(&f.app.db, "Private", &other_root)
        .await
        .unwrap()
        .id;
    for (id, title, library) in [
        ("visible", "Film <script>alert(1)</script>", &f.library),
        ("hidden", "Secret title", &other),
        ("public-child", "Public season", &f.library),
    ] {
        sqlx::query("INSERT INTO items(id,title,kind) VALUES (?,?,'video')")
            .bind(id)
            .bind(title)
            .execute(&f.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO item_origins(item_id,title) VALUES (?,?)")
            .bind(id)
            .bind(title)
            .execute(&f.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO editions(id,item_id,label) VALUES (?,?,'Original')")
            .bind(id)
            .bind(id)
            .execute(&f.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO timelines(id,edition_id,duration_ms) VALUES (?,?,90000)")
            .bind(id)
            .bind(id)
            .execute(&f.app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES (?,?,?,?,'hash','fp',1)").bind(id).bind(id).bind(library).bind(format!("{id}.mp4")).execute(&f.app.db).await.unwrap();
        sqlx::query("INSERT INTO media_versions(id,timeline_id,label,origin,equivalence) VALUES (?,?,'Visible original','original','declared')").bind(id).bind(id).execute(&f.app.db).await.unwrap();
        sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES (?,1,?,'hash')").bind(id).bind(id).execute(&f.app.db).await.unwrap();
    }
    sqlx::query("INSERT INTO item_structure(item_id,media_type,parent_id,number) VALUES ('visible','series',NULL,NULL),('public-child','season','visible',1),('hidden','season','visible',2)")
        .execute(&f.app.db).await.unwrap();
    // Same title, private occurrence: the aggregate must not expose its version.
    sqlx::query("INSERT INTO media_files(id,edition_id,library_id,relative_path,revision,fingerprint,bytes) VALUES ('private-file','visible',?,'private.mp4','private-hash','fp',1)").bind(&other).execute(&f.app.db).await.unwrap();
    sqlx::query("INSERT INTO media_versions(id,timeline_id,label,origin,equivalence) VALUES ('private-version','visible','Secret version','original','declared')").execute(&f.app.db).await.unwrap();
    sqlx::query("INSERT INTO version_files(version_id,part,file_id,file_revision) VALUES ('private-version',1,'private-file','private-hash')").execute(&f.app.db).await.unwrap();
    let (device, token) = f
        .pair(&["default"], &["catalog:read", "playback:request"])
        .await;
    f.grant_library(&device, &["catalog:read", "playback:request"])
        .await;
    let auth = bearer(&token);
    let app = api::router_with(
        f.app.clone(),
        None,
        Some(playscale::presentation::router(f.app.clone())),
    );
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    for (path, expected) in [
        ("/item/visible", StatusCode::OK),
        ("/item/hidden", StatusCode::NOT_FOUND),
        ("/item/missing", StatusCode::NOT_FOUND),
    ] {
        let response = app
            .clone()
            .oneshot(f.request("GET", path, None, &[("authorization", &auth)]))
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
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
        assert!(
            !html.contains("Secret title") && !html.contains("Secret version"),
            "{html}"
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(!html.contains(&token));
        assert!(!html.contains(other_root.to_str().unwrap()));
        if expected == StatusCode::OK {
            assert!(html.contains("Film &lt;script&gt;"), "{html}");
            assert!(html.contains("Visible original"), "{html}");
            assert!(html.contains("Public season"), "{html}");
            // Viewing state and the Play action come from the production v2
            // viewing read and version availability, never invented.
            assert!(html.contains("Not started."), "{html}");
            assert!(!html.contains("Viewing history is unavailable."), "{html}");
            assert!(!html.contains("Playback is unavailable."), "{html}");
            assert!(html.contains("href=\"/play/visible\""), "{html}");
        }
    }
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM change_events")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(before, after, "rendering must not write domain state");
    // The player's audio choices describe only a version this principal can
    // read: the hidden version sorts first on the same timeline and has more
    // (and differently labelled) audio streams.
    sqlx::query("UPDATE media_files SET tracks_json=? WHERE id='private-file'")
        .bind(r#"[{"index":0,"kind":"video","codec":"h264","language":null},{"index":1,"kind":"audio","codec":"aac","language":"fra"},{"index":2,"kind":"audio","codec":"ac3","language":"fra"},{"index":3,"kind":"audio","codec":"dts","language":"deu"}]"#)
        .execute(&f.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE media_files SET tracks_json=? WHERE id='visible'")
        .bind(r#"[{"index":0,"kind":"video","codec":"h264","language":null},{"index":1,"kind":"audio","codec":"aac","language":"eng"}]"#)
        .execute(&f.app.db)
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(f.request("GET", "/play/visible", None, &[("authorization", &auth)]))
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
    assert!(html.contains("value=\"a0\""), "{html}");
    assert!(html.contains("eng, aac"), "{html}");
    assert!(html.contains("data-version=\"visible\""), "{html}");
    for leaked in [
        "value=\"a1\"",
        "fra",
        "deu",
        "private-version",
        "Secret version",
    ] {
        assert!(!html.contains(leaked), "{leaked} disclosed: {html}");
    }
    // Bounded aggregate reads fail explicitly instead of displaying an
    // incomplete timeline set as if it were complete.
    for n in 0..20 {
        sqlx::query("INSERT INTO timelines(id,edition_id) VALUES (?,'visible')")
            .bind(format!("extra-{n:02}"))
            .execute(&f.app.db)
            .await
            .unwrap();
    }
    let response = app
        .oneshot(f.request("GET", "/item/visible", None, &[("authorization", &auth)]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn topcoat_sources_use_authorized_reads_and_report_missing_scan_service() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let app = api::router_with(
        f.app.clone(),
        None,
        Some(playscale::presentation::router(f.app.clone())),
    );
    let (_, token) = f.pair(&["default"], &["catalog:read"]).await;
    for (credential, expected) in [
        (bearer(OPERATOR), StatusCode::OK),
        (bearer(&token), StatusCode::FORBIDDEN),
    ] {
        let response = app
            .clone()
            .oneshot(f.request("GET", "/sources", None, &[("authorization", &credential)]))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
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
        assert!(!html.contains("Development mock"));
        if expected == StatusCode::OK {
            assert!(html.contains("Movies"), "{html}");
            assert!(html.contains("Scanning is unavailable on this server."));
            assert!(html.contains("POST /api/v2/sources"));
            assert!(!html.contains("Scan now"));
            assert!(!html.contains("No recent scans"));
        } else {
            assert!(!html.contains(f._dir.path().to_str().unwrap()));
            assert!(!html.contains("POST /api/v2/sources"));
        }
    }
}

#[tokio::test]
async fn topcoat_hashed_assets_are_identity_independent_and_immutable() {
    let f = Fixture::new(AccessMode::Restricted).await;
    let app = api::router_with(
        f.app.clone(),
        None,
        Some(playscale::presentation::router(f.app.clone())),
    );
    let asset = &motion_ui::assets::STYLESHEET;
    for auth in [None, Some("Bearer invalid")] {
        let headers = auth.map(|v| vec![("authorization", v)]).unwrap_or_default();
        let response = app
            .clone()
            .oneshot(f.request("GET", &asset.url(), None, &headers))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert!(!response.headers().contains_key("set-cookie"));
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes.as_ref(), asset.bytes);
    }
    let response = app
        .oneshot(f.request("GET", "/ui/motion.invalid.css", None, &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
