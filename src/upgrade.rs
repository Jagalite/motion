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
    legacy_progress(db).await?;
    timeline_progress(db).await
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
/// timeline holds an original version. Ambiguous and orphaned records
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
            "SELECT t.id,(SELECT count(*) FROM media_versions v WHERE v.timeline_id=t.id AND v.origin='original') FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE e.item_id=? ORDER BY t.id",
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

/// Forward repair for 0032 having run before legacy attribution on direct
/// upgrades. Receipt and inserts commit together; startup has not admitted any
/// writers, and the reserved transaction also fences competing openers.
async fn timeline_progress(db: &SqlitePool) -> anyhow::Result<()> {
    use playscale_core::timeline_viewing::{View, import_legacy};
    const REPAIR: &str = "migration:timeline_progress_repair";
    let mut tx = crate::db::begin_write(db).await?;
    let ready: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='timeline_viewing'",
    )
    .fetch_one(&mut *tx)
    .await?;
    if ready == 0 {
        return Ok(());
    }
    let done: i64 = sqlx::query_scalar("SELECT count(*) FROM catalog_receipts WHERE kind=?")
        .bind(REPAIR)
        .fetch_one(&mut *tx)
        .await?;
    if done > 0 {
        return Ok(());
    }
    // Retract only an untouched 0032 single-empty-cut fallback. Frozen
    // attribution and the exact backfill image fence against newer authority.
    type Suspect = (
        String,
        String,
        i64,
        i64,
        i64,
        bool,
        Option<bool>,
        Option<String>,
        i64,
        f64,
        bool,
        Option<bool>,
        i64,
        bool,
    );
    let suspects: Vec<Suspect> = sqlx::query_as(
        "SELECT a.profile_id,t.id,v.revision,v.manual_epoch,v.position_ms,v.automatic_watched,v.manual_watched,v.session_id,v.updated_at,coalesce(p.position_seconds,0),coalesce(l.automatic_watched,0),l.manual_watched,coalesce(p.updated_at,0),EXISTS(SELECT 1 FROM viewing_sessions s WHERE s.profile_id=a.profile_id AND s.timeline_id=t.id)
         FROM legacy_progress_attribution a JOIN editions e ON e.item_id=a.item_id JOIN timelines t ON t.edition_id=e.id
         JOIN timeline_viewing v ON v.profile_id=a.profile_id AND v.timeline_id=t.id
         LEFT JOIN progress p ON p.profile_id=a.profile_id AND p.item_id=a.item_id
         LEFT JOIN viewing_state l ON l.profile_id=a.profile_id AND l.item_id=a.item_id
         WHERE a.outcome='orphaned' AND json_array_length(a.candidates_json)=1
           AND json_extract(a.candidates_json,'$[0][0]')=t.id AND json_extract(a.candidates_json,'$[0][1]')=0")
        .fetch_all(&mut *tx).await?;
    let mut retracted = 0;
    for (
        profile,
        timeline,
        revision,
        epoch,
        position,
        automatic,
        manual,
        session,
        updated,
        seconds,
        old_automatic,
        old_manual,
        old_updated,
        has_sessions,
    ) in suspects
    {
        let current = View {
            revision: revision as u64,
            manual_epoch: epoch as u64,
            position_ms: position as u64,
            automatic_watched: automatic,
            manual_watched: manual,
            session,
        };
        let backfill = View {
            position_ms: (seconds.max(0.0) * 1000.0).round() as u64,
            automatic_watched: old_automatic,
            manual_watched: old_manual,
            ..View::default()
        };
        if playscale_core::timeline_viewing::retract_unattributed_legacy(
            &Attribution::Orphaned,
            &current,
            &backfill,
            has_sessions,
            updated,
            old_updated,
        ) {
            sqlx::query("DELETE FROM timeline_viewing WHERE profile_id=? AND timeline_id=?")
                .bind(profile)
                .bind(timeline)
                .execute(&mut *tx)
                .await?;
            retracted += 1;
        }
    }
    let rows: Vec<(String,String,f64,bool,Option<bool>,i64)> = sqlx::query_as(
        "SELECT a.profile_id,a.timeline_id,coalesce(p.position_seconds,0),coalesce(v.automatic_watched,0),v.manual_watched,coalesce(p.updated_at,0)
         FROM legacy_progress_attribution a
         JOIN profiles ON profiles.id=a.profile_id
         JOIN timelines t ON t.id=a.timeline_id JOIN editions e ON e.id=t.edition_id AND e.item_id=a.item_id
         LEFT JOIN progress p ON p.profile_id=a.profile_id AND p.item_id=a.item_id
         LEFT JOIN viewing_state v ON v.profile_id=a.profile_id AND v.item_id=a.item_id
         WHERE a.outcome='exact'")
        .fetch_all(&mut *tx).await?;
    let mut imported = 0;
    for (profile, timeline, seconds, automatic, manual, updated) in rows {
        // Invalid legacy values remain recoverable in the original tables.
        if !seconds.is_finite()
            || seconds < 0.0
            || seconds * 1000.0 > playscale_core::timeline_viewing::MAX_POSITION_MS as f64
        {
            continue;
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM timeline_viewing WHERE profile_id=? AND timeline_id=?)",
        )
        .bind(&profile)
        .bind(&timeline)
        .fetch_one(&mut *tx)
        .await?;
        let existing = exists.then(View::default);
        let Some(view) = import_legacy(
            &Attribution::Exact {
                timeline: timeline.clone(),
            },
            existing.as_ref(),
            (seconds * 1000.0).round() as u64,
            automatic,
            manual,
        ) else {
            continue;
        };
        sqlx::query("INSERT INTO timeline_viewing(profile_id,timeline_id,revision,manual_epoch,position_ms,automatic_watched,manual_watched,session_id,updated_at) VALUES (?,?,0,0,?,?,?,NULL,?)")
            .bind(profile).bind(timeline).bind(view.position_ms as i64).bind(view.automatic_watched).bind(view.manual_watched).bind(updated).execute(&mut *tx).await?;
        imported += 1;
    }
    sqlx::query("INSERT INTO catalog_receipts VALUES (?,?,?,?)")
        .bind(new_id())
        .bind(REPAIR)
        .bind(now())
        .bind(
            serde_json::json!({"imported":imported,"retracted_unattributed":retracted}).to_string(),
        )
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
