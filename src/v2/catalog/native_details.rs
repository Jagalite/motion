//! Bounded native child-detail reads. Compatibility rows supply stable Motion
//! references only after a complete current native owner capture. No legacy
//! visibility joins, alternate-file substitution, or historical readiness.
use super::*;
use sqlx::{Sqlite, Transaction};

#[derive(Default)]
struct Details {
    editions: BTreeMap<String, EditionBody>,
    timelines: BTreeMap<String, Value>,
    versions: BTreeMap<String, Value>,
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Problem> {
    value[field].as_str().ok_or_else(native_unavailable)
}
fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, Problem> {
    value[field].as_array().ok_or_else(native_unavailable)
}
fn head(value: &Value, field: &str) -> Result<i64, Problem> {
    value[field]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(native_unavailable)
}
fn bridged(base: &Value, current: i64) -> Result<i64, Problem> {
    playscale_core::catalog_projection::motion_revision(head(base, "revision")?, current)
        .ok_or_else(native_unavailable)
}

/// An empty structural owner is not a plain-work fallback. Prove the complete
/// current graph independently, then expose only descriptive no-file facts.
pub(super) async fn empty_work(
    runtime: &crate::native_catalog::Runtime,
    conn: &mut sqlx::SqliteConnection,
    mut body: CatalogItemBody,
    shape: &Value,
    generation: i64,
    catalog: &str,
    incarnation: &str,
) -> Result<CatalogItemBody, Problem> {
    let work = body.id.clone();
    let captured = runtime
        .with_host(move |host| {
            let capture = host.capture_work_dependencies(generation, &work)?;
            Ok((
                capture.catalog_id().to_owned(),
                capture.incarnation().to_owned(),
                capture.dependencies().clone(),
            ))
        })
        .await
        .map_err(native_read_error)?;
    if captured.0 != catalog || captured.1 != incarnation {
        return Err(native_unavailable());
    }
    let facts = captured.2;
    for name in [
        "versions",
        "version_heads",
        "version_retirements",
        "files",
        "associations",
        "bindings",
        "catalog_logical_retirements",
    ] {
        if !array(&facts, name)?.is_empty() {
            return Err(native_unavailable());
        }
    }
    let expected_editions: BTreeSet<String> =
        serde_json::from_value(shape["edition_ids"].clone()).map_err(|_| native_unavailable())?;
    let actual_editions: BTreeSet<String> = array(&facts, "owners")?
        .iter()
        .filter(|v| v["kind"] == "custom:motion_edition")
        .map(|v| text(v, "id").map(str::to_owned))
        .collect::<Result<_, _>>()?;
    if expected_editions.is_empty() || expected_editions != actual_editions {
        return Err(native_unavailable());
    }
    let expected_timelines: BTreeSet<String> =
        serde_json::from_value(shape["timeline_ids"].clone()).map_err(|_| native_unavailable())?;
    let mut actual_timelines = BTreeSet::new();
    for timeline in array(&facts, "timelines")? {
        let id = text(timeline, "id")?;
        let edition = text(timeline, "edition_item_id")?;
        if !actual_editions.contains(edition) || !actual_timelines.insert(id.to_owned()) {
            return Err(native_unavailable());
        }
        let motion_edition = edition
            .strip_prefix("motion.edition:")
            .ok_or_else(native_unavailable)?;
        let projected:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=? AND e.id=? AND e.item_id=?)").bind(id).bind(motion_edition).bind(&body.id).fetch_one(&mut *conn).await?;
        if !projected {
            return Err(native_unavailable());
        }
    }
    if actual_timelines != expected_timelines {
        return Err(native_unavailable());
    }
    body.default_timeline_id = if actual_timelines.len() == 1 {
        actual_timelines.into_iter().next()
    } else {
        None
    };
    body.availability = "unknown".into();
    body.library_ids.clear();
    Ok(body)
}

async fn capture(
    app: &App,
    caller: &Caller,
    id: &str,
    timeline: bool,
) -> Result<(Transaction<'static, Sqlite>, Details), Problem> {
    caller.require(Permission::CatalogRead)?;
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let mut initial = app.db.begin().await?;
    let principal = caller
        .reauthorize(&mut initial, Some(Permission::CatalogRead))
        .await?;
    match access::catalog_scope(&principal) {
        CatalogScope::Nothing => return Err(Problem::not_found()),
        CatalogScope::All => {}
        // Scoped physical proof is a separate adapter; copied library_sources
        // and definition source_ids are not sufficient authorization.
        _ => {
            return Err(Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "native_scope_unavailable",
                "Native physical detail scope is not available",
            ));
        }
    }
    initial.commit().await?;
    let selection = runtime
        .prepare_all_scope()
        .await
        .map_err(native_read_error)?;
    runtime
        .rebuild_projection()
        .await
        .map_err(native_read_error)?;
    let mut tx: Transaction<'static, Sqlite> = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    if access::catalog_scope(&principal) != CatalogScope::All {
        return Err(native_unavailable());
    }
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(native_read_error)?;
    // A cache lookup is only a hint for finding the owning work. The named
    // timeline must subsequently occur in that work's complete native capture.
    let work: String = if timeline {
        sqlx::query_scalar(
            "SELECT e.item_id FROM timelines t JOIN editions e ON e.id=t.edition_id WHERE t.id=?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(Problem::not_found)?
    } else {
        id.into()
    };
    let rows = runtime
        .read_projectable_items(&mut tx, vec![selection], Some(work.clone()))
        .await
        .map_err(native_read_error)?;
    let row = rows.first().ok_or_else(Problem::not_found)?;
    if rows.len() != 1 || row[0] != work {
        return Err(native_unavailable());
    }
    // Existing exact-occurrence adapter must accept the entire work first.
    // This never turns a missing/retired version into a playable one.
    projected_native_work(runtime, &mut tx, row, &[]).await?;
    let (catalog,incarnation,generation):(String,String,i64)=sqlx::query_as("SELECT catalog_id,incarnation,generation FROM catabolic_projection_state WHERE singleton=1").fetch_one(&mut *tx).await?;
    let work_for_capture = work.clone();
    let native = runtime
        .with_host(move |host| {
            let capture = host.capture_work_dependencies(generation, &work_for_capture)?;
            Ok((
                capture.catalog_id().to_owned(),
                capture.incarnation().to_owned(),
                capture.dependencies().clone(),
            ))
        })
        .await
        .map_err(native_read_error)?;
    if native.0 != catalog || native.1 != incarnation {
        return Err(native_unavailable());
    }
    let facts = native.2;
    let mut details = Details::default();
    let mut timeline_bases = BTreeMap::new();
    let mut version_bases = BTreeMap::new();
    for owner in array(&facts, "owners")? {
        let native_id = text(owner, "id")?;
        if native_id == work {
            continue;
        }
        let metadata: Value =
            serde_json::from_str(text(owner, "metadata")?).map_err(|_| native_unavailable())?;
        if owner["kind"] == "custom:motion_edition" {
            let model = &metadata["motion_edition"];
            let source = &model["source_edition"];
            let edition_id = text(source, "id")?;
            if model["contract_version"] != 1
                || source["item_id"] != work
                || native_id != format!("motion.edition:{edition_id}")
            {
                return Err(native_unavailable());
            }
            let revision = motion_catalog_backend::revision_bridge::wire_revision_in(
                &mut tx,
                native_id,
                &catalog,
                &incarnation,
                head(owner, "native_item_revision")?,
            )
            .await
            .map_err(native_read_error)?;
            let mut edition = edition_body(
                edition_id.into(),
                work.clone(),
                text(source, "label")?.into(),
                i64::try_from(revision).map_err(|_| native_unavailable())?,
            );
            edition.timeline_ids.clear();
            details.editions.insert(edition_id.into(), edition);
            for source in array(model, "source_timelines")? {
                if source["edition_id"] != edition_id {
                    return Err(native_unavailable());
                }
                let id = text(source, "id")?.to_owned();
                if timeline_bases.insert(id, source.clone()).is_some() {
                    return Err(native_unavailable());
                }
            }
        } else if owner["kind"] == "custom:motion_version" {
            let model = &metadata["motion_version"];
            let source = &model["source_version"];
            let id = text(source, "id")?;
            if model["contract_version"] != 1
                || native_id != format!("motion.version:{id}")
                || version_bases
                    .insert(id.to_owned(), source.clone())
                    .is_some()
            {
                return Err(native_unavailable());
            }
        } else {
            return Err(native_unavailable());
        }
    }
    // A typed retirement is an explicit descriptive domain, never a live DTO.
    if !array(&facts, "version_retirements")?.is_empty() {
        return Err(native_unavailable());
    }
    for native in array(&facts, "timelines")? {
        let id = text(native, "id")?;
        let edition = text(native, "edition_item_id")?
            .strip_prefix("motion.edition:")
            .ok_or_else(native_unavailable)?;
        let base = timeline_bases.get(id).ok_or_else(native_unavailable)?;
        if base["edition_id"] != edition
            || !base["order_group_id"].is_null()
            || !base["order_position"].is_null()
        {
            return Err(native_unavailable());
        }
        let current = head(native, "current_revision")?;
        let row = array(&facts, "timeline_heads")?
            .iter()
            .find(|r| r["timeline_id"] == id && r["revision"] == current)
            .ok_or_else(native_unavailable)?;
        let duration = match (
            row["coverage_start_us"].as_i64(),
            row["coverage_end_us"].as_i64(),
        ) {
            (None, None) => None,
            (Some(0), Some(end)) if end > 0 && end % 1000 == 0 => Some(end / 1000),
            _ => return Err(native_unavailable()),
        };
        details
            .editions
            .get_mut(edition)
            .ok_or_else(native_unavailable)?
            .timeline_ids
            .push(id.into());
        details.timelines.insert(id.into(),json!({"id":id,"revision":bridged(base,current)?.to_string(),"item_id":work,"edition_id":edition,"duration_ms":duration,"version_ids":[],"order_group_id":null,"order_position":null}));
    }
    for native in array(&facts, "versions")? {
        let id = text(native, "id")?;
        let base = version_bases.get(id).ok_or_else(native_unavailable)?;
        let current = head(native, "current_revision")?;
        let native_head = array(&facts, "version_heads")?
            .iter()
            .find(|r| r["version_id"] == id && r["revision"] == current)
            .ok_or_else(native_unavailable)?;
        let timeline = text(native_head, "timeline_id")?;
        if base["timeline_id"] != timeline
            || native["association_item_id"] != format!("motion.version:{id}")
        {
            return Err(native_unavailable());
        }
        let cache:Option<(i64,i64,i64,i64,i64,String)>=sqlx::query_as("SELECT motion_version_revision,motion_timeline_revision,native_version_revision,native_timeline_revision,native_current_timeline_revision,manifest_json FROM catabolic_projection_versions WHERE version_id=? AND timeline_id=? AND generation=?")
            .bind(id).bind(timeline).bind(generation).fetch_optional(&mut *tx).await?;
        let timeline_body = details
            .timelines
            .get_mut(timeline)
            .ok_or_else(native_unavailable)?;
        let wire = bridged(base, current)?;
        let manifest = if let Some((
            cached_wire,
            timeline_wire,
            native_revision,
            bound_head,
            current_head,
            raw,
        )) = cache
        {
            if cached_wire != wire
                || native_revision != current
                || bound_head != head(native_head, "timeline_revision")?
                || bound_head != current_head
                || timeline_body["revision"] != timeline_wire.to_string()
            {
                return Err(native_unavailable());
            }
            let envelope: Value = serde_json::from_str(&raw).map_err(|_| native_unavailable())?;
            let origin = motion_catalog_backend::version_proof::origin_in(
                &mut tx,
                id,
                &catalog,
                &incarnation,
                generation,
            )
            .await
            .map_err(native_read_error)?;
            if !matches!(
                (origin, text(base, "origin")?),
                (
                    motion_catalog_backend::version_proof::ProofOrigin::Original,
                    "original"
                ) | (
                    motion_catalog_backend::version_proof::ProofOrigin::ReceiptVerifiedGenerated,
                    "generated"
                )
            ) {
                return Err(native_unavailable());
            }
            let proof = motion_catalog_backend::version_proof::read(
                &envelope,
                &catalog,
                &incarnation,
                generation,
                &catabolic_store::catalog_projection::VersionLineageRequest {
                    work_item_id: work.clone(),
                    edition_item_id: format!(
                        "motion.edition:{}",
                        text(timeline_body, "edition_id")?
                    ),
                    version_item_id: format!("motion.version:{id}"),
                    version_id: id.into(),
                },
                origin,
            )
            .map_err(native_read_error)?;
            if proof.head["version_revision"] != current
                || proof.head["timeline_revision"] != native_head["timeline_revision"]
            {
                return Err(native_unavailable());
            }
            proof.version
        } else {
            // Absence of a ready cache is not retirement or unavailability. A
            // separate current native observation must prove a missing/replaced
            // occurrence, with the complete captured current head/binding set.
            unavailable_detail(runtime, &facts, id, generation, &catalog, &incarnation).await?
        };
        let contract = &manifest["version"];
        let (files, available) = bindings(
            &mut tx,
            contract,
            generation,
            text(base, "origin")? == "original",
        )
        .await?;
        let equivalence = match manifest["equivalence"]["basis"].as_str() {
            Some("declared") => "declared",
            Some("verified") => "verified",
            None if manifest["equivalence"].is_null() => "unknown",
            _ => return Err(native_unavailable()),
        };
        details.versions.insert(id.into(),json!({"id":id,"revision":wire.to_string(),"timeline_id":timeline,"label":text(base,"label")?,"origin":text(base,"origin")?,"files":files,"timeline_equivalence":equivalence,"availability":if available {"available"} else {"unavailable"}}));
        timeline_body["version_ids"]
            .as_array_mut()
            .ok_or_else(native_unavailable)?
            .push(json!(id));
    }
    if details.timelines.values().any(|row| {
        row["version_ids"]
            .as_array()
            .is_none_or(|ids| ids.len() > 100)
    }) {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "timeline_too_large",
            "Timeline exceeds the public representation bound",
        ));
    }
    if timeline && !details.timelines.contains_key(id) {
        return Err(Problem::not_found());
    }
    let current = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    if access::catalog_scope(&current) != CatalogScope::All {
        return Err(native_unavailable());
    }
    runtime
        .validate_projection(&mut tx)
        .await
        .map_err(native_read_error)?;
    Ok((tx, details))
}

/// Descriptive bindings only. Never issues readiness or retained equivalence.
async fn unavailable_detail(
    runtime: &crate::native_catalog::Runtime,
    complete: &Value,
    id: &str,
    generation: i64,
    catalog: &str,
    incarnation: &str,
) -> Result<Value, Problem> {
    let named = id.to_owned();
    let snapshot = runtime
        .with_host(move |host| {
            host.projection_unavailable_versions(generation, &[named])
                .map_err(Into::into)
        })
        .await
        .map_err(native_read_error)?;
    if snapshot["catalog_id"] != catalog
        || snapshot["incarnation"] != incarnation
        || snapshot["generation"] != generation
        || snapshot["contract_version"] != 1
    {
        return Err(native_unavailable());
    }
    let versions = array(&snapshot, "versions")?;
    if versions.len() != 1 || versions[0]["version_id"] != id {
        return Err(native_unavailable());
    }
    let observed = &versions[0];
    let owner = array(complete, "versions")?
        .iter()
        .find(|v| v["id"] == id)
        .ok_or_else(native_unavailable)?;
    let current = array(complete, "version_heads")?
        .iter()
        .find(|v| v["version_id"] == id && v["revision"] == owner["current_revision"])
        .ok_or_else(native_unavailable)?;
    if observed["version_revision"] != owner["current_revision"]
        || observed["association_item_id"] != owner["association_item_id"]
        || observed["timeline_id"] != current["timeline_id"]
        || observed["timeline_revision"] != current["timeline_revision"]
        || observed["timeline_revision"] != observed["current_timeline_revision"]
    {
        return Err(native_unavailable());
    }
    let native: Vec<_> = array(complete, "bindings")?
        .iter()
        .filter(|b| b["version_id"] == id)
        .collect();
    let bindings = array(observed, "bindings")?;
    if native.len() != bindings.len() || bindings.is_empty() {
        return Err(native_unavailable());
    }
    let mut descriptive = Vec::new();
    for b in bindings {
        let exact = native
            .iter()
            .find(|n| n["binding_id"] == b["binding_id"] && n["revision"] == b["binding_revision"])
            .ok_or_else(native_unavailable)?;
        if [
            "association_id",
            "profile",
            "part",
            "source_kind",
            "source_start_us",
            "source_end_us",
            "timeline_start_us",
            "timeline_end_us",
        ]
        .iter()
        .any(|k| exact[*k] != b[*k])
            || exact["file_id"] != b["file"]["file_id"]
            || exact["file_revision"] != b["file"]["revision"]
        {
            return Err(native_unavailable());
        }
        let interval = |start: &Value, end: &Value| -> Result<Value, Problem> {
            match (start.as_i64(), end.as_i64()) {
                (None, None) if start.is_null() && end.is_null() => Ok(Value::Null),
                (Some(a), Some(z)) if a >= 0 && z > a => Ok(json!({"start_us":a,"end_us":z})),
                _ => Err(native_unavailable()),
            }
        };
        let source_interval = interval(&b["source_start_us"], &b["source_end_us"])?;
        let source = match b["source_kind"].as_str() {
            Some("whole_file") => {
                json!({"kind":"whole_file","duration_us":if source_interval.is_null(){None}else{Some(source_interval["end_us"].as_i64().unwrap()-source_interval["start_us"].as_i64().unwrap())}})
            }
            Some("slice") if !source_interval.is_null() => {
                json!({"kind":"slice","interval":source_interval})
            }
            _ => return Err(native_unavailable()),
        };
        descriptive.push(json!({"binding_id":b["binding_id"],"association_id":b["association_id"],"file":b["file"],"part":b["part"],"source":source,"timeline_interval":interval(&b["timeline_start_us"],&b["timeline_end_us"])?}));
    }
    Ok(
        json!({"version":{"version_id":id,"association_item_id":observed["association_item_id"],"edition_item_id":observed["edition_item_id"],"timeline_id":observed["timeline_id"],"bindings":descriptive},"equivalence":null,"descriptive_only":true}),
    )
}

async fn bindings(
    tx: &mut Transaction<'_, Sqlite>,
    contract: &Value,
    generation: i64,
    original: bool,
) -> Result<(Vec<Value>, bool), Problem> {
    let bindings = array(contract, "bindings")?;
    if bindings.len() > 100 {
        return Err(native_unavailable());
    }
    let mut result = Vec::new();
    let mut available = !bindings.is_empty();
    let mut cursor = Some(0i64);
    for binding in bindings {
        let candidates:Vec<(String,i64,String)>=sqlx::query_as("SELECT file_id,available,mapping_json FROM catabolic_projection_files WHERE native_file_id=? AND native_revision=? AND generation=?")
            .bind(text(&binding["file"],"file_id")?).bind(text(&binding["file"],"revision")?).bind(generation).fetch_all(&mut **tx).await?;
        if candidates.len() != 1 {
            return Err(native_unavailable());
        }
        let (file_id, present, mapping) = &candidates[0];
        let mapping: Value = serde_json::from_str(mapping).map_err(|_| native_unavailable())?;
        let preserved = array(&mapping, "version_files")?
            .iter()
            .find(|r| r["version_id"] == contract["version_id"] && r["part"] == binding["part"])
            .ok_or_else(native_unavailable)?;
        if preserved["file_id"] != *file_id
            || preserved["file_revision"] != mapping["motion_file"]["revision"]
        {
            return Err(native_unavailable());
        }
        let interval = |v: &Value| Some((v["start_us"].as_i64()?, v["end_us"].as_i64()?));
        let source = &binding["source"];
        let extent = match source["kind"].as_str() {
            Some("whole_file") => cursor.and_then(|c| {
                playscale_core::catalog_projection::project_extent(
                    None,
                    source["duration_us"].as_i64(),
                    interval(&binding["timeline_interval"]),
                    c,
                )
            }),
            Some("slice") => interval(&source["interval"]).and_then(|s| {
                cursor.and_then(|c| {
                    playscale_core::catalog_projection::project_extent(
                        Some(s),
                        None,
                        interval(&binding["timeline_interval"]),
                        c,
                    )
                })
            }),
            _ => return Err(native_unavailable()),
        };
        let (start, end) = if let Some(extent) = extent {
            cursor = Some(extent.next_timeline_us);
            (extent.start_ms, extent.end_ms)
        } else if source["kind"] == "whole_file"
            && source["duration_us"].is_null()
            && binding["timeline_interval"].is_null()
        {
            cursor = None;
            (None, None)
        } else {
            return Err(native_unavailable());
        };
        let admitted = if original {
            let source = text(&mapping["source"], "library_id")?;
            motion_catalog_backend::source_lifecycle::current_in(&mut **tx, source)
                .await
                .map_err(native_read_error)?
                .state
                == motion_catalog_backend::source_lifecycle::State::Active
        } else {
            true
        };
        available &= *present == 1 && admitted;
        result.push(json!({"file":{"file_id":file_id,"file_revision":preserved["file_revision"]},"part":binding["part"],"start_ms":start,"end_ms":end}));
    }
    Ok((result, available))
}

pub(super) async fn editions(
    app: &App,
    caller: &Caller,
    id: &str,
    q: PageQuery,
) -> Result<Json<Page<EditionBody>>, Problem> {
    let limit = q.limit()?;
    let after = q.after()?.to_owned();
    let (mut tx, details) = capture(app, caller, id, false).await?;
    let rows = details
        .editions
        .into_iter()
        .filter(|(id, _)| id > &after)
        .take(limit as usize + 1)
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page))
}
pub(super) async fn timelines(
    app: &App,
    caller: &Caller,
    id: &str,
    q: PageQuery,
) -> Result<Response, Problem> {
    let limit = q.limit()?;
    let after = q.after()?.to_owned();
    let (mut tx, details) = capture(app, caller, id, false).await?;
    let rows = details
        .timelines
        .into_iter()
        .filter(|(id, _)| id > &after)
        .take(limit as usize + 1)
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page).into_response())
}
pub(super) async fn timeline(app: &App, caller: &Caller, id: &str) -> Result<Response, Problem> {
    let (tx, mut details) = capture(app, caller, id, true).await?;
    let body = details
        .timelines
        .remove(id)
        .ok_or_else(Problem::not_found)?;
    let revision = text(&body, "revision")?
        .parse()
        .map_err(|_| native_unavailable())?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, &body, revision))
}
pub(super) async fn versions(
    app: &App,
    caller: &Caller,
    id: &str,
    q: PageQuery,
) -> Result<Response, Problem> {
    let limit = q.limit()?;
    let after = q.after()?.to_owned();
    let (mut tx, details) = capture(app, caller, id, true).await?;
    let rows = details
        .versions
        .into_iter()
        .filter(|(key, value)| key > &after && value["timeline_id"] == id)
        .take(limit as usize + 1)
        .collect();
    let page = page(&mut tx, &caller.principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(page).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mapping() -> sqlx::SqlitePool {
        // Decoder-only fixtures. These rows are never treated as admission or
        // byte authority; the public adapter separately captures native owners.
        // The non-original flag isolates extent/mapping decoding here. It does
        // not establish generated provenance (the caller must prove that).
        // Original lifecycle positives are covered by real outer-router tests.
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE catabolic_projection_files(file_id TEXT,available INTEGER,mapping_json TEXT,native_file_id TEXT,native_revision TEXT,generation INTEGER)").execute(&db).await.unwrap();
        let value = json!({"motion_file":{"id":"motion-file","revision":"motion-revision"},"version_files":[{"version_id":"version","part":1,"file_id":"motion-file","file_revision":"motion-revision"}]});
        sqlx::query("INSERT INTO catabolic_projection_files VALUES('motion-file',1,?,'native-file','native-revision',7)").bind(value.to_string()).execute(&db).await.unwrap();
        db
    }
    fn contract() -> Value {
        json!({"version_id":"version","bindings":[{"part":1,"file":{"file_id":"native-file","revision":"native-revision"},"source":{"kind":"whole_file","duration_us":1000000},"timeline_interval":{"start_us":0,"end_us":1000000}}]})
    }
    #[tokio::test]
    async fn exact_native_binding_never_substitutes_a_same_revision_file() {
        let db = mapping().await;
        let mut tx = db.begin().await.unwrap();
        let (body, available) = bindings(&mut tx, &contract(), 7, false).await.unwrap();
        assert!(available);
        assert_eq!(
            body[0]["file"],
            json!({"file_id":"motion-file","file_revision":"motion-revision"})
        );
        assert!(body[0]["start_ms"].is_null());
        assert!(body[0]["end_ms"].is_null());
        assert!(bindings(&mut tx, &contract(), 8, false).await.is_err());
        let mut wrong = contract();
        wrong["bindings"][0]["file"]["file_id"] = json!("other-native-file");
        assert!(bindings(&mut tx, &wrong, 7, false).await.is_err());
        sqlx::query("UPDATE catabolic_projection_files SET available=0")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert!(!bindings(&mut tx, &contract(), 7, false).await.unwrap().1);
    }
    #[tokio::test]
    async fn original_binding_requires_independent_source_lifecycle() {
        let db = mapping().await;
        let mut tx = db.begin().await.unwrap();
        assert!(
            bindings(&mut tx, &contract(), 7, true).await.is_err(),
            "decoder-only mapping cannot grant original-source admission"
        );
        sqlx::query("UPDATE catabolic_projection_files SET mapping_json=json_set(mapping_json,'$.source.library_id','unregistered-source')")
            .execute(&mut *tx).await.unwrap();
        assert!(
            bindings(&mut tx, &contract(), 7, true).await.is_err(),
            "naming a source does not create its lifecycle proof"
        );
    }
    #[tokio::test]
    async fn whole_file_unknown_extent_stays_null_and_nonrepresentable_extent_refuses() {
        let db = mapping().await;
        let mut tx = db.begin().await.unwrap();
        let mut value = contract();
        value["bindings"][0]["source"]["duration_us"] = Value::Null;
        value["bindings"][0]["timeline_interval"] = Value::Null;
        let (body, _) = bindings(&mut tx, &value, 7, false).await.unwrap();
        assert!(body[0]["start_ms"].is_null() && body[0]["end_ms"].is_null());
        value["bindings"][0]["timeline_interval"] = json!({"start_us":0,"end_us":1000000});
        assert!(bindings(&mut tx, &value, 7, false).await.is_err());
        let mut fractional = contract();
        fractional["bindings"][0]["source"]["duration_us"] = json!(1000001);
        fractional["bindings"][0]["timeline_interval"]["end_us"] = json!(1000001);
        assert!(bindings(&mut tx, &fractional, 7, false).await.is_err());
    }
}
