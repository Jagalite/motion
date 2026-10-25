//! Durable local NFO refresh. The existing job reducer fences attempts and
//! cancellation; current authorization and source facts gate publication in
//! the same reserved transaction as the completion acknowledgement.
use crate::{
    App,
    v2::{Problem, auth},
};
use playscale_core::{
    access::{self, Permission, Principal},
    jobs::{self, Effect, Input, Job},
};
use sqlx::{Acquire, SqliteConnection, SqlitePool};

pub async fn recover(db: &SqlitePool) -> anyhow::Result<()> {
    let mut tx = crate::db::begin_write(db).await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT id,phase,attempt FROM metadata_jobs WHERE phase IN ('running','cancelling')",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, phase, attempt) in rows {
        let job = Job {
            phase: serde_json::from_value(serde_json::json!(phase))?,
            attempt: attempt.try_into()?,
        };
        let (next, _) = jobs::transition(&job, Input::Recover);
        sqlx::query("UPDATE metadata_jobs SET phase=? WHERE id=?")
            .bind(crate::db::phase_name(next.phase))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
pub(crate) async fn principal(
    tx: &mut SqliteConnection,
    id: &str,
    hash: Option<&str>,
    ingress: Option<&str>,
    permission: Permission,
) -> Result<Principal, Problem> {
    let p = if let Some(hash) = hash {
        let stored = auth::load_credential(tx, hash)
            .await?
            .ok_or_else(Problem::not_found)?;
        auth::authenticate_stored(tx, &stored, crate::now())
            .await?
            .0
    } else if let Some(id) = ingress {
        let device = auth::load_device(tx, id).await?;
        access::authenticate_ingress(device.as_ref().map(|d| &d.device))?
    } else if id == "operator" {
        Principal::operator()
    } else {
        return Err(Problem::not_found());
    };
    if !p.allows(permission) {
        return Err(access::AccessError::Forbidden(permission).into());
    }
    Ok(p)
}
/// A queued request remains intact until a qualified native publisher can own it.
#[derive(Debug)]
pub struct NativeRefreshUnavailable;
impl std::fmt::Display for NativeRefreshUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "native_metadata_refresh_unavailable: native refresh publication is not yet admitted",
        )
    }
}
impl std::error::Error for NativeRefreshUnavailable {}

pub async fn run_one(app: &App) -> anyhow::Result<bool> {
    let selected = {
        let _guard = app.jobs.lock().await;
        let mut tx = crate::db::begin_write(&app.db).await?;
        let native_bound: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_binding)")
                .fetch_one(&mut *tx)
                .await?;
        if app.native_catalog.is_some() || native_bound {
            // Check before job selection, attempt advance or filesystem observation.
            // Runtime absence is not permission to fall back to legacy catalog writes.
            return Err(NativeRefreshUnavailable.into());
        }
        type QueuedRow = (String, String, i64, String, Option<String>, Option<String>);
        let row:Option<QueuedRow>=sqlx::query_as("SELECT id,item_id,attempt,requester_id,credential_hash,ingress_id FROM metadata_jobs WHERE phase='queued' ORDER BY created_at,id LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some((id, item, attempt, owner, hash, ingress)) = row else {
            return Ok(false);
        };
        let job = Job {
            phase: jobs::Phase::Queued,
            attempt: attempt.try_into()?,
        };
        let (next, effects) = jobs::transition(&job, Input::Start);
        sqlx::query("UPDATE metadata_jobs SET phase=?,attempt=? WHERE id=?")
            .bind(crate::db::phase_name(next.phase))
            .bind(next.attempt)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if !matches!(effects.as_slice(), [Effect::Run { .. }]) {
            return Ok(true);
        }
        (id, item, next.attempt, owner, hash, ingress)
    };
    let (id, item, attempt, owner, hash, ingress) = selected;
    let observed = crate::nfo::observe_item(app, &item).await;
    let _guard = app.jobs.lock().await;
    let mut tx = crate::db::begin_write(&app.db).await?;
    let native_bound: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_binding)")
        .fetch_one(&mut *tx)
        .await?;
    if app.native_catalog.is_some() || native_bound {
        return Err(NativeRefreshUnavailable.into());
    }
    let (phase, current): (String, i64) =
        sqlx::query_as("SELECT phase,attempt FROM metadata_jobs WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
    let current = Job {
        phase: serde_json::from_value(serde_json::json!(phase))?,
        attempt: current.try_into()?,
    };
    let permitted = match principal(
        &mut tx,
        &owner,
        hash.as_deref(),
        ingress.as_deref(),
        Permission::CatalogWrite,
    )
    .await
    {
        Ok(p) => {
            crate::v2::catalog::curatable_item(&mut tx, &access::catalog_scope(&p), &item, false)
                .await
                .is_ok()
        }
        Err(_) => false,
    };
    // Provisional writes live in a savepoint until core confirms completeness.
    // Any unusable/stale input or adapter error rolls back the entire refresh.
    let mut success = false;
    if permitted
        && current.executing(attempt)
        && let Ok(observed) = &observed
    {
        let unusable = observed
            .iter()
            .filter(|(_, s)| matches!(s, crate::nfo::Sidecar::Unusable))
            .count();
        let mut publication = tx.begin().await?;
        match crate::nfo::publish_refresh_in(&mut publication, &item, observed).await {
            Ok(count)
                if playscale_core::metadata::refresh_complete(
                    observed.len(),
                    unusable,
                    count,
                    permitted,
                ) =>
            {
                publication.commit().await?;
                success = true;
            }
            _ => {
                publication.rollback().await?;
            }
        }
    }
    let (next, _) = jobs::transition(&current, Input::Finished { attempt, success });
    sqlx::query("UPDATE metadata_jobs SET phase=?,error=? WHERE id=?")
        .bind(crate::db::phase_name(next.phase))
        .bind((!success).then_some("refresh_unavailable_or_unauthorized"))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}
