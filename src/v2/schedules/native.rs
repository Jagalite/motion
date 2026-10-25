//! Candidate native scan schedules. Outer public route gate remains closed.
use super::*;
use crate::native_schedules as owner;
use playscale_core::scan_schedule::RetainedSchedule;

fn unavailable(error: impl std::fmt::Display) -> Problem {
    tracing::warn!(%error,"native schedule owner refused request");
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_schedule_unavailable",
        "Native schedule scope or retained ownership is unavailable",
    )
}
fn owner_error(error: anyhow::Error) -> Problem {
    if let Some(error) = error.downcast_ref::<owner::ScheduleAccessError>() {
        return error.0.clone().into();
    }
    unavailable(error)
}
fn wire(row: &RetainedSchedule) -> Value {
    json!({"id":row.id,"revision":row.revision.to_string(),"library_id":row.library_id,"task":row.task,"interval_seconds":row.interval_seconds,"enabled":row.enabled})
}
async fn guard(app: &App) -> Result<(), Problem> {
    if app.native_catalog.is_none() {
        return Err(unavailable("native runtime missing"));
    }
    Ok(())
}
async fn visible(
    app: &App,
    db: &mut sqlx::SqliteConnection,
    p: &access::Principal,
    id: &str,
) -> Result<owner::RetainedView, Problem> {
    let view = owner::lookup_in(app, db, id)
        .await
        .map_err(owner_error)?
        .ok_or_else(Problem::not_found)?;
    if view.tombstoned || !access::catalog_scope(p).library(&view.row.library_id) {
        return Err(Problem::not_found());
    }
    Ok(view)
}
async fn selection(
    app: &App,
    p: &access::Principal,
) -> Result<catabolic_store::catalog_scope::TrustedSelection, Problem> {
    let ids = match access::catalog_scope(p) {
        access::CatalogScope::All => None,
        access::CatalogScope::Libraries(ids) => Some(ids.into_iter().collect()),
        access::CatalogScope::Nothing => Some(vec![]),
    };
    app.native_catalog
        .as_ref()
        .ok_or_else(|| unavailable("native runtime missing"))?
        .prepare_library_definitions(ids)
        .await
        .map_err(unavailable)
}
fn desired(
    caller: &Caller,
    p: &access::Principal,
    id: String,
    i: Input,
) -> Result<RetainedSchedule, Problem> {
    validate(&i, p)?;
    if i.task != "scan" {
        return Err(Problem::invalid(
            "schedule_task_unavailable",
            "Native scheduled task is not qualified",
        ));
    }
    let next = crate::now()
        .checked_add(i.interval_seconds)
        .ok_or_else(|| Problem::invalid("invalid_interval", "Schedule time overflow"))?;
    Ok(RetainedSchedule {
        id,
        revision: 1,
        library_id: i.library_id,
        task: i.task,
        interval_seconds: i.interval_seconds,
        enabled: i.enabled,
        next_run: next,
        requester_id: p.id.clone(),
        credential_hash: caller.credential().map(|(h, _)| h.to_owned()),
        ingress_id: caller.ingress_device_id().map(str::to_owned),
    })
}
pub(super) async fn list(app: App, caller: Caller, q: PageQuery) -> Result<Response, Problem> {
    guard(&app).await?;
    caller.require(Permission::SourcesManage)?;
    let limit = q.limit()?;
    let after = q.after()?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let scope = access::catalog_scope(&p);
    let rows = owner::retained_inventory_in(&app, &mut tx)
        .await
        .map_err(owner_error)?;
    let values = rows
        .into_iter()
        .filter(|v| !v.tombstoned && v.row.id.as_str() > after && scope.library(&v.row.library_id))
        .take(limit as usize + 1)
        .map(|v| (v.row.id.clone(), wire(&v.row)))
        .collect();
    let result = page(&mut tx, &p, values, limit).await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}
pub(super) async fn read(app: App, caller: Caller, id: String) -> Result<Response, Problem> {
    guard(&app).await?;
    caller.require(Permission::SourcesManage)?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let view = visible(&app, &mut tx, &p, &id).await?;
    let value = wire(&view.row);
    tx.commit().await?;
    Ok(tagged(value, StatusCode::OK))
}
pub(super) async fn create(
    app: App,
    caller: Caller,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    guard(&app).await?;
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let grant = selection(&app, &caller.principal).await?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if access::catalog_scope(&p) != access::catalog_scope(&caller.principal) {
        return Err(unavailable(
            "schedule principal changed during scope preparation",
        ));
    }
    let row = desired(&caller, &p, crate::new_id(), body.value)?;
    if !access::catalog_scope(&p).library(&row.library_id) {
        return Err(Problem::not_found());
    }
    crate::native_catalog::scan_sources_in(&app, &mut tx, &row.library_id, grant, None).await?;
    let record = Scope {
        principal: &p.id,
        operation: "createSchedule",
        target: "",
        key: &key,
    };
    let old = record.load(&mut tx).await?;
    if access::idempotency(old.as_ref().map(|s| &s.record), &body.digest, crate::now())?
        == Idempotent::Replay
    {
        let old = old.unwrap();
        let mut response = tagged(old.body.unwrap_or_default(), old.status);
        response
            .headers_mut()
            .insert("idempotent-replayed", "true".parse().unwrap());
        return Ok(response);
    }
    let row = owner::create_in(&app, &mut tx, row)
        .await
        .map_err(owner_error)?;
    let value = wire(&row);
    record
        .save(
            &mut tx,
            &body.digest,
            StatusCode::CREATED,
            Some(&value),
            None,
        )
        .await?;
    tx.commit().await?;
    Ok(tagged(value, StatusCode::CREATED))
}
pub(super) async fn replace(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
    body: Body<Input>,
) -> Result<Response, Problem> {
    guard(&app).await?;
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let grant = selection(&app, &caller.principal).await?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if access::catalog_scope(&p) != access::catalog_scope(&caller.principal) {
        return Err(unavailable(
            "schedule principal changed during scope preparation",
        ));
    }
    let old = visible(&app, &mut tx, &p, &id).await?;
    if !old.native {
        return Err(unavailable("retained schedule requires explicit adoption"));
    }
    let row = desired(&caller, &p, id, body.value)?;
    if !access::catalog_scope(&p).library(&row.library_id) {
        return Err(Problem::not_found());
    }
    crate::native_catalog::scan_sources_in(&app, &mut tx, &row.library_id, grant, None).await?;
    let row = owner::replace_in(&app, &mut tx, row, expected)
        .await
        .map_err(owner_error)?;
    let value = wire(&row);
    tx.commit().await?;
    Ok(tagged(value, StatusCode::OK))
}
pub(super) async fn delete(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    guard(&app).await?;
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let mut tx = begin_write(&app.db).await?;
    let p = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let old = visible(&app, &mut tx, &p, &id).await?;
    if !old.native {
        return Err(unavailable("retained schedule requires explicit adoption"));
    }
    owner::tombstone_in(&app, &mut tx, &id, expected)
        .await
        .map_err(owner_error)?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
