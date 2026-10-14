use super::*;
const BINDING_COOKIE: &str = "motion_signin";
// Use only the transport peer supplied by axum. Forwarded headers cannot choose
// a rate-limit identity. Reverse proxies share their transport peer's bucket.
pub(super) type Peer = Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>;
pub(super) async fn network_limit(
    app: &App,
    peer: &Peer,
    lane: &str,
    count: i64,
    seconds: i64,
) -> Result<(), Problem> {
    let address = peer
        .as_ref()
        .map(|p| p.0.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into());
    let key = keyed(app, "signin-network", &address);
    limit(app, &format!("{lane}:network:{key}"), count, seconds).await
}

pub(super) fn binding(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, v)| {
            *k == BINDING_COOKIE && v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())
        })
        .map(|(_, v)| v)
}
pub(super) fn binding_header(app: &App, value: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{BINDING_COOKIE}={value}; Path=/api/v2/auth; HttpOnly; SameSite=Lax; Max-Age={};{}",
        core::TRANSACTION_TTL,
        if app.origin.starts_with("https://") {
            " Secure"
        } else {
            ""
        }
    ))
    .unwrap()
}
pub(super) fn origin(app: &App, headers: &HeaderMap) -> Result<(), Problem> {
    if headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) != Some(app.origin.as_str()) {
        return Err(Problem::new(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "The exact Motion origin is required",
        ));
    }
    Ok(())
}
async fn initiating_caller(
    app: &App,
    headers: &HeaderMap,
    purpose: core::Purpose,
) -> Result<Option<Caller>, Problem> {
    // Login proves a new identity. A lingering expired/revoked session must not
    // block it or turn it into linking. Origin and transaction-browser binding
    // still apply; every identity-changing purpose authenticates the caller.
    if purpose == core::Purpose::Login {
        return Ok(None);
    }
    if auth::bearer(headers)?.is_some() || auth::session_cookie(headers).is_some() {
        Ok(Some(
            auth::resolve(app, &axum::http::Method::POST, headers).await?,
        ))
    } else {
        Ok(None)
    }
}
#[derive(sqlx::FromRow)]
pub(super) struct TxRow {
    pub(super) provider_id: Option<String>,
    pub(super) state_json: String,
    nonce: Option<String>,
    verifier: Option<String>,
    pub(super) address: Option<String>,
    code_hash: Option<String>,
    return_to: String,
    actor_id: Option<String>,
    actor_credential_hash: Option<String>,
    identity_id: Option<String>,
    pub(super) result: Option<String>,
    ceiling_json: Option<String>,
}
impl TxRow {
    fn state(&self) -> Result<core::Transaction, Problem> {
        serde_json::from_str(&self.state_json).map_err(Problem::internal)
    }
}
async fn txrow(c: &mut sqlx::SqliteConnection, id: &str) -> Result<TxRow, Problem> {
    sqlx::query_as("SELECT provider_id,state_json,nonce,verifier,address,code_hash,return_to,actor_id,actor_credential_hash,identity_id,result,ceiling_json FROM signin_transactions WHERE id=?").bind(id).fetch_optional(c).await?.ok_or_else(Problem::not_found)
}
enum Step {
    Reserve,
    Complete,
    Code(bool),
}
async fn current(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    row: &TxRow,
    headers: &HeaderMap,
    step: Step,
) -> Result<core::Transaction, Problem> {
    let state = row.state()?;
    let browser = keyed(
        app,
        "browser",
        binding(headers).ok_or_else(|| problem(core::Error::BrowserMismatch))?,
    );
    let (revision, enabled) = if let Some(id) = &row.provider_id {
        let r = sqlx::query("SELECT revision,enabled FROM signin_providers WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *c)
            .await?
            .ok_or_else(unavailable)?;
        (
            r.get::<i64, _>("revision"),
            r.get::<bool, _>("enabled") || state.purpose == core::Purpose::Test,
        )
    } else {
        let mail = mail(c, app).await?;
        (mail.revision, mail.cfg.enabled)
    };
    let user = match &state.user_id {
        Some(id) => access_management::load_user(c, id).await?,
        None => None,
    };
    if let Some(actor) = &row.actor_id {
        actor_valid(c, actor, row.actor_credential_hash.as_deref()).await?;
    }
    let epoch = epoch(c).await?;
    let identity: Option<(i64, bool, String)> = match row.identity_id.as_deref() {
        Some(id) => {
            sqlx::query_as("SELECT revision,enabled,user_id FROM signin_identities WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *c)
                .await?
        }
        None => None,
    };
    let facts = core::Current {
        browser_hash: &browser,
        epoch: &epoch,
        server_epoch: &app.access.server_epoch,
        provider_revision: revision as u64,
        method_enabled: enabled,
        restore_hold: hold(c).await?,
        user_id: user.as_ref().map(|u| u.id.as_str()),
        user_revision: user.as_ref().map(|u| u.revision),
        identity_revision: identity.as_ref().map(|v| v.0 as u64),
        identity_enabled: row.identity_id.is_none() || identity.as_ref().is_some_and(|v| v.1),
        identity_owner: identity.as_ref().map(|v| v.2.as_str()),
        actor_revision: match row.actor_id.as_deref() {
            Some(id) if id != "operator" => {
                auth::load_device(c, id).await?.map(|r| r.device.revision)
            }
            _ => None,
        },
        user_enabled: state.user_id.is_none() || user.as_ref().is_some_and(|u| !u.disabled),
        now: now(),
    };
    Ok(match step {
        Step::Reserve => core::reserve(&state, &facts)?,
        Step::Complete => core::complete(&state, &facts)?,
        Step::Code(matches) => core::redeem_code(&state, &facts, matches)?.0,
    })
}
async fn save_state(
    c: &mut sqlx::SqliteConnection,
    state: &core::Transaction,
    result: Option<&str>,
) -> Result<(), Problem> {
    sqlx::query("UPDATE signin_transactions SET state_json=?,result=?,verifier=CASE WHEN ? IN ('consumed','failed') THEN NULL ELSE verifier END,code_hash=CASE WHEN ? IN ('consumed','failed') THEN NULL ELSE code_hash END WHERE id=?").bind(jsonstr(state)).bind(result).bind(jsonstr(&state.phase).trim_matches('"')).bind(jsonstr(&state.phase).trim_matches('"')).bind(&state.id).execute(c).await?;
    Ok(())
}
pub(super) async fn trim(c: &mut sqlx::SqliteConnection) -> Result<(), Problem> {
    sqlx::query("DELETE FROM signin_transactions WHERE expires_at<?")
        .bind(now() - 3600)
        .execute(&mut *c)
        .await?;
    sqlx::query("UPDATE signin_requests SET status='expired',revision=revision+1 WHERE status='pending' AND expires_at<=?").bind(now()).execute(&mut *c).await?;
    sqlx::query("DELETE FROM signin_requests WHERE expires_at<?")
        .bind(now() - 86400 * 7)
        .execute(&mut *c)
        .await?;
    sqlx::query("DELETE FROM signin_proofs WHERE expires_at<?")
        .bind(now() - 3600)
        .execute(&mut *c)
        .await?;
    Ok(())
}
pub async fn methods(State(app): State<App>) -> Result<Response, Problem> {
    let mut c = app.db.acquire().await?;
    let ready = !hold(&mut c).await?
        && sqlx::query_scalar::<_, Option<String>>("SELECT public_origin FROM signin_settings")
            .fetch_one(&mut *c)
            .await?
            .is_some();
    let rows =
        sqlx::query("SELECT id,name FROM signin_providers WHERE enabled=1 ORDER BY name LIMIT 32")
            .fetch_all(&mut *c)
            .await?;
    let email = mail(&mut c, &app).await?.cfg.enabled && ready;
    let registration = super::registration::public_policy(&mut c, email).await?;
    Ok(Json(json!({"providers":if ready{rows.iter().map(|r|json!({"id":r.get::<String,_>("id"),"name":r.get::<String,_>("name")})).collect::<Vec<_>>()}else{vec![]},"email_enabled":email,"registration":registration,"pairing_enabled":true})).into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    purpose: core::Purpose,
    #[serde(default = "default_return")]
    return_to: String,
}
fn default_return() -> String {
    "/".into()
}
pub async fn oidc_start(
    State(app): State<App>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    peer: Peer,
    body: Body<Start>,
) -> Result<Response, Problem> {
    origin(&app, &headers)?;
    if !core::safe_return(&body.value.return_to)
        || !matches!(
            body.value.purpose,
            core::Purpose::Login
                | core::Purpose::Link
                | core::Purpose::Test
                | core::Purpose::Reauth
        )
    {
        return Err(Problem::bad(
            "invalid_signin_request",
            "Choose a valid sign-in purpose and local destination",
        ));
    }
    limit(&app, "oidc:global", 60, 60).await?;
    network_limit(&app, &peer, "oidc", 30, 60).await?;
    let browser = binding(&headers)
        .map(str::to_owned)
        .unwrap_or_else(|| keyed(&app, "browser-random", &auth::nonce()));
    limit(&app, &format!("oidc:browser:{browser}"), 12, 60).await?;
    let caller = initiating_caller(&app, &headers, body.value.purpose).await?;
    let mut tx = begin_write(&app.db).await?;
    trim(&mut tx).await?;
    if hold(&mut tx).await? {
        return Err(unavailable());
    }
    let (revision, enabled, cfg) = provider(&mut tx, &app, &provider_id).await?;
    if !enabled && body.value.purpose != core::Purpose::Test {
        return Err(unavailable());
    }
    let mut evidence_until = now() + core::TRANSACTION_TTL;
    let user = if body.value.purpose != core::Purpose::Login {
        let caller = caller.as_ref().ok_or(AccessError::Unauthenticated)?;
        caller
            .reauthorize(
                &mut tx,
                if body.value.purpose == core::Purpose::Test {
                    Some(Permission::SystemAdmin)
                } else {
                    None
                },
            )
            .await?;
        if matches!(
            body.value.purpose,
            core::Purpose::Link | core::Purpose::Test
        ) {
            evidence_until = require_fresh(
                &mut tx,
                caller,
                &headers,
                if body.value.purpose == core::Purpose::Test {
                    "provider.test"
                } else {
                    "identity.link"
                },
                &provider_id,
            )
            .await?;
        }
        let user = owned_user(&mut tx, caller).await?;
        if body.value.purpose != core::Purpose::Test && user.is_none() {
            return Err(Problem::invalid(
                "user_required",
                "Assign this device to a user before linking a sign-in method",
            ));
        }
        user
    } else {
        None
    };
    let ceiling = match caller
        .as_ref()
        .filter(|c| !c.is_local_operator() && body.value.purpose != core::Purpose::Login)
    {
        Some(c) => auth::load_device(&mut tx, &c.principal.id)
            .await?
            .map(|r| r.device),
        None => None,
    };
    let id = new_id();
    let state_secret = keyed(&app, "oidc-state", &auth::nonce());
    let nonce = keyed(&app, "oidc-nonce", &auth::nonce());
    let verifier = keyed(&app, "oidc-pkce", &auth::nonce());
    let state = core::Transaction {
        id: id.clone(),
        purpose: body.value.purpose,
        browser_hash: keyed(&app, "browser", &browser),
        epoch: epoch(&mut tx).await?,
        server_epoch: app.access.server_epoch.clone(),
        provider_revision: revision as u64,
        user_id: user.as_ref().map(|u| u.id.clone()),
        user_revision: user.as_ref().map(|u| u.revision),
        identity_revision: None,
        actor_revision: ceiling.as_ref().map(|d| d.revision),
        expires_at: evidence_until,
        phase: core::Phase::Pending,
        attempts: 0,
        registration: None,
    };
    let actor = if body.value.purpose == core::Purpose::Login {
        None
    } else {
        caller.as_ref()
    };
    sqlx::query("INSERT INTO signin_transactions(id,state_hash,provider_id,state_json,nonce,verifier,return_to,actor_id,actor_credential_hash,created_at,expires_at,ceiling_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)").bind(&id).bind(keyed(&app,"state-lookup",&state_secret)).bind(&provider_id).bind(jsonstr(&state)).bind(&nonce).bind(seal(&app,&format!("verifier:{id}"),&verifier)?).bind(&body.value.return_to).bind(actor.map(|c|c.principal.id.as_str())).bind(actor.and_then(|c|c.credential().map(|x|x.0))).bind(now()).bind(state.expires_at).bind(ceiling.as_ref().map(jsonstr)).execute(&mut *tx).await?;
    tx.commit().await?;
    let url = crate::signin_protocol::authorization_url(&cfg, &state_secret, &nonce, &verifier)
        .await
        .map_err(|_| unavailable())?;
    let mut response = Json(json!({"transaction_id":id,"authorization_url":url})).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, binding_header(&app, &browser));
    Ok(response)
}
#[derive(Deserialize)]
pub struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
pub async fn oidc_callback(
    State(app): State<App>,
    Query(query): Query<Callback>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let state_secret = query
        .state
        .filter(|v| v.len() <= 256)
        .ok_or_else(unavailable)?;
    let mut tx = begin_write(&app.db).await?;
    let id: String = sqlx::query_scalar("SELECT id FROM signin_transactions WHERE state_hash=?")
        .bind(keyed(&app, "state-lookup", &state_secret))
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(unavailable)?;
    let row = txrow(&mut tx, &id).await?;
    let mut state = current(&mut tx, &app, &row, &headers, Step::Reserve).await?;
    if query.error.is_some() {
        state.phase = core::Phase::Failed;
        save_state(&mut tx, &state, Some("denied")).await?;
        tx.commit().await?;
        return Ok(callback_response(&id));
    }
    let code = query
        .code
        .filter(|v| !v.is_empty() && v.len() <= 8192)
        .ok_or_else(unavailable)?;
    let (_, _, cfg) = provider(
        &mut tx,
        &app,
        row.provider_id.as_deref().ok_or_else(unavailable)?,
    )
    .await?;
    let verifier = open(
        &app,
        &format!("verifier:{id}"),
        row.verifier.as_deref().ok_or_else(unavailable)?,
    )?;
    let nonce = row.nonce.as_deref().ok_or_else(unavailable)?.to_owned();
    // current() validated the same facts under this reserved writer snapshot.
    save_state(&mut tx, &state, None).await?;
    tx.commit().await?;
    let verified = crate::signin_protocol::exchange(&cfg, &code, &verifier, &nonce).await;
    let mut tx = begin_write(&app.db).await?;
    let row = txrow(&mut tx, &id).await?;
    let mut state = current(&mut tx, &app, &row, &headers, Step::Complete).await?;
    let verified = match verified {
        Ok(v) => v,
        Err(_) => {
            state.phase = core::Phase::Failed;
            save_state(&mut tx, &state, Some("failed")).await?;
            audit(
                &mut tx,
                "anonymous",
                row.provider_id.as_deref().unwrap(),
                "oidc.login",
                "failed",
            )
            .await?;
            tx.commit().await?;
            return Ok(callback_response(&id));
        }
    };
    let mut token = None;
    let result = match state.purpose {
        core::Purpose::Test => {
            let actor = row.actor_id.as_deref().ok_or_else(unavailable)?;
            if actor != "operator" {
                let device = auth::load_device(&mut tx, actor)
                    .await?
                    .ok_or_else(unavailable)?;
                if !device
                    .device
                    .grant
                    .permissions
                    .contains(&Permission::SystemAdmin)
                {
                    return Err(AccessError::Forbidden(Permission::SystemAdmin).into());
                }
            }
            sqlx::query("UPDATE signin_providers SET login_test_at=?,test_at=?,test_ok=1 WHERE id=? AND revision=?").bind(now()).bind(now()).bind(row.provider_id.as_deref()).bind(state.provider_revision as i64).execute(&mut *tx).await?;
            "tested"
        }
        core::Purpose::Link => {
            let user = access_management::load_user(&mut tx, state.user_id.as_deref().unwrap())
                .await?
                .ok_or_else(unavailable)?;
            link_identity(
                &mut tx,
                &user,
                &serde_json::from_str::<Device>(
                    row.ceiling_json.as_deref().ok_or_else(unavailable)?,
                )
                .map_err(Problem::internal)?,
                "oidc",
                row.provider_id.as_deref(),
                Some(&verified.issuer),
                Some(&verified.subject),
                None,
            )
            .await?;
            "linked"
        }
        core::Purpose::Login | core::Purpose::Reauth => {
            let identity: Option<String> = sqlx::query_scalar(
                "SELECT id FROM signin_identities WHERE issuer=? AND subject=? AND enabled=1",
            )
            .bind(&verified.issuer)
            .bind(&verified.subject)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(identity) = identity {
                if state.purpose == core::Purpose::Reauth {
                    let owner: String =
                        sqlx::query_scalar("SELECT user_id FROM signin_identities WHERE id=?")
                            .bind(&identity)
                            .fetch_one(&mut *tx)
                            .await?;
                    core::link_owner(state.user_id.as_deref(), &owner)?;
                }
                token = Some(
                    issue(
                        &mut tx,
                        &app,
                        &identity,
                        state.provider_revision,
                        verified.authenticated_at,
                    )
                    .await?,
                );
                "signed_in"
            } else if state.purpose == core::Purpose::Login {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM signin_requests WHERE status='pending'",
                )
                .fetch_one(&mut *tx)
                .await?;
                if count >= 1000 {
                    return Err(Problem::rate_limited(600));
                }
                sqlx::query("INSERT OR IGNORE INTO signin_requests(id,provider_id,provider_revision,issuer,subject,email,name,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?,?)").bind(new_id()).bind(row.provider_id.as_deref()).bind(state.provider_revision as i64).bind(&verified.issuer).bind(&verified.subject).bind(verified.email.as_deref().filter(|s|s.len()<=254)).bind(verified.display_name.as_deref().filter(|s|s.len()<=200)).bind(now()).bind(now()+86400).execute(&mut *tx).await?;
                "pending_approval"
            } else {
                return Err(unavailable());
            }
        }
        _ => return Err(unavailable()),
    };
    state.phase = core::Phase::Consumed;
    save_state(&mut tx, &state, Some(result)).await?;
    audit(
        &mut tx,
        row.actor_id.as_deref().unwrap_or("anonymous"),
        row.provider_id.as_deref().unwrap(),
        "oidc.login",
        result,
    )
    .await?;
    tx.commit().await?;
    let mut response = callback_response(&id);
    if let Some(token) = token {
        response
            .headers_mut()
            .append(header::SET_COOKIE, session_header(&app, &token));
    }
    Ok(response)
}
fn callback_response(id: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/signin?transaction={id}")).unwrap(),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
pub async fn transaction(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let mut c = app.db.acquire().await?;
    let row = txrow(&mut c, &id).await?;
    let state = row.state()?;
    if state.browser_hash
        != keyed(
            &app,
            "browser",
            binding(&headers).ok_or_else(Problem::not_found)?,
        )
    {
        return Err(Problem::not_found());
    }
    let status = if now() >= state.expires_at {
        "expired"
    } else {
        row.result.as_deref().unwrap_or("pending")
    };
    Ok(Json(json!({"status":status,"detail":match status{"pending_approval"=>"An administrator must approve access. Sign in again after approval.","tested"=>"Provider sign-in verified. Return to Authentication & Access to enable it.","linked"=>"Sign-in method linked.","failed"|"denied"=>"Sign-in did not complete. Start again.","invitation_accepted"=>"Invitation accepted. Your account is active.","account_active"=>"Your account is active.","registration_changed"=>"Registration settings changed. Start a new registration.",_=>""},"redirect_to":if status=="signed_in"{Some(row.return_to)}else{None}})).into_response())
}
pub(super) async fn link_identity(
    c: &mut sqlx::SqliteConnection,
    user: &User,
    initiating: &Device,
    kind: &str,
    provider: Option<&str>,
    issuer: Option<&str>,
    subject: Option<&str>,
    address: Option<&str>,
) -> Result<String, Problem> {
    let existing: Option<(String, String, bool)> = if kind == "oidc" {
        sqlx::query_as(
            "SELECT id,user_id,enabled FROM signin_identities WHERE issuer=? AND subject=?",
        )
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&mut *c)
        .await?
    } else {
        sqlx::query_as("SELECT id,user_id,enabled FROM signin_identities WHERE address=?")
            .bind(address)
            .fetch_optional(&mut *c)
            .await?
    };
    if let Some((id, owner, enabled)) = existing {
        core::link_owner(Some(&owner), &user.id)?;
        let row =
            sqlx::query("SELECT profile_ids,permissions,policy FROM signin_identities WHERE id=?")
                .bind(&id)
                .fetch_one(&mut *c)
                .await?;
        let mut previous = Device::approved(
            id.clone(),
            Grant {
                profile_ids: serde_json::from_str(row.get("profile_ids"))
                    .map_err(Problem::internal)?,
                permissions: serde_json::from_str(row.get("permissions"))
                    .map_err(Problem::internal)?,
            },
        );
        previous.policy = serde_json::from_str(row.get("policy")).map_err(Problem::internal)?;
        let next = access_admin::link_ceiling(user, initiating, Some(&previous))?;
        if !enabled || next.grant != previous.grant || next.policy != previous.policy {
            let families: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM signin_families WHERE identity_id=? AND revoked=0",
            )
            .bind(&id)
            .fetch_all(&mut *c)
            .await?;
            revoke_families(c, &families, &user.id).await?;
            sqlx::query("UPDATE signin_identities SET enabled=1,revision=revision+1,profile_ids=?,permissions=?,policy=? WHERE id=?").bind(jsonstr(&next.grant.profile_ids)).bind(jsonstr(&next.grant.permissions)).bind(jsonstr(&next.policy)).bind(&id).execute(&mut *c).await?;
        }
        return Ok(id);
    }
    let ceiling = access_admin::link_ceiling(user, initiating, None)?;
    let id = new_id();
    sqlx::query("INSERT INTO signin_identities(id,user_id,kind,provider_id,issuer,subject,address,profile_ids,permissions,policy,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)").bind(&id).bind(&user.id).bind(kind).bind(provider).bind(issuer).bind(subject).bind(address).bind(jsonstr(&ceiling.grant.profile_ids)).bind(jsonstr(&ceiling.grant.permissions)).bind(jsonstr(&ceiling.policy)).bind(now()).execute(&mut *c).await?;
    audit(c, &user.id, &id, "identity.link", "ok").await?;
    Ok(id)
}
pub(super) async fn issue(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    identity: &str,
    method_revision: u64,
    authenticated_at: i64,
) -> Result<(String, i64), Problem> {
    let issued_at = now();
    let expires_at = core::session_expiry(authenticated_at, issued_at).ok_or_else(unavailable)?;
    let row = sqlx::query("SELECT * FROM signin_identities WHERE id=? AND enabled=1")
        .bind(identity)
        .fetch_one(&mut *c)
        .await?;
    let user = access_management::load_user(c, row.get("user_id"))
        .await?
        .filter(|u| !u.disabled)
        .ok_or_else(unavailable)?;
    let initial = Device {
        id: new_id(),
        revision: 1,
        grant: Grant {
            profile_ids: serde_json::from_str(row.get("profile_ids")).map_err(Problem::internal)?,
            permissions: serde_json::from_str(row.get("permissions")).map_err(Problem::internal)?,
        },
        policy: serde_json::from_str(row.get("policy")).map_err(Problem::internal)?,
        revoked: false,
        generation: 1,
        credential_expires_at: Some(expires_at),
    };
    let mut device = access_admin::constrain(&initial, &user)?;
    device.revision = 1;
    let d = auth::DeviceRow {
        device,
        name: "Browser sign-in".into(),
        client_name: "Motion web".into(),
        user_id: Some(user.id.clone()),
    };
    auth::insert_device(c, &d).await?;
    let parent = auth::derive_secret(&app.access.key, auth::Purpose::Device, &auth::nonce());
    let token = auth::derive_secret(&app.access.key, auth::Purpose::Session, &auth::nonce());
    let csrf = format!("csrf_{}", auth::nonce());
    let parent_hash = auth::token_hash(&parent);
    let session_hash = auth::token_hash(&token);
    let mut credential = Credential {
        device_id: d.device.id.clone(),
        kind: CredentialKind::Device,
        generation: 1,
        expires_at,
    };
    auth::insert_credential(c, &parent_hash, &credential, None, None).await?;
    credential.kind = CredentialKind::Session;
    auth::insert_credential(
        c,
        &session_hash,
        &credential,
        Some(&parent_hash),
        Some(&csrf),
    )
    .await?;
    let epoch = epoch(c).await?;
    sqlx::query("INSERT INTO signin_families(id,device_id,user_id,identity_id,identity_revision,method_revision,epoch,issued_at,authenticated_at,expires_at,parent_hash,session_hash) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)").bind(new_id()).bind(&d.device.id).bind(&user.id).bind(identity).bind(row.get::<i64,_>("revision")).bind(method_revision as i64).bind(epoch).bind(issued_at).bind(authenticated_at).bind(credential.expires_at).bind(parent_hash).bind(session_hash).execute(&mut *c).await?;
    Ok((token, expires_at))
}
fn session_header(app: &App, (token, expires_at): &(String, i64)) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        auth::SESSION_COOKIE,
        expires_at.saturating_sub(now()).max(0),
        if app.origin.starts_with("https://") {
            "; Secure"
        } else {
            ""
        }
    ))
    .unwrap()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailStart {
    address: String,
    purpose: core::Purpose,
}
pub async fn email_start(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Peer,
    body: Body<EmailStart>,
) -> Result<Response, Problem> {
    origin(&app, &headers)?;
    if !matches!(
        body.value.purpose,
        core::Purpose::Login | core::Purpose::Enroll | core::Purpose::Reauth
    ) {
        return Err(Problem::bad(
            "invalid_signin_request",
            "Choose a valid email sign-in purpose",
        ));
    }
    limit(&app, "email:global", 60, 60).await?;
    network_limit(&app, &peer, "email", 20, 600).await?;
    let browser = binding(&headers)
        .map(str::to_owned)
        .unwrap_or_else(|| keyed(&app, "browser-random", &auth::nonce()));
    limit(&app, &format!("email:browser:{browser}"), 5, 600).await?;
    // Every canonical address follows the same durable throttling and outward
    // response path, including unknown, disabled and ineligible recipients.
    let address = core::canonical_email(&body.value.address).unwrap_or_default();
    limit(&app, &format!("email:address:{address}"), 5, 600).await?;
    let caller = initiating_caller(&app, &headers, body.value.purpose).await?;
    let mut tx = begin_write(&app.db).await?;
    trim(&mut tx).await?;
    let mail = mail(&mut tx, &app).await?;
    let revision = mail.revision;
    if !mail.cfg.enabled || !super::outbox::rehearse(&mail) || hold(&mut tx).await? {
        return Err(unavailable());
    }
    let mut evidence_until = now() + core::TRANSACTION_TTL;
    let user = if body.value.purpose == core::Purpose::Login {
        None
    } else {
        let caller = caller.as_ref().ok_or(AccessError::Unauthenticated)?;
        caller.reauthorize(&mut tx, None).await?;
        if body.value.purpose == core::Purpose::Enroll {
            evidence_until =
                require_fresh(&mut tx, caller, &headers, "identity.link", "email").await?;
        }
        Some(owned_user(&mut tx, caller).await?.ok_or_else(|| {
            Problem::invalid(
                "user_required",
                "Assign this device to a user before enrolling email",
            )
        })?)
    };
    let identity: Option<(String, String, bool)> =
        sqlx::query_as("SELECT id,user_id,enabled FROM signin_identities WHERE address=?")
            .bind(&address)
            .fetch_optional(&mut *tx)
            .await?;
    let eligible = if address.is_empty() {
        false
    } else if body.value.purpose == core::Purpose::Enroll {
        identity
            .as_ref()
            .is_none_or(|(_, owner, _)| Some(owner) == user.as_ref().map(|u| &u.id))
    } else if let Some((_, owner, true)) = &identity {
        let active = access_management::load_user(&mut tx, owner)
            .await?
            .is_some_and(|u| !u.disabled);
        active
            && (body.value.purpose == core::Purpose::Login
                || Some(owner) == user.as_ref().map(|u| &u.id))
    } else {
        false
    };
    let bound_user = if !eligible {
        None
    } else if let Some(user) = user {
        Some(user)
    } else if let Some((_, owner, _)) = &identity {
        access_management::load_user(&mut tx, owner).await?
    } else {
        None
    };
    let ceiling = match caller
        .as_ref()
        .filter(|c| !c.is_local_operator() && body.value.purpose != core::Purpose::Login)
    {
        Some(c) => auth::load_device(&mut tx, &c.principal.id)
            .await?
            .map(|r| r.device),
        None => None,
    };
    let id = new_id();
    let random = auth::random_key();
    let code = format!(
        "{:08}",
        u32::from_be_bytes(random[..4].try_into().unwrap()) % 100_000_000
    );
    let state = core::Transaction {
        id: id.clone(),
        purpose: body.value.purpose,
        browser_hash: keyed(&app, "browser", &browser),
        epoch: epoch(&mut tx).await?,
        server_epoch: app.access.server_epoch.clone(),
        provider_revision: revision as u64,
        user_id: bound_user.as_ref().map(|u| u.id.clone()),
        user_revision: bound_user.as_ref().map(|u| u.revision),
        identity_revision: if eligible {
            match identity.as_ref() {
                Some((id, _, _)) => sqlx::query_scalar::<_, i64>(
                    "SELECT revision FROM signin_identities WHERE id=?",
                )
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
                .map(|n| n as u64),
                None => None,
            }
        } else {
            None
        },
        actor_revision: ceiling.as_ref().map(|d| d.revision),
        expires_at: evidence_until,
        phase: core::Phase::Pending,
        attempts: 0,
        registration: None,
    };
    let actor = if body.value.purpose == core::Purpose::Login {
        None
    } else {
        caller.as_ref()
    };
    // A dummy keyed hash makes code handling indistinguishable and bounded.
    let hash = keyed(
        &app,
        &format!("email-code:{id}"),
        if eligible { &code } else { "ineligible" },
    );
    sqlx::query("INSERT INTO signin_transactions(id,state_json,address,code_hash,return_to,actor_id,actor_credential_hash,identity_id,created_at,expires_at,ceiling_json) VALUES(?,?,?,?,?,?,?,?,?,?,?)").bind(&id).bind(jsonstr(&state)).bind(&address).bind(hash).bind("/").bind(actor.map(|c|c.principal.id.as_str())).bind(actor.and_then(|c|c.credential().map(|x|x.0))).bind(identity.as_ref().filter(|_|eligible).map(|v|v.0.as_str())).bind(now()).bind(state.expires_at).bind(ceiling.as_ref().map(jsonstr)).execute(&mut *tx).await?;
    if !eligible {
        super::outbox::rehearse(&mail);
    } else {
        super::outbox::enqueue(
            &mut tx,
            &app,
            &mail,
            playscale_core::mail::Purpose::SigninCode,
            &id,
            Some(&id),
            &address,
            crate::mail::Content::SigninCode { code: &code },
            state.expires_at,
        )
        .await?;
    }
    audit(
        &mut tx,
        actor
            .map(|c| c.principal.id.as_str())
            .unwrap_or("anonymous"),
        &id,
        "email.request",
        "accepted",
    )
    .await?;
    tx.commit().await?;
    start_outbox(app.clone());
    let mut response=Json(json!({"transaction_id":id,"message":"If this address can sign in, a code has been sent. Enter it in this browser."})).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, binding_header(&app, &browser));
    Ok(response)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailFinish {
    code: String,
}
pub async fn email_finish(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    peer: Peer,
    body: Body<EmailFinish>,
) -> Result<Response, Problem> {
    origin(&app, &headers)?;
    limit(&app, "email-finish:global", 120, 60).await?;
    network_limit(&app, &peer, "email-finish", 60, 60).await?;
    let mut tx = begin_write(&app.db).await?;
    let row = txrow(&mut tx, &id).await?;
    if row.provider_id.is_some() {
        return Err(Problem::not_found());
    }
    // A retried registration (for example after a lost response) returns its
    // stable recorded outcome without issuing another user or session family.
    let recorded = row.state()?;
    if recorded.purpose == core::Purpose::Register
        && recorded.phase == core::Phase::Consumed
        && binding(&headers)
            .map(|b| keyed(&app, "browser", b))
            .as_deref()
            == Some(recorded.browser_hash.as_str())
    {
        return Ok(
            Json(json!({"status":row.result,"session_issued":false,"redirect_to":null}))
                .into_response(),
        );
    }
    let matches = body.value.code.len() == 8
        && body.value.code.bytes().all(|b| b.is_ascii_digit())
        && row.code_hash.as_deref().is_some_and(|expected| {
            verify_keyed(
                &app,
                &format!("email-code:{id}"),
                &body.value.code,
                expected,
            )
        });
    let state = current(&mut tx, &app, &row, &headers, Step::Code(matches)).await?;
    save_state(&mut tx, &state, None).await?;
    if !matches {
        audit(&mut tx, "anonymous", &id, "email.verify", "invalid_code").await?;
        tx.commit().await?;
        return Err(problem(core::Error::InvalidCode));
    }
    let mut updated = row;
    updated.state_json = jsonstr(&state);
    let state = current(&mut tx, &app, &updated, &headers, Step::Complete).await?;
    let mut token = None;
    let result = if state.purpose == core::Purpose::Register {
        match super::registration::complete(&mut tx, &app, &updated, &state).await? {
            Ok(super::registration::Completed::Session(result, issued)) => {
                token = Some(issued);
                static_result(result)
            }
            Ok(super::registration::Completed::NoSession(result)) => static_result(result),
            Err(error) => {
                // Verified proof is single-use even when policy refuses it.
                let mut failed = state.clone();
                failed.phase = core::Phase::Failed;
                save_state(&mut tx, &failed, Some(error.code())).await?;
                super::outbox::cancel_owner(&mut tx, &id).await?;
                audit(
                    &mut tx,
                    "anonymous",
                    &id,
                    "registration.verify",
                    error.code(),
                )
                .await?;
                tx.commit().await?;
                return Err(Problem::new(
                    StatusCode::CONFLICT,
                    error.code(),
                    match error {
                        playscale_core::registration::Error::PolicyChanged => {
                            "Registration settings changed. Start a new registration."
                        }
                        playscale_core::registration::Error::Closed => {
                            "This server no longer accepts new registrations."
                        }
                        _ => {
                            "This registration cannot be completed. Sign in instead, or contact the administrator."
                        }
                    },
                ));
            }
        }
    } else if state.purpose == core::Purpose::Enroll {
        let user = access_management::load_user(
            &mut tx,
            state.user_id.as_deref().ok_or_else(unavailable)?,
        )
        .await?
        .ok_or_else(unavailable)?;
        link_identity(
            &mut tx,
            &user,
            &serde_json::from_str::<Device>(
                updated.ceiling_json.as_deref().ok_or_else(unavailable)?,
            )
            .map_err(Problem::internal)?,
            "email",
            None,
            None,
            None,
            updated.address.as_deref(),
        )
        .await?;
        "linked"
    } else {
        let identity = updated.identity_id.as_deref().ok_or_else(unavailable)?;
        let owner: String =
            sqlx::query_scalar("SELECT user_id FROM signin_identities WHERE id=? AND enabled=1")
                .bind(identity)
                .fetch_one(&mut *tx)
                .await?;
        core::link_owner(state.user_id.as_deref(), &owner)?;
        token = Some(issue(&mut tx, &app, identity, state.provider_revision, now()).await?);
        "signed_in"
    };
    save_state(&mut tx, &state, Some(result)).await?;
    super::outbox::cancel_owner(&mut tx, &id).await?;
    audit(
        &mut tx,
        updated.actor_id.as_deref().unwrap_or("anonymous"),
        &id,
        "email.verify",
        result,
    )
    .await?;
    tx.commit().await?;
    let mut response = Json(json!({
        "status": result,
        "session_issued": token.is_some(),
        "redirect_to": if result == "linked" {
            Some("/settings/signin")
        } else if token.is_some() {
            Some("/")
        } else {
            None
        },
    }))
    .into_response();
    if let Some(token) = token {
        response
            .headers_mut()
            .insert(header::SET_COOKIE, session_header(&app, &token));
    }
    Ok(response)
}
/// Durable outbox worker. A lease is committed before I/O; an uncertain send
/// retries the same encrypted payload after its lease, never a new code.
pub fn start_outbox(app: App) {
    tokio::spawn(async move {
        for _ in 0..42 {
            match super::outbox::deliver_mail_once(&app).await {
                Ok(true) => continue,
                Ok(false) => break,
                Err(_) => {
                    tracing::warn!("sign-in mail delivery deferred");
                    break;
                }
            }
        }
    });
}
pub async fn mail_worker(
    app: App,
    stop: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
    loop {
        tokio::select! {_ = stop.cancelled()=>return Ok(()),_ = interval.tick()=>{for _ in 0..60{match super::outbox::deliver_mail_once(&app).await{Ok(true)=>continue,Ok(false)=>break,Err(_)=>{tracing::warn!("sign-in mail delivery deferred");break}}}}}
    }
}

/// Results are a small fixed vocabulary; keep them `'static` for `save_state`.
fn static_result(result: String) -> &'static str {
    match result.as_str() {
        "pending_approval" => "pending_approval",
        "invitation_accepted" => "invitation_accepted",
        "account_active" => "account_active",
        _ => "failed",
    }
}
