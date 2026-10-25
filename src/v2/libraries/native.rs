//! Source HTTP adapter. Native is physical binding owner; Motion keeps only
//! immutable registration intent/ack and current principal/idempotency custody.
use super::*;
use motion_catalog_backend::{source_registry::NativeSourceDescriptor, sources::SourceMapping};
use sha2::{Digest, Sha256};

fn unavailable(e: impl std::fmt::Display) -> Problem {
    tracing::warn!(error=%e,"native source operation rejected");
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "native_source_unavailable",
        "Native source is unavailable or its proof changed",
    )
}
async fn active_mapping_in(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<SourceMapping, Problem> {
    let (lifecycle, mapping) =
        motion_catalog_backend::source_lifecycle::current_mapping_in(conn, id)
            .await
            .map_err(unavailable)?;
    if lifecycle.state != playscale_core::source_lifecycle::State::Active {
        return Err(unavailable("source is not active"));
    }
    Ok(mapping)
}
async fn require_current_mapping_in(
    conn: &mut sqlx::SqliteConnection,
    saved: &SourceMapping,
) -> Result<(), Problem> {
    if active_mapping_in(conn, &saved.library_id).await? != *saved {
        return Err(unavailable("source mapping changed before library effect"));
    }
    Ok(())
}
fn descriptor_value(d: &NativeSourceDescriptor) -> Value {
    serde_json::json!({"id":d.id,"revision":"1","binding_revision":"1","name":d.name,"root_path":d.root,"volume_identity":d.root_identity,"availability":"unknown","exclusions":d.exclusions})
}
struct ResolvedSource {
    descriptor: NativeSourceDescriptor,
    wire: Value,
}
async fn descriptors(conn: &mut sqlx::SqliteConnection) -> Result<Vec<ResolvedSource>, Problem> {
    descriptors_with_tombstones(conn, false).await
}
async fn descriptors_with_tombstones(
    conn: &mut sqlx::SqliteConnection,
    include_tombstones: bool,
) -> Result<Vec<ResolvedSource>, Problem> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT descriptor_json FROM catabolic_source_registry ORDER BY id")
            .fetch_all(&mut *conn)
            .await?;
    let mut output = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for raw in rows {
        let descriptor: NativeSourceDescriptor =
            serde_json::from_str(&raw).map_err(Problem::internal)?;
        if !ids.insert(descriptor.id.clone()) {
            return Err(unavailable("duplicate native source descriptor"));
        }
        output.push(ResolvedSource {
            wire: descriptor_value(&descriptor),
            descriptor,
        });
    }
    for imported in motion_catalog_backend::imported_sources::imported_sources_in(&mut *conn)
        .await
        .map_err(unavailable)?
    {
        let descriptor = imported.descriptor;
        if !ids.insert(descriptor.id.clone()) {
            return Err(unavailable("source descriptor origins collide"));
        }
        let mut value = descriptor_value(&descriptor);
        for field in ["revision", "binding_revision"] {
            let revision = imported.proof.source_row[field]
                .as_u64()
                .ok_or_else(|| unavailable("frozen source revision missing"))?;
            value[field] = Value::String(revision.to_string());
        }
        output.push(ResolvedSource {
            descriptor,
            wire: value,
        });
    }
    let mapped: Vec<String> =
        sqlx::query_scalar("SELECT library_id FROM catabolic_source_mappings ORDER BY library_id")
            .fetch_all(&mut *conn)
            .await?;
    if mapped.iter().any(|id| !ids.contains(id)) {
        return Err(unavailable(
            "mapped source descriptor has not been reconciled",
        ));
    }
    let mut visible = Vec::new();
    for mut source in output {
        let (lifecycle, mapping) = motion_catalog_backend::source_lifecycle::current_mapping_in(
            conn,
            &source.descriptor.id,
        )
        .await
        .map_err(unavailable)?;
        // Project current authenticated bindings, without rewriting imported evidence.
        source.descriptor.root = mapping.motion_root;
        source.descriptor.root_identity = mapping.motion_root_identity;
        source.descriptor.exclusions = mapping.exclusions;
        source.wire["root_path"] = serde_json::json!(source.descriptor.root);
        source.wire["volume_identity"] = serde_json::json!(source.descriptor.root_identity);
        source.wire["exclusions"] = serde_json::json!(source.descriptor.exclusions);
        if !include_tombstones
            && lifecycle.state == playscale_core::source_lifecycle::State::Tombstoned
        {
            continue;
        }
        source.wire["revision"] = Value::String(lifecycle.source_revision.to_string());
        source.wire["binding_revision"] = Value::String(lifecycle.binding_revision.to_string());
        if lifecycle.state == playscale_core::source_lifecycle::State::Withdrawn {
            source.wire["availability"] = Value::String("unavailable".into());
        }
        visible.push(source);
    }
    let mut output = visible;
    output.sort_by(|a, b| a.descriptor.id.cmp(&b.descriptor.id));
    Ok(output)
}
/// Internal administrative inventory, not a physical or scan admission grant.
/// Reuses the complete descriptor/mapping/native binding owner, retaining tombstones.
pub(crate) async fn complete_source_inventory_in(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
) -> Result<Vec<(String, playscale_core::source_lifecycle::LifecycleProof)>, Problem> {
    let (count, bytes): (i64, i64) = sqlx::query_as("SELECT count(*),COALESCE(sum(bytes),0) FROM (SELECT length(CAST(descriptor_json AS BLOB)) AS bytes FROM catabolic_source_registry UNION ALL SELECT length(CAST(descriptor_json AS BLOB))+length(CAST(proof_json AS BLOB)) AS bytes FROM catabolic_imported_source_descriptors)").fetch_one(&mut *conn).await?;
    if count > 10000 || bytes > 16 * 1024 * 1024 {
        return Err(unavailable("complete source inventory budget exceeded"));
    }
    let (mapping_count, mapping_bytes): (i64,i64) = sqlx::query_as("SELECT count(*),COALESCE(sum(length(CAST(mapping_json AS BLOB))),0) FROM catabolic_source_mappings").fetch_one(&mut *conn).await?;
    if mapping_count > 10000 || mapping_bytes > 16 * 1024 * 1024 {
        return Err(unavailable(
            "complete source mapping inventory budget exceeded",
        ));
    }
    if app.native_catalog.is_none() {
        return Err(unavailable("native source owner missing"));
    }
    let all = descriptors_with_tombstones(conn, true).await?;
    audit_descriptors(app, conn, &all).await?;
    let mut result = Vec::with_capacity(all.len());
    for source in all {
        let lifecycle =
            motion_catalog_backend::source_lifecycle::current_in(conn, &source.descriptor.id)
                .await
                .map_err(unavailable)?;
        result.push((source.descriptor.id, lifecycle));
    }
    Ok(result)
}
async fn audit_descriptors(
    app: &App,
    conn: &mut sqlx::SqliteConnection,
    descriptors: &[ResolvedSource],
) -> Result<(), Problem> {
    let mut mappings = Vec::new();
    for descriptor in descriptors {
        let (_, mapping) = motion_catalog_backend::source_lifecycle::current_mapping_in(
            conn,
            &descriptor.descriptor.id,
        )
        .await
        .map_err(unavailable)?;
        mappings.push(mapping);
    }
    app.native_catalog
        .as_ref()
        .unwrap()
        .with_host(move |host| {
            let identity = host.identity()?;
            for mapping in mappings {
                anyhow::ensure!(
                    identity["catalog_id"] == mapping.catalog.catalog_id
                        && identity["catalog_incarnation"] == mapping.catalog.incarnation
                        && identity["profile"] == mapping.profile,
                    "source belongs to another native catalog"
                );
                anyhow::ensure!(
                    host.source_binding(&mapping.native_source)? == mapping.native_binding
                        && host.source_observation_policy(&mapping.native_source)?
                            == mapping.native_policy,
                    "source native binding/policy changed"
                );
            }
            Ok(())
        })
        .await
        .map_err(unavailable)
}
pub(super) async fn list(app: App, caller: Caller, q: PageQuery) -> Result<Response, Problem> {
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let after = q.after()?;
    let limit = q.limit()?;
    let selected: Vec<_> = descriptors(&mut tx)
        .await?
        .into_iter()
        .filter(|v| v.descriptor.id.as_str() > after)
        .take(limit as usize + 1)
        .collect();
    audit_descriptors(&app, &mut tx, &selected).await?;
    let rows = selected
        .into_iter()
        .map(|v| (v.descriptor.id.clone(), v.wire))
        .collect();
    let result = page(&mut tx, &principal, rows, limit).await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}
pub(super) async fn get(app: App, caller: Caller, id: String) -> Result<Response, Problem> {
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let d = descriptors(&mut tx)
        .await?
        .into_iter()
        .find(|v| v.descriptor.id == id)
        .ok_or_else(Problem::not_found)?;
    audit_descriptors(&app, &mut tx, std::slice::from_ref(&d)).await?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, d.wire))
}
pub(super) async fn create(
    app: App,
    caller: Caller,
    headers: HeaderMap,
    body: Body<SourceInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let runtime = app.native_catalog.as_ref().unwrap().clone();
    let _serial = app.jobs.lock().await;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createSource",
        target: "-",
        key: &key,
    };
    let stored = record.load(&mut tx).await?;
    if access::idempotency(
        stored.as_ref().map(|v| &v.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let stored = stored.unwrap();
        let body = stored.body.unwrap_or_default();
        let source = body["id"]
            .as_str()
            .ok_or_else(|| unavailable("saved source response has no identity"))?;
        let lifecycle = motion_catalog_backend::source_lifecycle::current_in(&mut tx, source)
            .await
            .map_err(unavailable)?;
        if lifecycle.state != playscale_core::source_lifecycle::State::Active {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "source_withdrawn",
                "Source registration cannot reactivate a withdrawn source",
            ));
        }
        return Ok(tagged(stored.status, body));
    }
    let operation = format!(
        "source-register:{:x}",
        Sha256::digest(format!("{}\0{}", principal.id, key).as_bytes())
    );
    let id = format!("{:x}", Sha256::digest(operation.as_bytes()));
    let saved:Option<(String,String)>=sqlx::query_as("SELECT request_digest,descriptor_json FROM catabolic_source_registration_intents WHERE operation_id=?").bind(&operation).fetch_optional(&mut *tx).await?;
    if saved
        .as_ref()
        .is_some_and(|(digest, _)| digest != &body.digest)
    {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Idempotency key was used for another source request",
        ));
    }
    tx.commit().await?;
    if body.value.root_path.is_empty() || body.value.root_path.len() > 4096 {
        return Err(Problem::invalid("invalid_root", "Invalid source root"));
    }
    let candidate = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::administration::candidate_root(&app, body.value.root_path.clone().into()),
    )
    .await
    .map_err(|_| {
        Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "source_timeout",
            "Source verification timed out",
        )
    })?
    .map_err(unavailable)?;
    playscale_core::sources::valid_name(&body.value.name)
        .map_err(|_| Problem::invalid("invalid_source_name", "Invalid source name"))?;
    let exclusions = playscale_core::sources::normalize_exclusions(&body.value.exclusions)
        .map_err(|_| Problem::invalid("invalid_source_exclusions", "Invalid source exclusions"))?;
    // This reserved key is explicitly NOT a logical library or authorization
    // scope. Real membership comes only from a native saved library selection.
    let descriptor = NativeSourceDescriptor {
        id: id.clone(),
        logical_library_id: "motion:unassigned".into(),
        name: body.value.name,
        root: candidate.root.clone(),
        root_identity: candidate.identity.clone(),
        exclusions,
    };
    if let Some((_, raw)) = saved {
        if serde_json::from_str::<NativeSourceDescriptor>(&raw).map_err(Problem::internal)?
            != descriptor
        {
            return Err(unavailable("pending source root/descriptor changed"));
        }
    }
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let roots:Vec<(String,String)>=sqlx::query_as("SELECT id,root FROM libraries UNION ALL SELECT id,json_extract(descriptor_json,'$.root') FROM catabolic_source_registry").fetch_all(&mut *tx).await?;
    // Private generated output is not a public source or a membership grant.
    // Check its immutable binding under the same writer as public admission so
    // neither registration order can expose generated bytes as new originals.
    motion_catalog_backend::source_registry::ensure_public_source_root_in(
        &mut tx,
        &id,
        std::path::Path::new(&descriptor.root),
    )
    .await
    .map_err(unavailable)?;
    for (other, root) in roots {
        if other != id && roots_overlap(&descriptor.root, &root) {
            return Err(Problem::new(
                StatusCode::CONFLICT,
                "source_overlap",
                "Root overlaps a registered source",
            ));
        }
    }
    sqlx::query("INSERT INTO catabolic_source_registration_intents VALUES(?,?,?,?,?) ON CONFLICT DO NOTHING").bind(&operation).bind(&id).bind(&body.digest).bind(serde_json::to_string(&descriptor).map_err(Problem::internal)?).bind(crate::now()).execute(&mut *tx).await?;
    let captured:(String,String)=sqlx::query_as("SELECT request_digest,descriptor_json FROM catabolic_source_registration_intents WHERE operation_id=?").bind(&operation).fetch_one(&mut *tx).await?;
    if captured.0 != body.digest
        || serde_json::from_str::<NativeSourceDescriptor>(&captured.1).map_err(Problem::internal)?
            != descriptor
    {
        return Err(Problem::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Source registration intent changed",
        ));
    }
    tx.commit().await?;
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let expected = descriptor.clone();
    let (prepared, mut tx) = runtime
        .with_host(move |host| {
            let _candidate = candidate; // actual filesystem permit lives through native effect
            let prepared = motion_catalog_backend::source_registry::register_with_host(
                host,
                &expected.id,
                &expected.logical_library_id,
                &expected.name,
                expected.root.into(),
                expected.exclusions,
            )?;
            anyhow::ensure!(
                prepared.descriptor().root_identity == expected.root_identity,
                "source root replaced before native binding"
            );
            Ok((prepared, tx))
        })
        .await
        .map_err(unavailable)?;
    let value = descriptor_value(prepared.descriptor());
    let already_registered: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM catabolic_source_registry WHERE id=?)")
            .bind(&prepared.descriptor().id)
            .fetch_one(&mut *tx)
            .await?;
    let handle = tokio::runtime::Handle::current();
    let mut tx = runtime
        .with_host(move |host| {
            handle.block_on(async {
                let mapping =
                    motion_catalog_backend::source_registry::acknowledge_native_source_in(
                        &mut tx, prepared,
                    )
                    .await?;
                let lifecycle = motion_catalog_backend::source_lifecycle::initialize_with_host(
                    &mut tx,
                    host,
                    &mapping.library_id,
                    crate::now(),
                )
                .await?;
                anyhow::ensure!(
                    lifecycle.state == playscale_core::source_lifecycle::State::Active,
                    "source registration cannot reactivate withdrawn source"
                );
                Ok(tx)
            })
        })
        .await
        .map_err(unavailable)?;
    Scope {
        principal: &principal.id,
        operation: "createSource",
        target: "-",
        key: &key,
    }
    .save(
        &mut tx,
        &body.digest,
        StatusCode::CREATED,
        Some(&value),
        None,
    )
    .await?;
    if !already_registered {
        sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('sources',?)")
            .bind(
                value["id"]
                    .as_str()
                    .ok_or_else(|| unavailable("source identity missing"))?,
            )
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, value))
}

fn scope_ids(scope: &access::CatalogScope) -> Option<Vec<String>> {
    match scope {
        access::CatalogScope::All => None,
        access::CatalogScope::Libraries(ids) => Some(ids.iter().cloned().collect()),
        access::CatalogScope::Nothing => Some(vec![]),
    }
}
fn library_value(row: &Value) -> Result<Value, Problem> {
    let metadata: Value = serde_json::from_str(
        row[2]
            .as_str()
            .ok_or_else(|| unavailable("missing library metadata"))?,
    )
    .map_err(Problem::internal)?;
    let d = &metadata["motion_library"];
    let id = d["id"]
        .as_str()
        .ok_or_else(|| unavailable("missing library ID"))?;
    if row[0] != format!("motion.library:{id}")
        || row[1] != "custom:motion_library"
        || d["contract_version"] != 1
    {
        return Err(unavailable("malformed native library"));
    }
    let revision = d["catalog_revision"]
        .as_u64()
        .ok_or_else(|| unavailable("missing library revision"))?;
    let name = d["name"]
        .as_str()
        .ok_or_else(|| unavailable("missing library name"))?;
    let language = d["language"]
        .as_str()
        .ok_or_else(|| unavailable("missing library language"))?;
    let _: LibraryKind = serde_json::from_value(d["kind"].clone()).map_err(Problem::internal)?;
    let _: Vec<String> =
        serde_json::from_value(d["source_ids"].clone()).map_err(Problem::internal)?;
    Ok(
        serde_json::json!({"id":id,"revision":revision.to_string(),"name":name,"kind":d["kind"],"language":language,"source_ids":d["source_ids"],"availability":"unknown"}),
    )
}
pub(super) async fn list_libraries(
    app: App,
    caller: Caller,
    q: PageQuery,
) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    let initial = access::catalog_scope(&caller.principal);
    let runtime = app.native_catalog.as_ref().unwrap();
    let grant = runtime
        .prepare_library_definitions(scope_ids(&initial))
        .await
        .map_err(unavailable)?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    if access::catalog_scope(&principal) != initial {
        return Err(unavailable(
            "library authority changed during scope preparation",
        ));
    }
    let after = q.after()?;
    let limit = q.limit()?;
    let rows = runtime
        .read_libraries(&mut tx, vec![grant], None)
        .await
        .map_err(unavailable)?;
    let mut output = Vec::new();
    for row in rows {
        let value = library_value(&row)?;
        let id = value["id"].as_str().unwrap().to_owned();
        if id.as_str() > after {
            output.push((id, value));
        }
    }
    output.sort_by(|a, b| a.0.cmp(&b.0));
    output.truncate(limit as usize + 1);
    let result = page(&mut tx, &principal, output, limit).await?;
    tx.commit().await?;
    Ok(Json(result).into_response())
}
pub(super) async fn get_library(app: App, caller: Caller, id: String) -> Result<Response, Problem> {
    caller.require(Permission::CatalogRead)?;
    if !access::catalog_scope(&caller.principal).library(&id) {
        return Err(Problem::not_found());
    }
    let runtime = app.native_catalog.as_ref().unwrap();
    let grant = runtime
        .prepare_library_definitions(scope_ids(&access::catalog_scope(&caller.principal)))
        .await
        .map_err(unavailable)?;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogRead))
        .await?;
    if !access::catalog_scope(&principal).library(&id) {
        return Err(Problem::not_found());
    }
    let rows = runtime
        .read_libraries(&mut tx, vec![grant], Some(format!("motion.library:{id}")))
        .await
        .map_err(|e| {
            if e.downcast_ref::<crate::native_catalog::ReadFailure>()
                .is_some_and(|failure| {
                    failure.0
                        == catabolic_store::catalog_scope::ScopeErrorCode::NotFoundOrNotVisible
                })
            {
                Problem::not_found()
            } else {
                unavailable(e)
            }
        })?;
    let body = library_value(rows.first().ok_or_else(Problem::not_found)?)?;
    tx.commit().await?;
    Ok(tagged(StatusCode::OK, body))
}
pub(super) async fn create_library(
    app: App,
    caller: Caller,
    headers: HeaderMap,
    body: Body<LibraryInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let key = idempotency_key(&headers)?;
    let runtime = app.native_catalog.as_ref().unwrap().clone();
    let _serial = app.jobs.lock().await;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let record = Scope {
        principal: &principal.id,
        operation: "createLibrary",
        target: "-",
        key: &key,
    };
    let saved = record.load(&mut tx).await?;
    if access::idempotency(
        saved.as_ref().map(|s| &s.record),
        &body.digest,
        crate::now(),
    )? == Idempotent::Replay
    {
        let saved = saved.unwrap();
        return Ok(tagged(saved.status, saved.body.unwrap_or_default()));
    }
    let registered: std::collections::BTreeSet<String> = descriptors(&mut tx)
        .await?
        .into_iter()
        .map(|d| d.descriptor.id)
        .collect();
    playscale_core::sources::validate_library(
        &body.value.name,
        &body.value.language,
        &body.value.source_ids,
        &registered,
    )
    .map_err(|_| {
        Problem::invalid(
            "invalid_library",
            "Invalid library definition or source membership",
        )
    })?;
    let mut mappings = Vec::new();
    for id in &body.value.source_ids {
        let mapping = active_mapping_in(&mut tx, id).await?;
        mappings.push(mapping);
    }
    mappings.sort_by(|a, b| a.library_id.cmp(&b.library_id));
    let operation = format!(
        "library-create:{:x}",
        Sha256::digest(format!("{}\0{}", principal.id, key).as_bytes())
    );
    let id = format!("{:x}", Sha256::digest(operation.as_bytes()));
    let definition = serde_json::json!({"contract_version":1,"id":id,"catalog_revision":1,"name":body.value.name,"kind":body.value.kind,"language":body.value.language,"source_ids":body.value.source_ids,"creation_request_digest":body.digest});
    tx.commit().await?;
    let command = runtime
        .prepare_library(operation, id.clone(), definition.clone())
        .await
        .map_err(unavailable)?;
    let (outcome, mut tx, principal, revision_intent) = loop {
        let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
            app.db.begin_with("BEGIN IMMEDIATE").await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::SourcesManage))
            .await?;
        for mapping in &mappings {
            require_current_mapping_in(&mut tx, mapping).await?;
        }
        let (_, revision_intent) =
            motion_catalog_backend::command_attempts::latest_in(&mut tx, &command)
                .await
                .map_err(unavailable)?;
        let (outcome, witness, mut tx) = runtime
            .execute_work_observed(revision_intent.clone(), tx)
            .await
            .map_err(unavailable)?;
        if let Some(witness) = witness {
            motion_catalog_backend::command_attempts::record_next_in(
                &mut tx,
                &command,
                &revision_intent,
                witness,
            )
            .await
            .map_err(unavailable)?;
            tx.commit().await?;
            continue;
        }
        if !matches!(
            outcome,
            catabolic_store::catalog_commands::CommandOutcome::Committed { .. }
                | catabolic_store::catalog_commands::CommandOutcome::Replay { .. }
        ) {
            return Err(unavailable("native library command did not commit"));
        }
        break (outcome, tx, principal, revision_intent);
    };
    motion_catalog_backend::revision_bridge::acknowledge_fresh_in(
        &mut tx,
        &format!("motion.library:{id}"),
        &revision_intent,
        &outcome,
    )
    .await
    .map_err(unavailable)?;
    let library = id.clone();
    let proof = mappings.clone();
    let (identity,saved,mut tx)=runtime.with_host(move|host|{
        let identity=host.identity()?;
        for mapping in &proof{
            anyhow::ensure!(host.source_binding(&mapping.native_source)?==mapping.native_binding && host.source_observation_policy(&mapping.native_source)?==mapping.native_policy,"native library source proof changed");
            anyhow::ensure!(identity["catalog_id"]==mapping.catalog.catalog_id && identity["catalog_incarnation"]==mapping.catalog.incarnation && identity["profile"]==mapping.profile,"native library source belongs to another catalog");
        }
        let quote=|value:&str|format!("'{}'",value.replace('\'',"''"));
        let locations:Vec<String>=proof.iter().map(|m|quote(&m.native_source)).collect();
        let predicate=if locations.is_empty(){"0".into()}else{format!("a.location IN ({})",locations.join(","))};
        let query=format!("SELECT DISTINCT w.item_id FROM catalog_items w JOIN catalog_relationships e ON e.source_id=w.item_id AND e.kind='custom:motion_edition' AND e.active=1 JOIN catalog_relationships v ON v.source_id=e.target_id AND v.kind='custom:motion_version' AND v.active=1 JOIN catalog_item_files a ON a.item_id=v.target_id AND a.active=1 WHERE json_type(w.metadata,'$.motion_catalog')='object' AND w.item_id=json_extract(w.metadata,'$.motion_catalog.item_id') AND a.profile={} AND {predicate} ORDER BY w.item_id",quote(identity["profile"].as_str().ok_or_else(||anyhow::anyhow!("missing native profile"))?));
        let name=format!("{:x}",Sha256::digest(format!("motion-library:{library}").as_bytes()));
        let saved=host.prepare_selection(&name,&serde_json::json!({"version":1,"mode":"selection","selection":{"language":"sql","query":query}}))?;
        Ok((identity,saved,tx))
    }).await.map_err(unavailable)?;
    let raw = serde_json::to_string(&mappings).map_err(Problem::internal)?;
    let digest = format!("{:x}", Sha256::digest(raw.as_bytes()));
    let inserted_scope = sqlx::query("INSERT INTO catabolic_server_scopes(profile,library_id,library_revision,catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest,created_at) VALUES(?,?,1,?,?,?,?,?,?,?,?) ON CONFLICT DO NOTHING")
        .bind(runtime.profile()).bind(&id).bind(identity["catalog_id"].as_str().unwrap()).bind(identity["catalog_incarnation"].as_str().unwrap()).bind(&digest).bind(&raw).bind(saved["id"].as_str().unwrap()).bind(saved["revision"].as_i64().unwrap()).bind(saved["digest"].as_str().unwrap()).bind(crate::now()).execute(&mut *tx).await?.rows_affected();
    let acknowledged: (String, String, String, String, i64, String) =
        sqlx::query_as("SELECT catalog_id,incarnation,source_mappings_json,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=1 AND source_digest=?")
            .bind(runtime.profile()).bind(&id).bind(&digest).fetch_one(&mut *tx).await?;
    if acknowledged
        != (
            identity["catalog_id"].as_str().unwrap().into(),
            identity["catalog_incarnation"].as_str().unwrap().into(),
            raw,
            saved["id"].as_str().unwrap().into(),
            saved["revision"].as_i64().unwrap(),
            saved["digest"].as_str().unwrap().into(),
        )
    {
        return Err(unavailable("native library scope acknowledgment changed"));
    }
    let value = serde_json::json!({"id":id,"revision":"1","name":definition["name"],"kind":definition["kind"],"language":definition["language"],"source_ids":definition["source_ids"],"availability":"unknown"});
    Scope {
        principal: &principal.id,
        operation: "createLibrary",
        target: "-",
        key: &key,
    }
    .save(
        &mut tx,
        &body.digest,
        StatusCode::CREATED,
        Some(&value),
        None,
    )
    .await?;
    if inserted_scope != 0 {
        sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('libraries',?)")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(tagged(StatusCode::CREATED, value))
}

fn roots_overlap(left: &str, right: &str) -> bool {
    std::path::Path::new(left).starts_with(right) || std::path::Path::new(right).starts_with(left)
}

#[cfg(test)]
mod private_root_tests {
    #[test]
    fn overlap_is_symmetric_and_component_anchored() {
        for (public, private) in [
            ("/media", "/media/generated"),
            ("/media/generated/sub", "/media/generated"),
            ("/media/generated", "/media/generated"),
        ] {
            assert!(super::roots_overlap(public, private));
            assert!(super::roots_overlap(private, public));
        }
        assert!(!super::roots_overlap(
            "/media/generated-other",
            "/media/generated"
        ));
    }
}

pub(super) async fn replace_library(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
    body: Body<LibraryInput>,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let runtime = app.native_catalog.as_ref().unwrap().clone();
    runtime
        .reconcile_library_updates()
        .await
        .map_err(unavailable)?;
    let initial = access::catalog_scope(&caller.principal);
    if !initial.library(&id) {
        return Err(Problem::not_found());
    }
    let grant = runtime
        .prepare_library_definitions(scope_ids(&initial))
        .await
        .map_err(unavailable)?;
    let _serial = app.jobs.lock().await;
    let mut tx = app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    if access::catalog_scope(&principal) != initial {
        return Err(unavailable("library authority changed"));
    }
    let operation = format!(
        "library-replace:{:x}",
        Sha256::digest(format!("{}\0{id}\0{expected}\0{}", principal.id, body.digest).as_bytes())
    );
    let saved: Option<String> =
        sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
            .bind(&operation)
            .fetch_optional(&mut *tx)
            .await?;
    let rows = runtime
        .read_libraries(&mut tx, vec![grant.clone()], None)
        .await
        .map_err(unavailable)?;
    let current = rows
        .iter()
        .find(|row| row[0] == format!("motion.library:{id}"))
        .ok_or_else(Problem::not_found)?;
    let value = library_value(current)?;
    let revision = value["revision"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .map_err(Problem::internal)?;
    let registered: std::collections::BTreeSet<String> = descriptors(&mut tx)
        .await?
        .into_iter()
        .map(|d| d.descriptor.id)
        .collect();
    playscale_core::sources::validate_library(
        &body.value.name,
        &body.value.language,
        &body.value.source_ids,
        &registered,
    )
    .map_err(|_| {
        Problem::invalid(
            "invalid_library",
            "Invalid library definition or source membership",
        )
    })?;
    let allowed: std::collections::BTreeSet<String> = rows
        .iter()
        .map(|row| serde_json::from_str::<Value>(row[2].as_str().unwrap()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(Problem::internal)?
        .into_iter()
        .flat_map(|m| {
            serde_json::from_value::<Vec<String>>(m["motion_library"]["source_ids"].clone())
                .unwrap_or_default()
        })
        .collect();
    if !matches!(initial, access::CatalogScope::All)
        && body.value.source_ids.iter().any(|id| !allowed.contains(id))
    {
        return Err(Problem::not_found());
    }
    let mut mappings = Vec::new();
    for source in &body.value.source_ids {
        let mapping = active_mapping_in(&mut tx, source).await?;
        mappings.push(mapping);
    }
    mappings.sort_by(|a, b| a.library_id.cmp(&b.library_id));
    let command = if let Some(saved) = saved {
        let command =
            motion_catalog_backend::command_attempts::decode(&saved).map_err(unavailable)?;
        let (saved_id, saved_expected, definition, saved_maps) =
            crate::native_catalog::library_update::update_parts(&command).map_err(unavailable)?;
        if saved_id != id
            || saved_expected != expected
            || definition["replacement_operation"] != operation
            || saved_maps != mappings
            || definition["name"] != body.value.name.trim()
            || definition["kind"]
                != serde_json::to_value(&body.value.kind).map_err(Problem::internal)?
            || definition["language"] != body.value.language
            || definition["source_ids"]
                != serde_json::to_value(&body.value.source_ids).map_err(Problem::internal)?
        {
            return Err(unavailable("saved library replacement differs"));
        }
        tx.commit().await?;
        command
    } else {
        access::revise(revision, expected, true)?;
        let mut metadata: Value =
            serde_json::from_str(current[2].as_str().unwrap()).map_err(Problem::internal)?;
        let definition = &mut metadata["motion_library"];
        definition["name"] = serde_json::json!(body.value.name.trim());
        definition["kind"] = serde_json::json!(body.value.kind);
        definition["language"] = serde_json::json!(body.value.language);
        definition["source_ids"] = serde_json::json!(body.value.source_ids);
        definition["catalog_revision"] = serde_json::json!(
            expected
                .checked_add(1)
                .ok_or_else(|| unavailable("library revision overflow"))?
        );
        definition["replacement_operation"] = serde_json::json!(operation);
        let definition = definition.clone();
        tx.commit().await?;
        runtime
            .prepare_library_update(
                operation,
                definition,
                expected,
                body.digest.clone(),
                mappings.clone(),
            )
            .await
            .map_err(unavailable)?
    };
    loop {
        let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
            app.db.begin_with("BEGIN IMMEDIATE").await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::SourcesManage))
            .await?;
        if access::catalog_scope(&principal) != initial {
            return Err(unavailable("library authority changed before replacement"));
        }
        // Current visible definitions determine source authorization, never the
        // captured principal or the request's source IDs by themselves.
        let rows = runtime
            .read_libraries(&mut tx, vec![grant.clone()], None)
            .await
            .map_err(unavailable)?;
        let mut allowed = std::collections::BTreeSet::new();
        for row in rows {
            let m: Value = serde_json::from_str(
                row[2]
                    .as_str()
                    .ok_or_else(|| unavailable("library metadata missing"))?,
            )
            .map_err(Problem::internal)?;
            let sources: Vec<String> =
                serde_json::from_value(m["motion_library"]["source_ids"].clone())
                    .map_err(Problem::internal)?;
            allowed.extend(sources);
        }
        if !matches!(initial, access::CatalogScope::All)
            && mappings.iter().any(|m| !allowed.contains(&m.library_id))
        {
            return Err(Problem::not_found());
        }
        for mapping in &mappings {
            require_current_mapping_in(&mut tx, mapping).await?;
        }
        let (_, sent) = motion_catalog_backend::command_attempts::latest_in(&mut tx, &command)
            .await
            .map_err(unavailable)?;
        let original = command.clone();
        let library = id.clone();
        let handle = tokio::runtime::Handle::current();
        let (retry, mismatch, tx) = runtime
            .with_host(move |host| {
                if let Some(outcome) = host.reconcile_command_receipt(&sent)? {
                    handle.block_on(
                        crate::native_catalog::Runtime::acknowledge_library_update_in(
                            host, &mut tx, &original, &outcome,
                        ),
                    )?;
                    return Ok((false, false, tx));
                }
                let (_, current) = handle.block_on(
                    crate::native_catalog::Runtime::library_head_in(host, &mut tx, &library),
                )?;
                if current != expected {
                    return Ok((false, true, tx));
                }
                let (outcome, witness) =
                    crate::native_catalog::Runtime::execute_library_update_with_host(host, &sent)?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        &mut tx, &original, &sent, witness,
                    ))?;
                    return Ok((true, false, tx));
                }
                handle.block_on(
                    crate::native_catalog::Runtime::acknowledge_library_update_in(
                        host, &mut tx, &original, &outcome,
                    ),
                )?;
                Ok((false, false, tx))
            })
            .await
            .map_err(unavailable)?;
        tx.commit().await?;
        if mismatch {
            return Err(Problem::new(
                StatusCode::PRECONDITION_FAILED,
                "precondition_failed",
                "The resource revision has changed",
            ));
        }
        if retry {
            continue;
        }
        runtime.rebuild_projection().await.map_err(unavailable)?;
        let (_, _, definition, _) =
            crate::native_catalog::library_update::update_parts(&command).map_err(unavailable)?;
        return Ok(tagged(
            StatusCode::OK,
            serde_json::json!({"id":id,"revision":definition["catalog_revision"].as_u64().unwrap().to_string(),"name":definition["name"],"kind":definition["kind"],"language":definition["language"],"source_ids":definition["source_ids"],"availability":"unknown"}),
        ));
    }
}

pub(super) async fn delete(
    app: App,
    caller: Caller,
    id: String,
    headers: HeaderMap,
) -> Result<Response, Problem> {
    caller.require(Permission::SourcesManage)?;
    let expected = if_match(&headers)?;
    let runtime = app.native_catalog.as_ref().unwrap().clone();
    // A complete definition selection is used only to establish the blocker
    // count, never to grant this caller membership in another library.
    let grant = runtime
        .prepare_library_definitions(None)
        .await
        .map_err(unavailable)?;
    let _serial = app.jobs.lock().await;
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::SourcesManage))
        .await?;
    let registered: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM catabolic_source_mappings WHERE library_id=?)",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await?;
    if !registered {
        return Err(Problem::not_found());
    }
    let current = motion_catalog_backend::source_lifecycle::current_in(&mut tx, &id)
        .await
        .map_err(unavailable)?;
    let operation = format!(
        "source-withdraw:{:x}",
        Sha256::digest(format!("{}\0{id}\0{expected}", principal.id).as_bytes())
    );
    let replay: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM catabolic_source_lifecycle WHERE operation_id=?)",
    )
    .bind(&operation)
    .fetch_one(&mut *tx)
    .await?;
    if !replay && current.state == playscale_core::source_lifecycle::State::Tombstoned {
        return Err(Problem::not_found());
    }
    if !replay {
        access::revise(
            u64::try_from(current.source_revision).map_err(unavailable)?,
            expected,
            true,
        )?;
    }
    let definitions = runtime
        .read_libraries(&mut tx, vec![grant], None)
        .await
        .map_err(unavailable)?;
    let mut memberships = 0u64;
    for row in definitions {
        let metadata: Value = serde_json::from_str(
            row[2]
                .as_str()
                .ok_or_else(|| unavailable("library definition missing"))?,
        )
        .map_err(unavailable)?;
        let sources: Vec<String> =
            serde_json::from_value(metadata["motion_library"]["source_ids"].clone())
                .map_err(unavailable)?;
        if sources.contains(&id) {
            memberships += 1;
        }
    }
    let handle = tokio::runtime::Handle::current();
    let source_id = id.clone();
    let (lifecycle, mut tx) = runtime
        .with_host(move |host| {
            handle.block_on(async {
                let lifecycle = motion_catalog_backend::source_lifecycle::withdraw_in(
                    &mut tx,
                    host,
                    &source_id,
                    i64::try_from(expected)
                        .map_err(|_| anyhow::anyhow!("source revision exceeds range"))?,
                    &operation,
                    memberships,
                    crate::now(),
                )
                .await?;
                Ok((lifecycle, tx))
            })
        })
        .await
        .map_err(|error| {
            if error
                .to_string()
                .contains("source withdrawal refused: Busy")
            {
                Problem::new(
                    StatusCode::CONFLICT,
                    "source_busy",
                    "Source is still referenced or has outstanding work",
                )
            } else {
                unavailable(error)
            }
        })?;
    let body = if lifecycle.state == playscale_core::source_lifecycle::State::Tombstoned {
        None
    } else {
        let mut wire = descriptors(&mut tx)
            .await?
            .into_iter()
            .find(|row| row.descriptor.id == id)
            .ok_or_else(Problem::not_found)?
            .wire;
        wire["revision"] = Value::String(lifecycle.source_revision.to_string());
        wire["binding_revision"] = Value::String(lifecycle.binding_revision.to_string());
        wire["availability"] = Value::String("unavailable".into());
        Some(wire)
    };
    if !replay {
        let kind = if lifecycle.state == playscale_core::source_lifecycle::State::Tombstoned {
            "deleted"
        } else {
            "changed"
        };
        sqlx::query("INSERT INTO change_events(topic,resource_id,kind) VALUES('sources',?,?)")
            .bind(&id)
            .bind(kind)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    // This is a deployment/access withdrawal, not native missing-file evidence.
    // Read paths consult lifecycle directly even before disposable cache rebuild.
    if let Some(body) = body {
        Ok(tagged(StatusCode::OK, body))
    } else {
        Ok(StatusCode::NO_CONTENT.into_response())
    }
}
