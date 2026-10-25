//! Native attributed metadata adapter. Retained Motion catalog documents are
//! never a fallback; application authorization and native facts are both current.
use super::super::catalog::{native_read_error, native_unavailable};
use super::*;

pub(super) async fn get(app: &App, caller: &Caller, id: &str) -> Result<Response, Problem> {
    let tx = scoped_transaction(app, caller, id, Permission::CatalogRead).await?;
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let (snapshot, revision) = runtime
        .metadata_and_commit(tx, id.into())
        .await
        .map_err(native_read_error)?;
    let body = body(snapshot, revision)?;
    Ok(tagged(StatusCode::OK, &body, revision))
}

pub(super) async fn scoped_transaction(
    app: &App,
    caller: &Caller,
    id: &str,
    permission: Permission,
) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, Problem> {
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let mut authorization = app.db.begin().await?;
    let initial = caller
        .reauthorize(&mut authorization, Some(permission))
        .await?;
    let prepared = core::catalog_scope(&initial);
    if prepared == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    authorization.commit().await?;
    let scoped = match &prepared {
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
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller.reauthorize(&mut tx, Some(permission)).await?;
    let current = core::catalog_scope(&principal);
    if current == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    if current != prepared {
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
    let rows = runtime
        .read_projectable_items(&mut tx, vec![selection], Some(id.into()))
        .await
        .map_err(native_read_error)?;
    if rows.len() != 1 {
        return Err(Problem::not_found());
    }
    Ok(tx)
}

fn public_field(field: &str) -> bool {
    !field.starts_with("motion_")
}

fn body(
    snapshot: catabolic_store::catalog_metadata_store::ContributionSnapshot,
    revision: u64,
) -> Result<MetadataBody, Problem> {
    use playscale_core::metadata::Contribution;
    const OWNER_FIELDS: &[&str] = &[
        "motion_catalog",
        "motion_artwork",
        "motion_library",
        "motion_edition",
        "motion_version",
        "motion_probe",
        "motion_discovery",
        "motion_historical_version",
        "motion_historical_file",
        "motion_historical_timeline",
        "motion_source_history",
        "motion_reference_tombstone",
    ];
    if snapshot
        .sources
        .iter()
        .flat_map(|source| source.fields.keys())
        .any(|field| field.starts_with("motion_") && !OWNER_FIELDS.contains(&field.as_str()))
        || snapshot.metadata.as_object().is_some_and(|fields| {
            fields.keys().any(|field| {
                field.starts_with("motion_") && !OWNER_FIELDS.contains(&field.as_str())
            })
        })
    {
        return Err(native_unavailable());
    }
    let mut documents = BTreeMap::new();
    let mut contributions = Vec::new();
    for source in snapshot.sources {
        if source.namespace == catabolic_store::catalog_metadata_store::LEGACY_NAMESPACE
            && source.fields.keys().all(|field| !public_field(field))
        {
            continue;
        }
        let name = if source.namespace == "motion.scan" && source.record_id == "origin" {
            "scan"
        } else if source.record_id == "document" {
            let Some(name) = source.namespace.strip_prefix("motion.") else {
                return Err(native_unavailable());
            };
            if !metadata_service::valid_source(name) {
                return Err(native_unavailable());
            }
            name
        } else if source.namespace == catabolic_store::catalog_metadata_store::LEGACY_NAMESPACE {
            // Native initialization retains existing native item values. Only
            // their public fields are the baseline, not a legacy Motion query.
            "scan"
        } else if source.record_id == "source_projection"
            && matches!(
                source.namespace.as_str(),
                "motion.structure" | "motion.artwork"
            )
        {
            continue;
        } else {
            return Err(native_unavailable());
        };
        let values = source
            .fields
            .into_iter()
            .filter(|(key, _)| public_field(key))
            .collect();
        let excluded_tags = if name == "local" {
            snapshot.excluded_tags.clone()
        } else {
            vec![]
        };
        let document = Contribution {
            values,
            tags: source.tags.iter().cloned().collect(),
            excluded_tags: excluded_tags.iter().cloned().collect(),
        };
        if documents
            .insert(name.to_owned(), document.clone())
            .is_some()
        {
            return Err(native_unavailable());
        }
        if name != "scan" {
            contributions.push(ContributionBody {
                source: name.into(),
                revision: source.revision.to_string(),
                values: document.values,
                tags: source.tags,
                excluded_tags,
                locked_fields: vec![],
            });
        }
    }
    if snapshot.schema_fingerprint.is_none() {
        let title = snapshot
            .metadata
            .get("title")
            .cloned()
            .ok_or_else(native_unavailable)?;
        documents.insert(
            "scan".into(),
            Contribution {
                values: BTreeMap::from([("title".into(), title)]),
                ..Default::default()
            },
        );
    }
    // Do not attribute a global native suppression to a fictional local source.
    if !snapshot.excluded_tags.is_empty() && !documents.contains_key("local") {
        return Err(native_unavailable());
    }
    contributions.sort_by(|a, b| a.source.cmp(&b.source));
    let resolved = playscale_core::metadata::resolve(&documents);
    Ok(MetadataBody {
        item_id: snapshot.subject,
        revision: revision.to_string(),
        values: resolved.values,
        field_sources: resolved.field_sources,
        contributions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use catabolic_store::catalog_metadata_store::{ContributionSnapshot, SourceSnapshot};
    fn source(namespace: &str, record: &str, values: Value) -> SourceSnapshot {
        SourceSnapshot {
            namespace: namespace.into(),
            record_id: record.into(),
            revision: 3,
            fields: serde_json::from_value(values).unwrap(),
            tags: vec![],
        }
    }
    fn snapshot(sources: Vec<SourceSnapshot>) -> ContributionSnapshot {
        ContributionSnapshot {
            subject: "work".into(),
            generation: 17,
            schema_fingerprint: Some("fingerprint".into()),
            metadata: serde_json::json!({}),
            sources,
            excluded_tags: vec![],
        }
    }
    #[test]
    fn metadata_native_sources_preserve_empty_contributions_and_hide_internal_fields() {
        let view = body(
            snapshot(vec![
                source(
                    "motion.scan",
                    "origin",
                    serde_json::json!({"title":"Scanned"}),
                ),
                source("motion.tmdb", "document", serde_json::json!({})),
                source(
                    "motion.local",
                    "document",
                    serde_json::json!({"title":"Curated", "motion_catalog":{"private":true}}),
                ),
                source(
                    "motion.structure",
                    "source_projection",
                    serde_json::json!({"motion_catalog":{}}),
                ),
            ]),
            41,
        )
        .unwrap();
        assert_eq!(view.revision, "41");
        assert_eq!(
            view.values,
            BTreeMap::from([("title".into(), Value::String("Curated".into()))])
        );
        assert_eq!(view.field_sources["title"], vec!["local"]);
        assert_eq!(view.contributions.len(), 2);
        assert_eq!(view.contributions[1].source, "tmdb");
        assert!(view.contributions[1].values.is_empty());
        assert_eq!(view.contributions[0].revision, "3");
    }
    #[test]
    fn metadata_native_unknown_reserved_fields_are_not_silently_hidden() {
        assert!(
            body(
                snapshot(vec![source(
                    "motion.local",
                    "document",
                    serde_json::json!({"title":"value", "motion_custom":"unproven"})
                )]),
                1
            )
            .is_err()
        );
    }
    #[test]
    fn metadata_native_source_tags_and_local_exclusions_remain_attributed() {
        let mut local = source("motion.local", "document", serde_json::json!({}));
        local.tags = vec!["kept".into()];
        let mut snap = snapshot(vec![local]);
        snap.excluded_tags = vec!["excluded".into()];
        let view = body(snap, 1).unwrap();
        assert_eq!(view.contributions[0].tags, vec!["kept"]);
        assert_eq!(view.contributions[0].excluded_tags, vec!["excluded"]);
    }
    #[test]
    fn metadata_native_snapshot_does_not_fabricate_local_suppression_source() {
        let mut snap = snapshot(vec![]);
        snap.excluded_tags = vec!["excluded".into()];
        assert!(body(snap, 1).is_err());
    }
    #[test]
    fn metadata_native_unmanaged_item_uses_only_native_title() {
        let mut snap = snapshot(vec![]);
        snap.schema_fingerprint = None;
        snap.metadata = serde_json::json!({"title":"Fresh", "motion_catalog": {"private":true}});
        let view = body(snap, 1).unwrap();
        assert_eq!(view.values.len(), 1);
        assert_eq!(view.field_sources["title"], vec!["scan"]);
        assert!(view.contributions.is_empty());
    }
}

/// Native39 fixed-schema cohort or native40 atomic schema/source/tag owner.
/// Unsupported normalization domains are refused before any durable intent.
pub(super) async fn replace(
    app: &App,
    caller: &Caller,
    id: &str,
    source: &str,
    expected: u64,
    contribution: playscale_core::metadata::Contribution,
    digest: &str,
) -> Result<Response, Problem> {
    use crate::native_catalog::Guarded;
    use catabolic_store::catalog_commands::CommandOutcome;
    use sha2::{Digest, Sha256};
    if !valid_id(id) {
        return Err(Problem::not_found());
    }
    if contribution
        .values
        .keys()
        .any(|key| !public_field(key) || key.starts_with("motion_"))
    {
        return Err(Problem::invalid(
            "reserved_metadata_field",
            "Native projection fields cannot be supplied as contributions",
        ));
    }
    for tag in contribution.tags.iter().chain(&contribution.excluded_tags) {
        if catabolic_store::tagging::tag_name(&serde_json::json!(tag))
            .ok()
            .as_ref()
            != Some(tag)
        {
            return Err(Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "native_tag_normalization_unavailable",
                "This tag identity cannot yet be represented losslessly by the native owner",
            ));
        }
    }
    let runtime = app.native_catalog.as_ref().ok_or_else(native_unavailable)?;
    let _jobs = app.jobs.lock().await;
    let mut authorization = app.db.begin().await?;
    let initial = caller
        .reauthorize(&mut authorization, Some(Permission::CatalogWrite))
        .await?;
    let prepared = core::catalog_scope(&initial);
    if prepared == CatalogScope::Nothing {
        return Err(Problem::not_found());
    }
    authorization.commit().await?;
    let scoped = match &prepared {
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
    let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
        app.db.begin_with("BEGIN IMMEDIATE").await?;
    let principal = caller
        .reauthorize(&mut tx, Some(Permission::CatalogWrite))
        .await?;
    if core::catalog_scope(&principal) != prepared {
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
    let rows = runtime
        .read_projectable_items(&mut tx, vec![selection.clone()], Some(id.into()))
        .await
        .map_err(native_read_error)?;
    if rows.len() != 1 {
        return Err(Problem::not_found());
    }
    let check_scope = prepared.clone();
    let check_subject = id.to_owned();
    let handle = tokio::runtime::Handle::current();
    let (allowed, returned) = runtime
        .with_host(move |host| {
            let allowed = handle.block_on(
                motion_catalog_backend::aggregate_revision::may_curate_work_in(
                    &mut tx,
                    host,
                    &check_subject,
                    &check_scope,
                ),
            )?;
            Ok((allowed, tx))
        })
        .await
        .map_err(native_read_error)?;
    tx = returned;
    if !allowed {
        return Err(outside_curation());
    }
    let operation = format!(
        "server-replace-contribution:{:x}",
        Sha256::digest(
            serde_json::to_vec(&(principal.id.as_str(), id, source, expected, digest))
                .map_err(Problem::internal)?
        )
    );
    let saved: Option<String> =
        sqlx::query_scalar("SELECT envelope_json FROM catabolic_commands WHERE operation_id=?")
            .bind(&operation)
            .fetch_optional(&mut *tx)
            .await?;
    let recorded = saved
        .as_deref()
        .map(motion_catalog_backend::command_attempts::decode)
        .transpose()
        .map_err(Problem::internal)?;
    if let Some(original) = &recorded {
        let payload = original.payload();
        let change = if payload["operation"] == "metadata_contribution_replace" {
            &payload["request"]
        } else if payload["operation"] == "metadata" {
            &payload["mutation"]["change"]
        } else if payload["operation"] == "metadata_initialize_and_patch" {
            payload
        } else {
            return Err(native_unavailable());
        };
        let evidence: Value = serde_json::from_str(
            change["evidence_json"]
                .as_str()
                .ok_or_else(native_unavailable)?,
        )
        .map_err(Problem::internal)?;
        if original.key().operation_id() != operation
            || change["subject"] != id
            || change["namespace"] != format!("motion.{source}")
            || change["record_id"] != "document"
            || evidence["owner"] != "motion.metadata.replace"
            || evidence["request_digest"] != digest
            || evidence["expected_aggregate_revision"] != expected
        {
            return Err(native_unavailable());
        }
        let (_, sent) = motion_catalog_backend::command_attempts::latest_in(&mut tx, original)
            .await
            .map_err(Problem::internal)?;
        let scope = prepared.clone();
        let subject = id.to_owned();
        let handle = tokio::runtime::Handle::current();
        let (receipt, returned) = runtime
            .with_host(move |host| {
                if !handle.block_on(
                    motion_catalog_backend::aggregate_revision::may_curate_work_in(
                        &mut tx, host, &subject, &scope,
                    ),
                )? {
                    return Err(anyhow::Error::new(OutsideCuration));
                }
                Ok((host.reconcile_command_receipt(&sent)?, tx))
            })
            .await
            .map_err(mutation_error)?;
        tx = returned;
        if let Some(outcome) = receipt {
            if !matches!(outcome, CommandOutcome::Replay { .. }) {
                return Err(native_unavailable());
            }
            tx.commit().await?;
            let mut response = get(app, caller, id).await?;
            response
                .headers_mut()
                .insert("idempotent-replayed", HeaderValue::from_static("true"));
            return Ok(response);
        }
    }
    let subject = id.to_owned();
    let scope = prepared.clone();
    let handle = tokio::runtime::Handle::current();
    let observed = runtime
        .guard_work_and_commit(tx, subject.clone(), expected, move |host, tx, _| {
            if !handle.block_on(
                motion_catalog_backend::aggregate_revision::may_curate_work_in(
                    tx, host, &subject, &scope,
                ),
            )? {
                return Err(anyhow::Error::new(OutsideCuration));
            }
            Ok((
                host.metadata_contributions(&subject)?,
                host.identity()?["schema_version"].as_u64(),
            ))
        })
        .await
        .map_err(mutation_error)?;
    let (snapshot, schema_version) = match observed {
        Guarded::Mismatch(_) => return Err(AccessError::StaleRevision.into()),
        Guarded::Applied { value, .. } => value,
    };
    if source != "local"
        && schema_version != Some(40)
        && (snapshot.schema_fingerprint.is_none()
            || snapshot.sources.iter().any(|record| {
                record.namespace == catabolic_store::catalog_metadata_store::LEGACY_NAMESPACE
                    && record.fields.keys().any(|field| public_field(field))
            }))
    {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "native_origin_provenance_unavailable",
            "Provider replacement requires explicit native scan-origin provenance",
        ));
    }
    let current = body(snapshot, expected)?;
    let old = current.contributions.iter().find(|c| c.source == source);
    let old_tags: BTreeSet<_> = old
        .map(|c| c.tags.iter().cloned().collect())
        .unwrap_or_default();
    let old_excluded: BTreeSet<_> = old
        .map(|c| c.excluded_tags.iter().cloned().collect())
        .unwrap_or_default();
    let tags_changed = contribution.tags != old_tags || contribution.excluded_tags != old_excluded;
    if tags_changed && schema_version != Some(40) {
        return Err(Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "native_contribution_tags_unavailable",
            "Atomic native contribution and tag replacement is not yet qualified",
        ));
    }
    if !tags_changed && old.is_some_and(|c| c.values == contribution.values) {
        return Ok(tagged(StatusCode::OK, &current, expected));
    }
    let command = match recorded {
        Some(command) => command,
        None => runtime
            .prepare_contribution(
                operation,
                id.into(),
                source.into(),
                contribution,
                digest.into(),
                expected,
            )
            .await
            .map_err(native_read_error)?,
    };
    loop {
        let mut tx: sqlx::Transaction<'static, sqlx::Sqlite> =
            app.db.begin_with("BEGIN IMMEDIATE").await?;
        let principal = caller
            .reauthorize(&mut tx, Some(Permission::CatalogWrite))
            .await?;
        if core::catalog_scope(&principal) != prepared {
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
        let rows = runtime
            .read_projectable_items(&mut tx, vec![selection.clone()], Some(id.into()))
            .await
            .map_err(native_read_error)?;
        if rows.len() != 1 {
            return Err(Problem::not_found());
        }
        let original = command.clone();
        let subject = id.to_owned();
        let source = source.to_owned();
        let handle = tokio::runtime::Handle::current();
        let scope = prepared.clone();
        let result = runtime
            .guard_work_and_commit(tx, id.into(), expected, move |host, tx, _| {
                if !handle.block_on(
                    motion_catalog_backend::aggregate_revision::may_curate_work_in(
                        tx, host, &subject, &scope,
                    ),
                )? {
                    return Err(anyhow::Error::new(OutsideCuration));
                }
                let (_, attempt) = handle.block_on(
                    motion_catalog_backend::command_attempts::latest_in(tx, &original),
                )?;
                let (outcome, witness) =
                    crate::native_catalog::Runtime::execute_contribution_with_host(
                        host,
                        &attempt,
                        subject.clone(),
                        source,
                    )?;
                if let Some(witness) = witness {
                    handle.block_on(motion_catalog_backend::command_attempts::record_next_in(
                        tx, &original, &attempt, witness,
                    ))?;
                    return Ok(true);
                }
                anyhow::ensure!(
                    matches!(
                        outcome,
                        CommandOutcome::Committed { .. } | CommandOutcome::Replay { .. }
                    ),
                    "native contribution requires reconciliation: {}",
                    outcome.code()
                );
                handle.block_on(
                    sqlx::query("INSERT INTO change_events(topic,resource_id) VALUES('catalog',?)")
                        .bind(&subject)
                        .execute(&mut **tx),
                )?;
                Ok(false)
            })
            .await
            .map_err(mutation_error)?;
        match result {
            Guarded::Mismatch(_) => return Err(AccessError::StaleRevision.into()),
            Guarded::Applied { value: true, .. } => continue,
            Guarded::Applied { value: false, .. } => break,
        }
    }
    get(app, caller, id).await
}

#[derive(Debug)]
pub(super) struct OutsideCuration;
impl std::fmt::Display for OutsideCuration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("all containing libraries are required for metadata curation")
    }
}
impl std::error::Error for OutsideCuration {}
fn outside_curation() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "outside_scope",
        "Changing shared metadata requires every containing library",
    )
}
pub(super) fn mutation_error(error: anyhow::Error) -> Problem {
    if error.downcast_ref::<OutsideCuration>().is_some() {
        outside_curation()
    } else {
        native_read_error(error)
    }
}
