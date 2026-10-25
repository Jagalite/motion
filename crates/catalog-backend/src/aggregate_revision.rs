//! Durable aggregate revision issuance, separate from disposable projections.
//! Not yet wired to serving: callers must first implement complete, freshly held
//! native dependency capture. The table is not part of active migrations yet.
mod owner;
use anyhow::{Context, ensure};
pub use owner::{issue_work_in, may_curate_work_in};
use playscale_core::catalog_aggregate_revision::{CompleteWorkSnapshot, IssuedRevision};
use sqlx::{Sqlite, Transaction};

/// Caller retains the native owner and a Motion writer reservation from fresh
/// complete capture through this transaction's commit. Request/DTO fields are
/// never an admissible capture. This stays crate-private until that owner exists.
#[allow(dead_code)]
pub(crate) async fn issue_in(
    tx: &mut Transaction<'_, Sqlite>,
    motion_baseline: u64,
    capture: &CompleteWorkSnapshot,
) -> anyhow::Result<IssuedRevision> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT state_json FROM catabolic_aggregate_issuance WHERE work_id=?")
            .bind(&capture.work_id)
            .fetch_optional(&mut **tx)
            .await?;
    let prior: Option<IssuedRevision> = raw.as_deref().map(serde_json::from_str).transpose()?;
    let mut fresh = capture.clone();
    // The adapter, not the caller's snapshot, allocates the durable capture order.
    fresh.observation_epoch = prior
        .as_ref()
        .map_or(Some(1), |old| old.observation_epoch.checked_add(1))
        .context("aggregate capture order exhausted")?;
    let issued =
        playscale_core::catalog_aggregate_revision::issue(prior.as_ref(), motion_baseline, &fresh)
            .map_err(|reason| anyhow::anyhow!("aggregate revision refused: {reason:?}"))?;
    let state = serde_json::to_string(&issued)?;
    if let Some(raw) = raw {
        let changed = sqlx::query(
            "UPDATE catabolic_aggregate_issuance SET state_json=? WHERE work_id=? AND state_json=?",
        )
        .bind(&state)
        .bind(&fresh.work_id)
        .bind(raw)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        ensure!(changed == 1, "aggregate issuance changed during capture");
    } else {
        sqlx::query("INSERT INTO catabolic_aggregate_issuance(work_id,state_json) VALUES(?,?)")
            .bind(&fresh.work_id)
            .bind(&state)
            .execute(&mut **tx)
            .await?;
    }
    Ok(issued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use playscale_core::catalog_aggregate_revision::{Dependency, DependencyKind};
    fn capture() -> CompleteWorkSnapshot {
        CompleteWorkSnapshot {
            catalog_id: "catalog".into(),
            incarnation: "incarnation".into(),
            work_id: "work".into(),
            native_generation: 10,
            observation_epoch: 999,
            metadata_revision: 9,
            dependencies: vec![Dependency {
                kind: DependencyKind::Work,
                id: "work".into(),
                native_revision: Some(1),
                content_digest: "a".repeat(64),
            }],
        }
    }
    async fn database() -> sqlx::SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE catabolic_aggregate_issuance(work_id TEXT PRIMARY KEY,state_json TEXT NOT NULL)").execute(&db).await.unwrap();
        db
    }
    #[tokio::test]
    async fn issuance_survives_cache_loss_and_rollback_without_recycling_committed_tokens() {
        let db = database().await;
        let mut facts = capture();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let first = issue_in(&mut tx, 9, &facts).await.unwrap();
        assert_eq!((first.revision, first.observation_epoch), (9, 1));
        tx.commit().await.unwrap();
        // A query-only native generation change is not a work mutation.
        facts.native_generation += 1;
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let same = issue_in(&mut tx, 9, &facts).await.unwrap();
        assert_eq!((same.revision, same.observation_epoch), (9, 2));
        tx.commit().await.unwrap();
        facts.dependencies.push(Dependency {
            kind: DependencyKind::Timeline,
            id: "timeline".into(),
            native_revision: Some(2),
            content_digest: "b".repeat(64),
        });
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let changed = issue_in(&mut tx, 9, &facts).await.unwrap();
        assert_eq!((changed.revision, changed.observation_epoch), (10, 3));
        tx.rollback().await.unwrap();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let committed = issue_in(&mut tx, 9, &facts).await.unwrap();
        assert_eq!(committed, changed);
        tx.commit().await.unwrap();
        // No disposable projection exists in this DB. Durable issuance still
        // retains the prior token, while actual metadata CAS remains revision9.
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let reopened = issue_in(&mut tx, 9, &facts).await.unwrap();
        assert_eq!(reopened.revision, 10);
        assert_eq!(facts.metadata_revision, 9);
        tx.commit().await.unwrap();
    }
    #[tokio::test]
    async fn issuance_refuses_incarnation_baseline_and_generation_regressions_atomically() {
        let db = database().await;
        let facts = capture();
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        issue_in(&mut tx, 9, &facts).await.unwrap();
        tx.commit().await.unwrap();
        let original: String =
            sqlx::query_scalar("SELECT state_json FROM catabolic_aggregate_issuance")
                .fetch_one(&db)
                .await
                .unwrap();
        for case in 0..3 {
            let mut invalid = facts.clone();
            if case == 0 {
                invalid.incarnation = "restored".into();
            }
            if case == 1 {
                invalid.native_generation = 9;
            }
            let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
            assert!(
                issue_in(&mut tx, if case == 2 { 8 } else { 9 }, &invalid)
                    .await
                    .is_err()
            );
            tx.commit().await.unwrap();
            let retained: String =
                sqlx::query_scalar("SELECT state_json FROM catabolic_aggregate_issuance")
                    .fetch_one(&db)
                    .await
                    .unwrap();
            assert_eq!(retained, original);
        }
    }
}
