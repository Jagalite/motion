//! Administrator access operations. Writer transactions reserve the snapshot
//! before authorization, lifecycle decisions, receipts and credential effects.
use super::{
    Body, Page, PageQuery, Problem, Scope,
    auth::{self, Caller, DeviceRow, Purpose},
    idempotency_key,
    identity::{self, tagged},
    if_match, page, timestamp,
};
use crate::{App, db::begin_write, new_id, now};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use playscale_core::{
    access::{
        self as core, AccessError, Credential, CredentialKind, Device, Grant, Idempotent,
        Permission, Policy, Revised,
    },
    access_admin::{self as admin, Invitation, InvitationPhase, Redemption, User},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("access serialization")
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: String,
    name: String,
    revision: i64,
    disabled: bool,
    profile_ids: String,
    permissions: String,
    policy: String,
}
impl UserRow {
    fn decode(self) -> Result<User, Problem> {
        Ok(User {
            id: self.id,
            name: self.name,
            revision: self.revision as u64,
            disabled: self.disabled,
            grant: Grant {
                profile_ids: serde_json::from_str(&self.profile_ids).map_err(Problem::internal)?,
                permissions: serde_json::from_str(&self.permissions).map_err(Problem::internal)?,
            },
            policy: serde_json::from_str(&self.policy).map_err(Problem::internal)?,
        })
    }
}
pub(super) async fn load_user(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<Option<User>, Problem> {
    sqlx::query_as::<_,UserRow>("SELECT id,name,revision,disabled,profile_ids,permissions,policy FROM access_users WHERE id=?").bind(id).fetch_optional(conn).await?.map(UserRow::decode).transpose()
}
#[derive(Serialize)]
pub struct UserBody {
    id: String,
    name: String,
    revision: String,
    disabled: bool,
    profile_ids: BTreeSet<String>,
    permissions: BTreeSet<Permission>,
    policy: Policy,
}
fn user_body(u: &User) -> UserBody {
    UserBody {
        id: u.id.clone(),
        name: u.name.clone(),
        revision: u.revision.to_string(),
        disabled: u.disabled,
        profile_ids: u.grant.profile_ids.clone(),
        permissions: u.grant.permissions.clone(),
        policy: u.policy.clone(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInput {
    name: String,
    disabled: bool,
    profile_ids: Vec<String>,
    permissions: Vec<Permission>,
    policy: Policy,
}
impl UserInput {
    fn user(self, id: String, revision: u64) -> Result<User, Problem> {
        identity::bounded("name", &self.name, 1, 100)?;
        Ok(User {
            id,
            revision,
            name: self.name,
            disabled: self.disabled,
            grant: Grant {
                profile_ids: self.profile_ids.into_iter().collect(),
                permissions: self.permissions.into_iter().collect(),
            },
            policy: self.policy,
        })
    }
}
pub(super) async fn validate_grant(
    conn: &mut sqlx::SqliteConnection,
    grant: &Grant,
    policy: &Policy,
) -> Result<(), Problem> {
    if policy.deny_rated && !policy.allowed_ratings.is_empty() {
        return Err(Problem::invalid(
            "invalid_field",
            "Denying rated content requires an empty rating allowlist",
        ));
    }
    identity::valid_ids(
        "profile_ids",
        &grant.profile_ids.iter().cloned().collect::<Vec<_>>(),
    )?;
    identity::valid_ids(
        "library_ids",
        &policy.library_ids.iter().cloned().collect::<Vec<_>>(),
    )?;
    if policy.allowed_ratings.len() > 200 || policy.blocked_labels.len() > admin::MAX_BLOCKED_LABELS
    {
        return Err(Problem::invalid(
            "invalid_field",
            "Too many rating or label restrictions",
        ));
    }
    for rating in &policy.allowed_ratings {
        identity::bounded("rating", rating, 1, 64)?;
    }
    for label in &policy.blocked_labels {
        identity::bounded("label", label, 1, 100)?;
    }
    if identity::existing_profiles(conn, &grant.profile_ids).await? != grant.profile_ids {
        return Err(AccessError::UnknownProfile.into());
    }
    let known: BTreeSet<String> = sqlx::query_scalar(
        "SELECT id FROM catalog_libraries WHERE id IN (SELECT value FROM json_each(?))",
    )
    .bind(json(&policy.library_ids))
    .fetch_all(conn)
    .await?
    .into_iter()
    .collect();
    if known != policy.library_ids {
        return Err(AccessError::UnknownLibrary.into());
    }
    Ok(())
}

pub async fn list_users(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<UserBody>>, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let rows:Vec<UserRow>=sqlx::query_as("SELECT id,name,revision,disabled,profile_ids,permissions,policy FROM access_users WHERE id>? ORDER BY id LIMIT ?").bind(q.after()?).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let rows = rows
        .into_iter()
        .map(|r| {
            let u = r.decode()?;
            Ok((u.id.clone(), user_body(&u)))
        })
        .collect::<Result<Vec<_>, Problem>>()?;
    let body = page(&mut tx, &principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(body))
}
pub async fn get_user(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let user = load_user(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    Ok(tagged(StatusCode::OK, &user_body(&user), user.revision))
}
pub async fn create_user(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<UserInput>,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createAccessUser",
        target: "",
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        let s = stored.unwrap();
        let value = s.body.clone().unwrap_or_default();
        return Ok(identity::replayed(s, value));
    }
    super::signin::require_fresh(&mut tx, &caller, &headers, "user.admin", "new").await?;
    let user = body.value.user(new_id(), 1)?;
    validate_grant(&mut tx, &user.grant, &user.policy).await?;
    sqlx::query("INSERT INTO access_users(id,name,revision,disabled,profile_ids,permissions,policy,created_at) VALUES (?,?,1,?,?,?,?,?)").bind(&user.id).bind(&user.name).bind(user.disabled).bind(json(&user.grant.profile_ids)).bind(json(&user.grant.permissions)).bind(json(&user.policy)).bind(now()).execute(&mut *tx).await?;
    let response = serde_json::to_value(user_body(&user)).map_err(Problem::internal)?;
    scope
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&response),
            None,
        )
        .await?;
    super::signin::audit(&mut tx, &caller.principal.id, &user.id, "user.create", "ok").await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &response, 1))
}
pub async fn replace_user(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<UserInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    super::signin::require_fresh(&mut tx, &caller, &headers, "user.admin", &id).await?;
    let previous = load_user(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let mut next = body.value.user(id.clone(), previous.revision)?;
    validate_grant(&mut tx, &next.grant, &next.policy).await?;
    if let Revised::Next(revision) = core::revise(previous.revision, expected, next != previous)? {
        next.revision = revision;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM devices WHERE user_id=? AND revoked=0")
            .bind(&id)
            .fetch_all(&mut *tx)
            .await?;
        let mut changes = vec![];
        for device_id in ids {
            let row = auth::load_device(&mut tx, &device_id)
                .await?
                .ok_or_else(Problem::not_found)?;
            let constrained = admin::constrain(&row.device, &next)?;
            changes.push((row.device, constrained));
        }
        guard_admin_change(&mut tx, &changes).await?;
        for (old, new) in changes {
            auth::update_device(&mut tx, &old, &new).await?;
            if new.revoked {
                sqlx::query("DELETE FROM credentials WHERE device_id=?")
                    .bind(&new.id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE access_users SET name=?,revision=?,disabled=?,profile_ids=?,permissions=?,policy=?,legacy=0 WHERE id=? AND revision=?").bind(&next.name).bind(next.revision as i64).bind(next.disabled).bind(json(&next.grant.profile_ids)).bind(json(&next.grant.permissions)).bind(json(&next.policy)).bind(&id).bind(expected as i64).execute(&mut *tx).await?;
    }
    super::signin::audit(&mut tx, &caller.principal.id, &id, "user.replace", "ok").await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &user_body(&next), next.revision))
}

/// Unassigned devices retain the existing device policy API. Explicit owners
/// always bound a device; changing a device never changes its owner's ceiling.
pub(super) async fn check_device_ceiling(
    conn: &mut sqlx::SqliteConnection,
    row: &DeviceRow,
    next: &Device,
) -> Result<(), Problem> {
    let Some(id) = row.user_id.as_deref() else {
        return Ok(());
    };
    let user = load_user(conn, id)
        .await?
        .ok_or(AccessError::UserDisabled)?;
    Ok(admin::authorize_grant(
        &user,
        user.revision,
        &next.grant,
        &next.policy,
    )?)
}
pub(super) async fn guard_admin_change(
    conn: &mut sqlx::SqliteConnection,
    changes: &[(Device, Device)],
) -> Result<(), Problem> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT DISTINCT d.id FROM devices d JOIN credentials c ON c.device_id=d.id AND c.kind='device' AND c.generation=d.generation LEFT JOIN access_users u ON u.id=d.user_id WHERE c.expires_at>? AND (d.user_id IS NULL OR u.disabled=0) AND d.revoked=0").bind(now()).fetch_all(&mut *conn).await?;
    let (mut before, mut after) = (0, 0);
    for id in ids {
        let row = auth::load_device(conn, &id)
            .await?
            .ok_or_else(Problem::not_found)?;
        before += usize::from(admin::usable_admin(&row.device, true, now()));
        let next = changes
            .iter()
            .find(|(old, _)| old.id == id)
            .map(|(_, n)| n)
            .unwrap_or(&row.device);
        after += usize::from(admin::usable_admin(next, true, now()));
    }
    Ok(admin::preserve_admin(before, after)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceUserInput {
    user_id: String,
    user_revision: String,
}

/// Attach an unassigned paired device to an explicitly selected user. Both
/// revision checks and the narrower device grant commit in one writer snapshot.
pub async fn assign_device_user(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body<DeviceUserInput>,
) -> Result<Response, Problem> {
    let expected = if_match(&headers)?;
    let expected_user = body.value.user_revision.parse::<u64>().map_err(|_| {
        Problem::invalid("user_revision_required", "Select a current user revision")
    })?;
    let mut tx = begin_write(&app.db).await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let row = auth::load_device(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let user = load_user(&mut tx, &body.value.user_id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let next = admin::assign_owner(
        &row.device,
        row.user_id.as_deref(),
        expected,
        &user,
        expected_user,
    )?;
    super::signin::require_fresh(&mut tx, &caller, &headers, "device.assign", &id).await?;
    guard_admin_change(&mut tx, &[(row.device.clone(), next.clone())]).await?;
    auth::update_device(&mut tx, &row.device, &next).await?;
    let changed = sqlx::query("UPDATE devices SET user_id=? WHERE id=? AND user_id IS NULL")
        .bind(&user.id)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    if changed.rows_affected() != 1 {
        return Err(AccessError::StaleRevision.into());
    }
    sqlx::query("UPDATE access_users SET legacy=0 WHERE id=?")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    let row = DeviceRow {
        device: next,
        user_id: Some(user.id),
        ..row
    };
    super::signin::audit(&mut tx, &caller.principal.id, &id, "device.assign", "ok").await?;
    tx.commit().await?;
    Ok(tagged(
        StatusCode::OK,
        &identity::device_body(&row),
        row.device.revision,
    ))
}

#[derive(Serialize)]
pub struct PairingSummary {
    id: String,
    device_name: String,
    client_name: String,
    user_code: String,
    created_at: String,
    expires_at: String,
    status: &'static str,
}
#[derive(sqlx::FromRow)]
struct PairingListRow {
    id: String,
    device_name: String,
    client_name: String,
    user_code: String,
    created_at: i64,
    expires_at: i64,
    phase: String,
    denied: bool,
}
pub async fn list_pairings(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<PairingSummary>>, Problem> {
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let rows: Vec<PairingListRow> =sqlx::query_as("SELECT id,device_name,client_name,user_code,created_at,expires_at,phase,denied FROM pairings WHERE id>? ORDER BY id LIMIT ?").bind(q.after()?).bind(i64::from(limit)+1).fetch_all(&mut *tx).await?;
    let rows = rows
        .into_iter()
        .map(
            |PairingListRow {
                 id,
                 device_name,
                 client_name,
                 user_code,
                 created_at: created,
                 expires_at: expires,
                 phase,
                 denied,
             }| {
                let status = if denied {
                    "denied"
                } else if phase == "claimed" {
                    "claimed"
                } else if now() >= expires {
                    "expired"
                } else if phase == "approved" {
                    "approved"
                } else {
                    "pending"
                };
                (
                    id.clone(),
                    PairingSummary {
                        id,
                        device_name,
                        client_name,
                        user_code,
                        created_at: timestamp(created),
                        expires_at: timestamp(expires),
                        status,
                    },
                )
            },
        )
        .collect();
    let body = page(&mut tx, &principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(body))
}
pub async fn deny_pairing(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "denyPairing",
        target: &id,
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), "deny", now())? == Idempotent::Replay {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let row = identity::load_pairing(&mut tx, &id)
        .await?
        .ok_or_else(Problem::not_found)?;
    admin::deny(&identity::pairing(&id, &row), &principal, now())?;
    sqlx::query("UPDATE pairings SET denied=1 WHERE id=? AND phase='pending' AND denied=0")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    scope
        .save(&mut tx, "deny", StatusCode::NO_CONTENT, None, None)
        .await?;
    super::signin::audit(&mut tx, &caller.principal.id, &id, "pairing.deny", "ok").await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(sqlx::FromRow)]
struct InviteRow {
    id: String,
    user_id: String,
    user_revision: i64,
    issuer_id: String,
    issuer_revision: i64,
    issuer_credential_hash: Option<String>,
    profile_ids: String,
    permissions: String,
    policy: String,
    created_at: i64,
    expires_at: i64,
    revision: i64,
    revoked: bool,
    claim_hash: Option<String>,
    device_id: Option<String>,
    generation: Option<i64>,
    credential_nonce: Option<String>,
}
impl InviteRow {
    fn invitation(&self) -> Result<Invitation, Problem> {
        Ok(Invitation {
            expires_at: self.expires_at,
            user_id: self.user_id.clone(),
            user_revision: self.user_revision as u64,
            grant: Grant {
                profile_ids: serde_json::from_str(&self.profile_ids).map_err(Problem::internal)?,
                permissions: serde_json::from_str(&self.permissions).map_err(Problem::internal)?,
            },
            policy: serde_json::from_str(&self.policy).map_err(Problem::internal)?,
            phase: if self.revoked {
                InvitationPhase::Revoked
            } else if let Some(device_id) = &self.device_id {
                InvitationPhase::Claimed {
                    device_id: device_id.clone(),
                    claim_hash: self.claim_hash.clone().unwrap_or_default(),
                    generation: self.generation.unwrap_or_default() as u64,
                }
            } else {
                InvitationPhase::Unused
            },
        })
    }
}
#[derive(Serialize)]
pub struct InvitationBody {
    id: String,
    user_id: String,
    revision: String,
    created_at: String,
    expires_at: String,
    status: &'static str,
    profile_ids: BTreeSet<String>,
    permissions: BTreeSet<Permission>,
    policy: Policy,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}
fn invite_body(
    row: &InviteRow,
    code: Option<String>,
    available: bool,
) -> Result<InvitationBody, Problem> {
    let inv = row.invitation()?;
    Ok(InvitationBody {
        id: row.id.clone(),
        user_id: row.user_id.clone(),
        revision: row.revision.to_string(),
        created_at: timestamp(row.created_at),
        expires_at: timestamp(row.expires_at),
        status: if row.revoked {
            "revoked"
        } else if row.device_id.is_some() {
            "claimed"
        } else if now() >= row.expires_at {
            "expired"
        } else if !available {
            "invalidated"
        } else {
            "unused"
        },
        profile_ids: inv.grant.profile_ids,
        permissions: inv.grant.permissions,
        policy: inv.policy,
        code,
    })
}
async fn issuer_current(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    row: &InviteRow,
) -> Result<bool, Problem> {
    if row.issuer_id == "operator" {
        return Ok(row.issuer_credential_hash.as_deref()
            == Some(auth::token_hash(&app.admin_token).as_str()));
    }
    let Some(hash) = &row.issuer_credential_hash else {
        return Ok(false);
    };
    let Some(stored) = auth::load_credential(conn, hash).await? else {
        return Ok(false);
    };
    match auth::authenticate_stored(conn, &stored, now()).await {
        Ok((p, _)) => Ok(p.id == row.issuer_id
            && p.policy_revision == row.issuer_revision as u64
            && p.is_admin()),
        Err(e) if e.status == StatusCode::UNAUTHORIZED => Ok(false),
        Err(e) => Err(e),
    }
}
async fn invitation_available(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    row: &InviteRow,
) -> Result<bool, Problem> {
    let Some(user) = load_user(conn, &row.user_id).await? else {
        return Ok(false);
    };
    let inv = row.invitation()?;
    Ok(issuer_current(app, conn, row).await?
        && admin::authorize_grant(&user, inv.user_revision, &inv.grant, &inv.policy).is_ok())
}
pub async fn list_invitations(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Json<Page<InvitationBody>>, Problem> {
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let rows: Vec<InviteRow> =
        sqlx::query_as("SELECT * FROM access_invitations WHERE id>? ORDER BY id LIMIT ?")
            .bind(q.after()?)
            .bind(i64::from(limit) + 1)
            .fetch_all(&mut *tx)
            .await?;
    let mut output = vec![];
    for row in rows {
        let available = invitation_available(&app, &mut tx, &row).await?;
        output.push((row.id.clone(), invite_body(&row, None, available)?));
    }
    let body = page(&mut tx, &principal, output, limit).await?;
    tx.commit().await?;
    Ok(Json(body))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitationInput {
    user_id: String,
    user_revision: String,
    profile_ids: Vec<String>,
    permissions: Vec<Permission>,
    policy: Policy,
    ttl_seconds: i64,
}
pub async fn create_invitation(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<InvitationInput>,
) -> Result<Response, Problem> {
    let key = idempotency_key(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let scope = Scope {
        principal: &principal.id,
        operation: "createAccessInvitation",
        target: "",
        key: &key,
    };
    let stored = scope.load(&mut tx).await?;
    if core::idempotency(stored.as_ref().map(|s| &s.record), &body.digest, now())?
        == Idempotent::Replay
    {
        let stored = stored.unwrap();
        let id = stored
            .body
            .as_ref()
            .and_then(|b| b["id"].as_str())
            .ok_or(AccessError::ReplayUnavailable)?;
        let row: InviteRow = sqlx::query_as("SELECT * FROM access_invitations WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if row.revoked
            || row.device_id.is_some()
            || now() >= row.expires_at
            || !invitation_available(&app, &mut tx, &row).await?
        {
            return Err(AccessError::ReplayUnavailable.into());
        }
        let nonce = stored
            .issued_nonce
            .as_deref()
            .ok_or(AccessError::ReplayUnavailable)?;
        let response = invite_body(
            &row,
            Some(auth::derive_secret(
                &app.access.key,
                Purpose::Invitation,
                nonce,
            )),
            true,
        )?;
        return Ok(tagged(StatusCode::CREATED, &response, row.revision as u64));
    }
    super::signin::require_fresh(
        &mut tx,
        &caller,
        &headers,
        "invitation.create",
        &body.value.user_id,
    )
    .await?;
    let input = body.value;
    if !(60..=admin::INVITATION_TTL_MAX).contains(&input.ttl_seconds) {
        return Err(Problem::invalid(
            "invalid_ttl",
            "Invitation lifetime must be 60–86400 seconds",
        ));
    }
    let user = load_user(&mut tx, &input.user_id)
        .await?
        .ok_or_else(Problem::not_found)?;
    let expected = input
        .user_revision
        .parse()
        .map_err(|_| Problem::invalid("invalid_revision", "Select a current user"))?;
    let grant = Grant {
        profile_ids: input.profile_ids.into_iter().collect(),
        permissions: input.permissions.into_iter().collect(),
    };
    validate_grant(&mut tx, &grant, &input.policy).await?;
    admin::authorize_grant(&user, expected, &grant, &input.policy)?;
    let id = new_id();
    let nonce = auth::nonce();
    let code = auth::derive_secret(&app.access.key, Purpose::Invitation, &nonce);
    let issuer_hash = if principal.id == "operator" {
        Some(auth::token_hash(&app.admin_token))
    } else {
        caller.credential().map(|(hash, _)| hash.to_owned())
    };
    sqlx::query("INSERT INTO access_invitations(id,code_hash,user_id,user_revision,issuer_id,issuer_revision,issuer_credential_hash,profile_ids,permissions,policy,created_at,expires_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)").bind(&id).bind(auth::token_hash(&code)).bind(&user.id).bind(expected as i64).bind(&principal.id).bind(principal.policy_revision as i64).bind(issuer_hash).bind(json(&grant.profile_ids)).bind(json(&grant.permissions)).bind(json(&input.policy)).bind(now()).bind(now()+input.ttl_seconds).execute(&mut *tx).await?;
    let row: InviteRow = sqlx::query_as("SELECT * FROM access_invitations WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    let receipt = serde_json::json!({"id":id});
    scope
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&receipt),
            Some(&nonce),
        )
        .await?;
    let response = invite_body(&row, Some(code), true)?;
    super::signin::audit(
        &mut tx,
        &caller.principal.id,
        &id,
        "invitation.create",
        "ok",
    )
    .await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, &response, 1))
}
pub async fn revoke_invitation(
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
    let row: InviteRow = sqlx::query_as("SELECT * FROM access_invitations WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?;
    admin::revoke_invitation(&row.invitation()?, row.revision as u64, expected)?;
    if !row.revoked {
        sqlx::query("UPDATE access_invitations SET revoked=1,revision=revision+1 WHERE id=?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    super::signin::audit(
        &mut tx,
        &caller.principal.id,
        &id,
        "invitation.revoke",
        "ok",
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedemptionInput {
    code: String,
    claim_id: String,
    device_name: String,
    client_name: String,
}
#[derive(Serialize)]
struct CredentialBody {
    device_id: String,
    access_token: String,
    expires_at: String,
}
pub async fn redeem_invitation(
    State(app): State<App>,
    body: Body<RedemptionInput>,
) -> Result<Response, Problem> {
    let input = body.value;
    identity::bounded("code", &input.code, 1, 128)?;
    identity::bounded("device_name", &input.device_name, 1, 100)?;
    identity::bounded("client_name", &input.client_name, 1, 100)?;
    if input.claim_id.len() != 32 || !input.claim_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Problem::invalid(
            "invalid_claim_id",
            "Use a random 128-bit hexadecimal claim ID and retain it across retries",
        ));
    }
    // The counter is independently durable: failed redemption transactions cannot
    // roll back failed attempts, and restarts do not reset the window.
    let mut admission = begin_write(&app.db).await?;
    let (start, count): (i64, u32) =
        sqlx::query_as("SELECT window_start,attempts FROM invitation_attempts WHERE singleton=1")
            .fetch_one(&mut *admission)
            .await?;
    let (start, count) = admin::redemption_attempt(start, count, now())
        .map_err(|wait| Problem::rate_limited(wait as u64))?;
    sqlx::query("UPDATE invitation_attempts SET window_start=?,attempts=? WHERE singleton=1")
        .bind(start)
        .bind(count)
        .execute(&mut *admission)
        .await?;
    admission.commit().await?;
    let mut tx = begin_write(&app.db).await?;
    let row: InviteRow = sqlx::query_as("SELECT * FROM access_invitations WHERE code_hash=?")
        .bind(auth::token_hash(&input.code))
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AccessError::InvitationUnavailable)?;
    let user = load_user(&mut tx, &row.user_id)
        .await?
        .ok_or(AccessError::InvitationUnavailable)?;
    let issuer = issuer_current(&app, &mut tx, &row).await?;
    let invitation = row.invitation()?;
    // Bind the full request so a retry cannot rename or reassign the device.
    let claim_hash = super::sha256(
        json(&(
            input.claim_id,
            input.device_name.clone(),
            input.client_name.clone(),
        ))
        .as_bytes(),
    );
    let previous = match &row.device_id {
        Some(id) => auth::load_device(&mut tx, id).await?,
        None => None,
    };
    let issued = match &row.credential_nonce {
        Some(n) => {
            auth::load_credential(
                &mut tx,
                &auth::token_hash(&auth::derive_secret(&app.access.key, Purpose::Device, n)),
            )
            .await?
        }
        None => None,
    };
    let decision = admin::redeem(
        &invitation,
        &user,
        issuer,
        &claim_hash,
        previous
            .as_ref()
            .zip(issued.as_ref())
            .map(|(d, c)| (&d.device, &c.credential)),
        now(),
    )?;
    let (device_id, nonce, expires) = match decision {
        Redemption::Replay => (
            row.device_id.unwrap(),
            row.credential_nonce.unwrap(),
            issued.unwrap().credential.expires_at,
        ),
        Redemption::Issue => {
            let mut device = Device::approved(new_id(), invitation.grant);
            device.policy = invitation.policy;
            let issue = core::Issue {
                device_id: device.id.clone(),
                generation: 1,
                expires_at: now() + core::DEVICE_CREDENTIAL_TTL_SECONDS,
            };
            device = core::apply_issue(&device, &issue)?;
            let device_row = DeviceRow {
                device,
                name: input.device_name,
                client_name: input.client_name,
                user_id: Some(user.id),
            };
            auth::insert_device(&mut tx, &device_row).await?;
            let nonce = auth::nonce();
            let token = auth::derive_secret(&app.access.key, Purpose::Device, &nonce);
            auth::insert_credential(
                &mut tx,
                &auth::token_hash(&token),
                &Credential {
                    device_id: issue.device_id.clone(),
                    kind: CredentialKind::Device,
                    generation: 1,
                    expires_at: issue.expires_at,
                },
                None,
                None,
            )
            .await?;
            sqlx::query("UPDATE access_invitations SET claim_hash=?,device_id=?,generation=1,credential_nonce=?,revision=revision+1 WHERE id=? AND revoked=0 AND device_id IS NULL").bind(&claim_hash).bind(&issue.device_id).bind(&nonce).bind(&row.id).execute(&mut *tx).await?;
            (issue.device_id, nonce, issue.expires_at)
        }
    };
    tx.commit().await?;
    Ok(Json(CredentialBody {
        device_id,
        access_token: auth::derive_secret(&app.access.key, Purpose::Device, &nonce),
        expires_at: timestamp(expires),
    })
    .into_response())
}
