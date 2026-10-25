//! Native scan acceptance is a durable application demand, not permission for a
//! detached HTTP future to invoke a scanner. Current authorization, exact native
//! library membership proof and idempotency acknowledgment share one writer.
use super::*;
use crate::native_catalog::scan_sources_in as sources_in;

fn unavailable(e: impl std::fmt::Display) -> Problem {
    tracing::warn!(error=%e,"native scan proof rejected");
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_scan_unavailable",
        "Native scan scope is unavailable or changed",
    )
}
fn scope_ids(scope: &access::CatalogScope) -> Option<Vec<String>> {
    match scope {
        access::CatalogScope::All => None,
        access::CatalogScope::Libraries(ids) => Some(ids.iter().cloned().collect()),
        access::CatalogScope::Nothing => Some(Vec::new()),
    }
}
pub(super) async fn create(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    if !super::super::catalog::valid_id(&id) {
        return Err(Problem::not_found());
    }
    let key = idempotency_key(&headers)?;
    let initial = access::catalog_scope(&caller.principal);
    if !initial.library(&id) {
        return Err(Problem::not_found());
    }
    let _guard = app.jobs.lock().await;
    let grant = app
        .native_catalog
        .as_ref()
        .unwrap()
        .prepare_library_definitions(scope_ids(&initial))
        .await
        .map_err(unavailable)?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if access::catalog_scope(&principal) != initial {
        return Err(unavailable("principal scope changed during admission"));
    }
    let record = Scope {
        principal: &principal.id,
        operation: "createScan",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|r| &r.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let stored = stored.unwrap();
        tx.commit().await?;
        return Ok(tagged(stored.status, stored.body.unwrap()));
    }
    let (mut proof, sources) =
        sources_in(&app, &mut tx, &id, grant, body.value.source_ids.as_deref()).await?;
    proof["accepted_principal"] = serde_json::to_value(&principal).map_err(Problem::internal)?;
    let value = crate::native_scans::admit_in(
        &mut tx,
        &id,
        &principal.id,
        proof,
        sources,
        matches!(body.value.mode, Mode::Verify),
        body.value.require_complete,
    )
    .await
    .map_err(unavailable)?;
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
    Ok(tagged(StatusCode::ACCEPTED, value))
}
async fn visible(
    conn: &mut sqlx::SqliteConnection,
    principal: &access::Principal,
    id: &str,
) -> Result<(), Problem> {
    let library: Option<String> =
        sqlx::query_scalar("SELECT library_id FROM catabolic_scan_requests WHERE id=?")
            .bind(id)
            .fetch_optional(conn)
            .await?;
    if !library.is_some_and(|id| access::catalog_scope(principal).library(&id)) {
        return Err(Problem::not_found());
    }
    Ok(())
}
pub(super) async fn read(app: App, caller: Caller, id: String) -> Result<Response, Problem> {
    let mut tx = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    visible(&mut tx, &principal, &id).await?;
    let value = crate::native_scans::get_in(&mut tx, &id)
        .await
        .map_err(unavailable)?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, value))
}
pub(super) async fn cancel(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let digest = super::super::sha256(b"");
    // The scanner retains Motion's writer reservation through exact cleanup.
    // A current read authorization may only signal conservative interruption;
    // it cannot acknowledge cancellation or change durable demand state.
    let mut read = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut read, Some(Permission::SourcesManage))
        .await?;
    visible(&mut read, &principal, &id).await?;
    let cancellation = crate::native_scans::signal_cancel(&app, &id).map_err(unavailable)?;
    read.commit().await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        cancellation.wait_cleanup(),
    )
    .await
    .map_err(|_| {
        unavailable("native scan cleanup remains pending; cancellation is not acknowledged")
    })?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut tx = loop {
        match app.db.begin_with("BEGIN IMMEDIATE").await {
            Ok(tx) => break tx,
            Err(sqlx::Error::Database(error))
                if error
                    .code()
                    .and_then(|code| code.parse::<i32>().ok())
                    .is_some_and(|code| matches!(code & 0xff, 5 | 6))
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(error) => return Err(Problem::internal(error)),
        }
    };
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    visible(&mut tx, &principal, &id).await?;
    let record = Scope {
        principal: &principal.id,
        operation: "cancelScan",
        target: &id,
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(stored.as_ref().map(|r| &r.record), &digest, crate::now())?
        == Idempotent::Replay
    {
        let stored = stored.unwrap();
        tx.commit().await?;
        return Ok(tagged(stored.status, stored.body.unwrap()));
    }
    let value = crate::native_scans::cancel_in(&mut tx, &id)
        .await
        .map_err(unavailable)?;
    record
        .save(&mut tx, &digest, StatusCode::OK, Some(&value), None)
        .await?;
    tx.commit().await?;
    drop(cancellation);
    Ok(tagged(StatusCode::OK, value))
}

pub(super) async fn list(app: App, caller: Caller, query: ListQuery) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    if query
        .library_id
        .as_deref()
        .is_some_and(|id| !super::super::catalog::valid_id(id))
    {
        return Err(Problem::bad(
            "invalid_query",
            "library_id must be a valid ID",
        ));
    }
    let q = PageQuery {
        cursor: query.cursor,
        limit: query.limit,
    };
    let mut tx = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let limit = q.limit()?;
    let scope = access::catalog_scope(&principal);
    let all = matches!(scope, access::CatalogScope::All);
    let ids = match scope {
        access::CatalogScope::Libraries(ids) => {
            serde_json::to_string(&ids).map_err(Problem::internal)?
        }
        _ => "[]".into(),
    };
    // Authorization and the optional library filter apply before ordering
    // and the limit, so hidden scans never shape a page. Optional predicates
    // are emitted only when present, so SQLite can seek its indexes.
    let scoped = "(? OR library_id IN (SELECT value FROM json_each(?)))";
    let library = if query.library_id.is_some() {
        " AND library_id=?"
    } else {
        ""
    };
    let keyed: Vec<(String, String)> = match query.order {
        Order::Id => {
            let sql = format!(
                "SELECT id FROM catabolic_scan_requests WHERE id>? AND {scoped}{library} ORDER BY id LIMIT ?"
            );
            let mut rows = sqlx::query_scalar::<_, String>(&sql)
                .bind(q.after()?)
                .bind(all)
                .bind(&ids);
            if let Some(library) = &query.library_id {
                rows = rows.bind(library);
            }
            rows.bind(i64::from(limit) + 1)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|id| (id.clone(), id))
                .collect()
        }
        Order::Recent => {
            // Newest admission first. A scan admitted mid-read has a larger
            // sequence than every cursor, so it never shifts a later page.
            let after = match q.after()? {
                "" => None,
                cursor => Some(decode_recent(cursor)?),
            };
            let before = if after.is_some() {
                " AND admission<?"
            } else {
                ""
            };
            let sql = format!(
                "SELECT admission,id FROM catabolic_scan_requests WHERE {scoped}{library}{before} ORDER BY admission DESC LIMIT ?"
            );
            let mut rows = sqlx::query_as::<_, (i64, String)>(&sql)
                .bind(all)
                .bind(&ids);
            if let Some(library) = &query.library_id {
                rows = rows.bind(library);
            }
            if let Some(after) = after {
                rows = rows.bind(after);
            }
            rows.bind(i64::from(limit) + 1)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|(sequence, id)| (recent_cursor(sequence), id))
                .collect()
        }
    };
    let mut rows = Vec::new();
    for (cursor, id) in keyed {
        rows.push((
            cursor,
            crate::native_scans::get_in(&mut tx, &id)
                .await
                .map_err(unavailable)?,
        ));
    }
    let value = page(&mut tx, &principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(value).into_response())
}
