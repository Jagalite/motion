//! Administrative snapshots and mutations. Authorization, revision fences,
//! credential revocation and audit writes share the reserved SQLite writer.
//! External provider/mail tests deliberately run outside that writer and may
//! publish results only after rechecking the actor and tested revision.
use super::super::{PageQuery, identity::tagged};
use super::*;
use crate::signin_protocol::{self, Destination};
use sqlx::sqlite::SqliteRow;

fn bounded(value: &str, min: usize, max: usize) -> Result<(), Problem> {
    if value.len() < min || value.len() > max || value.chars().any(char::is_control) {
        return Err(Problem::invalid(
            "invalid_field",
            "A field is empty, too long, or contains control characters",
        ));
    }
    Ok(())
}

fn check_revision(actual: i64, expected: u64) -> Result<(), Problem> {
    if actual < 1 || actual as u64 != expected {
        return Err(AccessError::StaleRevision.into());
    }
    Ok(())
}

fn next_revision(actual: i64) -> Result<i64, Problem> {
    actual.checked_add(1).ok_or_else(unavailable)
}

fn paged(mut items: Vec<Value>, limit: u32) -> Json<Value> {
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next = more
        .then(|| {
            items.last().and_then(|v| v.get("id")).map(|id| {
                id.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string())
            })
        })
        .flatten();
    Json(json!({"items":items,"next_cursor":next}))
}

fn user_value(user: &User) -> Value {
    json!({"id":user.id,"name":user.name,"revision":user.revision.to_string(),"disabled":user.disabled,"profile_ids":user.grant.profile_ids,"permissions":user.grant.permissions,"policy":user.policy})
}

async fn owner_or_admin(
    c: &mut sqlx::SqliteConnection,
    caller: &Caller,
    owner: &str,
) -> Result<(), Problem> {
    let principal = caller.reauthenticate(c).await?;
    if principal.allows(Permission::SystemAdmin)
        || owned_user(c, caller)
            .await?
            .is_some_and(|user| user.id == owner && !user.disabled)
    {
        Ok(())
    } else {
        Err(Problem::not_found())
    }
}

#[derive(Deserialize, Default)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretUpdate {
    #[default]
    Retain,
    Replace {
        value: String,
    },
    Clear,
}

impl SecretUpdate {
    fn changed(&self) -> bool {
        !matches!(self, Self::Retain)
    }
    fn apply(
        &self,
        app: &App,
        aad: &str,
        existing: Option<String>,
    ) -> Result<Option<String>, Problem> {
        match self {
            Self::Retain => Ok(existing),
            Self::Clear => Ok(None),
            Self::Replace { value } => {
                bounded(value, 1, 8192)?;
                if matches!(
                    value.as_str(),
                    "[redacted]" | "<redacted>" | "********" | "••••••••"
                ) {
                    return Err(Problem::invalid(
                        "invalid_secret",
                        "Enter the replacement secret itself",
                    ));
                }
                Ok(Some(seal(app, aad, value)?))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderInput {
    name: String,
    issuer: String,
    client_id: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    destinations: Vec<Destination>,
    #[serde(default)]
    client_secret: SecretUpdate,
}

fn validate_provider(input: &ProviderInput) -> Result<(), Problem> {
    bounded(&input.name, 1, 100)?;
    bounded(&input.client_id, 1, 512)?;
    bounded(&input.issuer, 1, 2048)?;
    let issuer = url::Url::parse(&input.issuer)
        .map_err(|_| Problem::invalid("invalid_issuer", "Enter an absolute issuer URL"))?;
    if !matches!(issuer.scheme(), "http" | "https")
        || issuer.host().is_none()
        || !issuer.username().is_empty()
        || issuer.password().is_some()
        || issuer.query().is_some()
        || issuer.fragment().is_some()
    {
        return Err(Problem::invalid(
            "invalid_issuer",
            "Issuer must be an HTTPS URL without credentials, query or fragment",
        ));
    }
    if input.destinations.len() > 8 {
        return Err(Problem::invalid(
            "invalid_destinations",
            "At most eight exact destinations are supported",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for destination in &input.destinations {
        let origin = url::Url::parse(&destination.origin).map_err(|_| {
            Problem::invalid(
                "invalid_destinations",
                "Enter an absolute destination origin",
            )
        })?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !seen.insert(origin.origin().ascii_serialization())
        {
            return Err(Problem::invalid(
                "invalid_destinations",
                "Destinations must be distinct exact origins",
            ));
        }
        if origin.scheme() == "http" {
            let literal = origin
                .host_str()
                .and_then(|s| s.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok());
            if !literal.is_some_and(|ip| ip.is_loopback() && destination.address == Some(ip)) {
                return Err(Problem::invalid(
                    "invalid_destinations",
                    "HTTP requires an exact literal loopback address pin",
                ));
            }
        }
    }
    if issuer.scheme() == "http"
        && !input.destinations.iter().any(|destination| {
            url::Url::parse(&destination.origin).is_ok_and(|url| url.origin() == issuer.origin())
                && destination
                    .address
                    .is_some_and(|address| address.is_loopback())
        })
    {
        return Err(Problem::invalid(
            "invalid_issuer",
            "HTTP issuer requires its exact loopback destination pin",
        ));
    }
    Ok(())
}

fn provider_value(row: &SqliteRow, origin: Option<&str>) -> Result<Value, Problem> {
    let destinations: Value =
        serde_json::from_str(row.get("destinations")).map_err(Problem::internal)?;
    Ok(json!({
        "id":row.get::<String,_>("id"),"revision":row.get::<i64,_>("revision").to_string(),
        "name":row.get::<String,_>("name"),"issuer":row.get::<String,_>("issuer"),
        "client_id":row.get::<String,_>("client_id"),"enabled":row.get::<bool,_>("enabled"),
        "has_secret":row.get::<Option<String>,_>("secret").is_some(),"destinations":destinations,
        "callback_url":origin.map(|origin|format!("{origin}/api/v2/auth/oidc/callback")),
        "last_test_at":row.get::<Option<i64>,_>("test_at"),"last_test_ok":row.get::<Option<bool>,_>("test_ok"),
        "last_login_at":row.get::<Option<i64>,_>("login_test_at")
    }))
}

pub async fn list_providers(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let origin: Option<String> = sqlx::query_scalar("SELECT public_origin FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    let rows = sqlx::query("SELECT * FROM signin_providers WHERE id>? ORDER BY id LIMIT ?")
        .bind(after)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await?;
    Ok(paged(
        rows.iter()
            .map(|row| provider_value(row, origin.as_deref()))
            .collect::<Result<_, _>>()?,
        limit,
    ))
}

pub async fn create_provider(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<ProviderInput>,
) -> Result<Response, Problem> {
    validate_provider(&body.value)?;
    if body.value.enabled {
        return Err(Problem::invalid(
            "provider_test_required",
            "Save and test a disabled provider before enabling it",
        ));
    }
    let key = super::super::idempotency_key(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let scope = super::super::Scope {
        principal: &principal.id,
        operation: "createSigninProvider",
        target: "",
        key: &key,
    };
    let prior = scope.load(&mut tx).await?;
    if playscale_core::access::idempotency(
        prior.as_ref().map(|stored| &stored.record),
        &body.digest,
        now(),
    )? == playscale_core::access::Idempotent::Replay
    {
        let stored = prior.unwrap();
        let value = stored.body.clone().unwrap_or_default();
        return Ok(super::super::identity::replayed(stored, value));
    }
    require_fresh(&mut tx, &caller, &headers, "provider.write", "new").await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM signin_providers")
        .fetch_one(&mut *tx)
        .await?;
    if count >= 32 {
        return Err(Problem::invalid(
            "provider_limit",
            "At most 32 providers are supported",
        ));
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM signin_providers WHERE issuer=?)")
            .bind(&body.value.issuer)
            .fetch_one(&mut *tx)
            .await?;
    if exists {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "issuer_exists",
            "This exact issuer already has a provider",
        ));
    }
    let id = new_id();
    let input = body.value;
    let secret = input
        .client_secret
        .apply(&app, &format!("provider:{id}"), None)?;
    sqlx::query("INSERT INTO signin_providers(id,revision,name,issuer,client_id,secret,destinations,enabled) VALUES(?,1,?,?,?,?,?,0)").bind(&id).bind(input.name).bind(input.issuer).bind(input.client_id).bind(secret).bind(jsonstr(&input.destinations)).execute(&mut *tx).await?;
    audit(&mut tx, &principal.id, &id, "provider.create", "ok").await?;
    let row = sqlx::query("SELECT * FROM signin_providers WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    let origin: Option<String> = sqlx::query_scalar("SELECT public_origin FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    let value = provider_value(&row, origin.as_deref())?;
    scope
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &value, 1))
}

/// Last-method checks use independent paired credentials or enabled identity
/// mappings. Browser-family parents never masquerade as independent pairing.
async fn preserve_methods(
    c: &mut sqlx::SqliteConnection,
    caller: &Caller,
    provider_id: Option<&str>,
    identity_id: Option<&str>,
    email: bool,
) -> Result<(), Problem> {
    if caller.is_local_operator() {
        return core::preserve_method(0, true).map_err(problem);
    }
    let affected:Vec<String>=sqlx::query_scalar("SELECT DISTINCT i.user_id FROM signin_identities i JOIN access_users u ON u.id=i.user_id WHERE i.enabled=1 AND u.disabled=0 AND ((? IS NOT NULL AND i.provider_id=?) OR (? IS NOT NULL AND i.id=?) OR (? AND i.kind='email'))").bind(provider_id).bind(provider_id).bind(identity_id).bind(identity_id).bind(email).fetch_all(&mut *c).await?;
    for owner in affected {
        let independent:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM devices d JOIN credentials c ON c.device_id=d.id AND c.kind='device' AND c.generation=d.generation WHERE d.user_id=? AND d.revoked=0 AND c.expires_at>? AND NOT EXISTS(SELECT 1 FROM signin_families f WHERE f.device_id=d.id))").bind(&owner).bind(now()).fetch_one(&mut *c).await?;
        let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM signin_identities i LEFT JOIN signin_providers p ON p.id=i.provider_id CROSS JOIN signin_settings s WHERE i.user_id=? AND i.enabled=1 AND (? IS NULL OR i.id!=?) AND (? IS NULL OR i.provider_id IS NULL OR i.provider_id!=?) AND NOT (? AND i.kind='email') AND ((i.kind='oidc' AND p.enabled=1) OR (i.kind='email' AND json_extract(s.email_json,'$.enabled')=1))").bind(&owner).bind(identity_id).bind(identity_id).bind(provider_id).bind(provider_id).bind(email).fetch_one(&mut *c).await?;
        core::preserve_method(remaining as usize + usize::from(independent), false)?;
    }
    Ok(())
}

async fn method_families(
    c: &mut sqlx::SqliteConnection,
    provider_id: Option<&str>,
    identity_id: Option<&str>,
    email: bool,
) -> Result<Vec<String>, Problem> {
    Ok(sqlx::query_scalar("SELECT f.id FROM signin_families f JOIN signin_identities i ON i.id=f.identity_id WHERE f.revoked=0 AND ((? IS NOT NULL AND i.provider_id=?) OR (? IS NOT NULL AND i.id=?) OR (? AND i.kind='email'))").bind(provider_id).bind(provider_id).bind(identity_id).bind(identity_id).bind(email).fetch_all(c).await?)
}

async fn cancel_transactions(
    c: &mut sqlx::SqliteConnection,
    provider_id: Option<&str>,
    identity_id: Option<&str>,
    email: bool,
) -> Result<(), Problem> {
    sqlx::query("UPDATE signin_transactions SET state_json=json_set(state_json,'$.phase','failed'),verifier=NULL,nonce=NULL,code_hash=NULL,result='method_changed' WHERE (? IS NOT NULL AND provider_id=?) OR (? IS NOT NULL AND identity_id=?) OR (? AND address IS NOT NULL)").bind(provider_id).bind(provider_id).bind(identity_id).bind(identity_id).bind(email).execute(&mut *c).await?;
    sqlx::query("UPDATE signin_outbox SET payload=NULL,phase='cancelled' WHERE transaction_id IN (SELECT id FROM signin_transactions WHERE result='method_changed')").execute(c).await?;
    Ok(())
}

pub async fn replace_provider(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<ProviderInput>,
) -> Result<Response, Problem> {
    validate_provider(&body.value)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = sqlx::query("SELECT * FROM signin_providers WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    let revision: i64 = row.get("revision");
    check_revision(revision, expected)?;
    require_fresh(&mut tx, &caller, &headers, "provider.write", &id).await?;
    let input = body.value;
    let destinations: Vec<Destination> =
        serde_json::from_str(row.get("destinations")).map_err(Problem::internal)?;
    let changed = input.name != row.get::<String, _>("name")
        || input.issuer != row.get::<String, _>("issuer")
        || input.client_id != row.get::<String, _>("client_id")
        || input.destinations != destinations
        || input.client_secret.changed();
    if input.issuer != row.get::<String, _>("issuer") {
        let linked: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM signin_identities WHERE provider_id=?)",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
        if linked {
            return Err(Problem::invalid(
                "issuer_has_identities",
                "Create a new provider to change an issuer with linked identities",
            ));
        }
        let duplicate: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM signin_providers WHERE issuer=? AND id!=?)",
        )
        .bind(&input.issuer)
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
        if duplicate {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "issuer_exists",
                "This exact issuer already has a provider",
            ));
        }
    }
    if input.enabled
        && (changed
            || row.get::<Option<bool>, _>("test_ok") != Some(true)
            || row.get::<Option<i64>, _>("login_test_at").is_none()
            || hold(&mut tx).await?)
    {
        return Err(Problem::invalid(
            "provider_test_required",
            "Save a disabled draft, pass configuration and browser tests, then enable it",
        ));
    }
    if changed || input.enabled != row.get::<bool, _>("enabled") {
        if !input.enabled && row.get::<bool, _>("enabled") {
            preserve_methods(&mut tx, &caller, Some(&id), None, false).await?;
        }
        let families = method_families(&mut tx, Some(&id), None, false).await?;
        revoke_families(&mut tx, &families, &caller.principal.id).await?;
        cancel_transactions(&mut tx, Some(&id), None, false).await?;
        let secret =
            input
                .client_secret
                .apply(&app, &format!("provider:{id}"), row.get("secret"))?;
        sqlx::query("UPDATE signin_providers SET revision=?,name=?,issuer=?,client_id=?,destinations=?,secret=?,enabled=?,test_at=CASE WHEN ? THEN NULL ELSE test_at END,test_ok=CASE WHEN ? THEN NULL ELSE test_ok END,login_test_at=CASE WHEN ? THEN NULL ELSE login_test_at END WHERE id=?").bind(next_revision(revision)?).bind(input.name).bind(input.issuer).bind(input.client_id).bind(jsonstr(&input.destinations)).bind(secret).bind(input.enabled).bind(changed).bind(changed).bind(changed).bind(&id).execute(&mut *tx).await?;
        audit(&mut tx, &caller.principal.id, &id, "provider.write", "ok").await?;
    }
    let row = sqlx::query("SELECT * FROM signin_providers WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    let origin: Option<String> = sqlx::query_scalar("SELECT public_origin FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    let value = provider_value(&row, origin.as_deref())?;
    let revision: i64 = row.get("revision");
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &value, revision as u64))
}

pub async fn test_provider(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, Problem> {
    limit(
        &app,
        &format!("provider-test:{}:{id}", caller.principal.id),
        5,
        60,
    )
    .await?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    require_fresh(&mut tx, &caller, &headers, "provider.test", &id).await?;
    let (revision, _, config) = provider(&mut tx, &app, &id).await?;
    let tested_epoch = epoch(&mut tx).await?;
    audit(
        &mut tx,
        &caller.principal.id,
        &id,
        "provider.test",
        "started",
    )
    .await?;
    tx.commit().await?;
    let success = signin_protocol::check_oidc(&config).await.is_ok();
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let current: i64 = sqlx::query_scalar("SELECT revision FROM signin_providers WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    check_revision(current, revision as u64)?;
    if tested_epoch != epoch(&mut tx).await? {
        return Err(problem(core::Error::Stale));
    }
    let at = now();
    sqlx::query("UPDATE signin_providers SET test_at=?,test_ok=? WHERE id=?")
        .bind(at)
        .bind(success)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        &caller.principal.id,
        &id,
        "provider.test",
        if success { "ok" } else { "failed" },
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"ok":success,"tested_at":at,"revision":revision.to_string()}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsInput {
    public_origin: String,
}

pub async fn replace_settings(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<SettingsInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let value = body.value.public_origin;
    let origin = url::Url::parse(&value)
        .map_err(|_| Problem::invalid("invalid_origin", "Enter the configured Motion origin"))?;
    let loopback = origin
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback());
    if value != app.origin.as_str()
        || value != origin.origin().ascii_serialization()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !(origin.scheme() == "https" || (origin.scheme() == "http" && loopback))
    {
        return Err(Problem::invalid(
            "invalid_origin",
            "Use the server's exact configured HTTPS origin (literal loopback HTTP is permitted for local qualification)",
        ));
    }
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let (revision, previous): (i64, Option<String>) =
        sqlx::query_as("SELECT revision,public_origin FROM signin_settings")
            .fetch_one(&mut *tx)
            .await?;
    check_revision(revision, expected)?;
    require_fresh(&mut tx, &caller, &headers, "settings.write", "settings").await?;
    let next = if previous.as_deref() != Some(value.as_str()) {
        let families: Vec<String> =
            sqlx::query_scalar("SELECT id FROM signin_families WHERE revoked=0")
                .fetch_all(&mut *tx)
                .await?;
        revoke_families(&mut tx, &families, &caller.principal.id).await?;
        sqlx::query("UPDATE signin_providers SET revision=revision+1,enabled=0,test_at=NULL,test_ok=NULL,login_test_at=NULL").execute(&mut *tx).await?;
        sqlx::query("UPDATE signin_transactions SET state_json=json_set(state_json,'$.phase','failed'),verifier=NULL,nonce=NULL,code_hash=NULL,result='origin_changed'").execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE signin_outbox SET payload=NULL,phase='cancelled' WHERE phase='pending'",
        )
        .execute(&mut *tx)
        .await?;
        let next = next_revision(revision)?;
        sqlx::query("UPDATE signin_settings SET public_origin=?,revision=?")
            .bind(&value)
            .bind(next)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            &caller.principal.id,
            "settings",
            "settings.write",
            "ok",
        )
        .await?;
        next
    } else {
        revision
    };
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        &json!({"public_origin":value,"revision":next.to_string()}),
        next as u64,
    ))
}

pub async fn review_restore(
    State(app): State<App>,
    caller: Caller,
) -> Result<Json<Value>, Problem> {
    let mut tx = begin_write(&app.db).await?;
    caller.reauthenticate(&mut tx).await?;
    if !caller.is_local_operator() {
        return Err(AccessError::Forbidden(Permission::SystemAdmin).into());
    }
    let current = epoch(&mut tx).await?;
    sqlx::query("UPDATE signin_settings SET reviewed_epoch=?,revision=revision+1")
        .bind(&current)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE signin_transactions SET state_json=json_set(state_json,'$.phase','failed'),nonce=NULL,verifier=NULL,code_hash=NULL,result='restore_reviewed'").execute(&mut *tx).await?;
    sqlx::query("UPDATE signin_outbox SET payload=NULL,phase='cancelled' WHERE phase='pending'")
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        &caller.principal.id,
        "settings",
        "restore.review",
        "ok",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"restored_hold":false})))
}

fn email_value(row: &SqliteRow) -> Result<Value, Problem> {
    let config: EmailConfig =
        serde_json::from_str(row.get("email_json")).map_err(Problem::internal)?;
    Ok(
        json!({"revision":row.get::<i64,_>("email_revision").to_string(),"enabled":config.enabled,"host":config.host,"port":if config.port==0{587}else{config.port},"username":config.username,"from":config.from,"loopback_test":config.loopback_test,"has_password":row.get::<Option<String>,_>("email_secret").is_some(),"last_test_at":row.get::<Option<i64>,_>("email_test_at"),"last_test_ok":row.get::<Option<bool>,_>("email_test_ok")}),
    )
}

pub async fn get_email(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = sqlx::query("SELECT * FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    Ok(tagged(
        StatusCode::OK,
        &email_value(&row)?,
        row.get::<i64, _>("email_revision") as u64,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailInput {
    enabled: bool,
    host: String,
    port: u16,
    username: Option<String>,
    from: String,
    #[serde(default)]
    loopback_test: bool,
    #[serde(default)]
    password: SecretUpdate,
}

pub async fn replace_email(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<EmailInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let input = body.value;
    bounded(&input.host, 1, 253)?;
    bounded(&input.from, 1, 320)?;
    if input.port == 0
        || input.host.contains(['/', '@', '?', '#', '\\'])
        || input.from.parse::<lettre::message::Mailbox>().is_err()
        || (input.loopback_test
            && !input
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback()))
    {
        return Err(Problem::invalid(
            "invalid_email_configuration",
            "Use a valid SMTP host, port, sender and TLS configuration",
        ));
    }
    let config = EmailConfig {
        enabled: input.enabled,
        host: input.host,
        port: input.port,
        username: input.username.filter(|value| !value.is_empty()),
        from: input.from,
        loopback_test: input.loopback_test,
    };
    if let Some(username) = &config.username {
        bounded(username, 1, 512)?;
    }
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = sqlx::query("SELECT * FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    let revision: i64 = row.get("email_revision");
    check_revision(revision, expected)?;
    require_fresh(&mut tx, &caller, &headers, "email.write", "email").await?;
    let old: EmailConfig =
        serde_json::from_str(row.get("email_json")).map_err(Problem::internal)?;
    let changed = config.host != old.host
        || config.port != old.port
        || config.username != old.username
        || config.from != old.from
        || config.loopback_test != old.loopback_test
        || input.password.changed();
    let secret = input
        .password
        .apply(&app, "mail-secret", row.get("email_secret"))?;
    if config.username.is_some() != secret.is_some() {
        return Err(Problem::invalid(
            "invalid_email_configuration",
            "Configure SMTP username and password together",
        ));
    }
    if config.enabled
        && (changed
            || row.get::<Option<bool>, _>("email_test_ok") != Some(true)
            || hold(&mut tx).await?)
    {
        return Err(Problem::invalid(
            "email_test_required",
            "Save disabled delivery settings and successfully send an explicit test before enabling email sign-in",
        ));
    }
    if changed || config.enabled != old.enabled {
        if old.enabled && !config.enabled {
            preserve_methods(&mut tx, &caller, None, None, true).await?;
        }
        let families = method_families(&mut tx, None, None, true).await?;
        revoke_families(&mut tx, &families, &caller.principal.id).await?;
        cancel_transactions(&mut tx, None, None, true).await?;
        sqlx::query("UPDATE signin_settings SET email_json=?,email_secret=?,email_revision=?,revision=revision+1,email_test_at=CASE WHEN ? THEN NULL ELSE email_test_at END,email_test_ok=CASE WHEN ? THEN NULL ELSE email_test_ok END").bind(jsonstr(&config)).bind(secret).bind(next_revision(revision)?).bind(changed).bind(changed).execute(&mut *tx).await?;
        audit(&mut tx, &caller.principal.id, "email", "email.write", "ok").await?;
    }
    let row = sqlx::query("SELECT * FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    let value = email_value(&row)?;
    let revision: i64 = row.get("email_revision");
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &value, revision as u64))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailTestInput {
    address: String,
}

pub async fn test_email(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<EmailTestInput>,
) -> Result<Json<Value>, Problem> {
    let address = core::canonical_email(&body.value.address)
        .ok_or_else(|| Problem::invalid("invalid_email", "Enter the explicit test destination"))?;
    limit(&app, &format!("email-test:{}", caller.principal.id), 5, 600).await?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    require_fresh(&mut tx, &caller, &headers, "email.test", "email").await?;
    let (_, revision, config) = mail(&mut tx, &app).await?;
    let tested_epoch = epoch(&mut tx).await?;
    audit(
        &mut tx,
        &caller.principal.id,
        "email",
        "email.test",
        "started",
    )
    .await?;
    tx.commit().await?;
    // This static demonstration code is never installed as an authentication
    // challenge and therefore cannot authorize a session even if copied.
    let success = signin_protocol::send_code(&config, &address, "00000000")
        .await
        .is_ok();
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let current: i64 = sqlx::query_scalar("SELECT email_revision FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    check_revision(current, revision as u64)?;
    if tested_epoch != epoch(&mut tx).await? {
        return Err(problem(core::Error::Stale));
    }
    let at = now();
    sqlx::query("UPDATE signin_settings SET email_test_at=?,email_test_ok=?")
        .bind(at)
        .bind(success)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        &caller.principal.id,
        "email",
        "email.test",
        if success { "accepted" } else { "failed" },
    )
    .await?;
    tx.commit().await?;
    if !success {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "email_delivery_failed",
            "The test message was not accepted; check the delivery configuration",
        ));
    }
    Ok(Json(
        json!({"accepted":true,"tested_at":at,"revision":revision.to_string()}),
    ))
}

pub async fn overview(State(app): State<App>, caller: Caller) -> Result<Json<Value>, Problem> {
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = sqlx::query("SELECT * FROM signin_settings")
        .fetch_one(&mut *tx)
        .await?;
    let origin: Option<String> = row.get("public_origin");
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM signin_requests WHERE status='pending' AND expires_at>?",
    )
    .bind(now())
    .fetch_one(&mut *tx)
    .await?;
    let ids:Vec<String>=sqlx::query_scalar("SELECT DISTINCT d.id FROM devices d JOIN credentials c ON c.device_id=d.id AND c.kind='device' AND c.generation=d.generation WHERE d.revoked=0 AND c.expires_at>? AND EXISTS(SELECT 1 FROM json_each(d.permissions) WHERE value='system:admin')").bind(now()).fetch_all(&mut *tx).await?;
    let mut admins = 0;
    for id in ids {
        if let Some(row) = auth::load_device(&mut tx, &id).await?
            && access_admin::usable_admin(&row.device, true, now())
            && family_valid(&mut tx, &id).await?
        {
            admins += 1;
        }
    }
    let mut errors = Vec::new();
    if origin.as_deref() != Some(app.origin.as_str()) {
        errors.push("Public sign-in origin must match the configured server origin");
    }
    Ok(Json(
        json!({"revision":row.get::<i64,_>("revision").to_string(),"public_origin":origin,"usable_administrators":admins,"pending_requests":pending,"restored_hold":hold(&mut tx).await?,"configuration_errors":errors}),
    ))
}

fn request_value(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"revision":row.get::<i64,_>("revision").to_string(),"provider_id":row.get::<String,_>("provider_id"),"issuer":row.get::<String,_>("issuer"),"subject":row.get::<String,_>("subject"),"email":row.get::<Option<String>,_>("email"),"name":row.get::<Option<String>,_>("name"),"status":if row.get::<String,_>("status")=="pending" && row.get::<i64,_>("expires_at")<=now(){"expired".to_owned()}else{row.get("status")},"created_at":row.get::<i64,_>("created_at"),"expires_at":row.get::<i64,_>("expires_at"),"user_id":row.get::<Option<String>,_>("user_id")})
}

pub async fn list_requests(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let rows = sqlx::query("SELECT * FROM signin_requests WHERE id>? ORDER BY id LIMIT ?")
        .bind(after)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await?;
    Ok(paged(rows.iter().map(request_value).collect(), limit))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestApproval {
    user_id: String,
    user_revision: String,
    recovery: bool,
}

pub async fn approve_request(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<RequestApproval>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let input = body.value;
    if !input.recovery {
        return Err(Problem::invalid(
            "recovery_confirmation_required",
            "Explicitly authorize the displayed issuer and subject for the selected user",
        ));
    }
    let user_revision = input.user_revision.parse::<u64>().map_err(|_| {
        Problem::invalid("user_revision_required", "Select a current user revision")
    })?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    if hold(&mut tx).await? {
        return Err(unavailable());
    }
    let request = sqlx::query("SELECT * FROM signin_requests WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    let revision: i64 = request.get("revision");
    check_revision(revision, expected)?;
    if request.get::<String, _>("status") != "pending"
        || request.get::<i64, _>("expires_at") <= now()
    {
        return Err(problem(core::Error::Expired));
    }
    let provider_id: String = request.get("provider_id");
    let provider = sqlx::query("SELECT issuer,revision,enabled FROM signin_providers WHERE id=?")
        .bind(&provider_id)
        .fetch_one(&mut *tx)
        .await?;
    if !provider.get::<bool, _>("enabled")
        || provider.get::<i64, _>("revision") != request.get::<i64, _>("provider_revision")
        || provider.get::<String, _>("issuer") != request.get::<String, _>("issuer")
    {
        return Err(problem(core::Error::Stale));
    }
    let user = access_management::load_user(&mut tx, &input.user_id)
        .await?
        .ok_or_else(Problem::not_found)?;
    access_admin::authorize_grant(&user, user_revision, &user.grant, &user.policy)?;
    require_fresh(&mut tx, &caller, &headers, "request.approve", &id).await?;
    let issuer: String = request.get("issuer");
    let subject: String = request.get("subject");
    let identity = super::flows::link_identity(
        &mut tx,
        &user,
        &Device {
            policy: user.policy.clone(),
            ..Device::approved("reviewed-recovery".into(), user.grant.clone())
        },
        "oidc",
        Some(&provider_id),
        Some(&issuer),
        Some(&subject),
        None,
    )
    .await?;
    let next = next_revision(revision)?;
    sqlx::query("UPDATE signin_requests SET status='approved',user_id=?,revision=? WHERE id=?")
        .bind(&user.id)
        .bind(next)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        &caller.principal.id,
        &id,
        "request.approve",
        "recovery_authorized",
    )
    .await?;
    audit(
        &mut tx,
        &caller.principal.id,
        &identity,
        "identity.recovery",
        "linked",
    )
    .await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        &json!({"id":id,"revision":next.to_string(),"status":"approved","user_id":user.id,"fresh_login_required":true}),
        next as u64,
    ))
}

pub async fn deny_request(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = sqlx::query("SELECT revision,status,expires_at FROM signin_requests WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    let revision: i64 = row.get("revision");
    check_revision(revision, expected)?;
    if row.get::<String, _>("status") != "pending" || row.get::<i64, _>("expires_at") <= now() {
        return Err(problem(core::Error::Expired));
    }
    let next = next_revision(revision)?;
    sqlx::query("UPDATE signin_requests SET status='denied',revision=? WHERE id=?")
        .bind(next)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, &caller.principal.id, &id, "request.deny", "ok").await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        &json!({"id":id,"revision":next.to_string(),"status":"denied"}),
        next as u64,
    ))
}

const PROOF_ACTIONS: &[&str] = &[
    "provider.write",
    "provider.test",
    "email.write",
    "email.test",
    "identity.unlink",
    "request.approve",
    "user.admin",
    "device.assign",
    "device.policy",
    "settings.write",
    "identity.link",
    "pairing.approve",
    "invitation.create",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofInput {
    action: String,
    target: String,
}

pub async fn create_proof(
    State(app): State<App>,
    caller: Caller,
    body: Body<ProofInput>,
) -> Result<Json<Value>, Problem> {
    let input = body.value;
    bounded(&input.target, 1, 256)?;
    if !PROOF_ACTIONS.contains(&input.action.as_str()) {
        return Err(Problem::invalid(
            "invalid_action",
            "This action cannot request a proof",
        ));
    }
    let hash = caller
        .credential()
        .map(|entry| entry.0.to_owned())
        .ok_or_else(|| {
            Problem::invalid(
                "credential_required",
                "A device credential is required to request another device's approval",
            )
        })?;
    limit(
        &app,
        &format!("proof-create:{}", caller.principal.id),
        5,
        600,
    )
    .await?;
    limit(&app, "proof-create:deployment", 100, 600).await?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller.reauthenticate(&mut tx).await?;
    if !matches!(input.action.as_str(), "identity.link" | "identity.unlink")
        && !principal.allows(Permission::SystemAdmin)
    {
        return Err(AccessError::Forbidden(Permission::SystemAdmin).into());
    }
    if !principal.allows(Permission::SystemAdmin) && owned_user(&mut tx, &caller).await?.is_none() {
        return Err(Problem::not_found());
    }
    if input.action == "identity.unlink" {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT user_id FROM signin_identities WHERE id=?")
                .bind(&input.target)
                .fetch_optional(&mut *tx)
                .await?;
        owner_or_admin(
            &mut tx,
            &caller,
            owner.as_deref().ok_or_else(Problem::not_found)?,
        )
        .await?;
    }
    sqlx::query("DELETE FROM signin_proofs WHERE expires_at<?")
        .bind(now() - 86400)
        .execute(&mut *tx)
        .await?;
    let id = new_id();
    let code = auth::nonce()[..8].to_ascii_uppercase();
    let at = now();
    let expires = at + core::FRESH_SECONDS;
    let current_epoch = epoch(&mut tx).await?;
    sqlx::query("INSERT INTO signin_proofs(id,actor_id,actor_credential_hash,action,target,epoch,code_hash,expires_at,created_at) VALUES(?,?,?,?,?,?,?,?,?)").bind(&id).bind(&principal.id).bind(hash).bind(&input.action).bind(&input.target).bind(current_epoch).bind(keyed(&app,"proof-code",&format!("{id}:{code}"))).bind(expires).bind(at).execute(&mut *tx).await?;
    audit(
        &mut tx,
        &principal.id,
        &input.target,
        "proof.request",
        "pending",
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"id":id,"code":code,"status":"pending","action":input.action,"target":input.target,"expires_at":expires}),
    ))
}

fn proof_value(row: &SqliteRow, current_epoch: &str) -> Value {
    let status = if row.get::<bool, _>("consumed") {
        "consumed"
    } else if row.get::<String, _>("epoch") != current_epoch {
        "invalidated"
    } else if row.get::<i64, _>("expires_at") <= now() {
        "expired"
    } else if row.get::<Option<String>, _>("approved_by").is_some() {
        "approved"
    } else {
        "pending"
    };
    json!({"id":row.get::<String,_>("id"),"actor":row.get::<String,_>("actor_id"),"action":row.get::<String,_>("action"),"target":row.get::<String,_>("target"),"status":status,"expires_at":row.get::<i64,_>("expires_at"),"created_at":row.get::<i64,_>("created_at")})
}

pub async fn get_proof(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, Problem> {
    let mut tx = app.db.begin().await?;
    caller.reauthenticate(&mut tx).await?;
    let row = sqlx::query("SELECT * FROM signin_proofs WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    if row.get::<String, _>("actor_id") != caller.principal.id
        || row
            .get::<Option<String>, _>("actor_credential_hash")
            .as_deref()
            != caller.credential().map(|entry| entry.0)
    {
        return Err(Problem::not_found());
    }
    let mut value = proof_value(&row, &epoch(&mut tx).await?);
    if let Some(approver) = row.get::<Option<String>, _>("approved_by")
        && actor_valid(
            &mut tx,
            &approver,
            row.get::<Option<String>, _>("approver_credential_hash")
                .as_deref(),
        )
        .await
        .is_err()
    {
        value["status"] = json!("invalidated");
    }
    Ok(Json(value))
}

pub async fn list_proofs(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let current_epoch = epoch(&mut tx).await?;
    let rows=sqlx::query("SELECT * FROM signin_proofs WHERE id>? AND consumed=0 AND expires_at>? AND epoch=? ORDER BY id LIMIT ?").bind(after).bind(now()).bind(&current_epoch).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    Ok(paged(
        rows.iter()
            .map(|row| proof_value(row, &current_epoch))
            .collect(),
        limit,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofApproval {
    code: String,
}

pub async fn approve_proof(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    body: Body<ProofApproval>,
) -> Result<Json<Value>, Problem> {
    limit(&app, &format!("proof-approve:{id}"), 5, 300).await?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller.reauthenticate(&mut tx).await?;
    let row = sqlx::query("SELECT * FROM signin_proofs WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    let actor: String = row.get("actor_id");
    let actor_hash: Option<String> = row.get("actor_credential_hash");
    if actor == principal.id {
        return Err(Problem::invalid(
            "distinct_device_required",
            "Approve from a different authorized device or the local operator",
        ));
    }
    actor_valid(&mut tx, &actor, actor_hash.as_deref()).await?;
    let requester_owner: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM devices WHERE id=?")
            .bind(&actor)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
    let approver_owner = owned_user(&mut tx, &caller).await?;
    let same_user = requester_owner.as_ref().is_some_and(|id| {
        approver_owner
            .as_ref()
            .is_some_and(|user| !user.disabled && &user.id == id)
    });
    if !core::can_approve_proof(
        &actor,
        &principal.id,
        principal.allows(Permission::SystemAdmin),
        same_user,
    ) {
        return Err(Problem::not_found());
    }
    if !caller.is_local_operator() && caller.credential().is_none() {
        return Err(Problem::not_found());
    }
    if row.get::<bool, _>("consumed")
        || row.get::<Option<String>, _>("approved_by").is_some()
        || row.get::<i64, _>("expires_at") <= now()
        || row.get::<String, _>("epoch") != epoch(&mut tx).await?
    {
        return Err(problem(core::Error::Expired));
    }
    let expected: String = row.get("code_hash");
    if !verify_keyed(
        &app,
        "proof-code",
        &format!("{id}:{}", body.value.code.trim().to_ascii_uppercase()),
        &expected,
    ) {
        audit(&mut tx, &principal.id, &id, "proof.approve", "invalid_code").await?;
        tx.commit().await?;
        return Err(problem(core::Error::InvalidCode));
    }
    sqlx::query("UPDATE signin_proofs SET approved_by=?,approver_credential_hash=? WHERE id=?")
        .bind(&principal.id)
        .bind(caller.credential().map(|entry| entry.0))
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, &principal.id, &id, "proof.approve", "ok").await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"status":"approved"})))
}

fn identity_value(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"revision":row.get::<i64,_>("revision").to_string(),"user_id":row.get::<String,_>("user_id"),"kind":row.get::<String,_>("kind"),"provider_id":row.get::<Option<String>,_>("provider_id"),"provider_name":row.get::<Option<String>,_>("provider_name"),"issuer":row.get::<Option<String>,_>("issuer"),"subject":row.get::<Option<String>,_>("subject"),"address":row.get::<Option<String>,_>("address"),"enabled":row.get::<bool,_>("enabled"),"created_at":row.get::<i64,_>("created_at")})
}

fn session_value(row: &SqliteRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"revision":row.get::<i64,_>("revision").to_string(),"device_id":row.get::<String,_>("device_id"),"device_name":row.get::<String,_>("device_name"),"user_id":row.get::<String,_>("user_id"),"user_name":row.get::<String,_>("user_name"),"method":row.get::<String,_>("kind"),"provider_id":row.get::<Option<String>,_>("provider_id"),"issued_at":row.get::<i64,_>("issued_at"),"expires_at":row.get::<i64,_>("expires_at"),"last_seen_at":row.get::<Option<i64>,_>("last_seen_at"),"revoked":row.get::<bool,_>("revoked")})
}

async fn identity_rows(
    c: &mut sqlx::SqliteConnection,
    owner: Option<&str>,
    after: &str,
    limit: i64,
) -> Result<Vec<Value>, Problem> {
    let rows=sqlx::query("SELECT i.*,p.name AS provider_name FROM signin_identities i LEFT JOIN signin_providers p ON p.id=i.provider_id WHERE (? IS NULL OR i.user_id=?) AND i.id>? ORDER BY i.id LIMIT ?").bind(owner).bind(owner).bind(after).bind(limit).fetch_all(c).await?;
    Ok(rows.iter().map(identity_value).collect())
}

async fn session_rows(
    c: &mut sqlx::SqliteConnection,
    owner: Option<&str>,
    after: &str,
    limit: i64,
) -> Result<Vec<Value>, Problem> {
    let rows=sqlx::query("SELECT f.*,d.name AS device_name,u.name AS user_name,i.kind,i.provider_id FROM signin_families f JOIN devices d ON d.id=f.device_id JOIN access_users u ON u.id=f.user_id JOIN signin_identities i ON i.id=f.identity_id WHERE (? IS NULL OR f.user_id=?) AND f.id>? ORDER BY f.id LIMIT ?").bind(owner).bind(owner).bind(after).bind(limit).fetch_all(c).await?;
    Ok(rows.iter().map(session_value).collect())
}

pub async fn list_identities(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    Ok(paged(
        identity_rows(&mut tx, None, after, i64::from(limit) + 1).await?,
        limit,
    ))
}

pub async fn list_sessions(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    Ok(paged(
        session_rows(&mut tx, None, after, i64::from(limit) + 1).await?,
        limit,
    ))
}

pub async fn my_identities(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller.reauthenticate(&mut tx).await?;
    let user = owned_user(&mut tx, &caller)
        .await?
        .ok_or_else(Problem::not_found)?;
    Ok(paged(
        identity_rows(&mut tx, Some(&user.id), after, i64::from(limit) + 1).await?,
        limit,
    ))
}

pub async fn my_sessions(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    caller.reauthenticate(&mut tx).await?;
    let user = owned_user(&mut tx, &caller)
        .await?
        .ok_or_else(Problem::not_found)?;
    Ok(paged(
        session_rows(&mut tx, Some(&user.id), after, i64::from(limit) + 1).await?,
        limit,
    ))
}

pub async fn me(State(app): State<App>, caller: Caller) -> Result<Json<Value>, Problem> {
    let mut tx = app.db.begin().await?;
    let principal = caller.reauthenticate(&mut tx).await?;
    let user = owned_user(&mut tx, &caller).await?;
    let (identities, sessions) = match &user {
        Some(user) => (
            identity_rows(&mut tx, Some(&user.id), "", 200).await?,
            session_rows(&mut tx, Some(&user.id), "", 200).await?,
        ),
        None => (vec![], vec![]),
    };
    Ok(Json(
        json!({"device_id":principal.id,"user":user.as_ref().map(user_value),"identities":identities,"sessions":sessions,"detail_limit":200,"fresh_until":fresh_until(&mut tx,&caller).await?}),
    ))
}

pub async fn user_details(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, Problem> {
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let user = access_management::load_user(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let identities = identity_rows(&mut tx, Some(&id), "", 200).await?;
    let sessions = session_rows(&mut tx, Some(&id), "", 200).await?;
    let rows=sqlx::query("SELECT id,name,revision,revoked,client_name FROM devices WHERE user_id=? ORDER BY id LIMIT 200").bind(&id).fetch_all(&mut *tx).await?;
    let devices:Vec<Value>=rows.iter().map(|row|json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"revision":row.get::<i64,_>("revision").to_string(),"revoked":row.get::<bool,_>("revoked"),"client_name":row.get::<String,_>("client_name")})).collect();
    Ok(Json(
        json!({"user":user_value(&user),"identities":identities,"sessions":sessions,"devices":devices,"detail_limit":200}),
    ))
}

pub async fn revoke_session(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let row = sqlx::query("SELECT user_id,revision,revoked FROM signin_families WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    owner_or_admin(&mut tx, &caller, row.get::<&str, _>("user_id")).await?;
    check_revision(row.get("revision"), expected)?;
    if !row.get::<bool, _>("revoked") {
        revoke_families(&mut tx, &[id], &caller.principal.id).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn unlink_identity(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let row = sqlx::query("SELECT user_id,revision,enabled FROM signin_identities WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    owner_or_admin(&mut tx, &caller, row.get::<&str, _>("user_id")).await?;
    check_revision(row.get("revision"), expected)?;
    require_fresh(&mut tx, &caller, &headers, "identity.unlink", &id).await?;
    if row.get::<bool, _>("enabled") {
        preserve_methods(&mut tx, &caller, None, Some(&id), false).await?;
        let families = method_families(&mut tx, None, Some(&id), false).await?;
        revoke_families(&mut tx, &families, &caller.principal.id).await?;
        cancel_transactions(&mut tx, None, Some(&id), false).await?;
        sqlx::query("UPDATE signin_identities SET enabled=0,revision=revision+1 WHERE id=?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        audit(&mut tx, &caller.principal.id, &id, "identity.unlink", "ok").await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn revoke_user(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let user = access_management::load_user(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    check_revision(user.revision as i64, expected)?;
    require_fresh(&mut tx, &caller, &headers, "user.admin", &id).await?;
    let device_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM devices WHERE user_id=? AND revoked=0")
            .bind(&id)
            .fetch_all(&mut *tx)
            .await?;
    let mut changes = Vec::new();
    for device_id in device_ids {
        if let Some(row) = auth::load_device(&mut tx, &device_id).await? {
            let mut next = row.device.clone();
            next.revoked = true;
            next.revision = next.revision.checked_add(1).ok_or_else(unavailable)?;
            changes.push((row.device, next));
        }
    }
    access_management::guard_admin_change(&mut tx, &changes).await?;
    for (old, next) in changes {
        auth::update_device(&mut tx, &old, &next).await?;
        sqlx::query("DELETE FROM credentials WHERE device_id=?")
            .bind(&old.id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "UPDATE signin_families SET revoked=1,revision=revision+1 WHERE user_id=? AND revoked=0",
    )
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE signin_transactions SET state_json=json_set(state_json,'$.phase','failed'),nonce=NULL,verifier=NULL,code_hash=NULL,result='user_revoked' WHERE json_extract(state_json,'$.user_id')=?").bind(&id).execute(&mut *tx).await?;
    sqlx::query("UPDATE signin_outbox SET payload=NULL,phase='cancelled' WHERE transaction_id IN (SELECT id FROM signin_transactions WHERE result='user_revoked')").execute(&mut *tx).await?;
    sqlx::query("UPDATE access_users SET revision=revision+1 WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, &caller.principal.id, &id, "user.revoke", "ok").await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn list_audit(
    State(app): State<App>,
    caller: Caller,
    Query(query): Query<PageQuery>,
) -> Result<Json<Value>, Problem> {
    let limit = query.limit()?;
    let after = if query.cursor.is_some() {
        query
            .after()?
            .parse::<i64>()
            .map_err(|_| Problem::bad("invalid_cursor", "The audit cursor is invalid"))?
    } else {
        0
    };
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let rows = sqlx::query("SELECT * FROM signin_audit WHERE id>? ORDER BY id LIMIT ?")
        .bind(after)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await?;
    let items=rows.iter().map(|row|json!({"id":row.get::<i64,_>("id").to_string(),"actor":row.get::<String,_>("actor"),"target":row.get::<String,_>("target"),"action":row.get::<String,_>("action"),"outcome":row.get::<String,_>("outcome"),"at":row.get::<i64,_>("at")})).collect();
    Ok(paged(items, limit))
}
