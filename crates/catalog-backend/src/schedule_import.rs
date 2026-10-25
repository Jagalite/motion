//! Retained schedule descriptions and HTTP acknowledgments, never scan authority.
//! Actual adoption/execution must independently reauthorize the current actor.
use anyhow::ensure;
use playscale_core::scan_schedule::{RetainedSchedule, plan_adoption};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) async fn qualify(db: &mut SqliteConnection) -> anyhow::Result<(bool, u64)> {
    // Progress and implicit source schedules need their own complete reference
    // proof. Do not silently erase or exempt either historical corridor.
    let progress: i64 = sqlx::query_scalar("SELECT count(*) FROM schedule_dispatch_progress")
        .fetch_one(&mut *db)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM api_schedules")
        .fetch_one(&mut *db)
        .await?;
    ensure!(count <= 10000, "schedule audit row budget exceeded");
    let bytes: i64 = sqlx::query_scalar("SELECT COALESCE(sum(length(CAST(id AS BLOB))+length(CAST(library_id AS BLOB))+length(CAST(task AS BLOB))+length(CAST(requester_id AS BLOB))+COALESCE(length(CAST(credential_hash AS BLOB)),0)+COALESCE(length(CAST(ingress_id AS BLOB)),0)),0) FROM api_schedules").fetch_one(&mut *db).await?;
    ensure!(
        bytes <= 2 * 1024 * 1024,
        "schedule audit byte budget exceeded"
    );
    let rows = sqlx::query("SELECT s.*,EXISTS(SELECT 1 FROM catalog_libraries l WHERE l.id=s.library_id) AS library_exists FROM api_schedules s ORDER BY id").fetch_all(&mut *db).await?;
    let mut schedules = BTreeMap::new();
    let mut qualified = progress == 0;
    for row in rows {
        let enabled: i64 = row.try_get("enabled")?;
        let schedule = RetainedSchedule {
            id: row.try_get("id")?,
            revision: row.try_get("revision")?,
            library_id: row.try_get("library_id")?,
            task: row.try_get("task")?,
            interval_seconds: row.try_get("interval_seconds")?,
            enabled: enabled == 1,
            next_run: row.try_get("next_run")?,
            requester_id: row.try_get("requester_id")?,
            credential_hash: row.try_get("credential_hash")?,
            ingress_id: row.try_get("ingress_id")?,
        };
        qualified &= matches!(enabled, 0 | 1)
            && row.try_get::<i64, _>("library_exists")? == 1
            && matches!(schedule.task.as_str(), "scan" | "metadata_refresh")
            && plan_adoption(schedule.clone(), None).is_ok();
        schedules.insert(schedule.id.clone(), schedule);
    }
    let receipt_bytes:i64=sqlx::query_scalar("SELECT COALESCE(sum(length(CAST(body AS BLOB))+length(CAST(principal_id AS BLOB))+length(CAST(target AS BLOB))+length(CAST(digest AS BLOB))),0) FROM idempotency_records WHERE operation='createSchedule'").fetch_one(&mut *db).await?;
    ensure!(
        receipt_bytes <= 2 * 1024 * 1024,
        "schedule receipt byte budget exceeded"
    );
    let receipts = sqlx::query("SELECT principal_id,target,status,body,digest,issued_nonce IS NULL AS no_nonce FROM idempotency_records WHERE operation='createSchedule' AND length(CAST(body AS BLOB))<=65536 LIMIT 10001").fetch_all(&mut *db).await?;
    ensure!(receipts.len() <= 10000, "schedule receipt budget exceeded");
    let mut retained = 0;
    for row in receipts {
        if row.try_get::<i64, _>("status")? != 201
            || row.try_get::<i64, _>("no_nonce")? != 1
            || !row.try_get::<String, _>("target")?.is_empty()
        {
            continue;
        }
        let body: String = row.try_get("body")?;
        let Ok(value) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        let request = json!({"library_id":value["library_id"],"task":value["task"],"interval_seconds":value["interval_seconds"],"enabled":value["enabled"]});
        if row.try_get::<String, _>("digest")? != catabolic_core::sha256(request.to_string()) {
            continue;
        }
        if receipt_matches(
            &value,
            row.try_get::<String, _>("principal_id")?.as_str(),
            &schedules,
        ) {
            retained += 1;
        }
    }
    Ok((qualified, retained))
}
fn receipt_matches(
    value: &Value,
    principal: &str,
    schedules: &BTreeMap<String, RetainedSchedule>,
) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let expected: BTreeSet<_> = [
        "id",
        "revision",
        "library_id",
        "task",
        "interval_seconds",
        "enabled",
    ]
    .into_iter()
    .collect();
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return false;
    }
    let Some(schedule) = value["id"].as_str().and_then(|id| schedules.get(id)) else {
        return false;
    };
    // This finite cohort authenticates the exact retained current acknowledgment.
    // Older changed/deleted schedule receipts remain an explicit obligation.
    principal == schedule.requester_id
        && value
            == &json!({
                "id":schedule.id,"revision":schedule.revision.to_string(),
                "library_id":schedule.library_id,"task":schedule.task,
                "interval_seconds":schedule.interval_seconds,"enabled":schedule.enabled
            })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_receipt_requires_exact_owner_and_closed_current_body() {
        let schedule = RetainedSchedule {
            id: "s".into(),
            revision: 1,
            library_id: "l".into(),
            task: "scan".into(),
            interval_seconds: 60,
            enabled: true,
            next_run: 100,
            requester_id: "operator".into(),
            credential_hash: None,
            ingress_id: None,
        };
        let map = BTreeMap::from([("s".into(), schedule)]);
        let body = json!({"id":"s","revision":"1","library_id":"l","task":"scan","interval_seconds":60,"enabled":true});
        assert!(receipt_matches(&body, "operator", &map));
        assert!(!receipt_matches(&body, "foreign", &map));
        for (key, value) in [
            ("id", json!("unknown")),
            ("library_id", json!("foreign")),
            ("revision", json!(2)),
            ("task", json!("unknown")),
            ("extra", json!(true)),
        ] {
            let mut changed = body.clone();
            changed[key] = value;
            assert!(!receipt_matches(&changed, "operator", &map));
        }
    }
    #[tokio::test]
    async fn retained_schedule_audit_refuses_unknown_library_actor_progress_and_receipt() {
        use sqlx::Connection;
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql("CREATE TABLE catalog_libraries(id TEXT PRIMARY KEY); CREATE TABLE schedule_dispatch_progress(schedule_id TEXT, after_item TEXT); CREATE TABLE api_schedules(id TEXT PRIMARY KEY,revision INTEGER,library_id TEXT,task TEXT,interval_seconds INTEGER,enabled INTEGER,next_run INTEGER,requester_id TEXT,credential_hash TEXT,ingress_id TEXT); CREATE TABLE idempotency_records(principal_id TEXT,operation TEXT,target TEXT,status INTEGER,body TEXT,digest TEXT,issued_nonce TEXT); INSERT INTO catalog_libraries VALUES('l'); INSERT INTO api_schedules VALUES('s',1,'l','scan',60,1,100,'operator',NULL,NULL);")
            .execute(&mut db).await.unwrap();
        let body=json!({"id":"s","revision":"1","library_id":"l","task":"scan","interval_seconds":60,"enabled":true}).to_string();
        sqlx::query(
            "INSERT INTO idempotency_records VALUES('operator','createSchedule','',201,?,?,NULL)",
        )
        .bind(&body)
        .bind(catabolic_core::sha256(
            json!({"library_id":"l","task":"scan","interval_seconds":60,"enabled":true})
                .to_string(),
        ))
        .execute(&mut db)
        .await
        .unwrap();
        assert_eq!(qualify(&mut db).await.unwrap(), (true, 1));
        for (change, restore) in [
            (
                "UPDATE api_schedules SET library_id='foreign'",
                "UPDATE api_schedules SET library_id='l'",
            ),
            (
                "UPDATE api_schedules SET requester_id=''",
                "UPDATE api_schedules SET requester_id='operator'",
            ),
            (
                "UPDATE api_schedules SET task='unknown'",
                "UPDATE api_schedules SET task='scan'",
            ),
            (
                "INSERT INTO schedule_dispatch_progress VALUES('s','i')",
                "DELETE FROM schedule_dispatch_progress",
            ),
        ] {
            sqlx::query(change).execute(&mut db).await.unwrap();
            assert!(!qualify(&mut db).await.unwrap().0);
            sqlx::query(restore).execute(&mut db).await.unwrap();
        }
        for (change, restore) in [
            (
                "UPDATE idempotency_records SET principal_id='foreign'",
                "UPDATE idempotency_records SET principal_id='operator'",
            ),
            (
                "UPDATE idempotency_records SET target='foreign'",
                "UPDATE idempotency_records SET target=''",
            ),
            (
                "UPDATE idempotency_records SET issued_nonce='secret'",
                "UPDATE idempotency_records SET issued_nonce=NULL",
            ),
            (
                "UPDATE idempotency_records SET operation='unknown'",
                "UPDATE idempotency_records SET operation='createSchedule'",
            ),
        ] {
            sqlx::query(change).execute(&mut db).await.unwrap();
            assert_eq!(qualify(&mut db).await.unwrap().1, 0);
            sqlx::query(restore).execute(&mut db).await.unwrap();
        }
        assert_eq!(qualify(&mut db).await.unwrap(), (true, 1));
        let retained: String = sqlx::query_scalar("SELECT body FROM idempotency_records")
            .fetch_one(&mut db)
            .await
            .unwrap();
        assert_eq!(retained, body);
    }
}
