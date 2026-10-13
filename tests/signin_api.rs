//! Isolated sign-in qualification through the real router, SQLite, and a local
//! SMTP receiver. No provider/mail secret or authentication proof is injected.
use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use playscale::{App, api, db};
use playscale_core::access::AccessMode;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex, Semaphore},
    task::JoinHandle,
};
use tower::ServiceExt;

const OPERATOR: &str = "isolated-signin-operator-0123456789abcdef";
const ORIGIN: &str = "http://127.0.0.1:8787";
const ADDRESS: &str = "person@example.invalid";

struct MailSink {
    port: u16,
    messages: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
impl MailSink {
    async fn new() -> Self {
        Self::bind(0).await
    }
    async fn bind(port: u16) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let captured = messages.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, peer)) = listener.accept().await {
                assert!(peer.ip().is_loopback());
                smtp(stream, &captured).await;
            }
        });
        Self {
            port,
            messages,
            task,
        }
    }
    async fn latest_code(&self) -> String {
        let messages = self.messages.lock().await;
        let message = messages.last().expect("local SMTP accepted a message");
        let code = message
            .split("Your Motion sign-in code is ")
            .nth(1)
            .expect("code is present in delivered message")
            .split('.')
            .next()
            .unwrap()
            .trim();
        assert!((6..=16).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit()));
        code.into()
    }
}
impl Drop for MailSink {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn smtp(stream: TcpStream, messages: &Mutex<Vec<String>>) {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    write
        .write_all(b"220 localhost isolated Motion test sink\r\n")
        .await
        .unwrap();
    let mut data = false;
    let mut message = String::new();
    loop {
        let mut line = String::new();
        if read.read_line(&mut line).await.unwrap() == 0 {
            break;
        }
        assert!(line.len() <= 16_384 && message.len() < 65_536);
        if data {
            if line == ".\r\n" {
                messages.lock().await.push(std::mem::take(&mut message));
                data = false;
                write.write_all(b"250 message captured\r\n").await.unwrap();
            } else {
                message.push_str(&line);
            }
        } else if line.starts_with("EHLO") || line.starts_with("HELO") {
            write
                .write_all(b"250-localhost\r\n250 8BITMIME\r\n")
                .await
                .unwrap();
        } else if line.starts_with("DATA") {
            data = true;
            write.write_all(b"354 send message\r\n").await.unwrap();
        } else if line.starts_with("QUIT") {
            write.write_all(b"221 closing\r\n").await.unwrap();
            break;
        } else {
            write.write_all(b"250 accepted\r\n").await.unwrap();
        }
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    app: App,
    library: String,
    mail: MailSink,
}
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}
impl Reply {
    fn cookie(&self, name: &str) -> Option<String> {
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|header| header.to_str().ok())
            .filter_map(|header| header.split(';').next())
            .find(|cookie| cookie.starts_with(&format!("{name}=")))
            .map(str::to_owned)
    }
}
fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}
fn tag(revision: &Value) -> String {
    format!("\"r-{}\"", revision.as_str().unwrap())
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("media");
        std::fs::create_dir(&root).unwrap();
        let database = db::connect(&directory.path().join("db.sqlite"))
            .await
            .unwrap();
        let library = db::add_library(&database, "Fixture", &root)
            .await
            .unwrap()
            .id;
        let state = directory.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let app = App {
            health: Arc::new(playscale::operations::Health::new(false)),
            db: database,
            admin_token: Arc::new(OPERATOR.into()),
            origin: Arc::new(ORIGIN.into()),
            authority: Arc::new("127.0.0.1:8787".into()),
            ffprobe: Arc::new("ffprobe".into()),
            jobs: Arc::new(Mutex::new(())),
            streams: Arc::new(Semaphore::new(2)),
            event_streams: Arc::new(Semaphore::new(4)),
            storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
            processing: Arc::new(playscale::processing::Runtime::new(
                directory.path().join("cache"),
                Default::default(),
            )),
            access: Arc::new(playscale::v2::Runtime::new(
                AccessMode::Restricted,
                [73; 32],
            )),
        };
        Self {
            _directory: directory,
            app,
            library,
            mail: MailSink::new().await,
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
            .header("host", "127.0.0.1:8787")
            .header("origin", ORIGIN);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = api::router(self.app.clone(), None)
            .oneshot(
                request
                    .body(
                        body.map(|body| Body::from(body.to_string()))
                            .unwrap_or_default(),
                    )
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
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        Reply {
            status,
            headers,
            body,
        }
    }
    async fn operator(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        revision: Option<&Value>,
    ) -> Reply {
        let auth = bearer(OPERATOR);
        let key = format!("signin-fixture-{}", uuid::Uuid::new_v4());
        let revision = revision.map(tag);
        let mut headers = vec![
            ("authorization", auth.as_str()),
            ("idempotency-key", key.as_str()),
        ];
        if let Some(revision) = &revision {
            headers.push(("if-match", revision));
        }
        self.call(method, path, body, &headers).await
    }
    async fn user(&self, permissions: &[&str]) -> Value {
        let response = self.operator("POST", "/api/v2/users", Some(json!({
            "name":"Isolated person", "disabled":false, "profile_ids":["default"], "permissions":permissions,
            "policy":{"library_ids":[self.library],"allow_unrated":true,"allowed_ratings":[],"deny_rated":false,"blocked_labels":[]}
        })), None).await;
        assert_eq!(response.status, StatusCode::CREATED, "{}", response.body);
        response.body
    }
    async fn paired(&self, user: &Value) -> (String, String) {
        let request = self
            .call(
                "POST",
                "/api/v2/auth/pairings",
                Some(json!({"device_name":"Fixture device","client_name":"signin-test"})),
                &[],
            )
            .await;
        assert_eq!(request.status, StatusCode::CREATED);
        let id = request.body["id"].as_str().unwrap();
        let approval = self.operator("POST", &format!("/api/v2/auth/pairings/{id}/approve"), Some(json!({
            "user_code":request.body["user_code"], "user_id":user["id"], "user_revision":user["revision"],
            "profile_ids":user["profile_ids"], "permissions":user["permissions"], "policy":user["policy"]
        })), None).await;
        assert_eq!(approval.status, StatusCode::OK, "{}", approval.body);
        let claim = self
            .call(
                "POST",
                &format!("/api/v2/auth/pairings/{id}/claim"),
                Some(json!({"device_code":request.body["device_code"]})),
                &[],
            )
            .await;
        assert_eq!(claim.status, StatusCode::OK, "{}", claim.body);
        (
            claim.body["device_id"].as_str().unwrap().into(),
            claim.body["access_token"].as_str().unwrap().into(),
        )
    }
    async fn proof(&self, actor: &str, action: &str, target: &str) -> Value {
        let response = self
            .call(
                "POST",
                "/api/v2/auth/proofs",
                Some(json!({"action":action,"target":target})),
                &[("authorization", &bearer(actor))],
            )
            .await;
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        response.body
    }
    async fn approve(&self, proof: &Value, token: &str) -> Reply {
        self.call(
            "POST",
            &format!(
                "/api/v2/auth/proofs/{}/approve",
                proof["id"].as_str().unwrap()
            ),
            Some(json!({"code":proof["code"]})),
            &[("authorization", &bearer(token))],
        )
        .await
    }

    /// Configure a disabled method, explicitly test local delivery, then enable
    /// through administrator APIs before any enrollment or login request.
    async fn configure_mail(&self) {
        let origin = self
            .operator(
                "PUT",
                "/api/v2/admin/auth/settings",
                Some(json!({"public_origin":ORIGIN})),
                Some(&json!("1")),
            )
            .await;
        assert_eq!(origin.status, StatusCode::OK, "{}", origin.body);
        let mut config = json!({"enabled":false,"host":"127.0.0.1","port":self.mail.port,
            "username":"","from":"motion@example.invalid","loopback_test":true,"password":{"action":"retain"}});
        let draft = self
            .operator(
                "PUT",
                "/api/v2/admin/auth/email",
                Some(config.clone()),
                Some(&json!("1")),
            )
            .await;
        assert_eq!(draft.status, StatusCode::OK, "{}", draft.body);
        let tested = self
            .operator(
                "POST",
                "/api/v2/admin/auth/email/test",
                Some(json!({"address":"sink@example.invalid"})),
                None,
            )
            .await;
        assert_eq!(tested.status, StatusCode::OK, "{}", tested.body);
        config["enabled"] = json!(true);
        let enabled = self
            .operator(
                "PUT",
                "/api/v2/admin/auth/email",
                Some(config),
                Some(&draft.body["revision"]),
            )
            .await;
        assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.body);
        assert_eq!(enabled.body["enabled"], true);
    }
    async fn email_start(&self, address: &str, purpose: &str, headers: &[(&str, &str)]) -> Reply {
        self.call(
            "POST",
            "/api/v2/auth/email/start",
            Some(json!({"address":address,"purpose":purpose})),
            headers,
        )
        .await
    }
    async fn email_finish(&self, id: &str, cookie: &str, code: &str) -> Reply {
        self.call(
            "POST",
            &format!("/api/v2/auth/email/{id}/finish"),
            Some(json!({"code":code})),
            &[("cookie", cookie)],
        )
        .await
    }
    async fn delivered_code(&self, previous: usize) -> String {
        // email_start also kicks the worker; the writer reservation fences this
        // explicit drain against it without sending a different challenge.
        playscale::v2::signin::deliver_mail_once(&self.app)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while self.mail.messages.lock().await.len() == previous {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("local SMTP delivery completed");
        self.mail.latest_code().await
    }
    async fn enrolled(&self) -> (Value, String, String) {
        self.enrolled_with(&["catalog:read", "playback:request"])
            .await
    }
    async fn enrolled_with(&self, permissions: &[&str]) -> (Value, String, String) {
        self.configure_mail().await;
        let user = self.user(permissions).await;
        let (device, token) = self.paired(&user).await;
        let proof = self.proof(&token, "identity.link", "email").await;
        let approved = self.approve(&proof, OPERATOR).await;
        assert!(approved.status.is_success(), "{}", approved.body);
        let previous = self.mail.messages.lock().await.len();
        let started = self
            .email_start(
                ADDRESS,
                "enroll",
                &[
                    ("authorization", &bearer(&token)),
                    ("x-motion-action-proof", proof["id"].as_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(started.status, StatusCode::OK, "{}", started.body);
        let cookie = started.cookie("motion_signin").unwrap();
        let code = self.delivered_code(previous).await;
        let finished = self
            .email_finish(
                started.body["transaction_id"].as_str().unwrap(),
                &cookie,
                &code,
            )
            .await;
        assert_eq!(finished.status, StatusCode::OK, "{}", finished.body);
        assert_eq!(finished.body["status"], "linked");
        assert!(finished.cookie("motion_session").is_none());
        (user, device, token)
    }
    async fn challenge(&self) -> (String, String, String) {
        let previous = self.mail.messages.lock().await.len();
        let started = self.email_start(ADDRESS, "login", &[]).await;
        assert_eq!(started.status, StatusCode::OK, "{}", started.body);
        let code = self.delivered_code(previous).await;
        (
            started.body["transaction_id"].as_str().unwrap().into(),
            started.cookie("motion_signin").unwrap(),
            code,
        )
    }
    async fn transaction_state(&self, id: &str) -> Value {
        let state: String =
            sqlx::query_scalar("SELECT state_json FROM signin_transactions WHERE id=?")
                .bind(id)
                .fetch_one(&self.app.db)
                .await
                .unwrap();
        serde_json::from_str(&state).unwrap()
    }
    async fn families(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM signin_families")
            .fetch_one(&self.app.db)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn email_requests_do_not_enumerate_addresses_or_create_unknown_identities() {
    let f = Fixture::new().await;
    f.enrolled().await;
    let unknown = f.email_start("unknown@example.invalid", "login", &[]).await;
    let eligible = f.email_start(ADDRESS, "login", &[]).await;
    sqlx::query("UPDATE signin_identities SET enabled=0,revision=revision+1 WHERE address=?")
        .bind(ADDRESS)
        .execute(&f.app.db)
        .await
        .unwrap();
    let disabled = f.email_start(ADDRESS, "login", &[]).await;
    for response in [&unknown, &eligible, &disabled] {
        assert_eq!(response.status, StatusCode::OK, "{}", response.body);
        assert_eq!(response.body["message"], unknown.body["message"]);
        let keys: Vec<_> = response
            .body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["message", "transaction_id"]);
        assert!(response.cookie("motion_session").is_none());
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM signin_identities WHERE address='unknown@example.invalid'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert_eq!(f.families().await, 0);
}

#[tokio::test]
async fn email_code_is_browser_bound_post_only_and_issues_one_owned_family() {
    let f = Fixture::new().await;
    let (user, paired, _) = f.enrolled().await;
    let (id, cookie, code) = f.challenge().await;
    let cross_browser = f
        .email_finish(&id, &format!("motion_signin={}", "a".repeat(64)), &code)
        .await;
    assert!(!cross_browser.status.is_success());
    assert_eq!(f.transaction_state(&id).await["attempts"], 0);
    let get = f
        .call(
            "GET",
            &format!("/api/v2/auth/email/{id}/finish"),
            None,
            &[("cookie", &cookie)],
        )
        .await;
    assert_eq!(get.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(f.transaction_state(&id).await["phase"], "pending");
    assert_eq!(f.families().await, 0);
    let first = f.email_finish(&id, &cookie, &code).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["status"], "signed_in");
    let session = first.cookie("motion_session").unwrap();
    let flags = first.headers["set-cookie"].to_str().unwrap();
    assert!(flags.contains("HttpOnly") && flags.contains("SameSite=Strict"));
    assert!(!first.body.to_string().contains("mss_"));
    let me = f
        .call("GET", "/api/v2/me", None, &[("cookie", &session)])
        .await;
    assert_eq!(me.status, StatusCode::OK);
    assert_ne!(me.body["id"], paired);
    let (owner, credentials): (String, i64) = sqlx::query_as("SELECT d.user_id,(SELECT count(*) FROM credentials c WHERE c.device_id=d.id) FROM devices d WHERE d.id=?")
        .bind(me.body["id"].as_str().unwrap()).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(owner, user["id"].as_str().unwrap());
    assert_eq!(credentials, 2, "one parent and one cookie session");
    let replay = f.email_finish(&id, &cookie, &code).await;
    assert!(!replay.status.is_success());
    assert_eq!(f.families().await, 1);
    let pending_payloads: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM signin_outbox WHERE transaction_id=? AND payload IS NOT NULL",
    )
    .bind(&id)
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(pending_payloads, 0);
}

#[tokio::test]
async fn wrong_email_codes_commit_the_attempt_budget_and_cannot_recover() {
    let f = Fixture::new().await;
    f.enrolled().await;
    let (id, cookie, code) = f.challenge().await;
    let wrong = if code == "00000000" {
        "11111111"
    } else {
        "00000000"
    };
    for attempts in 1..=playscale_core::signin::CODE_ATTEMPTS {
        let refused = f.email_finish(&id, &cookie, wrong).await;
        assert_eq!(refused.body["code"], "invalid_code");
        assert_eq!(f.transaction_state(&id).await["attempts"], attempts);
    }
    assert_eq!(f.transaction_state(&id).await["phase"], "failed");
    let state_before = f.transaction_state(&id).await;
    assert!(
        !f.email_finish(&id, &cookie, &code)
            .await
            .status
            .is_success()
    );
    assert_eq!(f.transaction_state(&id).await, state_before);
    assert_eq!(f.families().await, 0);
}

#[tokio::test]
async fn pending_email_challenges_reject_user_identity_method_and_epoch_changes() {
    for mutation in [
        "UPDATE access_users SET disabled=1,revision=revision+1",
        "UPDATE signin_identities SET enabled=0,revision=revision+1",
        "UPDATE signin_identities SET revision=revision+1",
        "UPDATE signin_settings SET email_revision=email_revision+1",
        "UPDATE signin_settings SET email_json=json_set(email_json,'$.enabled',json('false'))",
        "UPDATE server_identity SET restore_epoch='restored-signin-test'",
    ] {
        let f = Fixture::new().await;
        f.enrolled().await;
        let (id, cookie, code) = f.challenge().await;
        sqlx::query(mutation).execute(&f.app.db).await.unwrap();
        let refused = f.email_finish(&id, &cookie, &code).await;
        assert!(
            !refused.status.is_success(),
            "mutation accepted: {mutation}"
        );
        assert_ne!(
            refused.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "mutation became an internal error: {mutation}"
        );
        assert!(refused.cookie("motion_session").is_none());
        assert_eq!(f.families().await, 0);
    }
}

#[tokio::test]
async fn fresh_email_login_intersects_current_user_ceiling_and_uses_distinct_devices() {
    let f = Fixture::new().await;
    let (user, _, _) = f.enrolled().await;
    let mut edit = user.clone();
    edit.as_object_mut().unwrap().remove("id");
    edit.as_object_mut().unwrap().remove("revision");
    edit["permissions"] = json!(["catalog:read"]);
    let changed = f
        .operator(
            "PUT",
            &format!("/api/v2/users/{}", user["id"].as_str().unwrap()),
            Some(edit),
            Some(&user["revision"]),
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    let mut devices = Vec::new();
    for _ in 0..2 {
        let (id, cookie, code) = f.challenge().await;
        let session = f.email_finish(&id, &cookie, &code).await;
        assert_eq!(session.status, StatusCode::OK, "{}", session.body);
        let me = f
            .call(
                "GET",
                "/api/v2/me",
                None,
                &[("cookie", &session.cookie("motion_session").unwrap())],
            )
            .await;
        assert_eq!(me.body["permissions"], json!(["catalog:read"]));
        devices.push(me.body["id"].clone());
    }
    assert_ne!(devices[0], devices[1]);
    assert_eq!(f.families().await, 2);
}

#[tokio::test]
async fn enrollment_and_reenrollment_never_widen_a_restricted_device() {
    let f = Fixture::new().await;
    f.configure_mail().await;
    let owner = f.user(&["system:admin"]).await;
    let mut restricted = owner.clone();
    restricted["permissions"] = json!(["catalog:read"]);
    let (_, token) = f.paired(&restricted).await;
    for reenroll in [false, true] {
        if reenroll {
            let identities = f
                .operator("GET", "/api/v2/admin/auth/identities", None, None)
                .await;
            let identity = &identities.body["items"][0];
            let removed = f
                .operator(
                    "DELETE",
                    &format!(
                        "/api/v2/auth/identities/{}",
                        identity["id"].as_str().unwrap()
                    ),
                    None,
                    Some(&identity["revision"]),
                )
                .await;
            assert_eq!(removed.status, StatusCode::NO_CONTENT, "{}", removed.body);
        }
        let proof = f.proof(&token, "identity.link", "email").await;
        assert!(f.approve(&proof, OPERATOR).await.status.is_success());
        let previous = f.mail.messages.lock().await.len();
        let start = f
            .email_start(
                ADDRESS,
                "enroll",
                &[
                    ("authorization", &bearer(&token)),
                    ("x-motion-action-proof", proof["id"].as_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.body);
        let id = start.body["transaction_id"].as_str().unwrap();
        let state = f.transaction_state(id).await;
        assert!(state["expires_at"].as_i64().unwrap() <= proof["expires_at"].as_i64().unwrap());
        let code = f.delivered_code(previous).await;
        let finish = f
            .email_finish(id, &start.cookie("motion_signin").unwrap(), &code)
            .await;
        assert_eq!(finish.status, StatusCode::OK, "{}", finish.body);
        let (id, cookie, code) = f.challenge().await;
        let login = f.email_finish(&id, &cookie, &code).await;
        assert_eq!(login.status, StatusCode::OK, "{}", login.body);
        let me = f
            .call(
                "GET",
                "/api/v2/me",
                None,
                &[("cookie", &login.cookie("motion_session").unwrap())],
            )
            .await;
        assert_eq!(me.body["permissions"], json!(["catalog:read"]));
    }
}

#[tokio::test]
async fn proof_is_action_bound_single_use_and_cannot_be_self_approved() {
    let f = Fixture::new().await;
    let owner = f.user(&["system:admin"]).await;
    let (_, actor) = f.paired(&owner).await;
    let (_, other) = f.paired(&owner).await;
    let proof = f.proof(&actor, "user.admin", "new").await;
    assert!(!f.approve(&proof, &actor).await.status.is_success());
    assert!(f.approve(&proof, &other).await.status.is_success());
    let input = json!({"name":"Created through proof","disabled":false,"profile_ids":["default"],"permissions":["catalog:read"],"policy":owner["policy"]});
    let create = |key: &str| {
        vec![
            ("authorization", bearer(&actor)),
            (
                "x-motion-action-proof",
                proof["id"].as_str().unwrap().to_owned(),
            ),
            ("idempotency-key", key.to_owned()),
        ]
    };
    let h = create("proof-creation-first");
    let headers: Vec<_> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let first = f
        .call("POST", "/api/v2/users", Some(input.clone()), &headers)
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let h = create("proof-creation-second");
    let headers: Vec<_> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let repeated = f.call("POST", "/api/v2/users", Some(input), &headers).await;
    assert_eq!(repeated.body["code"], "recent_auth_required");
}

#[tokio::test]
async fn administrator_elevation_requires_fresh_action_proof() {
    let f = Fixture::new().await;
    let owner = f.user(&["system:admin"]).await;
    let (_, admin) = f.paired(&owner).await;
    let mut low = owner.clone();
    low["permissions"] = json!(["catalog:read"]);
    let (device, _) = f.paired(&low).await;
    let policy = f
        .operator(
            "GET",
            &format!("/api/v2/devices/{device}/policy"),
            None,
            None,
        )
        .await;
    let mut change = policy.body.clone();
    change.as_object_mut().unwrap().remove("revision");
    change["permissions"] = json!(["system:admin"]);
    let denied = f
        .call(
            "PUT",
            &format!("/api/v2/devices/{device}/policy"),
            Some(change.clone()),
            &[
                ("authorization", &bearer(&admin)),
                ("if-match", &tag(&policy.body["revision"])),
            ],
        )
        .await;
    assert_eq!(denied.body["code"], "recent_auth_required");
    let proof = f.proof(&admin, "device.policy", &device).await;
    assert!(f.approve(&proof, OPERATOR).await.status.is_success());
    let accepted = f
        .call(
            "PUT",
            &format!("/api/v2/devices/{device}/policy"),
            Some(change),
            &[
                ("authorization", &bearer(&admin)),
                ("if-match", &tag(&policy.body["revision"])),
                ("x-motion-action-proof", proof["id"].as_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
}

#[tokio::test]
async fn restore_holds_new_signins_and_never_revives_old_families() {
    let f = Fixture::new().await;
    f.enrolled().await;
    let (id, cookie, code) = f.challenge().await;
    let signed = f.email_finish(&id, &cookie, &code).await;
    let session = signed.cookie("motion_session").unwrap();
    sqlx::query("UPDATE server_identity SET restore_epoch='isolated-restored-epoch'")
        .execute(&f.app.db)
        .await
        .unwrap();
    assert_eq!(
        f.call("GET", "/api/v2/me", None, &[("cookie", &session)])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        !f.email_start(ADDRESS, "login", &[])
            .await
            .status
            .is_success()
    );
    let review = f
        .operator("POST", "/api/v2/admin/auth/restore-review", None, None)
        .await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    assert_eq!(
        f.call("GET", "/api/v2/me", None, &[("cookie", &session)])
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        f.email_start(ADDRESS, "login", &[])
            .await
            .status
            .is_success()
    );
}

#[tokio::test]
async fn enrollment_completion_rechecks_initiating_device_and_evidence_deadline() {
    for expire in [false, true] {
        let f = Fixture::new().await;
        let (_, device, token) = f.enrolled().await;
        let proof = f.proof(&token, "identity.link", "email").await;
        assert!(f.approve(&proof, OPERATOR).await.status.is_success());
        if expire {
            sqlx::query("UPDATE signin_proofs SET expires_at=? WHERE id=?")
                .bind(playscale::now() + 3)
                .bind(proof["id"].as_str().unwrap())
                .execute(&f.app.db)
                .await
                .unwrap();
        }
        let previous = f.mail.messages.lock().await.len();
        let start = f
            .email_start(
                "second@example.invalid",
                "enroll",
                &[
                    ("authorization", &bearer(&token)),
                    ("x-motion-action-proof", proof["id"].as_str().unwrap()),
                ],
            )
            .await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.body);
        let code = f.delivered_code(previous).await;
        if expire {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        } else {
            sqlx::query("UPDATE devices SET revision=revision+1 WHERE id=?")
                .bind(&device)
                .execute(&f.app.db)
                .await
                .unwrap();
        }
        let finish = f
            .email_finish(
                start.body["transaction_id"].as_str().unwrap(),
                &start.cookie("motion_signin").unwrap(),
                &code,
            )
            .await;
        assert!(!finish.status.is_success(), "{}", finish.body);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM signin_identities WHERE address='second@example.invalid'"
            )
            .fetch_one(&f.app.db)
            .await
            .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn network_limits_cannot_be_reset_with_new_cookies_or_forwarded_headers() {
    let f = Fixture::new().await;
    f.configure_mail().await;
    for attempt in 0..21 {
        let reply = f
            .email_start(
                &format!("unknown-{attempt}@example.invalid"),
                "login",
                &[("x-forwarded-for", &format!("192.0.2.{}", attempt + 1))],
            )
            .await;
        assert_eq!(
            reply.status,
            if attempt < 20 {
                StatusCode::OK
            } else {
                StatusCode::TOO_MANY_REQUESTS
            },
            "attempt {attempt}: {}",
            reply.body
        );
    }
    assert_eq!(f.families().await, 0);
}

#[tokio::test]
async fn provider_secrets_are_encrypted_and_never_returned_or_readable_by_ordinary_users() {
    let f = Fixture::new().await;
    let user = f.user(&["catalog:read"]).await;
    let (_, token) = f.paired(&user).await;
    let secret = "fixture-provider-secret-never-return-this";
    let mut input = json!({"name":"Isolated provider", "issuer":"https://provider.example.invalid", "client_id":"fixture", "enabled":false, "client_secret":{"action":"replace","value":secret}});
    let created = f
        .operator(
            "POST",
            "/api/v2/admin/auth/providers",
            Some(input.clone()),
            None,
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["has_secret"], true);
    assert!(!created.body.to_string().contains(secret));
    let id = created.body["id"].as_str().unwrap();
    let stored: String = sqlx::query_scalar("SELECT secret FROM signin_providers WHERE id=?")
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert!(!stored.contains(secret));
    let denied = f
        .call(
            "GET",
            "/api/v2/admin/auth/providers",
            None,
            &[("authorization", &bearer(&token))],
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let read = f
        .operator("GET", "/api/v2/admin/auth/providers", None, None)
        .await;
    assert!(!read.body.to_string().contains(secret));
    input["client_secret"] = json!({"action":"retain"});
    let retained = f
        .operator(
            "PUT",
            &format!("/api/v2/admin/auth/providers/{id}"),
            Some(input.clone()),
            Some(&created.body["revision"]),
        )
        .await;
    assert_eq!(retained.status, StatusCode::OK, "{}", retained.body);
    let after: String = sqlx::query_scalar("SELECT secret FROM signin_providers WHERE id=?")
        .bind(id)
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(stored, after);
    input["client_secret"] = json!({"action":"clear"});
    let cleared = f
        .operator(
            "PUT",
            &format!("/api/v2/admin/auth/providers/{id}"),
            Some(input),
            Some(&retained.body["revision"]),
        )
        .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
    assert_eq!(cleared.body["has_secret"], false);
    let audit = f
        .operator("GET", "/api/v2/admin/auth/audit", None, None)
        .await;
    assert!(!audit.body.to_string().contains(secret));
}

#[tokio::test]
async fn mail_outage_retries_the_same_challenge_and_erases_delivered_payload() {
    let mut f = Fixture::new().await;
    let (_, _, paired) = f.enrolled().await;
    let port = f.mail.port;
    f.mail.task.abort();
    while !f.mail.task.is_finished() {
        tokio::task::yield_now().await;
    }
    let started = f.email_start(ADDRESS, "login", &[]).await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let id = started.body["transaction_id"].as_str().unwrap();
    let cookie = started.cookie("motion_signin").unwrap();
    playscale::v2::signin::deliver_mail_once(&f.app)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let phase: String =
                sqlx::query_scalar("SELECT phase FROM signin_outbox WHERE transaction_id=?")
                    .bind(id)
                    .fetch_one(&f.app.db)
                    .await
                    .unwrap();
            if phase == "pending" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let before: (String, String) = sqlx::query_as("SELECT t.code_hash,o.payload FROM signin_transactions t JOIN signin_outbox o ON o.transaction_id=t.id WHERE t.id=?").bind(id).fetch_one(&f.app.db).await.unwrap();
    assert!(!before.1.contains(ADDRESS));
    assert_eq!(
        f.families().await,
        0,
        "delivery acceptance never authenticates"
    );
    let paired_read = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(&paired))],
        )
        .await;
    assert_eq!(
        paired_read.status,
        StatusCode::OK,
        "pairing survives SMTP outage"
    );
    f.mail = MailSink::bind(port).await;
    sqlx::query("UPDATE signin_outbox SET next_attempt=0 WHERE transaction_id=?")
        .bind(id)
        .execute(&f.app.db)
        .await
        .unwrap();
    let code = f.delivered_code(0).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let phase: String =
                sqlx::query_scalar("SELECT phase FROM signin_outbox WHERE transaction_id=?")
                    .bind(id)
                    .fetch_one(&f.app.db)
                    .await
                    .unwrap();
            if phase == "sent" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let after: (String, Option<String>, String, i64) = sqlx::query_as("SELECT t.code_hash,o.payload,o.phase,o.attempts FROM signin_transactions t JOIN signin_outbox o ON o.transaction_id=t.id WHERE t.id=?").bind(id).fetch_one(&f.app.db).await.unwrap();
    assert_eq!(
        before.0, after.0,
        "retry must retain the original challenge"
    );
    assert_eq!(after.1, None);
    assert_eq!(after.2, "sent");
    assert!(after.3 >= 2);
    assert_eq!(
        f.email_finish(id, &cookie, &code).await.status,
        StatusCode::OK
    );
    assert_eq!(f.families().await, 1);
}

#[tokio::test]
async fn disabling_email_ends_an_admitted_event_stream_and_preserves_pairing() {
    let f = Fixture::new().await;
    let (_, _, paired) = f.enrolled_with(&["catalog:read", "events:read"]).await;
    let (id, binding, code) = f.challenge().await;
    let signed = f.email_finish(&id, &binding, &code).await;
    let cookie = signed.cookie("motion_session").unwrap();
    let response = api::router(f.app.clone(), None)
        .oneshot(
            Request::builder()
                .uri("/api/v2/events")
                .header("host", "127.0.0.1:8787")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body();
    let initial = tokio::time::timeout(std::time::Duration::from_secs(5), stream.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("initial_snapshot"));
    let configuration = f
        .operator("GET", "/api/v2/admin/auth/email", None, None)
        .await;
    let disabled = f.operator("PUT", "/api/v2/admin/auth/email", Some(json!({"enabled":false,"host":"127.0.0.1","port":f.mail.port,"username":"","from":"motion@example.invalid","loopback_test":true,"password":{"action":"retain"}})), Some(&configuration.body["revision"])).await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.body);
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), stream.frame())
        .await
        .expect("admitted SSE must close after method revocation");
    assert!(
        ended.is_none(),
        "revoked family must not receive another hint"
    );
    let rejected = f
        .call("GET", "/api/v2/me", None, &[("cookie", &cookie)])
        .await;
    assert_eq!(rejected.status, StatusCode::UNAUTHORIZED);
    let independent = f
        .call(
            "GET",
            "/api/v2/me",
            None,
            &[("authorization", &bearer(&paired))],
        )
        .await;
    assert_eq!(independent.status, StatusCode::OK);
    let revoked: i64 = sqlx::query_scalar("SELECT count(*) FROM signin_families WHERE revoked=1")
        .fetch_one(&f.app.db)
        .await
        .unwrap();
    assert_eq!(revoked, 1);
}

#[tokio::test]
async fn fresh_login_ignores_existing_session_but_identity_changes_do_not() {
    for state in ["valid", "expired", "revoked"] {
        let f = Fixture::new().await;
        f.enrolled().await;
        let (id, binding, code) = f.challenge().await;
        let first = f.email_finish(&id, &binding, &code).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        let session = first.cookie("motion_session").unwrap();
        match state {
            "expired" => {
                sqlx::query("UPDATE signin_families SET expires_at=0")
                    .execute(&f.app.db)
                    .await
                    .unwrap();
            }
            "revoked" => {
                sqlx::query("UPDATE signin_families SET revoked=1")
                    .execute(&f.app.db)
                    .await
                    .unwrap();
            }
            _ => {}
        }
        // No CSRF header: public login relies on exact Origin and its own
        // browser-bound proof, regardless of an old session's validity.
        let previous = f.mail.messages.lock().await.len();
        let start = f
            .email_start(ADDRESS, "login", &[("cookie", &session)])
            .await;
        assert_eq!(start.status, StatusCode::OK, "{state}: {}", start.body);
        let transaction = start.body["transaction_id"].as_str().unwrap();
        let actor: Option<String> =
            sqlx::query_scalar("SELECT actor_id FROM signin_transactions WHERE id=?")
                .bind(transaction)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        assert!(
            actor.is_none(),
            "ordinary login never adopts a session actor"
        );
        let code = f.delivered_code(previous).await;
        let combined = format!("{}; {}", session, start.cookie("motion_signin").unwrap());
        let second = f.email_finish(transaction, &combined, &code).await;
        assert_eq!(second.status, StatusCode::OK, "{state}: {}", second.body);
        assert_ne!(second.cookie("motion_session").unwrap(), session);
        assert_eq!(f.families().await, 2);
        for purpose in ["enroll", "reauth"] {
            let denied = f
                .email_start(ADDRESS, purpose, &[("cookie", &session)])
                .await;
            assert!(
                matches!(
                    denied.status,
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                ),
                "{state}/{purpose}: {}",
                denied.body
            );
        }
        for purpose in ["link", "test", "reauth"] {
            let denied = f
                .call(
                    "POST",
                    "/api/v2/auth/oidc/missing/start",
                    Some(json!({"purpose":purpose,"return_to":"/"})),
                    &[("cookie", &session)],
                )
                .await;
            assert!(
                matches!(
                    denied.status,
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                ),
                "{state}/{purpose}: {}",
                denied.body
            );
        }
    }
}

/// Real HTTP token exchange is held outside SQLite. The second router request
/// must observe the durable reservation while the first is still in flight.
async fn oidc_reservation_case(cancel_exchange: bool) {
    let mut child = tokio::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/qualification/auth-signin/callback-provider.mjs"
        ))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut port = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        output.read_line(&mut port),
    )
    .await
    .unwrap()
    .unwrap();
    let origin = format!("http://127.0.0.1:{}", port.trim().parse::<u16>().unwrap());
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let f = Arc::new(Fixture::new().await);
    let configured = f
        .operator(
            "PUT",
            "/api/v2/admin/auth/settings",
            Some(json!({"public_origin":ORIGIN})),
            Some(&json!("1")),
        )
        .await;
    assert_eq!(configured.status, StatusCode::OK, "{}", configured.body);
    let provider = f.operator("POST", "/api/v2/admin/auth/providers", Some(json!({
        "name":"Callback fault fixture", "issuer":origin, "client_id":"motion-test", "enabled":false,
        "client_secret":{"action":"replace","value":"fixture-only"},
        "destinations":[{"origin":origin,"address":"127.0.0.1"},{"origin":ORIGIN,"address":"127.0.0.1"}]
    })), None).await;
    assert_eq!(provider.status, StatusCode::CREATED, "{}", provider.body);
    let provider_id = provider.body["id"].as_str().unwrap();
    let started = f
        .operator(
            "POST",
            &format!("/api/v2/auth/oidc/{provider_id}/start"),
            Some(json!({"purpose":"test"})),
            None,
        )
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let id = started.body["transaction_id"].as_str().unwrap();
    let cookie = started.cookie("motion_signin").unwrap();
    let authorized = client
        .get(started.body["authorization_url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::FOUND);
    let callback = url::Url::parse(authorized.headers()["location"].to_str().unwrap()).unwrap();
    let path = format!("{}?{}", callback.path(), callback.query().unwrap());
    let wrong_browser = f.call("GET", &path, None, &[]).await;
    assert_eq!(wrong_browser.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong_browser.body["code"], "signin_browser_mismatch");
    assert_eq!(f.transaction_state(id).await["phase"], "pending");
    let task = {
        let f = f.clone();
        let path = path.clone();
        let cookie = cookie.clone();
        tokio::spawn(async move { f.call("GET", &path, None, &[("cookie", &cookie)]).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let observed: Value = client
                .get(format!("{origin}/fixture/status"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if observed["exchanges"] == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first callback reached the held token exchange");
    // A separate pool reader sees the committed state while network I/O is held.
    assert_eq!(f.transaction_state(id).await["phase"], "exchanging");
    assert_eq!(f.families().await, 0);
    let duplicate = f.call("GET", &path, None, &[("cookie", &cookie)]).await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT, "{}", duplicate.body);
    assert_eq!(duplicate.body["code"], "signin_already_used");
    if cancel_exchange {
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        // Reopen the actual database and reconstruct runtime as after restart.
        let mut f = Arc::try_unwrap(f)
            .ok()
            .expect("cancelled request released its app");
        f.app.db.close().await;
        f.app.db = db::connect(&f._directory.path().join("db.sqlite"))
            .await
            .unwrap();
        f.app.access = Arc::new(playscale::v2::Runtime::new(
            AccessMode::Restricted,
            [73; 32],
        ));
        assert_eq!(f.transaction_state(id).await["phase"], "exchanging");
        let replay = f.call("GET", &path, None, &[("cookie", &cookie)]).await;
        assert_eq!(replay.status, StatusCode::CONFLICT, "{}", replay.body);
        assert!(replay.cookie("motion_session").is_none());
        assert_eq!(f.families().await, 0);
        let tested: Option<i64> =
            sqlx::query_scalar("SELECT login_test_at FROM signin_providers WHERE id=?")
                .bind(provider_id)
                .fetch_one(&f.app.db)
                .await
                .unwrap();
        assert_eq!(
            tested, None,
            "cancelled callback cannot publish provider test success"
        );
    } else {
        client
            .post(format!("{origin}/fixture/release"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let completed = task.await.unwrap();
        assert_eq!(
            completed.status,
            StatusCode::SEE_OTHER,
            "{}",
            completed.body
        );
        assert!(
            completed.cookie("motion_session").is_none(),
            "provider tests never issue a login family"
        );
        assert_eq!(f.transaction_state(id).await["phase"], "consumed");
        assert_eq!(f.families().await, 0);
        let effects: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM signin_audit WHERE action='oidc.login' AND outcome='tested'",
        )
        .fetch_one(&f.app.db)
        .await
        .unwrap();
        assert_eq!(effects, 1, "exactly one provider-test success is published");
        let replay = f.call("GET", &path, None, &[("cookie", &cookie)]).await;
        assert_eq!(replay.status, StatusCode::CONFLICT);
    }
    let observed: Value = client
        .get(format!("{origin}/fixture/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        observed["exchanges"], 1,
        "duplicates and restart never retry the provider code"
    );
    child.kill().await.unwrap();
}

#[tokio::test]
async fn oidc_callback_reservation_fences_concurrent_exchange_and_duplicate_effects() {
    oidc_reservation_case(false).await;
}

#[tokio::test]
async fn oidc_callback_cancellation_and_restart_preserve_the_reservation() {
    oidc_reservation_case(true).await;
}
