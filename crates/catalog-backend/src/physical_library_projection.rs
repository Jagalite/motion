//! Disposable membership from current native definitions and independently
//! retained source grants. Neither legacy library joins nor item metadata alone
//! confer visibility. The enclosing Motion writer and native lease stay held.
use crate::{occurrence_bridge::OccurrenceMapping, sources::SourceMapping};
use anyhow::{Context, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn digest(value: &Value) -> anyhow::Result<String> {
    Ok(catabolic_core::sha256(serde_json::to_vec(value)?))
}
fn text<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("native library proof lacks {field}"))
}
fn definition(item: &Value) -> anyhow::Result<Option<(String, i64, BTreeSet<String>)>> {
    let model = &item["metadata"]["motion_library"];
    if model.is_null() || !item["retirement"].is_null() {
        return Ok(None);
    }
    let id = text(model, "id")?.to_owned();
    ensure!(
        item["kind"] == "custom:motion_library"
            && item["item_id"] == format!("motion.library:{id}")
            && model["contract_version"] == 1
            && item["item_revision"].as_i64().is_some_and(|n| n > 0),
        "native library owner changed"
    );
    let revision = model["catalog_revision"]
        .as_i64()
        .filter(|n| *n >= 0)
        .context("invalid native library revision")?;
    let sources: Vec<String> = serde_json::from_value(model["source_ids"].clone())?;
    let unique: BTreeSet<_> = sources.iter().cloned().collect();
    ensure!(
        sources.len() <= 100 && unique.len() == sources.len(),
        "native library source membership is ambiguous"
    );
    Ok(Some((id, revision, unique)))
}

fn verify_source(
    db: &Connection,
    host: &catabolic_runtime::host::HostSession,
    mapping: &SourceMapping,
    imported: &BTreeMap<String, crate::imported_sources::ImportedSourceDescriptor>,
    require_root: bool,
) -> anyhow::Result<()> {
    ensure!(
        mapping.library_id != crate::generated_source::SOURCE_ID,
        "generated scope requires separate lineage owner"
    );
    let raw: String = db.query_row(
        "SELECT mapping_json FROM catabolic_source_mappings WHERE library_id=?",
        [&mapping.library_id],
        |r| r.get(0),
    )?;
    ensure!(
        serde_json::from_str::<SourceMapping>(&raw)? == *mapping,
        "scope source mapping changed"
    );
    let native: Option<String> = db
        .query_row(
            "SELECT descriptor_json FROM catabolic_source_registry WHERE id=?",
            [&mapping.library_id],
            |r| r.get(0),
        )
        .optional()?;
    let imported = imported.get(&mapping.library_id);
    ensure!(
        native.is_some() != imported.is_some(),
        "scope source has no unique registry owner"
    );
    let cached:String=db.query_row("SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?",[&mapping.library_id],|r|r.get(0))?;
    let cached: Value = serde_json::from_str(&cached)?;
    if let Some(record) = imported {
        ensure!(
            record.proof.mapping == *mapping,
            "imported source is not the exact owner"
        );
        crate::imported_sources::validate_cached_row(record, &cached)?;
    } else {
        let descriptor: crate::source_registry::NativeSourceDescriptor =
            serde_json::from_str(native.as_deref().unwrap())?;
        ensure!(
            descriptor.id == mapping.library_id
                && descriptor.root == mapping.motion_root
                && descriptor.root_identity == mapping.motion_root_identity
                && descriptor.exclusions == mapping.exclusions,
            "native source registry changed"
        );
        let expected = json!({"id":descriptor.id,"name":descriptor.name,"root":descriptor.root,"root_identity":descriptor.root_identity,
            "managed":0,"enabled":1,"revision":0,"binding_revision":mapping.motion_binding_revision,
            "exclusions_json":serde_json::to_string(&descriptor.exclusions)?,"scan_barrier":0});
        ensure!(
            cached == expected,
            "native source descriptor cache differs from registry owner"
        );
    }
    ensure!(
        host.source_binding(&mapping.native_source)? == mapping.native_binding
            && host.source_observation_policy(&mapping.native_source)? == mapping.native_policy,
        "native source binding or policy changed"
    );
    if require_root {
        crate::sources::check_root(&mapping.native_binding)?;
    }
    Ok(())
}

pub(crate) fn rebuild(
    db: &Connection,
    host: &catabolic_runtime::host::HostSession,
    generation: i64,
    items: &[Value],
) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "native membership projection requires writer custody"
    );
    let identity = host.identity()?;
    ensure!(
        host.catalog_generation()? == generation,
        "native membership generation changed"
    );
    db.execute("DELETE FROM catabolic_projection_file_libraries", [])?;
    db.execute("DELETE FROM catabolic_projection_generated_libraries", [])?;
    let imported: BTreeMap<_, _> = crate::imported_sources::imported_sources_sync(db)?
        .into_iter()
        .map(|r| (r.descriptor.id.clone(), r))
        .collect();
    // Bound input before materializing JSON; writer custody keeps both reads
    // within one immutable Motion snapshot.
    let (file_count, input_bytes): (i64, i64) = db.query_row(
        "SELECT count(*),coalesce(sum(length(CAST(mapping_json AS BLOB))),0) FROM (SELECT mapping_json FROM catabolic_projection_files WHERE generation=? AND available=1 LIMIT 10001)",
        [generation], |r| Ok((r.get(0)?,r.get(1)?)),
    )?;
    ensure!(
        file_count <= 10_000 && input_bytes <= 32 * 1024 * 1024,
        "native physical membership input exceeds bounded row/byte budget"
    );
    let files:Vec<(String,String)>=db.prepare("SELECT file_id,mapping_json FROM catabolic_projection_files WHERE generation=? AND available=1 ORDER BY file_id")?
        .query_map([generation],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
    let mut files_by_source = BTreeMap::<
        String,
        Vec<(
            String,
            OccurrenceMapping,
            Option<crate::generated_library_projection::Ancestry>,
        )>,
    >::new();
    let mut remaining_generated_bytes = 32 * 1024 * 1024;
    for (id, raw) in files {
        let mapping: OccurrenceMapping = serde_json::from_str(&raw)?;
        ensure!(
            mapping.motion_file["id"] == id
                && mapping.physical.profile == text(&identity, "profile")?,
            "projected occurrence identity changed"
        );
        let ancestry = if mapping.motion_file["generated"] == 1 {
            Some(crate::generated_library_projection::ancestry(
                db,
                &mapping,
                &mut remaining_generated_bytes,
            )?)
        } else {
            None
        };
        let source_id = ancestry
            .as_ref()
            .map(|a| a.input.occurrence.source.library_id.clone())
            .unwrap_or_else(|| mapping.source.library_id.clone());
        files_by_source
            .entry(source_id)
            .or_default()
            .push((id, mapping, ancestry));
    }
    let mut checked = BTreeMap::<String, SourceMapping>::new();
    let mut seen = BTreeSet::new();
    let mut total_bytes = 0usize;
    for item in items {
        let Some((id, revision, expected)) = definition(item)? else {
            continue;
        };
        ensure!(
            seen.insert(id.clone()) && seen.len() <= 10_000,
            "native library inventory exceeds bound"
        );
        type Ack = (String, String, String, String, String, i64, String);
        let rows:Vec<Ack>=db.prepare("SELECT catalog_id,incarnation,source_digest,source_mappings_json,query_id,query_revision,query_digest FROM catabolic_server_scopes WHERE profile=? AND library_id=? AND library_revision=? LIMIT 2")?
            .query_map(params![text(&identity,"profile")?,id,revision],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?.collect::<Result<_,_>>()?;
        ensure!(
            rows.len() == 1,
            "native library has no unique current scope acknowledgment"
        );
        let (catalog, incarnation, source_digest, raw, query, query_revision, query_digest) =
            rows.into_iter().next().unwrap();
        ensure!(
            identity["catalog_id"] == catalog
                && identity["catalog_incarnation"] == incarnation
                && catabolic_core::sha256(raw.as_bytes()) == source_digest,
            "native scope acknowledgment identity changed"
        );
        ensure!(
            raw.len() <= 4 * 1024 * 1024,
            "native scope source proof exceeds bound"
        );
        let mappings: Vec<SourceMapping> = serde_json::from_str(&raw)?;
        ensure!(
            mappings.len() == expected.len()
                && mappings
                    .iter()
                    .map(|m| m.library_id.clone())
                    .collect::<BTreeSet<_>>()
                    == expected,
            "current native membership differs from saved source grant"
        );
        let reference = catabolic_store::catalog_scope::SavedQueryRef::new(
            query.clone(),
            query_revision,
            query_digest.clone(),
        )
        .map_err(|e| anyhow::anyhow!("invalid native library query: {e:?}"))?;
        host.validate_selection_reference(&reference)?;
        for source in mappings {
            ensure!(
                source.catalog.catalog_id == catalog
                    && source.catalog.incarnation == incarnation
                    && source.profile == text(&identity, "profile")?,
                "scope source belongs to another native owner"
            );
            // A source without current files grants no physical visibility and
            // does not require materializing an otherwise unused FK cache row.
            let Some(files) = files_by_source.get(&source.library_id) else {
                continue;
            };
            let lifecycle = crate::source_lifecycle::current_sync(db, &source.library_id)?;
            if lifecycle.state != playscale_core::source_lifecycle::State::Active {
                continue;
            }
            ensure!(
                lifecycle.mapping_digest == digest(&serde_json::to_value(&source)?)?,
                "scope lifecycle mapping changed"
            );
            if let Some(prior) = checked.get(&source.library_id) {
                ensure!(prior == &source, "conflicting source scope grants");
            } else {
                verify_source(
                    db,
                    host,
                    &source,
                    &imported,
                    files.iter().any(|(_, _, a)| a.is_none()),
                )?;
                checked.insert(source.library_id.clone(), source.clone());
            }
            for (file, mapping, ancestry) in files {
                ensure!(
                    ancestry
                        .as_ref()
                        .map(|a| &a.input.occurrence.source)
                        .unwrap_or(&mapping.source)
                        == &source,
                    "scope source and current occurrence differ"
                );
                let mut proof = json!({"contract_version":1,"catalog_id":catalog,"incarnation":incarnation,"generation":generation,
                    "file_id":file,"library_id":id,"library_native_id":item["item_id"],"library_native_revision":item["item_revision"],
                    "library_revision":revision,"definition_digest":digest(&item["metadata"]["motion_library"])?,
                    "scope":{"query_id":query,"query_revision":query_revision,"query_digest":query_digest,"source_digest":source_digest},
                    "source_id":source.library_id,"native_source":source.native_source,"source_mapping_digest":digest(&serde_json::to_value(&source)?)?,
                    "occurrence_mapping_digest":digest(&serde_json::to_value(mapping)?)?});
                let table = if let Some(ancestry) = ancestry {
                    let object = proof.as_object_mut().context("membership proof shape")?;
                    object.remove("source_id");
                    object.remove("native_source");
                    object.remove("source_mapping_digest");
                    object.remove("occurrence_mapping_digest");
                    object.insert("contract_version".into(), json!(2));
                    object.insert("kind".into(), json!("generated"));
                    object.insert("input_source_id".into(), json!(source.library_id));
                    object.insert("input_native_source".into(), json!(source.native_source));
                    object.insert(
                        "input_source_mapping_digest".into(),
                        json!(ancestry.input.source_mapping_digest),
                    );
                    object.insert(
                        "input_occurrence_mapping_digest".into(),
                        json!(ancestry.input.occurrence_mapping_digest),
                    );
                    object.insert(
                        "output_occurrence_mapping_digest".into(),
                        json!(digest(&serde_json::to_value(mapping)?)?),
                    );
                    object.insert("publication".into(), ancestry.publication.clone());
                    object.insert("lifecycle".into(), serde_json::to_value(&lifecycle)?);
                    "catabolic_projection_generated_libraries"
                } else {
                    "catabolic_projection_file_libraries"
                };
                let raw = serde_json::to_string(&proof)?;
                total_bytes = total_bytes
                    .checked_add(raw.len())
                    .context("membership projection byte overflow")?;
                ensure!(
                    total_bytes <= 32 * 1024 * 1024,
                    "membership projection exceeds bounded proof budget"
                );
                db.execute(&format!("INSERT INTO {table}(file_id,library_id,generation,proof_json) VALUES(?,?,?,?)"),params![file,id,generation,raw])?;
            }
        }
    }
    ensure!(
        host.catalog_generation()? == generation,
        "native membership generation changed before commit"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definitions_require_current_exact_native_owners_and_unique_sources() {
        let item = json!({"kind":"custom:motion_library","item_id":"motion.library:lib","item_revision":2,"retirement":null,
            "metadata":{"motion_library":{"contract_version":1,"id":"lib","catalog_revision":3,"source_ids":["source"]}}});
        assert_eq!(
            definition(&item).unwrap().unwrap(),
            ("lib".into(), 3, ["source".into()].into())
        );
        let mut bad = item.clone();
        bad["metadata"]["motion_library"]["source_ids"] = json!(["source", "source"]);
        assert!(definition(&bad).is_err());
        let mut bad = item.clone();
        bad["item_id"] = json!("motion.library:other");
        assert!(definition(&bad).is_err());
        let mut retired = item;
        retired["retirement"] = json!({"kind":"alias"});
        assert!(definition(&retired).unwrap().is_none());
    }
    #[tokio::test]
    async fn ordinary_connections_cannot_mint_membership_rows() {
        use sqlx::Connection;
        let mut db = sqlx::SqliteConnection::connect("sqlite::memory:")
            .await
            .unwrap();
        crate::projection::configure_connection(&mut db)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0090_catabolic_physical_library_projection.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        let result=sqlx::query("INSERT INTO catabolic_projection_file_libraries VALUES('file','library',1,'{\"contract_version\":1}')").execute(&mut db).await;
        assert!(
            result.is_err(),
            "ordinary configured connection must not mint scope authority"
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM catabolic_projection_file_libraries")
                .fetch_one(&mut db)
                .await
                .unwrap();
        assert_eq!(count, 0);
        sqlx::raw_sql(include_str!(
            "../../../migrations/0091_catabolic_source_lifecycle.sql"
        ))
        .execute(&mut db)
        .await
        .unwrap();
        let result = sqlx::query("INSERT INTO catabolic_projection_generated_libraries VALUES('file','library',1,'{\"contract_version\":2,\"kind\":\"generated\"}')")
            .execute(&mut db).await;
        assert!(
            result.is_err(),
            "ordinary connection cannot mint generated scope"
        );
        let result = sqlx::query("INSERT INTO catabolic_source_lifecycle VALUES('source',0,'catalog','incarnation',?,1,1,'active','operation','{}',1)")
            .bind("a".repeat(64)).execute(&mut db).await;
        assert!(
            result.is_err(),
            "ordinary connection cannot mint lifecycle baseline"
        );
    }
}
