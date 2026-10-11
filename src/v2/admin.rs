//! Administrative storage accounting and durable, idempotent backup requests.
use super::{Body, PageQuery, Problem, Scope, auth::Caller, idempotency_key, page};
use crate::{App, db::begin_write};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use playscale_core::{
    access::{self, Idempotent, Permission},
    jobs::{self, Effect, Input, Job},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/admin/storage", get(storage))
        .route("/admin/backups", get(list).post(create))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupInput {
    include_generated_media: bool,
}
#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    phase: String,
    attempt: i64,
    requester_id: String,
    credential_hash: Option<String>,
    ingress_id: Option<String>,
    include_generated_media: bool,
    created_at: i64,
    manifest_digest: Option<String>,
    error: Option<String>,
}
impl Row {
    fn wire(&self) -> Value {
        json!({"id":self.id,"job_id":self.id,"created_at":super::timestamp(self.created_at),"status":match self.phase.as_str(){"completed" if self.error.is_none()=>"ready","completed"|"failed"|"cancelled"=>"failed",_=>"pending"},"manifest_digest":if self.phase=="completed" && self.error.is_none(){self.manifest_digest.clone()}else{None}})
    }
}
async fn create(
    State(app): State<App>,
    caller: Caller,
    headers: HeaderMap,
    body: Body<BackupInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let key = idempotency_key(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SystemAdmin))
        .await?;
    let record = Scope {
        principal: &p.id,
        operation: "createBackup",
        target: "",
        key: &key,
    };
    let old = record.load(&mut tx).await?;
    if access::idempotency(old.as_ref().map(|s| &s.record), &body.digest, crate::now())?
        == Idempotent::Replay
    {
        let old = old.unwrap();
        let mut r = (old.status, Json(old.body)).into_response();
        r.headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(r);
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM backup_jobs WHERE phase IN ('queued','running','cancelling')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if pending >= 100 {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "backup_limit",
            "Backup request queue is full",
        ));
    }
    let id = crate::new_id();
    sqlx::query("INSERT INTO backup_jobs(id,requester_id,credential_hash,ingress_id,include_generated_media,created_at) VALUES (?,?,?,?,?,?)").bind(&id).bind(&p.id).bind(caller.credential.as_ref().map(|(hash,_)|hash)).bind(&caller.ingress).bind(body.value.include_generated_media).bind(crate::now()).execute(&mut *tx).await?;
    let row: Row = sqlx::query_as("SELECT * FROM backup_jobs WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let value = row.wire();
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::ACCEPTED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(value)).into_response())
}
async fn list(
    State(app): State<App>,
    caller: Caller,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let limit = q.limit()?;
    let mut tx = app.db.begin().await?;
    let rows: Vec<Row> = sqlx::query_as("SELECT * FROM backup_jobs WHERE id>? ORDER BY id LIMIT ?")
        .bind(q.after()?)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await?;
    let result = page(
        &mut tx,
        &caller.principal,
        rows.into_iter().map(|r| (r.id.clone(), r.wire())).collect(),
        limit,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}
async fn storage(State(app): State<App>, caller: Caller) -> Result<Json<Value>, Problem> {
    caller.require(Permission::SystemAdmin)?;
    let permit = app
        .processing
        .inspection
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_busy",
                "Storage inspection is busy",
            )
        })?;
    let mut tx = app.db.begin().await?;
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&mut *tx)
        .await?;
    let size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&mut *tx)
        .await?;
    let assets: i64 =
        sqlx::query_scalar("SELECT coalesce(sum(length(bytes)),0) FROM artwork_assets")
            .fetch_one(&mut *tx)
            .await?;
    let generated: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(bytes),0) FROM media_files WHERE generated=1 AND available=1",
    )
    .fetch_one(&mut *tx)
    .await?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM processing_jobs WHERE phase IN ('running','cancelling')",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let root = app.storage.root.clone();
    let cache = app.processing.root.clone();
    let (cache_bytes, free) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        (
            crate::processing::cache_bytes(&cache),
            fs2::available_space(root).ok(),
        )
    })
    .await
    .map_err(Problem::internal)?;
    let cache_bytes = cache_bytes.map_err(Problem::internal)?;
    Ok(Json(
        json!({"database_bytes":(pages.max(0) as u64).saturating_mul(size.max(0) as u64).to_string(),"asset_bytes":(assets.max(0) as u64).saturating_add(generated.max(0) as u64).to_string(),"temporary_bytes":cache_bytes.saturating_sub(generated.max(0) as u64).to_string(),"reserved_bytes":app.storage.settings.min_free_bytes.saturating_add((active.max(0) as u64).saturating_mul(app.processing.settings.max_output_bytes)).to_string(),"free_bytes":free.map(|v|v.to_string())}),
    ))
}

pub async fn run_backup(app: &App) -> anyhow::Result<bool> {
    let Ok(permit) = app.storage.backup_io.clone().try_acquire_owned() else {
        return Ok(false);
    };
    let permit = std::sync::Arc::new(permit);
    retain_requested(app).await?;
    let selected = {
        let _guard = app.jobs.lock().await;
        let mut tx = begin_write(&app.db).await?;
        let row:Option<Row>=sqlx::query_as("SELECT * FROM backup_jobs WHERE phase IN ('queued','running','cancelling') ORDER BY created_at,id LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(mut row) = row else { return Ok(false) };
        let current = Job {
            phase: serde_json::from_value(json!(row.phase))?,
            attempt: row.attempt.try_into()?,
        };
        let (next, _) = if current.phase == jobs::Phase::Queued {
            jobs::transition(&current, Input::Start)
        } else {
            (current, vec![])
        };
        sqlx::query("UPDATE backup_jobs SET phase=?,attempt=? WHERE id=?")
            .bind(crate::db::phase_name(next.phase))
            .bind(next.attempt)
            .bind(&row.id)
            .execute(&mut *tx)
            .await?;
        row.attempt = next.attempt.into();
        row.phase = crate::db::phase_name(next.phase).into();
        let authorized = crate::metadata_jobs::principal(
            &mut tx,
            &row.requester_id,
            row.credential_hash.as_deref(),
            row.ingress_id.as_deref(),
            Permission::SystemAdmin,
        )
        .await
        .is_ok();
        tx.commit().await?;
        (row, authorized)
    };
    let (row, authorized) = selected;
    let receipt = if authorized && row.phase == "running" {
        crate::storage::requested_backup(app, &row.id, row.include_generated_media, permit.clone())
            .await
    } else {
        Err(anyhow::anyhow!(
            "backup authorization revoked or cancellation requested"
        ))
    };
    let _guard = app.jobs.lock().await;
    let mut tx = begin_write(&app.db).await?;
    let (phase, attempt): (String, i64) =
        sqlx::query_as("SELECT phase,attempt FROM backup_jobs WHERE id=?")
            .bind(&row.id)
            .fetch_one(&mut *tx)
            .await?;
    let authorized = crate::metadata_jobs::principal(
        &mut tx,
        &row.requester_id,
        row.credential_hash.as_deref(),
        row.ingress_id.as_deref(),
        Permission::SystemAdmin,
    )
    .await
    .is_ok();
    let current = Job {
        phase: serde_json::from_value(json!(phase))?,
        attempt: attempt.try_into()?,
    };
    let (next, effects) = jobs::transition(
        &current,
        Input::Finished {
            attempt: row.attempt.try_into()?,
            success: playscale_core::maintenance::BackupReceiptFacts {
                requester_authorized: authorized,
                database_verified: receipt.as_ref().is_ok_and(|r| r.database_verified),
                inventory_complete: receipt.as_ref().is_ok_and(|r| r.inventory_complete),
                destination_durable: receipt.as_ref().is_ok_and(|r| r.destination_durable),
            }
            .publishable(),
        },
    );
    let published = effects.iter().any(|e| matches!(e, Effect::Publish { .. }));
    sqlx::query("UPDATE backup_jobs SET phase=?,manifest_digest=?,error=? WHERE id=?")
        .bind(crate::db::phase_name(next.phase))
        .bind(if published {
            receipt.as_ref().ok().map(|r| &r.manifest_digest)
        } else {
            None
        })
        .bind(if receipt.is_err() || !authorized {
            Some("backup_failed")
        } else {
            None
        })
        .bind(&row.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    drop(_guard);
    retain_requested(app).await?;
    Ok(true)
}

async fn retain_requested(app: &App) -> anyhow::Result<()> {
    let ids:Vec<String> = sqlx::query_scalar("SELECT id FROM backup_jobs WHERE phase='completed' AND error='backup_expired' UNION SELECT id FROM (SELECT id FROM backup_jobs WHERE phase='completed' AND error IS NULL ORDER BY created_at DESC,id DESC LIMIT -1 OFFSET ?) LIMIT 500").bind(app.storage.settings.backups_keep as i64).fetch_all(&app.db).await?;
    for id in ids {
        // Withdraw the ready claim before removing bytes; retain job provenance.
        let mut tx = begin_write(&app.db).await?;
        sqlx::query(
            "UPDATE backup_jobs SET error='backup_expired' WHERE id=? AND phase='completed'",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let path = app
            .storage
            .root
            .join("backups")
            .join(format!("request-{id}"));
        let removed = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if path.exists() {
                anyhow::ensure!(
                    !std::fs::symlink_metadata(&path)?.file_type().is_symlink()
                        && std::fs::read(path.join(".playscale-backup"))?
                            == b"playscale-scheduled-v1",
                    "unowned backup destination"
                );
                std::fs::remove_dir_all(&path)?;
                #[cfg(unix)]
                std::fs::File::open(path.parent().unwrap())?.sync_all()?;
            }
            Ok(())
        })
        .await?;
        match removed {
            Err(error) => tracing::warn!(%error,"expired backup cleanup will retry"),
            Ok(()) => {
                sqlx::query("UPDATE backup_jobs SET error='backup_reclaimed' WHERE id=? AND error='backup_expired'").bind(&id).execute(&app.db).await?;
            }
        }
    }
    Ok(())
}
