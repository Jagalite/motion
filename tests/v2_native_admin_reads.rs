//! Real native activation and public router admission, not a pretend native
//! owner. Authentication rows remain Motion-owned; reads grant no catalog,
//! source, processing, restore or sign-in configuration authority.
use axum::{
    extract::State,
    http::{HeaderMap, Method, StatusCode},
};
use playscale as playscale_app;
use playscale::v2::{auth, signin, system};
use serde_json::{Value, json};

#[path = "support/native_application_fixture.rs"]
mod native_application_fixture;
use native_application_fixture::{Fixture, OPERATOR};

const PROVIDER_SECRET: &str = "native-admin-fixture-provider-secret";
const MAIL_SECRET: &str = "native-admin-fixture-mail-password";
const REMOTE_DETAIL: &str = "untrusted SMTP output private-path credential-sentinel";

async fn state(f: &Fixture) -> (i64, i64, String, i64, i64, i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT generation FROM catabolic_projection_state WHERE singleton=1),
        (SELECT count(*) FROM catabolic_commands), reviewed_epoch,revision,email_revision,
        (SELECT count(*) FROM signin_audit), (SELECT count(*) FROM signin_proofs),
        (SELECT COALESCE(SUM(consumed),0) FROM signin_proofs) FROM signin_settings",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap()
}

fn no_secrets(body: &Value, extra: &[&str]) {
    let raw = body.to_string();
    for forbidden in [OPERATOR, PROVIDER_SECRET, MAIL_SECRET, REMOTE_DETAIL]
        .into_iter()
        .chain(extra.iter().copied())
    {
        assert!(!raw.contains(forbidden), "private material escaped a read");
    }
    for key in [
        "\"secret\"",
        "\"password\"",
        "\"payload\"",
        "\"code_hash\"",
        "\"parent_hash\"",
        "\"session_hash\"",
        "\"actor_credential_hash\"",
        "\"approver_credential_hash\"",
        "\"email_secret\"",
        "\"dkim_secret\"",
        "\"nonce\"",
        "\"verifier\"",
        "\"state_hash\"",
        "\"ceiling_json\"",
        "\"code\"",
    ] {
        assert!(
            !raw.contains(key),
            "private credential field escaped a read: {key}"
        );
    }
}

#[tokio::test]
async fn native_admin_read_routes_are_private_redacted_readonly_and_restart_safe() {
    let mut f = Fixture::new().await;
    let (admin_id, admin_token) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let (_reserve_id, _reserve_token) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let (_, low_token) = f.pair(&["default"], &["catalog:read"]).await;
    let operator = format!("Bearer {OPERATOR}");
    let admin = format!("Bearer {admin_token}");
    let low = format!("Bearer {low_token}");
    // Opaque disabled provider material is fixture data, never a valid sign-in
    // or a credential proof. The read must not decrypt or return it. Avoid
    // inventing retention support for unqualified pre-cutover write receipts.
    for n in 0..2 {
        sqlx::query("INSERT INTO signin_providers(id,revision,name,issuer,client_id,secret,destinations,enabled) VALUES(?,1,?,?,?,?,'[]',0)")
            .bind(format!("provider-{n}")).bind(format!("Provider {n}"))
            .bind(format!("https://native-admin-{n}.example.invalid"))
            .bind("public-client-id").bind(PROVIDER_SECRET)
            .execute(&f.app.db).await.unwrap();
    }
    let email = f
        .call(
            "PUT",
            "/api/v2/admin/auth/email",
            Some(json!({
                "enabled":false,"host":"smtp.example.invalid","port":587,"username":"fixture",
                "from":"motion@example.invalid","password":{"action":"replace","value":MAIL_SECRET}
            })),
            &[("authorization", &operator), ("if-match", "\"r-1\"")],
        )
        .await;
    assert_eq!(email.status, StatusCode::OK, "{}", email.body);
    assert_eq!(email.body["has_password"], true);
    // Deliberately hostile historical network output. It is observational
    // fixture data, not a delivery, login, credential or catalog proof.
    sqlx::query("INSERT INTO signin_outbox(id,purpose,owner_id,phase,next_attempt,expires_at,mode,config_revision,epoch,last_error,recipient_domain,destination_hash,created_at,updated_at)
        SELECT 'mail-history','test','fixture','failed',0,1,'relay',email_revision,reviewed_epoch,?,'example.invalid','private-destination-hash',0,0 FROM signin_settings")
        .bind(REMOTE_DETAIL).execute(&f.app.db).await.unwrap();
    f.cutover().await;
    let user = f.call("POST", "/api/v2/users", Some(json!({
        "name":"Read-only auth fixture", "disabled":false, "profile_ids":["default"],
        "permissions":["catalog:read"],
        "policy":{"library_ids":[],"allow_unrated":true,"allowed_ratings":[],"blocked_labels":[]}
    })), &[("authorization", &operator), ("idempotency-key", "native-admin-user")]).await;
    assert_eq!(user.status, StatusCode::CREATED, "{}", user.body);
    let user_id = user.body["id"].as_str().unwrap().to_owned();
    let proof = f
        .call(
            "POST",
            "/api/v2/auth/proofs",
            Some(json!({"action":"provider.write","target":"new"})),
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(proof.status, StatusCode::OK, "{}", proof.body);
    let proof_code = proof.body["code"].as_str().unwrap();
    let before = state(&f).await;
    assert_eq!((before.6, before.7), (1, 0), "one unconsumed proof");
    let mut paths = vec![
        "/api/v2/admin/diagnostics".to_owned(),
        "/api/v2/admin/auth/overview".into(),
        "/api/v2/admin/auth/restore-review".into(),
        "/api/v2/admin/auth/providers".into(),
        "/api/v2/admin/auth/email".into(),
        "/api/v2/admin/auth/email/activity".into(),
        "/api/v2/admin/auth/registration".into(),
        "/api/v2/admin/auth/requests".into(),
        "/api/v2/admin/auth/fresh-approvals".into(),
        "/api/v2/admin/auth/identities".into(),
        "/api/v2/admin/auth/sessions".into(),
        "/api/v2/admin/auth/audit".into(),
    ];
    paths.push(format!("/api/v2/admin/auth/users/{user_id}"));
    for path in &paths {
        let anonymous = f.call("GET", path, None, &[]).await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(anonymous.headers["cache-control"], "no-store", "{path}");
        let forbidden = f.call("GET", path, None, &[("authorization", &low)]).await;
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(forbidden.headers["cache-control"], "no-store", "{path}");
        let allowed = f
            .call("GET", path, None, &[("authorization", &admin)])
            .await;
        assert_eq!(allowed.status, StatusCode::OK, "{path}: {}", allowed.body);
        // v2 request stamping and the outer API boundary both prohibit all
        // storage with no-store. private,no-store is the separate presentation
        // contract, not an additional requirement for these JSON API reads.
        assert_eq!(allowed.headers["cache-control"], "no-store", "{path}");
        no_secrets(
            &allowed.body,
            &[
                &admin_token,
                &low_token,
                proof_code,
                "private-destination-hash",
            ],
        );
    }
    let activity = f
        .call(
            "GET",
            "/api/v2/admin/auth/email/activity",
            None,
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(
        activity.body["items"][0]["detail"],
        "mail_delivery_detail_recorded"
    );
    let first = f
        .call(
            "GET",
            "/api/v2/admin/auth/providers?limit=1",
            None,
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["items"].as_array().unwrap().len(), 1);
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let second = f
        .call(
            "GET",
            &format!("/api/v2/admin/auth/providers?limit=1&cursor={cursor}"),
            None,
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    assert_ne!(first.body["items"][0]["id"], second.body["items"][0]["id"]);
    assert!(second.body["next_cursor"].is_null());
    // These methods are not registered for their paths. The outer API's
    // method-not-allowed fallback runs outside the native adapter gate and
    // returns an exact 405 problem, not native_route_unavailable.
    for (method, path) in [
        ("POST", "/api/v2/admin/diagnostics"),
        ("POST", "/api/v2/admin/auth/overview"),
        ("DELETE", "/api/v2/admin/auth/providers"),
        ("GET", "/api/v2/admin/auth/settings"),
        ("POST", "/api/v2/admin/auth/email"),
        ("PUT", "/api/v2/admin/auth/requests"),
        ("POST", "/api/v2/admin/auth/fresh-approvals"),
    ] {
        let blocked = f
            .call(method, path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(
            blocked.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}: {}",
            blocked.body
        );
        assert_eq!(
            blocked.body["code"], "method_not_allowed",
            "{method} {path}"
        );
        assert_eq!(blocked.headers["cache-control"], "no-store", "{path}");
    }
    // These operations are registered, but none has a native serving grant.
    // They must reach the gate and fail with this specific 503 before any
    // handler body, proof consumption, backup admission or auth mutation.
    for (method, path) in [
        ("POST", "/api/v2/admin/auth/providers"),
        ("PUT", "/api/v2/admin/auth/settings"),
        ("POST", "/api/v2/admin/auth/restore-review"),
        ("PUT", "/api/v2/admin/auth/email"),
        ("POST", "/api/v2/admin/auth/email/test"),
        ("POST", "/api/v2/admin/auth/email/diagnostics"),
        ("PUT", "/api/v2/admin/auth/registration"),
        ("POST", "/api/v2/admin/auth/requests/request-1/approve"),
        ("POST", "/api/v2/admin/auth/requests/request-1/deny"),
        ("POST", "/api/v2/admin/auth/users/user-1/revoke"),
        ("GET", "/api/v2/admin/backups"),
        ("POST", "/api/v2/admin/backups"),
    ] {
        let blocked = f
            .call(method, path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(
            blocked.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {path}: {}",
            blocked.body
        );
        assert_eq!(blocked.body["code"], "native_route_unavailable");
        assert_eq!(blocked.headers["cache-control"], "no-store", "{path}");
    }
    // Neither path has a handler. Unlike a wrong method on a known route,
    // these enter the nested v2 fallback, which remains wrapped by the native
    // positive gate. They must not become restore/cleanup operations or pass
    // through to the outer presentation fallback.
    for path in ["/api/v2/admin/restore", "/api/v2/admin/cleanup"] {
        let blocked = f
            .call("POST", path, None, &[("authorization", &operator)])
            .await;
        assert_eq!(
            blocked.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "POST {path}: {}",
            blocked.body
        );
        assert_eq!(blocked.body["code"], "native_route_unavailable", "{path}");
        assert_eq!(blocked.headers["cache-control"], "no-store", "{path}");
    }
    let capabilities = f
        .call(
            "GET",
            "/api/v2/system/capabilities",
            None,
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(capabilities.status, StatusCode::OK, "{}", capabilities.body);
    for id in ["admin.native_diagnostics", "admin.native_auth_reads"] {
        let feature = capabilities.body["features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .unwrap();
        assert_eq!(feature["implemented"], true);
        assert_eq!(feature["enabled"], true);
        assert_eq!(feature["qualification"], "unqualified");
        assert_eq!(feature["limits"]["authorization"], "current_system_admin");
    }
    assert_eq!(
        state(&f).await,
        before,
        "reads and blocked writes must not change auth/catalog state"
    );
    f.close().await;
    // Missing retained owner must not silently switch even these application
    // reads to legacy handlers. No schema41 or restore admission is granted.
    let unavailable = f
        .call(
            "GET",
            "/api/v2/admin/diagnostics",
            None,
            &[("authorization", &admin)],
        )
        .await;
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unavailable.body["code"], "native_runtime_unavailable");
    assert_eq!(unavailable.headers["cache-control"], "no-store");
    f.open().await;
    for path in &paths {
        let read = f
            .call("GET", path, None, &[("authorization", &admin)])
            .await;
        assert_eq!(read.status, StatusCode::OK, "{path}: {}", read.body);
        assert_eq!(read.headers["cache-control"], "no-store", "{path}");
        no_secrets(&read.body, &[&admin_token, &low_token, proof_code]);
    }
    assert_eq!(state(&f).await, before);
    let device = f
        .call(
            "GET",
            &format!("/api/v2/devices/{admin_id}"),
            None,
            &[("authorization", &operator)],
        )
        .await;
    let revoked = f
        .call(
            "DELETE",
            &format!("/api/v2/devices/{admin_id}"),
            None,
            &[
                ("authorization", &operator),
                ("if-match", device.headers["etag"].to_str().unwrap()),
            ],
        )
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT, "{}", revoked.body);
    for path in &paths {
        let revoked = f
            .call("GET", path, None, &[("authorization", &admin)])
            .await;
        assert_eq!(revoked.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(revoked.headers["cache-control"], "no-store", "{path}");
    }
    f.close().await;
}

#[tokio::test]
async fn native_admin_diagnostics_rechecks_cached_authority_and_observes_real_scan_demands() {
    let mut f = Fixture::new().await;
    let (device, token) = f
        .pair(&["default"], &["catalog:read", "system:admin"])
        .await;
    let operator = format!("Bearer {OPERATOR}");
    let credential = format!("Bearer {token}");
    f.cutover().await;
    let root = f._dir.path().join("scan-source");
    std::fs::create_dir(&root).unwrap();
    let source = f
        .call(
            "POST",
            "/api/v2/sources",
            Some(json!({"name":"Diagnostic source","root_path":root,"exclusions":[]})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "admin-diagnostic-source"),
            ],
        )
        .await;
    assert_eq!(source.status, StatusCode::CREATED, "{}", source.body);
    let library = f.call("POST", "/api/v2/libraries", Some(json!({"name":"Diagnostic library","kind":"mixed","language":"en","source_ids":[source.body["id"]]})),
        &[("authorization", &operator), ("idempotency-key", "admin-diagnostic-library")]).await;
    assert_eq!(library.status, StatusCode::CREATED, "{}", library.body);
    let scan = f
        .call(
            "POST",
            &format!(
                "/api/v2/libraries/{}/scans",
                library.body["id"].as_str().unwrap()
            ),
            Some(json!({"mode":"verify","require_complete":true})),
            &[
                ("authorization", &operator),
                ("idempotency-key", "admin-diagnostic-scan"),
            ],
        )
        .await;
    assert_eq!(scan.status, StatusCode::ACCEPTED, "{}", scan.body);
    let diagnostics = f
        .call(
            "GET",
            "/api/v2/admin/diagnostics",
            None,
            &[("authorization", &credential)],
        )
        .await;
    assert_eq!(diagnostics.status, StatusCode::OK, "{}", diagnostics.body);
    assert_eq!(diagnostics.body["queued_jobs"], "1");
    assert_eq!(diagnostics.body["running_jobs"], "0");
    assert_eq!(diagnostics.body["worker_errors"], json!([]));
    no_secrets(&diagnostics.body, &[&token, root.to_str().unwrap()]);
    let mut headers = HeaderMap::new();
    headers.insert("authorization", credential.parse().unwrap());
    let cached = auth::resolve(&f.app, &Method::GET, &headers).await.unwrap();
    // Model the interleaving after extraction and before transactional read.
    sqlx::query(
        "UPDATE devices SET permissions='[\"catalog:read\"]',revision=revision+1 WHERE id=?",
    )
    .bind(&device)
    .execute(&f.app.db)
    .await
    .unwrap();
    let denied = system::diagnostics(State(f.app.clone()), cached.clone())
        .await
        .err()
        .unwrap();
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let denied = signin::overview(State(f.app.clone()), cached)
        .await
        .err()
        .unwrap();
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catabolic_scan_demands WHERE phase='queued' AND status='pending'",
    )
    .fetch_one(&f.app.db)
    .await
    .unwrap();
    assert_eq!(
        queued, 1,
        "diagnostics must neither claim nor cancel a real accepted scan"
    );
    f.close().await;
}
