//! Processing-job observations belong to Motion, not the native catalog.
//! Read only application-owned job/admission history: `api_jobs` also unions frozen
//! legacy scan/metadata state and cannot stand in for native job adapters.
//! Recorded output IDs are historical references, never availability or bytes.
use super::*;

async fn rows_in(
    conn: &mut sqlx::SqliteConnection,
    principal: &access::Principal,
    after: &str,
    limit: u32,
) -> Result<Vec<Row>, Problem> {
    // Filter before the keyset limit. This is the same requester/admin rule
    // as core::access::owns_job; a legacy requester is never adopted on read.
    Ok(sqlx::query_as("SELECT id,revision,'rendition' AS kind,phase,attempt,error,output_file_id AS result_id,requester_id FROM processing_jobs WHERE id>? AND (? OR (requester_id!='legacy' AND requester_id=?)) ORDER BY id LIMIT ?")
        .bind(after).bind(principal.is_admin()).bind(&principal.id)
        .bind(i64::from(limit) + 1).fetch_all(conn).await?)
}

pub(super) async fn row_in(
    conn: &mut sqlx::SqliteConnection,
    principal: &access::Principal,
    id: &str,
) -> Result<Row, Problem> {
    let row: Row = sqlx::query_as("SELECT id,revision,'rendition' AS kind,phase,attempt,error,output_file_id AS result_id,requester_id FROM processing_jobs WHERE id=?")
        .bind(id).fetch_optional(conn).await?.ok_or_else(Problem::not_found)?;
    if !access::owns_job(principal, &row.requester_id) {
        return Err(Problem::not_found());
    }
    Ok(row)
}

pub(super) async fn list(
    app: &App,
    caller: &Caller,
    query: PageQuery,
) -> Result<Response, Problem> {
    let limit = query.limit()?;
    let after = query.after()?;
    let mut tx = app.db.begin().await?;
    // A fresh credential/policy and the disclosed rows share one snapshot.
    // No catalog projection, native generation, source pin or byte grant is
    // needed or implied by an owner's descriptive job history.
    let principal = caller.reauthorize(&mut tx, None).await?;
    let mut rows = Vec::new();
    for row in rows_in(&mut tx, &principal, after, limit).await? {
        let body = super::native_control::wire_in(&mut tx, &principal, &row).await?;
        rows.push((row.id.clone(), body));
    }
    let body = page(&mut tx, &principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(body).into_response())
}

pub(super) async fn get(app: &App, caller: &Caller, id: &str) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    let principal = caller.reauthorize(&mut tx, None).await?;
    let row = row_in(&mut tx, &principal, id).await?;
    let body = super::native_control::wire_in(&mut tx, &principal, &row).await?;
    tx.commit().await?;
    Ok(response(&row, body))
}

pub(super) fn response(row: &Row, body: Value) -> Response {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::ETAG, etag(row.revision as u64));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    async fn native_job_rows_need_no_legacy_union_or_catalog_authority() {
        let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        // Deliberately no api_jobs, native projection, catalog or grant table.
        sqlx::raw_sql("CREATE TABLE processing_jobs(id TEXT PRIMARY KEY,revision INTEGER,phase TEXT,attempt INTEGER,error TEXT,output_file_id TEXT,requester_id TEXT);
            INSERT INTO processing_jobs VALUES('a',3,'running',2,NULL,NULL,'other');
            INSERT INTO processing_jobs VALUES('b',4,'cancelling',2,NULL,NULL,'device');
            INSERT INTO processing_jobs VALUES('c',5,'completed',3,NULL,'historical-output','legacy');
            INSERT INTO processing_jobs VALUES('d',6,'failed',4,'private worker error',NULL,'device');")
            .execute(&mut conn).await.unwrap();
        let mut principal = access::Principal::operator();
        principal.id = "device".into();
        principal.grant.permissions = [Permission::CatalogRead].into();
        let rows = rows_in(&mut conn, &principal, "", 1).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "b");
        assert_eq!(rows[0].phase, "cancelling");
        assert_eq!(rows[1].id, "d");
        assert_eq!(rows[1].wire()["error_code"], "worker_failed");
        let last = rows_in(&mut conn, &principal, "b", 1).await.unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].id, "d");
        for id in ["a", "c", "missing"] {
            assert_eq!(
                row_in(&mut conn, &principal, id)
                    .await
                    .err()
                    .unwrap()
                    .status,
                StatusCode::NOT_FOUND
            );
        }
        let historical = row_in(&mut conn, &access::Principal::operator(), "c")
            .await
            .unwrap();
        assert_eq!(historical.wire()["kind"], "rendition");
        assert_eq!(
            historical.wire()["result_ids"],
            json!(["historical-output"])
        );
        assert_eq!(historical.wire()["phase"], "completed");
        assert_eq!(historical.wire()["attempt_generation"], "3");
        assert_eq!(historical.response().headers()[header::ETAG], "\"r-5\"");
        assert_eq!(
            rows_in(&mut conn, &access::Principal::operator(), "", 200)
                .await
                .unwrap()
                .len(),
            4
        );
    }
}
