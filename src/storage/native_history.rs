//! Motion-owned row retention only. No frozen catalog, backup, or disk cleanup.
use crate::App;
use anyhow::ensure;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) async fn worker(app: App, stop: CancellationToken) -> anyhow::Result<()> {
    ensure!(
        app.native_catalog.is_some(),
        "native history requires admitted native runtime"
    );
    loop {
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
        match cycle(&app, &stop).await {
            Ok(false) => return Ok(()),
            Ok(true) => {}
            Err(error) => {
                // Preserve the legacy maintenance retry policy, without stopping
                // the service or treating a failed transaction as successful.
                tracing::warn!(%error, "native application history retention failed");
                if !stop.is_cancelled() {
                    let _ = super::record(&app, "history", Some("history_prune_failed")).await;
                }
            }
        }
    }
}

async fn cycle(app: &App, stop: &CancellationToken) -> anyhow::Result<bool> {
    let _jobs = tokio::select! {
        biased;
        _ = stop.cancelled() => return Ok(false),
        guard = app.jobs.lock() => guard,
    };
    let mut writer = tokio::select! {
        biased;
        _ = stop.cancelled() => return Ok(false),
        writer = app.db.begin_with("BEGIN IMMEDIATE") => writer?,
    };
    if stop.is_cancelled() {
        writer.rollback().await?;
        return Ok(false);
    }
    // After admission the worker owns the complete small transaction through
    // commit/rollback. Stop joins it; no detached writes or filesystem effects.
    let cutoff = crate::now() - app.storage.settings.history_retention_seconds as i64;
    prune_in(&mut writer, cutoff).await?;
    sqlx::query("INSERT INTO maintenance_state VALUES ('history',?,?,NULL) ON CONFLICT(name) DO UPDATE SET last_success=excluded.last_success,last_attempt=excluded.last_attempt,error=NULL")
        .bind(crate::now()).bind(crate::now()).execute(&mut *writer).await?;
    writer.commit().await?;
    Ok(true)
}

async fn prune_in(db: &mut sqlx::SqliteConnection, cutoff: i64) -> anyhow::Result<()> {
    // Keep the latest authoritative viewing session and its legacy-write barrier.
    sqlx::query("DELETE FROM playback_sessions WHERE id IN (SELECT id FROM playback_sessions WHERE updated_at<? AND status IN ('ended','stopped','superseded','invalidated') AND id NOT IN (SELECT session_id FROM viewing_state WHERE session_id IS NOT NULL) ORDER BY updated_at LIMIT 500)")
        .bind(cutoff).execute(&mut *db).await?;
    // Native publication rows are permanent provenance. Unlike a merely expired
    // cache, their job is still required by exact rendition receipt validation.
    sqlx::query("DELETE FROM processing_jobs WHERE id IN (SELECT id FROM processing_jobs WHERE updated_at<? AND cache_cleaned=1 AND id NOT IN (SELECT job_id FROM preparations) AND id NOT IN (SELECT job_id FROM catabolic_generated_publications) AND phase IN ('completed','failed','cancelled') ORDER BY updated_at LIMIT 500)")
        .bind(cutoff).execute(&mut *db).await?;
    Ok(())
}

#[cfg(test)]
use crate as playscale_app;

#[cfg(test)]
#[path = "../../tests/support/native_application_fixture.rs"]
mod fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    async fn cancelled_waiter_releases_jobs_admission_without_writes() {
        use std::future::Future;
        let mut fixture = fixture::Fixture::new().await;
        fixture.cutover().await;
        let held = fixture.app.jobs.lock().await;
        let stop = CancellationToken::new();
        let mut attempt = Box::pin(cycle(&fixture.app, &stop));
        assert!(
            attempt
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        );
        stop.cancel();
        assert!(!attempt.await.unwrap());
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM maintenance_state WHERE name='history'")
                .fetch_one(&fixture.app.db)
                .await
                .unwrap();
        assert_eq!(count, 0);
        drop(held);
        assert!(fixture.app.jobs.try_lock().is_ok());
        fixture.close().await;
    }

    // Predicate tests use application rows only, not synthetic native authority.
    async fn schema() -> sqlx::SqliteConnection {
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE playback_sessions(id TEXT PRIMARY KEY,updated_at INTEGER,status TEXT); CREATE TABLE viewing_state(session_id TEXT); CREATE TABLE processing_jobs(id TEXT PRIMARY KEY,updated_at INTEGER,cache_cleaned INTEGER,phase TEXT); CREATE TABLE preparations(job_id TEXT); CREATE TABLE catabolic_generated_publications(job_id TEXT,acknowledged INTEGER); CREATE TABLE jobs(id TEXT); INSERT INTO jobs VALUES('frozen'); CREATE TRIGGER frozen_jobs BEFORE DELETE ON jobs BEGIN SELECT RAISE(ABORT,'frozen catalog'); END;").execute(&mut db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn retains_current_history_preparations_and_all_publication_phases() {
        let mut db = schema().await;
        for (id, status, updated) in [
            ("old", "ended", 1),
            ("barrier", "ended", 1),
            ("live", "playing", 1),
            ("recent", "stopped", 100),
        ] {
            sqlx::query("INSERT INTO playback_sessions VALUES(?,?,?)")
                .bind(id)
                .bind(updated)
                .bind(status)
                .execute(&mut db)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO viewing_state VALUES('barrier'),(NULL)")
            .execute(&mut db)
            .await
            .unwrap();
        for (id, phase, clean, updated) in [
            ("delete", "failed", 1, 1),
            ("preparation", "completed", 1, 1),
            ("pending", "completed", 1, 1),
            ("ack", "completed", 1, 1),
            ("active", "running", 1, 1),
            ("dirty", "cancelled", 0, 1),
            ("recent", "failed", 1, 100),
        ] {
            sqlx::query("INSERT INTO processing_jobs VALUES(?,?,?,?)")
                .bind(id)
                .bind(updated)
                .bind(clean)
                .bind(phase)
                .execute(&mut db)
                .await
                .unwrap();
        }
        sqlx::raw_sql("INSERT INTO preparations VALUES('preparation'); INSERT INTO catabolic_generated_publications VALUES('pending',0),('ack',1);").execute(&mut db).await.unwrap();
        prune_in(&mut db, 50).await.unwrap();
        let sessions: Vec<String> =
            sqlx::query_scalar("SELECT id FROM playback_sessions ORDER BY id")
                .fetch_all(&mut db)
                .await
                .unwrap();
        assert_eq!(sessions, ["barrier", "live", "recent"]);
        let jobs: Vec<String> = sqlx::query_scalar("SELECT id FROM processing_jobs ORDER BY id")
            .fetch_all(&mut db)
            .await
            .unwrap();
        assert_eq!(
            jobs,
            ["ack", "active", "dirty", "pending", "preparation", "recent"]
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT id FROM jobs")
                .fetch_one(&mut db)
                .await
                .unwrap(),
            "frozen"
        );
    }

    #[tokio::test]
    async fn each_application_table_has_a_500_row_batch_bound() {
        let mut db = schema().await;
        sqlx::raw_sql("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<501) INSERT INTO playback_sessions SELECT CAST(i AS TEXT),1,'stopped' FROM n; WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<501) INSERT INTO processing_jobs SELECT CAST(i AS TEXT),1,1,'cancelled' FROM n;").execute(&mut db).await.unwrap();
        prune_in(&mut db, 50).await.unwrap();
        for table in ["playback_sessions", "processing_jobs"] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&mut db)
                .await
                .unwrap();
            assert_eq!(count, 1);
        }
    }
}
