//! Typed native physical owners projected without changing application FK IDs.
use crate::occurrence_bridge::OccurrenceMapping;
use anyhow::{Context, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn text<'a>(v: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("physical projection missing {key}"))
}
fn lineage_request(
    items: &[Value],
    item: &Value,
    version: &Value,
) -> anyhow::Result<catabolic_store::catalog_projection::VersionLineageRequest> {
    let timeline = text(version, "timeline_id")?;
    let editions: Vec<_> = items
        .iter()
        .filter(|owner| {
            owner["metadata"]["motion_edition"]["source_timelines"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == timeline))
        })
        .collect();
    ensure!(
        editions.len() == 1,
        "version has no unique retained edition owner"
    );
    let edition = editions[0];
    let request = catabolic_store::catalog_projection::VersionLineageRequest {
        work_item_id: text(
            &edition["metadata"]["motion_edition"]["source_edition"],
            "item_id",
        )?
        .into(),
        edition_item_id: text(edition, "item_id")?.into(),
        version_item_id: text(item, "item_id")?.into(),
        version_id: text(version, "id")?.into(),
    };
    Ok(request)
}
fn current_lineage(
    host: &catabolic_runtime::host::HostSession,
    generation: i64,
    items: &[Value],
    item: &Value,
    version: &Value,
) -> anyhow::Result<bool> {
    let request = lineage_request(items, item, version)?;
    let facts = host.projection_version_lineage(generation, std::slice::from_ref(&request))?;
    let identity = host.identity()?;
    ensure!(
        facts["contract_version"] == 1
            && facts["catalog_id"] == identity["catalog_id"]
            && facts["incarnation"] == identity["catalog_incarnation"]
            && facts["generation"].as_i64() == Some(generation),
        "native lineage observation fence changed"
    );
    let rows = facts["rows"]
        .as_array()
        .context("native lineage rows missing")?;
    ensure!(rows.len() == 1, "native lineage response count changed");
    let row = &rows[0];
    ensure!(
        row["work_item_id"] == request.work_item_id
            && row["edition_item_id"] == request.edition_item_id
            && row["version_item_id"] == request.version_item_id
            && row["version_id"] == request.version_id,
        "native lineage response owner changed"
    );
    match row["status"].as_str() {
        Some("active") => Ok(true),
        Some("retired" | "detached" | "ambiguous") => Ok(false),
        _ => anyhow::bail!("unsupported native lineage disposition"),
    }
}

fn immutable_history_owner(db: &Connection, identity: &Value, item: &Value) -> anyhow::Result<()> {
    let (catalog, incarnation, native_base): (String,String,i64) = db.query_row(
        "SELECT catalog_id,incarnation,native_base FROM catabolic_revision_bridges WHERE native_id=?",
        [text(item,"item_id")?], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).context("historical owner lacks atomic import baseline")?;
    ensure!(
        identity["catalog_id"] == catalog
            && identity["catalog_incarnation"] == incarnation
            && item["item_revision"].as_i64() == Some(native_base),
        "historical source evidence changed after activation"
    );
    Ok(())
}
fn number(v: &Value, key: &str) -> anyhow::Result<i64> {
    v[key]
        .as_i64()
        .with_context(|| format!("physical projection missing {key}"))
}
fn revision(base: i64, head: i64) -> anyhow::Result<i64> {
    playscale_core::catalog_projection::motion_revision(base, head)
        .context("invalid/overflowing native revision bridge")
}

/// Identifiers come only from a fixed schema allowlist, never catalog JSON.
fn row(db: &Connection, table: &str, value: &Value, keys: &[&str]) -> anyhow::Result<()> {
    ensure!(
        [
            "editions",
            "timelines",
            "media_versions",
            "media_files",
            "version_files"
        ]
        .contains(&table),
        "projection table denied"
    );
    let object = value.as_object().context("physical row must be object")?;
    let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(
        object.keys().all(|k| columns.contains(k)),
        "unknown physical source column"
    );
    ensure!(
        keys.iter().all(|k| object.contains_key(*k)),
        "physical identity missing"
    );
    let selected: Vec<_> = columns
        .iter()
        .filter(|c| object.contains_key(c.as_str()))
        .collect();
    let values = selected
        .iter()
        .map(|c| match &object[c.as_str()] {
            Value::Null => Ok(rusqlite::types::Value::Null),
            Value::String(s) => Ok(s.clone().into()),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(i.into())
                } else {
                    n.as_f64().map(Into::into).context("invalid number")
                }
            }
            _ => anyhow::bail!("physical SQL row contains non scalar"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let updates = selected
        .iter()
        .filter(|c| !keys.contains(&c.as_str()))
        .map(|c| format!("{c}=excluded.{c}"))
        .collect::<Vec<_>>();
    let changed = selected
        .iter()
        .filter(|c| !keys.contains(&c.as_str()))
        .map(|c| format!("{c} IS NOT excluded.{c}"))
        .collect::<Vec<_>>();
    let sql = format!(
        "INSERT INTO {table}({}) VALUES({}) ON CONFLICT({}) DO UPDATE SET {} WHERE {}",
        selected
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(","),
        vec!["?"; values.len()].join(","),
        keys.join(","),
        updates.join(","),
        changed.join(" OR ")
    );
    db.execute(&sql, rusqlite::params_from_iter(values))?;
    Ok(())
}

pub(crate) fn rebuild(
    db: &Connection,
    host: &catabolic_runtime::host::HostSession,
    generation: i64,
    items: &[Value],
) -> anyhow::Result<()> {
    let identity = host.identity()?;
    let mut historical_absences = std::collections::BTreeSet::new();
    for item in items {
        let model = &item["metadata"]["motion_reference_tombstone"];
        if model.is_null() {
            continue;
        }
        ensure!(
            item["kind"] == "custom:motion_reference_tombstone"
                && item["retirement"].is_null()
                && model["contract_version"] == 1
                && model["absent_from_frozen_catalog"] == true,
            "historical reference tombstone changed"
        );
        immutable_history_owner(db, &identity, item)?;
        let table = text(model, "target_table")?;
        let original = text(model, "original_id")?;
        ensure!(
            ["timelines", "media_versions", "media_files"].contains(&table),
            "unsupported historical target"
        );
        let digest = catabolic_core::sha256(serde_json::to_vec(&(table, original))?);
        ensure!(
            item["item_id"] == format!("motion.tombstone:{digest}"),
            "historical tombstone identity changed"
        );
        // A later owner cannot reuse an absent historical ID and silently attach
        // old viewing/download history to unrelated new content.
        let exists: bool = db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?)"),
            [original],
            |row| row.get(0),
        )?;
        ensure!(!exists, "historical absent identity was reused");
        historical_absences.insert((table.to_owned(), original.to_owned()));
    }
    // Observation capture precedes import qualification. Its journal may also
    // contain files that became descriptive history-only owners. Only files
    // referenced by admitted live typed version owners may enter current
    // occurrence projection; the manifest owner checks their actual bindings.
    let mut typed_version_ids = std::collections::BTreeSet::new();
    for item in items {
        let model = &item["metadata"]["motion_version"];
        if model.is_null() || !item["retirement"].is_null() {
            continue;
        }
        ensure!(
            item["kind"] == "custom:motion_version" && model["contract_version"] == 1,
            "invalid current version owner"
        );
        typed_version_ids.insert(text(&model["source_version"], "id")?.to_owned());
    }
    let mut generated_history = Vec::<(OccurrenceMapping, Value)>::new();
    let mut generated_retired_versions = std::collections::BTreeSet::new();
    let mut generated_current_versions = std::collections::BTreeSet::new();
    let mut mappings = BTreeMap::<String, OccurrenceMapping>::new();
    let mut statement = db.prepare(
        "SELECT m.mapping_json,m.import_id FROM catabolic_occurrence_mappings m WHERE m.import_id IN(SELECT import_id FROM catabolic_cutover WHERE phase='activated' UNION SELECT root_import_id FROM catabolic_cutover_sets WHERE phase='activated') OR EXISTS(SELECT 1 FROM catabolic_discovery_intents d WHERE d.state='committed' AND m.import_id='discovery:'||d.operation_id AND m.native_profile=d.native_profile AND m.native_file_id=d.native_file_id) OR EXISTS(SELECT 1 FROM catabolic_generated_publications g WHERE g.receipt_json IS NOT NULL AND m.import_id='generated:'||g.operation_id) ORDER BY m.import_id,m.motion_file_id",
    )?;
    for raw in statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (raw, import_id) = raw?;
        let mapped: OccurrenceMapping = serde_json::from_str(&raw)?;
        let id = text(&mapped.motion_file, "id")?.to_owned();
        if let Some(operation) = import_id.strip_prefix("generated:") {
            let (saved,output,receipt):(String,String,String)=db.query_row(
                "SELECT command_json,output_json,receipt_json FROM catabolic_generated_publications WHERE operation_id=? AND receipt_json IS NOT NULL",
                [operation], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            let saved: Value = serde_json::from_str(&saved)?;
            let output: Value = serde_json::from_str(&output)?;
            let receipt: Value = serde_json::from_str(&receipt)?;
            let command = crate::generated_publication::decode_command(&saved["command"])?;
            ensure!(
                command.key().operation_id() == operation,
                "generated projection operation changed"
            );
            let reservation: playscale_core::processing_publication::Reservation =
                serde_json::from_value(saved["reservation"].clone())?;
            reservation.validate().map_err(anyhow::Error::msg)?;
            ensure!(
                reservation.identity().payload_digest == command.digest().as_str()
                    && reservation.identity().operation_id == operation
                    && reservation.identity().catalog_id == command.key().catalog_id()
                    && reservation.identity().incarnation == command.admission_incarnation(),
                "generated projection reservation changed"
            );
            let private = crate::generated_source::mapping_sync(db)?
                .context("private generated source missing")?;
            crate::generated_source::verify_with_host(host, &private)?;
            ensure!(
                output["mapping"] == serde_json::to_value(&private)?,
                "generated output root proof changed"
            );
            let reconstructed = crate::generated_publication::mapping(
                &command,
                &receipt["result"],
                &private,
                &output["probe"],
                receipt["committed_generation"]
                    .as_i64()
                    .context("generated receipt generation missing")?,
            )?;
            ensure!(
                mapped == reconstructed,
                "generated projection mapping changed"
            );
            let native_id = command.payload()["request"]["item"]["id"]
                .as_str()
                .context("generated native item missing")?;
            let item = items
                .iter()
                .find(|item| item["item_id"] == native_id)
                .context("generated native owner missing")?;
            let result = &receipt["result"];
            let history = host.historical_version(
                generation,
                text(result, "version_id")?,
                native_id,
                number(result, "version_revision")?,
            )?;
            if crate::generated_publication::verify_historical(&command, result, &history)? {
                generated_retired_versions.insert(text(result, "version_id")?.to_owned());
                generated_history.push((mapped, history));
                continue;
            }
            crate::generated_publication::verify_native_item(&command, result, item)?;
            generated_current_versions.insert(text(result, "version_id")?.to_owned());
        }
        if !mapped.version_files.iter().any(|binding| {
            binding["version_id"]
                .as_str()
                .is_some_and(|version| typed_version_ids.contains(version))
        }) {
            continue;
        }
        if let Some(prior) = mappings.insert(id, mapped.clone()) {
            ensure!(prior == mapped, "conflicting physical identity records");
        }
    }
    // Explicit invalidation, not absence-as-deletion: preserve every old FK target.
    db.execute("UPDATE catabolic_projection_files SET available=0", [])?;
    db.execute("UPDATE catabolic_projection_versions SET ready=0", [])?;
    let mut available = BTreeMap::<String, Value>::new();
    let mut retired = std::collections::BTreeSet::new();
    if !mappings.is_empty() {
        let projected = crate::occurrence_bridge::project_with_host(
            host,
            &mappings.values().cloned().collect::<Vec<_>>(),
        )?;
        ensure!(
            projected.generation == generation,
            "physical occurrence generation changed"
        );
        retired = projected.retired_file_ids;
        for file in projected.files {
            available.insert(text(&file, "id")?.to_owned(), file);
        }
    }
    // The private proof rows were invalidated above. Publish only final media
    // availability deltas, never a down/up cycle on every read: those legacy
    // triggers would spuriously revise preparation/download dependencies.
    let current_file_ids: Vec<_> = available
        .iter()
        .filter(|(_, file)| file["available"] == 1)
        .map(|(id, _)| id)
        .collect();
    db.execute("UPDATE media_files SET available=0 WHERE available<>0 AND id NOT IN(SELECT value FROM json_each(?))",
        [serde_json::to_string(&current_file_ids)?])?;
    // Only explicit current native missing/replacement evidence releases an
    // immutable old locator. Unknown/omitted occurrences remain collision-closed.
    for id in &retired {
        db.execute("UPDATE media_files SET available=0,native_history=1 WHERE id=? AND (available<>0 OR native_history<>1)", [id])?;
    }
    for (id, file) in &mut available {
        let previous: Option<i64> = db
            .query_row(
                "SELECT native_history FROM media_files WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        file["native_history"] = json!(if file["available"] == 1 {
            0
        } else if retired.contains(id) {
            1
        } else {
            previous.unwrap_or(0)
        });
    }
    let mut timelines = BTreeMap::<String, Value>::new();
    for item in items {
        let model = &item["metadata"]["motion_edition"];
        if model.is_null() || !item["retirement"].is_null() {
            continue;
        }
        ensure!(
            model["contract_version"] == 1,
            "unsupported edition projection"
        );
        let mut edition = model["source_edition"].clone();
        ensure!(
            item["item_id"] == format!("motion.edition:{}", text(&edition, "id")?),
            "edition identity mismatch"
        );
        let native_id = text(item, "item_id")?;
        let (catalog, incarnation, base, native_base): (String, String, i64, i64) = db.query_row(
            "SELECT catalog_id,incarnation,motion_base,native_base FROM catabolic_revision_bridges WHERE native_id=?",
            [native_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        ensure!(
            identity["catalog_id"] == catalog && identity["catalog_incarnation"] == incarnation,
            "edition revision bridge identity changed"
        );
        ensure!(
            base == number(&edition, "revision")?,
            "edition source baseline changed"
        );
        edition["revision"] = json!(
            playscale_core::catalog_projection::bridged_revision(
                base,
                native_base,
                number(item, "item_revision")?
            )
            .context("invalid edition revision bridge")?
        );
        row(db, "editions", &edition, &["id"])?;
        for timeline in model["source_timelines"]
            .as_array()
            .context("edition timeline evidence missing")?
        {
            ensure!(
                timeline["edition_id"] == edition["id"],
                "timeline edition mismatch"
            );
            let id = text(timeline, "id")?.to_owned();
            if model["origin"] == "native_empty_edition" {
                let facts = host.projection_timeline(generation, &id, native_id)?;
                if let Some(descriptive) = crate::empty_timeline_projection::row(
                    &identity, generation, item, timeline, &facts,
                )? {
                    row(db, "timelines", &descriptive, &["id"])?;
                }
            }
            if let Some(prior) = timelines.insert(id, timeline.clone()) {
                ensure!(prior == *timeline, "conflicting timeline base");
            }
        }
    }
    let mut bindings = Vec::new();
    // Explicit imported history owners are descriptive native records, not
    // occurrence/version grants. Keep application FK targets while making their
    // locators unavailable; missing inventory alone never enters this branch.
    let mut historical_files = Vec::new();
    let active_timelines: std::collections::BTreeSet<_> = items
        .iter()
        .filter(|item| {
            item["retirement"].is_null() && !item["metadata"]["motion_version"].is_null()
        })
        .filter_map(|item| {
            item["metadata"]["motion_version"]["source_version"]["timeline_id"].as_str()
        })
        .collect();
    for item in items {
        if !item["retirement"].is_null() {
            continue;
        }
        let model = &item["metadata"]["motion_historical_file"];
        if !model.is_null() {
            immutable_history_owner(db, &identity, item)?;
            ensure!(
                item["kind"] == "custom:motion_historical_file" && model["contract_version"] == 1,
                "invalid historical file owner"
            );
            let mut file = model["source_file"].clone();
            let id = text(&file, "id")?;
            ensure!(
                item["item_id"] == format!("motion.file-history:{id}")
                    && !mappings.contains_key(id),
                "historical file identity conflicts with occurrence authority"
            );
            let source_id = text(&file, "library_id")?;
            let source_owner = items
                .iter()
                .find(|owner| {
                    owner["item_id"] == format!("motion.source:{source_id}")
                        && owner["kind"] == "custom:motion_source"
                        && owner["retirement"].is_null()
                })
                .context("historical source native owner missing")?;
            immutable_history_owner(db, &identity, source_owner)?;
            let source_model = &source_owner["metadata"]["motion_source_history"];
            ensure!(
                source_model["contract_version"] == 1,
                "historical source contract changed"
            );
            let descriptor: String = db.query_row(
                "SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?",
                [source_id], |row| row.get(0),
            ).context("historical source descriptor must remain preserved")?;
            ensure!(
                serde_json::from_str::<Value>(&descriptor)? == source_model["source_row"],
                "historical source descriptor changed"
            );
            file["available"] = json!(0);
            file["native_history"] = json!(1);
            historical_files.push(file);
        }
        let model = &item["metadata"]["motion_historical_timeline"];
        if !model.is_null() {
            immutable_history_owner(db, &identity, item)?;
            ensure!(
                item["kind"] == "custom:motion_historical_timeline"
                    && model["contract_version"] == 1,
                "invalid historical timeline owner"
            );
            let timeline = &model["source_timeline"];
            ensure!(
                item["item_id"] == format!("motion.timeline-history:{}", text(timeline, "id")?),
                "historical timeline identity changed"
            );
            if !active_timelines.contains(text(timeline, "id")?) {
                row(db, "timelines", timeline, &["id"])?;
            }
        }
    }
    for item in items {
        let model = &item["metadata"]["motion_historical_version"];
        if model.is_null() || !item["retirement"].is_null() {
            continue;
        }
        immutable_history_owner(db, &identity, item)?;
        ensure!(
            item["kind"] == "custom:motion_historical_version" && model["contract_version"] == 1,
            "invalid historical version owner"
        );
        let version = &model["source_version"];
        let id = text(version, "id")?;
        let timeline = &model["source_timeline"];
        ensure!(
            item["item_id"] == format!("motion.version:{id}")
                && version["timeline_id"] == timeline["id"],
            "historical version identity changed"
        );
        upsert_historical_version(
            db,
            timeline,
            version,
            active_timelines.contains(text(timeline, "id")?),
        )?;
        for binding in model["source_bindings"]
            .as_array()
            .context("historical version bindings missing")?
        {
            ensure!(
                binding["version_id"] == id,
                "historical binding identity changed"
            );
            bindings.push(binding.clone());
        }
    }
    for file in historical_files {
        row(db, "media_files", &file, &["id"])?;
    }
    for item in items {
        let model = &item["metadata"]["motion_version"];
        if model.is_null() || !item["retirement"].is_null() {
            continue;
        }
        ensure!(
            model["contract_version"] == 1,
            "unsupported version projection"
        );
        let mut version = model["source_version"].clone();
        let id = text(&version, "id")?.to_owned();
        if generated_retired_versions.contains(&id) {
            continue;
        }
        ensure!(
            item["item_id"] == format!("motion.version:{id}"),
            "version item identity mismatch"
        );
        // A current native occurrence can explicitly retire an imported exact
        // file while its historical version remains a valid FK/history target.
        // The live identity-manifest owner correctly refuses that stale file;
        // do not ask it to bless history as a current playable manifest. The
        // proof cache was invalidated above and is deliberately not stamped
        // with this generation. Preserve old version/timeline/pin rows only.
        // This is based on exact current missing/replacement observations, not
        // absent inventory, a caught error string, or imported availability.
        let has_retired_binding = mappings.iter().any(|(file_id, mapping)| {
            retired.contains(file_id)
                && mapping
                    .version_files
                    .iter()
                    .any(|binding| binding["version_id"] == id)
        });
        if has_retired_binding {
            continue;
        }
        // Current typed heads alone do not prove that this work still owns the
        // edition/version through its native structural graph. Explicit native
        // detachment/retirement preserves history, but never stamps readiness.
        if !current_lineage(host, generation, items, item, &version)? {
            continue;
        }
        let request = lineage_request(items, item, &version)?;
        let generated = generated_current_versions.contains(&id);
        let envelope = if generated {
            host.projection_selected_current_versions(generation, std::slice::from_ref(&request))?
        } else {
            host.projection_version_manifest(generation, &[id.clone()])?
        };
        let proof = crate::version_proof::read(
            &envelope,
            text(&identity, "catalog_id")?,
            text(&identity, "catalog_incarnation")?,
            generation,
            &request,
            if generated {
                crate::version_proof::ProofOrigin::ReceiptVerifiedGenerated
            } else {
                crate::version_proof::ProofOrigin::Original
            },
        )?;
        let head = &proof.head;
        let manifest_version = &proof.version;
        let contract = &manifest_version["version"];
        ensure!(
            contract["association_item_id"] == item["item_id"]
                && contract["timeline_id"] == version["timeline_id"],
            "native version ownership changed without reference translation"
        );
        let timeline_id = text(&version, "timeline_id")?.to_owned();
        let mut timeline = timelines
            .get(&timeline_id)
            .context("version timeline has no preserved native evidence")?
            .clone();
        ensure!(
            contract["edition_item_id"]
                == format!("motion.edition:{}", text(&timeline, "edition_id")?),
            "version edition identity changed"
        );
        let native_version = number(head, "version_revision")?;
        let native_timeline = number(head, "timeline_revision")?;
        let current_timeline = number(head, "current_timeline_revision")?;
        version["revision"] = json!(revision(number(&version, "revision")?, native_version)?);
        timeline["revision"] = json!(revision(number(&timeline, "revision")?, current_timeline)?);
        let coverage = &head["timeline_coverage"];
        let current_duration = if coverage.is_null() {
            None
        } else {
            let start = number(coverage, "start_us")?;
            let end = number(coverage, "end_us")?;
            if start == 0 && end > 0 && end % 1000 == 0 {
                Some(end / 1000)
            } else {
                None
            }
        };
        // No fractional/offset timeline is silently rounded into Motion's
        // integral, zero-based wire representation.
        timeline["duration_ms"] = json!(current_duration);
        row(db, "timelines", &timeline, &["id"])?;
        // The current native evidence, not an imported scalar, owns equivalence.
        version["equivalence"] = match manifest_version["equivalence"]["basis"].as_str() {
            Some("declared") => json!("declared"),
            Some("verified") => json!("verified"),
            None if manifest_version["equivalence"].is_null() => json!("unknown"),
            _ => anyhow::bail!("unsupported current native equivalence"),
        };
        row(db, "media_versions", &version, &["id"])?;
        let mut ready = contract["state"] == "ready"
            && native_timeline == current_timeline
            && current_duration.is_some();
        let native_bindings = contract["bindings"]
            .as_array()
            .context("native version bindings missing")?;
        let preserved_count = mappings
            .values()
            .flat_map(|m| m.version_files.iter())
            .filter(|b| b["version_id"] == id)
            .count();
        ready &= preserved_count == native_bindings.len();
        let mut current_bindings = Vec::new();
        let mut cursor = Some(0_i64);
        for (index, native) in native_bindings.iter().enumerate() {
            ready &= native["part"].as_u64() == Some(index as u64 + 1);
            let mapped = mappings
                .values()
                .find(|m| {
                    m.physical.exact.file_id == native["file"]["file_id"]
                        && m.physical.exact.revision == native["file"]["revision"]
                        && m.version_files
                            .iter()
                            .any(|b| b["version_id"] == id && b["part"] == native["part"])
                })
                .context("native binding has no exact preserved Motion reference")?;
            let source = mapped
                .version_files
                .iter()
                .find(|b| b["version_id"] == id && b["part"] == native["part"])
                .unwrap();
            ensure!(
                source["file_revision"] == mapped.motion_file["revision"],
                "historical content revision mismatch"
            );
            // Translate the current native typed extent, never the immutable
            // imported extent. Only contiguous integral-millisecond layouts are
            // representable in Motion; a native gap/offset/fraction stays closed.
            let interval = |value: &Value| -> Option<(i64, i64)> {
                Some((value["start_us"].as_i64()?, value["end_us"].as_i64()?))
            };
            let projected = match native["source"]["kind"].as_str() {
                Some("whole_file") => cursor.and_then(|start| {
                    playscale_core::catalog_projection::project_extent(
                        None,
                        native["source"]["duration_us"].as_i64(),
                        interval(&native["timeline_interval"]),
                        start,
                    )
                }),
                Some("slice") => interval(&native["source"]["interval"]).and_then(|slice| {
                    cursor.and_then(|start| {
                        playscale_core::catalog_projection::project_extent(
                            Some(slice),
                            None,
                            interval(&native["timeline_interval"]),
                            start,
                        )
                    })
                }),
                _ => None,
            };
            let mut current_binding = source.clone();
            if let Some(extent) = projected {
                current_binding["start_ms"] = json!(extent.start_ms);
                current_binding["end_ms"] = json!(extent.end_ms);
                cursor = Some(extent.next_timeline_us);
            } else {
                cursor = None;
                ready = false;
            }
            let file = available
                .get(text(&mapped.motion_file, "id")?)
                .context("missing occurrence projection")?;
            ready &= file["available"] == 1;
            current_bindings.push(current_binding);
        }
        ready &= cursor == current_duration.and_then(|duration| duration.checked_mul(1000));
        if ready {
            bindings.extend(current_bindings);
        }
        db.execute("INSERT INTO catabolic_projection_versions VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(version_id) DO UPDATE SET motion_version_revision=excluded.motion_version_revision,timeline_id=excluded.timeline_id,motion_timeline_revision=excluded.motion_timeline_revision,native_version_revision=excluded.native_version_revision,native_timeline_revision=excluded.native_timeline_revision,native_current_timeline_revision=excluded.native_current_timeline_revision,generation=excluded.generation,ready=excluded.ready,manifest_json=excluded.manifest_json",params![id,number(&version,"revision")?,timeline_id,number(&timeline,"revision")?,native_version,native_timeline,current_timeline,generation,i64::from(ready),serde_json::to_string(&envelope)?])?;
    }
    let imported_sources = crate::imported_sources::imported_sources_sync(db)?
        .into_iter()
        .map(|record| (record.descriptor.id.clone(), record))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (id, file) in &available {
        let mapped = &mappings[id];
        if let Some(imported) = imported_sources.get(&mapped.source.library_id) {
            ensure!(
                imported.proof.mapping == mapped.source,
                "present imported source mapping differs from immutable proof"
            );
            let cached: Option<String> = db.query_row(
                "SELECT json_object('id',id,'name',name,'root',root,'root_identity',root_identity,'managed',managed,'enabled',enabled,'revision',revision,'binding_revision',binding_revision,'exclusions_json',exclusions_json,'scan_barrier',scan_barrier) FROM libraries WHERE id=?",
                [&mapped.source.library_id], |row| row.get(0)).optional()?;
            if let Some(cached) = cached {
                crate::imported_sources::validate_cached_row(
                    imported,
                    &serde_json::from_str(&cached)?,
                )?;
            }
        }
        let source: Option<(String, String)> = db
            .query_row(
                "SELECT root,root_identity FROM libraries WHERE id=?",
                [&mapped.source.library_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if source.is_none() || mapped.source.library_id == crate::generated_source::SOURCE_ID {
            crate::projection::materialize_source(db, host, &mapped.source)?;
        }
        ensure!(
            source.is_none()
                || source
                    == Some((
                        mapped.source.motion_root.clone(),
                        mapped.source.motion_root_identity.clone()
                    )),
            "native source compatibility descriptor missing or mismatched"
        );
        row(db, "media_files", file, &["id"])?;
        db.execute("INSERT INTO catabolic_projection_files VALUES(?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET native_profile=excluded.native_profile,native_file_id=excluded.native_file_id,native_revision=excluded.native_revision,generation=excluded.generation,available=excluded.available,mapping_json=excluded.mapping_json",params![id,mapped.physical.profile,mapped.physical.exact.file_id,mapped.physical.exact.revision,generation,number(file,"available")?,serde_json::to_string(mapped)?])?;
    }
    for binding in bindings {
        row(db, "version_files", &binding, &["version_id", "part"])?;
    }
    for (mapping, history) in generated_history {
        crate::projection::materialize_source(db, host, &mapping.source)?;
        let timeline_id = text(&history["version_snapshot"], "timeline_id")?;
        let fallback;
        let timeline = if let Some(timeline) = timelines.get(timeline_id) {
            timeline
        } else {
            fallback = retired_edition_timeline(db, &identity, items, &history)?;
            &fallback
        };
        materialize_generated_history(
            db,
            &mapping.motion_file,
            &mapping.version_files,
            &history,
            timeline,
        )?;
    }
    // Legacy version triggers increment a timeline during cache upserts. Its
    // wire revision is still exclusively the mapped current native timeline
    // head, not the number/order of projection writes.
    let conflicting_timeline_heads: i64 = db.query_row(
        "SELECT count(*) FROM (SELECT timeline_id FROM catabolic_projection_versions WHERE generation=? GROUP BY timeline_id HAVING count(DISTINCT motion_timeline_revision)<>1)",
        [generation], |row| row.get(0),
    )?;
    ensure!(
        conflicting_timeline_heads == 0,
        "conflicting native timeline revision bridges"
    );
    db.execute("UPDATE timelines SET revision=(SELECT p.motion_timeline_revision FROM catabolic_projection_versions p WHERE p.timeline_id=timelines.id AND p.generation=? LIMIT 1) WHERE EXISTS(SELECT 1 FROM catabolic_projection_versions p WHERE p.timeline_id=timelines.id AND p.generation=? AND p.motion_timeline_revision<>timelines.revision)", params![generation,generation])?;
    for (table, id) in &historical_absences {
        let exists: bool = db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?)"),
            [id],
            |row| row.get(0),
        )?;
        ensure!(
            !exists,
            "projection attempted to reuse historical absent identity"
        );
    }
    // Structural legacy triggers are cache maintenance, never a revision owner.
    db.execute("UPDATE items SET catalog_revision=(SELECT b.motion_base+(p.native_revision-b.native_base) FROM catabolic_projection_items p JOIN catabolic_revision_bridges b ON b.native_id=p.native_id WHERE p.native_id=items.id) WHERE id IN(SELECT native_id FROM catabolic_projection_items WHERE generation=?) AND catalog_revision IS NOT (SELECT b.motion_base+(p.native_revision-b.native_base) FROM catabolic_projection_items p JOIN catabolic_revision_bridges b ON b.native_id=p.native_id WHERE p.native_id=items.id)",[generation])?;
    Ok(())
}

/// A logically retired edition is not in the live timeline map. Its positive
/// native retirement and retained metadata may still reconstruct FK history.
fn retired_edition_timeline(
    db: &Connection,
    identity: &Value,
    items: &[Value],
    history: &Value,
) -> anyhow::Result<Value> {
    ensure!(
        history["retired"] == true
            && history["descriptive_only"] == true
            && history["playable"] == false,
        "descriptive native retirement required"
    );
    let edition_id = text(&history["timeline"], "edition_item_id")?;
    let timeline_id = text(&history["timeline"], "id")?;
    let edition = items
        .iter()
        .find(|item| item["item_id"] == edition_id)
        .context("retired native edition missing")?;
    let retired = history["logical_retirements"]
        .as_array()
        .context("logical retirement facts missing")?
        .iter()
        .find(|row| row["item_id"] == edition_id)
        .context("positive edition retirement missing")?;
    ensure!(
        edition["kind"] == "custom:motion_edition"
            && !edition["retirement"].is_null()
            && edition["retirement"]["kind"] == retired["kind"],
        "retired edition owner changed"
    );
    let model = &edition["metadata"]["motion_edition"];
    let mut source = model["source_edition"].clone();
    ensure!(
        model["contract_version"] == 1
            && edition_id == format!("motion.edition:{}", text(&source, "id")?),
        "retired edition metadata changed"
    );
    let (catalog,incarnation,base,native_base):(String,String,i64,i64)=db.query_row("SELECT catalog_id,incarnation,motion_base,native_base FROM catabolic_revision_bridges WHERE native_id=?",[edition_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    ensure!(
        identity["catalog_id"] == catalog
            && identity["catalog_incarnation"] == incarnation
            && base == number(&source, "revision")?,
        "retired edition receipt baseline changed"
    );
    source["revision"] = json!(
        playscale_core::catalog_projection::bridged_revision(
            base,
            native_base,
            number(edition, "item_revision")?
        )
        .context("retired edition revision invalid")?
    );
    let candidates = model["source_timelines"]
        .as_array()
        .context("retired edition timeline basis missing")?;
    let matches = candidates
        .iter()
        .filter(|row| row["id"] == timeline_id)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1
            && matches[0]["edition_id"] == source["id"]
            && history["timeline_snapshot"]["timeline_id"] == timeline_id,
        "retired timeline basis changed"
    );
    let existing: Option<String> = db
        .query_row(
            "SELECT item_id FROM editions WHERE id=?",
            [text(&source, "id")?],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(work) = existing {
        ensure!(
            source["item_id"] == work,
            "retired edition FK identity changed"
        );
    } else {
        row(db, "editions", &source, &["id"])?;
    }
    Ok(matches[0].clone())
}

/// Positive native retirement only. Existing FK/history rows are never rewound
/// to the original receipt snapshot, even if their current revisions are newer.
fn materialize_generated_history(
    db: &Connection,
    motion_file: &Value,
    version_files: &[Value],
    history: &Value,
    timeline_base: &Value,
) -> anyhow::Result<()> {
    ensure!(
        history["retired"] == true
            && history["descriptive_only"] == true
            && history["playable"] == false,
        "generated retirement proof required"
    );
    let metadata: Value = serde_json::from_str(text(&history["association_item"], "metadata")?)?;
    let mut version = metadata["motion_version"]["source_version"].clone();
    let version_id = text(&version, "id")?.to_owned();
    let timeline_id = text(&version, "timeline_id")?.to_owned();
    ensure!(
        timeline_base["id"] == timeline_id
            && history["version_snapshot"]["version_id"] == version_id,
        "generated history FK scope changed"
    );
    let prior_timeline: Option<i64> = db
        .query_row(
            "SELECT revision FROM timelines WHERE id=?",
            [&timeline_id],
            |r| r.get(0),
        )
        .optional()?;
    if prior_timeline.is_none() {
        let mut timeline = timeline_base.clone();
        timeline["revision"] = json!(revision(
            number(timeline_base, "revision")?,
            number(&history["timeline_snapshot"], "revision")?
        )?);
        let snapshot = &history["timeline_snapshot"];
        timeline["duration_ms"] = match (
            snapshot["coverage_start_us"].as_i64(),
            snapshot["coverage_end_us"].as_i64(),
        ) {
            (Some(0), Some(end)) if end > 0 && end % 1000 == 0 => json!(end / 1000),
            _ => Value::Null,
        };
        row(db, "timelines", &timeline, &["id"])?;
    }
    let retained_timeline: i64 = db.query_row(
        "SELECT revision FROM timelines WHERE id=?",
        [&timeline_id],
        |r| r.get(0),
    )?;
    let original_revision = revision(
        number(&version, "revision")?,
        number(&history["version_snapshot"], "revision")?,
    )?;
    let exists: Option<(String, i64)> = db
        .query_row(
            "SELECT timeline_id,revision FROM media_versions WHERE id=?",
            [&version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((existing, _)) = &exists {
        ensure!(
            existing == &timeline_id,
            "generated historical version identity collision"
        );
    } else {
        version["revision"] = json!(original_revision);
        row(db, "media_versions", &version, &["id"])?;
    }
    let mut file = motion_file.clone();
    file["available"] = json!(0);
    file["native_history"] = json!(1);
    let file_id = text(&file, "id")?;
    let prior_file: Option<(String, String, String, i64)> = db
        .query_row(
            "SELECT edition_id,library_id,revision,generated FROM media_files WHERE id=?",
            [file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if let Some((edition, source, revision, generated)) = prior_file {
        ensure!(
            file["edition_id"] == edition
                && file["library_id"] == source
                && file["revision"] == revision
                && generated == 1,
            "generated historical file identity collision"
        );
        db.execute("UPDATE media_files SET available=0,native_history=1 WHERE id=? AND (available<>0 OR native_history<>1)",[file_id])?;
    } else {
        row(db, "media_files", &file, &["id"])?;
    }
    if exists
        .as_ref()
        .is_none_or(|(_, head)| *head == original_revision)
    {
        for binding in version_files {
            let present: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM version_files WHERE version_id=? AND part=?)",
                params![text(binding, "version_id")?, number(binding, "part")?],
                |r| r.get(0),
            )?;
            if !present {
                row(db, "version_files", binding, &["version_id", "part"])?;
            }
        }
    }
    db.execute(
        "UPDATE timelines SET revision=? WHERE id=? AND revision<>?",
        params![retained_timeline, timeline_id, retained_timeline],
    )?;
    Ok(())
}

fn upsert_historical_version(
    db: &Connection,
    timeline: &Value,
    version: &Value,
    active_timeline: bool,
) -> anyhow::Result<()> {
    let prior_timeline_revision: Option<i64> = db
        .query_row(
            "SELECT revision FROM timelines WHERE id=?",
            [text(timeline, "id")?],
            |row| row.get(0),
        )
        .optional()?;
    if !active_timeline || prior_timeline_revision.is_none() {
        row(db, "timelines", timeline, &["id"])?;
    }
    row(db, "media_versions", version, &["id"])?;
    // A current sibling may be unavailable and therefore intentionally skip
    // live manifest publication. Restore the pre-existing history revision
    // now, not only if that sibling later produces a current proof row.
    if active_timeline {
        let retained = prior_timeline_revision.unwrap_or(number(timeline, "revision")?);
        db.execute(
            "UPDATE timelines SET revision=? WHERE id=? AND revision<>?",
            params![retained, text(timeline, "id")?, retained],
        )?;
    } else {
        row(db, "timelines", timeline, &["id"])?;
    }
    Ok(())
}

#[cfg(test)]
mod history_tests {
    use super::*;
    #[test]
    fn retired_edition_reconstructs_descriptive_basis_without_a_live_timeline_map() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE editions(id TEXT PRIMARY KEY,item_id TEXT,revision INTEGER,label TEXT); CREATE TABLE catabolic_revision_bridges(native_id TEXT PRIMARY KEY,catalog_id TEXT,incarnation TEXT,motion_base INTEGER,native_base INTEGER); INSERT INTO catabolic_revision_bridges VALUES('motion.edition:e','catalog','incarnation',7,1);").unwrap();
        let identity = json!({"catalog_id":"catalog","catalog_incarnation":"incarnation"});
        let timeline = json!({"id":"t","edition_id":"e","revision":9,"duration_ms":1000});
        let items = vec![
            json!({"item_id":"motion.edition:e","kind":"custom:motion_edition","item_revision":2,"retirement":{"kind":"merge"},"metadata":{"motion_edition":{"contract_version":1,"source_edition":{"id":"e","item_id":"work","revision":7,"label":"Original"},"source_timelines":[timeline]}}}),
        ];
        let mut history = json!({"retired":true,"descriptive_only":true,"playable":false,"timeline":{"id":"t","edition_item_id":"motion.edition:e"},"timeline_snapshot":{"timeline_id":"t"},"logical_retirements":[{"item_id":"motion.edition:e","kind":"merge"}]});
        assert_eq!(
            retired_edition_timeline(&db, &identity, &items, &history).unwrap(),
            timeline
        );
        assert_eq!(
            db.query_row("SELECT revision FROM editions", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            8
        );
        db.execute_batch("UPDATE editions SET revision=12,label='Preserved'")
            .unwrap();
        retired_edition_timeline(&db, &identity, &items, &history).unwrap();
        assert_eq!(
            db.query_row("SELECT revision,label FROM editions", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?
            )))
            .unwrap(),
            (12, "Preserved".into())
        );
        history["logical_retirements"] = json!([]);
        assert!(retired_edition_timeline(&db, &identity, &items, &history).is_err());
    }

    #[test]
    fn generated_retirement_rebuild_inserts_unavailable_history_without_rewinding_rows() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE timelines(id TEXT PRIMARY KEY,edition_id TEXT,revision INTEGER,duration_ms INTEGER); CREATE TABLE media_versions(id TEXT PRIMARY KEY,timeline_id TEXT,revision INTEGER,label TEXT,origin TEXT,equivalence TEXT); CREATE TABLE media_files(id TEXT PRIMARY KEY,edition_id TEXT,library_id TEXT,revision TEXT,generated INTEGER,available INTEGER,native_history INTEGER); CREATE TABLE version_files(version_id TEXT,part INTEGER,file_id TEXT,file_revision TEXT,start_ms INTEGER,end_ms INTEGER,PRIMARY KEY(version_id,part)); CREATE TRIGGER version_insert AFTER INSERT ON media_versions BEGIN UPDATE timelines SET revision=revision+1 WHERE id=NEW.timeline_id; END; INSERT INTO timelines VALUES('t','e',9,1000);").unwrap();
        let version = json!({"id":"v","timeline_id":"t","revision":1,"label":"Generated","origin":"generated","equivalence":"declared"});
        let history = json!({"retired":true,"descriptive_only":true,"playable":false,"association_item":{"metadata":json!({"motion_version":{"source_version":version}}).to_string()},"version_snapshot":{"version_id":"v","revision":1},"timeline_snapshot":{"revision":1,"coverage_start_us":0,"coverage_end_us":1000000}});
        let file = json!({"id":"f","edition_id":"e","library_id":"private","revision":"hash","generated":1,"available":1});
        let bindings = vec![
            json!({"version_id":"v","part":1,"file_id":"f","file_revision":"hash","start_ms":null,"end_ms":null}),
        ];
        let timeline = json!({"id":"t","edition_id":"e","revision":1,"duration_ms":1000});
        materialize_generated_history(&db, &file, &bindings, &history, &timeline).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT available,native_history FROM media_files",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
            (0, 1)
        );
        assert_eq!(
            db.query_row("SELECT revision FROM timelines", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            9
        );
        db.execute_batch("DELETE FROM version_files").unwrap();
        materialize_generated_history(&db, &file, &bindings, &history, &timeline).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM version_files", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        db.execute_batch("UPDATE media_versions SET revision=8,label='Later'; UPDATE version_files SET start_ms=20,end_ms=50;").unwrap();
        materialize_generated_history(&db, &file, &bindings, &history, &timeline).unwrap();
        assert_eq!(
            db.query_row("SELECT revision,label FROM media_versions", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?
            )))
            .unwrap(),
            (8, "Later".into())
        );
        assert_eq!(
            db.query_row("SELECT start_ms,end_ms FROM version_files", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?
            )))
            .unwrap(),
            (20, 50)
        );
        let mut absent = history.clone();
        absent["retired"] = json!(false);
        assert!(materialize_generated_history(&db, &file, &bindings, &absent, &timeline).is_err());
        let mut changed = file.clone();
        changed["revision"] = json!("other");
        assert!(
            materialize_generated_history(&db, &changed, &bindings, &history, &timeline).is_err()
        );
    }

    #[test]
    fn historical_sibling_rebuild_preserves_unavailable_current_timeline_revision() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE timelines(id TEXT PRIMARY KEY,edition_id TEXT,revision INTEGER,duration_ms INTEGER); CREATE TABLE media_versions(id TEXT PRIMARY KEY,timeline_id TEXT,revision INTEGER); CREATE TABLE events(id INTEGER); CREATE TRIGGER version_insert AFTER INSERT ON media_versions BEGIN UPDATE timelines SET revision=revision+1 WHERE id=NEW.timeline_id; INSERT INTO events VALUES(1); END;").unwrap();
        db.execute("INSERT INTO timelines VALUES('t','e',9,1000)", [])
            .unwrap();
        let historical_timeline =
            json!({"id":"t","edition_id":"e","revision":7,"duration_ms":2000});
        let version = json!({"id":"historical","timeline_id":"t","revision":3});
        upsert_historical_version(&db, &historical_timeline, &version, true).unwrap();
        assert_eq!(
            db.query_row("SELECT revision,duration_ms FROM timelines", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?
            )))
            .unwrap(),
            (9, 1000)
        );
        upsert_historical_version(&db, &historical_timeline, &version, true).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT revision FROM timelines", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            9
        );
    }
}
