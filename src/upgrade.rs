//! Idempotent data upgrades that follow schema migrations. Each step runs in one
//! writer transaction and records an immutable receipt; a recorded step never reruns.
use crate::{db::UpgradeBackup, new_id, now};
use playscale_core::identity::{Attribution, attribute_legacy_progress};
use sqlx::SqlitePool;
use std::collections::BTreeMap;

const LEGACY_PROGRESS: &str = "migration:legacy_progress";

pub async fn run(db: &SqlitePool, backup: Option<&UpgradeBackup>) -> anyhow::Result<()> {
    // Data steps target the identity schema; an older migrator (tests, or a
    // partially restored copy) has nothing for them to do yet.
    let ready: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('catalog_receipts','legacy_progress_attribution')",
    )
    .fetch_one(db)
    .await?;
    if ready < 2 {
        return Ok(());
    }
    if let Some(backup) = backup {
        record(
            db,
            "migration:schema",
            &serde_json::json!({
                "from_version": backup.from_version,
                "to_version": backup.to_version,
                "verified_backup": backup.path,
            }),
        )
        .await?;
    }
    legacy_progress(db).await
}

async fn record(db: &SqlitePool, kind: &str, document: &serde_json::Value) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO catalog_receipts VALUES (?,?,?,?)")
        .bind(new_id())
        .bind(kind)
        .bind(now())
        .bind(document.to_string())
        .execute(db)
        .await?;
    Ok(())
}

/// Attribute work-keyed legacy progress to one timeline only when exactly one
/// legacy timeline (edition) holds original media. Ambiguous and orphaned records
/// are preserved with their candidates for user correction; nothing is copied.
async fn legacy_progress(db: &SqlitePool) -> anyhow::Result<()> {
    let mut tx = crate::db::begin_write(db).await?;
    let done: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog_receipts WHERE kind=?")
        .bind(LEGACY_PROGRESS)
        .fetch_one(&mut *tx)
        .await?;
    if done > 0 {
        return Ok(());
    }
    let keys: Vec<(String, String)> = sqlx::query_as(
        "SELECT profile_id,item_id FROM progress UNION SELECT profile_id,item_id FROM viewing_state ORDER BY 1,2",
    )
    .fetch_all(&mut *tx)
    .await?;
    let receipt = new_id();
    let mut counts = BTreeMap::<&str, u64>::new();
    let mut rows = Vec::new();
    for (profile, item) in keys {
        let timelines: Vec<(String, i64)> = sqlx::query_as(
            "SELECT e.id,(SELECT count(*) FROM media_files f WHERE f.edition_id=e.id AND f.generated=0) FROM editions e WHERE e.item_id=? ORDER BY e.id",
        )
        .bind(&item)
        .fetch_all(&mut *tx)
        .await?;
        let candidates: Vec<(String, usize)> = timelines
            .into_iter()
            .map(|(id, n)| (id, usize::try_from(n).unwrap_or(0)))
            .collect();
        let attribution = attribute_legacy_progress(&candidates);
        let (outcome, timeline) = match &attribution {
            Attribution::Exact { timeline } => ("exact", Some(timeline.clone())),
            Attribution::Ambiguous { .. } => ("ambiguous", None),
            Attribution::Orphaned => ("orphaned", None),
        };
        *counts.entry(outcome).or_default() += 1;
        rows.push((profile, item, outcome, timeline, candidates));
    }
    sqlx::query("INSERT INTO catalog_receipts VALUES (?,?,?,?)")
        .bind(&receipt)
        .bind(LEGACY_PROGRESS)
        .bind(now())
        .bind(serde_json::json!({ "outcomes": counts }).to_string())
        .execute(&mut *tx)
        .await?;
    for (profile, item, outcome, timeline, candidates) in rows {
        sqlx::query("INSERT INTO legacy_progress_attribution VALUES (?,?,?,?,?,?)")
            .bind(profile)
            .bind(item)
            .bind(outcome)
            .bind(timeline)
            .bind(serde_json::to_string(&candidates)?)
            .bind(&receipt)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
