//! Decode a named version from an independently admitted native snapshot.
//! This parser grants neither receipt provenance, application scope nor bytes.
use anyhow::{Context, ensure};
use catabolic_store::catalog_projection::VersionLineageRequest;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofOrigin {
    Original,
    /// Caller has independently verified the full generated receipt and owner.
    ReceiptVerifiedGenerated,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProofKind {
    Strict,
    Selected,
}
pub struct VersionProof {
    pub kind: ProofKind,
    pub version: Value,
    pub head: Value,
    pub request: VersionLineageRequest,
}
fn one<'a>(rows: &'a Value, key: &str, id: &str) -> anyhow::Result<&'a Value> {
    let matches: Vec<_> = rows
        .as_array()
        .context("version proof inventory missing")?
        .iter()
        .filter(|r| r[key] == id)
        .collect();
    ensure!(
        matches.len() == 1,
        "version proof named inventory is ambiguous"
    );
    Ok(matches[0])
}
pub fn read(
    envelope: &Value,
    catalog: &str,
    incarnation: &str,
    generation: i64,
    request: &VersionLineageRequest,
    origin: ProofOrigin,
) -> anyhow::Result<VersionProof> {
    ensure!(
        serde_json::to_vec(envelope)?.len() <= 8 * 1024 * 1024,
        "version proof exceeds bound"
    );
    ensure!(
        envelope["catalog_id"] == catalog
            && envelope["incarnation"] == incarnation
            && envelope["generation"] == generation,
        "version proof fence changed"
    );
    let (kind, version, head) = if envelope.get("selection").is_some() {
        ensure!(
            origin == ProofOrigin::ReceiptVerifiedGenerated
                && envelope.get("manifest").is_none()
                && envelope.get("version_revisions").is_none(),
            "selected proof is not a strict original manifest"
        );
        ensure!(
            envelope["contract_version"] == 1
                && envelope["application_authority"] == false
                && envelope["profile"].as_str().is_some_and(|p| !p.is_empty()),
            "selected proof contract changed"
        );
        let selection = &envelope["selection"];
        ensure!(
            selection["all_context_occurrences_current"] == false,
            "selected proof claims whole-context occurrence authority"
        );
        let rows = envelope["lineage"]["rows"]
            .as_array()
            .context("selected lineage missing")?;
        ensure!(
            envelope["lineage"]["catalog_id"] == catalog
                && envelope["lineage"]["incarnation"] == incarnation
                && envelope["lineage"]["generation"] == generation
                && rows.len() == 1,
            "selected lineage fence changed"
        );
        let row = &rows[0];
        ensure!(
            row["status"] == "active"
                && row["work_item_id"] == request.work_item_id
                && row["edition_item_id"] == request.edition_item_id
                && row["version_item_id"] == request.version_item_id
                && row["version_id"] == request.version_id,
            "selected lineage tuple changed"
        );
        let versions = selection["selected_versions"]
            .as_array()
            .context("selected versions missing")?;
        ensure!(
            versions.len() == 1,
            "selected proof is not a single named output"
        );
        let version = versions[0].clone();
        let bindings = version["version"]["bindings"]
            .as_array()
            .context("selected output bindings missing")?;
        let occurrences = selection["current_occurrences"]
            .as_array()
            .context("selected current occurrences missing")?;
        ensure!(
            bindings.len() == 1
                && occurrences.len() == 1
                && bindings[0]["file"] == occurrences[0]["file"]
                && occurrences[0]["snapshot"]["status"] == "present"
                && occurrences[0]["snapshot"]["ctime_ns"].is_i64()
                && occurrences[0]["validated_stat"]["ctime_ns"]
                    == occurrences[0]["snapshot"]["ctime_ns"],
            "selected output occurrence proof changed"
        );
        let mut head = one(
            &selection["complete_context"]["version_heads"],
            "version_id",
            &request.version_id,
        )?
        .clone();
        let structural = selection["complete_context"]["structural_versions"]
            .as_array()
            .context("selected structural context missing")?;
        ensure!(
            structural
                .iter()
                .filter(|v| v["version"]["version_id"] == request.version_id)
                .count()
                == 1
                && structural.iter().any(|v| v == &version),
            "selected version differs from complete structural context"
        );
        head["timeline_coverage"] = match (
            head["coverage_start_us"].as_i64(),
            head["coverage_end_us"].as_i64(),
        ) {
            (None, None)
                if head["coverage_start_us"].is_null() && head["coverage_end_us"].is_null() =>
            {
                Value::Null
            }
            (Some(start), Some(end)) if start >= 0 && end > start => {
                json!({"start_us":start,"end_us":end})
            }
            _ => anyhow::bail!("selected timeline coverage malformed"),
        };
        (ProofKind::Selected, version, head)
    } else {
        ensure!(
            envelope.get("lineage").is_none() && envelope.get("application_authority").is_none(),
            "mixed version proof forms"
        );
        let versions = envelope["manifest"]["versions"]
            .as_array()
            .context("strict manifest versions missing")?;
        let matches: Vec<_> = versions
            .iter()
            .filter(|v| v["version"]["version_id"] == request.version_id)
            .collect();
        ensure!(
            matches.len() == 1,
            "strict named version missing or duplicated"
        );
        (
            ProofKind::Strict,
            matches[0].clone(),
            one(
                &envelope["version_revisions"],
                "version_id",
                &request.version_id,
            )?
            .clone(),
        )
    };
    let contract = &version["version"];
    ensure!(
        contract["version_id"] == request.version_id
            && contract["association_item_id"] == request.version_item_id
            && contract["edition_item_id"] == request.edition_item_id
            && head["version_id"] == request.version_id
            && head["timeline_id"] == contract["timeline_id"],
        "named version ownership changed"
    );
    for key in [
        "version_revision",
        "timeline_revision",
        "current_timeline_revision",
    ] {
        ensure!(
            head[key].as_i64().is_some_and(|n| n > 0),
            "named version head missing"
        );
    }
    ensure!(
        head["timeline_revision"] == head["current_timeline_revision"],
        "named version timeline is stale"
    );
    Ok(VersionProof {
        kind,
        version,
        head,
        request: request.clone(),
    })
}

/// Classify only a current private projection backed by the immutable generated
/// command, reservation, full result and exact reconstructed output mapping.
/// The caller holds the Motion authorization writer and separately validates
/// current native ownership. This is descriptive receipt evidence, not a grant.
pub async fn origin_in(
    conn: &mut sqlx::SqliteConnection,
    version_id: &str,
    catalog: &str,
    incarnation: &str,
    generation: i64,
) -> anyhow::Result<ProofOrigin> {
    let rows: Vec<(String,String,String,String)> = sqlx::query_as("SELECT operation_id,command_json,output_json,receipt_json FROM catabolic_generated_publications WHERE receipt_json IS NOT NULL AND json_extract(receipt_json,'$.result.version_id')=? LIMIT 2")
        .bind(version_id).fetch_all(&mut *conn).await?;
    if rows.is_empty() {
        return Ok(ProofOrigin::Original);
    }
    ensure!(rows.len() == 1, "generated version receipt is ambiguous");
    let (operation, saved, output, receipt) = &rows[0];
    ensure!(
        [saved, output, receipt]
            .iter()
            .all(|raw| raw.len() <= 4 * 1024 * 1024),
        "generated origin proof exceeds bound"
    );
    let saved: Value = serde_json::from_str(saved)?;
    let output: Value = serde_json::from_str(output)?;
    let receipt: Value = serde_json::from_str(receipt)?;
    let command = crate::generated_publication::decode_command(&saved["command"])?;
    ensure!(
        saved["contract_version"] == 1
            && receipt["contract_version"] == 1
            && command.key().operation_id() == operation
            && command.key().catalog_id() == catalog
            && command.admission_incarnation() == incarnation,
        "generated origin command identity changed"
    );
    let reservation: playscale_core::processing_publication::Reservation =
        serde_json::from_value(saved["reservation"].clone())?;
    reservation.validate().map_err(anyhow::Error::msg)?;
    let id = reservation.identity();
    ensure!(
        id.catalog_id == catalog
            && id.incarnation == incarnation
            && id.operation_id == *operation
            && id.payload_digest == command.digest().as_str(),
        "generated origin reservation changed"
    );
    let (_, attempt) = crate::command_attempts::latest_in(conn, &command).await?;
    ensure!(
        receipt["attempt_envelope"] == serde_json::from_str::<Value>(attempt.canonical_payload())?,
        "generated origin receipt attempt changed"
    );
    let source = crate::generated_source::mapping_in(conn)
        .await?
        .context("generated origin private source missing")?;
    ensure!(
        output["mapping"] == serde_json::to_value(&source)?,
        "generated origin private source changed"
    );
    let mapping = crate::generated_publication::mapping(
        &command,
        &receipt["result"],
        &source,
        &output["probe"],
        receipt["committed_generation"]
            .as_i64()
            .context("generated origin receipt generation missing")?,
    )?;
    ensure!(
        receipt["result"]["version_id"] == version_id
            && mapping.version_files.len() == 1
            && mapping.version_files[0]["version_id"] == version_id,
        "generated origin version changed"
    );
    let file = mapping.motion_file["id"]
        .as_str()
        .context("generated origin file missing")?;
    let raw:Option<String>=sqlx::query_scalar("SELECT p.mapping_json FROM catabolic_projection_files p JOIN catabolic_projection_state s ON s.singleton=1 AND s.generation=p.generation WHERE p.file_id=? AND p.generation=? AND s.catalog_id=? AND s.incarnation=?")
        .bind(file).bind(generation).bind(catalog).bind(incarnation).fetch_optional(&mut *conn).await?;
    ensure!(
        raw.as_deref().is_some_and(|r| r.len() <= 4 * 1024 * 1024),
        "generated origin current mapping missing"
    );
    let current: crate::occurrence_bridge::OccurrenceMapping =
        serde_json::from_str(raw.as_deref().unwrap())?;
    ensure!(
        current == mapping,
        "generated origin current occurrence mapping changed"
    );
    Ok(ProofOrigin::ReceiptVerifiedGenerated)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, VersionLineageRequest) {
        let r = VersionLineageRequest {
            work_item_id: "work".into(),
            edition_item_id: "edition".into(),
            version_item_id: "owner".into(),
            version_id: "v".into(),
        };
        let version = json!({"version":{"version_id":"v","association_item_id":"owner","edition_item_id":"edition","timeline_id":"t","bindings":[{"file":{"file_id":"f","revision":"r"}}]},"equivalence":null});
        let head = json!({"version_id":"v","version_revision":3,"timeline_id":"t","timeline_revision":4,"current_timeline_revision":4,"timeline_identity_state":"established","coverage_start_us":0,"coverage_end_us":1000});
        (
            json!({"contract_version":1,"catalog_id":"c","incarnation":"i","generation":7,"profile":"p","application_authority":false,"lineage":{"catalog_id":"c","incarnation":"i","generation":7,"rows":[{"status":"active","work_item_id":"work","edition_item_id":"edition","version_item_id":"owner","version_id":"v"}]},"selection":{"all_context_occurrences_current":false,"selected_versions":[version.clone()],"complete_context":{"version_heads":[head],"structural_versions":[version]},"current_occurrences":[{"file":{"file_id":"f","revision":"r"},"snapshot":{"status":"present","ctime_ns":8},"validated_stat":{"ctime_ns":8}}]}}),
            r,
        )
    }
    #[test]
    fn selected_is_distinct_fenced_and_requires_generated_origin() {
        let (e, r) = fixture();
        let p = read(&e, "c", "i", 7, &r, ProofOrigin::ReceiptVerifiedGenerated).unwrap();
        assert_eq!(p.kind, ProofKind::Selected);
        assert_eq!(
            p.head["timeline_coverage"],
            json!({"start_us":0,"end_us":1000})
        );
        assert!(read(&e, "c", "i", 7, &r, ProofOrigin::Original).is_err());
        assert!(read(&e, "c", "i", 8, &r, ProofOrigin::ReceiptVerifiedGenerated).is_err());
        for changed in ["tuple", "occurrence", "mixed", "head"] {
            let mut v = e.clone();
            match changed {
                "tuple" => v["lineage"]["rows"][0]["work_item_id"] = json!("other"),
                "occurrence" => {
                    v["selection"]["current_occurrences"][0]["snapshot"]["status"] =
                        json!("missing")
                }
                "mixed" => v["manifest"] = json!({}),
                _ => {
                    v["selection"]["complete_context"]["version_heads"][0]["current_timeline_revision"] =
                        json!(5)
                }
            };
            assert!(
                read(&v, "c", "i", 7, &r, ProofOrigin::ReceiptVerifiedGenerated).is_err(),
                "{changed}"
            );
        }
    }
    #[test]
    fn strict_manifest_remains_original_shape() {
        let (selected, r) = fixture();
        let mut head = selected["selection"]["complete_context"]["version_heads"][0].clone();
        head["timeline_coverage"] = json!({"start_us":0,"end_us":1000});
        let e = json!({"catalog_id":"c","incarnation":"i","generation":7,"manifest":{"versions":selected["selection"]["selected_versions"]},"version_revisions":[head]});
        assert_eq!(
            read(&e, "c", "i", 7, &r, ProofOrigin::Original)
                .unwrap()
                .kind,
            ProofKind::Strict
        );
    }
}
