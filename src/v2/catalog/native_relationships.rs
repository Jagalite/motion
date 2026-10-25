//! Native work-relationship reads. Native rows establish edge identity and
//! revision, never endpoint visibility. Each endpoint is authorized separately
//! using the actual caller's current saved-query scope under Motion's writer.
//! Native graph/shape validation stays in its production owner; this adapter
//! supplies authorization observations, wire projection and HTTP pagination.
use super::{
    PageQuery, Problem, native_read_error, native_unavailable, page, plain_native_work, tagged,
    valid_id,
};
use crate::{App, db::begin_write, native_catalog::Runtime, v2::auth::Caller};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use catabolic_store::catalog_scope::TrustedSelection;
use playscale_core::access::{self, CatalogScope, Permission};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const MAX_INCIDENT_RELATIONSHIPS: usize = 1000;

pub(super) async fn get(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    read(&app, &caller, &id, None).await
}

pub(super) async fn list(
    State(app): State<App>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    read(&app, &caller, &id, Some(q)).await
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRelationship {
    id: String,
    revision: u64,
    source_item_id: String,
    target_item_id: String,
    kind: String,
    native_kind: String,
    position: Option<u32>,
    active: u8,
}

impl NativeRelationship {
    fn valid(&self) -> bool {
        valid_id(&self.id)
            && valid_id(&self.source_item_id)
            && valid_id(&self.target_item_id)
            && self.source_item_id != self.target_item_id
            && (1..=i64::MAX as u64).contains(&self.revision)
            && self.active <= 1
            && catabolic_store::catalog_work_relationship::public_kind(&self.native_kind)
                == Some(self.kind.as_str())
    }

    fn visible_body(&self, source_visible: bool, target_visible: bool) -> Option<Value> {
        (self.active == 1 && source_visible && target_visible).then(|| {
            // Edge revisions are exact native row revisions. Work aggregate
            // revisions and imported Motion revision bridges are not edge ETags.
            json!({
                "id": self.id,
                "revision": self.revision.to_string(),
                "source_item_id": self.source_item_id,
                "target_item_id": self.target_item_id,
                "kind": self.kind,
                "position": self.position,
            })
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeCapture {
    catalog_id: String,
    incarnation: String,
    generation: i64,
    relationships: Vec<NativeRelationship>,
}

fn decode_capture(
    value: Value,
    expected: &(String, String, i64),
    id: &str,
    incident: bool,
) -> Result<Vec<NativeRelationship>, Problem> {
    let capture: NativeCapture = serde_json::from_value(value).map_err(|_| native_unavailable())?;
    if capture.catalog_id != expected.0
        || capture.incarnation != expected.1
        || capture.generation != expected.2
        || capture.generation < 0
        || capture.relationships.len()
            > if incident {
                MAX_INCIDENT_RELATIONSHIPS
            } else {
                1
            }
    {
        return Err(native_unavailable());
    }
    let mut previous: Option<&str> = None;
    for row in &capture.relationships {
        if !row.valid()
            || previous.is_some_and(|previous| previous >= row.id.as_str())
            || if incident {
                row.source_item_id != id && row.target_item_id != id
            } else {
                row.id != id
            }
        {
            return Err(native_unavailable());
        }
        previous = Some(&row.id);
    }
    Ok(capture.relationships)
}

async fn endpoint_visible(
    runtime: &Runtime,
    tx: &mut sqlx::SqliteConnection,
    selection: &TrustedSelection,
    id: &str,
    observed: &mut BTreeMap<String, bool>,
) -> Result<bool, Problem> {
    if let Some(visible) = observed.get(id) {
        return Ok(*visible);
    }
    // Never infer the opposite endpoint's visibility from an incident edge,
    // cached HTTP principal, copied library membership, or source endpoint.
    let visible = match runtime
        .read_projectable_items(tx, vec![selection.clone()], Some(id.into()))
        .await
        .map_err(native_read_error)
    {
        Ok(rows) if rows.is_empty() => false,
        Ok(rows) if rows.len() == 1 && rows[0][0] == id => {
            plain_native_work(&rows[0])?;
            true
        }
        Ok(_) => return Err(native_unavailable()),
        Err(problem) if problem.status == StatusCode::NOT_FOUND => false,
        Err(problem) => return Err(problem),
    };
    observed.insert(id.into(), visible);
    Ok(visible)
}

async fn read(
    app: &App,
    caller: &Caller,
    id: &str,
    query: Option<PageQuery>,
) -> Result<Response, Problem> {
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    let pagination = query
        .as_ref()
        .map(|query| -> Result<_, Problem> { Ok((query.limit()?, query.after()?.to_owned())) })
        .transpose()?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;

    // Preparation may create saved queries, so it precedes the final writer
    // reservation. Reauthenticate again after rebuilding its disposable model.
    let mut initial_tx = begin_write(&app.db).await?;
    let initial = caller
        .reauthorize(&mut initial_tx, Some(Permission::CatalogRead))
        .await?;
    let prepared_scope = access::catalog_scope(&initial);
    if prepared_scope == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    initial_tx.commit().await?;
    let scoped = match &prepared_scope {
        CatalogScope::Libraries(ids) => Some(
            runtime
                .prepare_library_read_scope(ids.clone())
                .await
                .map_err(native_read_error)?,
        ),
        _ => None,
    };
    let selection = match &scoped {
        Some(scope) => scope.selection.clone(),
        None => runtime
            .prepare_all_scope()
            .await
            .map_err(native_read_error)?,
    };
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;

    // Every retained-owner operation below follows Motion -> native lock order.
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    if access::catalog_scope(&principal) == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    if access::catalog_scope(&principal) != prepared_scope {
        return Err(native_unavailable());
    }
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(native_read_error)?;
    if let Some(scope) = &scoped {
        runtime
            .validate_library_read_scope(&mut tx, scope)
            .await
            .map_err(native_read_error)?;
    }
    let mut observed = BTreeMap::new();
    if pagination.is_some()
        && !endpoint_visible(runtime, &mut tx, &selection, id, &mut observed).await?
    {
        return Err(Problem::not_found());
    }
    let expected: (String, String, i64) = sqlx::query_as(
        "SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1",
    )
    .fetch_one(&mut *tx)
    .await?;
    let generation = expected.2;
    let requested_id = id.to_owned();
    let incident = pagination.is_some();
    let (capture, returned) = runtime
        .with_host(move |host| {
            // Capture the WHOLE bounded incident set before cursor/visibility
            // filtering. Native overflow is an error, never a truncated page.
            let capture = if incident {
                host.projection_work_relationships(
                    generation,
                    &requested_id,
                    MAX_INCIDENT_RELATIONSHIPS,
                )
            } else {
                host.projection_work_relationship(generation, &requested_id)
            }
            .map_err(|error| anyhow::anyhow!("native relationship capture: {error:?}"))?;
            // Keep custody in the queued worker if the HTTP future is dropped.
            Ok((capture, tx))
        })
        .await
        .map_err(native_read_error)?;
    tx = returned;
    let relationships = decode_capture(capture, &expected, id, incident)?;
    let mut rows = Vec::new();
    for relationship in relationships {
        if relationship.active == 0
            || pagination
                .as_ref()
                .is_some_and(|(_, after)| relationship.id.as_str() <= after.as_str())
        {
            continue;
        }
        let source_visible = endpoint_visible(
            runtime,
            &mut tx,
            &selection,
            &relationship.source_item_id,
            &mut observed,
        )
        .await?;
        let target_visible = endpoint_visible(
            runtime,
            &mut tx,
            &selection,
            &relationship.target_item_id,
            &mut observed,
        )
        .await?;
        if let Some(body) = relationship.visible_body(source_visible, target_visible) {
            rows.push((relationship.id, body));
        }
        if pagination
            .as_ref()
            .is_some_and(|(limit, _)| rows.len() > *limit as usize)
        {
            break;
        }
    }
    // Expiry is time-dependent even while the writer excludes policy changes.
    let current = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    if access::catalog_scope(&current) != prepared_scope {
        return Err(native_unavailable());
    }
    let response = match pagination {
        Some((limit, _)) => Json(page(&mut tx, &current, rows, limit).await?).into_response(),
        None => {
            let (_, body) = rows.pop().ok_or_else(Problem::not_found)?;
            let revision = body["revision"]
                .as_str()
                .and_then(|revision| revision.parse().ok())
                .ok_or_else(native_unavailable)?;
            tagged(StatusCode::OK, &body, revision)
        }
    };
    let handle = tokio::runtime::Handle::current();
    runtime
        .with_host(move |host| {
            let identity = host.identity()?;
            anyhow::ensure!(
                identity["catalog_id"] == expected.0
                    && identity["catalog_incarnation"] == expected.1
                    && host.catalog_generation()? == expected.2,
                "native relationship snapshot changed before disclosure"
            );
            handle.block_on(tx.commit())?;
            Ok(())
        })
        .await
        .map_err(native_read_error)?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(id: &str, revision: u64) -> Value {
        json!({
            "id":id,"revision":revision,"source_item_id":"source","target_item_id":"target",
            "kind":"extra_of","native_kind":"custom:motion_extra_of","position":null,"active":1,
        })
    }

    fn capture(relationships: Vec<Value>) -> Value {
        json!({"catalog_id":"catalog","incarnation":"incarnation","generation":9,"relationships":relationships})
    }

    fn expected() -> (String, String, i64) {
        ("catalog".into(), "incarnation".into(), 9)
    }

    #[test]
    fn both_endpoints_and_active_state_are_required_for_disclosure() {
        let mut relationship: NativeRelationship = serde_json::from_value(edge("edge", 7)).unwrap();
        for (source, target) in [(false, false), (false, true), (true, false)] {
            assert!(relationship.visible_body(source, target).is_none());
        }
        let body = relationship.visible_body(true, true).unwrap();
        assert_eq!(body["revision"], "7");
        assert!(body.get("active").is_none());
        assert!(body.get("native_kind").is_none());
        relationship.active = 0;
        assert!(relationship.visible_body(true, true).is_none());
    }

    #[test]
    fn exact_native_revision_and_public_position_round_trip_without_float() {
        let mut value = edge("edge", i64::MAX as u64);
        value["position"] = json!(u32::MAX);
        let rows = decode_capture(capture(vec![value]), &expected(), "edge", false).unwrap();
        let body = rows[0].visible_body(true, true).unwrap();
        assert_eq!(body["revision"], i64::MAX.to_string());
        assert_eq!(body["position"], u32::MAX);
        for revision in [0, i64::MAX as u64 + 1] {
            assert!(
                decode_capture(
                    capture(vec![edge("edge", revision)]),
                    &expected(),
                    "edge",
                    false
                )
                .is_err()
            );
        }
    }

    #[test]
    fn capture_rejects_wrong_identity_generation_endpoint_and_duplicates() {
        let valid = capture(vec![edge("edge", 1)]);
        for (field, value) in [
            ("catalog_id", json!("other")),
            ("incarnation", json!("other")),
            ("generation", json!(10)),
        ] {
            let mut value_capture = valid.clone();
            value_capture[field] = value;
            assert!(decode_capture(value_capture, &expected(), "source", true).is_err());
        }
        assert!(decode_capture(valid.clone(), &expected(), "other", true).is_err());
        assert!(decode_capture(valid, &expected(), "other", false).is_err());
        assert!(
            decode_capture(
                capture(vec![edge("edge", 1), edge("edge", 1)]),
                &expected(),
                "source",
                true
            )
            .is_err()
        );
        assert!(
            decode_capture(
                capture(vec![edge("z", 1), edge("a", 1)]),
                &expected(),
                "source",
                true
            )
            .is_err()
        );
    }

    #[test]
    fn entire_incident_capture_is_bounded_even_for_inactive_rows() {
        let rows: Vec<Value> = (0..MAX_INCIDENT_RELATIONSHIPS)
            .map(|index| edge(&format!("edge_{index:04}"), 1))
            .collect();
        assert_eq!(
            decode_capture(capture(rows.clone()), &expected(), "source", true)
                .unwrap()
                .len(),
            MAX_INCIDENT_RELATIONSHIPS
        );
        let mut overflow = rows;
        let mut inactive = edge("last", 1);
        inactive["active"] = json!(0);
        overflow.push(inactive);
        assert!(decode_capture(capture(overflow), &expected(), "source", true).is_err());
    }
}
