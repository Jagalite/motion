//! Profile-owned progress is historical state, not a physical readiness grant.
//! Resolve each timeline through current typed ownership and scope before paging.
use super::*;
use crate::native_catalog::timeline_visibility::{Reader, Visibility};
use playscale_core::access::CatalogScope;
use std::collections::{BTreeMap, BTreeSet};

fn unavailable() -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_viewing_unavailable",
        "This viewing history requires additional native identity evidence",
    )
}

pub(super) async fn read(
    app: &App,
    caller: &Caller,
    profile: &str,
    q: &PageQuery,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let limit = q.limit()?;
    let mut after = keyset(q)?;
    let runtime = app.native_catalog.as_ref().ok_or_else(unavailable)?.clone();
    let mut initial = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut initial, Some(Permission::CatalogRead))
        .await?;
    usable_profile(&mut initial, &principal, profile).await?;
    let scope = access::catalog_scope(&principal);
    initial.commit().await?;
    let scoped = match &scope {
        CatalogScope::Libraries(ids) => Some(
            runtime
                .prepare_library_read_scope(ids.clone())
                .await
                .map_err(catalog::native_read_error)?,
        ),
        _ => None,
    };
    let selection = match (&scope, &scoped) {
        (CatalogScope::Nothing, _) => None,
        (_, Some(s)) => Some(s.selection.clone()),
        _ => Some(
            runtime
                .prepare_all_scope()
                .await
                .map_err(catalog::native_read_error)?,
        ),
    };
    let definitions = match (&scope, &scoped) {
        (CatalogScope::Nothing, _) | (_, Some(_)) => None,
        _ => Some(
            runtime
                .prepare_library_definitions(None)
                .await
                .map_err(catalog::native_read_error)?,
        ),
    };
    runtime
        .rebuild_projection()
        .await
        .map_err(catalog::native_read_error)?;
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    usable_profile(&mut tx, &principal, profile).await?;
    if access::catalog_scope(&principal) != scope {
        return Err(unavailable());
    }
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(catalog::native_read_error)?;
    let libraries = match &scoped {
        Some(s) => runtime
            .validate_library_read_scope(&mut tx, s)
            .await
            .map_err(catalog::native_read_error)?,
        None => match definitions {
            Some(s) => runtime
                .read_libraries(&mut tx, vec![s], None)
                .await
                .map_err(catalog::native_read_error)?,
            None => Vec::new(),
        },
    };
    let selections: Vec<_> = selection.into_iter().collect();
    let mut reader = Reader::new(runtime.clone(), &mut tx, selections.clone())
        .await
        .map_err(catalog::native_read_error)?;
    let mut entries = Vec::new();
    let mut works = BTreeMap::<String, Value>::new();
    let mut scanned = 0usize;
    if scope != CatalogScope::Nothing {
        loop {
            // Only application-owned progress is queried here. No compatibility
            // joins establish scope, retirement, current duration or readiness.
            let rows: Vec<(String,i64)> = sqlx::query_as("SELECT timeline_id,updated_at FROM timeline_viewing WHERE profile_id=? AND position_ms>0 AND coalesce(manual_watched,automatic_watched)=0 AND (? IS NULL OR updated_at<? OR (updated_at=? AND timeline_id>?)) ORDER BY updated_at DESC,timeline_id LIMIT 128")
                .bind(profile).bind(after.as_ref().map(|a|a.0)).bind(after.as_ref().map(|a|a.0)).bind(after.as_ref().map(|a|a.0)).bind(after.as_ref().map(|a|a.1.as_str())).fetch_all(&mut *tx).await?;
            if rows.is_empty() {
                break;
            }
            let short = rows.len() < 128;
            for (id, updated) in rows {
                scanned += 1;
                if scanned > 10000 {
                    return Err(unavailable());
                }
                after = Some((updated, id.clone()));
                let work = match reader
                    .read(&mut tx, &id)
                    .await
                    .map_err(catalog::native_read_error)?
                {
                    Visibility::Visible { work_id, .. } => work_id,
                    Visibility::Hidden | Visibility::Retired => continue,
                    // An unknown stored identity is not evidence of retirement.
                    Visibility::Missing => return Err(unavailable()),
                };
                let timeline = reader
                    .timeline_body(&mut tx, &id)
                    .await
                    .map_err(catalog::native_read_error)?
                    .ok_or_else(unavailable)?;
                if !works.contains_key(&work) {
                    let rows = runtime
                        .read_projectable_items(&mut tx, selections.clone(), Some(work.clone()))
                        .await
                        .map_err(catalog::native_read_error)?;
                    if rows.len() != 1 {
                        return Err(unavailable());
                    }
                    let body =
                        catalog::projected_native_work(&runtime, &mut tx, &rows[0], &libraries)
                            .await?;
                    works.insert(
                        work.clone(),
                        serde_json::to_value(body).map_err(Problem::internal)?,
                    );
                }
                let view = load_view(&mut tx, profile, &id).await?;
                entries.push((format!("{updated}.{id}"),json!({"item":works[&work],"timeline":timeline,"viewing":view_json(profile,&id,&view)})));
                if entries.len() > limit as usize {
                    break;
                }
            }
            if entries.len() > limit as usize || short {
                break;
            }
        }
    }
    let mut result = page(&mut tx, &principal, entries, limit).await?;
    let ids: BTreeSet<String> = result
        .items
        .iter()
        .map(|entry| {
            entry["item"]["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(unavailable)
        })
        .collect::<Result<_, _>>()?;
    let issued = runtime
        .issue_work_revisions_and_commit(tx, ids.into_iter().collect())
        .await
        .map_err(catalog::native_read_error)?;
    for entry in &mut result.items {
        let id = entry["item"]["id"].as_str().ok_or_else(unavailable)?;
        let revision = issued.get(id).ok_or_else(unavailable)?.revision;
        entry["item"]["revision"] = json!(revision.to_string());
    }
    Ok(Json(result).into_response())
}
