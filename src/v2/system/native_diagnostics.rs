//! Read-only operational observations. Recorded phases and admission rows are
//! not fresh catalog/source authority, execution eligibility or byte grants.
//! No native reader, catalog projection or recovery operation is invoked.
use super::Problem;

pub(super) async fn workload_in(
    conn: &mut sqlx::SqliteConnection,
) -> Result<(i64, i64, Vec<String>), Problem> {
    // Count physical native scan demands, not parent requests (one request can
    // cover several sources). Processing rows without native admission are
    // retained legacy history, not native workload. A retained admission here
    // is descriptive only; the worker still validates its entire typed proof.
    let (queued, running): (i64, i64) = sqlx::query_as(
        "SELECT coalesce(sum(phase='queued'),0),coalesce(sum(phase IN ('admitted','running','validating','publishing','cancelling')),0) FROM (
            SELECT d.phase FROM catabolic_scan_demands d JOIN catabolic_scan_requests r ON r.id=d.request_id WHERE d.status='pending'
            UNION ALL
            SELECT j.phase FROM processing_jobs j JOIN catabolic_processing_admissions a ON a.job_id=j.id
        )",
    ).fetch_one(&mut *conn).await?;
    // There is no scan-demand completion timestamp. Its immutable parent
    // creation time orders scan failures; processing uses its recorded update
    // time. Stable kind/ID ties make the bounded observation deterministic.
    // Never select worker error text, capture JSON, paths or credential data.
    let errors: Vec<String> = sqlx::query_scalar(
        "SELECT kind || ':' || id || ':failed' FROM (
            SELECT 'scan-demand' AS kind,d.id,r.created_at AS changed FROM catabolic_scan_demands d JOIN catabolic_scan_requests r ON r.id=d.request_id WHERE d.phase='failed'
            UNION ALL
            SELECT 'processing' AS kind,j.id,j.updated_at AS changed FROM processing_jobs j JOIN catabolic_processing_admissions a ON a.job_id=j.id WHERE j.phase='failed'
        ) ORDER BY changed DESC,kind,id LIMIT 100",
    ).fetch_all(conn).await?;
    Ok((queued, running, errors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    async fn database() -> sqlx::SqliteConnection {
        let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        // Intentionally no legacy jobs/api_jobs/catalog/projection/native DB.
        sqlx::raw_sql("CREATE TABLE catabolic_scan_requests(id TEXT PRIMARY KEY,created_at INTEGER);
            CREATE TABLE catabolic_scan_demands(id TEXT PRIMARY KEY,request_id TEXT,phase TEXT,status TEXT,error TEXT);
            CREATE TABLE processing_jobs(id TEXT PRIMARY KEY,phase TEXT,updated_at INTEGER,error TEXT);
            CREATE TABLE catabolic_processing_admissions(job_id TEXT PRIMARY KEY,capture_json TEXT);")
            .execute(&mut conn).await.unwrap();
        conn
    }

    #[tokio::test]
    async fn native_diagnostics_count_only_recorded_native_workload_without_legacy_authority() {
        let mut conn = database().await;
        assert_eq!(workload_in(&mut conn).await.unwrap(), (0, 0, vec![]));
        sqlx::raw_sql("INSERT INTO catabolic_scan_requests VALUES('request',10);
            INSERT INTO catabolic_scan_demands VALUES('scan-queued','request','queued','pending',NULL),
                ('scan-running','request','running','pending',NULL),('scan-cancelling','request','cancelling','pending',NULL),
                ('scan-complete','request','completed','complete',NULL),('scan-failed','request','failed','failed','/private/source secret'),
                ('terminal-is-not-queued','request','queued','cancelled',NULL);
            INSERT INTO processing_jobs VALUES('job-queued','queued',10,NULL),('job-running','running',10,NULL),
                ('job-validating','validating',10,NULL),('job-publishing','publishing',10,NULL),('job-cancelling','cancelling',10,NULL),
                ('job-failed','failed',11,'private subprocess secret'),('job-complete','completed',10,NULL),
                ('legacy-queued','queued',10,NULL),('legacy-failed','failed',12,'legacy worker secret');
            INSERT INTO catabolic_processing_admissions SELECT id,'{\"private_capture\":\"secret\"}' FROM processing_jobs WHERE id NOT LIKE 'legacy-%';")
            .execute(&mut conn).await.unwrap();
        let (queued, running, errors) = workload_in(&mut conn).await.unwrap();
        assert_eq!((queued, running), (2, 6));
        assert_eq!(
            errors,
            [
                "processing:job-failed:failed",
                "scan-demand:scan-failed:failed"
            ]
        );
        assert!(!errors.join(",").contains("secret"));
        assert!(!errors.join(",").contains("private"));
        assert!(!errors.join(",").contains("legacy"));
    }

    #[tokio::test]
    async fn native_diagnostics_failure_history_is_bounded_and_stably_ordered() {
        let mut conn = database().await;
        sqlx::query("INSERT INTO catabolic_scan_requests VALUES('request',10)")
            .execute(&mut conn)
            .await
            .unwrap();
        for n in 0..105 {
            sqlx::query("INSERT INTO catabolic_scan_demands VALUES(?,'request','failed','failed','unreported raw output')")
                .bind(format!("failure-{n:03}")).execute(&mut conn).await.unwrap();
        }
        let before: i64 = sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let first = workload_in(&mut conn).await.unwrap();
        assert_eq!(first.0, 0);
        assert_eq!(first.1, 0);
        assert_eq!(first.2.len(), 100);
        assert_eq!(first.2.first().unwrap(), "scan-demand:failure-000:failed");
        assert_eq!(first.2.last().unwrap(), "scan-demand:failure-099:failed");
        assert_eq!(first, workload_in(&mut conn).await.unwrap());
        let after: i64 = sqlx::query_scalar("SELECT total_changes()")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(after, before, "diagnostics cannot change operational state");
    }
}
