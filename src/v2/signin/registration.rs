//! Email registration adapters. The production core decides admission and the
//! outcome of verified proof (`playscale_core::registration`); this module
//! supplies current facts under the reserved writer and commits the decided
//! effects atomically with challenge consumption.
use super::outbox;
use super::*;
use crate::mail::Content;
use playscale_core::{
    access::Policy as AccessPolicy,
    mail::Purpose as MailPurpose,
    registration::{self as policy, Admission, Outcome},
};
use std::collections::BTreeSet;

pub(super) const REQUEST_TTL: i64 = 7 * 86_400;
/// Open registrations activated per rolling day, and pending email requests.
pub(super) const DAILY_ACTIVATIONS: i64 = 100;
pub(super) const PENDING_REQUESTS: i64 = 1000;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct DefaultAccess {
    pub profile_ids: BTreeSet<String>,
    pub permissions: BTreeSet<Permission>,
    pub policy: AccessPolicy,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Settings {
    pub policy: policy::Policy,
    /// The open-registration user ceiling. Never includes administration.
    pub default: DefaultAccess,
}

impl DefaultAccess {
    fn grant(&self) -> Grant {
        Grant {
            profile_ids: self.profile_ids.clone(),
            permissions: self.permissions.clone(),
        }
    }
}

pub(super) async fn settings(c: &mut sqlx::SqliteConnection) -> Result<(Settings, i64), Problem> {
    let (json, revision): (String, i64) =
        sqlx::query_as("SELECT registration_json,registration_revision FROM signin_settings")
            .fetch_one(c)
            .await?;
    Ok((
        serde_json::from_str(&json).map_err(Problem::internal)?,
        revision,
    ))
}

/// Publicly visible policy: closed unless email sign-in is ready.
pub(super) async fn public_policy(
    c: &mut sqlx::SqliteConnection,
    email_ready: bool,
) -> Result<policy::Policy, Problem> {
    Ok(if email_ready {
        settings(c).await?.0.policy
    } else {
        policy::Policy::Closed
    })
}

/// The latest available email-bound invitation for exactly this address.
async fn available_invitation(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    address: &str,
) -> Result<Option<policy::InvitationRef>, Problem> {
    let rows: Vec<access_management::InviteRow> = sqlx::query_as(
        "SELECT * FROM access_invitations WHERE email=? AND revoked=0 AND device_id IS NULL AND identity_id IS NULL AND expires_at>? ORDER BY created_at DESC LIMIT 4",
    )
    .bind(address)
    .bind(now())
    .fetch_all(&mut *c)
    .await?;
    for row in rows {
        if registrable(c, app, &row).await? {
            return Ok(Some(policy::InvitationRef {
                id: row.id.clone(),
                revision: row.revision_value(),
            }));
        }
    }
    Ok(None)
}

async fn registrable(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    row: &access_management::InviteRow,
) -> Result<bool, Problem> {
    let invitation = row.invitation()?;
    let Some(user) = access_management::load_user(c, &invitation.user_id).await? else {
        return Ok(false);
    };
    let issuer = access_management::issuer_current(app, c, row).await?;
    Ok(policy::invitation_registrable(
        &invitation,
        &user,
        issuer,
        now(),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterStart {
    address: String,
    #[serde(default)]
    name: Option<String>,
}

/// Start an email registration. Every outcome (verification code, account
/// notice, or nothing) produces the same public response and rate limits.
pub async fn register_start(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Peer,
    body: Body<RegisterStart>,
) -> Result<Response, Problem> {
    origin(&app, &headers)?;
    let name = body
        .value
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("New user")
        .to_owned();
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(Problem::invalid(
            "invalid_field",
            "Use a display name of at most 100 characters",
        ));
    }
    limit(&app, "register:global", 30, 60).await?;
    network_limit(&app, &peer, "register", 10, 600).await?;
    let browser = binding(&headers)
        .map(str::to_owned)
        .unwrap_or_else(|| keyed(&app, "browser-random", &auth::nonce()));
    limit(&app, &format!("register:browser:{browser}"), 5, 600).await?;
    let address = core::canonical_email(&body.value.address).unwrap_or_default();
    limit(&app, &format!("email:address:{address}"), 5, 600).await?;
    let mut tx = begin_write(&app.db).await?;
    trim(&mut tx).await?;
    let mail = mail(&mut tx, &app).await?;
    if !mail.cfg.enabled || !mail.deliverable() || hold(&mut tx).await? {
        return Err(unavailable());
    }
    let (registration, revision) = settings(&mut tx).await?;
    let owned = !address.is_empty()
        && sqlx::query_scalar::<_, i64>("SELECT count(*) FROM signin_identities WHERE address=?")
            .bind(&address)
            .fetch_one(&mut *tx)
            .await?
            > 0;
    let invitation = if address.is_empty() || owned {
        None
    } else {
        available_invitation(&mut tx, &app, &address).await?
    };
    let admission = match policy::admit(registration.policy, revision as u64, owned, invitation) {
        Ok(_) if address.is_empty() => Admission::Silent,
        Ok(admission) => admission,
        Err(error) => {
            return Err(Problem::new(
                StatusCode::FORBIDDEN,
                error.code(),
                "This server does not accept new account registrations",
            ));
        }
    };
    let id = new_id();
    let random = auth::random_key();
    let code = format!(
        "{:08}",
        u32::from_be_bytes(random[..4].try_into().unwrap()) % 100_000_000
    );
    let intent = match &admission {
        Admission::Verify(intent) => Some(intent.clone()),
        _ => None,
    };
    let state = core::Transaction {
        id: id.clone(),
        purpose: core::Purpose::Register,
        browser_hash: keyed(&app, "browser", &browser),
        epoch: epoch(&mut tx).await?,
        server_epoch: app.access.server_epoch.clone(),
        provider_revision: mail.revision as u64,
        user_id: None,
        user_revision: None,
        identity_revision: None,
        actor_revision: None,
        expires_at: now() + core::TRANSACTION_TTL,
        phase: core::Phase::Pending,
        attempts: 0,
        // A notice or silent admission keeps an unredeemable placeholder so
        // the transaction is indistinguishable but can never complete.
        registration: Some(intent.clone().unwrap_or(policy::Intent {
            policy: policy::Policy::Closed,
            policy_revision: 0,
            invitation: None,
        })),
    };
    let hash = keyed(
        &app,
        &format!("email-code:{id}"),
        if intent.is_some() {
            &code
        } else {
            "ineligible"
        },
    );
    sqlx::query("INSERT INTO signin_transactions(id,state_json,address,code_hash,return_to,created_at,expires_at,registration_name) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&id)
        .bind(jsonstr(&state))
        .bind(&address)
        .bind(hash)
        .bind("/")
        .bind(now())
        .bind(state.expires_at)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    match admission {
        Admission::Verify(_) => {
            outbox::enqueue(
                &mut tx,
                &app,
                &mail,
                MailPurpose::RegistrationCode,
                &id,
                Some(&id),
                &address,
                Content::RegistrationCode {
                    code: &code,
                    origin: &app.origin,
                },
                state.expires_at,
            )
            .await?;
        }
        Admission::Notice => {
            outbox::enqueue(
                &mut tx,
                &app,
                &mail,
                MailPurpose::AccountNotice,
                &id,
                Some(&id),
                &address,
                Content::AccountNotice {
                    origin: &app.origin,
                },
                state.expires_at,
            )
            .await?;
        }
        Admission::Silent => {}
    }
    audit(
        &mut tx,
        "anonymous",
        &id,
        "registration.request",
        "accepted",
    )
    .await?;
    tx.commit().await?;
    start_outbox(app.clone());
    let mut response = Json(json!({
        "transaction_id": id,
        "message": "If this address can register, a verification code has been sent. Enter it in this browser."
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, binding_header(&app, &browser));
    Ok(response)
}

pub(super) enum Completed {
    Session(String, (String, i64)),
    NoSession(String),
}

/// Consume verified registration proof. The caller has already recorded the
/// matching code attempt and moved the challenge to Exchanging in `tx`.
pub(super) async fn complete(
    tx: &mut sqlx::SqliteConnection,
    app: &App,
    row: &TxRow,
    state: &core::Transaction,
) -> Result<Result<Completed, policy::Error>, Problem> {
    let intent = state.registration.as_ref().ok_or_else(unavailable)?;
    let address = row.address.as_deref().ok_or_else(unavailable)?;
    let (registration, revision) = settings(tx).await?;
    let owned =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM signin_identities WHERE address=?")
            .bind(address)
            .fetch_one(&mut *tx)
            .await?
            > 0;
    let invitation_row: Option<access_management::InviteRow> = match &intent.invitation {
        Some(reference) => {
            sqlx::query_as("SELECT * FROM access_invitations WHERE id=?")
                .bind(&reference.id)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    let invitation_registrable = match &invitation_row {
        Some(row) => row.email.as_deref() == Some(address) && registrable(tx, app, row).await?,
        None => false,
    };
    let pending = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM signin_requests WHERE kind='email' AND status='pending' AND address=? AND expires_at>?",
    )
    .bind(address)
    .bind(now())
    .fetch_one(&mut *tx)
    .await?
        > 0;
    let default_valid = policy::default_grant_allowed(&registration.default.grant())
        && access_management::validate_grant(
            tx,
            &registration.default.grant(),
            &registration.default.policy,
        )
        .await
        .is_ok();
    let capacity = match registration.policy {
        policy::Policy::Open => {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM registration_activations WHERE at>?")
                .bind(now() - 86_400)
                .fetch_one(&mut *tx)
                .await?
                < DAILY_ACTIVATIONS
        }
        _ => {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM signin_requests WHERE status='pending' AND expires_at>?",
            )
            .bind(now())
            .fetch_one(&mut *tx)
            .await?
                < PENDING_REQUESTS
        }
    };
    let decision = policy::decide(
        intent,
        &policy::Current {
            policy: registration.policy,
            policy_revision: revision as u64,
            address_owned: owned,
            invitation: invitation_row
                .as_ref()
                .map(|row| policy::CurrentInvitation {
                    id: &row.id,
                    revision: row.revision_value(),
                    registrable: invitation_registrable,
                }),
            pending_request: pending,
            default_grant_valid: default_valid,
            capacity,
        },
    );
    let outcome = match decision {
        Ok(outcome) => outcome,
        Err(error) => return Ok(Err(error)),
    };
    let name: Option<String> =
        sqlx::query_scalar("SELECT registration_name FROM signin_transactions WHERE id=?")
            .bind(&state.id)
            .fetch_one(&mut *tx)
            .await?;
    let result = outcome.result().to_owned();
    Ok(Ok(match outcome {
        Outcome::PendingApproval { existing } => {
            if !existing {
                sqlx::query("INSERT INTO signin_requests(id,kind,address,name,created_at,expires_at) VALUES(?,'email',?,?,?,?)")
                    .bind(new_id())
                    .bind(address)
                    .bind(name.as_deref().filter(|n| n.len() <= 200))
                    .bind(now())
                    .bind(now() + REQUEST_TTL)
                    .execute(&mut *tx)
                    .await?;
            }
            audit(tx, "anonymous", &state.id, "registration.request", &result).await?;
            Completed::NoSession(result)
        }
        Outcome::InvitationAccepted => {
            let row = invitation_row.ok_or_else(unavailable)?;
            let invitation = row.invitation()?;
            let user = access_management::load_user(tx, &invitation.user_id)
                .await?
                .ok_or_else(unavailable)?;
            let ceiling = Device {
                policy: invitation.policy.clone(),
                ..Device::approved("email-invitation".into(), invitation.grant.clone())
            };
            let identity = link_identity(
                tx,
                &user,
                &ceiling,
                "email",
                None,
                None,
                None,
                Some(address),
            )
            .await?;
            let claimed = sqlx::query("UPDATE access_invitations SET identity_id=?,revision=revision+1 WHERE id=? AND revision=? AND revoked=0 AND device_id IS NULL AND identity_id IS NULL")
                .bind(&identity)
                .bind(&row.id)
                .bind(row.revision_value() as i64)
                .execute(&mut *tx)
                .await?;
            if claimed.rows_affected() != 1 {
                return Err(problem(core::Error::Stale));
            }
            outbox::cancel_owner(tx, &row.id).await?;
            audit(tx, &user.id, &row.id, "invitation.register", "ok").await?;
            let token = issue(tx, app, &identity, state.provider_revision, now()).await?;
            Completed::Session(result, token)
        }
        Outcome::Activated => {
            let default = &registration.default;
            let user = policy::open_user(
                new_id(),
                name.unwrap_or_else(|| "New user".into()),
                &default.grant(),
                &default.policy,
            );
            access_management::insert_user(tx, &user).await?;
            sqlx::query("DELETE FROM registration_activations WHERE at<?")
                .bind(now() - 2 * 86_400)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO registration_activations(user_id,at) VALUES(?,?)")
                .bind(&user.id)
                .bind(now())
                .execute(&mut *tx)
                .await?;
            let ceiling = Device {
                policy: user.policy.clone(),
                ..Device::approved("email-registration".into(), user.grant.clone())
            };
            let identity = link_identity(
                tx,
                &user,
                &ceiling,
                "email",
                None,
                None,
                None,
                Some(address),
            )
            .await?;
            audit(tx, &user.id, &user.id, "registration.activate", "ok").await?;
            let token = issue(tx, app, &identity, state.provider_revision, now()).await?;
            Completed::Session(result, token)
        }
    }))
}

/// Queue the secret-free invitation notice for an email-bound invitation.
pub(crate) async fn email_invitation(
    c: &mut sqlx::SqliteConnection,
    app: &App,
    invitation: &str,
    address: &str,
    expires_at: i64,
) -> Result<(), Problem> {
    let mail = mail(c, app).await?;
    if !mail.cfg.enabled || hold(c).await? {
        return Err(Problem::invalid(
            "email_unavailable",
            "Enable email delivery before emailing invitations",
        ));
    }
    if settings(c).await?.0.policy == policy::Policy::Closed {
        return Err(Problem::invalid(
            "registration_closed",
            "Choose a registration policy other than Closed before emailing invitations",
        ));
    }
    outbox::enqueue(
        c,
        app,
        &mail,
        MailPurpose::Invitation,
        invitation,
        None,
        address,
        Content::Invitation {
            origin: &app.origin,
            expires_at,
        },
        expires_at,
    )
    .await?;
    Ok(())
}

pub(crate) async fn cancel_invitation_mail(
    c: &mut sqlx::SqliteConnection,
    invitation: &str,
) -> Result<(), Problem> {
    outbox::cancel_owner(c, invitation).await
}

fn settings_value(settings: &Settings, revision: i64) -> Value {
    json!({
        "policy": settings.policy,
        "revision": revision.to_string(),
        "default_access": {
            "profile_ids": settings.default.profile_ids,
            "permissions": settings.default.permissions,
            "policy": settings.default.policy,
        },
    })
}

pub async fn get_registration(State(app): State<App>, caller: Caller) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let (settings, revision) = settings(&mut tx).await?;
    Ok(super::super::identity::tagged(
        StatusCode::OK,
        &settings_value(&settings, revision),
        revision as u64,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationInput {
    policy: policy::Policy,
    default_access: DefaultAccess,
}

pub async fn replace_registration(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<RegistrationInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let input = body.value;
    let next = Settings {
        policy: input.policy,
        default: input.default_access,
    };
    if !policy::default_grant_allowed(&next.default.grant()) {
        return Err(Problem::invalid(
            "registration_admin_forbidden",
            "Open registration can never grant administrator access",
        ));
    }
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let (current, revision) = settings(&mut tx).await?;
    if revision < 1 || revision as u64 != expected {
        return Err(AccessError::StaleRevision.into());
    }
    require_fresh(
        &mut tx,
        &caller,
        &headers,
        "registration.write",
        "registration",
    )
    .await?;
    access_management::validate_grant(&mut tx, &next.default.grant(), &next.default.policy).await?;
    let revision = if next != current {
        let revision = revision.checked_add(1).ok_or_else(unavailable)?;
        sqlx::query("UPDATE signin_settings SET registration_json=?,registration_revision=?,revision=revision+1")
            .bind(jsonstr(&next))
            .bind(revision)
            .execute(&mut *tx)
            .await?;
        // Pending registration challenges captured the old revision and would
        // fail at completion anyway; end them now and withdraw queued codes.
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM signin_transactions WHERE json_extract(state_json,'$.purpose')='register' AND json_extract(state_json,'$.phase')='pending'")
            .fetch_all(&mut *tx)
            .await?;
        for id in ids {
            sqlx::query("UPDATE signin_transactions SET state_json=json_set(state_json,'$.phase','failed'),code_hash=NULL,result='registration_changed' WHERE id=?")
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            outbox::cancel_owner(&mut tx, &id).await?;
        }
        audit(
            &mut tx,
            &caller.principal.id,
            "registration",
            "registration.write",
            "ok",
        )
        .await?;
        revision
    } else {
        revision
    };
    tx.commit().await?;
    Ok(super::super::identity::tagged(
        StatusCode::OK,
        &settings_value(&next, revision),
        revision as u64,
    ))
}
