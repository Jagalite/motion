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
pub async fn run_one(app: &App) -> anyhow::Result<bool> {
    let selected = {
        let _guard = app.jobs.lock().await;
        let mut tx = crate::db::begin_write(&app.db).await?;
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
    use playscale_core::metadata::{RefreshResult, finish_refresh, refresh_may_publish};
    let mut completion = finish_refresh(&current, attempt, permitted, &RefreshResult::Unavailable);
    if refresh_may_publish(&current, attempt, permitted, observed.is_ok())
        && let Ok(observed) = &observed
    {
        let identities: Vec<String> = observed.iter().map(|(row, _)| row.0.clone()).collect();
        let usable: Vec<String> = observed
            .iter()
            .filter(|(_, sidecar)| !matches!(sidecar, crate::nfo::Sidecar::Unusable))
            .map(|(row, _)| row.0.clone())
            .collect();
        let mut publication = tx.begin().await?;
        let result = match crate::nfo::publish_refresh_in(&mut publication, &item, observed).await {
            Ok(published) => RefreshResult::Acknowledged {
                observed: identities,
                usable,
                published,
            },
            Err(_) => RefreshResult::Unavailable,
        };
        completion = finish_refresh(&current, attempt, permitted, &result);
        if completion
            .as_ref()
            .is_some_and(|decision| decision.commit_publication)
        {
            publication.commit().await?;
        } else {
            publication.rollback().await?;
        }
    }
    persist_completion(&mut tx, &id, completion).await?;
    tx.commit().await?;
    Ok(true)
}

/// Apply the core completion in the caller's transaction. No decision means an
/// obsolete worker, which must not update phase, error or revision.
async fn persist_completion(
    conn: &mut SqliteConnection,
    id: &str,
    completion: Option<playscale_core::metadata::RefreshCompletion>,
) -> anyhow::Result<()> {
    if let Some(completion) = completion {
        sqlx::query("UPDATE metadata_jobs SET phase=?,error=? WHERE id=?")
            .bind(crate::db::phase_name(completion.job.phase))
            .bind(completion.error)
            .bind(id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod completion_adapter_tests {
    use super::*;
    #[tokio::test]
    async fn obsolete_completion_preserves_phase_error_and_revision() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE metadata_jobs (id TEXT PRIMARY KEY, phase TEXT, error TEXT, revision INTEGER)")
            .execute(&pool).await.unwrap();
        sqlx::query("CREATE TRIGGER bump AFTER UPDATE ON metadata_jobs BEGIN UPDATE metadata_jobs SET revision=revision+1 WHERE id=NEW.id; END")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO metadata_jobs VALUES ('job','running','current error',7)")
            .execute(&pool)
            .await
            .unwrap();
        let current = Job {
            phase: jobs::Phase::Running,
            attempt: 2,
        };
        let completion = playscale_core::metadata::finish_refresh(
            &current,
            1,
            true,
            &playscale_core::metadata::RefreshResult::Unavailable,
        );
        let mut tx = pool.begin().await.unwrap();
        persist_completion(&mut tx, "job", completion)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let row: (String, Option<String>, i64) =
            sqlx::query_as("SELECT phase,error,revision FROM metadata_jobs WHERE id='job'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, ("running".into(), Some("current error".into()), 7));
    }
}
