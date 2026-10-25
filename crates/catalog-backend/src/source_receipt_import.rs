//! Exact unchanged source/library creation acknowledgments. These retained
//! descriptions never confer current source, filesystem, or caller authority.
use anyhow::ensure;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeSet;

pub(crate) async fn qualify(db: &mut SqliteConnection) -> anyhow::Result<u64> {
    let (count, bytes): (i64, i64) = sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(body AS BLOB))+length(CAST(principal_id AS BLOB))+length(CAST(target AS BLOB))+length(CAST(digest AS BLOB))),0) FROM idempotency_records WHERE operation IN ('createSource','createLibrary')").fetch_one(&mut *db).await?;
    ensure!(
        count <= 10000 && bytes <= 2097152,
        "source creation receipt audit budget exceeded"
    );
    let rows = sqlx::query("SELECT operation,principal_id,target,status,body,digest,issued_nonce IS NULL AS no_nonce FROM idempotency_records WHERE operation IN ('createSource','createLibrary') ORDER BY operation,key").fetch_all(&mut *db).await?;
    let mut retained = 0;
    let mut remaining = 10000;
    for row in rows {
        if row.try_get::<i64, _>("status")? != 201
            || row.try_get::<i64, _>("no_nonce")? != 1
            || row.try_get::<String, _>("target")? != "-"
            || row.try_get::<String, _>("principal_id")?.is_empty()
        {
            continue;
        }
        let raw: String = row.try_get("body")?;
        let Ok(body) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(id) = body["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256)
        else {
            continue;
        };
        let operation: String = row.try_get("operation")?;
        let expected = if operation == "createSource" {
            source(db, id, &mut remaining).await?
        } else {
            library(db, id, &mut remaining).await?
        };
        let Some(expected) = expected else {
            continue;
        };
        let request = if operation == "createSource" {
            json!({"name":body["name"],"root_path":body["root_path"],"exclusions":body["exclusions"]})
        } else {
            json!({"name":body["name"],"kind":body["kind"],"language":body["language"],"source_ids":body["source_ids"]})
        };
        if body == expected
            && row.try_get::<String, _>("digest")? == catabolic_core::sha256(request.to_string())
        {
            retained += 1;
        }
    }
    Ok(retained)
}
async fn source(
    db: &mut SqliteConnection,
    id: &str,
    remaining: &mut usize,
) -> anyhow::Result<Option<Value>> {
    ensure!(*remaining > 0, "source receipt lookup budget exceeded");
    *remaining -= 1;
    // Finite unchanged creation cohort. Historical edits, old availability and
    // normalized request spellings need their own retained request evidence.
    // Migration 0008 defaults physical libraries.revision to 0;
    // libraries::register_source_in does not override it. Logical libraries and
    // source bindings independently begin at revision 1.
    let raw: Option<String> = sqlx::query_scalar("SELECT json_object('id',id,'revision',CAST(revision AS TEXT),'binding_revision',CAST(binding_revision AS TEXT),'name',name,'root_path',root,'volume_identity',root_identity,'availability','unknown','exclusions',json(exclusions_json)) FROM libraries WHERE id=? AND revision=0 AND binding_revision=1 AND enabled=1 AND managed=0 AND length(CAST(name AS BLOB))+length(CAST(root AS BLOB))+length(CAST(root_identity AS BLOB))+length(CAST(exclusions_json AS BLOB))<=65536 AND NOT EXISTS(SELECT 1 FROM jobs WHERE library_id=libraries.id)").bind(id).fetch_optional(&mut *db).await?;
    raw.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
async fn library(
    db: &mut SqliteConnection,
    id: &str,
    remaining: &mut usize,
) -> anyhow::Result<Option<Value>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT json_object('id',id,'revision',CAST(revision AS TEXT),'name',name,'kind',kind,'language',language,'availability','unknown') FROM catalog_libraries WHERE id=? AND revision=1 AND length(CAST(name AS BLOB))+length(CAST(kind AS BLOB))+length(CAST(language AS BLOB))<=65536").bind(id).fetch_optional(&mut *db).await?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let (count,bytes):(i64,i64)=sqlx::query_as("SELECT count(*),coalesce(sum(length(CAST(source_id AS BLOB))),0) FROM library_sources WHERE library_id=?").bind(id).fetch_one(&mut *db).await?;
    ensure!(
        count <= 10000 && bytes <= 65536,
        "source membership receipt audit budget exceeded"
    );
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT source_id FROM library_sources WHERE library_id=? ORDER BY source_id",
    )
    .bind(id)
    .fetch_all(&mut *db)
    .await?;
    if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Ok(None);
    }
    for source_id in &ids {
        if source(db, source_id, remaining).await?.is_none() {
            return Ok(None);
        }
    }
    let mut body: Value = serde_json::from_str(&raw)?;
    body["source_ids"] = json!(ids);
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;
    #[tokio::test]
    async fn creation_receipts_require_exact_current_source_library_and_request() {
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql("CREATE TABLE libraries(id TEXT,revision INTEGER,binding_revision INTEGER,name TEXT,root TEXT,root_identity TEXT,exclusions_json TEXT,enabled INTEGER,managed INTEGER); CREATE TABLE catalog_libraries(id TEXT,revision INTEGER,name TEXT,kind TEXT,language TEXT); CREATE TABLE library_sources(library_id TEXT,source_id TEXT); CREATE TABLE jobs(library_id TEXT); CREATE TABLE idempotency_records(operation TEXT,principal_id TEXT,target TEXT,key TEXT,status INTEGER,body TEXT,digest TEXT,issued_nonce TEXT); INSERT INTO libraries VALUES('s',0,1,'Source','/root','volume','[]',1,0); INSERT INTO catalog_libraries VALUES('l',1,'Library','movies','en'); INSERT INTO library_sources VALUES('l','s');").execute(&mut db).await.unwrap();
        let source_body = json!({"id":"s","revision":"0","binding_revision":"1","name":"Source","root_path":"/root","volume_identity":"volume","exclusions":[],"availability":"unknown"});
        let library_body = json!({"id":"l","revision":"1","name":"Library","kind":"movies","language":"en","source_ids":["s"],"availability":"unknown"});
        for (op, body, request) in [
            (
                "createSource",
                source_body,
                json!({"name":"Source","root_path":"/root","exclusions":[]}),
            ),
            (
                "createLibrary",
                library_body,
                json!({"name":"Library","kind":"movies","language":"en","source_ids":["s"]}),
            ),
        ] {
            sqlx::query(
                "INSERT INTO idempotency_records VALUES(?,'operator','-','key',201,?,?,NULL)",
            )
            .bind(op)
            .bind(body.to_string())
            .bind(catabolic_core::sha256(request.to_string()))
            .execute(&mut db)
            .await
            .unwrap();
        }
        assert_eq!(qualify(&mut db).await.unwrap(), 2);
        for (mutate, restore, expected) in [
            (
                "UPDATE libraries SET root_identity='foreign'",
                "UPDATE libraries SET root_identity='volume'",
                1,
            ),
            (
                "UPDATE libraries SET revision=1",
                "UPDATE libraries SET revision=0",
                0,
            ),
            (
                "UPDATE libraries SET enabled=0",
                "UPDATE libraries SET enabled=1",
                0,
            ),
            (
                "UPDATE library_sources SET source_id='foreign'",
                "UPDATE library_sources SET source_id='s'",
                1,
            ),
            (
                "UPDATE idempotency_records SET target='foreign'",
                "UPDATE idempotency_records SET target='-'",
                0,
            ),
            (
                "UPDATE idempotency_records SET issued_nonce='nonce'",
                "UPDATE idempotency_records SET issued_nonce=NULL",
                0,
            ),
            (
                "UPDATE idempotency_records SET body=json_set(body,'$.extra',1)",
                "UPDATE idempotency_records SET body=json_remove(body,'$.extra')",
                0,
            ),
            ("INSERT INTO jobs VALUES('s')", "DELETE FROM jobs", 0),
        ] {
            sqlx::query(mutate).execute(&mut db).await.unwrap();
            assert_eq!(qualify(&mut db).await.unwrap(), expected, "{mutate}");
            sqlx::query(restore).execute(&mut db).await.unwrap();
            assert_eq!(qualify(&mut db).await.unwrap(), 2);
        }
        sqlx::query("UPDATE idempotency_records SET digest='wrong'")
            .execute(&mut db)
            .await
            .unwrap();
        assert_eq!(qualify(&mut db).await.unwrap(), 0);
        // Unknown operations remain outside the exact coverage count.
        sqlx::query("UPDATE idempotency_records SET operation='unknown'")
            .execute(&mut db)
            .await
            .unwrap();
        assert_eq!(qualify(&mut db).await.unwrap(), 0);
    }
}
