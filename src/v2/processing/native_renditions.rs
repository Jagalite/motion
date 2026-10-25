//! Native rendition presentation is not a physical-file grant. Current scope
//! comes from the admitted original; historical rows remain descriptive for All.
use super::*;

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    timeline_id: String,
    source_file_id: String,
    source_revision: String,
    recipe_digest: String,
    phase: String,
    revision: i64,
    expired: bool,
    recipe: String,
    tracks_json: String,
}

pub(super) async fn list(
    app: &App,
    caller: &Caller,
    query: RenditionQuery,
) -> Result<Json<super::super::Page<Value>>, Problem> {
    let q = PageQuery {
        cursor: query.cursor,
        limit: query.limit,
    };
    let limit = q.limit()?;
    let mut after = q.after()?.to_owned();
    let mut tx = super::super::playback::begin_catalog_poll(app).await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::ProcessingRequest))
        .await?;
    super::super::playback::require_native_scope(&mut tx, &principal).await?;
    let scope = access::catalog_scope(&principal);
    let all = matches!(&scope, access::CatalogScope::All);
    let library_ids = match &scope {
        access::CatalogScope::Libraries(ids) => {
            serde_json::to_string(ids).map_err(Problem::internal)?
        }
        _ => "[]".to_owned(),
    };
    let mut values = Vec::new();
    // Scan stable keyset batches until the authorized page is complete. A SQL
    // LIMIT followed by filtering would silently omit later authorized rows.
    'scan: loop {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT j.id,p.timeline_id,j.source_file_id,j.source_revision,p.recipe_digest,j.phase,p.revision,j.expired,j.recipe,p.tracks_json FROM preparations p JOIN processing_jobs j ON j.id=p.job_id WHERE j.id>? AND (? IS NULL OR p.timeline_id=?) AND (? OR EXISTS(SELECT 1 FROM catabolic_projection_file_libraries m JOIN catabolic_projection_state s ON s.singleton=1 AND s.generation=m.generation WHERE m.file_id=j.source_file_id AND m.library_id IN (SELECT value FROM json_each(?))) OR EXISTS(SELECT 1 FROM catabolic_projection_generated_libraries gm JOIN catabolic_projection_state gs ON gs.singleton=1 AND gs.generation=gm.generation WHERE gm.file_id=j.output_file_id AND gm.library_id IN (SELECT value FROM json_each(?)))) ORDER BY j.id LIMIT 128",
        ).bind(&after).bind(&query.timeline_id).bind(&query.timeline_id).bind(all).bind(&library_ids).bind(&library_ids).fetch_all(&mut *tx).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            after = row.id.clone();
            let source = super::super::content::file_facts(&mut tx, &row.source_file_id).await?;
            let source_current = source
                .as_ref()
                .is_some_and(|f| f.revision == row.source_revision);
            let binding = exact_binding(
                &mut tx,
                &row.timeline_id,
                &row.source_file_id,
                &row.source_revision,
            )
            .await?;
            let output = crate::native_processing::current_rendition_in(
                app.native_catalog
                    .as_ref()
                    .expect("native list requires runtime"),
                &mut tx,
                &row.id,
            )
            .await
            .map_err(Problem::internal)?;
            let source_visible = source
                .as_ref()
                .is_some_and(|f| scope.admits(f.library_ids.iter()));
            if !all && !source_visible {
                let output_visible = if let Some(output) = output.as_ref().filter(|o| o.ready) {
                    super::super::content::transport_file_facts(
                        &mut tx,
                        &principal,
                        &output.output_file,
                    )
                    .await?
                    .is_some_and(|f| scope.admits(f.library_ids.iter()))
                } else {
                    false
                };
                if !output_visible {
                    continue;
                }
            }
            if output
                .as_ref()
                .is_some_and(|o| o.timeline_id != row.timeline_id)
            {
                return Err(Problem::internal(anyhow::anyhow!(
                    "Native rendition timeline identity changed"
                )));
            }
            let status = playscale_core::processing::rendition_status(
                &playscale_core::processing::RenditionFacts {
                    expired: row.expired,
                    phase: serde_json::from_value(json!(row.phase)).map_err(Problem::internal)?,
                    source_current,
                    binding_current: binding,
                    output_current: output.as_ref().is_some_and(|o| o.ready),
                },
            );
            let (output_ref, version) = match output {
                Some(o) => (
                    json!({"file_id":o.output_file,"file_revision":o.output_revision}),
                    Some(o.version_id),
                ),
                None => (Value::Null, None),
            };
            let tracks: Value =
                serde_json::from_str(&row.tracks_json).map_err(Problem::internal)?;
            values.push((row.id.clone(), json!({"id":row.id,"revision":row.revision.to_string(),"timeline_id":row.timeline_id,"source":{"file_id":row.source_file_id,"file_revision":row.source_revision},"output":output_ref,"recipe_digest":row.recipe_digest,"job_id":row.id,"status":status,"recipe_id":row.recipe,"tracks":tracks,"version_id":version})));
            if values.len() > limit as usize {
                break 'scan;
            }
        }
    }
    let result = page(&mut tx, &principal, values, limit).await?;
    tx.commit().await?;
    Ok(Json(result))
}
